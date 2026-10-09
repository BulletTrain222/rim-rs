//! Recreation (docs/research.md §57): when a colonist seeks joy
//! (`ThinkNode_Priority_GetJoy`), which activity it picks
//! (`JobGiver_GetJoy`: a weighted draw over the `JoyGiverDef`s) and where
//! each activity happens (`JoyGiver_GoForWalk` with `WalkPathFinder`,
//! `JoyGiver_Skygaze`, `JoyGiver_InPrivateRoom`).

use rimworld_defs::{GameDefs, JoyGiverDef};

use crate::cell_finder::MapView;
use crate::grid::Cell;
use crate::job::{Job, JobKind, JoyActivity, JoyStage};
use crate::needs::JoyState;
use crate::path::LocomotionUrgency;
use crate::rand::Rand;
use crate::region::RegionId;
use crate::rest::TimeAssignment;

/// `ThinkNode_Priority_GetJoy.GameStartNoJoyTicks`.
pub const GAME_START_NO_JOY_TICKS: u64 = 5000;
/// `JobGiver_IdleJoy.GameStartNoIdleJoyTicks`.
pub const GAME_START_NO_IDLE_JOY_TICKS: u64 = 60_000;
/// `JoyTickCheckEnd`'s gain request: extra factor × `joyGainRate` × 0.36 /
/// 2,500 × delta, evaluated left to right at the runtime's wide precision
/// and rounded to binary32 when passed on.
pub fn joy_gain(extra: f32, rate: f32, delta: i32) -> f32 {
    (extra as f64 * rate as f64 * 0.36f32 as f64 / 2500.0 * delta as f64) as f32
}

/// What the joy givers know about the pawn and its surroundings.
#[derive(Debug, Clone)]
pub struct JoyFacts<'a> {
    pub level: f32,
    pub state: &'a JoyState,
    /// `thingIDNumber` (seeds `pctPawnsEverDo`).
    pub id_number: i32,
    pub assignment: TimeAssignment,
    pub tick: u64,
    /// `JoyUtility.EnjoyableOutsideNow`: the outdoor temperature is
    /// comfortable (there is no weather yet).
    pub enjoyable_outside: bool,
    /// `PawnUtility.WillSoonHaveBasicNeed`.
    pub soon_basic_need: bool,
    /// The cells of the pawn's own bedroom (`ownership.OwnedRoom`).
    pub owned_room: Option<&'a [Cell]>,
    /// Per room: `PsychologicallyOutdoors`.
    pub outdoor_rooms: &'a [bool],
    /// Capacities the pawn is incapable of.
    pub incapable: &'a [String],
}

/// `ThinkNode_Priority_GetJoy.GetPriority`.
pub fn get_joy_priority(joy: Option<&JoyFacts<'_>>) -> f32 {
    let Some(j) = joy else {
        return 0.0;
    };
    if j.tick < GAME_START_NO_JOY_TICKS {
        return 0.0;
    }
    match j.assignment {
        TimeAssignment::Anything if j.level < 0.35 => 6.0,
        TimeAssignment::Joy if j.level < 0.95 => 7.0,
        TimeAssignment::Sleep if j.level < 0.95 => 2.0,
        _ => 0.0,
    }
}

/// Whether the timetable allows joy now (`TimeAssignmentDef.allowJoy`).
pub fn allows_joy(a: TimeAssignment) -> bool {
    !matches!(a, TimeAssignment::Work)
}

/// `GenCollection.TryRandomElementByWeight` over a list: one draw unless
/// a single element.
pub fn try_random_element_by_weight(weights: &[f32], rng: &mut Rand) -> Option<usize> {
    let total: f32 = weights.iter().map(|w| w.max(0.0)).sum();
    if weights.len() == 1 && total > 0.0 {
        return Some(0);
    }
    if total == 0.0 {
        return None;
    }
    let mut left = total * rng.value();
    for (i, &w) in weights.iter().enumerate() {
        if w <= 0.0 {
            continue;
        }
        left -= w;
        if left <= 0.0 {
            return Some(i);
        }
    }
    None
}

/// The joy job givers' map access.
pub struct JoyEnv<'a, 'b> {
    pub view: &'b MapView<'a>,
    pub at: Cell,
    pub job_defs: &'b crate::job::JobDefs,
    /// Whether the pawn can reserve a cell.
    pub reservable: &'b dyn Fn(Cell) -> bool,
}

/// `JobGiver_GetJoy.TryGiveJob` (`in_bed`: `JobGiver_GetJoyInBed`'s
/// variant, which only offers in-bed givers).
// COMPATIBILITY TODO: currently approximate — only walking, skygazing,
// praying and meditating (in the pawn's bedroom) have workers; givers for
// buildings, art, graves, snowmen, drugs, food, books and social
// relaxation find nothing. Medical rest in bed is not checked.
pub fn get_joy(
    defs: &GameDefs,
    joy: &JoyFacts<'_>,
    env: &JoyEnv<'_, '_>,
    rng: &mut Rand,
) -> Option<Job> {
    if joy.level >= 0.99 {
        return None;
    }
    let givers: Vec<&JoyGiverDef> = defs.joy_givers.iter().map(|(_, g)| g).collect();
    let mut chances: Vec<f32> = givers
        .iter()
        .map(|g| {
            let kind = g.joy_kind.as_deref().unwrap_or("");
            if joy.state.bored_of(kind) || !can_be_given_to(g, joy) {
                return 0.0;
            }
            if g.pct_pawns_ever_do < 1.0 {
                rng.push_state_seeded(joy.id_number ^ 0x3C4_9C49);
                let v = rng.value();
                rng.pop_state();
                if v >= g.pct_pawns_ever_do {
                    return 0.0;
                }
            }
            let tolerance = joy.state.tolerance(kind);
            g.base_chance * (1.0 - tolerance).powi(5).max(0.001)
        })
        .collect();
    for _ in 0..chances.len() {
        let i = try_random_element_by_weight(&chances, rng)?;
        if let Some(job) = try_give_job(defs, givers[i], joy, env, rng) {
            return Some(job);
        }
        chances[i] = 0.0;
    }
    None
}

/// `JoyGiver.CanBeGivenTo`: required capacities (outdoor enjoyment and
/// royal titles always allow it here).
fn can_be_given_to(g: &JoyGiverDef, joy: &JoyFacts<'_>) -> bool {
    !g.required_capacities
        .iter()
        .any(|c| joy.incapable.contains(c))
}

fn joy_job(
    defs: &GameDefs,
    g: &JoyGiverDef,
    activity: JoyActivity,
    urgency: LocomotionUrgency,
) -> Option<Job> {
    let def = defs.jobs.id(g.job.as_deref()?)?;
    Some(Job {
        def: Some(def),
        kind: JobKind::Joy {
            activity,
            stage: JoyStage::Goto,
        },
        forced: false,
        urgency,
        start_tick: 0,
    })
}

pub fn try_give_job(
    defs: &GameDefs,
    g: &JoyGiverDef,
    joy: &JoyFacts<'_>,
    env: &JoyEnv<'_, '_>,
    rng: &mut Rand,
) -> Option<Job> {
    let _ = env.job_defs;
    match g.giver_class.as_str() {
        "JoyGiver_GoForWalk" => {
            if !joy.enjoyable_outside || joy.soon_basic_need {
                return None;
            }
            let cell_ok = |c: Cell| walk_cell(env.view, c);
            let region = closest_region_with(env, joy, 100, &cell_ok, rng)?;
            let root =
                env.view
                    .regions
                    .try_find_random_cell_in_region(region, |c, _| cell_ok(c), rng)?;
            let path = walk_path(env.view, root)?;
            joy_job(
                defs,
                g,
                JoyActivity::Walk {
                    path,
                    len: WALK_PATH_LEN as u8,
                    next: 0,
                },
                LocomotionUrgency::Walk,
            )
        }
        "JoyGiver_Skygaze" => {
            if !joy.enjoyable_outside {
                return None;
            }
            let cell = skygaze_cell(env, joy, rng)?;
            joy_job(
                defs,
                g,
                JoyActivity::Skygaze { cell },
                LocomotionUrgency::Jog,
            )
        }
        // Without Royalty, meditating is relaxing in one's own room, like
        // praying (`JoyGiver_InPrivateRoom`).
        "JoyGiver_Meditate" | "JoyGiver_InPrivateRoom" => {
            // Standable, reservable room cells; `Range(0, count)` (one
            // draw, even for one cell).
            // COMPATIBILITY TODO: currently approximate — fog, forbidden
            // areas and reachability are not checked; cells come in our
            // room-cell order.
            let cells = joy.owned_room?;
            let ok: Vec<Cell> = cells
                .iter()
                .copied()
                .filter(|&c| env.view.standable(c) && (env.reservable)(c))
                .collect();
            if ok.is_empty() {
                return None;
            }
            let cell = ok[rng.range(0, ok.len() as i32) as usize];
            joy_job(defs, g, JoyActivity::Relax { cell }, LocomotionUrgency::Jog)
        }
        _ => None,
    }
}

/// A standable, unroofed cell (Skygaze's `IsGoodDestinationFor` part).
// COMPATIBILITY TODO: currently approximate — fog, forbidden areas,
// dangerous terrain and danger are not checked.
fn outdoor_cell(view: &MapView<'_>, c: Cell) -> bool {
    view.map.size().contains(c) && view.standable(c) && !view.map.roofed(c)
}

/// The walk validators also reject `avoidWander` terrain.
fn walk_cell(view: &MapView<'_>, c: Cell) -> bool {
    outdoor_cell(view, c) && !view.defs.terrain[view.map.terrain[c]].avoid_wander
}

/// The region validator: in a psychologically outdoor room, with a cell
/// passing `cell_ok` (a random-cell search, drawing random numbers).
fn region_ok(
    env: &JoyEnv<'_, '_>,
    joy: &JoyFacts<'_>,
    r: RegionId,
    cell_ok: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> bool {
    env.view
        .regions
        .room_of_region(r)
        .is_some_and(|room| joy.outdoor_rooms.get(room).copied().unwrap_or(false))
        && env
            .view
            .regions
            .try_find_random_cell_in_region(r, |c, _| cell_ok(c), rng)
            .is_some()
}

/// `CellFinder.TryFindClosestRegionWith` from the pawn's region.
fn closest_region_with(
    env: &JoyEnv<'_, '_>,
    joy: &JoyFacts<'_>,
    max_regions: usize,
    cell_ok: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Option<RegionId> {
    let root = env.view.regions.region_at(env.at)?;
    let mut found = None;
    env.view.regions.traverse(
        root,
        |_, _| true,
        |r| {
            if region_ok(env, joy, r, cell_ok, rng) {
                found = Some(r);
                true
            } else {
                false
            }
        },
        max_regions,
    );
    found
}

/// `RCellFinder.TryFindSkygazeCell`: the closest outdoor region within 45,
/// then a random region near it (14 regions, weighted by size) and a cell
/// in it.
fn skygaze_cell(env: &JoyEnv<'_, '_>, joy: &JoyFacts<'_>, rng: &mut Rand) -> Option<Cell> {
    let cell_ok = |c: Cell| outdoor_cell(env.view, c);
    let start = closest_region_with(env, joy, 45, &cell_ok, rng)?;
    // `RandomRegionNear(start, 14, validator)`: the validator gates entry
    // into each neighbour; every region reached is a candidate.
    let mut near = Vec::new();
    env.view.regions.traverse(
        start,
        |_, r| region_ok(env, joy, r, &cell_ok, rng),
        |r| {
            near.push(r);
            false
        },
        14,
    );
    let weights: Vec<f32> = near
        .iter()
        .map(|&r| env.view.regions.region(r).cell_count as f32)
        .collect();
    let pick = near[crate::region::random_element_by_weight(&weights, rng)?];
    env.view
        .regions
        .try_find_random_cell_in_region(pick, |c, _| cell_ok(c), rng)
}

/// `GenSight.LineOfSight(start, end)`: the 4×-scaled line walk, every cell
/// but the last must not be filled by a sight-blocking building.
pub fn line_of_sight(view: &MapView<'_>, start: Cell, end: Cell) -> bool {
    let size = view.map.size();
    if !size.contains(start) || !size.contains(end) {
        return false;
    }
    let toward_positive = if start.x != end.x {
        start.x < end.x
    } else {
        start.z < end.z
    };
    let mut dx = (end.x - start.x).abs();
    let mut dz = (end.z - start.z).abs();
    let (mut x, mut z) = (start.x, start.z);
    let mut steps = 1 + dx + dz;
    let sx = if end.x > start.x { 1 } else { -1 };
    let sz = if end.z > start.z { 1 } else { -1 };
    dx *= 4;
    dz *= 4;
    let mut err = dx / 2 - dz / 2;
    while steps > 1 {
        if !can_be_seen_over(view, Cell::new(x, z)) {
            return false;
        }
        if err > 0 || (err == 0 && toward_positive) {
            x += sx;
            err -= dz;
        } else {
            z += sz;
            err += dx;
        }
        steps -= 1;
    }
    true
}

/// `CanBeSeenOverFast`: no full-fill edifice (open doors don't block).
fn can_be_seen_over(view: &MapView<'_>, c: Cell) -> bool {
    match view.map.structure_at(c) {
        Some(s) if view.defs.things[s.def].fill_percent >= 0.99 => {
            view.map.door_at(c).is_some_and(|d| d.open)
        }
        _ => true,
    }
}

/// `WalkPathFinder.TryFindWalkPath`: eight waypoints, each the best of
/// every third radial cell between 14 and 2 cells from the last (far from
/// the waypoints so far, not doubling back, heading home after four),
/// then back to the start.
pub fn walk_path(view: &MapView<'_>, root: Cell) -> Option<[Cell; WALK_PATH_LEN]> {
    let radial = crate::clean::radial_pattern();
    let start = num_cells_in_radius(14.0);
    let end = num_cells_in_radius(2.0);
    let mut list = vec![root];
    let mut cur = root;
    for _ in 0..8 {
        let mut best: Option<Cell> = None;
        let mut best_score = -1.0f32;
        let mut i = start;
        while i > end {
            let c = cur + radial[i];
            i -= 3;
            if !walk_cell(view, c) || !line_of_sight(view, cur, c) {
                continue;
            }
            let mut score = 10_000.0f32;
            for &p in &list {
                score += manhattan(p, c) as f32;
            }
            let from_root = manhattan(c, root) as f32;
            if from_root > 40.0 {
                score *= inverse_lerp(70.0, 40.0, from_root);
            }
            if list.len() >= 2 {
                let (p, q) = (list[list.len() - 1], list[list.len() - 2]);
                let mut a = angle_flat(Cell::new(p.x - q.x, p.z - q.z));
                let b = angle_flat(Cell::new(c.x - cur.x, c.z - cur.z));
                let turn = if b > a {
                    b - a
                } else {
                    a -= 360.0;
                    b - a
                };
                if turn > 110.0 {
                    score *= 0.01;
                }
            }
            if list.len() >= 4 && manhattan(cur, root) < manhattan(c, root) {
                score *= 1e-5;
            }
            if score > best_score {
                best = Some(c);
                best_score = score;
            }
        }
        let next = best?;
        list.push(next);
        cur = next;
    }
    list.push(root);
    list.try_into().ok()
}

/// A walk's waypoints: the start, eight more, the start again.
pub const WALK_PATH_LEN: usize = 10;

fn manhattan(a: Cell, b: Cell) -> i32 {
    (a.x - b.x).abs() + (a.z - b.z).abs()
}

/// `Mathf.InverseLerp`.
fn inverse_lerp(a: f32, b: f32, v: f32) -> f32 {
    if a != b {
        ((v - a) / (b - a)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `IntVec3.AngleFlat`: degrees clockwise from north (x east, z north).
fn angle_flat(v: Cell) -> f32 {
    if v.x == 0 && v.z == 0 {
        return 0.0;
    }
    // `Vector3.AngleFlat`: atan2 in degrees, turned so north is 0 and
    // angles grow clockwise, in [0, 360).
    let a = (v.x as f32).atan2(v.z as f32).to_degrees();
    if a < 0.0 { a + 360.0 } else { a }
}

/// `GenRadial.NumCellsInRadius`.
fn num_cells_in_radius(radius: f32) -> usize {
    let r2 = radius * radius;
    crate::clean::radial_pattern()
        .iter()
        .position(|c| (c.x * c.x + c.z * c.z) as f32 > r2)
        .unwrap_or(crate::clean::radial_pattern().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_by_assignment_and_level() {
        let state = JoyState::default();
        let facts = |level, assignment, tick| JoyFacts {
            level,
            state: &state,
            id_number: 0,
            assignment,
            tick,
            enjoyable_outside: true,
            soon_basic_need: false,
            owned_room: None,
            outdoor_rooms: &[],
            incapable: &[],
        };
        assert_eq!(get_joy_priority(None), 0.0);
        assert_eq!(
            get_joy_priority(Some(&facts(0.1, TimeAssignment::Anything, 4999))),
            0.0
        );
        assert_eq!(
            get_joy_priority(Some(&facts(0.34, TimeAssignment::Anything, 6000))),
            6.0
        );
        assert_eq!(
            get_joy_priority(Some(&facts(0.35, TimeAssignment::Anything, 6000))),
            0.0
        );
        assert_eq!(
            get_joy_priority(Some(&facts(0.9, TimeAssignment::Joy, 6000))),
            7.0
        );
        assert_eq!(
            get_joy_priority(Some(&facts(0.9, TimeAssignment::Sleep, 6000))),
            2.0
        );
        assert_eq!(
            get_joy_priority(Some(&facts(0.1, TimeAssignment::Work, 6000))),
            0.0
        );
    }

    #[test]
    fn angles_are_clockwise_from_north() {
        assert_eq!(angle_flat(Cell::new(0, 1)), 0.0);
        assert_eq!(angle_flat(Cell::new(1, 0)), 90.0);
        assert_eq!(angle_flat(Cell::new(0, -1)), 180.0);
        assert_eq!(angle_flat(Cell::new(-1, 0)), 270.0);
    }

    #[test]
    fn weighted_pick() {
        let mut rng = Rand::new(5);
        assert_eq!(try_random_element_by_weight(&[0.0, 0.0], &mut rng), None);
        assert_eq!(try_random_element_by_weight(&[2.0], &mut rng), Some(0));
        assert_eq!(
            try_random_element_by_weight(&[0.0, 3.0, 0.0], &mut rng),
            Some(1)
        );
    }
}
