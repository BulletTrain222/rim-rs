//! Starting apparel (docs/research.md §41).

use super::Sim;
use crate::pawn::PawnId;

impl Sim {
    /// Dresses a starting colonist: a cloth shirt and pants, and when the
    /// season calls for warm clothes a free cloth parka and hat as needed
    /// (`PawnApparelGenerator`'s needed warmth and free warm layers).
    // COMPATIBILITY TODO: currently approximate — the game picks apparel at
    // random from the pawn kind's tags and budget; every colonist here gets
    // the same cloth basics.
    pub fn give_starting_apparel(&mut self, pawn: PawnId) {
        let defs = self.defs.clone();
        let cloth = defs.things.id("Cloth");
        for name in ["Apparel_BasicShirt", "Apparel_Pants"] {
            if let Some(d) = defs.things.id(name) {
                self.wear_apparel(pawn, d, cloth);
            }
        }
        let Some(climate) = self.climate else {
            return;
        };
        let Some(i) = self.index_of(pawn) else {
            return;
        };
        // `ApparelWarmthNeededNow`: this twelfth or the next decides.
        let race_min = crate::stats::def_stat(
            &defs,
            &defs.things[self.pawns[i].race],
            None,
            "ComfyTemperatureMin",
        );
        let local = self.abs_tick() + crate::climate::time_zone(self.longitude) * 2500;
        let twelfth = local.div_euclid(300_000);
        let avg = |k: i64| {
            crate::climate::average_twelfth_temperature(&climate, self.latitude, twelfth + k)
        };
        let warm = (0..2)
            .map(avg)
            .find(|&t| t < race_min - 4.0 || t > race_min + 4.0)
            .is_some_and(|t| t < race_min - 4.0);
        if !warm {
            return;
        }
        // `AddFreeWarmthAsNeeded`: Warm is satisfied by safety at this
        // twelfth's temperature and 52 cold insulation; a free parka, then
        // a free hat.
        let map_temperature = avg(0);
        let satisfied = |sim: &Sim| {
            let insulation: f32 = sim.pawns[i]
                .apparel
                .iter()
                .map(|a| {
                    crate::stats::def_stat(
                        &defs,
                        &defs.things[a.def],
                        a.stuff.map(|s| &defs.things[s]),
                        "Insulation_Cold",
                    )
                })
                .sum();
            map_temperature >= sim.pawn_stat_of(i, "ComfyTemperatureMin") - 10.0
                && insulation >= 52.0
        };
        for name in ["Apparel_Parka", "Apparel_Tuque"] {
            if satisfied(self) {
                return;
            }
            if let Some(d) = defs.things.id(name) {
                self.wear_apparel(pawn, d, cloth);
            }
        }
    }
}
