//! Downed pawns (the `Downed` think subtree, docs/research.md §54): a
//! downed colonist that can crawl crawls to a bed (`JobGiver_PatientGoToBed`
//! under `ThinkNode_CrawlInterrupt`, which puts a short break first);
//! otherwise it waits (`JobGiver_IdleForever`).

use super::Sim;
use crate::job::{Job, JobKind};
use crate::path::LocomotionUrgency;

/// `ThinkNode_CrawlInterrupt.ticksBetweenCrawlsRange`.
const CRAWL_BREAK_TICKS: (i32, i32) = (240, 480);
/// `ThinkNode_CrawlInterrupt.crawlDurationTicksRange`.
const CRAWL_TICKS: (i32, i32) = (720, 1440);
/// `JobGiver_IdleForever`'s wait.
const IDLE_FOREVER_TICKS: u32 = 2500;
/// `CanCrawl`: Manipulation needed.
const CRAWL_MIN_MANIPULATION: f32 = 0.15;

impl Sim {
    /// `Pawn_HealthTracker.CanCrawl`: a humanlike, awake, with
    /// Manipulation of at least 0.15 and no hediff that prevents crawling.
    // COMPATIBILITY TODO: currently approximate — age (at least 8) and
    // psychic ritual lords are not modelled.
    pub(super) fn can_crawl(&self, i: usize) -> bool {
        let p = &self.pawns[i];
        let humanlike = self.defs.things[p.race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            == Some("Humanlike");
        if !humanlike || p.asleep {
            return false;
        }
        let Some(view) = self.health_view(p.id) else {
            return false;
        };
        view.can_be_awake()
            && view.capacity("Manipulation") >= CRAWL_MIN_MANIPULATION
            && !p
                .health
                .hediffs
                .iter()
                .any(|h| self.defs.hediffs[h.def].prevents_crawling)
    }

    /// The `Downed` subtree for pawn `i` (downed, not carried, idle).
    // COMPATIBILITY TODO: currently approximate — fleeing danger and queued
    // jobs are not modelled.
    pub(super) fn downed_job(&mut self, i: usize) -> Job {
        if self.pawns[i].is_colonist && self.can_crawl(i) {
            let facts = self.patient_facts(i);
            if !self.in_bed(i)
                && let Some((bed, spot)) = facts.bed
            {
                // `ThinkNode_CrawlInterrupt`: a break, then the crawl.
                if self.pawns[i].crawl_break_next {
                    self.pawns[i].crawl_break_next = false;
                    let ticks = self
                        .rng
                        .range_inclusive(CRAWL_BREAK_TICKS.0, CRAWL_BREAK_TICKS.1);
                    return self.wait_downed(ticks as u32);
                }
                self.pawns[i].crawl_break_next = true;
                // The crawl's own expiry only re-thinks (the lying pawn keeps
                // its job), but it is drawn all the same.
                let _ = self.rng.range_inclusive(CRAWL_TICKS.0, CRAWL_TICKS.1);
                return Job {
                    def: self.job_defs.lay_down,
                    kind: JobKind::LayDown {
                        spot,
                        bed: Some(bed),
                    },
                    forced: false,
                    urgency: LocomotionUrgency::Jog,
                    start_tick: 0,
                };
            }
        }
        self.wait_downed(IDLE_FOREVER_TICKS)
    }

    fn wait_downed(&self, ticks: u32) -> Job {
        Job {
            def: self.job_defs.wait_downed,
            kind: JobKind::Wait {
                expiry_interval: ticks,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        }
    }
}
