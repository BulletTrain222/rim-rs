//! Regions: the game's coarse map partition (docs/research.md §19).
//!
//! The map is cut into 12×12 blocks; inside a block, each 4-connected group
//! of cells of the same region type forms a region. Regions are joined by
//! links (edge spans on their borders). Random cell searches ("a random
//! cell near here") pick regions by breadth-first traversal over links and
//! cells inside regions, so building regions in the game's order matters:
//! regions are generated from roots in map order (z, then x), each filled
//! breadth-first (south, west, north, east), and each region's links are
//! created by sweeping its cells in fill order (north, south, east, west).

use std::collections::{HashMap, VecDeque};

use rimworld_defs::GameDefs;

use crate::grid::{Cell, Grid, GridSize};
use crate::map::Map;
use crate::path::PathGrid;
use crate::rand::Rand;

/// Side of the blocks regions may not cross.
pub const REGION_SIZE: i32 = 12;

/// Fill order of the region flood fill (`GenAdj.CardinalDirectionsAround`).
const FILL_ORDER: [Cell; 4] = [
    Cell::new(0, -1),
    Cell::new(-1, 0),
    Cell::new(0, 1),
    Cell::new(1, 0),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionType {
    Normal,
    /// A door cell (one-cell region).
    Portal,
    /// Impassable but not filling the cell (air still flows).
    ImpassableFreeAir,
}

impl RegionType {
    /// In the game's `Set_Passable` (Normal, Portal, Fence).
    pub fn passable(self) -> bool {
        matches!(self, RegionType::Normal | RegionType::Portal)
    }
}

/// Inclusive cell rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub min_x: i32,
    pub max_x: i32,
    pub min_z: i32,
    pub max_z: i32,
}

impl Rect {
    pub fn contains(&self, c: Cell) -> bool {
        (self.min_x..=self.max_x).contains(&c.x) && (self.min_z..=self.max_z).contains(&c.z)
    }

    /// Squared distance from `c` to the nearest cell of the rectangle
    /// (`CellRect.ClosestDistSquaredTo`).
    pub fn closest_dist_squared_to(&self, c: Cell) -> f32 {
        let dx = if c.x < self.min_x {
            self.min_x - c.x
        } else if c.x > self.max_x {
            c.x - self.max_x
        } else {
            0
        };
        let dz = if c.z < self.min_z {
            self.min_z - c.z
        } else if c.z > self.max_z {
            c.z - self.max_z
        } else {
            0
        };
        (dx * dx + dz * dz) as f32
    }

    /// `CellRect.RandomCell`: x first, then z.
    pub fn random_cell(&self, rng: &mut Rand) -> Cell {
        let x = rng.range_inclusive(self.min_x, self.max_x);
        let z = rng.range_inclusive(self.min_z, self.max_z);
        Cell::new(x, z)
    }
}

pub type RegionId = usize;

#[derive(Debug, Clone)]
pub struct Region {
    pub id: RegionId,
    pub kind: RegionType,
    /// Bounding box of the region's cells (`extentsClose`).
    pub extents: Rect,
    pub cell_count: usize,
    /// Links in creation order.
    pub links: Vec<usize>,
}

/// An edge span between regions: `len` cells starting at `root`, running
/// east (`north_dir == false`, a horizontal edge) or north.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Span {
    root: Cell,
    north: bool,
    len: i32,
}

#[derive(Debug, Clone)]
pub struct Link {
    /// The two regions, in registration order.
    pub regions: [Option<RegionId>; 2],
}

#[derive(Debug, Clone)]
pub struct Regions {
    grid: Grid<Option<RegionId>>,
    regions: Vec<Region>,
    links: Vec<Link>,
    /// Room of each region: passable regions connected through links
    /// without passing a door; each door is a room of its own.
    room_of: Vec<Option<usize>>,
    /// Connected component of each passable region (doors included): what
    /// a pawn able to open doors can reach.
    component_of: Vec<Option<usize>>,
}

/// The region type a cell belongs to, or `None` for cells outside every
/// region (filled by a building) (`RegionTypeUtility.GetExpectedRegionType`).
// COMPATIBILITY TODO: currently approximate — fences do not exist yet.
pub fn expected_type(map: &Map, defs: &GameDefs, grid: &PathGrid, c: Cell) -> Option<RegionType> {
    if !map.size().contains(c) {
        return None;
    }
    if map.door_at(c).is_some() {
        return Some(RegionType::Portal);
    }
    if grid.walkable(c) {
        return Some(RegionType::Normal);
    }
    if map.buildings[c].is_some_and(|b| defs.things[b].fill_percent > 0.99) {
        return None;
    }
    Some(RegionType::ImpassableFreeAir)
}

impl Regions {
    /// Builds all regions (`RebuildAllRegionsAndRooms` on a clean map).
    pub fn build(map: &Map, defs: &GameDefs, path_grid: &PathGrid) -> Self {
        let size = map.size();
        let types = Grid::from_fn(size, |c| expected_type(map, defs, path_grid, c));
        let mut out = Regions {
            grid: Grid::new(size, None),
            regions: Vec::new(),
            links: Vec::new(),
            room_of: Vec::new(),
            component_of: Vec::new(),
        };
        let mut link_index: HashMap<Span, usize> = HashMap::new();
        for z in 0..size.height {
            for x in 0..size.width {
                let root = Cell::new(x, z);
                if out.grid[root].is_none()
                    && let Some(kind) = types[root]
                {
                    out.generate(root, kind, &types, &mut link_index);
                }
            }
        }
        out.assign_rooms();
        out
    }

    /// Groups regions into rooms and passable regions into connected
    /// components (through doors). A room joins normal regions and
    /// impassable free-air regions (water, low obstacles) linked to each
    /// other (`RegionAndRoomUpdater.ShouldBeInTheSameRoom`): air passes over
    /// water, so water does not wall a room off. Doors are rooms of their
    /// own.
    // COMPATIBILITY TODO: currently approximate — fences do not exist yet;
    // the game goes through districts first, which only matters for
    // district-level queries we don't make.
    fn assign_rooms(&mut self) {
        self.room_of =
            self.flood(|kind| matches!(kind, RegionType::Normal | RegionType::ImpassableFreeAir));
        self.component_of = self.flood(RegionType::passable);
        // Each door is a room of its own.
        let mut next = self.room_of.iter().flatten().max().map_or(0, |m| m + 1);
        for r in 0..self.regions.len() {
            if self.regions[r].kind == RegionType::Portal {
                self.room_of[r] = Some(next);
                next += 1;
            }
        }
    }

    /// Numbers groups of regions of the accepted kinds connected through
    /// links.
    fn flood(&self, accept: impl Fn(RegionType) -> bool) -> Vec<Option<usize>> {
        let mut out = vec![None; self.regions.len()];
        let mut next = 0;
        for start in 0..self.regions.len() {
            if out[start].is_some() || !accept(self.regions[start].kind) {
                continue;
            }
            let mut stack = vec![start];
            out[start] = Some(next);
            while let Some(r) = stack.pop() {
                let neighbors: Vec<RegionId> = self.neighbors(r).collect();
                for n in neighbors {
                    if out[n].is_none() && accept(self.regions[n].kind) {
                        out[n] = Some(next);
                        stack.push(n);
                    }
                }
            }
            next += 1;
        }
        out
    }

    /// Whether a pawn that can open doors gets from `a` to `b`
    /// (both in passable regions of one connected component).
    pub fn connected(&self, a: Cell, b: Cell) -> bool {
        matches!((self.component_at(a), self.component_at(b)), (Some(x), Some(y)) if x == y)
    }

    /// The connected component (through doors) containing `c`.
    pub fn component_at(&self, c: Cell) -> Option<usize> {
        self.component_of.get(self.region_at(c)?).copied().flatten()
    }

    /// The room containing `c`, if it is in a passable region.
    pub fn room_at(&self, c: Cell) -> Option<usize> {
        self.room_of.get(self.region_at(c)?).copied().flatten()
    }

    fn generate(
        &mut self,
        root: Cell,
        kind: RegionType,
        types: &Grid<Option<RegionType>>,
        link_index: &mut HashMap<Span, usize>,
    ) {
        let size = types.size();
        let id = self.regions.len();
        let block = Rect {
            min_x: root.x - root.x % REGION_SIZE,
            max_x: (root.x - root.x % REGION_SIZE + REGION_SIZE - 1).min(size.width - 1),
            min_z: root.z - root.z % REGION_SIZE,
            max_z: (root.z - root.z % REGION_SIZE + REGION_SIZE - 1).min(size.height - 1),
        };
        // Breadth-first fill inside the block.
        let mut cells = Vec::new();
        let mut queue = VecDeque::from([root]);
        self.grid[root] = Some(id);
        while let Some(c) = queue.pop_front() {
            cells.push(c);
            if kind == RegionType::Portal {
                break;
            }
            for d in FILL_ORDER {
                let n = c + d;
                if block.contains(n) && self.grid[n].is_none() && types[n] == Some(kind) {
                    self.grid[n] = Some(id);
                    queue.push_back(n);
                }
            }
        }
        let extents = cells.iter().fold(
            Rect {
                min_x: root.x,
                max_x: root.x,
                min_z: root.z,
                max_z: root.z,
            },
            |r, c| Rect {
                min_x: r.min_x.min(c.x),
                max_x: r.max_x.max(c.x),
                min_z: r.min_z.min(c.z),
                max_z: r.max_z.max(c.z),
            },
        );
        self.regions.push(Region {
            id,
            kind,
            extents,
            cell_count: cells.len(),
            links: Vec::new(),
        });
        // Links: sweep each border run once per direction.
        let mut done: [Vec<Cell>; 4] = Default::default();
        for &c in &cells {
            for (k, dir) in [
                Cell::new(0, 1),  // north
                Cell::new(0, -1), // south
                Cell::new(1, 0),  // east
                Cell::new(-1, 0), // west
            ]
            .into_iter()
            .enumerate()
            {
                self.sweep_link(id, c, dir, &mut done[k], types, size, link_index);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sweep_link(
        &mut self,
        id: RegionId,
        c: Cell,
        dir: Cell,
        done: &mut Vec<Cell>,
        types: &Grid<Option<RegionType>>,
        size: GridSize,
        link_index: &mut HashMap<Span, usize>,
    ) {
        if done.contains(&c) {
            return;
        }
        let other = c + dir;
        if size.contains(other) && self.grid[other] == Some(id) {
            return;
        }
        let Some(other_type) = types.get(other).copied().flatten() else {
            return;
        };
        // Run along the edge: clockwise of `dir` is "forward".
        let fwd = Cell::new(dir.z, -dir.x);
        done.push(c);
        let (mut ahead, mut behind) = (0, 0);
        if other_type != RegionType::Portal {
            let along = |n: i32| Cell::new(c.x + fwd.x * n, c.z + fwd.z * n);
            let continues = |cell: Cell, this: &Self| {
                size.contains(cell)
                    && this.grid[cell] == Some(id)
                    && types.get(cell + dir).copied().flatten() == Some(other_type)
            };
            while continues(along(ahead + 1), self) {
                ahead += 1;
                done.push(along(ahead));
            }
            while continues(along(-(behind + 1)), self) {
                behind += 1;
                done.push(along(-behind));
            }
        }
        let len = ahead + behind + 1;
        let shift = |n: i32| Cell::new(c.x + fwd.x * n, c.z + fwd.z * n);
        let span = match (dir.x, dir.z) {
            (0, 1) => {
                let r = shift(-behind);
                Span {
                    root: Cell::new(r.x, r.z + 1),
                    north: false,
                    len,
                }
            }
            (0, -1) => Span {
                root: shift(ahead),
                north: false,
                len,
            },
            (1, 0) => {
                let r = shift(ahead);
                Span {
                    root: Cell::new(r.x + 1, r.z),
                    north: true,
                    len,
                }
            }
            _ => Span {
                root: shift(-behind),
                north: true,
                len,
            },
        };
        let link = *link_index.entry(span).or_insert_with(|| {
            self.links.push(Link {
                regions: [None, None],
            });
            self.links.len() - 1
        });
        let slots = &mut self.links[link].regions;
        if slots[0].is_none() {
            slots[0] = Some(id);
        } else if slots[1].is_none() {
            slots[1] = Some(id);
        }
        self.regions[id].links.push(link);
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn region(&self, id: RegionId) -> &Region {
        &self.regions[id]
    }

    /// The room a region belongs to.
    pub fn room_of_region(&self, id: RegionId) -> Option<usize> {
        self.room_of.get(id).copied().flatten()
    }

    /// Each room's regions, in region order.
    pub fn rooms(&self) -> Vec<Vec<RegionId>> {
        let n = self.room_of.iter().flatten().max().map_or(0, |m| m + 1);
        let mut out = vec![Vec::new(); n];
        for (r, room) in self.room_of.iter().enumerate() {
            if let Some(room) = room {
                out[*room].push(r);
            }
        }
        out
    }

    pub fn region_at(&self, c: Cell) -> Option<RegionId> {
        self.grid.get(c).copied().flatten()
    }

    /// Neighbouring regions in link order (`Region.Neighbors`); a region
    /// can appear more than once.
    pub fn neighbors(&self, id: RegionId) -> impl Iterator<Item = RegionId> + '_ {
        self.regions[id].links.iter().flat_map(move |&l| {
            self.links[l]
                .regions
                .into_iter()
                .flatten()
                .filter(move |&r| r != id)
        })
    }

    /// Breadth-first traversal over passable regions
    /// (`RegionTraverser.BreadthFirstTraverse`): `processor` sees each
    /// region in order and may stop the search; `entry(from, to)` decides
    /// whether a neighbour is queued. Stops after `max_regions` regions.
    pub fn traverse(
        &self,
        root: RegionId,
        mut entry: impl FnMut(RegionId, RegionId) -> bool,
        mut processor: impl FnMut(RegionId) -> bool,
        max_regions: usize,
    ) {
        if !self.regions[root].kind.passable() {
            return;
        }
        let mut closed = vec![false; self.regions.len()];
        let mut open = VecDeque::from([root]);
        closed[root] = true;
        let mut processed = 0;
        while let Some(r) = open.pop_front() {
            if processor(r) {
                return;
            }
            if self.regions[r].kind != RegionType::Portal {
                processed += 1;
            }
            if processed >= max_regions {
                return;
            }
            for &l in &self.regions[r].links {
                for n in self.links[l].regions.into_iter().flatten() {
                    if !closed[n] && self.regions[n].kind.passable() && entry(r, n) {
                        closed[n] = true;
                        open.push_back(n);
                    }
                }
            }
        }
    }

    /// The region's cells in map order within its bounds (`Region.Cells`).
    pub fn cells(&self, id: RegionId) -> impl Iterator<Item = Cell> + '_ {
        let e = self.regions[id].extents;
        (e.min_z..=e.max_z)
            .flat_map(move |z| (e.min_x..=e.max_x).map(move |x| Cell::new(x, z)))
            .filter(move |&c| self.grid[c] == Some(id))
    }

    /// A random cell of the region (`Region.RandomCell`): random cells of
    /// its bounds until one belongs to it (at most 1000), else its first.
    pub fn random_cell(&self, id: RegionId, rng: &mut Rand) -> Cell {
        let e = self.regions[id].extents;
        for _ in 0..1000 {
            let c = e.random_cell(rng);
            if self.grid[c] == Some(id) {
                return c;
            }
        }
        self.cells(id).next().expect("regions are not empty")
    }

    /// `CellFinder.TryFindRandomCellInRegion`: ten random cells, then every
    /// cell in shuffled order.
    pub fn try_find_random_cell_in_region(
        &self,
        id: RegionId,
        mut validator: impl FnMut(Cell, &mut Rand) -> bool,
        rng: &mut Rand,
    ) -> Option<Cell> {
        for _ in 0..10 {
            let c = self.random_cell(id, rng);
            if validator(c, rng) {
                return Some(c);
            }
        }
        let mut cells: Vec<Cell> = self.cells(id).collect();
        rng.shuffle(&mut cells);
        let found = cells.into_iter().find(|&c| validator(c, rng));
        if found.is_none() {
            self.random_cell(id, rng);
        }
        found
    }
}

/// `GenCollection.RandomElementByWeight` over a list: no draw for a single
/// positively weighted element; otherwise one draw, choosing the first
/// element whose running total reaches `Value × total`.
pub fn random_element_by_weight(weights: &[f32], rng: &mut Rand) -> Option<usize> {
    let total: f32 = weights.iter().map(|w| w.max(0.0)).sum();
    if weights.len() == 1 && total > 0.0 {
        return Some(0);
    }
    if total <= 0.0 {
        return None;
    }
    let target = rng.value() * total;
    let mut sum = 0.0;
    for (i, &w) in weights.iter().enumerate() {
        if w > 0.0 {
            sum += w;
            if sum >= target {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    fn open_grid(w: i32, h: i32) -> (Map, GameDefs, PathGrid) {
        let (db, _) = rimworld_defs::load_documents(
            "t",
            &[(
                "t.xml",
                "<Defs><TerrainDef><defName>Soil</defName></TerrainDef></Defs>",
            )],
            &rimworld_defs::xml::ActivePackages::default(),
        );
        let defs = GameDefs::from_database(db).0;
        let map = Map::new(GridSize::new(w, h), defs.terrain.id("Soil").unwrap());
        let grid = PathGrid::new(Grid::new(GridSize::new(w, h), Some(0)));
        (map, defs, grid)
    }

    #[test]
    fn open_map_is_cut_into_blocks() {
        let (map, defs, grid) = open_grid(30, 20);
        let r = Regions::build(&map, &defs, &grid);
        // 3 × 2 blocks: 12, 12, 6 wide; 12, 8 high.
        assert_eq!(r.regions().len(), 6);
        assert_eq!(r.region(0).cell_count, 144);
        assert_eq!(r.region(2).cell_count, 6 * 12);
        assert_eq!(r.region(5).cell_count, 6 * 8);
        // Region 0's neighbours: east (1) then north (3), by link order.
        let n: Vec<_> = r.neighbors(0).collect();
        assert_eq!(n, vec![3, 1]);
        assert_eq!(r.region_at(Cell::new(13, 13)), Some(4));
    }

    #[test]
    fn walls_split_blocks_and_links_join_both_sides() {
        let (map, defs, _) = open_grid(12, 12);
        // A wall across z = 5 except a gap at x = 11.
        let mut costs = Grid::new(GridSize::new(12, 12), Some(0));
        for x in 0..11 {
            costs[Cell::new(x, 5)] = None;
        }
        let grid = PathGrid::new(costs);
        let r = Regions::build(&map, &defs, &grid);
        let normal: Vec<_> = r
            .regions()
            .iter()
            .filter(|g| g.kind == RegionType::Normal)
            .collect();
        assert_eq!(normal.len(), 1, "connected through the gap");
        let walls = r
            .regions()
            .iter()
            .filter(|g| g.kind == RegionType::ImpassableFreeAir);
        assert_eq!(walls.count(), 1);
    }

    #[test]
    fn traversal_counts_regions() {
        let (map, defs, grid) = open_grid(36, 12);
        let r = Regions::build(&map, &defs, &grid);
        let mut seen = Vec::new();
        r.traverse(
            0,
            |_, _| true,
            |id| {
                seen.push(id);
                false
            },
            2,
        );
        assert_eq!(seen, vec![0, 1]);
    }

    #[test]
    fn weighted_choice_draw_counts() {
        let mut rng = Rand::new(12345);
        assert_eq!(random_element_by_weight(&[5.0], &mut rng), Some(0));
        assert_eq!(rng.state().1, 0, "single element: no draw");
        assert_eq!(random_element_by_weight(&[0.0, 0.0], &mut rng), None);
        assert_eq!(rng.state().1, 0);
        // Seed 12345, weights 1, 2, 3: the game picks the third (research §17).
        assert_eq!(
            random_element_by_weight(&[1.0, 2.0, 3.0], &mut rng),
            Some(2)
        );
        assert_eq!(rng.state().1, 1);
    }
}
