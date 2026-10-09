//! Floor work (docs/research.md §39): `WorkGiver_ConstructRemoveFloor` and
//! `WorkGiver_ConstructSmoothFloor` scan the designated cells.

use rimworld_defs::{DefId, JobDef};

use crate::grid::Cell;
use crate::job::{Job, JobKind, RoofStage};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `JobDriver_RemoveFloor.BaseWorkAmount`.
pub const REMOVE_FLOOR_WORK: f32 = 200.0;
/// `JobDriver_SmoothFloor.BaseWorkAmount`.
pub const SMOOTH_FLOOR_WORK: f32 = 2800.0;

/// `WorkGiver_ConstructAffectFloor` for one designation.
pub struct AffectFloorGiver<'a> {
    pub regions: &'a crate::region::Regions,
    pub smooth: bool,
    pub job: Option<DefId<JobDef>>,
}

impl AffectFloorGiver<'_> {
    fn cells(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        if self.smooth {
            ctx.map.smooth_floor_designations.clone()
        } else {
            ctx.map.remove_floor_designations.clone()
        }
    }
}

impl WorkGiver for AffectFloorGiver<'_> {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        self.cells(ctx).is_empty()
    }

    fn potential_cells(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        self.cells(ctx)
    }

    // COMPATIBILITY TODO: currently approximate — buildings whose support
    // the floor provides (`AnyBuildingBlockingFloorRemoval`) are not
    // checked; touch reachability is region connectivity of a walkable
    // cell on or next to it.
    fn job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> Option<(Job, Vec<ItemId>)> {
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Floor(c), 1, 1, STACK_ALL)
        {
            return None;
        }
        if !self.smooth && !ctx.map.can_remove_top_layer(ctx.defs, c) {
            return None;
        }
        let touchable = crate::roof::ADJACENT_AND_INSIDE.iter().any(|&d| {
            let n = c + d;
            ctx.map.size().contains(n)
                && ctx.grid.walkable(n)
                && crate::roof::touch_allowed(ctx.grid, ctx.map, n, c)
                && (n == ctx.position || self.regions.connected(ctx.position, n))
        });
        if !touchable {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::AffectFloor {
                    cell: c,
                    smooth: self.smooth,
                    stage: RoofStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// `JobDriver_SmoothWall.BaseWorkAmount`.
pub const SMOOTH_WALL_WORK: f32 = 6500.0;

/// The rock on `c` can be smoothed (`IsSmoothable`: it names a
/// `smoothedThing`).
pub fn smoothable_wall(map: &crate::map::Map, defs: &rimworld_defs::GameDefs, c: Cell) -> bool {
    map.size().contains(c)
        && map.buildings[c].is_some_and(|b| {
            defs.things[b]
                .building
                .as_ref()
                .is_some_and(|p| p.smoothed_thing.is_some())
        })
}

/// `WorkGiver_ConstructSmoothWall`: a cell scanner over the designated
/// walls.
pub struct SmoothWallGiver<'a> {
    pub regions: &'a crate::region::Regions,
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for SmoothWallGiver<'_> {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.smooth_wall_designations.is_empty()
    }

    fn potential_cells(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        ctx.map.smooth_wall_designations.clone()
    }

    // COMPATIBILITY TODO: currently approximate — touch reachability is
    // region connectivity of a walkable cell next to it.
    fn job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> Option<(Job, Vec<ItemId>)> {
        if !smoothable_wall(ctx.map, ctx.defs, c)
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Cell(c), 1, 1, STACK_ALL)
        {
            return None;
        }
        let touchable = crate::roof::ADJACENT_AND_INSIDE.iter().any(|&d| {
            let n = c + d;
            ctx.map.size().contains(n)
                && ctx.grid.walkable(n)
                && crate::roof::touch_allowed(ctx.grid, ctx.map, n, c)
                && (n == ctx.position || self.regions.connected(ctx.position, n))
        });
        if !touchable {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::SmoothWall {
                    cell: c,
                    stage: RoofStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
