//! Refuelling (docs/research.md §32): `WorkGiver_Refuel` sends a hauler to
//! bring fuel to a lamp (or other refuelable building) once it is down to
//! 30% of its target fuel.

use rimworld_defs::{DefId, GameDefs, JobDef, RefuelableProperties};

use crate::job::{Job, JobKind, RefuelStage};
use crate::map::{ItemId, Structure};
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `CompRefuelable.IsFull`: within one unit of the target (the capacity;
/// the fuel multiplier is 1).
pub fn is_full(props: &RefuelableProperties, fuel: f32) -> bool {
    props.capacity - fuel < 1.0
}

/// `ShouldAutoRefuelNow`: at most 30% of the target (`autoRefuelPercent`)
/// and not full.
pub fn should_auto_refuel(props: &RefuelableProperties, fuel: f32) -> bool {
    props.capacity > 0.0 && fuel / props.capacity <= 0.3 && !is_full(props, fuel)
}

/// `GetFuelCountToFullyRefuel`: units of fuel to fill it, at least one.
pub fn fuel_count_to_fill(props: &RefuelableProperties, fuel: f32) -> i32 {
    ((props.capacity - fuel).ceil() as i32).max(1)
}

/// `WorkGiver_Refuel`.
pub struct RefuelGiver {
    pub job: Option<DefId<JobDef>>,
}

fn props<'d>(defs: &'d GameDefs, s: &Structure) -> Option<&'d RefuelableProperties> {
    defs.things[s.def].refuelable.as_ref()
}

impl WorkGiver for RefuelGiver {
    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(
            ctx.map
                .structures()
                .iter()
                .filter(|s| props(ctx.defs, s).is_some())
                .map(|s| s.id)
                .collect(),
        )
    }

    /// Refuelable buildings are stored in regions.
    fn region_request(&self) -> bool {
        true
    }

    // COMPATIBILITY TODO: currently approximate — fog,
    // switches, burning, deconstruct designations, target fuel levels,
    // difficulty fuel multipliers and atomic refuelling are not modelled;
    // the fuel search is the nearest reachable stack by straight-line
    // distance.
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let s = ctx.map.structure(t)?;
        let p = props(ctx.defs, s)?;
        if is_full(p, s.fuel) || !should_auto_refuel(p, s.fuel) {
            return None;
        }
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
        {
            return None;
        }
        // `FindBestFuel`: the closest reachable, reservable fuel.
        let fuel = ctx
            .map
            .items()
            .iter()
            .filter(|i| !i.is_filth() && !i.forbidden)
            .filter(|i| {
                p.fuel_defs
                    .iter()
                    .any(|d| ctx.defs.things[i.def].def_name == *d)
            })
            .filter(|i| {
                ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Item(i.id),
                    i.stack_count as i32,
                    1,
                    STACK_ALL,
                )
            })
            .filter(|i| ctx.can_reach(i.position))
            .min_by_key(|i| i.position.distance_squared(ctx.position))?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::Refuel {
                    building: t,
                    fuel: fuel.id,
                    count: 0,
                    stage: RefuelStage::GotoFuel,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
