//! Footprints of things larger than one cell (`GenAdj`): the occupied
//! rectangle for a centre, rotation and size, the 8-way adjacent cells in
//! the game's order, and bed sleeping slots (`BedUtility`).

use crate::grid::Cell;
use crate::job::Rot4;
use crate::region::Rect;

/// `Rot4.AsInt`.
pub fn rot_index(r: Rot4) -> i32 {
    match r {
        Rot4::North => 0,
        Rot4::East => 1,
        Rot4::South => 2,
        Rot4::West => 3,
    }
}

/// `IntVec3.RotatedBy`: an (x, z) offset for a north-facing thing, turned
/// to `rot` (clockwise).
pub fn rotate_offset((x, z): (i32, i32), rot: Rot4) -> (i32, i32) {
    match rot {
        Rot4::North => (x, z),
        Rot4::East => (z, -x),
        Rot4::South => (-x, -z),
        Rot4::West => (-z, x),
    }
}

/// Where a thing of `size` (x, z) at `center` with rotation `rot` sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Footprint {
    pub center: Cell,
    pub rot: Rot4,
    pub size: (i32, i32),
}

impl Footprint {
    pub fn single(center: Cell) -> Self {
        Self {
            center,
            rot: Rot4::North,
            size: (1, 1),
        }
    }

    /// `GenAdj.AdjustForRotation` (reference North): horizontal rotations
    /// swap the size; even sizes shift the centre by the rotation's offset.
    fn adjusted(&self) -> (Cell, (i32, i32)) {
        let (mut c, mut size) = (self.center, self.size);
        if size == (1, 1) {
            return (c, size);
        }
        if matches!(self.rot, Rot4::East | Rot4::West) {
            size = (size.1, size.0);
        }
        let (dx, dz) = match self.rot {
            Rot4::North => (0, 0),
            Rot4::East => (0, -1),
            Rot4::South => (-1, -1),
            Rot4::West => (-1, 0),
        };
        if size.0 % 2 == 0 {
            c.x += dx;
        }
        if size.1 % 2 == 0 {
            c.z += dz;
        }
        (c, size)
    }

    /// `GenAdj.OccupiedRect`.
    pub fn rect(&self) -> Rect {
        let (c, (sx, sz)) = self.adjusted();
        let min_x = c.x - (sx - 1) / 2;
        let min_z = c.z - (sz - 1) / 2;
        Rect {
            min_x,
            max_x: min_x + sx - 1,
            min_z,
            max_z: min_z + sz - 1,
        }
    }

    pub fn contains(&self, c: Cell) -> bool {
        self.rect().contains(c)
    }

    /// Occupied cells in `CellRect` order (rows from the south, west to east).
    pub fn cells(&self) -> impl Iterator<Item = Cell> {
        let r = self.rect();
        (r.min_z..=r.max_z).flat_map(move |z| (r.min_x..=r.max_x).map(move |x| Cell::new(x, z)))
    }

    /// `GenAdj.CellsAdjacent8Way`: the ring around the rectangle, clockwise
    /// from the south-west corner.
    pub fn adjacent_8_way(&self) -> Vec<Cell> {
        let r = self.rect();
        let (min_x, max_x, min_z, max_z) = (r.min_x - 1, r.max_x + 1, r.min_z - 1, r.max_z + 1);
        let mut out = Vec::new();
        let mut cur = Cell::new(min_x - 1, min_z);
        loop {
            cur.x += 1;
            out.push(cur);
            if cur.x >= max_x {
                break;
            }
        }
        loop {
            cur.z += 1;
            out.push(cur);
            if cur.z >= max_z {
                break;
            }
        }
        loop {
            cur.x -= 1;
            out.push(cur);
            if cur.x <= min_x {
                break;
            }
        }
        loop {
            cur.z -= 1;
            out.push(cur);
            if cur.z <= min_z + 1 {
                break;
            }
        }
        out
    }

    /// `GenAdj.CellsAdjacentCardinal`: the cells sharing an edge with the
    /// rectangle — the south side west to east, the east side upwards, the
    /// north side east to west, the west side downwards.
    pub fn adjacent_cardinal(&self) -> Vec<Cell> {
        let r = self.rect();
        let (min_x, max_x, min_z, max_z) = (r.min_x - 1, r.max_x + 1, r.min_z - 1, r.max_z + 1);
        let mut out = Vec::new();
        for x in min_x + 1..max_x {
            out.push(Cell::new(x, min_z));
        }
        for z in min_z + 1..max_z {
            out.push(Cell::new(max_x, z));
        }
        for x in (min_x + 1..max_x).rev() {
            out.push(Cell::new(x, max_z));
        }
        for z in (min_z + 1..max_z).rev() {
            out.push(Cell::new(min_x, z));
        }
        out
    }

    /// Chebyshev distance from `c` to the rectangle (0 inside).
    pub fn distance(&self, c: Cell) -> i32 {
        let r = self.rect();
        let dx = (r.min_x - c.x).max(c.x - r.max_x).max(0);
        let dz = (r.min_z - c.z).max(c.z - r.max_z).max(0);
        dx.max(dz)
    }

    /// The nearest occupied cell to `c`.
    pub fn nearest_cell(&self, c: Cell) -> Cell {
        let r = self.rect();
        Cell::new(c.x.clamp(r.min_x, r.max_x), c.z.clamp(r.min_z, r.max_z))
    }

    /// `BedUtility.GetSleepingSlotPos`: the head cell of slot `index`.
    pub fn sleeping_slot(&self, index: i32) -> Cell {
        let r = self.rect();
        match self.rot {
            Rot4::South => Cell::new(r.min_x + index, r.max_z),
            Rot4::North => Cell::new(r.max_x - index, r.min_z),
            Rot4::West => Cell::new(r.max_x, r.max_z - index),
            Rot4::East => Cell::new(r.min_x, r.min_z + index),
        }
    }

    /// `BedUtility.GetSleepingSlotsCount`.
    pub fn sleeping_slots(&self) -> i32 {
        self.size.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_cell_ring_matches_the_game_order() {
        let a = Footprint::single(Cell::new(5, 5)).adjacent_8_way();
        let expect = [
            (4, 4),
            (5, 4),
            (6, 4),
            (6, 5),
            (6, 6),
            (5, 6),
            (4, 6),
            (4, 5),
        ];
        assert_eq!(a.len(), 8);
        for (c, (x, z)) in a.iter().zip(expect) {
            assert_eq!(*c, Cell::new(x, z));
        }
    }

    #[test]
    fn bed_footprints_by_rotation() {
        let at = Cell::new(10, 10);
        let fp = |rot| Footprint {
            center: at,
            rot,
            size: (1, 2),
        };
        // North: the centre and the cell north of it; head slot = centre.
        let n = fp(Rot4::North);
        assert_eq!(n.cells().collect::<Vec<_>>(), vec![at, Cell::new(10, 11)]);
        assert_eq!(n.sleeping_slot(0), at);
        // South: the cell south of the centre and the centre.
        let s = fp(Rot4::South);
        assert_eq!(s.cells().collect::<Vec<_>>(), vec![Cell::new(10, 9), at]);
        assert_eq!(s.sleeping_slot(0), at);
        // East and West lie along x.
        let e = fp(Rot4::East);
        assert_eq!(e.cells().collect::<Vec<_>>(), vec![at, Cell::new(11, 10)]);
        assert_eq!(e.sleeping_slot(0), at);
        let w = fp(Rot4::West);
        assert_eq!(w.cells().collect::<Vec<_>>(), vec![Cell::new(9, 10), at]);
        assert_eq!(w.sleeping_slot(0), at);
        // A 1x2 thing has a 10-cell ring.
        assert_eq!(n.adjacent_8_way().len(), 10);
        assert_eq!(n.adjacent_8_way()[0], Cell::new(9, 9));
    }
}
