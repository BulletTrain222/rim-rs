//! Steady environment effects (docs/research.md §42): every tick a few
//! cells, taken in a fixed shuffled order (`MapCellsInRandomOrder`), get
//! their weathering: unprotected items deteriorate
//! (`SteadyEnvironmentEffects`).

use super::Sim;
use crate::grid::Cell;
use crate::map::ItemId;
use crate::reservation::Target;

/// `SteadyEnvironmentEffects.MapFractionCheckPerTick`.
const MAP_FRACTION_PER_TICK: f32 = 0.0006;
/// A visit every ~1/0.0006 ticks: 36 a day, so a daily rate is split 36
/// ways (`Rand.Chance(rate / 36)`).
const VISITS_PER_DAY: f32 = 36.0;

impl Sim {
    /// The map's cells in their fixed random order (`Rand.Seed` set, the
    /// row-major cell list shuffled).
    fn steady_order(&mut self) -> &[Cell] {
        let size = self.map.size();
        if self.steady_order.len() != size.area() {
            let mut cells: Vec<Cell> = (0..size.height)
                .flat_map(|z| (0..size.width).map(move |x| Cell::new(x, z)))
                .collect();
            let mut rng = crate::rand::Rand::new(self.steady_seed);
            rng.shuffle(&mut cells);
            self.steady_order = cells;
        }
        &self.steady_order
    }

    /// `SteadyEnvironmentEffectsTick`: ceil(area × 0.0006) cells this tick.
    // COMPATIBILITY TODO: currently approximate — no weather (rain washing
    // filth, snow), fire from heat or terrain heat; the shuffle seed is
    // ours, not the world seed ^ tile hash.
    pub(super) fn tick_steady_effects(&mut self) {
        let area = self.map.size().area();
        if area == 0 {
            return;
        }
        let n = (area as f32 * MAP_FRACTION_PER_TICK).ceil() as usize;
        for _ in 0..n {
            if self.steady_cycle >= area {
                self.steady_cycle = 0;
            }
            let k = self.steady_cycle;
            let c = self.steady_order()[k];
            self.cell_steady_effects(c);
            self.steady_cycle += 1;
        }
    }

    /// `DoCellSteadyEffects` (the parts we model): in a room (outdoors
    /// counts), old filth disappears and each other item may take a point
    /// of deterioration damage.
    fn cell_steady_effects(&mut self, c: Cell) {
        let Some(room) = self.room_at(c) else {
            return;
        };
        let outdoors = self.room_uses_outdoor(room);
        let roofed = self.map.roofed(c);
        // Filth that has lain long enough since it last thickened vanishes.
        let now = self.tick as i64;
        let stale: Vec<ItemId> = self
            .map
            .items_at(c)
            .filter(|i| {
                i.is_filth() && i.disappear_after != 0 && now - i.grow_tick > i.disappear_after
            })
            .map(|i| i.id)
            .collect();
        for id in stale.into_iter().rev() {
            self.map.take_from_item(id, u32::MAX);
            self.reservations.release_all_for_target(Target::Item(id));
        }
        let items: Vec<ItemId> = self
            .map
            .items_at(c)
            .filter(|i| !i.is_filth())
            .map(|i| i.id)
            .collect();
        for id in items.into_iter().rev() {
            let rate = self.deterioration_rate(id, roofed, outdoors, c);
            if rate >= 0.001 && self.rng.chance(rate / VISITS_PER_DAY) {
                self.deteriorate(id);
            }
        }
        // Plants that can deteriorate (stumps) weather too.
        if let Some((id, def)) = self.map.plant_at(c).map(|p| (p.id, p.def)) {
            let d = &self.defs.things[def];
            if d.plant.as_ref().is_some_and(|p| p.can_deteriorate) && d.use_hit_points {
                let base = crate::stats::def_stat(&self.defs, d, None, "DeteriorationRate");
                let factor = (if roofed { 0.0 } else { 0.5 })
                    + (if outdoors { 0.5 } else { 0.0 })
                    + self.defs.terrain[self.map.terrain[c]].extra_deterioration_factor;
                let rate = if d.deteriorate_from_environmental_effects {
                    base * factor
                } else {
                    base
                };
                if rate >= 0.001 && self.rng.chance(rate / VISITS_PER_DAY) {
                    let gone = self.map.plant_mut(id).is_some_and(|p| {
                        p.hit_points -= 1.0;
                        p.hit_points <= 0.0
                    });
                    if gone {
                        self.map.remove_plant(id);
                        self.reservations.release_all_for_target(Target::Item(id));
                    }
                }
            }
        }
    }

    /// `FinalDeteriorationRate`: the DeteriorationRate stat with
    /// `StatPart_EnvironmentalEffects` (+0.5 unroofed, +0.5 in an outdoor
    /// room, the terrain's extra factor; nothing indoors under a roof).
    // COMPATIBILITY TODO: currently approximate — quality, shelves that
    // protect what lies on them and rain are not modelled.
    fn deterioration_rate(&self, id: ItemId, roofed: bool, outdoors: bool, c: Cell) -> f32 {
        let Some(item) = self.map.item(id) else {
            return 0.0;
        };
        let def = &self.defs.things[item.def];
        if !def.use_hit_points || def.category.as_deref() != Some("Item") {
            return 0.0;
        }
        let base = crate::stats::def_stat(&self.defs, def, None, "DeteriorationRate");
        if base <= 0.0 {
            return 0.0;
        }
        if !def.deteriorate_from_environmental_effects {
            return base;
        }
        let (unroofed, outdoor) = self
            .defs
            .stats
            .get("DeteriorationRate")
            .and_then(|s| s.environmental_effects)
            .unwrap_or((0.5, 0.5));
        let mut factor = 0.0;
        if !roofed {
            factor += unroofed;
        }
        if outdoors {
            factor += outdoor;
        }
        factor += self.defs.terrain[self.map.terrain[c]].extra_deterioration_factor;
        base * factor
    }

    /// `DoDeteriorationDamage`: one point of damage.
    fn deteriorate(&mut self, id: ItemId) {
        self.damage_item(id, 1);
    }

    /// An item takes `amount` damage; at zero hit points it is destroyed.
    pub(super) fn damage_item(&mut self, id: ItemId, amount: i32) {
        let Some(def) = self.map.item(id).map(|i| i.def) else {
            return;
        };
        let max = self.max_hit_points(def);
        let gone = match self.map.items_mut().iter_mut().find(|i| i.id == id) {
            Some(i) => {
                let hp = i.hit_points.unwrap_or(max) - amount;
                i.hit_points = Some(hp);
                hp <= 0
            }
            None => false,
        };
        if gone {
            self.map.take_from_item(id, u32::MAX);
            self.reservations.release_all_for_target(Target::Item(id));
        }
    }
}
