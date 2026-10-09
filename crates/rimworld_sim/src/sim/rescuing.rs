//! The rescue driver (`JobDriver_TakeToBed` for `Rescue`, docs/research.md
//! §44): walk onto the downed colonist, pick them up, carry them to touch
//! the bed and tuck them in.

use super::{JobEvent, Sim, tick_movement};
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::job::{FeedStage, Job, JobKind};
use crate::map::{ItemId, Map};
use crate::path::{COLONIST_HEURISTIC_STRENGTH, LocomotionUrgency, PathGrid};
use crate::pawn::Pawn;
use crate::rescue::RescueCandidate;

impl Sim {
    /// Whether pawn `i` lies in a bed now (`InBed`).
    pub(super) fn in_bed(&self, i: usize) -> bool {
        let p = &self.pawns[i];
        matches!(
            p.job.as_ref().map(|j| j.kind),
            Some(JobKind::LayDown { bed: Some(_), spot }) if p.position == spot
        )
    }

    /// Downed colonists who want rescue (`WantsToBeRescued`: downed, not in
    /// bed, not carried), each with the bed found for them
    /// (`FindBedFor(patient)`: their own bed if usable, else the nearest
    /// best bed).
    pub(super) fn rescue_candidates(&self, rescuer: usize) -> Vec<RescueCandidate> {
        let occupancy = self.bed_occupancy();
        let at = self.pawns[rescuer].position;
        let regions = &self.regions;
        let mut out = Vec::new();
        for (k, p) in self.pawns.iter().enumerate() {
            if k == rescuer
                || !p.is_colonist
                || !p.health.downed
                || p.health.dead
                || p.carried_by.is_some()
                || self.in_bed(k)
            {
                continue;
            }
            let bed = crate::rest::find_bed_for(
                &self.defs,
                &self.map,
                &|c| regions.connected(at, c),
                &self.reservations,
                self.claimant(k),
                p.position,
                p.owned_bed,
                &occupancy,
            );
            if let Some((bed, slots)) = bed.and_then(|b| {
                self.map
                    .structure(b)
                    .map(|s| (b, s.footprint.sleeping_slots()))
            }) {
                out.push(RescueCandidate {
                    patient: p.id,
                    cell: p.position,
                    bed,
                    slots,
                });
            }
        }
        out
    }

    /// The job starts: the patient claims the bed (`ClaimBedIfNonMedical`),
    /// then the rescuer walks onto them (`GotoThing` ClosestTouch).
    pub(super) fn begin_rescue(&mut self, i: usize) -> bool {
        let Some(JobKind::Rescue {
            patient,
            bed,
            carrying,
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(k) = self.index_of(patient) else {
            return false;
        };
        self.claim_bed(k, bed);
        if carrying {
            return self.walk_to_bed(i, bed);
        }
        let cell = self.pawns[k].position;
        self.walk_to(i, cell, true)
    }

    /// Walks pawn `i` to touch the bed (`GotoThing` Touch over its
    /// footprint, the heuristic aimed at its position).
    fn walk_to_bed(&mut self, i: usize, bed: ItemId) -> bool {
        let Some(fp) = self.map.structure(bed).map(|s| s.footprint) else {
            return false;
        };
        let tick = self.tick;
        let grid = &self.path_grid;
        let map = &self.map;
        let touches = |c: Cell| touches_footprint(grid, map, c, &fp);
        // The search uses the carrier's slower ticks per move (×0.6 speed).
        let costs = if self.pawns[i].carried_pawn.is_some() && self.pawns[i].base_move_speed > 0.0 {
            crate::path::MoveCosts::from_move_speed(
                self.pawns[i].base_move_speed * self.pawns[i].move_capacity_factor * 0.6,
            )
        } else {
            self.pawns[i].move_costs
        };
        let pawn = &mut self.pawns[i];
        pawn.path.clear();
        pawn.destination = None;
        let at = pawn.next_stop();
        if touches(at) {
            return true;
        }
        match crate::path::search(
            grid,
            at,
            fp.center,
            touches,
            costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) if !path.cells.is_empty() => {
                let end = *path.cells.last().expect("non-empty");
                pawn.path = path.cells.into();
                pawn.destination = Some(end);
                pawn.move_ready_tick = tick + super::PATH_START_LATENCY_TICKS;
                self.on_start_path(i, fp.center, true);
                true
            }
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// At the patient (`StartCarryThing`): pick them up and head for the
    /// bed. Fails if they got up or someone else carries them.
    pub(super) fn rescue_pick_up(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::Rescue { patient, bed, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let ok = self.index_of(patient).is_some_and(|k| {
            let p = &self.pawns[k];
            p.health.downed && !p.health.dead && p.carried_by.is_none()
        });
        if !ok || self.map.structure(bed).is_none() {
            self.end_job(i, false);
            return;
        }
        let k = self.index_of(patient).expect("checked");
        let rescuer = self.pawns[i].id;
        // Picked up, the patient stops whatever it was doing (crawling).
        self.cleanup_job(k);
        let p = &mut self.pawns[k];
        p.job = None;
        p.path.clear();
        p.step = None;
        p.destination = None;
        self.pawns[k].carried_by = Some(rescuer);
        self.pawns[i].carried_pawn = Some(patient);
        if let Some(Job {
            kind: JobKind::Rescue { carrying, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *carrying = true;
        }
        if !self.walk_to_bed(i, bed) {
            self.end_job(i, false);
            return;
        }
        if !self.pawns[i].is_moving() {
            self.rescue_tuck(i);
        }
    }

    /// At the bed (`TuckIntoBed`): the patient is laid in their sleeping
    /// slot and starts lying down there.
    // COMPATIBILITY TODO: currently approximate — the slot is the bed's
    // first free one, not looked up through the patient's ownership.
    pub(super) fn rescue_tuck(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::Rescue { patient, bed, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(k) = self.index_of(patient) else {
            self.end_job(i, false);
            return;
        };
        let Some(fp) = self.map.structure(bed).map(|s| s.footprint) else {
            self.end_job(i, false);
            return;
        };
        // `Toils_Reserve.Release(B)`: the patient reserves the bed next.
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        self.reservations
            .release(crate::reservation::Target::Item(bed), claimant.pawn, job_id);
        let occupied: Vec<Cell> = self
            .bed_occupancy()
            .into_iter()
            .filter(|o| o.1 == bed)
            .map(|o| o.2)
            .collect();
        let spot = (0..fp.sleeping_slots())
            .map(|s| fp.sleeping_slot(s))
            .find(|c| !occupied.contains(c))
            .unwrap_or_else(|| fp.sleeping_slot(0));
        self.pawns[i].carried_pawn = None;
        {
            let p = &mut self.pawns[k];
            p.carried_by = None;
            p.position = spot;
            p.step = None;
            p.path.clear();
            p.destination = None;
        }
        let lay_down = self.job_defs.lay_down;
        self.start_job(
            k,
            Job {
                def: lay_down,
                kind: JobKind::LayDown {
                    spot,
                    bed: Some(bed),
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        );
        self.end_job(i, true);
    }

    /// Debug tool: lays `patient` in `bed` (`Notify_TuckedIntoBed`) on its
    /// first free sleeping slot.
    pub fn debug_tuck_into_bed(&mut self, patient: crate::pawn::PawnId, bed: ItemId) {
        let Some(k) = self.index_of(patient) else {
            return;
        };
        let Some(fp) = self.map.structure(bed).map(|s| s.footprint) else {
            return;
        };
        let spot = fp.sleeping_slot(0);
        {
            let p = &mut self.pawns[k];
            p.position = spot;
            p.step = None;
            p.path.clear();
            p.destination = None;
        }
        self.claim_bed(k, bed);
        let lay_down = self.job_defs.lay_down;
        self.start_job(
            k,
            Job {
                def: lay_down,
                kind: JobKind::LayDown {
                    spot,
                    bed: Some(bed),
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        );
    }

    /// A carried patient is put down where the carrier stands (the job's
    /// finish action `TryDropCarriedThing`).
    pub(super) fn drop_carried_pawn(&mut self, i: usize) {
        let Some(patient) = self.pawns[i].carried_pawn.take() else {
            return;
        };
        let at = self.pawns[i].position;
        if let Some(k) = self.index_of(patient) {
            let p = &mut self.pawns[k];
            p.carried_by = None;
            p.position = at;
        }
    }

    /// Carried pawns move with their carrier.
    pub(super) fn sync_carried_pawn(&mut self, i: usize) {
        let Some(patient) = self.pawns[i].carried_pawn else {
            return;
        };
        let at = self.pawns[i].position;
        if let Some(k) = self.index_of(patient) {
            self.pawns[k].position = at;
        }
    }
}

impl Sim {
    /// Bedridden colonists to feed (`FeedPatientUtility`): lying in bed on
    /// medical rest (`ShouldSeekMedicalRest`), hungry (food at most the
    /// hungry threshold + 0.02), each with the best food for them as seen
    /// from the feeder.
    pub(super) fn feed_candidates(&self, feeder: usize) -> Vec<crate::rescue::FeedCandidate> {
        let mut out = Vec::new();
        for (k, p) in self.pawns.iter().enumerate() {
            if k == feeder
                || !p.is_colonist
                || p.health.dead
                || !self.in_bed(k)
                || !self.should_seek_medical_rest(k)
            {
                continue;
            }
            let Some(need) = p.needs.get(crate::needs::NeedKind::Food) else {
                continue;
            };
            if need.percent() > need.want_eat * 0.8 + 0.02 {
                continue;
            }
            let Some(race) = self.defs.things[p.race].race.as_ref() else {
                continue;
            };
            let humanlike = race.intelligence.as_deref() == Some("Humanlike");
            let eater = crate::food::Eater {
                race,
                humanlike,
                at: self.pawns[feeder].position,
                move_costs: self.pawns[feeder].move_costs,
                hunger: need.hunger_category(),
                temperature: Some(self.outdoor_temperature),
            };
            let claimant = self.claimant(feeder);
            let reservations = &self.reservations;
            let can_reserve = |item: &crate::map::Item| {
                reservations.can_reserve(
                    claimant,
                    crate::reservation::Target::Item(item.id),
                    item.stack_count as i32,
                    10,
                    1,
                )
            };
            let Some(food) = crate::food::best_food_source(
                &self.defs,
                &self.map,
                &self.path_grid,
                &eater,
                can_reserve,
            ) else {
                continue;
            };
            let def = &self.defs.things[self.map.item(food).map_or(p.race, |i| i.def)];
            let unit = crate::food::unit_nutrition(&self.defs, def);
            if unit <= 0.0 {
                continue;
            }
            let max_at_once = def
                .ingestible
                .as_ref()
                .map_or(0, |i| i.max_num_to_ingest_at_once);
            let count =
                crate::food::will_ingest_stack_count(need.max - need.level, unit, max_at_once);
            out.push(crate::rescue::FeedCandidate {
                patient: p.id,
                cell: p.position,
                food,
                count,
            });
        }
        out
    }

    /// The feeding job starts: walk to the food (`ClosestTouch`).
    pub(super) fn begin_feed(&mut self, i: usize) -> bool {
        let Some(JobKind::FeedPatient { food, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(cell) = self.map.item(food).map(|it| it.position) else {
            return false;
        };
        let at = self.pawns[i].next_stop();
        let Some(dest) =
            crate::food::touch_destination(&self.path_grid, cell, at, self.pawns[i].move_costs)
        else {
            return false;
        };
        self.walk_to(i, dest, true)
    }

    /// At the food (`PickupIngestible`): take the count and carry it to the
    /// patient (`GotoThing` Touch).
    pub(super) fn feed_pick_up(&mut self, i: usize) {
        let Some(Job {
            kind:
                JobKind::FeedPatient {
                    food,
                    patient,
                    count,
                    ..
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some((def, stack, rot, hp)) = self
            .map
            .item(food)
            .map(|it| (it.def, it.stack_count, it.rot, it.hit_points))
        else {
            self.end_job(i, false);
            return;
        };
        let taken = self.map.take_from_item(food, count.min(stack));
        let id = if taken < stack {
            self.map.allocate_item_id()
        } else {
            food
        };
        self.refresh_path_grid();
        self.pawns[i].carried = Some(crate::pawn::Carried {
            id,
            def,
            count: taken,
            rot,
            hit_points: hp,
        });
        set_feed_stage(&mut self.pawns[i], FeedStage::CarryToPatient);
        let Some(cell) = self.index_of(patient).map(|k| self.pawns[k].position) else {
            self.end_job(i, false);
            return;
        };
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => self.feed_start(i),
            super::farming::Touch::Walking => {}
            super::farming::Touch::NoPath => self.end_job(i, false),
        }
    }

    /// Next to the patient: they chew for round(baseIngestTicks × 1.5).
    pub(super) fn feed_start(&mut self, i: usize) {
        let Some(carried) = self.pawns[i].carried else {
            self.end_job(i, false);
            return;
        };
        let ticks = self.defs.things[carried.def]
            .ingestible
            .as_ref()
            .map_or(0, |ing| {
                (ing.base_ingest_ticks as f32 * 1.5).round_ties_even() as i32
            });
        // The toil starts on arrival and the driver pays its first tick in
        // the same tick (as for eating).
        set_feed_stage(
            &mut self.pawns[i],
            FeedStage::Feeding {
                ticks_left: ticks - 1,
            },
        );
        if ticks - 1 <= 0 {
            self.feed_finish(i);
        }
    }

    /// `FinalizeIngest` for the patient: they eat what they want of the
    /// carried food; any rest stays with the feeder (dropped at the end).
    pub(super) fn feed_finish(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::FeedPatient { patient, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        if let (Some(carried), Some(k)) = (self.pawns[i].carried, self.index_of(patient)) {
            let def = &self.defs.things[carried.def];
            let unit = crate::food::unit_nutrition(&self.defs, def);
            let max_at_once = def
                .ingestible
                .as_ref()
                .map_or(0, |ing| ing.max_num_to_ingest_at_once);
            if let Some(need) = self.pawns[k].needs.get_mut(crate::needs::NeedKind::Food) {
                let eaten = crate::food::ingested_count(
                    need.max - need.level,
                    unit,
                    carried.count,
                    max_at_once,
                );
                need.level = (need.level + eaten as f32 * unit).clamp(0.0, need.max);
                let left = carried.count - eaten.min(carried.count);
                self.pawns[i].carried = (left > 0).then_some(crate::pawn::Carried {
                    count: left,
                    ..carried
                });
            }
        }
        self.end_job(i, true);
    }
}

fn set_feed_stage(pawn: &mut Pawn, new: FeedStage) {
    if let Some(Job {
        kind: JobKind::FeedPatient { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new;
    }
}

/// The per-tick part of feeding: walking, then the patient's chewing.
pub(super) fn tick_feed(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::FeedPatient { stage, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    Some(match stage {
        FeedStage::GotoFood | FeedStage::CarryToPatient => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else if stage == FeedStage::GotoFood {
                JobEvent::ArrivedAtFoodForPatient
            } else {
                JobEvent::ArrivedToFeed
            }
        }
        FeedStage::Feeding { ticks_left } => {
            let left = ticks_left - 1;
            set_feed_stage(pawn, FeedStage::Feeding { ticks_left: left });
            if left <= 0 {
                JobEvent::FedPatient
            } else {
                JobEvent::None
            }
        }
    })
}

/// Touching a footprint: on or next to it, a diagonal corner needing a
/// walkable side cell that isn't a door.
fn touches_footprint(grid: &PathGrid, map: &Map, c: Cell, fp: &Footprint) -> bool {
    fp.distance(c) <= 1 && crate::roof::touch_allowed(grid, map, c, fp.nearest_cell(c))
}

/// The per-tick part of the rescue driver: walking.
pub(super) fn tick_rescue(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::Rescue { carrying, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    tick_movement(pawn, grid, map, t);
    Some(if pawn.is_moving() {
        JobEvent::None
    } else if carrying {
        JobEvent::ArrivedAtBedWithPatient
    } else {
        JobEvent::ArrivedAtPatient
    })
}
