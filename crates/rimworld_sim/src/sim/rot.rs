//! Rotting (`CompRottable`, docs/research.md §28): rottable items gain rot
//! on their rare tick by the rot rate at their temperature; food that
//! starts rotting is destroyed.

use super::Sim;
use crate::reservation::Target;

/// Ticks between an item's rare ticks (`TickerType.Rare`).
pub const RARE_TICK_INTERVAL: u64 = 250;

/// `GenTemperature.RotRateAtTemperature`: none below 0 °C, full from
/// 10 °C, linear between.
pub fn rot_rate_at_temperature(celsius: f32) -> f32 {
    if celsius < 0.0 {
        0.0
    } else if celsius >= 10.0 {
        1.0
    } else {
        celsius / 10.0
    }
}

impl Sim {
    /// The rare tick list's turn for items: those whose `thingIDNumber`
    /// falls in this tick's bucket (`TickList`, after the normal list).
    // COMPATIBILITY TODO: currently approximate — carried things don't
    // rot; rot stink is not modelled.
    pub(super) fn tick_rot(&mut self) {
        let bucket = (self.tick % RARE_TICK_INTERVAL) as i64;
        let due: Vec<(usize, f32)> = self
            .map
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                (item.id_number as i64).rem_euclid(RARE_TICK_INTERVAL as i64) == bucket
                    && self.defs.things[item.def].rottable.is_some()
            })
            .map(|(n, item)| (n, self.cell_temperature(item.position)))
            .collect();
        let mut rotted = Vec::new();
        let mut damaged = Vec::new();
        for (n, temperature) in due {
            let item = &mut self.map.items_mut()[n];
            let Some(props) = self.defs.things[item.def].rottable.as_ref() else {
                continue;
            };
            let before = item.rot;
            item.rot += rot_rate_at_temperature(temperature) * RARE_TICK_INTERVAL as f32;
            if props.rot_destroys && item.rot >= props.ticks_to_rot_start() as f32 {
                rotted.push(item.id);
            } else if (before / 60_000.0).floor() != (item.rot / 60_000.0).floor() {
                // Each rot day crossed while rotting or dessicated deals rot
                // damage (`rotDamagePerDay`, `dessicatedDamagePerDay`).
                let per_day = if item.rot >= props.days_to_dessicated * 60_000.0 {
                    props.dessicated_damage_per_day
                } else if item.rot >= props.ticks_to_rot_start() as f32 {
                    props.rot_damage_per_day
                } else {
                    0.0
                };
                if per_day > 0.0 {
                    damaged.push((item.id, per_day));
                }
            }
        }
        for (id, per_day) in damaged {
            let amount = crate::plant::round_random(per_day, &mut self.rng);
            if amount > 0 {
                self.damage_item(id, amount as i32);
            }
        }
        for id in rotted {
            if let Some(n) = self.map.item(id).map(|i| i.stack_count) {
                self.map.take_from_item(id, n);
                self.reservations.release_all_for_target(Target::Item(id));
            }
        }
    }
}
