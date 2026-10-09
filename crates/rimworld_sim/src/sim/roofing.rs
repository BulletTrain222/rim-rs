//! Roof building in the simulation: the auto-roof queue, the BuildRoof
//! driver (`JobDriver_BuildRoof`, docs/research.md §30) and its per-tick
//! checks.

use super::farming::Touch;
use super::{JobEvent, Sim, tick_movement};
use crate::job::{Job, JobKind, RoofStage};
use crate::map::Map;
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::roof::{
    ADJACENT_AND_INSIDE, auto_roof_area, blocking_plant, connected_to_roof_holder, room_signature,
    within_range_of_roof_holder,
};
use rimworld_defs::GameDefs;

/// `JobDriver_AffectRoof.BaseWorkAmount`.
const ROOF_WORK: f32 = 65.0;

impl Sim {
    /// After the rooms were rebuilt: rooms with a new shape are queued for
    /// the auto-roof check (`Room.Notify_RoomShapeChanged`).
    pub(super) fn queue_changed_rooms(&mut self) {
        let mut seen = std::collections::HashSet::new();
        for room in self.regions.rooms() {
            if room.is_empty() {
                continue;
            }
            let sig = room_signature(&self.regions, &room);
            seen.insert(sig);
            if !self.room_signatures.contains(&sig) {
                self.queued_roof_rooms.push(sig);
            }
        }
        self.room_signatures = seen;
    }

    /// `AutoBuildRoofAreaSetterTick_First`: the queued rooms get their
    /// build roof area at the start of the next tick.
    pub(super) fn resolve_queued_roofs(&mut self) {
        if self.queued_roof_rooms.is_empty() {
            return;
        }
        let queued = std::mem::take(&mut self.queued_roof_rooms);
        let defs = self.defs.clone();
        for room in self.regions.rooms() {
            if !room.is_empty() && queued.contains(&room_signature(&self.regions, &room)) {
                auto_roof_area(&mut self.map, &defs, &self.regions, &room);
            }
        }
    }

    /// Debug tool: resolves queued auto-roof checks now (a replay whose
    /// rooms were built before it starts).
    pub fn debug_resolve_queued_roofs(&mut self) {
        self.resolve_queued_roofs();
    }

    /// The job starts: walk to touch the cell, or start at once.
    pub(super) fn begin_roof(&mut self, i: usize) -> bool {
        let Some(JobKind::BuildRoof { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        match self.walk_to_touch(i, cell, false) {
            Touch::Here => {
                self.start_roofing(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    /// The work toil starts with 65 work to do.
    pub(super) fn start_roofing(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::BuildRoof { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RoofStage::Work {
                work_left: ROOF_WORK,
            };
        }
    }

    /// Roof work per interval: `ConstructionSpeed × 1.7 × delta`; when it
    /// is done the roof goes up (`DoEffect`) and the job ends.
    // Roof work teaches nothing (`JobDriver_AffectRoof` has no learning).
    pub(super) fn roof_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::BuildRoof {
                    cell,
                    stage: RoofStage::Work { work_left },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let speed = self.pawn_stat_of(i, "ConstructionSpeed");
        let left = work_left - speed * 1.7 * delta as f32;
        if left <= 0.0 {
            self.put_up_roof(cell);
            self.end_job(i, true);
            return;
        }
        if let Some(Job {
            kind: JobKind::BuildRoof { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RoofStage::Work { work_left: left };
        }
    }

    /// `JobDriver_BuildRoof.DoEffect`: the cell and its eight neighbours get
    /// a roof where they are in the build roof area, unroofed, held up and
    /// free of trees.
    fn put_up_roof(&mut self, cell: crate::grid::Cell) {
        let defs = self.defs.clone();
        for d in ADJACENT_AND_INSIDE {
            let c = cell + d;
            if !self.map.size().contains(c)
                || !self.map.build_roof(c)
                || self.map.roofed(c)
                || !within_range_of_roof_holder(&self.map, &defs, c, false)
                || blocking_plant(&self.map, &defs, c).is_some()
            {
                continue;
            }
            self.map.set_roof(c, true);
        }
    }
}

/// The per-tick part of the roof driver: the job's fail conditions, then
/// walking.
pub(super) fn tick_roof(
    pawn: &mut Pawn,
    map: &Map,
    defs: &GameDefs,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::BuildRoof { cell, stage } = pawn.job.as_ref()?.kind else {
        return None;
    };
    if !map.build_roof(cell)
        || !within_range_of_roof_holder(map, defs, cell, false)
        || !connected_to_roof_holder(map, defs, cell, true)
    {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        RoofStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToRoof
            }
        }
        // `DoWorkFailOn`: someone else roofed it.
        RoofStage::Work { .. } if map.roofed(cell) => JobEvent::Failed,
        RoofStage::Work { .. } => JobEvent::None,
    })
}
