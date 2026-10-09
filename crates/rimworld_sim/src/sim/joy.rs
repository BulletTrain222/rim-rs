//! Recreation in the simulation (docs/research.md §57): the joy need's
//! interval with the colony's expectations (from its wealth), and the joy
//! jobs (`JobDriver_GoForWalk`, `JobDriver_Skygaze`,
//! `JobDriver_RelaxAlone`) with `JoyUtility.JoyTickCheckEnd`.

use rimworld_defs::{DefId, ExpectationDef, ThingDef};

use super::{JobEvent, Sim, tick_movement};
use crate::grid::Cell;
use crate::job::{Job, JobKind, JoyActivity, JoyStage};
use crate::map::Map;
use crate::needs::NeedKind;
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::recreation::{JoyFacts, allows_joy, joy_gain};
use crate::stats::{def_stat, terrain_stat};

/// `WealthWatcher.MinCountInterval`.
const WEALTH_RECOUNT_TICKS: u64 = 5000;

/// The cached colony wealth (`WealthWatcher`) and the outdoor-room flags
/// recreation searches use.
#[derive(Debug, Clone, Default)]
pub struct JoyCache {
    expectation_override: Option<String>,
    pub(super) wealth: Option<(u64, f32)>,
    outdoor_rooms: Option<((u64, u64), Vec<bool>)>,
}

impl Sim {
    /// `StatWorker_MarketValue` for a def made of `stuff`: its
    /// `MarketValue` base if it has one, else its costs' values (stuff by
    /// volume) plus 0.0036 per unit of work.
    // COMPATIBILITY TODO: currently approximate — things priced through
    // their recipe (no costs), comps, quality and hit points are not
    // modelled.
    pub fn market_value(&self, def: DefId<ThingDef>, stuff: Option<DefId<ThingDef>>) -> f32 {
        let defs = &self.defs;
        let d = &defs.things[def];
        if let Some(&v) = d.stat_bases.get("MarketValue") {
            return v;
        }
        let mut v: f32 = d
            .cost_list
            .iter()
            .filter_map(|(name, n)| {
                let c = defs.things.get(name)?;
                Some(*n as f32 * c.stat_bases.get("MarketValue").copied().unwrap_or(0.0))
            })
            .sum();
        if d.cost_stuff_count > 0 {
            v += match stuff {
                Some(s) => {
                    let sd = &defs.things[s];
                    d.cost_stuff_count as f32 / sd.volume_per_unit()
                        * sd.stat_bases.get("MarketValue").copied().unwrap_or(0.0)
                }
                None => d.cost_stuff_count as f32 * 2.0,
            };
        }
        let stuff_def = stuff.map(|s| &defs.things[s]);
        let work = def_stat(defs, d, stuff_def, "WorkToMake").max(def_stat(
            defs,
            d,
            stuff_def,
            "WorkToBuild",
        ));
        if work > 2.0 {
            v += work * 0.0036;
        }
        v
    }

    /// `WealthWatcher.WealthTotal`, recounted at most every 5,000 ticks:
    /// the items on the map, the colony's buildings and floors, and its
    /// pawns.
    // COMPATIBILITY TODO: currently approximate — pawns count their race's
    // base market value (no skills, traits or health), worn apparel and
    // corpses are not counted, and built floors count every non-natural
    // terrain.
    pub fn wealth_total(&mut self) -> f32 {
        if let Some((t, w)) = self.joy_cache.wealth
            && self.tick < t + WEALTH_RECOUNT_TICKS
        {
            return w;
        }
        let mut w = 0.0;
        for item in self.map.items() {
            if item.is_filth() || self.defs.things[item.def].corpse_of.is_some() {
                continue;
            }
            w += self.market_value(item.def, None) * item.stack_count as f32;
        }
        for s in self.map.structures() {
            w += self.market_value(s.def, s.stuff);
        }
        let size = self.map.size();
        for z in 0..size.height {
            for x in 0..size.width {
                let t = &self.defs.terrain[self.map.terrain[Cell::new(x, z)]];
                if !t.cost_list.is_empty() {
                    w += terrain_stat(&self.defs, t, "MarketValue");
                }
            }
        }
        for p in &self.pawns {
            if p.is_colonist && !p.health.dead {
                w += self.defs.things[p.race]
                    .stat_bases
                    .get("MarketValue")
                    .copied()
                    .unwrap_or(0.0);
            }
        }
        self.joy_cache.wealth = Some((self.tick, w));
        w
    }

    /// Debug: use this expectation instead of the one from wealth.
    pub fn debug_set_expectation(&mut self, name: Option<&str>) {
        self.joy_cache.expectation_override = name.map(str::to_owned);
    }

    /// Debug: what joy giver `giver` would offer pawn `pawn` with the
    /// random stream set to counter 0 of `seed`: the activity's cells
    /// (walk waypoints or the skygaze/relax cell) and the stream counter
    /// after. The stream is restored afterwards.
    pub fn debug_joy_giver(
        &mut self,
        pawn: crate::pawn::PawnId,
        giver: &str,
        seed: i32,
    ) -> (Option<Vec<Cell>>, u32) {
        let Some(i) = self.index_of(pawn) else {
            return (None, 0);
        };
        let hour = self.hour_of_day();
        let comfy = (
            self.pawn_stat_of(i, "ComfyTemperatureMin"),
            self.pawn_stat_of(i, "ComfyTemperatureMax"),
        );
        let outdoor_rooms = self.outdoor_rooms();
        let owned = self.owned_room_cells(i);
        let incapable: Vec<String> = Vec::new();
        let defs = self.defs.clone();
        let Some(g) = defs.joy_givers.get(giver) else {
            return (None, 0);
        };
        let saved = self.rng.state();
        self.rng.set_state(seed as u32, 0);
        let result = {
            let Some(facts) = joy_facts(
                &defs,
                &self.pawns[i],
                self.tick,
                self.outdoor_temperature,
                hour,
                comfy,
                owned.as_deref(),
                &outdoor_rooms,
                &incapable,
            ) else {
                self.rng.set_state(saved.0, saved.1);
                return (None, 0);
            };
            let view = crate::cell_finder::MapView {
                map: &self.map,
                defs: &defs,
                grid: &self.path_grid,
                regions: &self.regions,
            };
            let reservable = |_: Cell| true;
            let env = crate::recreation::JoyEnv {
                view: &view,
                at: self.pawns[i].position,
                job_defs: &self.job_defs,
                reservable: &reservable,
            };
            crate::recreation::try_give_job(&defs, g, &facts, &env, &mut self.rng)
        };
        let after = self.rng.state().1;
        self.rng.set_state(saved.0, saved.1);
        let cells = result.and_then(|j| match j.kind {
            JobKind::Joy { activity, .. } => Some(match activity {
                JoyActivity::Walk { path, len, .. } => path[..len as usize].to_vec(),
                JoyActivity::Skygaze { cell } | JoyActivity::Relax { cell } => vec![cell],
            }),
            _ => None,
        });
        (cells, after)
    }

    /// `ExpectationsUtility.CurrentExpectationFor(map)`: the first
    /// wealth-triggered expectation whose `maxMapWealth` exceeds the wealth.
    pub fn expectation(&mut self) -> Option<ExpectationDef> {
        let w = self.wealth_total();
        self.expectation_for(w)
    }

    /// The expectation for a colony wealth (or the debug override).
    pub(super) fn expectation_for(&self, w: f32) -> Option<ExpectationDef> {
        if let Some(name) = &self.joy_cache.expectation_override {
            return self
                .defs
                .expectations
                .iter()
                .find(|e| &e.def_name == name)
                .cloned();
        }
        let list: Vec<&ExpectationDef> = self
            .defs
            .expectations
            .iter()
            .filter(|e| e.max_map_wealth.is_some())
            .collect();
        list.iter()
            .find(|e| w < e.max_map_wealth.unwrap_or(f32::MAX))
            .or(list.last())
            .map(|e| (*e).clone())
    }

    /// The joy part of the need interval (not while asleep).
    pub(super) fn joy_need_interval(&mut self, i: usize) {
        if self.pawns[i].asleep || self.pawns[i].needs.get(NeedKind::Joy).is_none() {
            return;
        }
        let drop = self
            .expectation()
            .map_or(0.0, |e| e.joy_tolerance_drop_per_day);
        let fall = self.pawn_stat_of(i, "JoyFallRateFactor");
        let tick = self.tick;
        self.pawns[i].needs.joy_interval(tick, drop, fall);
    }

    /// Per room: `PsychologicallyOutdoors`, cached until roofs or
    /// buildings change.
    pub(super) fn outdoor_rooms(&mut self) -> Vec<bool> {
        let key = (self.map.roof_revision, self.map.structure_revision());
        if let Some((k, v)) = &self.joy_cache.outdoor_rooms
            && *k == key
        {
            return v.clone();
        }
        let n = self.regions.rooms().len();
        let v: Vec<bool> = (0..n)
            .map(|r| self.room_psychologically_outdoors(r))
            .collect();
        self.joy_cache.outdoor_rooms = Some((key, v.clone()));
        v
    }

    /// `ownership.OwnedRoom`: the cells of the bedroom holding the pawn's
    /// bed, when it is a bedroom (`Room.Owners`).
    pub(super) fn owned_room_cells(&self, i: usize) -> Option<Vec<Cell>> {
        let bed = self.pawns[i].owned_bed?;
        let s = self.map.structure(bed)?;
        let room = self.room_at(s.footprint.center)?;
        if self.room_role(room) != "Bedroom" || !s.owners.contains(&self.pawns[i].id) {
            return None;
        }
        Some(self.room_cells(room))
    }

    /// `WalkPathFinder.TryFindWalkPath` from `root` (for tests).
    pub fn debug_walk_path(&self, root: Cell) -> Option<[Cell; crate::recreation::WALK_PATH_LEN]> {
        let view = crate::cell_finder::MapView {
            map: &self.map,
            defs: &self.defs,
            grid: &self.path_grid,
            regions: &self.regions,
        };
        crate::recreation::walk_path(&view, root)
    }

    /// Debug: starts a recreation job (a giver's job built by the test).
    pub fn debug_start_joy(&mut self, pawn: crate::pawn::PawnId, def: &str, activity: JoyActivity) {
        let Some(i) = self.index_of(pawn) else {
            return;
        };
        let job = Job {
            def: self.defs.jobs.id(def),
            kind: JobKind::Joy {
                activity,
                stage: JoyStage::Goto,
            },
            forced: false,
            urgency: if matches!(activity, JoyActivity::Walk { .. }) {
                crate::path::LocomotionUrgency::Walk
            } else {
                crate::path::LocomotionUrgency::Jog
            },
            start_tick: 0,
        };
        self.drop_carried(i);
        self.cleanup_job(i);
        self.pawns[i].job = None;
        self.start_job(i, job, false);
    }

    /// The joy jobs' per-tick fail conditions, checked before the delay
    /// can expire (`FailOn` runs in the driver tick ahead of the delay):
    /// a walk fails whenever outdoors stops being enjoyable; skygazing,
    /// once lying, also when its current cell is roofed.
    /// Returns whether the job failed.
    pub(super) fn joy_tick_checks(&mut self, i: usize) -> bool {
        let Some(JobKind::Joy { activity, stage }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let check = match activity {
            JoyActivity::Walk { .. } => true,
            JoyActivity::Skygaze { .. } => matches!(stage, JoyStage::Active { .. }),
            JoyActivity::Relax { .. } => false,
        };
        if !check {
            return false;
        }
        if matches!(activity, JoyActivity::Skygaze { .. })
            && self.map.roofed(self.pawns[i].position)
        {
            self.end_job(i, false);
            return true;
        }
        let comfy = (
            self.pawn_stat_of(i, "ComfyTemperatureMin"),
            self.pawn_stat_of(i, "ComfyTemperatureMax"),
        );
        if !(comfy.0..=comfy.1).contains(&self.outdoor_temperature) {
            self.end_job(i, false);
            return true;
        }
        false
    }

    /// The joy job starts: walk to its cell.
    pub(super) fn begin_joy(&mut self, i: usize) -> bool {
        let Some(JobKind::Joy { activity, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        let cell = activity.cell();
        if self.pawns[i].position == cell {
            // Already there: the job starts outside the driver tick, so the
            // activity's delay doesn't pay this tick.
            self.joy_arrived_paying(i, false);
            return true;
        }
        self.walk_to(i, cell, false)
    }

    /// At the cell: a walk heads for its next waypoint (ending after the
    /// last); the other activities start their `joyDuration` toil.
    pub(super) fn joy_arrived(&mut self, i: usize) {
        self.joy_arrived_paying(i, true);
    }

    /// [`Sim::joy_arrived`]; `paid`: the arrival came in this tick's driver
    /// tick, which already counts down the new delay once.
    // COMPATIBILITY TODO: currently approximate — the paid arrival tick is
    // inferred from the research bench's recorded timing, not observed for
    // joy jobs.
    fn joy_arrived_paying(&mut self, i: usize, paid: bool) {
        let Some(job) = self.pawns[i].job.as_ref() else {
            return;
        };
        let JobKind::Joy { activity, .. } = job.kind else {
            return;
        };
        let duration = job.def.map_or(4000, |d| self.defs.jobs[d].joy_duration);
        match activity {
            JoyActivity::Walk { path, len, next } => {
                let next = next as usize + 1;
                if next >= len as usize {
                    self.end_job(i, true);
                    return;
                }
                set_joy(&mut self.pawns[i], |a, _| {
                    if let JoyActivity::Walk { next: n, .. } = a {
                        *n = next as u8;
                    }
                });
                if !self.walk_to(i, path[next], false) {
                    self.end_job(i, false);
                }
            }
            JoyActivity::Skygaze { .. } | JoyActivity::Relax { .. } => {
                // `JobDriver_RelaxAlone`: the job has no facing, so one is
                // picked at random (`Rot4.Random`).
                if matches!(activity, JoyActivity::Relax { .. }) {
                    self.rng.range_inclusive(0, 3);
                }
                let left = if paid { duration - 1 } else { duration };
                set_joy(&mut self.pawns[i], |_, s| {
                    *s = JoyStage::Active { ticks_left: left }
                });
            }
        }
    }

    /// The joy toil's interval (`JoyTickCheckEnd`): gain
    /// joyGainRate × 0.36/2500 × delta of the job's joy kind (and its
    /// skill's xp); end when joy is full or the timetable forbids joy; a
    /// walk also ends after `joyDuration`; skygazing fails under a roof or
    /// when outdoors stops being enjoyable.
    // COMPATIBILITY TODO: currently approximate — the order of the end
    // checks against the toil's countdown and the first interval's timing
    // are not verified at runtime.
    pub(super) fn joy_interval(&mut self, i: usize, delta: i32) {
        let Some(job) = self.pawns[i].job.as_ref() else {
            return;
        };
        let JobKind::Joy { activity, stage } = job.kind else {
            return;
        };
        let walking = matches!(activity, JoyActivity::Walk { .. });
        if walking != (stage == JoyStage::Goto) {
            return;
        }
        let Some(def) = job.def.map(|d| self.defs.jobs[d].clone()) else {
            return;
        };
        let start = job.start_tick;
        if walking && self.tick > start + def.joy_duration as u64 {
            self.end_job(i, true);
            return;
        }
        let tick = self.tick;
        self.pawns[i].needs.gain_joy(
            joy_gain(1.0, def.joy_gain_rate, delta),
            def.joy_kind.as_deref(),
            tick,
        );
        if let Some(skill) = &def.joy_skill {
            self.learn(i, skill, def.joy_xp_per_tick * delta as f32);
        }
        if !allows_joy(crate::rest::assignment_for(
            &self.pawns[i],
            self.hour_of_day(),
            true,
        )) {
            self.end_job(i, false);
            return;
        }
        if self.pawns[i]
            .needs
            .get(NeedKind::Joy)
            .is_some_and(|j| j.level > 0.9999)
        {
            self.end_job(i, true);
        }
    }
}

/// What the joy givers need about `p`.
#[allow(clippy::too_many_arguments)]
pub(super) fn joy_facts<'a>(
    defs: &rimworld_defs::GameDefs,
    p: &'a Pawn,
    tick: u64,
    outdoor_temperature: f32,
    hour: u32,
    comfy: (f32, f32),
    owned_room: Option<&'a [Cell]>,
    outdoor_rooms: &'a [bool],
    incapable: &'a [String],
) -> Option<JoyFacts<'a>> {
    let level = p.needs.get(NeedKind::Joy)?.level;
    let humanlike = defs.things[p.race]
        .race
        .as_ref()
        .and_then(|r| r.intelligence.as_deref())
        == Some("Humanlike");
    let soon_basic_need = p.needs.rest_level().is_some_and(|r| r < 0.28 + 0.05)
        || p.needs
            .get(NeedKind::Food)
            .is_some_and(|f| f.percent() < f.want_eat * 0.8 + 0.05);
    Some(JoyFacts {
        level,
        state: &p.needs.joy,
        id_number: p.id_number,
        assignment: crate::rest::assignment_for(p, hour, humanlike),
        tick,
        enjoyable_outside: (comfy.0..=comfy.1).contains(&outdoor_temperature),
        soon_basic_need,
        owned_room,
        outdoor_rooms,
        incapable,
    })
}

fn set_joy(pawn: &mut Pawn, f: impl FnOnce(&mut JoyActivity, &mut JoyStage)) {
    if let Some(Job {
        kind: JobKind::Joy { activity, stage },
        ..
    }) = &mut pawn.job
    {
        f(activity, stage);
    }
}

/// The per-tick part of a joy job: walking, then the toil's countdown.
pub(super) fn tick_joy(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::Joy { stage, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    Some(match stage {
        JoyStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedForJoy
            }
        }
        JoyStage::Active { ticks_left } => {
            let left = ticks_left - 1;
            set_joy(pawn, |_, s| *s = JoyStage::Active { ticks_left: left });
            if left <= 0 {
                JobEvent::Ended(true)
            } else {
                JobEvent::None
            }
        }
    })
}
