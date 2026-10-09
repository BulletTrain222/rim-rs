//! Research (docs/research.md §56): `WorkGiver_Researcher` sends a
//! researcher to the best research bench while a project is selected.

use rimworld_defs::{DefId, JobDef};

use crate::grid::Cell;
use crate::job::{Job, JobKind, ResearchStage};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `JobDriver_Research.JobEndInterval`: the research toil's length.
pub const RESEARCH_TOIL_TICKS: i32 = 4000;
/// `ResearchManager.ResearchPerformed`'s points per unit of speed.
pub const RESEARCH_POINTS_PER_SPEED: f32 = 0.00825;
/// Intellectual experience per tick of research.
pub const RESEARCH_XP_PER_TICK: f32 = 0.1;

/// A research bench, as the work giver sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchBench {
    pub id: ItemId,
    /// Where the researcher stands (`InteractionCell`).
    pub cell: Cell,
    /// `CanBeResearchedAt(bench, ignoreResearchBenchPowerStatus: false)`
    /// for the current project.
    pub usable: bool,
    /// `ResearchSpeedFactor` (the giver's priority).
    pub speed_factor: f32,
}

/// `WorkGiver_Researcher`; `benches` is empty while no project is chosen.
pub struct ResearchGiver<'a> {
    pub benches: &'a [ResearchBench],
    pub job: Option<DefId<JobDef>>,
}

impl ResearchGiver<'_> {
    fn bench(&self, t: ItemId) -> Option<&ResearchBench> {
        self.benches.iter().find(|b| b.id == t)
    }
}

impl WorkGiver for ResearchGiver<'_> {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        self.benches.is_empty()
    }

    /// `ThingRequestGroup.ResearchBench`.
    fn potential_things(&self, _ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(self.benches.iter().map(|b| b.id).collect())
    }

    fn prioritized(&self) -> bool {
        true
    }

    fn thing_priority(&self, _ctx: &WorkContext<'_>, t: ItemId) -> f32 {
        self.bench(t).map_or(0.0, |b| b.speed_factor)
    }

    // COMPATIBILITY TODO: currently approximate — the interaction cell is
    // reserved as a cell (`ReserveSittableOrSpot` would reserve a chair
    // standing there).
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let b = self.bench(t)?;
        if !b.usable
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Cell(b.cell), 1, 1, STACK_ALL)
        {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Research {
                    bench: t,
                    cell: b.cell,
                    stage: ResearchStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// `ResearchProjectDef.CostFactor`: projects above the researcher's tech
/// level (capped at Industrial) cost half again per level.
pub fn cost_factor(project_tech: i32, researcher_tech: i32) -> f32 {
    if project_tech == 0 {
        return 1.0;
    }
    let level = project_tech.min(4);
    if researcher_tech >= level {
        1.0
    } else {
        1.0 + (level - researcher_tech) as f32 * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_factor_by_tech_level() {
        // Industrial colony: nothing costs more (Spacer is capped).
        assert_eq!(cost_factor(4, 4), 1.0);
        assert_eq!(cost_factor(5, 4), 1.0);
        // Tribal (Neolithic) colony.
        assert_eq!(cost_factor(3, 2), 1.5);
        assert_eq!(cost_factor(4, 2), 2.0);
        assert_eq!(cost_factor(6, 2), 2.0);
        assert_eq!(cost_factor(2, 2), 1.0);
        assert_eq!(cost_factor(0, 2), 1.0);
    }
}
