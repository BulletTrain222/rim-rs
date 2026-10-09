//! Light from lamps (docs/research.md §32): each lit glower floods light
//! outward by path distance around light blockers (walls, doors), fading
//! with distance; the lights' colors add up per cell, and the ground glow
//! is read from the brightest channel (`GlowGrid`, `ComputeGlowGridsJob`).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rimworld_defs::GlowerProperties;
use serde::{Deserialize, Serialize};

use crate::grid::{Cell, Grid, GridSize};

/// A cell's summed light: color channels and whether it is overlit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LightCell {
    pub rgb: [u8; 3],
    pub overlit: bool,
}

/// `ComputeGlowGridsJob.Directions`: cardinals, then diagonals.
const DIRECTIONS: [Cell; 8] = [
    Cell::new(0, -1),
    Cell::new(1, 0),
    Cell::new(0, 1),
    Cell::new(-1, 0),
    Cell::new(1, -1),
    Cell::new(1, 1),
    Cell::new(-1, 1),
    Cell::new(-1, -1),
];

/// `ColorInt.ProjectToColor32Fast`: channels over 255 scale down together;
/// alpha saturates.
fn project(c: [i32; 4]) -> ([u8; 3], u8) {
    let a = c[3].clamp(0, 255) as u8;
    let max = c[0].max(c[1]).max(c[2]);
    let rgb = if max > 255 {
        [
            (c[0] * 255 / max) as u8,
            (c[1] * 255 / max) as u8,
            (c[2] * 255 / max) as u8,
        ]
    } else {
        [c[0] as u8, c[1] as u8, c[2] as u8]
    };
    (rgb, a)
}

/// One light's colors over the cells it reaches: (cell, color projected,
/// distance in whole cells as alpha).
fn flood(
    size: GridSize,
    blocks: &dyn Fn(Cell) -> bool,
    at: Cell,
    glower: &GlowerProperties,
) -> Vec<(Cell, [u8; 3], u8)> {
    let radius = glower.radius;
    let limit = (radius * 100.0).round_ties_even() as i32;
    let reach = radius.ceil() as i32 + 1;
    let side = reach * 2 + 1;
    let idx = |d: Cell| -> Option<usize> {
        let (x, z) = (d.x + reach, d.z + reach);
        (x >= 0 && z >= 0 && x < side && z < side).then(|| (z * side + x) as usize)
    };
    // 0 unvisited, 1 open, 2 finalized.
    let mut status = vec![0u8; (side * side) as usize];
    let mut dist = vec![i32::MAX; (side * side) as usize];
    let mut blockers = [false; 8];
    let mut queue = BinaryHeap::new();
    dist[idx(Cell::new(0, 0)).unwrap()] = 100;
    queue.push(Reverse((100, 0i32, 0i32)));
    let mut out = Vec::new();
    while let Some(Reverse((d, x, z))) = queue.pop() {
        let delta = Cell::new(x, z);
        let i = idx(delta).unwrap();
        if status[i] == 2 || d > dist[i] {
            continue;
        }
        status[i] = 2;
        // `SetGlowFromDist`: Lerp(1 − d/r, 1/d², 0.4) of the light color.
        let cells = dist[i] as f32 / 100.0;
        if cells <= radius {
            let b = 1.0 / (cells * cells);
            let a = 1.0 + (-1.0 / radius) * cells;
            let f = a + (b - a) * 0.4;
            let c = [
                (glower.color[0] as f32 * f) as i32,
                (glower.color[1] as f32 * f) as i32,
                (glower.color[2] as f32 * f) as i32,
            ];
            if c.iter().any(|&v| v > 0) {
                let (rgb, a) = project([c[0].max(0), c[1].max(0), c[2].max(0), cells as i32]);
                out.push((at + delta, rgb, a));
            }
        }
        for (k, dir) in DIRECTIONS.iter().enumerate() {
            let nd = delta + *dir;
            let Some(j) = idx(nd) else { continue };
            if status[j] == 2 {
                continue;
            }
            let c = at + nd;
            if !size.contains(c) {
                continue;
            }
            let blocked = blocks(c);
            blockers[k] = blocked;
            if blocked {
                continue;
            }
            let nd_dist = dist[i] + if k < 4 { 100 } else { 141 };
            if nd_dist > limit {
                continue;
            }
            // No squeezing diagonally between two blockers.
            let corner = match k {
                4 => blockers[0] && blockers[1],
                5 => blockers[1] && blockers[2],
                6 => blockers[2] && blockers[3],
                7 => blockers[0] && blockers[3],
                _ => false,
            };
            if corner {
                continue;
            }
            if nd_dist < dist[j] {
                dist[j] = nd_dist;
                status[j] = 1;
                queue.push(Reverse((nd_dist, nd.x, nd.z)));
            }
        }
    }
    out
}

/// Sums the lights (`CombineColorsJob`): colors add, and a cell within a
/// light's overlight radius is overlit.
pub fn light_grid(
    size: GridSize,
    blocks: &dyn Fn(Cell) -> bool,
    lights: &[(Cell, &GlowerProperties)],
) -> Grid<LightCell> {
    let mut sums: Grid<[i32; 4]> = Grid::new(size, [0; 4]);
    for &(at, glower) in lights {
        for (c, rgb, a) in flood(size, blocks, at, glower) {
            let s = &mut sums[c];
            s[0] += rgb[0] as i32;
            s[1] += rgb[1] as i32;
            s[2] += rgb[2] as i32;
            if (a as f32) < glower.overlight_radius {
                s[3] = 1;
            }
            // Each addition is projected back to 8-bit color.
            let (p, _) = project(*s);
            s[0] = p[0] as i32;
            s[1] = p[1] as i32;
            s[2] = p[2] as i32;
        }
    }
    Grid::from_fn(size, |c| {
        let s = sums[c];
        LightCell {
            rgb: [s[0] as u8, s[1] as u8, s[2] as u8],
            overlit: s[3] == 1,
        }
    })
}

/// The lamps' part of `GroundGlowAt`: 1 if overlit, else the brightest
/// channel / 255 × 3.6, at most 0.5.
pub fn lamp_glow(c: LightCell) -> f32 {
    if c.overlit {
        return 1.0;
    }
    let max = c.rgb[0].max(c.rgb[1]).max(c.rgb[2]) as f32;
    (max / 255.0 * 3.6).min(0.5)
}
