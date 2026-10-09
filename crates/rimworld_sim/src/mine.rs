//! Mining (docs/research.md §33, §64): `WorkGiver_Miner` sends a miner to
//! the nearest reachable rock designated for mining.

use rimworld_defs::{DefId, JobDef};

use crate::construct::BuildView;
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::job::{Job, JobKind, MineStage, Rot4};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver, WorkTarget};

/// `MineAIUtility.MiningJobTicks`: the job's expiry.
pub const MINING_JOB_TICKS: i32 = 20_000;

/// `WorkGiver_Miner`.
pub struct MineGiver<'a> {
    pub view: BuildView<'a>,
    pub job: Option<DefId<JobDef>>,
    /// `HaulToCell`, for hauling a chunk aside.
    pub haul_job: Option<DefId<JobDef>>,
}

fn rock_footprint(c: Cell) -> Footprint {
    Footprint {
        center: c,
        rot: Rot4::North,
        size: (1, 1),
    }
}

impl MineGiver<'_> {
    /// `PotentialMineables`: designated cells (in designation order) with an
    /// in-bounds walkable neighbour and a mineable thing.
    fn potential(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        ctx.map
            .mine_designations
            .iter()
            .copied()
            .filter(|&c| {
                Cell::NEIGHBORS_8.iter().any(|&d| {
                    let n = c + d;
                    ctx.map.size().contains(n) && ctx.grid.walkable(n)
                }) && ctx.map.buildings[c].is_some_and(|b| ctx.defs.things[b].mineable)
            })
            .collect()
    }

    /// Reachable with `PathEndMode.Touch`: some walkable cell touching the
    /// rock that the miner can get to.
    fn reachable(&self, ctx: &WorkContext<'_>, rock: Cell) -> bool {
        let fp = rock_footprint(rock);
        Cell::NEIGHBORS_8.iter().any(|&d| {
            let n = rock + d;
            ctx.map.size().contains(n)
                && ctx.grid.walkable(n)
                && self.view.touches(n, &fp)
                && self.view.reachable(ctx.position, n)
        })
    }

    /// `MineAIUtility.JobOnThing`: designated, reservable; a standable
    /// neighbour that touches it gives a Mine job; failing that, the first
    /// walkable but unstandable touching neighbour — a haulable pass-through
    /// item there (a chunk) is hauled aside first, otherwise Mine.
    fn job_on_rock(&self, ctx: &WorkContext<'_>, rock: Cell) -> Option<(Job, Vec<ItemId>)> {
        if !ctx.map.mine_designations.contains(&rock)
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Cell(rock), 1, 1, STACK_ALL)
        {
            return None;
        }
        let fp = rock_footprint(rock);
        let touching = |n: Cell| ctx.map.size().contains(n) && self.view.touches(n, &fp);
        let standable = Cell::NEIGHBORS_8
            .iter()
            .any(|&d| touching(rock + d) && self.view.standable(rock + d));
        if !standable {
            let blocked = Cell::NEIGHBORS_8
                .iter()
                .map(|&d| rock + d)
                .find(|&n| touching(n) && ctx.grid.walkable(n) && !self.view.standable(n))?;
            let chunk = ctx.map.items_at(blocked).find(|it| {
                let d = &ctx.defs.things[it.def];
                d.designate_haulable && d.passability == rimworld_defs::Passability::PassThroughOnly
            });
            if let Some(it) = chunk
                && let Some(job) = self.view.haul_aside_job(ctx, self.haul_job, it.id)
            {
                return Some(job);
            }
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Mine {
                    cell: rock,
                    stage: MineStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

impl WorkGiver for MineGiver<'_> {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.mine_designations.is_empty()
    }

    /// A clicked rock: one of the potential mineables, and its job.
    fn job_on_target(
        &self,
        ctx: &WorkContext<'_>,
        target: WorkTarget,
    ) -> Option<Option<(Job, Vec<ItemId>)>> {
        let WorkTarget::Rock(c) = target else {
            return None;
        };
        if !self.potential(ctx).contains(&c) {
            return None;
        }
        Some(self.job_on_rock(ctx, c))
    }

    /// `ClosestThing_Global_Reachable` over the potential mineables
    /// (Touch, straight-line distance, strictly nearer wins) with the job
    /// check as validator.
    // COMPATIBILITY TODO: currently approximate — vein designations,
    // forbidden rock and danger are not modelled.
    fn non_scan_job(&self, ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        let mut best: Option<(i32, (Job, Vec<ItemId>))> = None;
        for c in self.potential(ctx) {
            let d = c.distance_squared(ctx.position);
            if best.as_ref().is_some_and(|(bd, _)| d >= *bd) {
                continue;
            }
            if !self.reachable(ctx, c) {
                continue;
            }
            if let Some(job) = self.job_on_rock(ctx, c) {
                best = Some((d, job));
            }
        }
        best.map(|(_, j)| j)
    }
}
