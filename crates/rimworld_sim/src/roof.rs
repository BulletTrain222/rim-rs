//! Roofs (docs/research.md §30): which cells a roof can span, the build
//! roof area the game marks over enclosed rooms (`AutoBuildRoofAreaSetter`)
//! and the roof-building work giver (`WorkGiver_BuildRoof`).

use std::collections::VecDeque;

use rimworld_defs::{GameDefs, Passability};

use crate::grid::Cell;
use crate::map::{ItemId, Map};
use crate::region::{RegionType, Regions};

/// `RoofCollapseUtility.RoofMaxSupportDistance`.
pub const MAX_SUPPORT_DISTANCE: f32 = 6.9;

/// `InHorDistOf(c, 6.9)`.
fn within_support(a: Cell, b: Cell) -> bool {
    a.distance_squared(b) as f32 <= MAX_SUPPORT_DISTANCE * MAX_SUPPORT_DISTANCE
}

/// `GenAdj.CardinalDirectionsAndInside`.
const CARDINAL_AND_INSIDE: [Cell; 5] = [
    Cell::new(0, 1),
    Cell::new(1, 0),
    Cell::new(0, -1),
    Cell::new(-1, 0),
    Cell::new(0, 0),
];

/// `GenAdj.AdjacentCellsAndInside`: the eight neighbours, then the cell.
pub const ADJACENT_AND_INSIDE: [Cell; 9] = [
    Cell::new(0, 1),
    Cell::new(1, 1),
    Cell::new(1, 0),
    Cell::new(1, -1),
    Cell::new(0, -1),
    Cell::new(-1, -1),
    Cell::new(-1, 0),
    Cell::new(-1, 1),
    Cell::new(0, 0),
];

/// `TouchPathEndModeUtility.IsAdjacentOrInsideAndAllowedToTouch` for a
/// cell (or a thing that isn't a roof-holding edifice): on or next to it,
/// and a diagonal touch needs one of the two side cells walkable and not a
/// door.
pub fn touch_allowed(grid: &crate::path::PathGrid, map: &Map, from: Cell, target: Cell) -> bool {
    if from.chebyshev(target) > 1 {
        return false;
    }
    if from.x == target.x || from.z == target.z {
        return true;
    }
    let open = |c: Cell| grid.walkable(c) && map.door_at(c).is_none();
    open(Cell::new(from.x, target.z)) || open(Cell::new(target.x, from.z))
}

/// A building on the cell that holds up roofs (`def.holdsRoof`).
pub fn holds_roof(map: &Map, defs: &GameDefs, c: Cell) -> bool {
    map.size().contains(c) && map.buildings[c].is_some_and(|b| defs.things[b].holds_roof)
}

/// `GetRoofHolderOrImpassable`: a building that holds roofs or blocks the
/// cell.
pub fn roof_holder_or_impassable(
    map: &Map,
    defs: &GameDefs,
    c: Cell,
) -> Option<rimworld_defs::DefId<rimworld_defs::ThingDef>> {
    let b = map.buildings[c]?;
    let d = &defs.things[b];
    (d.holds_roof || d.passability == Passability::Impassable).then_some(b)
}

/// Breadth-first flood fill over 4-neighbours from `root` through cells
/// passing `pass`; `found` on a visited cell ends it with `true`.
fn flood(
    map: &Map,
    root: Cell,
    mut pass: impl FnMut(Cell) -> bool,
    mut found: impl FnMut(Cell) -> bool,
) -> bool {
    if !map.size().contains(root) || !pass(root) {
        return false;
    }
    let mut seen = std::collections::HashSet::from([root]);
    let mut open = VecDeque::from([root]);
    while let Some(c) = open.pop_front() {
        if found(c) {
            return true;
        }
        for d in &CARDINAL_AND_INSIDE[..4] {
            let n = c + *d;
            if map.size().contains(n) && !seen.contains(&n) && pass(n) {
                seen.insert(n);
                open.push_back(n);
            }
        }
    }
    false
}

/// `RoofCollapseUtility.WithinRangeOfRoofHolder`: a roof on `c` would be
/// held up: roofed cells (or, with `assume_roofed`, any cells) lead from
/// `c` to a roof holder within 6.9 cells.
// COMPATIBILITY TODO: currently approximate — there is no no-roof area,
// so `assume_roofed` takes every cell as roofable.
pub fn within_range_of_roof_holder(
    map: &Map,
    defs: &GameDefs,
    c: Cell,
    assume_roofed: bool,
) -> bool {
    flood(
        map,
        c,
        |x| (map.roofed(x) || x == c || assume_roofed) && within_support(x, c),
        |x| {
            CARDINAL_AND_INSIDE.iter().any(|&d| {
                let n = x + d;
                within_support(n, c) && holds_roof(map, defs, n)
            })
        },
    )
}

/// `RoofCollapseUtility.ConnectedToRoofHolder`: roofed cells (with `c`
/// itself when `assume_root`) connect `c` to a roof holder.
pub fn connected_to_roof_holder(map: &Map, defs: &GameDefs, c: Cell, assume_root: bool) -> bool {
    flood(
        map,
        c,
        |x| map.roofed(x) || (x == c && assume_root),
        |x| {
            CARDINAL_AND_INSIDE
                .iter()
                .any(|&d| holds_roof(map, defs, x + d))
        },
    )
}

/// `RoofUtility.FirstBlockingThing`: a plant that can't stand under a roof
/// (`interferesWithRoof`, trees).
pub fn blocking_plant(map: &Map, defs: &GameDefs, c: Cell) -> Option<ItemId> {
    let p = map.plant_at(c)?;
    defs.things[p.def]
        .plant
        .as_ref()
        .is_some_and(|pp| pp.interferes_with_roof)
        .then_some(p.id)
}

/// A room's cells, in region order then map order within each region.
pub fn room_cells(regions: &Regions, room: &[usize]) -> Vec<Cell> {
    room.iter().flat_map(|&r| regions.cells(r)).collect()
}

/// `AutoBuildRoofAreaSetter.TryGenerateAreaNow` for one room: an enclosed
/// room bordered by the colony's roof holders gets its cells, and the roof
/// holders around them, added to the build roof area where a roof would be
/// held up.
// COMPATIBILITY TODO: currently approximate — every roof holder is the
// colony's (no factions on buildings); `allowAutoroof` is honoured;
// natural rock counts as an unowned holder.
pub fn auto_roof_area(map: &mut Map, defs: &GameDefs, regions: &Regions, room: &[usize]) {
    if room.len() > 26
        || room
            .iter()
            .any(|&r| regions.region(r).kind == RegionType::Portal)
    {
        return;
    }
    let cells = room_cells(regions, room);
    let size = map.size();
    let edge = |c: Cell| c.x == 0 || c.z == 0 || c.x == size.width - 1 || c.z == size.height - 1;
    if cells.len() > 320 || cells.iter().any(|&c| edge(c)) {
        return;
    }
    let room_id = regions.room_of_region(room[0]);
    let in_room = |c: Cell| {
        regions
            .region_at(c)
            .is_some_and(|r| regions.room_of_region(r) == room_id)
    };
    // The border must hold colony walls, and nothing that forbids roofing.
    let mut ours = false;
    for &c in &cells {
        for d in &ADJACENT_AND_INSIDE[..8] {
            let b = c + *d;
            if !size.contains(b) || in_room(b) {
                continue;
            }
            if let Some(h) = roof_holder_or_impassable(map, defs, b) {
                let def = &defs.things[h];
                if def.building.as_ref().is_some_and(|bp| !bp.allow_autoroof) {
                    return;
                }
                if !def.building.as_ref().is_some_and(|bp| bp.is_natural_rock) {
                    ours = true;
                }
            }
        }
    }
    if !ours {
        return;
    }
    // Inner cells: the room (multi-cell holders next to it would join, but
    // our holders are single cells); then each inner cell with the holders
    // around it.
    let mut to_roof: Vec<Cell> = Vec::new();
    for &c in &cells {
        for (k, d) in ADJACENT_AND_INSIDE.iter().enumerate() {
            let n = c + *d;
            if size.contains(n)
                && (k == 8 || roof_holder_or_impassable(map, defs, n).is_some())
                && !to_roof.contains(&n)
            {
                to_roof.push(n);
            }
        }
    }
    for c in to_roof {
        if !map.roofed(c) && within_range_of_roof_holder(map, defs, c, true) {
            map.set_build_roof(c, true);
        }
    }
}

/// A signature of a room's shape, to notice new and changed rooms.
pub fn room_signature(regions: &Regions, room: &[usize]) -> u64 {
    let mut cells = room_cells(regions, room);
    cells.sort_by_key(|c| (c.z, c.x));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in cells {
        for v in [c.x as u64, c.z as u64] {
            h ^= v;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// `WorkGiver_BuildRoof`: a cell scanner over the build roof area.
pub struct BuildRoofGiver<'a> {
    pub regions: &'a Regions,
    pub job: Option<rimworld_defs::DefId<rimworld_defs::JobDef>>,
    pub cut_job: Option<rimworld_defs::DefId<rimworld_defs::JobDef>>,
}

impl BuildRoofGiver<'_> {
    /// `CanReach(c, Touch)`, or the building on `c` can be touched.
    fn touchable(&self, ctx: &crate::work::WorkContext<'_>, c: Cell) -> bool {
        ADJACENT_AND_INSIDE.iter().any(|&d| {
            let n = c + d;
            ctx.map.size().contains(n)
                && ctx.grid.walkable(n)
                && touch_allowed(ctx.grid, ctx.map, n, c)
                && (n == ctx.position || self.regions.connected(ctx.position, n))
        })
    }
}

impl crate::work::WorkGiver for BuildRoofGiver<'_> {
    fn should_skip(&self, ctx: &crate::work::WorkContext<'_>) -> bool {
        ctx.map.build_roof_cells().is_empty()
    }

    fn allow_unreachable(&self) -> bool {
        true
    }

    fn potential_cells(&self, ctx: &crate::work::WorkContext<'_>) -> Vec<Cell> {
        ctx.map.build_roof_cells()
    }

    // COMPATIBILITY TODO: currently approximate — danger is not modelled;
    // touch reachability is "a walkable cell on or next to it connected to
    // the pawn".
    fn job_on_cell(
        &self,
        ctx: &crate::work::WorkContext<'_>,
        c: Cell,
    ) -> Option<(crate::job::Job, Vec<ItemId>)> {
        use crate::reservation::{STACK_ALL, Target};
        if !ctx.map.build_roof(c) || ctx.map.roofed(c) {
            return None;
        }
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Ceiling(c), 1, 1, STACK_ALL)
        {
            return None;
        }
        if !self.touchable(ctx, c)
            || !within_range_of_roof_holder(ctx.map, ctx.defs, c, false)
            || !connected_to_roof_holder(ctx.map, ctx.defs, c, true)
        {
            return None;
        }
        if let Some(plant) = blocking_plant(ctx.map, ctx.defs, c) {
            // `HandleBlockingThingJob`: cut the tree first.
            return ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(plant), 1, 1, STACK_ALL)
                .then(|| crate::farm::cut_job(self.cut_job, plant));
        }
        Some((
            crate::job::Job {
                def: self.job,
                kind: crate::job::JobKind::BuildRoof {
                    cell: c,
                    stage: crate::job::RoofStage::Goto,
                },
                forced: false,
                urgency: crate::path::LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
