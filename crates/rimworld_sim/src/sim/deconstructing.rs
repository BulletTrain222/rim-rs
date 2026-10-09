//! The deconstruction driver (`JobDriver_Deconstruct`): walk to touch the
//! building, work clamp(WorkToBuild, 20, 3000) at ConstructionSpeed × 1.7,
//! then take it down and leave part of its cost
//! (`GenLeaving.DoLeavingsFor` with `DestroyMode.Deconstruct`).

use super::farming::Touch;
use super::{JobEvent, Sim, tick_movement};
use crate::deconstruct::{MAX_WORK, MIN_WORK};
use crate::job::{DeconstructStage, Job, JobKind};
use crate::map::{ItemId, Map};
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::plant::round_random;
use crate::reservation::Target;
use crate::stats::{cost_list, def_stat};

impl Sim {
    /// Designates finished buildings on `cells` for deconstruction.
    /// Returns how many were added.
    pub fn designate_deconstruct(&mut self, cells: &[crate::grid::Cell]) -> usize {
        let mut n = 0;
        for &c in cells {
            let Some(s) = self
                .map
                .structures()
                .iter()
                .find(|s| s.footprint.contains(c))
            else {
                continue;
            };
            let id = s.id;
            let ok = self.defs.things[s.def]
                .building
                .as_ref()
                .is_some_and(|b| b.deconstructible);
            if ok && !self.map.deconstruct_designations.contains(&id) {
                self.map.deconstruct_designations.push(id);
                n += 1;
            }
        }
        n
    }

    /// The job starts: walk to touch the building.
    pub(super) fn begin_deconstruct(&mut self, i: usize) -> bool {
        let Some(JobKind::Deconstruct { building, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(cell) = self.map.structure(building).map(|s| s.footprint.center) else {
            return false;
        };
        match self.walk_to_touch(i, cell, true) {
            Touch::Here => {
                self.start_deconstructing(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    /// The work toil starts: `TotalNeededWork`.
    pub(super) fn start_deconstructing(&mut self, i: usize) {
        let Some(JobKind::Deconstruct { building, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let work = self.map.structure(building).map_or(MIN_WORK, |s| {
            let def = &self.defs.things[s.def];
            let stuff = s.stuff.map(|st| &self.defs.things[st]);
            def_stat(&self.defs, def, stuff, "WorkToBuild").clamp(MIN_WORK, MAX_WORK)
        });
        if let Some(Job {
            kind: JobKind::Deconstruct { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = DeconstructStage::Work { work_left: work };
        }
    }

    /// The work toil's interval: ConstructionSpeed × 1.7 × delta.
    pub(super) fn deconstruct_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Deconstruct {
                    building,
                    stage: DeconstructStage::Work { work_left },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let speed = self.pawn_stat_of(i, "ConstructionSpeed");
        let left = work_left - speed * 1.7 * delta as f32;
        // `TickActionInterval`: experience if the building cost anything.
        let costs = self.map.structure(building).is_some_and(|s| {
            !crate::stats::cost_list(&self.defs, &self.defs.things[s.def], s.stuff).is_empty()
        });
        if costs {
            self.learn(i, "Construction", 0.25 * delta as f32);
        }
        if left <= 0.0 {
            self.finish_deconstructing(building);
            self.end_job(i, true);
            return;
        }
        if let Some(Job {
            kind: JobKind::Deconstruct { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = DeconstructStage::Work { work_left: left };
        }
    }

    /// `FinishedRemoving`: the building goes, leaving
    /// min(RoundRandom(count × fraction), count) of each cost.
    // COMPATIBILITY TODO: currently approximate — leavings are placed with
    // our simplified "near" placement, not dropped over the footprint in
    // random order.
    fn finish_deconstructing(&mut self, building: ItemId) {
        self.power_despawned(building);
        let Some(s) = self.map.remove_structure(building) else {
            return;
        };
        if self.defs.things[s.def].holds_roof {
            self.roof_holder_despawned(s.footprint);
        }
        self.map.deconstruct_designations.retain(|&d| d != building);
        self.reservations
            .release_all_for_target(Target::Item(building));
        for p in &mut self.pawns {
            if p.owned_bed == Some(building) {
                p.owned_bed = None;
            }
        }
        // The cells are free again before the leavings drop onto them.
        self.refresh_path_grid();
        let defs = self.defs.clone();
        let def = &defs.things[s.def];
        let fraction = def.resources_fraction_when_deconstructed;
        if fraction != 0.0 {
            for (thing, count) in cost_list(&defs, def, s.stuff) {
                let n = round_random(count as f32 * fraction, &mut self.rng).min(count);
                if n > 0 {
                    self.place_near(thing, n, s.footprint.center);
                }
            }
        }
        self.refresh_path_grid();
        self.rebuild_regions();
        self.light_key = None;
    }
}

/// The per-tick part of the deconstruction driver.
pub(super) fn tick_deconstruct(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::Deconstruct { building, stage } = pawn.job.as_ref()?.kind else {
        return None;
    };
    // `FailOnThingMissingDesignation`, and the building must still exist.
    if map.structure(building).is_none() || !map.deconstruct_designations.contains(&building) {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        DeconstructStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToDeconstruct
            }
        }
        DeconstructStage::Work { .. } => JobEvent::None,
    })
}
