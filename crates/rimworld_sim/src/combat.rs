//! Melee combat (docs/research.md §26): a pawn's melee verbs from its
//! race's tools and the maneuvers, verb selection (`Pawn_MeleeVerbs`,
//! `VerbUtility`) and the hit and dodge rolls (`Verb_MeleeAttack`).

use rimworld_defs::{GameDefs, ThingDef};

use crate::health::HealthView;
use crate::rand::Rand;
use crate::stats::{Skills, pawn_stat_with_capacities};

/// A melee verb: a tool used with one of its capacities.
#[derive(Debug, Clone, PartialEq)]
pub struct MeleeVerb {
    pub tool: usize,
    /// The `DamageDef` the maneuver deals.
    pub damage: String,
    pub power: f32,
    pub cooldown_seconds: f32,
    pub chance_factor: f32,
    pub armor_penetration: f32,
    pub linked_group: Option<String>,
    pub ensure_usable: bool,
}

/// `VerbTracker`: one verb per tool and capacity with a maneuver.
pub fn melee_verbs(defs: &GameDefs, race: &ThingDef) -> Vec<MeleeVerb> {
    let mut out = Vec::new();
    for (n, tool) in race.tools.iter().enumerate() {
        for cap in &tool.capacities {
            for (_, m) in defs.maneuvers.iter() {
                if &m.required_capacity == cap
                    && let Some(damage) = &m.melee_damage_def
                {
                    out.push(MeleeVerb {
                        tool: n,
                        damage: damage.clone(),
                        power: tool.power,
                        cooldown_seconds: tool.cooldown_time,
                        chance_factor: tool.chance_factor,
                        armor_penetration: tool.armor_penetration,
                        linked_group: tool.linked_body_parts_group.clone(),
                        ensure_usable: tool.ensure_linked_body_parts_group_always_usable,
                    });
                }
            }
        }
    }
    out
}

/// The attacker as the melee formulas see it.
pub struct Attacker<'a> {
    pub defs: &'a GameDefs,
    pub race: &'a ThingDef,
    pub skills: &'a Skills,
    pub health: &'a HealthView<'a>,
}

impl Attacker<'_> {
    fn stat(&self, stat: &str) -> f32 {
        pawn_stat_with_capacities(self.defs, self.race, self.skills, stat, &|c| {
            self.health.capacity(c)
        })
    }

    /// `CalculateNaturalPartsAverageEfficiency` of the tool's body part
    /// group (at least 0.4 for tools that must stay usable).
    fn group_efficiency(&self, v: &MeleeVerb) -> f32 {
        let Some(group) = &v.linked_group else {
            return 1.0;
        };
        let body = self.health.body;
        let parts: Vec<usize> = (0..body.parts.len())
            .filter(|&p| !self.health.missing(p) && body.parts[p].groups.iter().any(|g| g == group))
            .collect();
        let mut e = if parts.is_empty() {
            0.0
        } else {
            parts
                .iter()
                .map(|&p| self.health.part_efficiency(p))
                .sum::<f32>()
                / parts.len() as f32
        };
        if v.ensure_usable {
            e = e.max(0.4);
        }
        e
    }

    /// `AdjustedMeleeDamageAmount`.
    pub fn damage_amount(&self, v: &MeleeVerb) -> f32 {
        v.power * self.group_efficiency(v) * self.stat("MeleeDamageFactor")
    }

    /// `AdjustedCooldown` in seconds.
    pub fn cooldown_seconds(&self, v: &MeleeVerb) -> f32 {
        v.cooldown_seconds * self.stat("MeleeCooldownFactor")
    }

    /// `VerbUtility.DPS` × `AdditionalSelectionFactor`.
    fn initial_weight(&self, v: &MeleeVerb) -> f32 {
        let dmg = self.damage_amount(v);
        let pen = if v.armor_penetration < 0.0 {
            dmg * 0.015
        } else {
            v.armor_penetration
        };
        let cycle = self.cooldown_seconds(v);
        let dps = if cycle > 0.0 {
            dmg * (1.0 + pen) / cycle
        } else {
            0.0
        };
        dps * v.chance_factor
    }

    /// `ChooseMeleeVerb`: one draw for the terrain-tool chance (4%; no
    /// terrain tools exist here), then a weighted pick where the best verbs
    /// (≥ 95% of the best weight) share 0.75, middling ones (≥ 25%) share
    /// 0.25 and the rest get nothing (`FinalSelectionWeight`).
    pub fn choose_verb(&self, verbs: &[MeleeVerb], rng: &mut Rand) -> Option<usize> {
        let weights = self.final_weights(verbs);
        let _terrain = rng.chance(0.04);
        crate::region::random_element_by_weight(&weights, rng)
    }

    /// `FinalSelectionWeight` of each verb.
    pub fn final_weights(&self, verbs: &[MeleeVerb]) -> Vec<f32> {
        let weights: Vec<f32> = verbs.iter().map(|v| self.initial_weight(v)).collect();
        let best = weights.iter().copied().fold(0.0f32, f32::max);
        #[derive(PartialEq)]
        enum Cat {
            Best,
            Mid,
            Worst,
        }
        let cat = |w: f32| {
            if w >= best * 0.95 {
                Cat::Best
            } else if w < best * 0.25 {
                Cat::Worst
            } else {
                Cat::Mid
            }
        };
        let cats: Vec<Cat> = weights.iter().map(|&w| cat(w)).collect();
        let final_w: Vec<f32> = cats
            .iter()
            .map(|c| {
                if *c == Cat::Worst {
                    return 0.0;
                }
                let n = cats.iter().filter(|x| *x == c).count() as f32;
                1.0 / n * if *c == Cat::Mid { 0.25 } else { 0.75 }
            })
            .collect();
        final_w
    }

    pub fn hit_chance(&self) -> f32 {
        self.stat("MeleeHitChance")
    }
}

/// A defender's `MeleeDodgeChance` (0 when it cannot move: downed).
pub fn dodge_chance(
    defs: &GameDefs,
    race: &ThingDef,
    skills: &Skills,
    health: &HealthView<'_>,
    immobile: bool,
) -> f32 {
    if immobile {
        return 0.0;
    }
    pawn_stat_with_capacities(defs, race, skills, "MeleeDodgeChance", &|c| {
        health.capacity(c)
    })
}
