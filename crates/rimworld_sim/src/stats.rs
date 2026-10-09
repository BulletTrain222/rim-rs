//! Stat evaluation (docs/research.md §22), following `StatWorker`: the base
//! value, skill offsets, stuff factors and offsets, stat factors, skill
//! factors, then finalization (round to fives above `roundToFiveOver`,
//! clamp to the stat's range).
//!
//! COMPATIBILITY TODO: currently approximate — stat parts, capacities,
//! traits, hediffs, genes, ideology, life stages, apparel and comps are
//! not applied (none are modelled yet).

use std::collections::BTreeMap;

use rimworld_defs::{GameDefs, ThingDef};

/// A pawn's skill levels (0–20).
// COMPATIBILITY TODO: currently approximate — skills totally disabled by
// backstories, aptitudes and memory traits are not modelled.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Skills {
    levels: BTreeMap<String, i32>,
    /// Experience toward the next level (`xpSinceLastLevel`).
    #[serde(default)]
    xp: BTreeMap<String, f32>,
    /// Experience gained since midnight (`xpSinceMidnight`).
    #[serde(default)]
    xp_today: BTreeMap<String, f32>,
    #[serde(default)]
    passions: BTreeMap<String, Passion>,
    /// Tick of the last midnight reset (`lastXpSinceMidnightResetTimestamp`).
    #[serde(default = "never")]
    last_midnight_reset: i64,
}

fn never() -> i64 {
    -1
}

impl Default for Skills {
    fn default() -> Self {
        Self {
            levels: BTreeMap::new(),
            xp: BTreeMap::new(),
            xp_today: BTreeMap::new(),
            passions: BTreeMap::new(),
            last_midnight_reset: never(),
        }
    }
}

/// `Passion` (ordered None < Minor < Major).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum Passion {
    #[default]
    None,
    Minor,
    Major,
}

/// `SkillRecord.XpForLevelUpCurve`.
const XP_FOR_LEVEL_UP: [(f32, f32); 3] = [(0.0, 1000.0), (9.0, 10_000.0), (19.0, 30_000.0)];
/// `SkillRecord.MaxFullRateXpPerDay`.
const MAX_FULL_RATE_XP_PER_DAY: f32 = 4000.0;

/// `XpRequiredToLevelUpFrom`.
pub fn xp_to_level_up(level: i32) -> f32 {
    evaluate_curve(&XP_FOR_LEVEL_UP, level as f32)
}

impl Skills {
    pub fn level(&self, skill: &str) -> i32 {
        self.levels.get(skill).copied().unwrap_or(0)
    }

    pub fn set(&mut self, skill: &str, level: i32) {
        self.levels.insert(skill.to_owned(), level.clamp(0, 20));
    }

    pub fn xp(&self, skill: &str) -> f32 {
        self.xp.get(skill).copied().unwrap_or(0.0)
    }

    pub fn set_xp(&mut self, skill: &str, xp: f32) {
        self.xp.insert(skill.to_owned(), xp);
    }

    pub fn passion(&self, skill: &str) -> Passion {
        self.passions.get(skill).copied().unwrap_or_default()
    }

    pub fn set_passion(&mut self, skill: &str, passion: Passion) {
        self.passions.insert(skill.to_owned(), passion);
    }

    /// `SkillRecord.Learn` (not direct): positive experience is scaled by
    /// the passion (0.35 / 1 / 1.5), `learning_factor`
    /// (GlobalLearningFactor) and ×0.2 once 4000 was gained since
    /// midnight; levels rise and fall with it.
    pub fn learn(&mut self, skill: &str, mut xp: f32, learning_factor: f32) {
        let mut level = self.level(skill);
        if xp < 0.0 && level == 0 {
            return;
        }
        if xp > 0.0 {
            let mut factor = match self.passion(skill) {
                Passion::None => 0.35,
                Passion::Minor => 1.0,
                Passion::Major => 1.5,
            } * learning_factor;
            if self.xp_today.get(skill).copied().unwrap_or(0.0) > MAX_FULL_RATE_XP_PER_DAY {
                factor *= 0.2;
            }
            xp *= factor;
        }
        let mut cur = self.xp(skill) + xp;
        *self.xp_today.entry(skill.to_owned()).or_insert(0.0) += xp;
        if level == 20 && cur > xp_to_level_up(level) - 1.0 {
            cur = xp_to_level_up(level) - 1.0;
        }
        while cur >= xp_to_level_up(level) {
            cur -= xp_to_level_up(level);
            level += 1;
            if level >= 20 {
                level = 20;
                cur = cur.clamp(0.0, xp_to_level_up(level) - 1.0);
                break;
            }
        }
        while cur <= -1000.0 {
            level -= 1;
            cur += xp_to_level_up(level);
            if level <= 0 {
                level = 0;
                cur = 0.0;
                break;
            }
        }
        self.levels.insert(skill.to_owned(), level);
        self.xp.insert(skill.to_owned(), cur);
    }

    /// `Pawn_SkillTracker.SkillsTickInterval` on its 200-tick hash
    /// interval: the midnight reset of daily experience (local hour 0, at
    /// most every 30000 ticks), then each skill's decay above level 9.
    pub fn interval(&mut self, tick: i64, local_hour: u32) {
        if local_hour == 0
            && (self.last_midnight_reset < 0 || tick - self.last_midnight_reset >= 30_000)
        {
            self.xp_today.clear();
            self.last_midnight_reset = tick;
        }
        let skills: Vec<String> = self.levels.keys().cloned().collect();
        for s in skills {
            let decay = match self.level(&s) {
                10 => 0.1,
                11 => 0.2,
                12 => 0.4,
                13 => 0.6,
                14 => 1.0,
                15 => 1.8,
                16 => 2.8,
                17 => 4.0,
                18 => 6.0,
                19 => 8.0,
                20 => 12.0,
                _ => 0.0,
            };
            if decay > 0.0 {
                self.learn(&s, -decay, 1.0);
            }
        }
    }
}

/// `Mathf.Round` (ties to even).
fn round(v: f32) -> f32 {
    v.round_ties_even()
}

/// `SimpleCurve.Evaluate` (linear between points, clamped outside them):
/// t = (x − x0) / (x1 − x0) stored, then `Mathf.Lerp(y0, y1, t)`.
pub fn evaluate_curve(points: &[(f32, f32)], x: f32) -> f32 {
    match points {
        [] => x,
        [(_, y)] => *y,
        _ => {
            if x <= points[0].0 {
                return points[0].1;
            }
            if x >= points[points.len() - 1].0 {
                return points[points.len() - 1].1;
            }
            for w in points.windows(2) {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                if x <= x1 {
                    let t = ((x as f64 - x0 as f64) / (x1 as f64 - x0 as f64)) as f32;
                    let t = t.clamp(0.0, 1.0);
                    return (y0 as f64 + (y1 as f64 - y0 as f64) * t as f64) as f32;
                }
            }
            points[points.len() - 1].1
        }
    }
}

fn finalize(defs: &GameDefs, stat: &str, v: f32) -> f32 {
    finalize_lit(defs, stat, v, None)
}

/// `FinalizeValue` after the caller applied the stat's parts: the
/// post-process curve, rounding and clamping.
pub fn finalize_after_parts(defs: &GameDefs, stat: &str, v: f32) -> f32 {
    finalize(defs, stat, v)
}

/// `FinalizeValue`: the stat parts (only `StatPart_Glow`, given the light
/// on the pawn's cell and whether it is humanlike), then the post-process
/// curve, rounding and clamping.
fn finalize_lit(defs: &GameDefs, stat: &str, mut v: f32, light: Option<(f32, bool)>) -> f32 {
    if let Some(s) = defs.stats.get(stat) {
        if let (Some(g), Some((glow, humanlike))) = (&s.glow_part, light)
            && (humanlike || !g.humanlike_only)
        {
            v *= evaluate_curve(&g.curve, glow);
        }
        if !s.post_process_curve.is_empty() {
            v = evaluate_curve(&s.post_process_curve, v);
        }
        if v.abs() > s.round_to_five_over {
            v = round(v / 5.0) * 5.0;
        }
        v = v.clamp(s.min_value, s.max_value);
    }
    v
}

fn base_value(defs: &GameDefs, def: &ThingDef, stat: &str) -> f32 {
    def.stat(stat)
        .or_else(|| defs.stats.get(stat).map(|s| s.default_base_value))
        .unwrap_or(1.0)
}

/// A pawn's stat (`GetStatValue` on a pawn without equipment), for a
/// healthy pawn (every capacity at 1).
pub fn pawn_stat(defs: &GameDefs, race: &ThingDef, skills: &Skills, stat: &str) -> f32 {
    pawn_stat_with_capacities(defs, race, skills, stat, &|_| 1.0)
}

/// [`pawn_stat`] with the pawn's capacity levels (`capacityOffsets`).
pub fn pawn_stat_with_capacities(
    defs: &GameDefs,
    race: &ThingDef,
    skills: &Skills,
    stat: &str,
    capacity: &dyn Fn(&str) -> f32,
) -> f32 {
    pawn_stat_lit(defs, race, skills, stat, capacity, None)
}

/// [`pawn_stat_with_capacities`] for a pawn standing in `glow` light: stats
/// with a light part (`StatPart_Glow`: work speed, move speed) drop in the
/// dark.
pub fn pawn_stat_lit(
    defs: &GameDefs,
    race: &ThingDef,
    skills: &Skills,
    stat: &str,
    capacity: &dyn Fn(&str) -> f32,
    glow: Option<f32>,
) -> f32 {
    let humanlike = race.race.as_ref().and_then(|r| r.intelligence.as_deref()) == Some("Humanlike");
    let light = glow.map(|g| (g, humanlike));
    let mut v = base_value(defs, race, stat);
    let Some(s) = defs.stats.get(stat) else {
        return v;
    };
    for need in &s.skill_need_offsets {
        v += need.value_at(skills.level(need.skill()));
    }
    // `PawnCapacityOffset.GetOffset`: (min(level, max) − 1) × scale.
    for (cap, scale, max) in &s.capacity_offsets {
        v += (capacity(cap).min(*max) - 1.0) * scale;
    }
    for factor in &s.stat_factors {
        v *= pawn_stat_lit(defs, race, skills, factor, capacity, glow);
    }
    for need in &s.skill_need_factors {
        v *= need.value_at(skills.level(need.skill()));
    }
    v *= capacity_factor(s, capacity);
    finalize_lit(defs, stat, v, light)
}

/// The health part of a stat (`capacityFactors`): each factor applies as
/// Lerp(v, v × factor, weight); 1 for a healthy pawn.
pub fn capacity_factor(stat: &rimworld_defs::StatDef, capacity: &dyn Fn(&str) -> f32) -> f32 {
    stat.capacity_factors.iter().fold(1.0, |v, f| {
        let factor = f.factor(capacity(&f.capacity));
        v + (v * factor - v) * f.weight.clamp(0.0, 1.0)
    })
}

/// `StatPart_GearStatOffset`: the worn apparel's value of the part's
/// apparel stat, added (or subtracted).
// COMPATIBILITY TODO: currently approximate — apparel quality, hit points
// and `equippedStatOffsets` are not modelled; the part is added after the
// stat's other parts and clamps.
pub fn gear_offset(defs: &GameDefs, apparel: &[crate::pawn::WornApparel], stat: &str) -> f32 {
    let Some((apparel_stat, subtract)) = defs.stats.get(stat).and_then(|s| s.gear_offset.as_ref())
    else {
        return 0.0;
    };
    let sum: f32 = apparel
        .iter()
        .map(|a| {
            def_stat(
                defs,
                &defs.things[a.def],
                a.stuff.map(|s| &defs.things[s]),
                apparel_stat,
            )
        })
        .sum();
    if *subtract { -sum } else { sum }
}

/// A def's stat for a given stuff (`GetStatValueAbstract`).
pub fn def_stat(defs: &GameDefs, def: &ThingDef, stuff: Option<&ThingDef>, stat: &str) -> f32 {
    let mut v = base_value(defs, def, stat);
    if let Some(props) = stuff.and_then(|s| s.stuff_props.as_ref()) {
        if v > 0.0 {
            v *= props.stat_factors.get(stat).copied().unwrap_or(1.0);
        }
        v += props.stat_offsets.get(stat).copied().unwrap_or(0.0);
    }
    // `StatPart_Stuff`: the stuff's power × the thing's multiplier.
    if let (Some(stuff), Some((power, multiplier))) = (
        stuff,
        defs.stats.get(stat).and_then(|s| s.stuff_part.as_ref()),
    ) {
        v += base_value(defs, def, multiplier) * base_value(defs, stuff, power);
    }
    finalize(defs, stat, v)
}

/// A terrain's stat (`GetStatValueAbstract` on a floor: its stat base or
/// the stat's default).
pub fn terrain_stat(defs: &GameDefs, terrain: &rimworld_defs::TerrainDef, stat: &str) -> f32 {
    let v = terrain
        .stat_bases
        .get(stat)
        .copied()
        .or_else(|| defs.stats.get(stat).map(|s| s.default_base_value))
        .unwrap_or(1.0);
    finalize(defs, stat, v)
}

/// A floor's `costList`.
pub fn terrain_cost_list(
    defs: &GameDefs,
    terrain: &rimworld_defs::TerrainDef,
) -> Vec<(rimworld_defs::DefId<ThingDef>, u32)> {
    terrain
        .cost_list
        .iter()
        .filter_map(|(name, n)| Some((defs.things.id(name)?, *n)))
        .collect()
}

/// `CostListAdjusted`: the fixed costs plus round(costStuffCount / stuff
/// volume) of the stuff (at least 1), merged into an existing entry of
/// the same def.
// COMPATIBILITY TODO: currently approximate — difficulty cost factors are
// not applied.
pub fn cost_list(
    defs: &GameDefs,
    def: &ThingDef,
    stuff: Option<rimworld_defs::DefId<ThingDef>>,
) -> Vec<(rimworld_defs::DefId<ThingDef>, u32)> {
    let stuff_count = match stuff {
        Some(s) if def.made_from_stuff() => ((def.cost_stuff_count as f32
            / defs.things[s].volume_per_unit())
        .round_ties_even() as u32)
            .max(1),
        _ => 0,
    };
    let mut out: Vec<(rimworld_defs::DefId<ThingDef>, u32)> = Vec::new();
    let mut merged = false;
    for (name, count) in &def.cost_list {
        let Some(id) = defs.things.id(name) else {
            continue;
        };
        if Some(id) == stuff {
            out.push((id, count + stuff_count));
            merged = true;
        } else {
            out.push((id, *count));
        }
    }
    if let Some(s) = stuff
        && !merged
        && stuff_count > 0
    {
        out.push((s, stuff_count));
    }
    out
}
