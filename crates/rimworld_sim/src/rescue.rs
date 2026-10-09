//! Rescuing downed colonists (docs/research.md §44):
//! `WorkGiver_RescueDowned` sends a doctor to carry a downed colonist who
//! isn't in bed to a bed found for them.

use rimworld_defs::{DefId, JobDef};

use crate::grid::Cell;
use crate::job::{Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::pawn::PawnId;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// A downed colonist waiting for rescue and the bed found for them
/// (`RestUtility.FindBedFor(patient, rescuer)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RescueCandidate {
    pub patient: PawnId,
    pub cell: Cell,
    pub bed: ItemId,
    /// The bed's sleeping slots (its reservation stack).
    pub slots: i32,
}

/// `WorkGiver_RescueDowned`.
pub struct RescueGiver<'a> {
    pub candidates: &'a [RescueCandidate],
    pub regions: &'a crate::region::Regions,
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for RescueGiver<'_> {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        self.candidates.is_empty()
    }

    /// The nearest patient (straight-line order, as `ClosestThingReachable`
    /// over the global list) that can be reserved and reached, with a bed
    /// that can be reserved.
    // COMPATIBILITY TODO: currently approximate — forbidden patients and
    // nearby enemies are not checked; the global search is by straight
    // line distance with region connectivity.
    fn non_scan_job(&self, ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        let mut order: Vec<&RescueCandidate> = self.candidates.iter().collect();
        order.sort_by_key(|c| {
            (c.cell.x - ctx.position.x).pow(2) + (c.cell.z - ctx.position.z).pow(2)
        });
        let c = order.into_iter().find(|c| {
            c.patient != ctx.claimant.pawn
                && ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Pawn(c.patient),
                    1,
                    1,
                    STACK_ALL,
                )
                && ctx
                    .reservations
                    .can_reserve(ctx.claimant, Target::Item(c.bed), 1, c.slots, 0)
                && self.regions.connected(ctx.position, c.cell)
        })?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::Rescue {
                    patient: c.patient,
                    bed: c.bed,
                    carrying: false,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// A bedridden colonist to feed, with the food found for them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedCandidate {
    pub patient: PawnId,
    pub cell: Cell,
    pub food: ItemId,
    /// Units to bring (`WillIngestStackCountOf`).
    pub count: u32,
}

/// `WorkGiver_FeedPatient` (humanlikes).
pub struct FeedGiver<'a> {
    pub candidates: &'a [FeedCandidate],
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for FeedGiver<'_> {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        self.candidates.is_empty()
    }

    /// The nearest patient (straight line) that can be reserved, with food
    /// that can be reserved.
    // COMPATIBILITY TODO: currently approximate — inventories, dispensers,
    // food policies and the game's scanner order are not modelled.
    fn non_scan_job(&self, ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        let mut order: Vec<&FeedCandidate> = self.candidates.iter().collect();
        order.sort_by_key(|c| {
            (c.cell.x - ctx.position.x).pow(2) + (c.cell.z - ctx.position.z).pow(2)
        });
        let c = order.into_iter().find(|c| {
            c.patient != ctx.claimant.pawn
                && ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Pawn(c.patient),
                    1,
                    1,
                    STACK_ALL,
                )
                && ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Item(c.food),
                    c.count as i32,
                    10,
                    1,
                )
        })?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::FeedPatient {
                    food: c.food,
                    patient: c.patient,
                    count: c.count,
                    stage: crate::job::FeedStage::GotoFood,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
