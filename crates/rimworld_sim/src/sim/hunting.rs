//! Animals and hunting in the simulation (docs/research.md §61): Hunt
//! designations, the hunting work target, the `JobDriver_Hunt` loop
//! (position, shoot, execute the downed, hand over the corpse), animal
//! flight from impacts and damage, and revenge (manhunter).

use super::Sim;
use crate::grid::Cell;
use crate::hunt::{
    CastEnv, CastRequest, find_cast_position, flee_dest_animal, manhunter_on_damage_chance,
};
use crate::job::{HuntStage, Job, JobKind};
use crate::path::LocomotionUrgency;
use crate::pawn::PawnId;
use crate::ranged::Vec3;
use crate::reservation::{STACK_ALL, Target};
use crate::storage::StoragePriority;

/// The Hunt driver's positioning and casting give up after this many
/// ticks (strictly more).
const HUNT_TIMEOUT_TICKS: u64 = 5000;
/// The hunting execution wait (`WaitWith`); its first count-down runs on
/// the tick it starts (fixture L: entry to execution is 179 ticks).
const EXECUTION_TICKS: i32 = 180;
/// Projectile impacts make this much noise (`Impact` clamor radius).
const IMPACT_CLAMOR_RADIUS: f32 = 12.0;
/// `Difficulty.manhunterChanceOnDamageFactor` (Rough).
const MANHUNTER_DIFFICULTY_FACTOR: f32 = 1.0;

impl Sim {
    /// Whether the pawn is an animal (`RaceProps.Animal`).
    pub fn is_animal_index(&self, i: usize) -> bool {
        self.defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            .is_none_or(|v| v == "Animal")
    }

    pub fn is_animal(&self, pawn: PawnId) -> bool {
        self.index_of(pawn).is_some_and(|i| self.is_animal_index(i))
    }

    /// A wild (factionless) animal: an animal that is not the colony's.
    // COMPATIBILITY TODO: currently approximate — animal factions (tamed,
    // other factions' animals) are not modelled: every animal is wild.
    fn is_wild_animal(&self, i: usize) -> bool {
        self.is_animal_index(i) && !self.pawns[i].is_colonist
    }

    /// `Designator_Hunt`: marks the wild animals on `cells` (alive, not
    /// already marked).
    pub fn designate_hunt(&mut self, cells: &[Cell]) -> usize {
        let mut n = 0;
        for i in 0..self.pawns.len() {
            let p = &self.pawns[i];
            if p.health.dead || !cells.contains(&p.position) || !self.is_wild_animal(i) {
                continue;
            }
            if !self.map.hunt_designations.contains(&p.id) {
                self.map.hunt_designations.push(p.id);
                n += 1;
            }
        }
        n
    }

    /// Removes Hunt marks from the animals on `cells`.
    pub fn cancel_hunt(&mut self, cells: &[Cell]) -> usize {
        let before = self.map.hunt_designations.len();
        let gone: Vec<PawnId> = self
            .pawns
            .iter()
            .filter(|p| cells.contains(&p.position))
            .map(|p| p.id)
            .collect();
        self.map.hunt_designations.retain(|p| !gone.contains(p));
        before - self.map.hunt_designations.len()
    }

    pub fn hunt_designated(&self, pawn: PawnId) -> bool {
        self.map.hunt_designations.contains(&pawn)
    }

    /// `WorkGiver_HunterHunt.HasHuntingWeapon`: a primary ranged weapon that
    /// harms health and fires no explosive projectiles.
    pub(super) fn has_hunting_weapon(&self, i: usize) -> bool {
        let Some(eq) = &self.pawns[i].equipment else {
            return false;
        };
        self.defs.things[eq.def].verbs.iter().any(|v| {
            v.is_primary
                && v.default_projectile
                    .as_deref()
                    .and_then(|p| self.defs.things.get(p))
                    .and_then(|p| p.projectile.as_ref())
                    .is_some_and(|p| p.explosion_radius <= 0.0)
        })
    }

    /// The hunting cast position for hunter `i` against `t`
    /// (`Toils_Combat.GotoCastPosition`: standing prey within 95% of range,
    /// downed prey within the execution range).
    fn hunt_cast_position(&self, i: usize, t: usize) -> Option<Cell> {
        let gun = self.firearm(i)?;
        let range = gun.verb.range;
        let downed = self.pawns[t].health.downed;
        let execution = self.defs.things[self.pawns[t].race]
            .race
            .as_ref()
            .map_or(2.0, |r| r.execution_range);
        let max = if downed {
            range.min(execution)
        } else {
            (range * 0.95).max(1.42)
        };
        let claimant = self.claimant(i);
        let at = self.pawns[i].position;
        let destinations = &self.destinations;
        // COMPATIBILITY TODO: currently approximate — PassThroughOnly
        // things on candidate cells are not penalized.
        let env = CastEnv {
            size: self.map.size(),
            walkable: &|c| self.path_grid.walkable(c),
            reachable: &|c| self.regions.connected(at, c),
            can_hit: &|c| self.shoot_line(i, t, c).is_some(),
            reservable: &|c| destinations.can_reserve(c, claimant, false),
            pass_through: &|_| false,
        };
        find_cast_position(
            CastRequest {
                caster: at,
                target: self.pawns[t].position,
                max_range_from_target: max,
                effective_range: range,
                verb_range: range,
            },
            &env,
        )
    }

    /// `WorkGiver_HunterHunt` through `JobGiver_Work`: the closest marked
    /// animal (squared distance, designation order on ties) that is
    /// reachable, reservable and shootable from some cast position — if the
    /// hunter has a hunting weapon.
    pub(super) fn hunt_work_target(&self, i: usize) -> Option<PawnId> {
        if !self.has_hunting_weapon(i) || self.map.hunt_designations.is_empty() {
            return None;
        }
        let at = self.pawns[i].position;
        let claimant = self.claimant(i);
        let mut best: Option<(i32, PawnId)> = None;
        for &target in &self.map.hunt_designations {
            let Some(t) = self.index_of(target) else {
                continue;
            };
            let c = self.pawns[t].position;
            let d = (c.x - at.x).pow(2) + (c.z - at.z).pow(2);
            if best.is_some_and(|(b, _)| d >= b) {
                continue;
            }
            if self.pawns[t].health.dead
                || !self.regions.connected(at, c)
                || !self
                    .reservations
                    .can_reserve(claimant, Target::Pawn(target), 1, 1, STACK_ALL)
                || self.hunt_cast_position(i, t).is_none()
            {
                continue;
            }
            best = Some((d, target));
        }
        best.map(|(_, t)| t)
    }

    /// The Hunt job for a target (`JobOnThing`).
    pub(super) fn hunt_job(&self, victim: PawnId) -> Job {
        Job {
            def: self.job_defs.hunt,
            kind: JobKind::Hunt {
                victim,
                stage: HuntStage::Start,
                start_tick: 0,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        }
    }

    fn set_hunt_stage(&mut self, i: usize, new: HuntStage) {
        if let Some(Job {
            kind: JobKind::Hunt { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = new;
        }
    }

    /// Toils 0–2: note the start tick, keep the attack verb, go to a cast
    /// position.
    pub(super) fn begin_hunt(&mut self, i: usize) -> bool {
        let t = self.tick;
        if let Some(Job {
            kind: JobKind::Hunt { start_tick, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *start_tick = t;
        }
        if self.firearm(i).is_none() {
            return false;
        }
        self.hunt_goto_cast(i)
    }

    /// Toil 2 (`GotoCastPosition`): find a cast position and walk there.
    fn hunt_goto_cast(&mut self, i: usize) -> bool {
        let Some(JobKind::Hunt { victim, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        let Some(t) = self.index_of(victim) else {
            return false;
        };
        let Some(cell) = self.hunt_cast_position(i, t) else {
            return false;
        };
        self.set_hunt_stage(i, HuntStage::GotoCast { cell });
        if self.pawns[i].next_stop() == cell {
            self.hunt_loop(i);
            return true;
        }
        self.walk_to(i, cell, false)
    }

    /// Toils 3–5: execute a downed prey, else reposition if it can't be hit
    /// from here, else start a shot (no hitting other pawns).
    fn hunt_loop(&mut self, i: usize) {
        let Some(JobKind::Hunt { victim, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return;
        };
        let Some(t) = self.index_of(victim) else {
            self.end_job(i, false);
            return;
        };
        if self.pawns[t].health.dead {
            self.hunt_collect(i, victim);
            return;
        }
        if self.pawns[t].health.downed {
            // Toils 8–9: touch the prey.
            self.set_hunt_stage(i, HuntStage::GotoVictim);
            let cell = self.pawns[t].position;
            match self.walk_to_touch(i, cell, true) {
                super::farming::Touch::Here => self.set_hunt_stage(
                    i,
                    HuntStage::ExecuteWait {
                        ticks_left: EXECUTION_TICKS - 1,
                    },
                ),
                super::farming::Touch::Walking => {}
                super::farming::Touch::NoPath => self.end_job(i, false),
            }
            return;
        }
        let root = self.pawns[i].position;
        if self.shoot_line(i, t, root).is_none() {
            if !self.hunt_goto_cast(i) {
                self.end_job(i, false);
            }
            return;
        }
        self.set_hunt_stage(i, HuntStage::Cast);
        self.try_start_cast(i, t, false);
    }

    /// The Hunt driver's tick: the global and toil failure conditions, then
    /// the current toil's progress.
    // COMPATIBILITY TODO: currently approximate — the driver's checks are
    // skipped while the hunter is busy (warmup/cooldown), and the hunter
    // carries the corpse with a separate haul job rather than Hunt's own
    // hauling toils.
    pub(super) fn hunt_tick(&mut self, i: usize) {
        let Some(JobKind::Hunt {
            victim,
            stage,
            start_tick,
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(t) = self.index_of(victim) else {
            self.end_job(i, false);
            return;
        };
        let dead = self.pawns[t].health.dead;
        if !dead && !self.hunt_designated(victim) {
            self.end_job(i, false);
            return;
        }
        let timed_out = self.tick > start_tick + HUNT_TIMEOUT_TICKS;
        match stage {
            HuntStage::Start => {}
            HuntStage::GotoCast { .. } => {
                if dead || timed_out {
                    self.end_job(i, false);
                } else if !self.pawns[i].is_moving() {
                    self.hunt_loop(i);
                }
            }
            HuntStage::Cast => {
                if timed_out {
                    self.end_job(i, false);
                } else if self.pawns[i].stance.is_none() {
                    self.hunt_loop(i);
                }
            }
            HuntStage::GotoVictim => {
                if dead {
                    self.hunt_collect(i, victim);
                } else if !self.pawns[t].health.downed {
                    self.end_job(i, false);
                } else if !self.pawns[i].is_moving() {
                    self.set_hunt_stage(
                        i,
                        HuntStage::ExecuteWait {
                            ticks_left: EXECUTION_TICKS - 1,
                        },
                    );
                }
            }
            HuntStage::ExecuteWait { ticks_left } => {
                if dead {
                    self.hunt_collect(i, victim);
                } else if !self.pawns[t].health.downed {
                    self.end_job(i, false);
                } else if ticks_left <= 1 {
                    self.hunting_execution(t);
                    self.hunt_collect(i, victim);
                } else {
                    self.set_hunt_stage(
                        i,
                        HuntStage::ExecuteWait {
                            ticks_left: ticks_left - 1,
                        },
                    );
                }
            }
        }
    }

    /// `ExecutionUtility.DoHuntingExecution`: blood, then the prey dies.
    // COMPATIBILITY TODO: currently approximate — the execution cut on the
    // highest-priority part is not applied (the pawn is killed directly);
    // blood is one filth.
    fn hunting_execution(&mut self, t: usize) {
        let race = self.defs.things[self.pawns[t].race].race.clone();
        if let Some(blood) = race
            .as_ref()
            .and_then(|r| r.blood_def.as_deref())
            .and_then(|b| self.defs.things.id(b))
        {
            let cell = self.pawns[t].position;
            self.try_make_filth(cell, blood, 0, true);
        }
        self.die(t);
    }

    /// Toils 13–18: the corpse is unforbidden and carried to the best
    /// storage (a new haul job), or left where it lies.
    fn hunt_collect(&mut self, i: usize, victim: PawnId) {
        let Some(corpse) = self.corpse_of(victim) else {
            self.end_job(i, false);
            return;
        };
        let defs = self.defs.clone();
        self.map.set_forbidden(&defs, corpse, false);
        let Some(item) = self.map.item(corpse).cloned() else {
            self.end_job(i, true);
            return;
        };
        let claimant = self.claimant(i);
        let dest = {
            let view = crate::haul::StoreView {
                defs: &self.defs,
                map: &self.map,
                grid: &self.path_grid,
                regions: &self.regions,
                reservations: &self.reservations,
            };
            view.best_better_store_cell(
                crate::haul::Storable {
                    def: item.def,
                    position: item.position,
                },
                Some(claimant),
                StoragePriority::Unstored,
                true,
                &mut self.rng,
            )
        };
        self.end_job(i, true);
        if let Some(dest) = dest {
            let job = Job {
                def: self.job_defs.haul_to_cell,
                kind: JobKind::Haul {
                    source: corpse,
                    dest,
                    count: 1,
                    stage: crate::job::HaulStage::GotoSource,
                    start_tick: 0,
                    aside: false,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            };
            self.interrupt_for_order(i);
            self.start_job(i, job, false);
        }
    }

    /// `FleeUtility.ShouldAnimalFleeDanger`.
    fn should_animal_flee_danger(&self, i: usize) -> bool {
        let p = &self.pawns[i];
        if !self.is_animal_index(i)
            || p.mind.mental_state.is_some()
            || p.health.downed
            || p.health.dead
            || matches!(
                p.job.as_ref().map(|j| j.kind),
                Some(JobKind::AttackMelee { .. })
            )
        {
            return false;
        }
        match p.job.as_ref() {
            Some(j) if matches!(j.kind, JobKind::Flee { .. }) => j.start_tick != self.tick,
            _ => true,
        }
    }

    /// `StartFleeingBecauseOfPawnAction`: flee 28 cells farther than the
    /// threat is; an outward escape rolls 0.5 for leaving the map; herd
    /// animals roll 0.1 for their packmates to flee too.
    // COMPATIBILITY TODO: currently approximate — leaving the map is not
    // modelled (the exit search is treated as failing: the animal flees to
    // the ordinary destination), and packmates don't flee.
    fn start_fleeing(&mut self, i: usize, threat: usize) {
        let at = self.pawns[i].position;
        let from = self.pawns[threat].position;
        let dist = (((at.x - from.x).pow(2) + (at.z - from.z).pow(2)) as f64).sqrt() as f32 + 28.0;
        let claimant = self.claimant(i);
        let dest = {
            let map = &self.map;
            let grid = &self.path_grid;
            let regions = &self.regions;
            let destinations = &self.destinations;
            let defs = &self.defs;
            let can_flee = |c: Cell| {
                map.size().contains(c)
                    && grid.walkable(c)
                    && map.door_at(c).is_none()
                    && !defs.terrain[map.terrain[c]].avoid_wander
                    && destinations.can_reserve(c, claimant, false)
                    && regions.connected(at, c)
            };
            flee_dest_animal(at, from, dist, &can_flee, &mut self.rng)
        };
        if dest != at {
            let center = Cell::new(self.map.size().width / 2, self.map.size().height / 2);
            let outward =
                (dest.x - at.x) * (center.x - at.x) + (dest.z - at.z) * (center.z - at.z) < 0;
            if outward {
                let _leave = self.rng.chance(0.5);
            }
            let threat_id = self.pawns[threat].id;
            let job = Job {
                def: self.job_defs.flee,
                kind: JobKind::Flee {
                    dest,
                    threat: Some(threat_id),
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            };
            self.interrupt_for_order(i);
            self.start_job(i, job, false);
        }
        let herd = self.defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .is_some_and(|r| r.herd_animal);
        if herd {
            let _pack = self.rng.chance(0.1);
        }
    }

    /// The flee job starts: walk to its cell.
    pub(super) fn begin_flee(&mut self, i: usize) -> bool {
        let Some(JobKind::Flee { dest, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        self.walk_to(i, dest, false)
    }

    /// A projectile's impact clamor (`GenClamor`, radius 12): animals near
    /// it may flee from the shooter (`Notify_ClamorImpact`: a 0.4 roll,
    /// then flee eligibility).
    // COMPATIBILITY TODO: currently approximate — every pawn within the
    // radius hears it (no region/sound traversal), in pawn order; the
    // sleep deadline isn't kept.
    pub(super) fn impact_clamor(&mut self, at: Vec3, launcher: PawnId) {
        let Some(li) = self.index_of(launcher) else {
            return;
        };
        let center = at.cell();
        for j in 0..self.pawns.len() {
            let p = &self.pawns[j];
            if p.health.dead || p.carried_by.is_some() || !self.is_animal_index(j) {
                continue;
            }
            let (dx, dz) = (
                (p.position.x - center.x) as f32,
                (p.position.z - center.z) as f32,
            );
            if dx * dx + dz * dz > IMPACT_CLAMOR_RADIUS * IMPACT_CLAMOR_RADIUS {
                continue;
            }
            if self.rng.chance(0.4) && self.should_animal_flee_danger(j) {
                self.start_fleeing(j, li);
            }
        }
    }

    /// `Pawn_MindState.Notify_DamageTaken` for external damage: a wild
    /// animal harmed by a pawn of a humanlike faction may turn manhunter
    /// (its race's chance × 3 × (1 − the shooter's HuntingStealth)); else
    /// damage that makes animals flee sends it running.
    pub(super) fn notify_damage_taken(&mut self, i: usize, instigator: PawnId, damage: &str) {
        if self.pawns[i].health.dead {
            return;
        }
        let Some(ii) = self.index_of(instigator) else {
            return;
        };
        let humanlike_instigator = !self.is_animal_index(ii);
        // Not when the attacker is its own prey (`PredatorHunt`).
        let hunting_instigator = matches!(
            self.pawns[i].job.as_ref().map(|j| j.kind),
            Some(JobKind::PredatorHunt { prey, .. }) if prey == instigator
        );
        if self.pawns[i].mind.mental_state.is_none()
            && humanlike_instigator
            && !hunting_instigator
            && self.is_wild_animal(i)
        {
            let race = self.defs.things[self.pawns[i].race]
                .race
                .as_ref()
                .map_or(0.0, |r| r.manhunter_on_damage_chance);
            let stealth = self.pawn_stat_of(ii, "HuntingStealth");
            let chance = manhunter_on_damage_chance(race, MANHUNTER_DIFFICULTY_FACTOR, stealth);
            if self.rng.chance(chance) {
                self.start_manhunter(i);
                return;
            }
        }
        let flees = self
            .defs
            .damages
            .get(damage)
            .is_some_and(|d| d.makes_animals_flee);
        if flees && self.should_animal_flee_danger(i) {
            self.start_fleeing(i, ii);
        }
    }

    /// `StartManhunterBecauseOfPawnAction`: the Manhunter state, then the
    /// pack-revenge roll (0.5, big threats allowed).
    // COMPATIBILITY TODO: currently approximate — packmates don't join.
    fn start_manhunter(&mut self, i: usize) {
        if self.try_start_mental_state(i, "Manhunter", false) {
            let _pack = self.rng.value() < 0.5;
        }
    }

    /// `JobGiver_Manhunter`'s target: the closest reachable humanlike.
    // COMPATIBILITY TODO: currently approximate — the game's hostile target
    // search (threat/auto-targetable scoring) is reduced to the closest
    // reachable humanlike pawn; the melee job has no 420–900 tick expiry.
    pub(super) fn manhunter_target(&self, i: usize) -> Option<PawnId> {
        let at = self.pawns[i].position;
        self.pawns
            .iter()
            .enumerate()
            .filter(|&(j, p)| {
                j != i
                    && !p.health.dead
                    && p.carried_by.is_none()
                    && !self.is_animal_index(j)
                    && self.regions.connected(at, p.position)
            })
            .min_by_key(|(_, p)| (p.position.x - at.x).pow(2) + (p.position.z - at.z).pow(2))
            .map(|(_, p)| p.id)
    }

    /// Debug tool: spawns a wild animal of the named kind.
    pub fn spawn_animal(&mut self, kind: &str, at: Cell) -> Option<PawnId> {
        let k = self.defs.pawn_kinds.id(kind)?;
        let name = self.defs.pawn_kinds[k].label.clone();
        self.spawn_pawn(k, name, at).ok()
    }
}
