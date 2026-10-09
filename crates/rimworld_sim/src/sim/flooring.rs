//! Floor work in the simulation: designating floors for removal or
//! smoothing, and the `JobDriver_AffectFloor` driver (docs/research.md
//! §39).

use super::farming::Touch;
use super::{JobEvent, Sim, tick_movement};
use crate::floorwork::{REMOVE_FLOOR_WORK, SMOOTH_FLOOR_WORK, SMOOTH_WALL_WORK, smoothable_wall};
use crate::grid::Cell;
use crate::job::{Job, JobKind, RoofStage};
use crate::map::Map;
use crate::path::PathGrid;
use crate::pawn::Pawn;

impl Sim {
    /// `Designator_RemoveFloor`: cells with a removable floor. Returns how
    /// many were designated.
    // COMPATIBILITY TODO: currently approximate — fog and buildings that
    // need the floor's support are not checked.
    pub fn designate_remove_floor(&mut self, cells: &[Cell]) -> usize {
        let mut n = 0;
        for &c in cells {
            if !self.map.size().contains(c)
                || self.map.remove_floor_designations.contains(&c)
                || !self.map.can_remove_top_layer(&self.defs, c)
                || self.full_impassable_edifice(c)
            {
                continue;
            }
            self.map.remove_floor_designations.push(c);
            n += 1;
        }
        n
    }

    /// `Designator_SmoothSurface` for floors: cells whose terrain affords
    /// smoothing. Returns how many were designated.
    // COMPATIBILITY TODO: currently approximate — wall smoothing and the
    // rules for smoothing under buildings are not modelled (any building
    // prevents it).
    pub fn designate_smooth_floor(&mut self, cells: &[Cell]) -> usize {
        let mut n = 0;
        for &c in cells {
            if !self.map.size().contains(c)
                || self.map.smooth_floor_designations.contains(&c)
                || self.map.buildings[c].is_some()
            {
                continue;
            }
            let t = &self.defs.terrain[self.map.terrain[c]];
            if !t.affordances.iter().any(|a| a == "SmoothableStone") || t.smoothed_terrain.is_none()
            {
                continue;
            }
            self.map.smooth_floor_designations.push(c);
            n += 1;
        }
        n
    }

    /// `Designator_SmoothSurface`: smoothable rock walls get a SmoothWall
    /// designation (dropping a Mine one), other cells are smoothed as
    /// floors. Returns how many were designated.
    pub fn designate_smooth_surface(&mut self, cells: &[Cell]) -> usize {
        let mut n = 0;
        let mut floors = Vec::new();
        for &c in cells {
            if smoothable_wall(&self.map, &self.defs, c) {
                if !self.map.smooth_wall_designations.contains(&c) {
                    self.map.smooth_wall_designations.push(c);
                    self.map.mine_designations.retain(|&m| m != c);
                    n += 1;
                }
            } else {
                floors.push(c);
            }
        }
        n + self.designate_smooth_floor(&floors)
    }

    /// The wall job starts: walk to touch the cell (`GotoCell` Touch).
    pub(super) fn begin_smooth_wall(&mut self, i: usize) -> bool {
        let Some(JobKind::SmoothWall { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        match self.walk_to_touch(i, cell, false) {
            Touch::Here => {
                self.start_smooth_wall(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    pub(super) fn start_smooth_wall(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::SmoothWall { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RoofStage::Work {
                work_left: SMOOTH_WALL_WORK,
            };
        }
    }

    /// Wall smoothing per interval: SmoothingSpeed × 1.7 × delta; done, the
    /// rock is replaced by its smoothed wall (`SmoothableWallUtility.SmoothWall`).
    // COMPATIBILITY TODO: currently approximate — the smoothed wall is
    // natural-rock-like (no faction).
    pub(super) fn smooth_wall_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::SmoothWall {
                    cell,
                    stage: RoofStage::Work { work_left },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let left = work_left - self.pawn_stat_of(i, "SmoothingSpeed") * 1.7 * delta as f32;
        self.learn(i, "Construction", 0.1 * delta as f32);
        if left <= 0.0 {
            let smoothed = self.map.buildings[cell]
                .and_then(|b| self.defs.things[b].building.as_ref())
                .and_then(|p| p.smoothed_thing.as_deref())
                .and_then(|t| self.defs.things.id(t));
            if let Some(smoothed) = smoothed {
                self.map.buildings[cell] = Some(smoothed);
                self.map.mined.retain(|m| m.0 != cell);
                self.map.bump_structure_revision();
            }
            self.map.smooth_wall_designations.retain(|&c| c != cell);
            self.end_job(i, true);
            return;
        }
        if let Some(Job {
            kind: JobKind::SmoothWall { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RoofStage::Work { work_left: left };
        }
    }

    pub(super) fn full_impassable_edifice(&self, c: Cell) -> bool {
        self.map.buildings[c].is_some_and(|b| {
            let d = &self.defs.things[b];
            d.fill_percent > 0.99 && d.passability == rimworld_defs::Passability::Impassable
        })
    }

    /// The job starts: walk to touch the cell (`GotoCell` Touch).
    pub(super) fn begin_affect_floor(&mut self, i: usize) -> bool {
        let Some(JobKind::AffectFloor { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        match self.walk_to_touch(i, cell, false) {
            Touch::Here => {
                self.start_affect_floor(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    /// The work toil starts with its base work.
    pub(super) fn start_affect_floor(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::AffectFloor { stage, smooth, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            let work = if *smooth {
                SMOOTH_FLOOR_WORK
            } else {
                REMOVE_FLOOR_WORK
            };
            *stage = RoofStage::Work { work_left: work };
        }
    }

    /// Work per interval: the speed stat (ConstructionSpeed to remove,
    /// SmoothingSpeed to smooth) × 1.7 × delta; then the effect, and the
    /// designation goes.
    // COMPATIBILITY TODO: currently approximate — snow is not modelled.
    pub(super) fn affect_floor_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::AffectFloor {
                    cell,
                    smooth,
                    stage: RoofStage::Work { work_left },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let stat = if smooth {
            "SmoothingSpeed"
        } else {
            "ConstructionSpeed"
        };
        let left = work_left - self.pawn_stat_of(i, stat) * 1.7 * delta as f32;
        self.learn(i, "Construction", 0.1 * delta as f32);
        if left <= 0.0 {
            if smooth {
                self.smooth_floor(cell);
                self.map.smooth_floor_designations.retain(|&c| c != cell);
            } else {
                self.remove_floor(cell);
                self.map.remove_floor_designations.retain(|&c| c != cell);
            }
            self.end_job(i, true);
            return;
        }
        if let Some(Job {
            kind: JobKind::AffectFloor { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = RoofStage::Work { work_left: left };
        }
    }

    /// `JobDriver_RemoveFloor.DoEffect`: `RemoveTopLayer` leaves part of
    /// the floor's cost (`GenLeaving.DoLeavingsFor`: RoundRandom(count ×
    /// fraction) of each) near the cell, then the terrain beneath returns.
    // COMPATIBILITY TODO: currently approximate — leavings use our
    // simplified "near" placement.
    fn remove_floor(&mut self, c: Cell) {
        if !self.map.can_remove_top_layer(&self.defs, c) {
            return;
        }
        let defs = self.defs.clone();
        let floor = &defs.terrain[self.map.terrain[c]];
        for (thing, count) in crate::stats::terrain_cost_list(&defs, floor) {
            let n = crate::plant::round_random(
                count as f32 * floor.resources_fraction_when_deconstructed,
                &mut self.rng,
            );
            if n > 0 {
                self.place_near(thing, n, c);
            }
        }
        self.map.remove_top_layer(c);
        self.remove_all_filth(c);
        self.refresh_path_grid();
    }

    /// `JobDriver_SmoothFloor.DoEffect`.
    fn smooth_floor(&mut self, c: Cell) {
        let defs = self.defs.clone();
        let Some(smooth) = defs.terrain[self.map.terrain[c]]
            .smoothed_terrain
            .as_deref()
            .and_then(|s| defs.terrain.id(s))
        else {
            return;
        };
        self.map.set_terrain_layered(&defs, c, smooth);
        self.remove_all_filth(c);
        self.refresh_path_grid();
    }

    /// `FilthMaker.RemoveAllFilth`.
    pub(super) fn remove_all_filth(&mut self, c: Cell) {
        let filth: Vec<crate::map::ItemId> = self
            .map
            .items_at(c)
            .filter(|i| i.is_filth())
            .map(|i| i.id)
            .collect();
        for f in filth {
            self.map.take_from_item(f, u32::MAX);
            self.reservations
                .release_all_for_target(crate::reservation::Target::Item(f));
        }
    }
}

/// The per-tick part: fail without the designation, then walking.
pub(super) fn tick_affect_floor(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::AffectFloor {
        cell,
        smooth,
        stage,
    } = pawn.job.as_ref()?.kind
    else {
        return None;
    };
    let designated = if smooth {
        map.smooth_floor_designations.contains(&cell)
    } else {
        map.remove_floor_designations.contains(&cell)
    };
    if !designated {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        RoofStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToFloor
            }
        }
        RoofStage::Work { .. } => JobEvent::None,
    })
}

/// The per-tick part of wall smoothing: fail without the designation or
/// the rock, then walking.
pub(super) fn tick_smooth_wall(
    pawn: &mut Pawn,
    map: &Map,
    defs: &rimworld_defs::GameDefs,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::SmoothWall { cell, stage } = pawn.job.as_ref()?.kind else {
        return None;
    };
    if !map.smooth_wall_designations.contains(&cell) || !smoothable_wall(map, defs, cell) {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        RoofStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToWall
            }
        }
        RoofStage::Work { .. } => JobEvent::None,
    })
}
