//! The mining driver (`JobDriver_Mine`): walk to touch the rock, hit it
//! with the pick every round(100 / MiningSpeed) ticks for 80 damage
//! (natural rock), and when it breaks, maybe leave its yield
//! (`Mineable.DestroyMined`).

use super::farming::Touch;
use super::{JobEvent, Sim, tick_movement};
use crate::grid::Cell;
use crate::job::{Job, JobKind, MineStage};
use crate::map::Map;
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::plant::round_random;

/// `JobDriver_Mine` damage per pick hit.
const DAMAGE_NATURAL_ROCK: i32 = 80;
const DAMAGE_OTHER: i32 = 40;
/// `ticksToPickHit` before the first hit.
const NOT_STARTED: i32 = -1000;
/// `Difficulty.mineYieldFactor` (Rough).
const MINE_YIELD_DIFFICULTY_FACTOR: f32 = 1.0;

impl Sim {
    /// Designates rock for mining (`Designator_Mine`): in-bounds cells not
    /// designated yet with a mineable thing; a smoothing designation there
    /// is replaced. Returns how many were added.
    // COMPATIBILITY TODO: currently approximate — there is no fog, so the
    // designator's acceptance of fogged cells without a known rock doesn't
    // arise; vein designations don't exist.
    pub fn designate_mine(&mut self, cells: &[Cell]) -> usize {
        let mut n = 0;
        for &c in cells {
            if !self.map.size().contains(c)
                || self.map.mine_designations.contains(&c)
                || !self.map.buildings[c].is_some_and(|b| self.defs.things[b].mineable)
            {
                continue;
            }
            self.map.mine_designations.push(c);
            self.map.smooth_wall_designations.retain(|&d| d != c);
            n += 1;
        }
        n
    }

    /// Removes mine designations on `cells` (the cancel designator); the
    /// rock keeps the damage it took. Returns how many were removed.
    pub fn cancel_mine(&mut self, cells: &[Cell]) -> usize {
        let before = self.map.mine_designations.len();
        self.map.mine_designations.retain(|c| !cells.contains(c));
        before - self.map.mine_designations.len()
    }

    /// The yield fraction attributed to the rock on `c` so far
    /// (`Mineable.yieldPct`).
    pub fn rock_yield_fraction(&self, c: Cell) -> f32 {
        self.map
            .mined
            .iter()
            .find(|m| m.0 == c)
            .map_or(0.0, |m| m.2)
    }

    /// Hit points left in the rock on `c`.
    pub fn rock_hit_points(&self, c: Cell) -> Option<i32> {
        let b = self.map.buildings[c]?;
        self.defs.things[b].mineable.then_some(())?;
        Some(
            self.map
                .mined
                .iter()
                .find(|m| m.0 == c)
                .map_or_else(|| self.rock_max_hit_points(b), |m| m.1),
        )
    }

    fn rock_max_hit_points(&self, b: rimworld_defs::DefId<rimworld_defs::ThingDef>) -> i32 {
        self.defs.things[b]
            .stat("MaxHitPoints")
            .unwrap_or(100.0)
            .round() as i32
    }

    /// The job starts: walk to touch the rock.
    pub(super) fn begin_mine(&mut self, i: usize) -> bool {
        let Some(JobKind::Mine { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        match self.walk_to_touch(i, cell, true) {
            Touch::Here => {
                self.start_mining(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    pub(super) fn start_mining(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::Mine { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = MineStage::Mining {
                ticks_to_hit: NOT_STARTED,
            };
        }
    }

    /// `ResetTicksToPickHit`: round(100 / MiningSpeed).
    fn ticks_per_pick_hit(&self, i: usize) -> i32 {
        let speed = self.pawn_stat_of(i, "MiningSpeed");
        (100.0 / speed).round_ties_even() as i32
    }

    /// The mining toil's interval work.
    // COMPATIBILITY TODO: currently approximate — ore strikes, vein
    // designations and the difficulty mine-yield factor are not modelled.
    pub(super) fn mine_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Mine {
                    cell,
                    stage: MineStage::Mining { ticks_to_hit },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let mut ticks = ticks_to_hit;
        if ticks < -100 {
            ticks = self.ticks_per_pick_hit(i);
        }
        // Natural rock has no faction: mining it teaches.
        self.learn(i, "Mining", 0.07 * delta as f32);
        ticks -= delta;
        if ticks <= 0 {
            if self.pick_hit(i, cell) {
                self.end_job(i, true);
                return;
            }
            ticks = self.ticks_per_pick_hit(i);
        }
        if let Some(Job {
            kind: JobKind::Mine { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = MineStage::Mining {
                ticks_to_hit: ticks,
            };
        }
    }

    /// `Mineable.Notify_TookMiningDamage`: the share of hit points taken ×
    /// the miner's MiningYield, kept in binary32.
    fn attribute_yield(&mut self, entry: usize, amount: i32, hp: i32, max: i32, yield_factor: f32) {
        let share = amount.min(hp) as f32 / max as f32;
        let pct = self.map.mined[entry].2;
        self.map.mined[entry].2 = (pct as f64 + share as f64 * yield_factor as f64) as f32;
    }

    /// `DoDamage`: one pick hit. A surviving rock takes the damage through
    /// the ordinary path — the damage angle draw, then (wasteable yield
    /// only) the yield share of the randomly rounded amount, then the hit
    /// points' own random rounding; the final hit attributes exactly the
    /// remaining hit points and destroys the rock (`DestroyMined`).
    /// Returns whether the rock broke.
    fn pick_hit(&mut self, i: usize, cell: Cell) -> bool {
        let Some(b) = self.map.buildings[cell] else {
            return true;
        };
        let def = self.defs.things[b].clone();
        let props = def.building.as_ref();
        let natural = props.is_some_and(|p| p.is_natural_rock);
        let damage = if natural {
            DAMAGE_NATURAL_ROCK
        } else {
            DAMAGE_OTHER
        };
        let max = self.rock_max_hit_points(b);
        let hp = self.rock_hit_points(cell).unwrap_or(max);
        let yield_factor = self.pawn_stat_of(i, "MiningYield");
        let entry = match self.map.mined.iter().position(|m| m.0 == cell) {
            Some(n) => n,
            None => {
                self.map.mined.push((cell, max, 0.0));
                self.map.mined.len() - 1
            }
        };
        let wasteable =
            props.is_some_and(|p| p.mineable_thing.is_some() && p.mineable_yield_wasteable);
        if hp > damage {
            // `new DamageInfo(...)`: angle −1 draws one in 0..359.
            let _angle = self.rng.range_inclusive(0, 359);
            // `Mineable.PreApplyDamage`.
            if wasteable {
                let amount = round_random(damage as f32, &mut self.rng) as i32;
                self.attribute_yield(entry, amount, hp, max, yield_factor);
            }
            // `DamageWorker.Apply`: hit points lose the rounded amount.
            let applied = round_random(damage as f32, &mut self.rng) as i32;
            self.map.mined[entry].1 = hp - applied;
            return false;
        }
        self.attribute_yield(entry, hp, hp, max, yield_factor);
        let yield_pct = self.map.mined[entry].2;
        self.map.mined.remove(entry);
        self.map.mine_designations.retain(|&c| c != cell);
        let holds_roof = self.map.buildings[cell].is_some_and(|b| self.defs.things[b].holds_roof);
        self.map.buildings[cell] = None;
        self.map.building_stuff[cell] = None;
        if holds_roof {
            self.roof_holder_despawned(crate::geom::Footprint {
                center: cell,
                rot: crate::job::Rot4::North,
                size: (1, 1),
            });
        }
        // `Building.DeSpawn`: the rock leaves its rough-hewn stone floor.
        if let Some(t) = props
            .and_then(|p| p.leave_terrain.as_deref())
            .and_then(|t| self.defs.terrain.id(t))
        {
            self.map.set_terrain(cell, t);
        }
        self.map.bump_structure_revision();
        // The cell is free again before the yield drops onto it.
        self.refresh_path_grid();
        // `GenLeaving.DoLeavingsFor` (killed): the rock's filth leavings,
        // 1–3 on its cell.
        // COMPATIBILITY TODO: currently approximate — the building's
        // destroy effects (dust) and their random draws are not modelled.
        if let Some(filth) = raw_filth_leaving(&self.defs, &def) {
            let count = self.rng.range_inclusive(1, 3);
            for _ in 0..count {
                self.try_make_filth(cell, filth, 0, true);
            }
        }
        // `TrySpawnYield`: rejected only when the roll exceeds the drop
        // chance (a chance of 1 still draws); the count is
        // max(1, RoundToInt(yield × difficulty factor)), and for wasteable
        // yield max(1, RoundRandom(count × yield fraction)).
        self.try_spawn_yield(&def, cell, yield_pct);
        self.refresh_path_grid();
        true
    }
}

impl Sim {
    /// `Mineable.TrySpawnYield` for rock `def` at `cell` with the
    /// attributed `yield_pct`.
    fn try_spawn_yield(&mut self, def: &rimworld_defs::ThingDef, cell: Cell, yield_pct: f32) {
        let props = def.building.as_ref();
        if let Some(thing) = props.and_then(|p| p.mineable_thing.as_deref())
            && let Some(thing) = self.defs.things.id(thing)
            && {
                let roll = self.rng.value() <= props.map_or(1.0, |p| p.mineable_drop_chance);
                self.debug_mine_drops.unwrap_or(roll)
            }
        {
            let base = props.map_or(1, |p| p.mineable_yield) as f32 * MINE_YIELD_DIFFICULTY_FACTOR;
            let mut n = (base.round_ties_even() as i32).max(1);
            if props.is_some_and(|p| p.mineable_yield_wasteable) {
                n = (round_random(n as f32 * yield_pct, &mut self.rng) as i32).max(1);
            }
            self.place_near(thing, n as u32, cell);
        }
    }

    /// Debug: the random stream's (seed, counter).
    pub fn rng_state(&self) -> (u32, u32) {
        self.rng.state()
    }

    /// Debug: sets the random stream (a seeded push in the game's probes).
    pub fn debug_set_rng(&mut self, seed: u32, counter: u32) {
        self.rng.set_state(seed, counter);
    }

    /// Debug: one pick hit by `pawn` on the rock at `cell` (a function
    /// boundary fixture). Returns whether the rock broke.
    pub fn debug_mining_hit(&mut self, pawn: crate::pawn::PawnId, cell: Cell) -> bool {
        let i = self.index_of(pawn).expect("pawn");
        self.pick_hit(i, cell)
    }

    /// Debug: the yield helper alone for a rock def at `cell`.
    pub fn debug_try_spawn_yield(
        &mut self,
        rock: rimworld_defs::DefId<rimworld_defs::ThingDef>,
        cell: Cell,
        yield_pct: f32,
    ) {
        let def = self.defs.things[rock].clone();
        self.try_spawn_yield(&def, cell, yield_pct);
    }
}

/// A thing's `filthLeaving`.
fn raw_filth_leaving(
    defs: &rimworld_defs::GameDefs,
    def: &rimworld_defs::ThingDef,
) -> Option<rimworld_defs::DefId<rimworld_defs::ThingDef>> {
    defs.raw
        .get("ThingDef", &def.def_name)
        .and_then(|d| d.node.child_text("filthLeaving"))
        .and_then(|f| defs.things.id(f.trim()))
}

/// The per-tick part of the mining driver.
pub(super) fn tick_mine(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::Mine { cell, stage } = pawn.job.as_ref()?.kind else {
        return None;
    };
    // `FailOn`: no longer designated, or the rock is gone.
    if !map.mine_designations.contains(&cell) || map.buildings[cell].is_none() {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        MineStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToMine
            }
        }
        MineStage::Mining { .. } => JobEvent::None,
    })
}
