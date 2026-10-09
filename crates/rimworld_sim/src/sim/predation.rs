//! Predators (docs/research.md §65): the live-prey search when no food
//! lies about (`FoodUtility.BestPawnToHuntForPredator`, `IsAcceptablePreyFor`,
//! `GetPreyScoreFor`), the hunt (`JobDriver_PredatorHunt`: follow and attack
//! until the prey dies, then eat its corpse in meals until 90% fed) and
//! corpse meals (`Corpse.IngestedCalculateAmounts`).

use super::{JobEvent, Sim, tick_movement};
use crate::job::{Job, JobKind, PredatorStage};
use crate::map::{ItemId, Map};
use crate::path::{LocomotionUrgency, PathGrid};
use crate::pawn::{Pawn, PawnId};

/// `JobDriver_PredatorHunt.MaxHuntTicks`.
const MAX_HUNT_TICKS: u64 = 5000;
/// The hunt eats on while food is below this share.
const KEEP_EATING_BELOW: f32 = 0.9;
/// `GetMaxRegionsToScan` for a wild animal.
const WILD_PREY_REGIONS: usize = 30;

impl Sim {
    fn body_size_index(&self, i: usize) -> f32 {
        // COMPATIBILITY TODO: currently approximate — life stages don't
        // exist: every pawn has its adult body size (factor 1).
        self.defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .map_or(1.0, |r| r.base_body_size)
    }

    fn summary_health(&self, i: usize) -> f32 {
        if self.pawns[i].health.dead {
            return 0.0;
        }
        self.health_view(self.pawns[i].id)
            .map_or(1.0, |v| v.summary_health_percent())
    }

    /// A corpse's `Nutrition`: the corpse's base (5.2) × the body size ×
    /// the coverage of its natural parts not missing, 0 unless fresh flesh.
    pub(super) fn corpse_nutrition(&self, item: ItemId) -> f32 {
        let Some(t) = self.corpse_pawn(item).and_then(|p| self.index_of(p)) else {
            return 0.0;
        };
        let Some(it) = self.map.item(item) else {
            return 0.0;
        };
        let base = crate::stats::def_stat(&self.defs, &self.defs.things[it.def], None, "Nutrition");
        let mut v = base * self.body_size_index(t);
        v *= self
            .health_view(self.pawns[t].id)
            .map_or(1.0, |h| h.coverage_of_not_missing_natural_parts());
        let flesh = self.defs.things[it.def]
            .ingestible
            .as_ref()
            .is_some_and(|g| {
                g.preferability != rimworld_defs::FoodPreferability::NeverForNutrition
            });
        let fresh = self
            .rot_stage(item)
            .is_none_or(|s| s == super::RotStage::Fresh);
        if !flesh || !fresh {
            return 0.0;
        }
        v
    }

    /// `Corpse.IngestedCalculateAmounts`: the not-missing outside part
    /// whose nutrition (corpse nutrition × its coverage share) is closest
    /// to what is wanted (first on ties; the core if none) is eaten — a
    /// part leaves the corpse missing it, the core consumes the corpse.
    /// Returns the part's whole nutrition.
    pub(super) fn eat_corpse(&mut self, item: ItemId, wanted: f32) -> f32 {
        let Some(t) = self.corpse_pawn(item).and_then(|p| self.index_of(p)) else {
            return 0.0;
        };
        let nutrition = self.corpse_nutrition(item);
        let id = self.pawns[t].id;
        let (part, core, amount) = {
            let Some(view) = self.health_view(id) else {
                return 0.0;
            };
            let core = view
                .body
                .parts
                .iter()
                .position(|p| p.parent.is_none())
                .unwrap_or(0);
            let total = view.coverage_of_not_missing_natural_parts_from(core);
            let part_nutrition = |p: usize| {
                if total <= 0.0 {
                    0.0
                } else {
                    nutrition * (view.coverage_of_not_missing_natural_parts_from(p) / total)
                }
            };
            let mut best: Option<(usize, f32)> = None;
            for p in view.not_missing_parts() {
                if view.body.parts[p].depth != rimworld_defs::health::PartDepth::Outside {
                    continue;
                }
                let n = part_nutrition(p);
                if n <= 0.001 {
                    continue;
                }
                let d = (n - wanted).abs();
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((p, d));
                }
            }
            let part = best.map_or(core, |(p, _)| p);
            (part, core, part_nutrition(part))
        };
        if part == core {
            self.map.take_from_item(item, 1);
            self.corpses.remove(&item);
            self.reservations
                .release_all_for_target(crate::reservation::Target::Item(item));
            self.refresh_path_grid();
        } else {
            let defs = self.defs.clone();
            if let Some(body) = self.body_of(t).map(|b| b.0.clone()) {
                self.pawns[t].health.remove_part(&defs, &body, part);
            }
        }
        amount
    }

    /// `IsAcceptablePreyFor` for a wild predator: preyable flesh no bigger
    /// than its maximum prey size; standing prey at most twice its combat
    /// power and strictly weaker (power × health × size); not its own
    /// species if it herds; humanlikes allowed (Rough difficulty).
    fn acceptable_prey(&self, i: usize, t: usize) -> bool {
        let (p, q) = (&self.pawns[i], &self.pawns[t]);
        let Some(pr) = self.defs.things[p.race].race.as_ref() else {
            return false;
        };
        let Some(qr) = self.defs.things[q.race].race.as_ref() else {
            return false;
        };
        let flesh = qr.flesh_type.as_deref().is_none_or(|f| f != "Mechanoid");
        if !qr.can_be_predator_prey || !flesh || self.body_size_index(t) > pr.max_prey_body_size {
            return false;
        }
        if !q.health.downed {
            let (cp_p, cp_q) = (
                self.defs.pawn_kinds[p.kind].combat_power,
                self.defs.pawn_kinds[q.kind].combat_power,
            );
            if cp_q > 2.0 * cp_p {
                return false;
            }
            let prey = cp_q * self.summary_health(t) * self.body_size_index(t);
            let pred = cp_p * self.summary_health(i) * self.body_size_index(i);
            if prey >= pred {
                return false;
            }
        }
        // COMPATIBILITY TODO: currently approximate — factions are not
        // modelled (a wild predator skips the faction tests anyway); tame
        // colony animals don't exist.
        if pr.herd_animal && p.race == q.race {
            return false;
        }
        true
    }

    /// `GetPreyScoreFor`: −distance − 56 × health² × (prey power /
    /// predator power) × life-stage size factor (downed health counts at
    /// most 0.2); −35 more for humanlikes.
    pub(super) fn prey_score(&self, i: usize, t: usize) -> f32 {
        let (p, q) = (&self.pawns[i], &self.pawns[t]);
        let ratio =
            self.defs.pawn_kinds[q.kind].combat_power / self.defs.pawn_kinds[p.kind].combat_power;
        let mut health = self.summary_health(t);
        if q.health.downed {
            health = health.min(0.2);
        }
        let size_factor = 1.0f32;
        let len = (p.position.distance_squared(q.position) as f32).sqrt();
        let mut score = (0.0f64
            - len as f64
            - 56.0 * health as f64 * health as f64 * ratio as f64 * size_factor as f64)
            as f32;
        if !self.is_animal_index(t) {
            score -= 35.0;
        }
        score
    }

    /// `BestPawnToHuntForPredator`: with a melee attack, over at most 30
    /// regions from the predator (pawns of each region in turn), the
    /// highest-scoring acceptable, reachable pawn in its district (only
    /// downed ones below 25% health); the first keeps ties.
    // COMPATIBILITY TODO: currently approximate — the melee verb cache
    // (whose refresh draws a 0.04 chance and a weighted pick) is not
    // queried; region pawn lists are in pawn order, without the game's
    // duplicate entries for pawns on region borders.
    pub(super) fn best_prey_for(&self, i: usize) -> Option<usize> {
        let race = &self.defs.things[self.pawns[i].race];
        if race.tools.is_empty() {
            return None;
        }
        let only_downed = self.summary_health(i) < 0.25;
        let at = self.pawns[i].position;
        let root = self.regions.region_at(at)?;
        let room = self.regions.room_at(at);
        let mut order: Vec<usize> = Vec::new();
        self.regions.traverse(
            root,
            |_, r| {
                let k = self.regions.region(r).kind;
                k.passable() && k != crate::region::RegionType::Portal
            },
            |r| {
                for (k, p) in self.pawns.iter().enumerate() {
                    if !p.health.dead
                        && p.carried_by.is_none()
                        && self.regions.region_at(p.position) == Some(r)
                    {
                        order.push(k);
                    }
                }
                false
            },
            WILD_PREY_REGIONS,
        );
        let mut best: Option<(usize, f32)> = None;
        for t in order {
            if t == i
                || self.regions.room_at(self.pawns[t].position) != room
                || (only_downed && !self.pawns[t].health.downed)
                || !self.acceptable_prey(i, t)
                || !self.regions.connected(at, self.pawns[t].position)
            {
                continue;
            }
            let s = self.prey_score(i, t);
            if best.is_none_or(|(_, b)| s > b) {
                best = Some((t, s));
            }
        }
        best.map(|(t, _)| t)
    }

    /// Debug: the prey a pawn would hunt now and its score.
    pub fn debug_best_prey(&self, predator: PawnId) -> Option<(PawnId, f32)> {
        let i = self.index_of(predator)?;
        let t = self.best_prey_for(i)?;
        Some((self.pawns[t].id, self.prey_score(i, t)))
    }

    /// Debug: whether `prey` is acceptable prey for `predator`.
    pub fn debug_acceptable_prey(&self, predator: PawnId, prey: PawnId) -> bool {
        match (self.index_of(predator), self.index_of(prey)) {
            (Some(i), Some(t)) => self.acceptable_prey(i, t),
            _ => false,
        }
    }

    /// Debug: a corpse's current nutrition.
    pub fn debug_corpse_nutrition(&self, item: ItemId) -> f32 {
        self.corpse_nutrition(item)
    }

    /// The `PredatorHunt` job on `prey`.
    pub(super) fn predator_hunt_job(&self, prey: PawnId) -> Job {
        Job {
            def: self.defs.jobs.id("PredatorHunt"),
            kind: JobKind::PredatorHunt {
                prey,
                corpse: None,
                stage: PredatorStage::Follow,
                first_hit: true,
                start_tick: 0,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        }
    }

    /// Per tick during the chase: the follow toil's jumps (the prey dead or
    /// gone → its corpse) and its failure (more than 5000 ticks in and more
    /// than 2 cells away); then the path follower moves the predator.
    pub(super) fn predator_tick(&mut self, i: usize) {
        let Some(JobKind::PredatorHunt {
            prey,
            stage: PredatorStage::Follow,
            ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let start = self.pawns[i].job.as_ref().map_or(0, |j| j.start_tick);
        let gone = self
            .index_of(prey)
            .is_none_or(|t| self.pawns[t].health.dead);
        if gone {
            self.predator_prepare_corpse(i);
            return;
        }
        let t = self.index_of(prey).expect("prey");
        if self.tick > start + MAX_HUNT_TICKS
            && self.pawns[i]
                .position
                .distance_squared(self.pawns[t].position)
                > 4
        {
            self.end_job(i, false);
            return;
        }
        self.melee_tick(i);
    }

    /// The corpse step: the prey's spawned corpse becomes the target
    /// (forbidden: a wild predator's), else the hunt fails.
    fn predator_prepare_corpse(&mut self, i: usize) {
        let Some(JobKind::PredatorHunt { prey, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(corpse) = self.corpse_of(prey).filter(|&c| self.map.item(c).is_some()) else {
            self.end_job(i, false);
            return;
        };
        let defs = self.defs.clone();
        let forbid = !self.pawns[i].is_colonist;
        self.map.set_forbidden(&defs, corpse, forbid);
        self.set_predator(i, |c, s| {
            *c = Some(corpse);
            *s = PredatorStage::GotoCorpse;
        });
        self.predator_goto_corpse(i);
    }

    fn set_predator(&mut self, i: usize, f: impl FnOnce(&mut Option<ItemId>, &mut PredatorStage)) {
        if let Some(Job {
            kind: JobKind::PredatorHunt { corpse, stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            f(corpse, stage);
        }
    }

    /// `GotoThing(corpse, Touch)`.
    fn predator_goto_corpse(&mut self, i: usize) {
        let Some(JobKind::PredatorHunt {
            corpse: Some(c), ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(cell) = self.map.item(c).map(|it| it.position) else {
            self.end_job(i, false);
            return;
        };
        let after_meal = matches!(
            self.pawns[i].job.as_ref().map(|j| j.kind),
            Some(JobKind::PredatorHunt {
                stage: PredatorStage::Chew { .. },
                ..
            })
        );
        self.set_predator(i, |_, s| *s = PredatorStage::GotoCorpse);
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => self.predator_chew(i, !after_meal),
            super::farming::Touch::Walking => {}
            super::farming::Touch::NoPath => self.end_job(i, false),
        }
    }

    /// `ChewIngestible` on arrival: base ingest ticks / EatingSpeed, the
    /// first tick paid at once (the toil starts before the driver ticks).
    pub(super) fn predator_start_chew(&mut self, i: usize) {
        self.predator_chew(i, true);
    }

    /// `ChewIngestible`; `paid_now`: whether this tick's driver tick is
    /// still to come (on arrival) or already past (after a meal).
    fn predator_chew(&mut self, i: usize, paid_now: bool) {
        let Some(JobKind::PredatorHunt {
            corpse: Some(c), ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(def) = self.map.item(c).map(|it| it.def) else {
            self.end_job(i, false);
            return;
        };
        let speed = self.pawns[i].eating_speed * self.capacity_factor(i, "EatingSpeed");
        let ticks = self.defs.things[def].ingestible.as_ref().map_or(500, |g| {
            crate::food::chew_ticks(g.base_ingest_ticks, speed, g.use_eating_speed_stat)
        });
        let left = if paid_now { ticks - 1 } else { ticks };
        self.set_predator(i, |_, s| *s = PredatorStage::Chew { ticks_left: left });
        if left <= 0 {
            self.predator_finish_chew(i);
        }
    }

    /// `FinalizeIngest` on the corpse, then on to another meal while below
    /// 90% food (the corpse gone: the next approach fails).
    pub(super) fn predator_finish_chew(&mut self, i: usize) {
        let Some(JobKind::PredatorHunt {
            corpse: Some(c), ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(need) = self.pawns[i]
            .needs
            .get(crate::needs::NeedKind::Food)
            .cloned()
        else {
            self.end_job(i, false);
            return;
        };
        let gained = self.eat_corpse(c, need.max - need.level);
        if let Some(f) = self.pawns[i].needs.get_mut(crate::needs::NeedKind::Food) {
            f.level = (f.level + gained).clamp(0.0, f.max);
        }
        let pct = self.pawns[i]
            .needs
            .get(crate::needs::NeedKind::Food)
            .map_or(1.0, |f| f.percent());
        if pct < KEEP_EATING_BELOW {
            self.predator_goto_corpse(i);
        } else {
            self.end_job(i, true);
        }
    }
}

/// The hunt's per-tick driver after the chase: walking to the corpse, then
/// chewing.
pub(super) fn tick_predator(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let Some(JobKind::PredatorHunt { corpse, stage, .. }) = pawn.job.as_ref().map(|j| j.kind)
    else {
        return None;
    };
    match stage {
        PredatorStage::Follow => Some(JobEvent::None),
        PredatorStage::GotoCorpse => {
            if corpse.is_none_or(|c| map.item(c).is_none()) {
                return Some(JobEvent::Failed);
            }
            tick_movement(pawn, grid, map, t);
            Some(if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedAtCorpse
            })
        }
        PredatorStage::Chew { ticks_left } => {
            if corpse.is_none_or(|c| map.item(c).is_none()) {
                return Some(JobEvent::Failed);
            }
            let left = ticks_left - 1;
            if let Some(Job {
                kind: JobKind::PredatorHunt { stage, .. },
                ..
            }) = &mut pawn.job
            {
                *stage = PredatorStage::Chew { ticks_left: left };
            }
            Some(if left <= 0 {
                JobEvent::DoneChewingCorpse
            } else {
                JobEvent::None
            })
        }
    }
}
