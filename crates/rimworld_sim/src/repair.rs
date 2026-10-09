//! Repairing broken-down buildings (docs/research.md §55):
//! `WorkGiver_FixBrokenDownBuilding` sends a builder with a component to a
//! broken-down building in the home area.

use rimworld_defs::{DefId, JobDef, ThingDef};

use crate::job::{FixStage, Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `JobDriver_FixBrokenDownBuilding.TicksDuration`.
pub const FIX_TICKS: i32 = 1000;

/// `WorkGiver_FixBrokenDownBuilding`.
pub struct FixGiver<'a> {
    pub regions: &'a crate::region::Regions,
    pub component: Option<DefId<ThingDef>>,
    pub job: Option<DefId<JobDef>>,
}

impl FixGiver<'_> {
    /// `FindClosestComponent`: the nearest reachable, reservable,
    /// unforbidden industrial component.
    // COMPATIBILITY TODO: currently approximate — nearest by straight-line
    // distance with region connectivity instead of the game's search.
    fn closest_component(&self, ctx: &WorkContext<'_>) -> Option<ItemId> {
        let def = self.component?;
        ctx.map
            .items()
            .iter()
            .filter(|i| {
                i.def == def
                    && !i.forbidden
                    && self.regions.connected(ctx.position, i.position)
                    && ctx.reservations.can_reserve(
                        ctx.claimant,
                        Target::Item(i.id),
                        1,
                        1,
                        STACK_ALL,
                    )
            })
            .min_by_key(|i| {
                (i.position.x - ctx.position.x).pow(2) + (i.position.z - ctx.position.z).pow(2)
            })
            .map(|i| i.id)
    }
}

impl WorkGiver for FixGiver<'_> {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        !ctx.map.structures().iter().any(|s| s.power.broken_down)
    }

    /// `PotentialWorkThingsGlobal`: the broken-down buildings.
    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(
            ctx.map
                .structures()
                .iter()
                .filter(|s| s.power.broken_down)
                .map(|s| s.id)
                .collect(),
        )
    }

    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let s = ctx.map.structure(t)?;
        let repairable = ctx.defs.things[s.def]
            .building
            .as_ref()
            .is_some_and(|b| b.repairable);
        if !repairable
            || !s.power.broken_down
            || !ctx.map.home[s.footprint.center]
            || ctx.map.deconstruct_designations.contains(&t)
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
        {
            return None;
        }
        let component = self.closest_component(ctx)?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::FixBrokenDown {
                    building: t,
                    component,
                    stage: FixStage::GotoComponent,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
