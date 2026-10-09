//! Tending (docs/research.md §47): `WorkGiver_TendOther` sends a doctor to
//! a colonist lying in bed with injuries that need tending, and the
//! patient work givers (`WorkGiver_PatientGoToBed*`) send injured
//! colonists to bed.

use rimworld_defs::{DefId, JobDef};

use crate::geom::{Footprint, rot_index};
use crate::grid::Cell;
use crate::job::{Job, JobKind, TendStage};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::pawn::PawnId;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// A colonist in bed who should be tended now
/// (`ShouldBeTendedNowByPlayer` with `GoodLayingStatusForTend`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TendCandidate {
    pub patient: PawnId,
    /// Where the doctor stands (the patient's `InteractionCell`).
    pub cell: Cell,
    /// `ShouldBeTendedNowByPlayerUrgent`: bleeding out within 45000 ticks.
    pub urgent: bool,
}

/// `WorkGiver_TendOther_Humanlike`, or `WorkGiver_TendOtherUrgent` when
/// `urgent_only`.
pub struct TendGiver<'a> {
    pub candidates: &'a [TendCandidate],
    pub regions: &'a crate::region::Regions,
    pub urgent_only: bool,
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for TendGiver<'_> {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        !self
            .candidates
            .iter()
            .any(|c| c.urgent || !self.urgent_only)
    }

    /// The nearest patient (straight-line order, as `ClosestThingReachable`
    /// over the global list) that can be reserved and reached.
    // COMPATIBILITY TODO: currently approximate — the global search is by
    // straight-line distance with region connectivity; medicine and
    // self-tending (off by default for colonists) are not modelled.
    fn non_scan_job(&self, ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        let mut order: Vec<&TendCandidate> = self
            .candidates
            .iter()
            .filter(|c| c.urgent || !self.urgent_only)
            .collect();
        order.sort_by_key(|c| {
            (c.cell.x - ctx.position.x).pow(2) + (c.cell.z - ctx.position.z).pow(2)
        });
        let c = order.into_iter().find(|c| {
            c.patient != ctx.claimant.pawn
                && ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Pawn(c.patient),
                    1,
                    1,
                    STACK_ALL,
                )
                && self.regions.connected(ctx.position, c.cell)
        })?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::TendPatient {
                    patient: c.patient,
                    stage: TendStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// What the patient work givers know about the thinking pawn.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PatientFacts {
    /// `ShouldBeTendedNowByPlayerUrgent`.
    pub urgent_tend: bool,
    /// `ShouldSeekMedicalRestUrgent`: needs tending.
    pub seek_urgent: bool,
    /// `ShouldSeekMedicalRest`: also a tended injury still healing.
    pub seek: bool,
    /// `AnyAvailableDoctorFor`.
    pub doctor_available: bool,
    /// The bed found for the pawn (`FindBedFor`) and its sleeping spot.
    pub bed: Option<(ItemId, Cell)>,
}

/// Which patient giver: emergency treatment, treatment or recuperation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatientGiverKind {
    EmergencyTreatment,
    Treatment,
    Recuperate,
}

/// `WorkGiver_PatientGoToBed*`: lie down in a bed (`JobGiver_PatientGoToBed`
/// without the timetable).
pub struct PatientGiver {
    pub facts: PatientFacts,
    pub kind: PatientGiverKind,
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for PatientGiver {
    fn non_scan_job(&self, _ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        let f = self.facts;
        let wanted = match self.kind {
            PatientGiverKind::EmergencyTreatment => f.urgent_tend && f.seek,
            PatientGiverKind::Treatment => f.seek_urgent && f.doctor_available,
            PatientGiverKind::Recuperate => f.seek,
        };
        if !wanted {
            return None;
        }
        let (bed, spot) = f.bed?;
        Some((
            Job {
                def: self.job,
                kind: JobKind::LayDown {
                    spot,
                    bed: Some(bed),
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// `Building_Bed.FindPreferredInteractionCell`: where to stand to tend
/// someone lying at `occupant`. Candidate offsets (`BedInteractionCellSearch
/// Pattern`, for a bed facing south, turned to the bed's rotation) keep the
/// cells that are standable and touch the bed; then, unless that removes
/// them all, cells without a bed, then cells without a door.
pub fn bed_interaction_cell(
    bed: &Footprint,
    occupant: Cell,
    usable: impl Fn(Cell) -> bool,
    has_bed: impl Fn(Cell) -> bool,
    has_door: impl Fn(Cell) -> bool,
) -> Option<Cell> {
    if !bed.contains(occupant) {
        return None;
    }
    let r = bed.rect();
    let slot = match bed.rot {
        crate::job::Rot4::South => occupant.x - r.min_x,
        crate::job::Rot4::North => r.max_x - occupant.x,
        crate::job::Rot4::West => r.max_z - occupant.z,
        crate::job::Rot4::East => occupant.z - r.min_z,
    };
    const W: (i32, i32) = (-1, 0);
    const E: (i32, i32) = (1, 0);
    const S: (i32, i32) = (0, -1);
    const N: (i32, i32) = (0, 1);
    let add = |a: (i32, i32), b: (i32, i32)| (a.0 + b.0, a.1 + b.1);
    let mut offsets: Vec<(i32, i32)> = Vec::new();
    if bed.size == (1, 1) {
        offsets.extend([
            W,
            E,
            S,
            N,
            add(S, W),
            add(S, E),
            add(N, W),
            add(N, E),
            (0, 0),
        ]);
    } else if bed.size.1 == 2 {
        let right = slot == 0;
        let left = slot == bed.sleeping_slots() - 1;
        if right {
            offsets.push(W);
        }
        if left {
            offsets.push(E);
        }
        if right {
            offsets.push(add(W, S));
        }
        if left {
            offsets.push(add(E, S));
        }
        offsets.extend([
            N,
            add(N, W),
            add(N, E),
            (0, -2),
            (-1, -2),
            (1, -2),
            S,
            (0, 0),
        ]);
    } else {
        return None;
    }
    // `Rot4.GetRelativeRotation(South, rot)` then `RotatedBy`.
    let turn = (rot_index(bed.rot) - 2).rem_euclid(4);
    let mut cells: Vec<Cell> = offsets
        .into_iter()
        .map(|(x, z)| {
            let (x, z) = match turn {
                0 => (x, z),
                1 => (z, -x),
                2 => (-x, -z),
                _ => (-z, x),
            };
            Cell::new(occupant.x + x, occupant.z + z)
        })
        .filter(|&c| usable(c))
        .collect();
    let keep_if_any = |cells: &mut Vec<Cell>, keep: &dyn Fn(Cell) -> bool| -> bool {
        if cells.iter().any(|&c| keep(c)) {
            let before = cells.len();
            cells.retain(|&c| keep(c));
            cells.len() != before
        } else {
            false
        }
    };
    if keep_if_any(&mut cells, &|c| !has_bed(c)) {
        keep_if_any(&mut cells, &|c| !has_door(c));
    }
    cells.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Rot4;

    fn bed(center: Cell, rot: Rot4, size: (i32, i32)) -> Footprint {
        Footprint { center, rot, size }
    }

    #[test]
    fn single_bed_facing_north_prefers_the_east_side() {
        // The probe's bed: at (34, 21) facing north, the sleeper at its
        // head; the doctor stood at (35, 21).
        let fp = bed(Cell::new(34, 21), Rot4::North, (1, 2));
        let cell = bed_interaction_cell(
            &fp,
            Cell::new(34, 21),
            |_| true,
            |c| fp.contains(c),
            |_| false,
        );
        assert_eq!(cell, Some(Cell::new(35, 21)));
    }

    #[test]
    fn falls_back_past_blocked_cells() {
        let fp = bed(Cell::new(10, 10), Rot4::South, (1, 2));
        let occupant = fp.sleeping_slot(0);
        // Facing south: the south-frame offsets apply as they are, so west
        // comes first, then east.
        let blocked = Cell::new(occupant.x - 1, occupant.z);
        let cell = bed_interaction_cell(
            &fp,
            occupant,
            |c| c != blocked,
            |c| fp.contains(c),
            |_| false,
        );
        assert_eq!(cell, Some(Cell::new(occupant.x + 1, occupant.z)));
    }
}
