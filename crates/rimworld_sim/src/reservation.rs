//! Reservations, as the game keeps them (docs/research.md §18):
//!
//! - [`ReservationManager`]: ordinary reservations of things and cells,
//!   an ordered list of rows (claimant, job, target, max pawns, quantity),
//!   with stack sharing by quantity.
//! - [`DestinationManager`]: where pawns intend to stand, per faction;
//!   rows survive their job (job cleared) until the pawn reserves a new
//!   destination.
//!
//! Behaviour follows `Verse.AI.ReservationManager` and
//! `Verse.PawnDestinationReservationManager` (checked against our own
//! managed-code inspection). Not modelled yet: reservation layers,
//! physical-interaction claims, building interaction cells, hostility and
//! host factions (every pawn with a faction is in the player faction), and
//! player-forced reservation pre-emption.

use crate::grid::Cell;
use crate::map::ItemId;
use crate::pawn::PawnId;

/// Identity of a started job (each start gets a new one).
pub type JobId = u64;

/// `StackCount_All`: the whole stack, whatever its size at query time.
pub const STACK_ALL: i32 = -1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    Item(ItemId),
    Cell(Cell),
    /// A cell's ceiling (`ReservationLayerDefOf.Ceiling`): roof work,
    /// separate from standing on the cell.
    Ceiling(Cell),
    /// A cell's floor (`ReservationLayerDefOf.Floor`): floor work.
    Floor(Cell),
    /// A pawn (to be rescued).
    Pawn(crate::pawn::PawnId),
}

/// A claimant as the managers see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Claimant {
    pub pawn: PawnId,
    /// Has a faction (all such pawns are the player's for now).
    pub has_faction: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reservation {
    pub claimant: Claimant,
    pub job: JobId,
    pub target: Target,
    pub max_pawns: i32,
    pub stack_count: i32,
}

/// `RespectsReservationsOf`: a pawn respects its own rows, and rows of
/// pawns in a faction when it has one too (no hostility modelled).
// COMPATIBILITY TODO: currently approximate — hostile and host-faction
// cases are not modelled.
fn respects(new: Claimant, old: Claimant) -> bool {
    new.pawn == old.pawn || (new.has_faction && old.has_faction)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ReservationManager {
    rows: Vec<Reservation>,
}

impl ReservationManager {
    pub fn rows(&self) -> &[Reservation] {
        &self.rows
    }

    /// Whether `claimant` could reserve `stack_count` of `target` (whose
    /// stack size is `target_stack`; 1 for cells). Rows of one other pawn
    /// count once; its first row's quantity is used.
    pub fn can_reserve(
        &self,
        claimant: Claimant,
        target: Target,
        target_stack: i32,
        max_pawns: i32,
        stack_count: i32,
    ) -> bool {
        let wanted = if stack_count == STACK_ALL {
            target_stack
        } else {
            stack_count
        };
        if wanted > target_stack {
            return false;
        }
        if self.is_already_reserved(claimant, target, wanted) {
            return true;
        }
        let mut seen: Vec<PawnId> = Vec::new();
        let (mut pawns, mut held) = (0, 0);
        for r in &self.rows {
            if r.target != target
                || r.claimant.pawn == claimant.pawn
                || seen.contains(&r.claimant.pawn)
                || !respects(claimant, r.claimant)
            {
                continue;
            }
            if r.max_pawns != max_pawns {
                return false;
            }
            seen.push(r.claimant.pawn);
            pawns += 1;
            held += if r.stack_count == STACK_ALL {
                target_stack
            } else {
                r.stack_count
            };
            if pawns >= max_pawns || wanted + held > target_stack {
                return false;
            }
        }
        true
    }

    fn is_already_reserved(&self, claimant: Claimant, target: Target, wanted: i32) -> bool {
        self.rows.iter().any(|r| {
            r.target == target
                && r.claimant.pawn == claimant.pawn
                && (r.stack_count == STACK_ALL || r.stack_count >= wanted)
        })
    }

    /// How much of `target` is still available to `claimant`
    /// (`CanReserveStack`): every other row counts, even several of one
    /// pawn.
    pub fn can_reserve_stack(
        &self,
        claimant: Claimant,
        target: Target,
        target_stack: i32,
        max_pawns: i32,
    ) -> i32 {
        let (mut rows, mut held) = (0, 0);
        for r in &self.rows {
            if r.target != target
                || r.claimant.pawn == claimant.pawn
                || !respects(claimant, r.claimant)
            {
                continue;
            }
            if r.max_pawns != max_pawns {
                return 0;
            }
            rows += 1;
            held += if r.stack_count == STACK_ALL {
                target_stack
            } else {
                r.stack_count
            };
            if rows >= max_pawns || held >= target_stack {
                return 0;
            }
        }
        (target_stack - held).max(0)
    }

    /// Reserves, appending a row (a same-job row covering the quantity
    /// already counts as done). Returns `false` when not available.
    // COMPATIBILITY TODO: currently approximate — player-forced jobs do not
    // pre-empt other reservers.
    pub fn reserve(
        &mut self,
        claimant: Claimant,
        job: JobId,
        target: Target,
        target_stack: i32,
        max_pawns: i32,
        stack_count: i32,
    ) -> bool {
        let wanted = if stack_count == STACK_ALL {
            target_stack
        } else {
            stack_count
        };
        if self.rows.iter().any(|r| {
            r.target == target
                && r.claimant.pawn == claimant.pawn
                && r.job == job
                && (r.stack_count == STACK_ALL || r.stack_count >= wanted)
        }) {
            return true;
        }
        if !self.can_reserve(claimant, target, target_stack, max_pawns, stack_count) {
            return false;
        }
        self.rows.push(Reservation {
            claimant,
            job,
            target,
            max_pawns,
            stack_count,
        });
        true
    }

    /// Removes the first row of this pawn and job on `target`.
    pub fn release(&mut self, target: Target, pawn: PawnId, job: JobId) {
        if let Some(i) = self
            .rows
            .iter()
            .position(|r| r.target == target && r.claimant.pawn == pawn && r.job == job)
        {
            self.rows.remove(i);
        }
    }

    /// Removes every row of this pawn's job (job cleanup).
    pub fn release_claimed_by(&mut self, pawn: PawnId, job: JobId) {
        self.rows
            .retain(|r| !(r.claimant.pawn == pawn && r.job == job));
    }

    pub fn release_all_claimed_by(&mut self, pawn: PawnId) {
        self.rows.retain(|r| r.claimant.pawn != pawn);
    }

    /// Removes every row on a target (it was destroyed).
    pub fn release_all_for_target(&mut self, target: Target) {
        self.rows.retain(|r| r.target != target);
    }

    /// Removes other pawns' rows on a target (what a forced query may
    /// ignore).
    pub fn release_others_on(&mut self, target: Target, pawn: PawnId) {
        self.rows
            .retain(|r| r.target != target || r.claimant.pawn == pawn);
    }

    /// The pawns other than `pawn` holding a row on `target`, first
    /// reserver first.
    pub fn reservers_of(&self, target: Target, pawn: PawnId) -> Vec<PawnId> {
        let mut out = Vec::new();
        for r in &self.rows {
            if r.target == target && r.claimant.pawn != pawn && !out.contains(&r.claimant.pawn) {
                out.push(r.claimant.pawn);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Destination {
    pub cell: Cell,
    pub pawn: PawnId,
    /// `None` once the job that reserved it ended.
    pub job: Option<JobId>,
    /// Replaced by a newer destination, but kept until its job ends.
    pub obsolete: bool,
}

/// Destinations of the player faction (pawns without a faction neither
/// reserve nor are blocked).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DestinationManager {
    rows: Vec<Destination>,
}

impl DestinationManager {
    pub fn rows(&self) -> &[Destination] {
        &self.rows
    }

    /// Whether `searcher` may head for `cell`: no other pawn's row (current
    /// or obsolete) is on it. `drafted_only` limits this to drafted
    /// claimants, which do not exist yet.
    pub fn can_reserve(&self, cell: Cell, searcher: Claimant, drafted_only: bool) -> bool {
        if !searcher.has_faction || drafted_only {
            return true;
        }
        !self
            .rows
            .iter()
            .any(|d| d.cell == cell && d.pawn != searcher.pawn)
    }

    /// Records `cell` as the pawn's destination. It does not check
    /// availability; the pawn's earlier rows become obsolete (jobless ones
    /// are dropped).
    pub fn reserve(&mut self, claimant: Claimant, job: JobId, cell: Cell) {
        if !claimant.has_faction {
            return;
        }
        self.obsolete_all_claimed_by(claimant.pawn);
        self.rows.push(Destination {
            cell,
            pawn: claimant.pawn,
            job: Some(job),
            obsolete: false,
        });
    }

    /// Job cleanup: the job's rows lose their job; obsolete ones go.
    pub fn release_claimed_by(&mut self, pawn: PawnId, job: JobId) {
        let mut i = 0;
        while i < self.rows.len() {
            let d = &mut self.rows[i];
            if d.pawn == pawn && d.job == Some(job) {
                d.job = None;
                if d.obsolete {
                    self.rows.swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    pub fn obsolete_all_claimed_by(&mut self, pawn: PawnId) {
        let mut i = 0;
        while i < self.rows.len() {
            let d = &mut self.rows[i];
            if d.pawn == pawn {
                d.obsolete = true;
                if d.job.is_none() {
                    self.rows.swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    pub fn release_all_claimed_by(&mut self, pawn: PawnId) {
        let mut i = 0;
        while i < self.rows.len() {
            if self.rows[i].pawn == pawn {
                self.rows.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// The pawn's newest row.
    pub fn most_recent_for(&self, pawn: PawnId) -> Option<&Destination> {
        self.rows.iter().rev().find(|d| d.pawn == pawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u32) -> Claimant {
        Claimant {
            pawn: PawnId(n),
            has_faction: true,
        }
    }
    const MEAL: Target = Target::Item(ItemId(1));

    #[test]
    fn stack_sharing_by_quantity() {
        let mut m = ReservationManager::default();
        // Case A: one meal, one taker.
        assert!(m.reserve(p(1), 10, MEAL, 1, 10, 1));
        assert!(!m.can_reserve(p(2), MEAL, 1, 10, 1));
        // A zero-quantity row is allowed and takes a pawn slot only.
        assert!(m.reserve(p(2), 11, MEAL, 1, 10, 0));
        assert_eq!(m.rows().len(), 2);
        m.release_claimed_by(PawnId(2), 11);
        // Case B: five meals, two takers of one.
        let mut m = ReservationManager::default();
        assert!(m.reserve(p(1), 1, MEAL, 5, 10, 1));
        assert!(m.reserve(p(2), 2, MEAL, 5, 10, 1));
        assert_eq!(m.can_reserve_stack(p(3), MEAL, 5, 10), 3);
    }

    #[test]
    fn dedup_in_can_reserve_but_not_in_can_reserve_stack() {
        // Case Q of the research report.
        let mut m = ReservationManager::default();
        assert!(m.reserve(p(1), 17, MEAL, 5, 10, 1));
        assert!(
            m.reserve(p(1), 17, MEAL, 5, 10, 1),
            "same job, same quantity"
        );
        assert_eq!(m.rows().len(), 1);
        assert!(
            m.reserve(p(1), 17, MEAL, 5, 10, 2),
            "larger request appends"
        );
        assert_eq!(m.rows().len(), 2);
        assert_eq!(m.can_reserve_stack(p(2), MEAL, 5, 10), 2);
        assert!(
            m.can_reserve(p(2), MEAL, 5, 10, 4),
            "first row of pawn 1 only"
        );
        assert!(m.reserve(p(2), 18, MEAL, 5, 10, 4));
        // Release removes only the first matching row.
        m.release(MEAL, PawnId(1), 17);
        assert_eq!(m.rows().len(), 2);
        assert!(!m.can_reserve(p(2), MEAL, 5, 10, 6));
    }

    #[test]
    fn max_pawns_must_agree_and_cells_are_exclusive() {
        let mut m = ReservationManager::default();
        let cell = Target::Cell(Cell::new(3, 3));
        assert!(m.reserve(p(1), 1, cell, 1, 1, STACK_ALL));
        assert!(!m.can_reserve(p(2), cell, 1, 1, STACK_ALL));
        assert!(m.can_reserve(p(1), cell, 1, 1, STACK_ALL), "own row");
        assert!(m.reserve(p(1), 1, MEAL, 5, 10, 1));
        assert!(!m.can_reserve(p(2), MEAL, 5, 1, 1), "max pawns mismatch");
        // Pawns without a faction ignore others' rows.
        let wild = Claimant {
            pawn: PawnId(9),
            has_faction: false,
        };
        assert!(m.can_reserve(wild, cell, 1, 1, STACK_ALL));
    }

    #[test]
    fn destination_states() {
        // Case E of the research report.
        let mut d = DestinationManager::default();
        let c = Cell::new(110, 113);
        assert!(d.can_reserve(c, p(1), false));
        d.reserve(p(1), 14, c);
        assert!(!d.can_reserve(c, p(2), false));
        d.reserve(p(2), 15, c); // not checked: both recorded
        assert_eq!(d.rows().len(), 2);
        d.release_claimed_by(PawnId(1), 14);
        assert_eq!(d.rows().len(), 2, "jobless row stays");
        assert_eq!(d.rows()[0].job, None);
        d.obsolete_all_claimed_by(PawnId(1));
        assert_eq!(d.rows().len(), 1, "jobless row dropped when obsoleted");
        d.reserve(p(2), 15, Cell::new(111, 113));
        assert_eq!(d.rows().len(), 2, "old row obsolete, still there");
        assert!(!d.can_reserve(c, p(3), false), "obsolete rows still block");
        d.release_claimed_by(PawnId(2), 15);
        assert_eq!(d.rows().len(), 1, "obsolete row removed, new one jobless");
        assert_eq!(
            d.most_recent_for(PawnId(2)).unwrap().cell,
            Cell::new(111, 113)
        );
    }
}
