//! Flicking switches (docs/research.md §55): the player marks a switchable
//! building (`CompFlickable.wantSwitchOn` and the Flick designation) and a
//! colonist doing basic work walks over and flips it (`WorkGiver_Flick`,
//! `JobDriver_Flick`).

use rimworld_defs::{DefId, JobDef};

use crate::job::{FlickStage, Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `JobDriver_Flick`'s wait.
pub const FLICK_TICKS: i32 = 15;

/// `WorkGiver_Flick`.
pub struct FlickGiver {
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for FlickGiver {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.flick_designations.is_empty()
    }

    /// `PotentialWorkThingsGlobal`: the designated buildings.
    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(ctx.map.flick_designations.clone())
    }

    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        ctx.map.structure(t)?;
        if !ctx.map.flick_designations.contains(&t)
            || !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
        {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Flick {
                    target: t,
                    stage: FlickStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
