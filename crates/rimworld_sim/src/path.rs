//! Grid pathfinding and movement costs, following the game's rules as
//! documented in docs/research.md §13:
//!
//! - Ticks per move = 60 / MoveSpeed (diagonal × 1.41421, clamped 1–450).
//! - Searching uses those ticks rounded to integers plus the cell cost;
//!   diagonals may not pass impassable cells or full-fill buildings; the
//!   heuristic is octile distance × 13 × heuristic strength.
//! - Walking a cell costs ticks per move + cell cost (capped at 450), scaled
//!   by the job's locomotion urgency.

use crate::grid::{Cell, Grid, GridSize};

/// Path grid cost meaning "impassable".
pub const IMPASSABLE: u32 = 10_000;
const MAX_MOVE_TICKS: f32 = 450.0;
/// The game's diagonal factor (deliberately its 1.41421, not exact sqrt 2).
#[allow(clippy::approx_constant)]
const DIAGONAL_FACTOR: f32 = 1.41421;
/// Heuristic cost per cell of octile distance.
const HEURISTIC_CELL_COST: f32 = 13.0;
/// Heuristic strength for colonists.
pub const COLONIST_HEURISTIC_STRENGTH: f32 = 1.0;

/// Per-cell entry costs (`IMPASSABLE` = blocked) and which cells hold a
/// full-fill building (these block diagonal moves past them).
#[derive(Debug, Clone)]
pub struct PathGrid {
    costs: Grid<u32>,
    full_buildings: Grid<bool>,
}

impl PathGrid {
    /// From optional costs (`None` = impassable), with no buildings.
    pub fn new(costs: Grid<Option<u32>>) -> Self {
        let size = costs.size();
        Self {
            costs: Grid::from_fn(size, |c| costs[c].map_or(IMPASSABLE, |v| v.min(IMPASSABLE))),
            full_buildings: Grid::new(size, false),
        }
    }

    pub fn with_buildings(costs: Grid<u32>, full_buildings: Grid<bool>) -> Self {
        assert_eq!(costs.size(), full_buildings.size());
        Self {
            costs,
            full_buildings,
        }
    }

    pub fn size(&self) -> GridSize {
        self.costs.size()
    }

    fn raw_cost(&self, c: Cell) -> u32 {
        self.costs.get(c).copied().unwrap_or(IMPASSABLE)
    }

    pub fn walkable(&self, c: Cell) -> bool {
        self.raw_cost(c) < IMPASSABLE
    }

    /// Path cost of a walkable cell.
    pub fn cost(&self, c: Cell) -> Option<u32> {
        let v = self.raw_cost(c);
        (v < IMPASSABLE).then_some(v)
    }

    /// Whether a single step `from → from + dir` is allowed: the target must
    /// be walkable, and a diagonal step may not pass a side cell that is
    /// impassable or holds a full-fill building.
    pub fn can_step(&self, from: Cell, dir: Cell) -> bool {
        let to = from + dir;
        if !self.walkable(to) {
            return false;
        }
        if dir.is_diagonal_step() {
            let side_ok =
                |c: Cell| self.walkable(c) && !self.full_buildings.get(c).copied().unwrap_or(false);
            side_ok(from + Cell::new(dir.x, 0)) && side_ok(from + Cell::new(0, dir.z))
        } else {
            true
        }
    }
}

/// How hurriedly a job moves the pawn (`LocomotionUrgency`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LocomotionUrgency {
    Amble,
    Walk,
    #[default]
    Jog,
    Sprint,
}

/// A pawn's ticks per move.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MoveCosts {
    pub cardinal: f32,
    pub diagonal: f32,
}

impl MoveCosts {
    /// From the `MoveSpeed` stat (cells per second at 60 ticks/second).
    // DIFFERENTIAL VERIFIED: equals the game's TicksPerMoveCardinal /
    // TicksPerMoveDiagonal bit for bit for six MoveSpeed values.
    // COMPATIBILITY TODO: currently approximate — the weather move-speed
    // multiplier for unroofed cells is assumed 1.0 (clear weather); crawling
    // and restraints are not applied (carrying a pawn is, by the step code).
    pub fn from_move_speed(move_speed: f32) -> Self {
        let per_tick = move_speed / 60.0;
        if per_tick == 0.0 {
            return Self {
                cardinal: MAX_MOVE_TICKS,
                diagonal: MAX_MOVE_TICKS,
            };
        }
        let cardinal = 1.0 / per_tick;
        Self {
            cardinal: cardinal.clamp(1.0, MAX_MOVE_TICKS),
            diagonal: (cardinal * DIAGONAL_FACTOR).clamp(1.0, MAX_MOVE_TICKS),
        }
    }

    /// Integer ticks used by the pathfinder (rounded like Unity's
    /// `Mathf.RoundToInt`, i.e. half to even).
    pub fn search_cardinal(self) -> u32 {
        round_half_even(self.cardinal)
    }

    pub fn search_diagonal(self) -> u32 {
        round_half_even(self.diagonal)
    }

    /// Ticks needed to walk into a cell with path cost `cell_cost`.
    // DIFFERENTIAL VERIFIED: base ticks (cardinal/diagonal), terrain path
    // cost, Jog and Walk (x2, min 50) against 21 original-game traces
    // (crates/rimworld_sim/tests/differential.rs).
    // COMPATIBILITY TODO: currently approximate — edifice walk costs and the
    // pawn kind's terrain-tag speed factors are not applied (no such things
    // exist on the map yet).
    pub fn step_cost(self, dir: Cell, cell_cost: u32, urgency: LocomotionUrgency) -> f32 {
        let base = if dir.is_diagonal_step() {
            self.diagonal
        } else {
            self.cardinal
        };
        let mut cost = (base + cell_cost as f32).min(MAX_MOVE_TICKS);
        cost = match urgency {
            LocomotionUrgency::Amble => (cost * 3.0).max(60.0),
            LocomotionUrgency::Walk => (cost * 2.0).max(50.0),
            LocomotionUrgency::Jog => cost,
            LocomotionUrgency::Sprint => round_half_even(cost * 0.75) as f32,
        };
        cost.max(1.0)
    }
}

fn round_half_even(v: f32) -> u32 {
    v.round_ties_even().max(0.0) as u32
}

/// Ticks paid per game tick while walking a cell costing `total`.
pub fn cost_paid_per_tick(total: f32) -> f32 {
    1.0f32.max(total / MAX_MOVE_TICKS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    /// Cells to walk through, excluding the start, ending with the goal.
    pub cells: Vec<Cell>,
    /// Total search cost (integer ticks as the pathfinder counts them).
    pub cost: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    StartOutOfBounds,
    GoalOutOfBounds,
    GoalImpassable,
    NoPath,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathError::StartOutOfBounds => "start is outside the map",
            PathError::GoalOutOfBounds => "destination is outside the map",
            PathError::GoalImpassable => "destination is impassable",
            PathError::NoPath => "no path to destination",
        })
    }
}

impl std::error::Error for PathError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    None,
    Open,
    Closed,
}

#[derive(Clone, Copy)]
struct Node {
    g: u32,
    f: f32,
    h: f32,
    parent: usize,
    status: Status,
}

/// The frontier: a 4-ary min-heap with the game's sift rules
/// (`NativePriorityQueue`), so that cells of equal `f` come out in the
/// game's order. A new entry rises while strictly less than its parent; a
/// sinking entry follows the first smallest child while strictly greater.
#[derive(Default)]
struct Frontier {
    items: Vec<(usize, f32)>,
}

impl Frontier {
    fn push(&mut self, index: usize, f: f32) {
        let mut i = self.items.len();
        self.items.push((index, f));
        while i > 0 {
            let parent = (i - 1) >> 2;
            if f >= self.items[parent].1 {
                break;
            }
            self.items[i] = self.items[parent];
            i = parent;
        }
        self.items[i] = (index, f);
    }

    fn pop(&mut self) -> Option<(usize, f32)> {
        let top = *self.items.first()?;
        let last = self.items.pop().expect("non-empty");
        let n = self.items.len();
        if n > 0 {
            let mut i = 0;
            loop {
                let first = (i << 2) + 1;
                if first >= n {
                    break;
                }
                let mut best = first;
                for c in first + 1..(first + 4).min(n) {
                    if self.items[c].1 < self.items[best].1 {
                        best = c;
                    }
                }
                if last.1 <= self.items[best].1 {
                    break;
                }
                self.items[i] = self.items[best];
                i = best;
            }
            self.items[i] = last;
        }
        Some(top)
    }
}

/// The search's neighbour order (`CellConnectionExtensions.OffsetFromBitIndex`):
/// N, S, E, W, NE, SE, SW, NW.
const SEARCH_NEIGHBORS: [Cell; 8] = [
    Cell::new(0, 1),
    Cell::new(0, -1),
    Cell::new(1, 0),
    Cell::new(-1, 0),
    Cell::new(1, 1),
    Cell::new(1, -1),
    Cell::new(-1, -1),
    Cell::new(-1, 1),
];

/// Finds a path from `start` to `goal` with the game's search rules.
pub fn find_path(
    grid: &PathGrid,
    start: Cell,
    goal: Cell,
    costs: MoveCosts,
    heuristic_strength: f32,
) -> Result<Path, PathError> {
    let size = grid.size();
    if !size.contains(start) {
        return Err(PathError::StartOutOfBounds);
    }
    if !size.contains(goal) {
        return Err(PathError::GoalOutOfBounds);
    }
    if !grid.walkable(goal) {
        return Err(PathError::GoalImpassable);
    }
    search(grid, start, goal, |c| c == goal, costs, heuristic_strength)
}

/// `PathEndMode.Touch` (`PathFinder.MakeDestination`): the search ends at
/// the first cell taken from the frontier inside the target's rectangle
/// grown by one, except corners from which it may not be touched
/// (`allowed`); the heuristic still aims at the target cell.
pub fn find_path_touch(
    grid: &PathGrid,
    start: Cell,
    target: Cell,
    allowed: impl Fn(Cell) -> bool,
    costs: MoveCosts,
    heuristic_strength: f32,
) -> Result<Path, PathError> {
    let size = grid.size();
    if !size.contains(start) {
        return Err(PathError::StartOutOfBounds);
    }
    if !size.contains(target) {
        return Err(PathError::GoalOutOfBounds);
    }
    search(
        grid,
        start,
        target,
        |c| c.chebyshev(target) <= 1 && allowed(c),
        costs,
        heuristic_strength,
    )
}

/// The game's A* (`PathFinderJob.Execute`) from `start` until a cell
/// satisfying `is_dest` is taken from the frontier, with the heuristic
/// aimed at `goal`.
pub fn search(
    grid: &PathGrid,
    start: Cell,
    goal: Cell,
    is_dest: impl Fn(Cell) -> bool,
    costs: MoveCosts,
    heuristic_strength: f32,
) -> Result<Path, PathError> {
    let size = grid.size();
    if is_dest(start) {
        return Ok(Path {
            cells: Vec::new(),
            cost: 0,
        });
    }

    let card = costs.search_cardinal();
    let diag = costs.search_diagonal();
    // A closed node is reopened only for a clear improvement.
    let reopen_margin = (card as f32 * 0.8).ceil() as u32;
    let heuristic = |c: Cell| -> f32 {
        let dx = (goal.x - c.x).unsigned_abs() as f32;
        let dz = (goal.z - c.z).unsigned_abs() as f32;
        ((dx + dz) - 0.585_790_04 * dx.min(dz)) * HEURISTIC_CELL_COST * heuristic_strength
    };

    let n = size.area();
    let mut nodes = vec![
        Node {
            g: 0,
            f: 0.0,
            h: 0.0,
            parent: usize::MAX,
            status: Status::None,
        };
        n
    ];
    let mut frontier = Frontier::default();
    let si = size.index(start);
    nodes[si].status = Status::Open;
    frontier.push(si, 0.0);

    let mut expanded = 0;
    while let Some((ci, f)) = frontier.pop() {
        // Skip stale queue entries.
        if nodes[ci].status == Status::Closed || f != nodes[ci].f {
            continue;
        }
        if is_dest(size.cell(ci)) {
            let mut cells = Vec::new();
            let mut i = ci;
            while i != si {
                cells.push(size.cell(i));
                i = nodes[i].parent;
            }
            cells.reverse();
            return Ok(Path {
                cells,
                cost: nodes[ci].g,
            });
        }
        if expanded >= n {
            break;
        }
        let cur = size.cell(ci);
        for dir in SEARCH_NEIGHBORS {
            if !grid.can_step(cur, dir) {
                continue;
            }
            let next = cur + dir;
            let ni = size.index(next);
            let step = grid.raw_cost(next) + if dir.is_diagonal_step() { diag } else { card };
            let g = nodes[ci].g + step;
            match nodes[ni].status {
                Status::None => nodes[ni].h = heuristic(next),
                Status::Open if nodes[ni].g <= g => continue,
                Status::Closed if nodes[ni].g <= g + reopen_margin => continue,
                _ => {}
            }
            let f = (g as f32 + nodes[ni].h).max(0.0);
            nodes[ni].g = g;
            nodes[ni].f = f;
            nodes[ni].parent = ci;
            nodes[ni].status = Status::Open;
            frontier.push(ni, f);
        }
        expanded += 1;
        nodes[ci].status = Status::Closed;
    }
    Err(PathError::NoPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BinaryHeap;

    /// A pawn with exactly 10 / 14 ticks per move (search costs 10 / 14).
    const COSTS: MoveCosts = MoveCosts {
        cardinal: 10.0,
        diagonal: 14.0,
    };

    fn find(g: &PathGrid, s: Cell, t: Cell) -> Result<Path, PathError> {
        find_path(g, s, t, COSTS, COLONIST_HEURISTIC_STRENGTH)
    }

    /// `#` = wall, `~` = costly (cost 100), `B` = passable full-fill building,
    /// anything else open. First line is the top row (highest z).
    fn grid(rows: &[&str]) -> PathGrid {
        let h = rows.len() as i32;
        let w = rows[0].len() as i32;
        let size = GridSize::new(w, h);
        let ch = |c: Cell| rows[(h - 1 - c.z) as usize].as_bytes()[c.x as usize];
        PathGrid::with_buildings(
            Grid::from_fn(size, |c| match ch(c) {
                b'#' => IMPASSABLE,
                b'~' => 100,
                _ => 0,
            }),
            Grid::from_fn(size, |c| ch(c) == b'B'),
        )
    }

    fn assert_valid(g: &PathGrid, start: Cell, path: &Path) {
        let mut prev = start;
        for &c in &path.cells {
            let dir = Cell::new(c.x - prev.x, c.z - prev.z);
            assert!(dir.x.abs() <= 1 && dir.z.abs() <= 1 && dir != Cell::default());
            assert!(g.can_step(prev, dir), "illegal step {prev:?} -> {c:?}");
            prev = c;
        }
    }

    #[test]
    fn ticks_per_move_from_speed() {
        let c = MoveCosts::from_move_speed(4.6);
        assert!((c.cardinal - 60.0 / 4.6).abs() < 1e-4);
        assert!((c.diagonal - 60.0 / 4.6 * DIAGONAL_FACTOR).abs() < 1e-3);
        assert_eq!((c.search_cardinal(), c.search_diagonal()), (13, 18));
        assert_eq!(MoveCosts::from_move_speed(0.0).cardinal, 450.0);
        assert_eq!(MoveCosts::from_move_speed(1000.0).cardinal, 1.0);
    }

    #[test]
    fn step_cost_with_urgency() {
        let c = MoveCosts::from_move_speed(4.6);
        let card = Cell::new(1, 0);
        let jog = c.step_cost(card, 2, LocomotionUrgency::Jog);
        assert!((jog - (60.0 / 4.6 + 2.0)).abs() < 1e-4);
        // Walk doubles, with a 50-tick minimum.
        assert_eq!(c.step_cost(card, 2, LocomotionUrgency::Walk), 50.0);
        let walk_marsh = c.step_cost(card, 14, LocomotionUrgency::Walk);
        assert!((walk_marsh - (60.0 / 4.6 + 14.0) * 2.0).abs() < 1e-3);
        assert_eq!(c.step_cost(card, 0, LocomotionUrgency::Amble), 60.0);
        assert_eq!(c.step_cost(card, 2, LocomotionUrgency::Sprint), 11.0);
        // Capped at 450 before urgency.
        assert_eq!(c.step_cost(card, 1000, LocomotionUrgency::Jog), 450.0);
        assert_eq!(cost_paid_per_tick(900.0), 2.0);
        assert_eq!(cost_paid_per_tick(20.0), 1.0);
    }

    #[test]
    fn straight_and_diagonal() {
        let g = grid(&["....", "....", "....", "...."]);
        let p = find(&g, Cell::new(0, 0), Cell::new(3, 0)).unwrap();
        assert_eq!(p.cells.len(), 3);
        assert_eq!(p.cost, 30);
        let p = find(&g, Cell::new(0, 0), Cell::new(3, 3)).unwrap();
        assert_eq!(
            p.cells,
            vec![Cell::new(1, 1), Cell::new(2, 2), Cell::new(3, 3)]
        );
        assert_eq!(p.cost, 42);
    }

    #[test]
    fn routes_around_walls() {
        let g = grid(&[".....", ".###.", ".#...", ".#.#.", "...#."]);
        let start = Cell::new(2, 1);
        let goal = Cell::new(4, 4);
        let p = find(&g, start, goal).unwrap();
        assert_eq!(*p.cells.last().unwrap(), goal);
        assert_valid(&g, start, &p);
    }

    #[test]
    fn diagonals_blocked_by_walls_and_full_buildings() {
        let walls = grid(&["..", ".#"]);
        assert!(!walls.can_step(Cell::new(0, 1), Cell::new(1, -1)));
        // A passable full-fill building beside the move also blocks it.
        let building = grid(&["..", "B."]);
        assert!(building.walkable(Cell::new(0, 0)));
        assert!(!building.can_step(Cell::new(1, 0), Cell::new(-1, 1)));
        let p = find(&building, Cell::new(1, 0), Cell::new(0, 1)).unwrap();
        assert_eq!(p.cells, vec![Cell::new(1, 1), Cell::new(0, 1)]);
    }

    #[test]
    fn avoids_expensive_terrain_when_cheaper() {
        let g = grid(&[".....", "~~~~.", "....."]);
        let p = find(&g, Cell::new(0, 0), Cell::new(0, 2)).unwrap();
        assert!(!p.cells.contains(&Cell::new(0, 1)));
        assert_valid(&g, Cell::new(0, 0), &p);
    }

    #[test]
    fn errors() {
        let g = grid(&["..#", "###", "..."]);
        let s = Cell::new(0, 0);
        assert_eq!(find(&g, s, Cell::new(2, 2)), Err(PathError::GoalImpassable));
        assert_eq!(find(&g, s, Cell::new(0, 2)), Err(PathError::NoPath));
        assert_eq!(
            find(&g, s, Cell::new(9, 9)),
            Err(PathError::GoalOutOfBounds)
        );
        assert_eq!(
            find(&g, Cell::new(-1, 0), s),
            Err(PathError::StartOutOfBounds)
        );
        assert_eq!(find(&g, s, s).unwrap().cells.len(), 0);
    }

    #[test]
    fn colonist_paths_are_optimal_on_random_grids() {
        // The game's heuristic is not strictly admissible (its diagonal term,
        // 13 x 1.414 = 18.38, slightly exceeds the 18-tick diagonal edge), so
        // optimality is not guaranteed in general; on these grids the costs
        // still match a plain Dijkstra.
        let costs = MoveCosts::from_move_speed(4.6);
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..40 {
            let size = GridSize::new(14, 10);
            let g = PathGrid::new(Grid::from_fn(size, |_| match rnd() % 10 {
                0..=2 => None,
                3 => Some(30),
                _ => Some((rnd() % 4) as u32),
            }));
            let (start, goal) = (Cell::new(0, 0), Cell::new(13, 9));
            if !g.walkable(start) {
                continue;
            }
            let astar = find_path(&g, start, goal, costs, 1.0).map(|p| p.cost).ok();
            assert_eq!(astar, dijkstra(&g, start, goal, costs));
        }
    }

    fn dijkstra(g: &PathGrid, start: Cell, goal: Cell, costs: MoveCosts) -> Option<u32> {
        use std::cmp::Reverse;
        if !g.walkable(goal) {
            return None;
        }
        let size = g.size();
        let mut dist = vec![u32::MAX; size.area()];
        let mut heap = BinaryHeap::new();
        dist[size.index(start)] = 0;
        heap.push(Reverse((0u32, start)));
        while let Some(Reverse((d, c))) = heap.pop() {
            if c == goal {
                return Some(d);
            }
            if d > dist[size.index(c)] {
                continue;
            }
            for dir in Cell::NEIGHBORS_8 {
                if g.can_step(c, dir) {
                    let n = c + dir;
                    let base = if dir.is_diagonal_step() {
                        costs.search_diagonal()
                    } else {
                        costs.search_cardinal()
                    };
                    let nd = d + base + g.cost(n).unwrap();
                    if nd < dist[size.index(n)] {
                        dist[size.index(n)] = nd;
                        heap.push(Reverse((nd, n)));
                    }
                }
            }
        }
        None
    }
}
