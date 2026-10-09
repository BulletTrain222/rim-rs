//! Grid coordinates and dense per-cell storage.
//!
//! Like RimWorld, the map is a horizontal plane addressed by `(x, z)`;
//! `x` grows east, `z` grows north.

use std::ops::{Add, Index, IndexMut};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Cell {
    pub x: i32,
    pub z: i32,
}

impl Cell {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// The 8 neighbour offsets: 4 cardinal first, then 4 diagonal.
    pub const NEIGHBORS_8: [Cell; 8] = [
        Cell::new(0, 1),
        Cell::new(1, 0),
        Cell::new(0, -1),
        Cell::new(-1, 0),
        Cell::new(1, 1),
        Cell::new(1, -1),
        Cell::new(-1, -1),
        Cell::new(-1, 1),
    ];

    /// The 4 cardinal offsets (`GenAdj.CardinalDirections`: N, E, S, W).
    pub const NEIGHBORS_4: [Cell; 4] = [
        Cell::new(0, 1),
        Cell::new(1, 0),
        Cell::new(0, -1),
        Cell::new(-1, 0),
    ];

    pub fn is_diagonal_step(self) -> bool {
        self.x != 0 && self.z != 0
    }

    /// Chebyshev distance (number of 8-way steps ignoring obstacles).
    pub fn chebyshev(self, other: Cell) -> i32 {
        (self.x - other.x).abs().max((self.z - other.z).abs())
    }

    /// Squared straight-line distance (`LengthHorizontalSquared`).
    pub fn distance_squared(self, other: Cell) -> i32 {
        let (dx, dz) = (self.x - other.x, self.z - other.z);
        dx * dx + dz * dz
    }
}

impl Add for Cell {
    type Output = Cell;
    fn add(self, o: Cell) -> Cell {
        Cell::new(self.x + o.x, self.z + o.z)
    }
}

/// Map dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GridSize {
    pub width: i32,
    pub height: i32,
}

impl GridSize {
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
    pub fn contains(self, c: Cell) -> bool {
        c.x >= 0 && c.z >= 0 && c.x < self.width && c.z < self.height
    }
    pub fn area(self) -> usize {
        (self.width * self.height) as usize
    }
    /// Row-major index (`z * width + x`). Caller must ensure `contains(c)`.
    pub fn index(self, c: Cell) -> usize {
        debug_assert!(self.contains(c), "{c:?} outside {self:?}");
        (c.z * self.width + c.x) as usize
    }
    pub fn cell(self, index: usize) -> Cell {
        let i = index as i32;
        Cell::new(i % self.width, i / self.width)
    }
    pub fn cells(self) -> impl Iterator<Item = Cell> {
        (0..self.area()).map(move |i| self.cell(i))
    }
}

/// Dense per-cell storage.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Grid<T> {
    size: GridSize,
    data: Vec<T>,
}

impl<T: Clone> Grid<T> {
    pub fn new(size: GridSize, fill: T) -> Self {
        Self {
            size,
            data: vec![fill; size.area()],
        }
    }
}

impl<T> Grid<T> {
    pub fn from_fn(size: GridSize, mut f: impl FnMut(Cell) -> T) -> Self {
        Self {
            size,
            data: size.cells().map(&mut f).collect(),
        }
    }
    pub fn size(&self) -> GridSize {
        self.size
    }
    pub fn get(&self, c: Cell) -> Option<&T> {
        self.size
            .contains(c)
            .then(|| &self.data[self.size.index(c)])
    }
    pub fn iter(&self) -> impl Iterator<Item = (Cell, &T)> {
        self.data
            .iter()
            .enumerate()
            .map(|(i, v)| (self.size.cell(i), v))
    }
}

impl<T> Index<Cell> for Grid<T> {
    type Output = T;
    fn index(&self, c: Cell) -> &T {
        &self.data[self.size.index(c)]
    }
}

impl<T> IndexMut<Cell> for Grid<T> {
    fn index_mut(&mut self, c: Cell) -> &mut T {
        let i = self.size.index(c);
        &mut self.data[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrip() {
        let s = GridSize::new(7, 5);
        for c in s.cells() {
            assert_eq!(s.cell(s.index(c)), c);
        }
        assert_eq!(s.cells().count(), 35);
        assert!(!s.contains(Cell::new(7, 0)));
        assert!(!s.contains(Cell::new(0, -1)));
    }

    #[test]
    fn grid_access() {
        let mut g = Grid::new(GridSize::new(3, 3), 0u8);
        g[Cell::new(2, 1)] = 5;
        assert_eq!(g.get(Cell::new(2, 1)), Some(&5));
        assert_eq!(g.get(Cell::new(3, 1)), None);
        let h = Grid::from_fn(GridSize::new(2, 2), |c| c.x + 10 * c.z);
        assert_eq!(h[Cell::new(1, 1)], 11);
    }

    #[test]
    fn neighbors() {
        assert_eq!(
            Cell::NEIGHBORS_8
                .iter()
                .filter(|c| c.is_diagonal_step())
                .count(),
            4
        );
        assert_eq!(Cell::new(0, 0).chebyshev(Cell::new(3, -5)), 5);
    }
}
