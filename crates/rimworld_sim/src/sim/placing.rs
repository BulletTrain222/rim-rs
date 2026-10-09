//! Placing things near a cell (`GenPlace.TryPlaceThing` in Near mode,
//! docs/research.md §52): rate cells of the radial pattern by how well the
//! thing fits there and drop it on the best, stacking first.

use super::Sim;
use crate::grid::Cell;
use crate::pawn::Carried;

/// `GenPlace.PlaceSpotQuality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Quality {
    Unusable,
    Awful,
    Bad,
    Okay,
    Perfect,
}

/// `GenRadial.NumCellsInRadius(3)`.
fn middle_cells() -> usize {
    cells_in_radius(3.0)
}

/// `GenRadial.NumCellsInRadius(12.9)`.
fn max_cells() -> usize {
    cells_in_radius(12.9)
}

fn cells_in_radius(r: f32) -> usize {
    let r2 = (r * r) as i32;
    crate::clean::radial_pattern()
        .iter()
        .take_while(|c| c.x * c.x + c.z * c.z <= r2)
        .count()
}

impl Sim {
    /// `PlaceSpotQualityAt` for an item.
    // COMPATIBILITY TODO: currently approximate — shelves (more items per
    // cell, haul destinations that refuse the thing), work tables and
    // `preventDroppingThingsOn` are not modelled; reachability between
    // rooms is region connectivity.
    fn place_spot_quality(&self, c: Cell, thing: &Carried, center: Cell) -> Quality {
        let defs = &self.defs;
        if !self.map.size().contains(c)
            || !self.path_grid.walkable(c)
            || !self.map.standable_things(defs, c)
        {
            return Quality::Unusable;
        }
        let limit = defs.things[thing.def].stack_limit as u32;
        let items: Vec<&crate::map::Item> =
            self.map.items_at(c).filter(|i| !i.is_filth()).collect();
        let can_stack = items
            .iter()
            .any(|i| i.def == thing.def && i.stack_count < limit);
        if !can_stack && items.len() >= crate::haul::MAX_ITEMS_IN_CELL {
            return Quality::Unusable;
        }
        if self.regions.room_at(c) != self.regions.room_at(center) {
            return if self.regions.connected(center, c) {
                Quality::Bad
            } else {
                Quality::Awful
            };
        }
        if can_stack {
            return Quality::Perfect;
        }
        if self.map.door_at(c).is_some()
            || self
                .pawns
                .iter()
                .any(|p| p.position == c && p.health.downed)
        {
            return Quality::Bad;
        }
        // Selectable plants (bushes, trees, crops; not grass) make it
        // merely okay.
        if self
            .map
            .plant_at(c)
            .is_some_and(|p| defs.things[p.def].selectable)
        {
            return Quality::Okay;
        }
        Quality::Perfect
    }

    /// `TryFindPlaceSpotNear`: the best cell among the first 9 of the
    /// radial pattern (stopping at a perfect one), then within radius 3,
    /// then 12.9; the first two passes need at least an okay spot.
    fn find_place_spot_near(&self, center: Cell, thing: &Carried) -> Option<Cell> {
        let pattern = crate::clean::radial_pattern();
        let mut best = (Quality::Unusable, center);
        for (pass, n) in [9, middle_cells(), max_cells()].into_iter().enumerate() {
            for &d in &pattern[..n] {
                let c = center + d;
                let q = self.place_spot_quality(c, thing, center);
                if q > best.0 {
                    best = (q, c);
                }
                if best.0 == Quality::Perfect {
                    break;
                }
            }
            if best.0 >= Quality::Okay || (pass == 2 && best.0 > Quality::Unusable) {
                return Some(best.1);
            }
        }
        None
    }

    /// Debug tool: places `count` new `def` near `center` as
    /// `GenPlace.TryPlaceThing(Near)` does; returns whether all of it fit.
    pub fn debug_place_near(
        &mut self,
        def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
        count: u32,
        center: Cell,
    ) -> bool {
        let mut thing = Carried {
            id: self.map.allocate_item_id(),
            def,
            count,
            rot: 0.0,
            hit_points: None,
        };
        self.place_thing_near(&mut thing, center)
    }

    /// `GenPlace.TryPlaceThing(Near)`: find a spot, place directly, and go
    /// on with what is left while each round places something. Returns
    /// whether everything was placed.
    pub(super) fn place_thing_near(&mut self, thing: &mut Carried, center: Cell) -> bool {
        loop {
            let before = thing.count;
            let Some(spot) = self.find_place_spot_near(center, thing) else {
                return false;
            };
            if self.place_direct(thing, spot) {
                self.refresh_path_grid();
                return true;
            }
            if thing.count == before {
                return false;
            }
        }
    }
}
