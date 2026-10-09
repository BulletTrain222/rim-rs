//! The refuel driver (`JobDriver_Refuel`): fetch fuel, carry it to the
//! building, wait 240 ticks, put it in.

use super::farming::Touch;
use super::{JobEvent, PATH_START_LATENCY_TICKS, Sim, tick_movement};
use crate::haul::max_carry;
use crate::job::{Job, JobKind, RefuelStage};
use crate::map::Map;
use crate::path::{COLONIST_HEURISTIC_STRENGTH, PathGrid, find_path};
use crate::pawn::{Carried, Pawn};
use crate::refuel::{fuel_count_to_fill, is_full};
use crate::reservation::Target;
use rimworld_defs::GameDefs;

/// `JobDriver_Refuel.RefuelingDuration`.
const REFUEL_TICKS: i32 = 240;

impl Sim {
    /// The job starts: `job.count` is what fills the building; walk onto
    /// the fuel (or pick it up at once).
    pub(super) fn begin_refuel(&mut self, i: usize) -> bool {
        let Some(JobKind::Refuel { building, fuel, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let defs = self.defs.clone();
        let Some(need) = self.map.structure(building).and_then(|s| {
            defs.things[s.def]
                .refuelable
                .as_ref()
                .map(|p| fuel_count_to_fill(p, s.fuel))
        }) else {
            return false;
        };
        let Some(at_fuel) = self.map.item(fuel).map(|it| it.position) else {
            return false;
        };
        if let Some(Job {
            kind: JobKind::Refuel { count, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *count = need;
        }
        let tick = self.tick;
        let pawn = &mut self.pawns[i];
        let from = pawn.next_stop();
        if from == at_fuel {
            self.refuel_pick_up(i);
            return true;
        }
        match find_path(
            &self.path_grid,
            from,
            at_fuel,
            pawn.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) => {
                pawn.path = path.cells.into();
                pawn.destination = Some(at_fuel);
                pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, at_fuel, true);
                true
            }
            Err(_) => false,
        }
    }

    /// On the fuel: pick up what is still wanted (`StartCarryThing`), then
    /// carry it to the building.
    // COMPATIBILITY TODO: currently approximate — collecting nearby stacks
    // of the same fuel (`CheckForGetOpportunityDuplicate`) is not done.
    pub(super) fn refuel_pick_up(&mut self, i: usize) {
        let Some(JobKind::Refuel {
            building,
            fuel,
            count,
            ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some((def, stack, rot, hp)) = self
            .map
            .item(fuel)
            .map(|it| (it.def, it.stack_count, it.rot, it.hit_points))
        else {
            self.end_job(i, false);
            return;
        };
        let room = max_carry(&self.defs, def, self.carrying_capacity(i))
            - self.pawns[i].carried.map_or(0, |c| c.count as i32);
        let wanted = count.min(room).min(stack as i32);
        if wanted <= 0 {
            self.end_job(i, false);
            return;
        }
        let taken = self.map.take_from_item(fuel, wanted as u32);
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        let id = if taken < stack {
            self.reservations
                .release(Target::Item(fuel), claimant.pawn, job_id);
            self.map.allocate_item_id()
        } else {
            fuel
        };
        let (already, carried_hp) = self.pawns[i]
            .carried
            .map_or((0, None), |c| (c.count, c.hit_points));
        let max_hp = self.max_hit_points(def);
        self.pawns[i].carried = Some(Carried {
            id,
            def,
            count: already + taken,
            rot,
            hit_points: crate::map::blend_hit_points(carried_hp, already, hp, taken, max_hp),
        });
        self.refresh_path_grid();
        if let Some(Job {
            kind: JobKind::Refuel { count, stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *count -= taken as i32;
            *stage = RefuelStage::GotoBuilding;
        }
        let Some(cell) = self.map.structure(building).map(|s| s.footprint.center) else {
            self.end_job(i, false);
            return;
        };
        match self.walk_to_touch(i, cell, true) {
            Touch::Here => self.start_refuel_wait(i),
            Touch::Walking => {}
            Touch::NoPath => self.end_job(i, false),
        }
    }

    /// Next to the building: the 240-tick wait starts; the tick it starts
    /// in counts as its first (recorded).
    pub(super) fn start_refuel_wait(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::Refuel { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RefuelStage::Wait {
                ticks_left: REFUEL_TICKS - 1,
            };
        }
    }

    /// `FinalizeRefueling`: the carried fuel goes in, up to what fills the
    /// building; the job succeeds.
    pub(super) fn finish_refuel(&mut self, i: usize) {
        let Some(JobKind::Refuel { building, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let defs = self.defs.clone();
        if let (Some(mut carried), Some(s)) =
            (self.pawns[i].carried, self.map.structure_mut(building))
            && let Some(p) = defs.things[s.def].refuelable.as_ref()
        {
            let used = (fuel_count_to_fill(p, s.fuel) as u32).min(carried.count);
            s.fuel = (s.fuel + used as f32).min(p.capacity);
            carried.count -= used;
            self.pawns[i].carried = (carried.count > 0).then_some(carried);
            self.light_key = None;
        }
        self.end_job(i, true);
    }
}

/// The per-tick part of the refuel driver.
pub(super) fn tick_refuel(
    pawn: &mut Pawn,
    map: &Map,
    defs: &GameDefs,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::Refuel {
        building,
        fuel,
        stage,
        ..
    } = pawn.job.as_ref()?.kind
    else {
        return None;
    };
    let Some(s) = map.structure(building) else {
        return Some(JobEvent::Failed);
    };
    // `AddEndCondition`: a full building ends the job as done.
    if defs.things[s.def]
        .refuelable
        .as_ref()
        .is_some_and(|p| is_full(p, s.fuel))
    {
        return Some(JobEvent::Ended(true));
    }
    Some(match stage {
        RefuelStage::GotoFuel => {
            if map.item(fuel).is_none() {
                return Some(JobEvent::Failed);
            }
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedAtFuel
            }
        }
        RefuelStage::GotoBuilding => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToRefuel
            }
        }
        RefuelStage::Wait { ticks_left } => {
            let left = ticks_left - 1;
            if let Some(Job {
                kind: JobKind::Refuel { stage, .. },
                ..
            }) = &mut pawn.job
            {
                *stage = RefuelStage::Wait { ticks_left: left };
            }
            if left <= 0 {
                JobEvent::RefuelDone
            } else {
                JobEvent::None
            }
        }
    })
}
