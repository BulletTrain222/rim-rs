//! The tend driver (`JobDriver_TendPatient` without medicine,
//! docs/research.md §47): walk to the patient's interaction cell, wait
//! 600 / MedicalTendSpeed ticks, tend the most urgent injury, and repeat
//! while the patient still needs tending.

use super::{JobEvent, Sim, tick_movement};
use crate::grid::Cell;
use crate::job::{Job, JobKind, TendStage};
use crate::map::{ItemId, Map};
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::tend::{PatientFacts, TendCandidate};

/// `TendUtility.NoMedicinePotency`.
const NO_MEDICINE_POTENCY: f32 = 0.3;
/// `TendUtility.NoMedicineQualityMax`.
const NO_MEDICINE_QUALITY_MAX: f32 = 0.7;
/// `HealthAIUtility.ShouldBeTendedNowByPlayerUrgent`: bleeding out sooner.
const URGENT_BLEED_OUT_TICKS: i32 = 45_000;

impl Sim {
    /// The bed pawn `i` lies in now (`CurrentBed`).
    pub(super) fn current_bed(&self, i: usize) -> Option<ItemId> {
        let p = &self.pawns[i];
        match p.job.as_ref().map(|j| j.kind) {
            Some(JobKind::LayDown { bed: Some(b), spot }) if p.position == spot => Some(b),
            _ => None,
        }
    }

    /// Pawn `k`'s `InteractionCell`: beside their bed when lying in one
    /// (`FindPreferredInteractionCell`), else their own cell.
    pub(super) fn patient_interaction_cell(&self, k: usize) -> Cell {
        let at = self.pawns[k].position;
        let Some(fp) = self
            .current_bed(k)
            .and_then(|b| self.map.structure(b))
            .map(|s| s.footprint)
        else {
            return at;
        };
        let (grid, map, defs) = (&self.path_grid, &self.map, &*self.defs);
        crate::tend::bed_interaction_cell(
            &fp,
            at,
            |c| {
                map.size().contains(c)
                    && grid.walkable(c)
                    && map.standable_things(defs, c)
                    && fp.distance(c) <= 1
                    && crate::roof::touch_allowed(grid, map, c, fp.nearest_cell(c))
            },
            |c| map.buildings[c].is_some_and(|b| defs.things[b].is_bed()),
            |c| map.door_at(c).is_some(),
        )
        .unwrap_or(at)
    }

    /// Whether colonist `k` should be tended now (`ShouldBeTendedNowByPlayer`)
    /// and, if so, whether urgently.
    fn tend_need(&self, k: usize) -> Option<bool> {
        let p = &self.pawns[k];
        if !p.is_colonist || p.health.dead {
            return None;
        }
        let view = self.health_view(p.id)?;
        view.needs_tending()
            .then(|| view.ticks_until_death_by_blood_loss() < URGENT_BLEED_OUT_TICKS)
    }

    /// Colonists lying in bed who should be tended now, as seen by doctor
    /// `doctor` (`WorkGiver_Tend.HasJobOnThing` apart from reservations).
    // COMPATIBILITY TODO: currently approximate — only humanlike colonists
    // are tended (no animals, guests or prisoners).
    pub(super) fn tend_candidates(&self, doctor: usize) -> Vec<TendCandidate> {
        let mut out = Vec::new();
        for k in 0..self.pawns.len() {
            if k == doctor || !self.in_bed(k) {
                continue;
            }
            if let Some(urgent) = self.tend_need(k) {
                out.push(TendCandidate {
                    patient: self.pawns[k].id,
                    cell: self.patient_interaction_cell(k),
                    urgent,
                });
            }
        }
        out
    }

    /// `ShouldSeekMedicalRest`: downed, needing tending, or with a tended
    /// injury still healing.
    // COMPATIBILITY TODO: currently approximate — diseases (immunity),
    // labor and surgery are not modelled.
    pub(super) fn should_seek_medical_rest(&self, i: usize) -> bool {
        let p = &self.pawns[i];
        p.health.downed
            || self
                .health_view(p.id)
                .is_some_and(|v| v.needs_tending() || v.has_tended_and_healing_injury())
    }

    /// What the patient work givers need to know about pawn `i`.
    pub(super) fn patient_facts(&self, i: usize) -> PatientFacts {
        let p = &self.pawns[i];
        let Some(view) = self.health_view(p.id) else {
            return PatientFacts::default();
        };
        let needs_tend = view.needs_tending();
        let seek_urgent = p.health.downed || needs_tend;
        let seek = seek_urgent || view.has_tended_and_healing_injury();
        if !seek {
            return PatientFacts::default();
        }
        let urgent_tend =
            needs_tend && view.ticks_until_death_by_blood_loss() < URGENT_BLEED_OUT_TICKS;
        // `AnyAvailableDoctorFor`: another colonist up and about with
        // doctoring enabled.
        let doctor = self.defs.work_types.id("Doctor");
        let doctor_available = self.pawns.iter().enumerate().any(|(k, d)| {
            k != i
                && d.is_colonist
                && !d.health.downed
                && !d.health.dead
                && !d.asleep
                && !self.in_bed(k)
                && d.work.as_ref().is_some_and(|w| {
                    doctor.is_some_and(|t| w.effective(t, self.use_work_priorities, true) > 0)
                })
                && self.regions.connected(d.position, p.position)
        });
        let occupancy = self.bed_occupancy();
        let regions = &self.regions;
        let at = p.position;
        let bed = crate::rest::find_bed_for(
            &self.defs,
            &self.map,
            &|c| regions.connected(at, c),
            &self.reservations,
            self.claimant(i),
            at,
            p.owned_bed,
            &occupancy,
        )
        .and_then(|b| {
            self.map
                .structure(b)
                .map(|s| (b, s.footprint.sleeping_slot(0)))
        });
        PatientFacts {
            urgent_tend,
            seek_urgent,
            seek,
            doctor_available,
            bed,
        }
    }

    /// The tend job starts (and restarts after each tend): walk to the
    /// patient's interaction cell (`GotoThing` InteractionCell).
    pub(super) fn begin_tend(&mut self, i: usize) -> bool {
        let Some(JobKind::TendPatient { patient, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(k) = self.index_of(patient) else {
            return false;
        };
        let cell = self.patient_interaction_cell(k);
        self.walk_to(i, cell, true)
    }

    /// `600 / MedicalTendSpeed` ticks, truncated.
    fn tend_ticks(&self, i: usize) -> i32 {
        (1.0 / self.pawn_stat_of(i, "MedicalTendSpeed") * 600.0) as i32
    }

    /// At the patient: the wait starts, paying its first tick at once.
    pub(super) fn start_tend_wait(&mut self, i: usize) {
        let ticks = self.tend_ticks(i);
        set_tend_stage(
            &mut self.pawns[i],
            TendStage::Wait {
                ticks_left: ticks - 1,
            },
        );
        if ticks - 1 <= 0 {
            self.finish_tend(i);
        }
    }

    /// `FinalizeTend`: Medicine experience, `DoTend`, then the end
    /// condition — loop back to the goto while the patient still needs
    /// tending, else the job succeeds.
    // COMPATIBILITY TODO: currently approximate — the end condition and the
    // wait's "still next to the patient" check run only here, not every
    // tick.
    pub(super) fn finish_tend(&mut self, i: usize) {
        let Some(JobKind::TendPatient { patient, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(k) = self.index_of(patient) else {
            self.end_job(i, false);
            return;
        };
        if self.pawns[k].health.dead || self.pawns[i].position != self.patient_interaction_cell(k) {
            self.end_job(i, false);
            return;
        }
        // 500 xp for a humanlike patient, ×0.5 without medicine.
        self.learn(i, "Medicine", 500.0 * 0.5);
        // `CalculateBaseTendQuality`: MedicalTendQuality × potency plus the
        // bed's offset, capped by the medicine's maximum.
        let bed_offset = self
            .current_bed(k)
            .and_then(|b| self.map.structure(b))
            .map_or(0.0, |s| {
                crate::stats::def_stat(
                    &self.defs,
                    &self.defs.things[s.def],
                    s.stuff.map(|st| &self.defs.things[st]),
                    "MedicalTendQualityOffset",
                )
            });
        let quality = (self.pawn_stat_of(i, "MedicalTendQuality") * NO_MEDICINE_POTENCY
            + bed_offset)
            .clamp(0.0, NO_MEDICINE_QUALITY_MAX);
        let defs = self.defs.clone();
        if let Some(race) = defs.things[self.pawns[k].race].race.as_ref()
            && let Some(body) = race.body.as_deref().and_then(|b| defs.bodies.get(b))
        {
            crate::health::tend(
                &mut self.pawns[k].health,
                &defs,
                body,
                race.base_health_scale,
                race.bleed_rate_factor,
                quality,
                NO_MEDICINE_QUALITY_MAX,
                &mut self.rng,
            );
            self.check_health_state(k);
        }
        if self.tend_need(k).is_none() {
            self.end_job(i, true);
            return;
        }
        // `Jump(gotoToil)`: standing at the cell the goto ends at once and
        // the next wait starts without paying a tick.
        let cell = self.patient_interaction_cell(k);
        if self.pawns[i].position == cell {
            let ticks = self.tend_ticks(i);
            set_tend_stage(&mut self.pawns[i], TendStage::Wait { ticks_left: ticks });
        } else {
            set_tend_stage(&mut self.pawns[i], TendStage::Goto);
            if !self.walk_to(i, cell, true) {
                self.end_job(i, false);
            }
        }
    }
}

fn set_tend_stage(pawn: &mut Pawn, new: TendStage) {
    if let Some(Job {
        kind: JobKind::TendPatient { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new;
    }
}

/// The per-tick part of tending: walking, then the wait.
pub(super) fn tick_tend(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::TendPatient { stage, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    Some(match stage {
        TendStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToTend
            }
        }
        TendStage::Wait { ticks_left } => {
            let left = ticks_left - 1;
            set_tend_stage(pawn, TendStage::Wait { ticks_left: left });
            if left <= 0 {
                JobEvent::TendDone
            } else {
                JobEvent::None
            }
        }
    })
}
