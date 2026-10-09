//! Work selection (docs/research.md §20): per-pawn work priorities, the
//! ordered work-giver lists and the `JobGiver_Work` dispatcher with the
//! `WorkGiver_Scanner` contract.
//!
//! Behaviour follows the game's `Pawn_WorkSettings`, `JobGiver_Work` and
//! `GenClosest` (verified against our own managed-code inspection; the
//! external work-selection report's runtime cases A–Q are reproduced as
//! tests).

use std::cell::RefCell;

use rimworld_defs::{DefId, GameDefs, WorkGiverDef, WorkTypeDef};

use crate::grid::Cell;
use crate::job::Job;
use crate::map::{ItemId, Map};
use crate::path::{COLONIST_HEURISTIC_STRENGTH, MoveCosts, PathGrid, find_path};
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{Claimant, ReservationManager};
use crate::rest::TimeAssignment;

/// Highest valid player priority number (lowest urgency).
pub const MAX_PRIORITY: i32 = 4;
/// Priority given to enabled work.
pub const DEFAULT_PRIORITY: i32 = 3;
/// Work types a new colonist starts with, besides always-active ones.
const INITIAL_ACTIVE_WORKS: usize = 6;
/// The global search's initial "closest distance".
const GLOBAL_INITIAL_DIST_SQ: f32 = 2.147_483_6e9;
/// Search radii used by the dispatcher.
const REACHABLE_RADIUS: f32 = 9999.0;
const UNREACHABLE_RADIUS: f32 = 99999.0;
/// The cell phase's initial closest squared distance.
const CELL_INITIAL_DIST_SQ: f32 = 99999.0;

/// A pawn's raw work priorities (`Pawn_WorkSettings`), by work type index.
/// 0 = off, 1 = highest, 4 = lowest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkSettings {
    priorities: Vec<i32>,
}

impl WorkSettings {
    /// `EnableAndInitialize`: everything off, then priority 3 for up to six
    /// work types that are not always active, ordered by the pawn's average
    /// relevant skill (descending, stable; 3 for a type without relevant
    /// skills), then for every always-active type. Disabled types stay off.
    pub fn initialize(defs: &GameDefs, disabled: &dyn Fn(DefId<WorkTypeDef>) -> bool) -> Self {
        Self::initialize_with_skills(defs, disabled, &|_| 0)
    }

    /// [`WorkSettings::initialize`] with the pawn's skill levels
    /// (`AverageOfRelevantSkillsFor`).
    // COMPATIBILITY TODO: currently approximate — passions and incapable
    // skills are not modelled.
    pub fn initialize_with_skills(
        defs: &GameDefs,
        disabled: &dyn Fn(DefId<WorkTypeDef>) -> bool,
        skill: &dyn Fn(&str) -> i32,
    ) -> Self {
        let mut s = Self {
            priorities: vec![0; defs.work_types.len()],
        };
        let average = |t: DefId<WorkTypeDef>| {
            let skills = &defs.work_types[t].relevant_skills;
            if skills.is_empty() {
                3.0f32
            } else {
                skills.iter().map(|k| skill(k) as f32).sum::<f32>() / skills.len() as f32
            }
        };
        let mut candidates: Vec<DefId<WorkTypeDef>> = defs
            .work_type_ids()
            .filter(|&t| !defs.work_types[t].always_start_active && !disabled(t))
            .collect();
        candidates.sort_by(|&a, &b| average(b).total_cmp(&average(a)));
        for &t in candidates.iter().take(INITIAL_ACTIVE_WORKS) {
            s.priorities[t.index()] = DEFAULT_PRIORITY;
        }
        for t in defs.work_type_ids() {
            if defs.work_types[t].always_start_active && !disabled(t) {
                s.priorities[t.index()] = DEFAULT_PRIORITY;
            }
        }
        s
    }

    /// The stored priority.
    pub fn raw(&self, t: DefId<WorkTypeDef>) -> i32 {
        self.priorities[t.index()]
    }

    /// `GetPriority`: with manual priorities off, humanlikes treat any
    /// enabled type as 3 (the stored number is kept).
    pub fn effective(&self, t: DefId<WorkTypeDef>, manual: bool, humanlike: bool) -> i32 {
        let p = self.raw(t);
        if humanlike && p > 0 && !manual {
            DEFAULT_PRIORITY
        } else {
            p
        }
    }

    /// `SetPriority`: a non-zero priority for a disabled type is refused.
    /// Other values are stored as given (the game logs but does not clamp
    /// out-of-range numbers).
    pub fn set(&mut self, t: DefId<WorkTypeDef>, priority: i32, disabled: bool) -> bool {
        if priority != 0 && disabled {
            return false;
        }
        self.priorities[t.index()] = priority;
        true
    }
}

/// The two ordered giver lists (`WorkGiversInOrderNormal` / `...Emergency`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GiverLists {
    pub normal: Vec<DefId<WorkGiverDef>>,
    pub emergency: Vec<DefId<WorkGiverDef>>,
}

/// `CacheWorkGiversInOrder`. Enabled types are sorted by
/// `naturalPriority + (4 − priority) × 100000` (descending, stable in
/// database order). Emergency givers go to the emergency list only when
/// their type's priority is at least as urgent as the most urgent enabled
/// type with a non-emergency giver; otherwise they join the normal list.
pub fn giver_lists(
    defs: &GameDefs,
    settings: &WorkSettings,
    manual: bool,
    humanlike: bool,
) -> GiverLists {
    let prio = |t: DefId<WorkTypeDef>| settings.effective(t, manual, humanlike);
    let mut threshold = 999;
    let mut types: Vec<DefId<WorkTypeDef>> = Vec::new();
    for t in defs.work_type_ids() {
        let p = prio(t);
        if p > 0 {
            if p < threshold
                && defs.work_types[t]
                    .givers_by_priority
                    .iter()
                    .any(|&g| !defs.work_givers[g].emergency)
            {
                threshold = p;
            }
            types.push(t);
        }
    }
    let score = |t: DefId<WorkTypeDef>| {
        (defs.work_types[t].natural_priority + (MAX_PRIORITY - prio(t)) * 100_000) as f32
    };
    // Stable: equal scores keep database order.
    types.sort_by(|&a, &b| score(b).total_cmp(&score(a)));
    let mut lists = GiverLists::default();
    for &t in &types {
        for &g in &defs.work_types[t].givers_by_priority {
            if defs.work_givers[g].emergency && prio(t) <= threshold {
                lists.emergency.push(g);
            }
        }
    }
    for &t in &types {
        for &g in &defs.work_types[t].givers_by_priority {
            if !defs.work_givers[g].emergency || prio(t) > threshold {
                lists.normal.push(g);
            }
        }
    }
    lists
}

/// `JobGiver_Work.GetPriority`: ordinary work's weight in the priority
/// sorter, from the current timetable assignment (0 without work settings).
pub fn work_priority(assignment: TimeAssignment, ever_work: bool) -> f32 {
    if !ever_work {
        return 0.0;
    }
    match assignment {
        TimeAssignment::Anything => 5.5,
        TimeAssignment::Work => 9.0,
        TimeAssignment::Sleep => 3.0,
        TimeAssignment::Joy | TimeAssignment::Meditate => 2.0,
    }
}

/// What a work giver can see and query.
pub struct WorkContext<'a> {
    pub defs: &'a GameDefs,
    pub map: &'a Map,
    pub grid: &'a PathGrid,
    pub regions: &'a Regions,
    /// The game's shared random stream (storage searches draw from it).
    pub rng: RefCell<&'a mut Rand>,
    pub reservations: &'a ReservationManager,
    pub claimant: Claimant,
    pub position: Cell,
    pub costs: MoveCosts,
    pub tick: u64,
    /// Capacities the pawn is not capable of (`CapableOf` false: level at
    /// most the capacity's `minForCapable`).
    pub incapable: &'a [String],
}

impl WorkContext<'_> {
    /// Reachability (`CanReach`) for a cell, by path search.
    // COMPATIBILITY TODO: currently approximate — path end modes other
    // than "on the cell" and danger limits are not modelled.
    pub fn can_reach(&self, c: Cell) -> bool {
        c == self.position
            || find_path(
                self.grid,
                self.position,
                c,
                self.costs,
                COLONIST_HEURISTIC_STRENGTH,
            )
            .is_ok()
            || Cell::NEIGHBORS_8.iter().any(|&d| {
                let n = c + d;
                self.grid.walkable(n)
                    && find_path(
                        self.grid,
                        self.position,
                        n,
                        self.costs,
                        COLONIST_HEURISTIC_STRENGTH,
                    )
                    .is_ok()
            })
    }
}

/// What a "Prioritize" float menu order was clicked on
/// (`FloatMenuOptionProvider_WorkGivers`): a thing, a natural rock (a
/// `Mineable`, which this simulation keeps as a cell) or a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WorkTarget {
    Thing(ItemId),
    Rock(Cell),
    Cell(Cell),
}

/// A job a work giver produced, with targets queued for it
/// (`job.targetQueueA`).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkJob {
    pub job: Job,
    pub queue: Vec<ItemId>,
    pub giver: DefId<WorkGiverDef>,
}

/// A work giver (`WorkGiver` / `WorkGiver_Scanner`). Defaults are the
/// scanner's: no targets, not prioritized, reachability required.
pub trait WorkGiver {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        false
    }
    fn non_scan_job(&self, _ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        None
    }
    /// `PotentialWorkThingsGlobal` (`None`: use the map's things matching
    /// [`WorkGiver::thing_request`]).
    fn potential_things(&self, _ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        None
    }
    /// `PotentialWorkThingRequest`: which map things the lister supplies.
    fn thing_request(&self, _ctx: &WorkContext<'_>, _item: ItemId) -> bool {
        false
    }
    /// Whether the giver's `PotentialWorkThingRequest` is a group stored in
    /// regions (blueprints, frames, filth), so a reachable search goes
    /// region by region; otherwise it searches its global set.
    fn region_request(&self) -> bool {
        false
    }
    fn prioritized(&self) -> bool {
        false
    }
    fn allow_unreachable(&self) -> bool {
        false
    }
    fn has_job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> bool {
        self.job_on_thing(ctx, t).is_some()
    }
    fn job_on_thing(&self, _ctx: &WorkContext<'_>, _t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        None
    }
    fn thing_priority(&self, _ctx: &WorkContext<'_>, _t: ItemId) -> f32 {
        0.0
    }
    fn potential_cells(&self, _ctx: &WorkContext<'_>) -> Vec<Cell> {
        Vec::new()
    }
    fn has_job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> bool {
        self.job_on_cell(ctx, c).is_some()
    }
    fn job_on_cell(&self, _ctx: &WorkContext<'_>, _c: Cell) -> Option<(Job, Vec<ItemId>)> {
        None
    }
    fn cell_priority(&self, _ctx: &WorkContext<'_>, _c: Cell) -> f32 {
        0.0
    }
    /// The float menu's question for one clicked target: `None` when the
    /// giver does not consider it at all (`ScannerShouldSkip`: not one of
    /// its potential things or cells); otherwise its job there
    /// (`HasJobOnThing` then `JobOnThing`, or the cell versions).
    fn job_on_target(
        &self,
        ctx: &WorkContext<'_>,
        target: WorkTarget,
    ) -> Option<Option<(Job, Vec<ItemId>)>> {
        match target {
            WorkTarget::Thing(t) => {
                let listed = self
                    .potential_things(ctx)
                    .map_or_else(|| self.thing_request(ctx, t), |v| v.contains(&t));
                if !listed {
                    return None;
                }
                Some(if self.has_job_on_thing(ctx, t) {
                    self.job_on_thing(ctx, t)
                } else {
                    None
                })
            }
            WorkTarget::Cell(c) => {
                if !self.potential_cells(ctx).contains(&c) {
                    return None;
                }
                Some(if self.has_job_on_cell(ctx, c) {
                    self.job_on_cell(ctx, c)
                } else {
                    None
                })
            }
            WorkTarget::Rock(_) => None,
        }
    }
}

/// Stand-in for givers that are not implemented: never any work.
struct NoWork;
impl WorkGiver for NoWork {}

/// Whether the pawn may use a giver at all (`PawnCanUseWorkGiver`),
/// including `MissingRequiredCapacity`.
// COMPATIBILITY TODO: currently approximate — work tags and work types are
// never disabled (no backstories or traits).
fn pawn_can_use(def: &WorkGiverDef, giver: &dyn WorkGiver, ctx: &WorkContext<'_>) -> bool {
    if !def.non_colonists_can_do && !ctx.claimant.has_faction {
        return false;
    }
    if def
        .required_capacities
        .iter()
        .any(|c| ctx.incapable.contains(c))
    {
        return false;
    }
    !giver.should_skip(ctx)
}

/// `Mathf.Approximately`.
fn approximately(a: f32, b: f32) -> bool {
    (b - a).abs() < (1e-6 * a.abs().max(b.abs())).max(f32::from_bits(1) * 8.0)
}

fn dist_sq(a: Cell, b: Cell) -> f32 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    (dx * dx + dz * dz) as f32
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Thing(ItemId),
    Cell(Cell),
}

/// `GenClosest.ClosestThing_Global` / `ClosestThing_Global_Reachable`.
fn closest_thing_global(
    ctx: &WorkContext<'_>,
    set: &[ItemId],
    max_distance: f32,
    reachable: bool,
    validator: &mut dyn FnMut(ItemId) -> bool,
    priority: Option<&dyn Fn(ItemId) -> f32>,
) -> Option<ItemId> {
    let mut closest = GLOBAL_INITIAL_DIST_SQ;
    let mut best_prio = f32::MIN;
    let mut chosen = None;
    let max_sq = max_distance * max_distance;
    for &t in set {
        let Some(position) = ctx.map.thing_position(t) else {
            continue;
        };
        let d = dist_sq(ctx.position, position);
        if d > max_sq || (priority.is_none() && d >= closest) {
            continue;
        }
        if reachable && !ctx.can_reach(position) {
            continue;
        }
        if !validator(t) {
            continue;
        }
        let mut p = 0.0;
        if let Some(get) = priority {
            p = get(t);
            if p < best_prio || (approximately(p, best_prio) && d >= closest) {
                continue;
            }
        }
        chosen = Some(t);
        closest = d;
        best_prio = p;
    }
    chosen
}

/// Regions searched before falling back to a global search
/// (`ClosestThingReachable` with no `searchRegionsMax`).
const MAX_REGIONS_BEFORE_GLOBAL: usize = 30;

/// Regions whose listers hold a thing (`GetTouchableRegions`): those of
/// its cells and of the passable cells around it that can touch it.
fn thing_regions(ctx: &WorkContext<'_>, t: ItemId) -> Vec<usize> {
    use crate::geom::Footprint;
    let fp = match ctx.map.constructible(t) {
        Some(k) => k.footprint(),
        None => match ctx.map.thing_position(t) {
            Some(c) => Footprint {
                center: c,
                rot: crate::job::Rot4::North,
                size: (1, 1),
            },
            None => return Vec::new(),
        },
    };
    let mut out = Vec::new();
    let cells: Vec<Cell> = fp.cells().collect();
    let around = cells.iter().copied().chain(fp.adjacent_8_way());
    for c in around {
        let Some(r) = ctx.regions.region_at(c) else {
            continue;
        };
        if !ctx.regions.region(r).kind.passable() || out.contains(&r) {
            continue;
        }
        if cells.contains(&c) || touches(ctx, c, &fp) {
            out.push(r);
        }
    }
    out
}

/// `ReachabilityImmediate.CanReachImmediate(cell, thing, Touch)` from a
/// cell next to the footprint.
// COMPATIBILITY TODO: currently approximate — the corner rule for diagonal
// touching is simplified to "not both side cells blocked".
fn touches(ctx: &WorkContext<'_>, from: Cell, fp: &crate::geom::Footprint) -> bool {
    if fp.distance(from) != 1 {
        return false;
    }
    let near = fp.nearest_cell(from);
    let (dx, dz) = (near.x - from.x, near.z - from.z);
    if dx == 0 || dz == 0 {
        return true;
    }
    ctx.grid.walkable(Cell::new(from.x + dx, from.z))
        || ctx.grid.walkable(Cell::new(from.x, from.z + dz))
}

/// `GenClosest.ClosestThingReachable` for a request stored in regions:
/// breadth-first over regions from the pawn's; each region's things
/// (from `set`; a thing is listed in every region it can be touched from)
/// are checked, keeping the nearest valid one, and the search stops after
/// the first region that yields one. With nothing found
/// within 30 regions it falls back to the global search over `set`.
// COMPATIBILITY TODO: currently approximate — a region's things are taken
// in `set` order (the game's region listers keep registration order),
// reachability within a region is assumed, and doors are entered like
// other regions.
fn closest_thing_reachable(
    ctx: &WorkContext<'_>,
    set: &[ItemId],
    validator: &mut dyn FnMut(ItemId) -> bool,
) -> Option<ItemId> {
    let root = ctx.regions.region_at(ctx.position)?;
    let mut by_region: std::collections::HashMap<usize, Vec<ItemId>> = Default::default();
    for &t in set {
        for r in thing_regions(ctx, t) {
            by_region.entry(r).or_default().push(t);
        }
    }
    let mut closest = GLOBAL_INITIAL_DIST_SQ;
    let mut chosen = None;
    let mut seen_regions = 0;
    ctx.regions.traverse(
        root,
        |_, _| true,
        |r| {
            if ctx.regions.region(r).kind != crate::region::RegionType::Portal {
                seen_regions += 1;
            }
            for &t in by_region.get(&r).map_or(&[][..], |v| v.as_slice()) {
                let Some(position) = ctx.map.thing_position(t) else {
                    continue;
                };
                let d = dist_sq(ctx.position, position);
                if d < closest && validator(t) {
                    closest = d;
                    chosen = Some(t);
                }
            }
            chosen.is_some()
        },
        MAX_REGIONS_BEFORE_GLOBAL,
    );
    if chosen.is_none() && seen_regions >= MAX_REGIONS_BEFORE_GLOBAL {
        return closest_thing_global(ctx, set, REACHABLE_RADIUS, true, validator, None);
    }
    chosen
}

/// `JobGiver_Work.TryIssueJobPackage` over an ordered giver list.
///
/// Givers run in order; each first gets a chance at a non-scan job, then
/// scans things and cells. The first giver to yield a job wins. A target
/// whose giver then produces no job is remembered, and stops the search at
/// the next change of `priorityInType` (the game logs an error there).
pub fn try_issue_job<'p>(
    ctx: &WorkContext<'_>,
    list: &[DefId<WorkGiverDef>],
    providers: &dyn Fn(DefId<WorkGiverDef>) -> Option<Box<dyn WorkGiver + 'p>>,
) -> Option<WorkJob> {
    let mut last_priority = -999;
    let mut remembered: Option<(Target, DefId<WorkGiverDef>, Box<dyn WorkGiver + 'p>)> = None;
    for &g in list {
        let def = &ctx.defs.work_givers[g];
        if def.priority_in_type != last_priority && remembered.is_some() {
            break;
        }
        // COMPATIBILITY TODO: currently approximate — givers we do not
        // implement act as eligible givers that find nothing (in the game
        // they might find work).
        let giver: Box<dyn WorkGiver + 'p> = providers(g).unwrap_or_else(|| Box::new(NoWork));
        if !pawn_can_use(def, giver.as_ref(), ctx) {
            continue;
        }
        if let Some((job, queue)) = giver.non_scan_job(ctx) {
            return Some(WorkJob {
                job,
                queue,
                giver: g,
            });
        }
        let mut found: Option<Target> = None;
        if def.scan_things {
            let set = giver.potential_things(ctx).unwrap_or_else(|| {
                ctx.map
                    .items()
                    .iter()
                    .map(|i| i.id)
                    .filter(|&id| giver.thing_request(ctx, id))
                    .collect()
            });
            let mut validator = |t: ItemId| giver.has_job_on_thing(ctx, t);
            let thing = if giver.prioritized() {
                let prio = |t: ItemId| giver.thing_priority(ctx, t);
                if giver.allow_unreachable() {
                    closest_thing_global(
                        ctx,
                        &set,
                        UNREACHABLE_RADIUS,
                        false,
                        &mut validator,
                        Some(&prio),
                    )
                } else {
                    closest_thing_global(
                        ctx,
                        &set,
                        REACHABLE_RADIUS,
                        true,
                        &mut validator,
                        Some(&prio),
                    )
                }
            } else if giver.allow_unreachable() {
                closest_thing_global(ctx, &set, UNREACHABLE_RADIUS, false, &mut validator, None)
            } else if giver.region_request() {
                closest_thing_reachable(ctx, &set, &mut validator)
            } else {
                closest_thing_global(ctx, &set, REACHABLE_RADIUS, true, &mut validator, None)
            };
            if let Some(t) = thing {
                found = Some(Target::Thing(t));
            }
        }
        if def.scan_cells {
            let prioritized = giver.prioritized();
            let mut closest = CELL_INITIAL_DIST_SQ;
            let mut best = f32::MIN;
            for c in giver.potential_cells(ctx) {
                let d = dist_sq(c, ctx.position);
                let take = if prioritized {
                    if !giver.has_job_on_cell(ctx, c) {
                        continue;
                    }
                    if !giver.allow_unreachable() && !ctx.can_reach(c) {
                        continue;
                    }
                    let p = giver.cell_priority(ctx, c);
                    let better = p > best || (p == best && d < closest);
                    if better {
                        best = p;
                    }
                    better
                } else {
                    if d >= closest || !giver.has_job_on_cell(ctx, c) {
                        continue;
                    }
                    if !giver.allow_unreachable() && !ctx.can_reach(c) {
                        continue;
                    }
                    true
                };
                if take {
                    found = Some(Target::Cell(c));
                    closest = d;
                }
            }
        }
        if let Some(target) = found {
            remembered = Some((target, g, giver));
        }
        if let Some((target, rg, rgiver)) = &remembered {
            let made = match *target {
                Target::Thing(t) => rgiver.job_on_thing(ctx, t),
                Target::Cell(c) => rgiver.job_on_cell(ctx, c),
            };
            if let Some((job, queue)) = made {
                return Some(WorkJob {
                    job,
                    queue,
                    giver: *rg,
                });
            }
        }
        last_priority = def.priority_in_type;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximately_matches_unity() {
        assert!(approximately(1.0, 1.000_000_5));
        assert!(!approximately(1.0, 1.000_01));
        assert!(approximately(0.0, 0.0));
    }

    #[test]
    fn work_priority_by_assignment() {
        assert_eq!(work_priority(TimeAssignment::Anything, true), 5.5);
        assert_eq!(work_priority(TimeAssignment::Work, true), 9.0);
        assert_eq!(work_priority(TimeAssignment::Sleep, true), 3.0);
        assert_eq!(work_priority(TimeAssignment::Joy, true), 2.0);
        assert_eq!(work_priority(TimeAssignment::Work, false), 0.0);
    }
}
