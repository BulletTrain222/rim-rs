//! Lamps in the simulation: fuel burning (`CompRefuelable.CompTick`) and
//! the light grid kept up to date (`GlowGrid`).

use super::Sim;
use crate::grid::Cell;
use crate::hash::is_hash_interval_tick;

/// `CompHeatPusher` pushes every 60 ticks on the normal tick list.
const HEAT_PUSH_INTERVAL: i64 = 60;
/// On the rare tick list it pushes 250/60 times as much every 250 ticks.
const RARE_TICK_INTERVAL: u64 = 250;
const RARE_HEAT_FACTOR: f32 = 4.166_666_5;

impl Sim {
    /// Refuelable buildings burn fuel each tick; one that runs out goes
    /// dark.
    // COMPATIBILITY TODO: currently approximate — no on/off switches, rain
    // consumption, or fuel use only when working.
    pub(super) fn tick_fuel(&mut self) {
        let defs = self.defs.clone();
        let mut ran_out = false;
        for s in self.map.structures_mut() {
            let Some(r) = &defs.things[s.def].refuelable else {
                continue;
            };
            // `CompRefuelable.CompTick`: burns while switched on (work
            // tables that burn only while used do it in `UsedThisTick`).
            if r.consume_fuel_only_when_used
                || s.fuel <= 0.0
                || !s.power.switch_on
                || s.power.broken_down
            {
                continue;
            }
            s.fuel -= r.consumption_rate / 60_000.0;
            if s.fuel <= 0.0 {
                s.fuel = 0.0;
                ran_out = true;
            }
        }
        if ran_out {
            self.light_key = None;
        }
    }

    /// Debug tool: brings the light grid up to date now.
    pub fn debug_refresh_light(&mut self) {
        self.refresh_light();
    }

    /// Fuel left in the building on `cell`, if it burns fuel.
    pub fn fuel_at(&self, cell: Cell) -> Option<f32> {
        let s = self
            .map
            .structures()
            .iter()
            .find(|s| s.footprint.contains(cell))?;
        self.defs.things[s.def].refuelable.as_ref().map(|_| s.fuel)
    }

    /// Debug tool: sets the fuel of the building on `cell`.
    pub fn debug_set_fuel(&mut self, cell: Cell, fuel: f32) {
        if let Some(s) = self
            .map
            .structures_mut()
            .iter_mut()
            .find(|s| s.footprint.contains(cell))
        {
            s.fuel = fuel;
        }
        self.light_key = None;
    }

    /// Debug tool: sets the `thingIDNumber` of the building on `cell` (its
    /// heat push ticks).
    pub fn debug_set_building_id_number(&mut self, cell: Cell, id_number: i32) {
        if let Some(s) = self
            .map
            .structures_mut()
            .iter_mut()
            .find(|s| s.footprint.contains(cell))
        {
            s.id_number = id_number;
        }
    }

    /// Recomputes the light grid when buildings changed or a lamp went out.
    pub(super) fn refresh_light(&mut self) {
        let key = self.map.structure_revision();
        if self.light_key == Some(key) && self.map.light.is_some() {
            return;
        }
        self.light_key = Some(key);
        let defs = &self.defs;
        let lights: Vec<(Cell, &rimworld_defs::GlowerProperties)> = self
            .map
            .structures()
            .iter()
            .filter_map(|s| {
                let d = &defs.things[s.def];
                let g = d.glower.as_ref()?;
                // `CompGlower.ShouldBeLitNow`: switched on, powered, fuelled.
                let lit = (d.refuelable.is_none() || s.fuel > 0.0)
                    && (d.power.is_none() || s.power.on)
                    && s.power.switch_on
                    && !s.power.broken_down;
                lit.then_some((s.footprint.center, g))
            })
            .collect();
        let map = &self.map;
        let blocks = |c: Cell| map.buildings[c].is_some_and(|b| defs.things[b].block_light);
        let grid = crate::light::light_grid(map.size(), &blocks, &lights);
        self.map.light = Some(grid);
    }

    /// Heaters and fires warm their room (`CompHeatPusher`): while the
    /// temperature at the building is inside its range (and, for
    /// `CompHeatPusherPowered`, it has fuel), heat per second goes into the
    /// room, divided by its cells (`Room.PushHeat`). A building outside any
    /// room shares it among the rooms around it.
    pub(super) fn tick_heat_pushers(&mut self) {
        let t = self.tick;
        let mut pushes = Vec::new();
        for s in self.map.structures() {
            let def = &self.defs.things[s.def];
            let Some(h) = &def.heat_pusher else {
                continue;
            };
            let energy = match def.ticker_type.as_deref() {
                Some("Normal") if is_hash_interval_tick(t, s.id_number, HEAT_PUSH_INTERVAL) => {
                    h.heat_per_second
                }
                Some("Rare")
                    if (s.id_number as i64).rem_euclid(RARE_TICK_INTERVAL as i64)
                        == (t % RARE_TICK_INTERVAL) as i64 =>
                {
                    h.heat_per_second * RARE_HEAT_FACTOR
                }
                _ => continue,
            };
            // `CompHeatPusherPowered`: switched on, powered and fuelled.
            if h.powered
                && (!s.power.switch_on
                    || s.power.broken_down
                    || (def.power.is_some() && !s.power.on)
                    || (def.refuelable.is_some() && s.fuel <= 0.0))
            {
                continue;
            }
            let ambient = self.cell_temperature(s.footprint.center);
            if ambient < h.max_temperature && ambient > h.min_temperature {
                pushes.push((s.footprint.center, energy));
            }
        }
        for (c, energy) in pushes {
            self.push_heat(c, energy);
        }
    }
}
