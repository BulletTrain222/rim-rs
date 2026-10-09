//! Deconstruction (docs/research.md §34): `WorkGiver_Deconstruct` sends a
//! builder to the nearest reachable building designated for
//! deconstruction.

use rimworld_defs::{DefId, JobDef};

use crate::job::{DeconstructStage, Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `JobDriver_Deconstruct` work bounds.
pub const MIN_WORK: f32 = 20.0;
pub const MAX_WORK: f32 = 3000.0;

/// `WorkGiver_Deconstruct`.
pub struct DeconstructGiver {
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for DeconstructGiver {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.deconstruct_designations.is_empty()
    }

    /// `PotentialWorkThingsGlobal`: the designated buildings.
    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(ctx.map.deconstruct_designations.clone())
    }

    // COMPATIBILITY TODO: currently approximate — forbidding, minified
    // things, explosives and faction claims are not modelled.
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let s = ctx.map.structure(t)?;
        let deconstructible = ctx.defs.things[s.def]
            .building
            .as_ref()
            .is_some_and(|b| b.deconstructible);
        if !deconstructible
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
        {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Deconstruct {
                    building: t,
                    stage: DeconstructStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
