//! Roof collapse (docs/research.md §40): when a roof holder goes, roofs
//! left unsupported are marked (`RoofCollapseCellsFinder`) and fall at the
//! start of the next tick (`RoofCollapseBufferResolver`,
//! `RoofCollapserImmediate`).

use std::collections::{HashSet, VecDeque};

use super::Sim;
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::reservation::Target;
use crate::roof::{MAX_SUPPORT_DISTANCE, holds_roof, within_range_of_roof_holder};

/// `GenAdj.CardinalDirectionsAndInside`.
const CARDINAL_AND_INSIDE: [Cell; 5] = [
    Cell::new(0, 1),
    Cell::new(1, 0),
    Cell::new(0, -1),
    Cell::new(-1, 0),
    Cell::new(0, 0),
];

/// `RoofCollapserImmediate.ThinRoofCrushDamageRange`.
const THIN_ROOF_CRUSH: (i32, i32) = (15, 30);

impl Sim {
    /// `RoofCollapseCellsFinder.Notify_RoofHolderDespawned` (non-removal
    /// mode): roofs no longer connected to any holder, and roofs within
    /// 6.9 cells now out of range of every holder, are marked to collapse.
    pub(super) fn roof_holder_despawned(&mut self, footprint: Footprint) {
        let rect: Vec<Cell> = footprint.cells().collect();
        self.check_flying_roofs(&rect);
        let r2 = (MAX_SUPPORT_DISTANCE * MAX_SUPPORT_DISTANCE) as i32;
        let mut too_far = Vec::new();
        for &d in crate::clean::radial_pattern()
            .iter()
            .take_while(|d| d.x * d.x + d.z * d.z <= r2)
        {
            let c = footprint.center + d;
            if self.map.size().contains(c)
                && self.map.roofed(c)
                && !self.roof_collapse.contains(&c)
                && !within_range_of_roof_holder(&self.map, &self.defs, c, false)
            {
                self.mark_to_collapse(c);
                too_far.push(c);
            }
        }
        self.check_flying_roofs(&too_far);
    }

    fn mark_to_collapse(&mut self, c: Cell) {
        if !self.roof_collapse.contains(&c) {
            self.roof_collapse.push(c);
        }
    }

    /// `CheckCollapseFlyingRoofs`: around each cell, a roofed patch that no
    /// longer reaches a roof holder is marked whole.
    fn check_flying_roofs(&mut self, near: &[Cell]) {
        let mut visited: HashSet<Cell> = HashSet::new();
        for &root in near {
            for d in CARDINAL_AND_INSIDE {
                let c = root + d;
                if !self.map.size().contains(c)
                    || !self.map.roofed(c)
                    || visited.contains(&c)
                    || self.roof_collapse.contains(&c)
                    || self.connects_to_roof_holder(c, &mut visited)
                {
                    continue;
                }
                for x in self.flood_roofed(c) {
                    self.mark_to_collapse(x);
                }
            }
        }
    }

    /// The roofed cells reachable from `root` (`FloodFiller` order).
    fn flood_roofed(&self, root: Cell) -> Vec<Cell> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([root]);
        let mut open = VecDeque::from([root]);
        while let Some(c) = open.pop_front() {
            out.push(c);
            for d in &CARDINAL_AND_INSIDE[..4] {
                let n = c + *d;
                if self.map.size().contains(n) && self.map.roofed(n) && seen.insert(n) {
                    open.push_back(n);
                }
            }
        }
        out
    }

    /// `ConnectsToRoofHolder`: floods the roofed cells from `c`; reaching
    /// a cell already checked (in `visited`) or one next to a roof holder
    /// counts as connected. Cells are added to `visited` as they are
    /// processed.
    fn connects_to_roof_holder(&self, c: Cell, visited: &mut HashSet<Cell>) -> bool {
        let mut connected = false;
        let mut seen = HashSet::from([c]);
        let mut open = VecDeque::from([c]);
        while let Some(x) = open.pop_front() {
            if visited.contains(&x) {
                connected = true;
            } else {
                visited.insert(x);
                if CARDINAL_AND_INSIDE
                    .iter()
                    .any(|&d| holds_roof(&self.map, &self.defs, x + d))
                {
                    connected = true;
                }
            }
            for d in &CARDINAL_AND_INSIDE[..4] {
                let n = x + *d;
                if !connected && self.map.size().contains(n) && self.map.roofed(n) && seen.insert(n)
                {
                    open.push_back(n);
                }
            }
        }
        connected
    }

    /// `RoofCollapseBufferResolver.CollapseRoofsMarkedToCollapse` (start of
    /// the tick): first every marked roof crushes what is under it, then
    /// each leaves rubble and vanishes.
    // COMPATIBILITY TODO: currently approximate — only constructed (thin)
    // roofs exist; buildings have no hit points, and pawn damage ignores
    // the top body height.
    pub(super) fn collapse_marked_roofs(&mut self) {
        if self.roof_collapse.is_empty() {
            return;
        }
        let cells = std::mem::take(&mut self.roof_collapse);
        for &c in &cells {
            if self.map.size().contains(c) && self.map.roofed(c) {
                self.crush_under_roof(c);
            }
        }
        // `RoofConstructed.filthLeaving`.
        let rubble = self.defs.things.id("Filth_RubbleBuilding");
        for &c in &cells {
            if self.map.size().contains(c) && self.map.roofed(c) {
                if let Some(rubble) = rubble {
                    self.try_make_filth(c, rubble, 0, true);
                }
                self.map.set_roof(c, false);
            }
        }
        self.light_key = None;
    }

    /// `DropRoofInCellPhaseOne` for a thin roof: 15–30 crush damage
    /// (randomly rounded) to each pawn, item and plant in the cell.
    fn crush_under_roof(&mut self, c: Cell) {
        let pawns: Vec<crate::pawn::PawnId> = self
            .pawns
            .iter()
            .filter(|p| p.position == c && !p.health.dead)
            .map(|p| p.id)
            .collect();
        for id in pawns.into_iter().rev() {
            let amount =
                self.rng
                    .range_inclusive(THIN_ROOF_CRUSH.0, THIN_ROOF_CRUSH.1) as f32;
            let amount = crate::plant::round_random(amount, &mut self.rng) as f32;
            self.damage_pawn_at(
                id,
                "Crush",
                amount,
                None,
                Some(rimworld_defs::health::PartDepth::Outside),
            );
        }
        let items: Vec<crate::map::ItemId> = self
            .map
            .items_at(c)
            .filter(|i| !i.is_filth() && self.defs.things[i.def].use_hit_points)
            .map(|i| i.id)
            .collect();
        for id in items.into_iter().rev() {
            let amount =
                self.rng
                    .range_inclusive(THIN_ROOF_CRUSH.0, THIN_ROOF_CRUSH.1) as f32;
            let amount = crate::plant::round_random(amount, &mut self.rng) as i32;
            self.damage_item(id, amount);
        }
        if let Some((id, hp)) = self.map.plant_at(c).map(|p| (p.id, p.hit_points)) {
            let amount =
                self.rng
                    .range_inclusive(THIN_ROOF_CRUSH.0, THIN_ROOF_CRUSH.1) as f32;
            let amount = crate::plant::round_random(amount, &mut self.rng) as f32;
            if hp - amount <= 0.0 {
                self.map.remove_plant(id);
                self.reservations.release_all_for_target(Target::Item(id));
            } else if let Some(p) = self.map.plant_mut(id) {
                p.hit_points -= amount;
            }
        }
    }
}
