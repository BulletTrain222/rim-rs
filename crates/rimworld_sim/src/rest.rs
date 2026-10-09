//! Resting: the timetable, `JobGiver_GetRest` and the lay-down rules
//! (docs/research.md §12).

use rimworld_defs::{DefId, GameDefs, ThingDef};

use crate::cell_finder::{MapView, try_random_closewalk_cell_near};
use crate::grid::Cell;
use crate::map::{ItemId, Map, Structure};
use crate::needs::{FALL_ASLEEP_MAX_LEVEL, WAKE_THRESHOLD};
use crate::pawn::PawnId;
use crate::rand::Rand;
use crate::reservation::{Claimant, ReservationManager, Target};
use crate::stats::def_stat;

/// Priority `JobGiver_GetRest` reports to a `ThinkNode_PrioritySorter`.
pub const GET_REST_PRIORITY: f32 = 8.0;
/// In "Anything" hours, a pawn wants rest below this level.
const ANYTHING_REST_THRESHOLD: f32 = 0.3;
/// Below this rest level a pawn assigned to meditate goes to sleep.
const MEDITATE_REST_THRESHOLD: f32 = 0.16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TimeAssignment {
    Anything,
    Work,
    Joy,
    Meditate,
    Sleep,
}

impl TimeAssignment {
    /// The `TimeAssignmentDef` it stands for.
    pub fn def_name(self) -> &'static str {
        match self {
            TimeAssignment::Anything => "Anything",
            TimeAssignment::Work => "Work",
            TimeAssignment::Joy => "Joy",
            TimeAssignment::Meditate => "Meditate",
            TimeAssignment::Sleep => "Sleep",
        }
    }
}

/// `Pawn_TimetableTracker`'s initial schedule: hours 6–21 Anything, the
/// rest Sleep.
pub fn default_timetable() -> Vec<TimeAssignment> {
    (0..24).map(|h| time_assignment(h, true)).collect()
}

/// `Pawn_TimetableTracker.CurrentAssignment` (and, without a timetable,
/// the animal hours `JobGiver_GetRest` uses): colonists follow their
/// timetable, other humanlikes are always Anything.
pub fn assignment_for(pawn: &crate::pawn::Pawn, hour: u32, humanlike: bool) -> TimeAssignment {
    if !humanlike {
        return time_assignment(hour, false);
    }
    if !pawn.is_colonist {
        return TimeAssignment::Anything;
    }
    pawn.timetable
        .as_ref()
        .and_then(|t| t.get(hour as usize).copied())
        .unwrap_or_else(|| time_assignment(hour, true))
}

/// The default colonist timetable: 06–21 Anything, 22–05 Sleep. Animals
/// (non-humanlike) treat 07–21 as Anything.
pub fn time_assignment(hour: u32, humanlike: bool) -> TimeAssignment {
    let anything = if humanlike {
        hour > 5 && hour <= 21
    } else {
        (7..=21).contains(&hour)
    };
    if anything {
        TimeAssignment::Anything
    } else {
        TimeAssignment::Sleep
    }
}

/// Whether a lying pawn may fall asleep now.
// COMPATIBILITY TODO: currently approximate — disturbance (400-tick
// cooldown), sleep-blocking hediffs and life-stage overrides are not modelled.
pub fn can_fall_asleep(rest_level: f32, starving: bool) -> bool {
    !starving && rest_level < FALL_ASLEEP_MAX_LEVEL.min(WAKE_THRESHOLD - 0.01)
}

pub fn should_wake_up(rest_level: f32) -> bool {
    rest_level >= WAKE_THRESHOLD
}

/// `JobGiver_GetRest` priority for a pawn with the given rest level and
/// current assignment.
pub fn get_rest_priority(
    rest_level: Option<f32>,
    starving: bool,
    assignment: TimeAssignment,
) -> f32 {
    let Some(rest) = rest_level else { return 0.0 };
    // maxLevelPercentage (default 1.0) and minCategory (default Rested) never
    // exclude a pawn with default parameters.
    if !can_fall_asleep(rest, starving) {
        return 0.0;
    }
    match assignment {
        TimeAssignment::Anything if rest < ANYTHING_REST_THRESHOLD => GET_REST_PRIORITY,
        TimeAssignment::Anything => 0.0,
        TimeAssignment::Work => 0.0,
        TimeAssignment::Meditate if rest < MEDITATE_REST_THRESHOLD => GET_REST_PRIORITY,
        TimeAssignment::Meditate => 0.0,
        TimeAssignment::Joy if rest < ANYTHING_REST_THRESHOLD => GET_REST_PRIORITY,
        TimeAssignment::Joy => 0.0,
        TimeAssignment::Sleep => GET_REST_PRIORITY,
    }
}

/// A ground cell to sleep on (`JobGiver_GetRest.TryFindGroundSleepSpotFor`):
/// the pawn's own cell if valid, else a random valid cell within 4, then
/// 12 cells (`CellFinder.TryRandomClosewalkCellNear`). Valid: not avoided
/// by wanderers and reservable.
pub fn find_ground_sleep_spot(
    view: &MapView<'_>,
    at: Cell,
    can_reserve: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    let valid = |c: Cell| !view.defs.terrain[view.map.terrain[c]].avoid_wander && can_reserve(c);
    if valid(at) {
        return Some(at);
    }
    [4, 12]
        .into_iter()
        .find_map(|radius| try_random_closewalk_cell_near(view, at, radius, valid, rng))
}

/// `RestUtility.bedDefsBestToWorst_RestEffectiveness`: beds by
/// `bed_maxBodySize`, then by BedRestEffectiveness (best first); a stable
/// sort keeps database order for ties (`OrderBy` is stable).
pub fn beds_best_to_worst(defs: &GameDefs) -> Vec<DefId<ThingDef>> {
    let mut beds: Vec<DefId<ThingDef>> = defs
        .things
        .iter()
        .filter(|(_, d)| d.is_bed())
        .map(|(id, _)| id)
        .collect();
    let key = |id: DefId<ThingDef>| {
        let d = &defs.things[id];
        (
            d.building.as_ref().map_or(9999.0, |b| b.bed_max_body_size),
            def_stat(defs, d, None, "BedRestEffectiveness"),
        )
    };
    beds.sort_by(|&a, &b| {
        let (sa, ea) = key(a);
        let (sb, eb) = key(b);
        sa.total_cmp(&sb).then(eb.total_cmp(&ea))
    });
    beds
}

/// Who is lying in which bed now (pawn, bed, cell).
pub type Occupancy<'a> = &'a [(PawnId, ItemId, Cell)];

/// `RestUtility.CanUseBedNow` for a colonist and a non-medical bed: a
/// free sleeping slot (unless it owns the bed or lies in it), and the bed
/// unowned or owned by the sleeper.
// COMPATIBILITY TODO: currently approximate — lovers sharing beds,
// prisoner/slave/medical beds, social properness, ideology and burning are
// not modelled.
pub fn can_use_bed_now(bed: &Structure, sleeper: PawnId, occupancy: Occupancy<'_>) -> bool {
    let owner = bed.owners.contains(&sleeper);
    let lying_here = occupancy
        .iter()
        .any(|&(p, b, _)| p == sleeper && b == bed.id);
    let occupied = occupancy
        .iter()
        .filter(|&&(p, b, _)| b == bed.id && p != sleeper)
        .count() as i32;
    if occupied >= bed.footprint.sleeping_slots() && !owner && !lying_here {
        return false;
    }
    owner || bed.owners.is_empty()
}

/// `RestUtility.FindBedFor` for a colonist without medical needs: its own
/// bed if usable, else the nearest usable reachable bed of the best bed
/// def that has one.
// COMPATIBILITY TODO: currently approximate — nearest by straight-line
// distance with connectivity instead of the region search; danger passes
// and corpse checks are not modelled (neither exists yet).
#[allow(clippy::too_many_arguments)]
pub fn find_bed_for(
    defs: &GameDefs,
    map: &Map,
    reachable: &dyn Fn(Cell) -> bool,
    reservations: &ReservationManager,
    claimant: Claimant,
    at: Cell,
    owned: Option<ItemId>,
    occupancy: Occupancy<'_>,
) -> Option<ItemId> {
    let valid = |bed: &Structure| {
        can_use_bed_now(bed, claimant.pawn, occupancy)
            && reachable(bed.footprint.sleeping_slot(0))
            && reservations.can_reserve(
                claimant,
                Target::Item(bed.id),
                1,
                bed.footprint.sleeping_slots(),
                0,
            )
    };
    if let Some(b) = owned.and_then(|b| map.structure(b))
        && valid(b)
    {
        return Some(b.id);
    }
    for def in beds_best_to_worst(defs) {
        // `CanUseBedEver` for a human-sized humanlike.
        // COMPATIBILITY TODO: currently approximate — the sleeper's body
        // size and race are not checked (colonists are assumed).
        let props = defs.things[def].building.as_ref();
        if !props.is_none_or(|b| b.bed_humanlike && b.bed_max_body_size >= 1.0) {
            continue;
        }
        let found = map
            .structures()
            .iter()
            .filter(|s| s.def == def && valid(s))
            .min_by_key(|s| {
                let c = s.footprint.center;
                (c.x - at.x).pow(2) + (c.z - at.z).pow(2)
            });
        if let Some(s) = found {
            return Some(s.id);
        }
    }
    None
}

/// BedRestEffectiveness of the bed lain in, else the stat's
/// `valueIfMissing` (ground).
// COMPATIBILITY TODO: currently approximate — quality is not modelled
// (normal quality assumed).
pub fn rest_effectiveness(defs: &GameDefs, map: &Map, bed: Option<ItemId>, ground: f32) -> f32 {
    match bed.and_then(|b| map.structure(b)) {
        Some(s) if defs.things[s.def].stat("BedRestEffectiveness").is_some() => def_stat(
            defs,
            &defs.things[s.def],
            s.stuff.map(|x| &defs.things[x]),
            "BedRestEffectiveness",
        ),
        _ => ground,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timetable() {
        assert_eq!(time_assignment(5, true), TimeAssignment::Sleep);
        assert_eq!(time_assignment(6, true), TimeAssignment::Anything);
        assert_eq!(time_assignment(21, true), TimeAssignment::Anything);
        assert_eq!(time_assignment(22, true), TimeAssignment::Sleep);
        assert_eq!(time_assignment(6, false), TimeAssignment::Sleep);
        assert_eq!(time_assignment(7, false), TimeAssignment::Anything);
    }

    #[test]
    fn rest_priority_rules() {
        // Daytime: only when below 0.3.
        assert_eq!(
            get_rest_priority(Some(0.29), false, time_assignment(12, true)),
            8.0
        );
        assert_eq!(
            get_rest_priority(Some(0.31), false, time_assignment(12, true)),
            0.0
        );
        // Night: whenever the pawn could fall asleep (< 0.75).
        assert_eq!(
            get_rest_priority(Some(0.7), false, time_assignment(23, true)),
            8.0
        );
        assert_eq!(
            get_rest_priority(Some(0.8), false, time_assignment(23, true)),
            0.0
        );
        // Starving pawns cannot fall asleep.
        assert_eq!(
            get_rest_priority(Some(0.1), true, time_assignment(23, true)),
            0.0
        );
        assert_eq!(
            get_rest_priority(None, false, time_assignment(23, true)),
            0.0
        );
    }

    #[test]
    fn sleep_and_wake_thresholds() {
        assert!(can_fall_asleep(0.74, false));
        assert!(!can_fall_asleep(0.75, false));
        assert!(should_wake_up(1.0));
        assert!(!should_wake_up(0.99));
    }
}
