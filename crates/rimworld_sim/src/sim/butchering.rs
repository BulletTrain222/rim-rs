//! Butchering (docs/research.md §62): what a corpse yields
//! (`Pawn.ButcherProducts`, `Corpse.ButcherProducts`) and the meat and
//! leather amount stats.

use rimworld_defs::{DefId, ThingDef};

use super::Sim;
use crate::pawn::Carried;

/// `Difficulty.butcherYieldFactor` (Rough).
const BUTCHER_YIELD_DIFFICULTY_FACTOR: f32 = 1.0;

impl Sim {
    /// `MeatAmount` / `LeatherAmount` of dead pawn `t`: the race's base
    /// value (else the stat's default), then the stat's parts in their Def
    /// order — body size, the coverage of natural parts not missing, ×
    /// `factor` with a fresh injury (`StatPart_NotCarefullySlaughtered`),
    /// the difficulty's butcher yield and the malnutrition curve — then
    /// the post-process curve and clamping.
    // COMPATIBILITY TODO: currently approximate — life stages don't exist
    // (every animal is an adult: body size factor 1) and injuries are
    // never permanent; the execution cut is not made, so it never needs
    // excluding.
    pub(super) fn butcher_stat(&self, t: usize, stat: &str) -> f32 {
        let pawn = &self.pawns[t];
        let race_def = &self.defs.things[pawn.race];
        let Some(s) = self.defs.stats.get(stat) else {
            return 0.0;
        };
        let mut v = race_def.stat(stat).unwrap_or(s.default_base_value);
        let view = self.health_view(pawn.id);
        let parts = self
            .defs
            .raw
            .get("StatDef", stat)
            .and_then(|d| d.node.child("parts"))
            .map(|p| p.children.clone())
            .unwrap_or_default();
        for part in parts.iter().filter(|c| c.name == "li") {
            match part.attr("Class") {
                Some("StatPart_BodySize") => {
                    v *= race_def.race.as_ref().map_or(1.0, |r| r.base_body_size);
                }
                Some("StatPart_NaturalNotMissingBodyPartsCoverage") => {
                    v *= view
                        .as_ref()
                        .map_or(1.0, |h| h.coverage_of_not_missing_natural_parts());
                }
                Some("StatPart_NotCarefullySlaughtered") => {
                    let factor = part
                        .child_text("factor")
                        .and_then(|f| f.trim().parse::<f32>().ok())
                        .unwrap_or(0.0);
                    let wounded = pawn
                        .health
                        .hediffs
                        .iter()
                        .any(|h| self.defs.hediffs[h.def].is_injury());
                    if wounded {
                        v *= factor;
                    }
                }
                Some("StatPart_Difficulty_ButcherYield") => v *= BUTCHER_YIELD_DIFFICULTY_FACTOR,
                Some("StatPart_Malnutrition") => {
                    let severity = self.defs.hediffs.id("Malnutrition").and_then(|m| {
                        pawn.health
                            .hediffs
                            .iter()
                            .find(|h| h.def == m)
                            .map(|h| h.severity)
                    });
                    if let Some(sev) = severity {
                        let curve: Vec<(f32, f32)> = part
                            .path(&["curve", "points"])
                            .map(|pts| {
                                pts.children
                                    .iter()
                                    .filter_map(|li| {
                                        let x = li.path_text(&["loc", "x"])?.trim().parse().ok()?;
                                        let y = li.path_text(&["loc", "y"])?.trim().parse().ok()?;
                                        Some((x, y))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        v *= crate::stats::evaluate_curve(&curve, sev);
                    }
                }
                _ => {}
            }
        }
        crate::stats::finalize_after_parts(&self.defs, stat, v)
    }

    /// `Corpse.ButcherProducts` for the dead pawn `t` butchered by `i` at
    /// `efficiency`: meat RoundRandom(MeatAmount × efficiency), then
    /// leather RoundRandom(LeatherAmount × efficiency) (one draw each,
    /// none made at 0), then the race's blood on the butcher's cell.
    // COMPATIBILITY TODO: currently approximate — the race's own
    // `butcherProducts` and life-stage body parts (tusks, horns) are not
    // made; humanlike butchering events and tales are not recorded.
    pub(super) fn butcher_products(&mut self, t: usize, i: usize, efficiency: f32) -> Vec<Carried> {
        let race = self.defs.things[self.pawns[t].race].race.clone();
        let mut out = Vec::new();
        let make =
            |sim: &mut Sim, def: Option<DefId<ThingDef>>, stat: &str, out: &mut Vec<Carried>| {
                let Some(def) = def else {
                    return;
                };
                let amount = sim.butcher_stat(t, stat);
                let n = crate::plant::round_random(amount * efficiency, &mut sim.rng);
                if n > 0 {
                    out.push(Carried {
                        id: sim.map.allocate_item_id(),
                        def,
                        count: n,
                        rot: 0.0,
                        hit_points: None,
                    });
                }
            };
        let meat = race
            .as_ref()
            .and_then(|r| r.meat_def.as_deref())
            .and_then(|m| self.defs.things.id(m));
        make(self, meat, "MeatAmount", &mut out);
        let leather = race
            .as_ref()
            .and_then(|r| r.leather_def.as_deref())
            .and_then(|m| self.defs.things.id(m));
        make(self, leather, "LeatherAmount", &mut out);
        if let Some(blood) = race
            .as_ref()
            .and_then(|r| r.blood_def.as_deref())
            .and_then(|b| self.defs.things.id(b))
        {
            let cell = self.pawns[i].position;
            self.try_make_filth(cell, blood, 0, true);
        }
        out
    }

    /// The meat and leather amounts a dead pawn's corpse would yield
    /// before the butcher's efficiency (`MeatAmount`, `LeatherAmount`).
    pub fn butcher_yield(&self, pawn: crate::pawn::PawnId) -> Option<(f32, f32)> {
        let t = self.index_of(pawn)?;
        Some((
            self.butcher_stat(t, "MeatAmount"),
            self.butcher_stat(t, "LeatherAmount"),
        ))
    }

    /// Debug tool: kills a pawn outright (no injury).
    pub fn debug_kill(&mut self, pawn: crate::pawn::PawnId) {
        if let Some(t) = self.index_of(pawn)
            && !self.pawns[t].health.dead
        {
            self.die(t);
        }
    }
}
