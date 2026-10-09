//! The game's camera-dependent update rate (docs/research.md §14):
//! `GenTicks.GetCameraUpdateRate`, `CameraDriver.CurrentZoom`,
//! `CameraDriver.CurrentViewRect`, `CameraDriver.InViewOf`.
//!
//! The front-end describes its camera in the game's terms — `root_size`
//! (orthographic size: half the visible height, in cells), the screen aspect
//! ratio and the camera centre in cell coordinates — and the simulation
//! derives each pawn's interval-logic update rate.

use crate::grid::{Cell, GridSize};

/// Default camera size range (`CameraMapConfig.sizeRange`, non–Steam Deck).
pub const SIZE_RANGE_MIN: f32 = 11.0;
pub const SIZE_RANGE_MAX: f32 = 60.0;
/// Update rate for pawns not in view (and the maximum rate).
pub const OFFSCREEN_UPDATE_RATE: u32 = 15;

/// `CameraZoomRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZoomRange {
    Closest = 0,
    Close = 1,
    Middle = 2,
    Far = 3,
    Furthest = 4,
}

impl ZoomRange {
    /// Zoom range for a camera root size, with the default size range.
    pub fn from_root_size(root_size: f32) -> Self {
        if root_size < SIZE_RANGE_MIN + 1.0 {
            Self::Closest
        } else if root_size < SIZE_RANGE_MAX * 0.23 {
            Self::Close
        } else if root_size < SIZE_RANGE_MAX * 0.7 {
            Self::Middle
        } else if root_size < SIZE_RANGE_MAX * 0.95 {
            Self::Far
        } else {
            Self::Furthest
        }
    }
}

/// A map camera described in the game's terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    /// Camera centre in cell coordinates (cell (x, z) spans [x, x+1)).
    pub centre_x: f32,
    pub centre_z: f32,
    /// Orthographic size: half the visible height, in cells.
    pub root_size: f32,
    /// Screen width / height.
    pub aspect: f32,
}

impl CameraView {
    /// The game's view rectangle, inclusive cell bounds (minX, minZ, maxX,
    /// maxZ), before `InViewOf` expands it by one cell.
    pub fn view_rect(&self) -> (i32, i32, i32, i32) {
        let half_w = self.root_size * self.aspect;
        (
            (self.centre_x - half_w - 1.0).floor() as i32,
            (self.centre_z - self.root_size - 1.0).floor() as i32,
            (self.centre_x + half_w).ceil() as i32,
            (self.centre_z + self.root_size).ceil() as i32,
        )
    }

    /// `InViewOf` for a 1×1 thing at `cell`: overlaps the view rectangle
    /// expanded by one cell and clipped to the map.
    pub fn in_view(&self, cell: Cell, map: GridSize) -> bool {
        let (min_x, min_z, max_x, max_z) = self.view_rect();
        let (min_x, min_z) = ((min_x - 1).max(0), (min_z - 1).max(0));
        let (max_x, max_z) = (
            (max_x + 1).min(map.width - 1),
            (max_z + 1).min(map.height - 1),
        );
        cell.x >= min_x && cell.x <= max_x && cell.z >= min_z && cell.z <= max_z
    }

    /// The interval-logic update rate for a humanlike pawn at `cell`:
    /// zoom range + 1 when in view, else 15. (Animals always use 15.)
    pub fn update_rate(&self, cell: Cell, map: GridSize) -> u32 {
        if self.in_view(cell, map) {
            ZoomRange::from_root_size(self.root_size) as u32 + 1
        } else {
            OFFSCREEN_UPDATE_RATE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_thresholds() {
        // Boundaries: 12, 13.8, 42, 57 (from sizeRange 11-60).
        assert_eq!(ZoomRange::from_root_size(11.0), ZoomRange::Closest);
        assert_eq!(ZoomRange::from_root_size(11.99), ZoomRange::Closest);
        assert_eq!(ZoomRange::from_root_size(12.0), ZoomRange::Close);
        assert_eq!(ZoomRange::from_root_size(13.79), ZoomRange::Close);
        assert_eq!(ZoomRange::from_root_size(13.8), ZoomRange::Middle);
        assert_eq!(ZoomRange::from_root_size(24.0), ZoomRange::Middle); // StartingSize
        assert_eq!(ZoomRange::from_root_size(41.99), ZoomRange::Middle);
        assert_eq!(ZoomRange::from_root_size(42.0), ZoomRange::Far);
        assert_eq!(ZoomRange::from_root_size(57.0), ZoomRange::Furthest);
    }

    #[test]
    fn view_rect_and_rates() {
        let cam = CameraView {
            centre_x: 50.5,
            centre_z: 50.5,
            root_size: 24.0,
            aspect: 16.0 / 9.0,
        };
        let map = GridSize::new(250, 250);
        // half width 42.67: x from floor(50.5-42.67-1)=6 to ceil(93.17)=94.
        assert_eq!(cam.view_rect(), (6, 25, 94, 75));
        assert!(cam.in_view(Cell::new(5, 50), map), "one-cell margin");
        assert!(!cam.in_view(Cell::new(4, 50), map));
        assert!(cam.in_view(Cell::new(50, 76), map));
        assert!(!cam.in_view(Cell::new(50, 77), map));
        assert_eq!(cam.update_rate(Cell::new(50, 50), map), 3); // Middle
        assert_eq!(cam.update_rate(Cell::new(200, 50), map), 15);
        let close = CameraView {
            root_size: 11.0,
            ..cam
        };
        assert_eq!(close.update_rate(Cell::new(50, 50), map), 1);
    }
}
