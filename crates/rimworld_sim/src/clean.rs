//! Cleaning (docs/research.md §20): the `CleanFilth` work giver.
//!
//! Filth inside the home area that is at least 600 ticks old and can be
//! reserved is cleaned. A job collects up to 15 such filth around the
//! target in the same room (in the game's radial order), sorted by
//! distance when there are at least five.

use std::sync::OnceLock;

use crate::grid::Cell;
use crate::job::{CleanStage, Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::region::Regions;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// Filth must be this old (ticks since it appeared or thickened).
pub const MIN_TICKS_SINCE_THICKENED: u64 = 600;
/// A cleaning job collects at most this many filth.
const MAX_QUEUE: usize = 15;
/// Radial cells searched around the first filth.
const QUEUE_SEARCH_CELLS: usize = 100;
/// From this many queued filth on, the queue is sorted by distance.
const SORT_QUEUE_FROM: usize = 5;

/// The game's radial pattern (`GenRadial.RadialPattern`): the offsets of
/// (-80..80)² in x-major order, sorted by squared distance with .NET's
/// unstable sort (which fixes the order among equal distances), first
/// 20,000.
pub fn radial_pattern() -> &'static [Cell] {
    static PATTERN: OnceLock<Vec<Cell>> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let mut cells: Vec<Cell> = (-80..80)
            .flat_map(|x| (-80..80).map(move |z| Cell::new(x, z)))
            .collect();
        crate::netsort::sort(&mut cells, |a, b| {
            (a.x * a.x + a.z * a.z).cmp(&(b.x * b.x + b.z * b.z))
        });
        cells.truncate(20_000);
        cells
    })
}

/// `WorkGiver_CleanFilth`.
pub struct CleanFilth<'a> {
    pub regions: &'a Regions,
    pub clean_job: Option<rimworld_defs::DefId<rimworld_defs::JobDef>>,
}

impl CleanFilth<'_> {
    fn job_for(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let first = ctx.map.item(t)?;
        let mut queue = vec![t];
        let room = self.regions.room_at(first.position);
        for &d in &radial_pattern()[..QUEUE_SEARCH_CELLS] {
            let c = first.position + d;
            if !ctx.map.size().contains(c) || self.regions.room_at(c) != room {
                continue;
            }
            for item in ctx.map.items_at(c) {
                if item.id != t && self.has_job_on_thing(ctx, item.id) {
                    queue.push(item.id);
                }
            }
            if queue.len() >= MAX_QUEUE {
                break;
            }
        }
        if queue.len() >= SORT_QUEUE_FROM {
            // `SortBy` (.NET `List.Sort`, unstable).
            let at = ctx.position;
            let key = |id: &ItemId| {
                ctx.map.item(*id).map_or(i32::MAX, |i| {
                    let (dx, dz) = (i.position.x - at.x, i.position.z - at.z);
                    dx * dx + dz * dz
                })
            };
            crate::netsort::sort(&mut queue, |a, b| key(a).cmp(&key(b)));
        }
        Some((
            Job {
                def: self.clean_job,
                kind: JobKind::Clean {
                    target: None,
                    stage: CleanStage::Extract,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            queue,
        ))
    }
}

impl WorkGiver for CleanFilth<'_> {
    /// Blueprints, frames and filth are stored in regions.
    fn region_request(&self) -> bool {
        true
    }

    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.filth_in_home().is_empty()
    }

    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(ctx.map.filth_in_home().to_vec())
    }

    fn thing_request(&self, ctx: &WorkContext<'_>, item: ItemId) -> bool {
        ctx.map.item(item).is_some_and(|i| i.is_filth())
    }

    // COMPATIBILITY TODO: currently approximate — fog is not modelled.
    fn has_job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> bool {
        let Some(item) = ctx.map.item(t) else {
            return false;
        };
        item.is_filth()
            && ctx.map.home[item.position]
            && ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
            && ctx.tick as i64 - item.grow_tick >= MIN_TICKS_SINCE_THICKENED as i64
    }

    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        self.job_for(ctx, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_pattern_starts_at_the_centre() {
        let p = radial_pattern();
        assert_eq!(p[0], Cell::new(0, 0));
        assert!(p[1..5].iter().all(|c| c.x.abs() + c.z.abs() == 1));
        assert!(p[5..9].iter().all(|c| c.x.abs() == 1 && c.z.abs() == 1));
        assert_eq!(p.len(), 20_000);
    }
}
