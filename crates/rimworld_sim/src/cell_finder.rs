//! Region-based random cell searches (docs/research.md §19): the game's
//! `CellFinder.TryRandomClosewalkCellNear`, `RCellFinder.RandomWanderDestFor`
//! and `RCellFinder.SpotToStandDuringJob`, drawing random numbers in the
//! game's order.

use rimworld_defs::GameDefs;

use crate::grid::Cell;
use crate::map::Map;
use crate::path::{COLONIST_HEURISTIC_STRENGTH, MoveCosts, PathGrid, find_path};
use crate::rand::Rand;
use crate::region::{RegionId, RegionType, Regions, random_element_by_weight};

/// What the searches read about the map.
pub struct MapView<'a> {
    pub map: &'a Map,
    pub defs: &'a GameDefs,
    pub grid: &'a PathGrid,
    pub regions: &'a Regions,
}

impl MapView<'_> {
    /// `GenGrid.Standable`: walkable and no non-standable thing on it.
    pub fn standable(&self, c: Cell) -> bool {
        self.grid.walkable(c) && self.map.standable_things(self.defs, c)
    }

    fn reachable(&self, from: Cell, to: Cell, costs: MoveCosts) -> bool {
        from == to || find_path(self.grid, from, to, costs, COLONIST_HEURISTIC_STRENGTH).is_ok()
    }
}

fn dist_sq(a: Cell, b: Cell) -> f32 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    (dx * dx + dz * dz) as f32
}

/// Picks one of `regions` weighted by cell count.
fn weighted_region(view: &MapView<'_>, regions: &[RegionId], rng: &mut Rand) -> Option<usize> {
    let weights: Vec<f32> = regions
        .iter()
        .map(|&r| view.regions.region(r).cell_count as f32)
        .collect();
    random_element_by_weight(&weights, rng)
}

/// `CellFinder.TryFindRandomReachableNearbyCell`: gathers the regions
/// within `radius` of `root` reachable from it, then repeatedly picks one
/// (weighted by size) and tries a random cell in it.
// COMPATIBILITY TODO: currently approximate — a non-standable root is not
// moved next to the blocking thing first.
pub fn try_find_random_reachable_nearby_cell(
    view: &MapView<'_>,
    root: Cell,
    radius: f32,
    mut cell_validator: impl FnMut(Cell) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    let start = view.regions.region_at(root)?;
    let r2 = radius * radius;
    let mut found = Vec::new();
    view.regions.traverse(
        start,
        |_, r| {
            radius > 1000.0 || view.regions.region(r).extents.closest_dist_squared_to(root) <= r2
        },
        |r| {
            found.push(r);
            false
        },
        999_999,
    );
    while !found.is_empty() {
        let i = weighted_region(view, &found, rng)?;
        let pick = view.regions.try_find_random_cell_in_region(
            found[i],
            |c, _| dist_sq(c, root) <= r2 && cell_validator(c),
            rng,
        );
        if pick.is_some() {
            return pick;
        }
        found.remove(i);
    }
    None
}

/// `CellFinder.TryRandomClosewalkCellNear`: a random standable cell within
/// `radius` reachable without passing closed doors.
pub fn try_random_closewalk_cell_near(
    view: &MapView<'_>,
    root: Cell,
    radius: i32,
    mut extra: impl FnMut(Cell) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    try_find_random_reachable_nearby_cell(
        view,
        root,
        radius as f32,
        |c| view.standable(c) && extra(c),
        rng,
    )
}

/// The wandering pawn, as `RandomWanderDestFor` sees it.
pub struct Wanderer {
    pub position: Cell,
    pub costs: MoveCosts,
}

/// `RCellFinder.RandomWanderDestFor`: up to 35 tries, each picking up to
/// five random cells of the regions near `root` until one is within
/// `radius`, then checking it (`CanWanderToCell`, stricter on early tries).
/// `available` is the destination-reservation check.
// COMPATIBILITY TODO: currently approximate — when every try fails the game
// falls back to `CellFinder.TryFindRandomCellNear` searches; we return the
// pawn's position (which makes it wait).
pub fn random_wander_dest_for(
    view: &MapView<'_>,
    pawn: &Wanderer,
    root: Cell,
    radius: f32,
    available: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Cell {
    let Some(start) = view.regions.region_at(root) else {
        return pawn.position;
    };
    let max_regions = ((radius as i32) / 3).max(13) as usize;
    let r2 = radius * radius;
    let mut regions = Vec::new();
    // `CellFinder.AllRegionsNear`.
    view.regions.traverse(
        start,
        |_, r| view.regions.region(r).extents.closest_dist_squared_to(root) <= r2,
        |r| {
            regions.push(r);
            false
        },
        max_regions,
    );
    if regions.is_empty() {
        return pawn.position;
    }
    for try_index in 0..35 {
        let mut pick = None;
        for _ in 0..5 {
            let Some(i) = weighted_region(view, &regions, rng) else {
                break;
            };
            let c = view.regions.random_cell(regions[i], rng);
            if dist_sq(c, root) <= r2 {
                pick = Some(c);
                break;
            }
        }
        if let Some(c) = pick
            && can_wander_to_cell(view, pawn, c, try_index, available)
        {
            return c;
        }
    }
    pawn.position
}

/// `RCellFinder.CanWanderToCell` for what exists on our maps.
// COMPATIBILITY TODO: currently approximate — danger, traps, sunlight,
// pollution, rot stink, fire, doors and water seekers are not modelled
// (none exist yet); no base-game terrain is `dangerous`.
fn can_wander_to_cell(
    view: &MapView<'_>,
    pawn: &Wanderer,
    c: Cell,
    try_index: usize,
    available: &dyn Fn(Cell) -> bool,
) -> bool {
    if !view.grid.walkable(c) {
        return false;
    }
    if try_index < 10 && !view.standable(c) {
        return false;
    }
    if !view.reachable(pawn.position, c, pawn.costs) {
        return false;
    }
    if try_index < 10 {
        let terrain = &view.defs.terrain[view.map.terrain[c]];
        if terrain.avoid_wander || view.grid.cost(c).is_none_or(|cost| cost > 20) {
            return false;
        }
    }
    available(c)
}

/// The colony wander root of a humanlike colonist without gathering spots
/// or colony buildings (`WanderUtility.GetColonyWanderRoot`): the position
/// of a random free colonist it can reach (possibly itself).
// COMPATIBILITY TODO: currently approximate — gathering spots and colony
// buildings (walls, chill destinations) are not modelled yet.
pub fn colony_wander_root(
    view: &MapView<'_>,
    pawn: &Wanderer,
    colonists: &[Cell],
    rng: &mut Rand,
) -> Cell {
    let reachable: Vec<Cell> = colonists
        .iter()
        .copied()
        .filter(|&c| view.reachable(pawn.position, c, pawn.costs) || touches(view, pawn, c))
        .collect();
    if reachable.is_empty() {
        return pawn.position;
    }
    reachable[rng.range(0, reachable.len() as i32) as usize]
}

/// Reachability with `PathEndMode.Touch`: next to the cell is enough.
fn touches(view: &MapView<'_>, pawn: &Wanderer, to: Cell) -> bool {
    Cell::NEIGHBORS_8.iter().any(|&d| {
        let n = to + d;
        view.grid.walkable(n) && view.reachable(pawn.position, n, pawn.costs)
    })
}

/// Search state for `SpotToStandDuringJob`.
pub struct StandSpotSearch<'a> {
    pub position: Cell,
    /// Destination-reservation check (`pawnDestinationReservationManager.CanReserve`).
    pub destination_free: &'a dyn Fn(Cell) -> bool,
    /// Cells holding a thing that blocks placing an item (`HaulPlaceBlockerIn`).
    pub blocked: &'a dyn Fn(Cell) -> bool,
}

/// `RCellFinder.SpotToStandDuringJob`: 30 attempts with widening limits
/// (4 cells / 1 region, then 4 regions, then 8 cells / 12 regions,
/// 15 cells / 16 regions, ...), some "desperate" (fewer checks). Each
/// attempt picks a random region near the pawn (by size) and a random cell
/// in it passing the checks and `extra`.
pub fn spot_to_stand_during_job(
    view: &MapView<'_>,
    search: &StandSpotSearch<'_>,
    mut extra: impl FnMut(Cell, &mut Rand) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    let start = view.regions.region_at(search.position)?;
    let (mut desperate, mut max_distance, mut max_regions) = (false, 4.0f32, 1usize);
    for i in 0..30 {
        match i {
            1 => desperate = true,
            2 => (desperate, max_regions) = (false, 4),
            6 => desperate = true,
            10 => (desperate, max_distance, max_regions) = (false, 8.0, 12),
            15 => desperate = true,
            20 => (max_distance, max_regions) = (15.0, 16),
            // Attempts 26-28 also ignore danger, which we do not model.
            26 => (max_distance, max_regions) = (5.0, 4),
            29 => (max_distance, max_regions) = (15.0, 16),
            _ => {}
        }
        let region = random_region_near(view, start, max_regions, rng);
        let Some(region) = region else { continue };
        let found = view.regions.try_find_random_cell_in_region(
            region,
            |c, rng| {
                if dist_sq(search.position, c) > max_distance * max_distance {
                    return false;
                }
                if !desperate
                    && (!view.standable(c)
                        || (search.blocked)(c)
                        || view
                            .regions
                            .region_at(c)
                            .is_some_and(|r| view.regions.region(r).kind == RegionType::Portal))
                {
                    return false;
                }
                (search.destination_free)(c) && extra(c, rng)
            },
            rng,
        );
        if found.is_some() {
            return found;
        }
    }
    let c = view.regions.random_cell(start, rng);
    if !extra(c, rng) {
        return None;
    }
    Some(view.regions.random_cell(start, rng))
}

/// `CellFinder.RandomRegionNear`: the root itself when at most one region
/// is allowed; otherwise one of the passable regions found breadth-first,
/// weighted by size.
fn random_region_near(
    view: &MapView<'_>,
    root: RegionId,
    max_regions: usize,
    rng: &mut Rand,
) -> Option<RegionId> {
    if max_regions <= 1 {
        return Some(root);
    }
    let mut found = Vec::new();
    view.regions.traverse(
        root,
        |_, _| true,
        |r| {
            found.push(r);
            false
        },
        max_regions,
    );
    weighted_region(view, &found, rng).map(|i| found[i])
}

/// The game's persistent shuffle lists for `TryFindAdjacentIngestionPlaceSpot`
/// (they keep their order between calls).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestionSpotOrder {
    pub cardinals: [Cell; 4],
    pub diagonals: [Cell; 4],
}

impl Default for IngestionSpotOrder {
    fn default() -> Self {
        Self {
            // `GenAdj.CardinalDirections` and `GenAdj.DiagonalDirections`.
            cardinals: [
                Cell::new(0, 1),
                Cell::new(1, 0),
                Cell::new(0, -1),
                Cell::new(-1, 0),
            ],
            diagonals: [
                Cell::new(-1, -1),
                Cell::new(-1, 1),
                Cell::new(1, 1),
                Cell::new(1, -1),
            ],
        }
    }
}

/// `Toils_Ingest.TryFindAdjacentIngestionPlaceSpot` without tables: the
/// cardinal and diagonal orders are reshuffled (six draws), then the first
/// walkable cell of cardinals, diagonals and the cell itself without an
/// item of the same def is the place spot.
pub fn ingestion_place_spot(
    view: &MapView<'_>,
    order: &mut IngestionSpotOrder,
    c: Cell,
    same_def_at: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    rng.shuffle(&mut order.cardinals);
    rng.shuffle(&mut order.diagonals);
    order
        .cardinals
        .iter()
        .chain(order.diagonals.iter())
        .chain(std::iter::once(&Cell::new(0, 0)))
        .map(|&d| c + d)
        .find(|&p| view.map.size().contains(p) && view.grid.walkable(p) && !same_def_at(p))
}
