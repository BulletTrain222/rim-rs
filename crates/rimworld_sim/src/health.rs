//! Health (docs/research.md §25): hediffs on body parts, part health and
//! efficiency, pawn capacities, pain, bleeding, damage, healing, and the
//! downed and death checks (`HediffSet`, `PawnCapacityUtility`,
//! `DamageWorker_AddInjury`, `Pawn_HealthTracker`).
//!
//! There is no single hit-point bar: injuries are hediffs on body parts, a
//! part with no health left is destroyed, and the pawn's state follows from
//! its capacities, pain and blood loss.

use rimworld_defs::health::{BodyDef, BodyPartDef, DamageDef, HediffDef, PartDepth};
use rimworld_defs::{DefId, GameDefs};
use serde::{Deserialize, Serialize};

use crate::rand::Rand;

/// Total injury severity that kills, per unit of health scale.
const LETHAL_DAMAGE_PER_HEALTH_SCALE: f32 = 150.0;
/// Consciousness a pawn needs to be awake (`CanBeAwake`).
const AWAKE_CONSCIOUSNESS: f32 = 0.3;
/// Default pain shock threshold (`PainShockThreshold` stat default).
pub const DEFAULT_PAIN_SHOCK_THRESHOLD: f32 = 0.8;
/// Injuries stop bleeding after this age, more for severe ones.
const BLEED_STOP_BASE_TICKS: i64 = 90_000;
/// A destroyed part stays "fresh" (bleeding, painful) this long.
const FRESH_MISSING_TICKS: i64 = 90_000;

/// A health condition (`Hediff`), possibly on a body part (an index into
/// the race's `BodyDef` parts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hediff {
    pub def: DefId<HediffDef>,
    pub part: Option<usize>,
    pub severity: f32,
    pub age_ticks: i64,
    /// Per-hediff state of its comps.
    #[serde(default)]
    pub comps: HediffComps,
}

/// `HediffComp_SeverityPerDay` and `HediffComp_Disappears` state, rolled
/// when the hediff is first ticked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct HediffComps {
    pub initialized: bool,
    pub severity_per_day: f32,
    /// Removed at this age (`disappearsAfterTicks`; 0 = never).
    pub disappears_after: i64,
    /// `HediffComp_TendDuration.tendTicksLeft`: tended while positive (a
    /// permanent tend keeps 1).
    pub tend_ticks_left: i32,
    /// `tendQuality` of the last tend.
    pub tend_quality: f32,
}

/// A pawn's health (`Pawn_HealthTracker` state).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub hediffs: Vec<Hediff>,
    pub downed: bool,
    pub dead: bool,
}

/// Health queries for one pawn.
pub struct HealthView<'a> {
    pub defs: &'a GameDefs,
    pub body: &'a BodyDef,
    pub hediffs: &'a [Hediff],
    /// `HealthScale` (life stage factor × race `baseHealthScale`).
    pub health_scale: f32,
    pub bleed_rate_factor: f32,
}

fn inverse_lerp(a: f32, b: f32, v: f32) -> f32 {
    if a != b {
        ((v - a) / (b - a)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// `GenMath.RoundedHundredth`.
fn rounded_hundredth(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

impl HealthView<'_> {
    /// `HediffSet.GetCoverageOfNotMissingNaturalParts(corePart)`: the
    /// summed own coverage of every part with no missing part at or above
    /// it, visited depth-first from the core (children pushed in order,
    /// so popped in reverse).
    // COMPATIBILITY TODO: currently approximate — added (artificial)
    // parts don't exist yet.
    pub fn coverage_of_not_missing_natural_parts(&self) -> f32 {
        let Some(core) = self.body.parts.iter().position(|p| p.parent.is_none()) else {
            return 0.0;
        };
        self.coverage_of_not_missing_natural_parts_from(core)
    }

    /// [`HealthView::coverage_of_not_missing_natural_parts`] from `root`
    /// (0 when that part itself is missing).
    pub fn coverage_of_not_missing_natural_parts_from(&self, core: usize) -> f32 {
        let parts = &self.body.parts;
        if self.part_is_missing(core) {
            return 0.0;
        }
        let rejected: Vec<usize> = self
            .hediffs
            .iter()
            .filter(|h| self.is_missing_hediff(h))
            .filter_map(|h| h.part)
            .collect();
        let mut sum = 0.0f32;
        let mut stack = vec![core];
        while let Some(p) = stack.pop() {
            sum += parts[p].coverage_abs;
            for (c, part) in parts.iter().enumerate() {
                if part.parent == Some(p) && !rejected.contains(&c) {
                    stack.push(c);
                }
            }
        }
        sum
    }

    /// `SummaryHealthHandler.SummaryHealthPercent` for a living pawn: ×
    /// (1 − min(impact, 0.95)) per hediff — an injury's severity / (75 ×
    /// health scale) — clamped to 0.05..1.
    // COMPATIBILITY TODO: currently approximate — freshly missing, bleeding
    // extremities' impact (part hit points / (75 × health scale)) is not
    // counted (parts don't track freshness); permanent injuries don't exist.
    pub fn summary_health_percent(&self) -> f32 {
        let mut v = 1.0f32;
        for h in self.hediffs {
            if self.hediff_def(h).is_injury() {
                let impact = (h.severity / (75.0 * self.health_scale)).min(0.95);
                v *= 1.0 - impact;
            }
        }
        v.clamp(0.05, 1.0)
    }

    /// `HediffSet.GetNotMissingParts`: parts in body order whose part and
    /// ancestors are all present.
    pub fn not_missing_parts(&self) -> Vec<usize> {
        (0..self.body.parts.len())
            .filter(|&p| !self.missing(p))
            .collect()
    }

    pub fn part_def(&self, part: usize) -> Option<&BodyPartDef> {
        self.defs.body_parts.get(&self.body.parts[part].def)
    }

    fn hediff_def(&self, h: &Hediff) -> &HediffDef {
        &self.defs.hediffs[h.def]
    }

    fn is_missing_hediff(&self, h: &Hediff) -> bool {
        self.hediff_def(h).hediff_class.as_deref() == Some("Hediff_MissingPart")
    }

    /// `GetMaxHealth` of a part.
    pub fn max_health(&self, part: usize) -> f32 {
        self.part_def(part)
            .map_or(10.0, |d| d.max_health(self.health_scale))
    }

    /// The part or an ancestor is destroyed (`GetNotMissingParts`).
    pub fn missing(&self, part: usize) -> bool {
        self.hediffs.iter().any(|h| {
            self.is_missing_hediff(h)
                && h.part
                    .is_some_and(|p| self.body.is_ancestor_or_self(p, part))
        })
    }

    /// `PartIsMissing`: a missing-part hediff on exactly this part.
    fn part_is_missing(&self, part: usize) -> bool {
        self.hediffs
            .iter()
            .any(|h| h.part == Some(part) && self.is_missing_hediff(h))
    }

    /// `GetPartHealth`: max health minus injuries on the part, rounded to a
    /// whole number (`Mathf.RoundToInt`, ties to even).
    // COMPATIBILITY TODO: currently approximate — social-fight injuries
    // (which never take a part below 1) are not modelled.
    pub fn part_health(&self, part: usize) -> f32 {
        if self.part_is_missing(part) {
            return 0.0;
        }
        let mut hp = self.max_health(part);
        for h in self.hediffs {
            if h.part == Some(part) && self.hediff_def(h).is_injury() {
                hp -= h.severity;
            }
        }
        hp = hp.max(0.0);
        if !self.part_def(part).is_none_or(|d| d.destroyable_by_damage) {
            hp = hp.max(1.0);
        }
        hp.round_ties_even()
    }

    /// `CalculatePartEfficiency` (no added parts or stage offsets).
    // COMPATIBILITY TODO: currently approximate — prosthetics and hediff
    // stages' part efficiency offsets are not modelled.
    pub fn part_efficiency(&self, part: usize) -> f32 {
        if let Some(parent) = self.body.parts[part].parent
            && self.part_is_missing(parent)
        {
            return 0.0;
        }
        let mut frac = self.part_health(part) / self.max_health(part);
        if frac != 1.0
            && self.body.parts[part].depth == PartDepth::Outside
            && self.body.parts[part].parent.is_some()
        {
            frac = inverse_lerp(0.1, 1.0, frac);
        }
        frac.max(0.0)
    }

    /// `CalculateTagEfficiency`: the mean efficiency of parts with the tag
    /// (1 without such parts), capped at `max`.
    pub fn tag_efficiency(&self, tag: &str, max: f32) -> f32 {
        self.tag_efficiency_with(tag, max, None, None)
    }

    /// [`HealthView::tag_efficiency`] with a lerp range for the mean and a
    /// best-part weight (with two or more parts, the best part counts
    /// that much and the mean of the rest the remainder).
    pub fn tag_efficiency_with(
        &self,
        tag: &str,
        max: f32,
        lerp_range: Option<(f32, f32)>,
        best_part_weight: Option<f32>,
    ) -> f32 {
        let parts: Vec<f32> = self
            .body
            .parts_with_tag(&self.defs.body_parts, tag)
            .map(|p| self.part_efficiency(p))
            .collect();
        if parts.is_empty() {
            return 1.0;
        }
        let sum: f32 = parts.iter().sum();
        let best = parts.iter().copied().fold(0.0f32, f32::max);
        let n = parts.len() as f32;
        let mut v = match best_part_weight {
            Some(w) if parts.len() >= 2 => best * w + (sum - best) / (n - 1.0) * (1.0 - w),
            _ => sum / n,
        };
        if let Some((a, b)) = lerp_range {
            v = lerp(a, b, v.clamp(0.0, 1.0));
        }
        v.min(max)
    }

    /// Parts below (and including) `part` with the tag (`GetChildParts`).
    fn child_parts_with_tag(&self, part: usize, tag: &str) -> Vec<usize> {
        self.body
            .subtree(part)
            .into_iter()
            .filter(|&p| {
                self.part_def(p)
                    .is_some_and(|d| d.tags.iter().any(|t| t == tag))
            })
            .collect()
    }

    /// `CalculateLimbEfficiency`: per limb core, its efficiency times its
    /// connected segments', with digits weighted in; returns the mean and
    /// the fraction of functional limbs.
    pub fn limb_efficiency(
        &self,
        core: &str,
        segment: &str,
        digit: &str,
        weight: f32,
    ) -> (f32, f32) {
        let cores: Vec<usize> = self
            .body
            .parts_with_tag(&self.defs.body_parts, core)
            .collect();
        if cores.is_empty() {
            return (0.0, 0.0);
        }
        let (mut sum, mut working) = (0.0, 0);
        for &c in &cores {
            let mut e = self.part_efficiency(c);
            // `GetConnectedParts`: climb while the parent is a segment, then
            // every segment below.
            let mut top = c;
            while let Some(p) = self.body.parts[top].parent {
                if self
                    .part_def(p)
                    .is_some_and(|d| d.tags.iter().any(|t| t == segment))
                {
                    top = p;
                } else {
                    break;
                }
            }
            for s in self.child_parts_with_tag(top, segment) {
                e *= self.part_efficiency(s);
            }
            let digits = self.child_parts_with_tag(c, digit);
            if !digits.is_empty() {
                let avg = digits.iter().map(|&d| self.part_efficiency(d)).sum::<f32>()
                    / digits.len() as f32;
                e = lerp(e, e * avg, weight);
            }
            sum += e;
            if e > 0.0 {
                working += 1;
            }
        }
        (
            sum / cores.len() as f32,
            working as f32 / cores.len() as f32,
        )
    }

    /// `HediffSet.PainTotal`: injuries, fresh missing parts and stage pain
    /// offsets, times the stages' pain factors, clamped to 0..1.
    // COMPATIBILITY TODO: currently approximate — traits and genes are not
    // modelled.
    pub fn pain_total(&self) -> f32 {
        let mut pain = 0.0;
        for h in self.hediffs {
            let def = self.hediff_def(h);
            if let Some(stage) = def.stage_at(h.severity) {
                pain += stage.pain_offset;
            }
            let Some(inj) = def.injury.as_ref() else {
                continue;
            };
            if def.is_injury() {
                pain += h.severity * inj.pain_per_severity / self.health_scale;
            } else if self.is_missing_hediff(h) && self.fresh_missing(h) {
                let part = h.part.expect("missing parts have a part");
                pain += self.max_health(part) * inj.pain_per_severity / self.health_scale;
            }
        }
        // Stages' `painFactor` (anesthetic numbs pain).
        for h in self.hediffs {
            if let Some(stage) = self.hediff_def(h).stage_at(h.severity) {
                pain *= stage.pain_factor;
            }
        }
        pain.clamp(0.0, 1.0)
    }

    fn fresh_missing(&self, h: &Hediff) -> bool {
        let Some(part) = h.part else { return false };
        let parent_missing = self.body.parts[part]
            .parent
            .is_some_and(|p| self.part_is_missing(p));
        h.age_ticks < FRESH_MISSING_TICKS
            && !self.is_tended(h)
            && self.body.parts[part].depth != PartDepth::Inside
            && !self.part_def(part).is_some_and(|d| d.solid)
            && !parent_missing
    }

    /// `HediffSet.GetHungerRateFactor`: the product of the stages' hunger
    /// factors plus their offsets, at least 0.
    pub fn hunger_rate_factor(&self) -> f32 {
        let stages: Vec<_> = self
            .hediffs
            .iter()
            .filter_map(|h| self.hediff_def(h).stage_at(h.severity))
            .collect();
        let product: f32 = stages.iter().map(|st| st.hunger_rate_factor).product();
        let offsets: f32 = stages.iter().map(|st| st.hunger_rate_factor_offset).sum();
        (product + offsets).max(0.0)
    }

    /// `HediffSet.BleedRateTotal`.
    pub fn bleed_rate_total(&self) -> f32 {
        let total: f32 = self.hediffs.iter().map(|h| self.bleed_rate(h)).sum();
        total / self.health_scale
    }

    /// One hediff's `BleedRate`: an untended injury on a soft part until
    /// it stops by age, or a fresh missing extremity.
    pub fn bleed_rate(&self, h: &Hediff) -> f32 {
        let def = self.hediff_def(h);
        let Some(inj) = def.injury.as_ref() else {
            return 0.0;
        };
        let Some(part) = h.part else { return 0.0 };
        let pdef = self.part_def(part);
        if def.is_injury() {
            let stop = BLEED_STOP_BASE_TICKS
                + lerp(0.0, 90_000.0, inverse_lerp(1.0, 30.0, h.severity)).round() as i64;
            if pdef.is_some_and(|d| d.solid) || h.age_ticks >= stop || self.is_tended(h) {
                return 0.0;
            }
            h.severity
                * inj.bleed_rate
                * self.bleed_rate_factor
                * pdef.map_or(1.0, |d| d.bleed_rate)
        } else if self.is_missing_hediff(h) && self.fresh_missing(h) {
            self.max_health(part)
                * inj.bleed_rate
                * pdef.map_or(1.0, |d| d.bleed_rate)
                * self.bleed_rate_factor
        } else {
            0.0
        }
    }

    /// `IsTended`.
    pub fn is_tended(&self, h: &Hediff) -> bool {
        self.hediff_def(h).tend_duration.is_some() && h.comps.tend_ticks_left > 0
    }

    /// `TendableNow`: a tendable hediff with severity whose tend has run
    /// out (into the overlap), or a fresh missing extremity.
    // COMPATIBILITY TODO: currently approximate — visibility, immunity and
    // permanent injuries (scars) are not modelled.
    pub fn tendable_now(&self, h: &Hediff) -> bool {
        let def = self.hediff_def(h);
        if self.is_missing_hediff(h) {
            return self.fresh_missing(h);
        }
        if !def.tendable || h.severity <= 0.0 {
            return false;
        }
        match def.tend_duration {
            Some(t) if t.permanent() => !self.is_tended(h),
            Some(t) => t.tend_ticks_overlap() > h.comps.tend_ticks_left,
            None => true,
        }
    }

    /// `TendPriority`: 1 for a life-threatening stage, else 1.5 × the
    /// bleed rate, at least 0.025 for a hediff that heals with tending.
    pub fn tend_priority(&self, h: &Hediff) -> f32 {
        let def = self.hediff_def(h);
        let mut p: f32 = 0.0;
        if def.stage_at(h.severity).is_some_and(|s| s.life_threatening) {
            p = 1.0;
        }
        p = p.max(self.bleed_rate(h) * 1.5);
        if def
            .tend_duration
            .is_some_and(|t| t.severity_per_day_tended < 0.0)
        {
            p = p.max(0.025);
        }
        p
    }

    /// `HasHediffsNeedingTend`.
    pub fn needs_tending(&self) -> bool {
        self.hediffs.iter().any(|h| self.tendable_now(h))
    }

    /// `HasTendedAndHealingInjury`.
    pub fn has_tended_and_healing_injury(&self) -> bool {
        self.hediffs
            .iter()
            .any(|h| self.hediff_def(h).is_injury() && self.is_tended(h) && h.severity > 0.0)
    }

    /// `TicksUntilDeathDueToBloodLoss`.
    pub fn ticks_until_death_by_blood_loss(&self) -> i32 {
        let bleed = self.bleed_rate_total();
        if bleed < 0.0001 {
            return i32::MAX;
        }
        let loss = self
            .hediffs
            .iter()
            .find(|h| self.hediff_def(h).def_name == "BloodLoss")
            .map_or(0.0, |h| h.severity);
        ((1.0 - loss) / bleed * 60_000.0) as i32
    }

    /// The capacity worker's level before hediff modifiers.
    // COMPATIBILITY TODO: currently approximate — the remaining capacities
    // (e.g. the Biotech/Anomaly ones) count as fully working.
    fn worker_level(&self, cap: &str) -> f32 {
        match cap {
            "Consciousness" => {
                let mut c = self.tag_efficiency("ConsciousnessSource", f32::MAX);
                let pain = (lerp_double(0.1, 1.0, 0.0, 0.4, self.pain_total())).clamp(0.0, 0.4);
                if pain >= 0.01 {
                    c -= pain;
                }
                c = lerp(c, c * self.capacity("BloodPumping").min(1.0), 0.2);
                c = lerp(c, c * self.capacity("Breathing").min(1.0), 0.2);
                lerp(c, c * self.capacity("BloodFiltration").min(1.0), 0.1)
            }
            "Moving" => {
                let (e, functional) = self.limb_efficiency(
                    "MovingLimbCore",
                    "MovingLimbSegment",
                    "MovingLimbDigit",
                    0.4,
                );
                if functional < 0.4999 {
                    return 0.0;
                }
                let mut m = e
                    * self.tag_efficiency("Pelvis", f32::MAX)
                    * self.tag_efficiency("Spine", f32::MAX);
                m = lerp(m, m * self.capacity("Breathing"), 0.2);
                m = lerp(m, m * self.capacity("BloodPumping"), 0.2);
                m * self.capacity("Consciousness").min(1.0)
            }
            "Manipulation" => {
                let (e, _) = self.limb_efficiency(
                    "ManipulationLimbCore",
                    "ManipulationLimbSegment",
                    "ManipulationLimbDigit",
                    0.8,
                );
                e * self.capacity("Consciousness")
            }
            "BloodPumping" => self.tag_efficiency("BloodPumpingSource", f32::MAX),
            "Sight" => self.tag_efficiency_with("SightSource", f32::MAX, None, Some(0.75)),
            "Hearing" => self.tag_efficiency_with("HearingSource", f32::MAX, None, Some(0.75)),
            "Metabolism" => self.tag_efficiency("MetabolismSource", f32::MAX),
            "Eating" => {
                self.tag_efficiency("EatingSource", f32::MAX)
                    * self.tag_efficiency("EatingPathway", 1.0)
                    * self.tag_efficiency_with("Tongue", f32::MAX, Some((0.5, 1.0)), None)
                    * self.capacity("Consciousness")
            }
            "Talking" => {
                self.tag_efficiency("TalkingSource", f32::MAX)
                    * self.tag_efficiency("TalkingPathway", 1.0)
                    * self.tag_efficiency("Tongue", 1.0)
                    * self.capacity("Consciousness")
            }
            "Breathing" => {
                self.tag_efficiency("BreathingSource", f32::MAX)
                    * self.tag_efficiency("BreathingPathway", 1.0)
                    * self.tag_efficiency("BreathingSourceCage", 1.0)
            }
            "BloodFiltration" => {
                let kidney = self
                    .body
                    .parts_with_tag(&self.defs.body_parts, "BloodFiltrationKidney")
                    .next()
                    .is_some();
                if kidney {
                    self.tag_efficiency("BloodFiltrationKidney", f32::MAX)
                        * self.tag_efficiency("BloodFiltrationLiver", f32::MAX)
                } else {
                    self.tag_efficiency("BloodFiltrationSource", f32::MAX)
                }
            }
            _ => 1.0,
        }
    }

    /// `PawnCapacityUtility.CalculateCapacityLevel`: the worker's level with
    /// the hediffs' capacity modifiers, the capacity's minimum, rounded to
    /// hundredths.
    pub fn capacity(&self, cap: &str) -> f32 {
        let def = self.defs.capacities.get(cap);
        if def.is_some_and(|d| d.zero_if_cannot_be_awake) && !self.can_be_awake() {
            return 0.0;
        }
        let mut level = self.worker_level(cap);
        if level > 0.0 {
            let (mut set_max, mut factor) = (99_999.0f32, 1.0f32);
            for h in self.hediffs {
                let Some(stage) = self.hediff_def(h).stage_at(h.severity) else {
                    continue;
                };
                for m in stage.cap_mods.iter().filter(|m| m.capacity == cap) {
                    level += m.offset;
                    factor *= m.post_factor;
                    set_max = set_max.min(m.set_max);
                }
            }
            level = (level * factor).min(set_max);
        }
        rounded_hundredth(level.max(def.map_or(0.0, |d| d.min_value)))
    }

    /// `CapableOf`.
    pub fn capable_of(&self, cap: &str) -> bool {
        let min = self
            .defs
            .capacities
            .get(cap)
            .map_or(0.0, |d| d.min_for_capable);
        self.capacity(cap) > min
    }

    /// `CanBeAwake`.
    pub fn can_be_awake(&self) -> bool {
        self.capacity("Consciousness") >= AWAKE_CONSCIOUSNESS
    }

    /// `ShouldBeDowned`: in pain shock, unable to be awake, or unable to
    /// move.
    pub fn should_be_downed(&self, pain_shock_threshold: f32) -> bool {
        self.pain_total() >= pain_shock_threshold
            || !self.can_be_awake()
            || !self.capable_of("Moving")
    }

    /// `ShouldBeDead`: a hediff at its lethal severity, a lethal capacity
    /// lost, a destroyed core part, or total injury severity past the
    /// lethal threshold.
    pub fn should_be_dead(&self) -> bool {
        if self.hediffs.iter().any(|h| {
            let d = self.hediff_def(h);
            d.lethal_severity > 0.0 && h.severity >= d.lethal_severity
        }) {
            return true;
        }
        for (_, cap) in self.defs.capacities.iter() {
            if cap.lethal_flesh && !self.capable_of(&cap.def_name) {
                return true;
            }
        }
        if !self.body.parts.is_empty() && self.part_efficiency(0) <= 0.0001 {
            return true;
        }
        let injuries: f32 = self
            .hediffs
            .iter()
            .filter(|h| self.hediff_def(h).is_injury())
            .map(|h| h.severity)
            .sum();
        injuries >= LETHAL_DAMAGE_PER_HEALTH_SCALE * self.health_scale
    }

    /// `GetRandomNotMissingPart` (height and depth undefined): weighted by
    /// each part's own coverage.
    // COMPATIBILITY TODO: currently approximate — damage-specific hit
    // chance factors and height/depth restrictions are not modelled.
    pub fn random_not_missing_part(
        &self,
        depth: Option<PartDepth>,
        rng: &mut Rand,
    ) -> Option<usize> {
        let parts: Vec<usize> = (0..self.body.parts.len())
            .filter(|&p| {
                !self.missing(p)
                    && self.body.parts[p].coverage_abs > 0.0
                    && depth.is_none_or(|d| self.body.parts[p].depth == d)
            })
            .collect();
        let weights: Vec<f32> = parts
            .iter()
            .map(|&p| self.body.parts[p].coverage_abs)
            .collect();
        crate::region::random_element_by_weight(&weights, rng).map(|i| parts[i])
    }
}

/// `GenMath.LerpDouble`.
fn lerp_double(in_from: f32, in_to: f32, out_from: f32, out_to: f32, x: f32) -> f32 {
    if in_from == in_to {
        return out_from;
    }
    out_from + (out_to - out_from) * ((x - in_from) / (in_to - in_from))
}

/// What one application of damage did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DamageResult {
    pub parts_hit: Vec<usize>,
    pub total_damage: f32,
    pub destroyed: Vec<usize>,
    /// Armor stopped the hit.
    pub deflected: bool,
}

impl Health {
    /// Removes a body part (a `MissingBodyPart` on it; its and its
    /// children's hediffs go), e.g. a corpse part eaten.
    pub fn remove_part(&mut self, defs: &GameDefs, body: &BodyDef, part: usize) {
        let mut result = DamageResult::default();
        self.destroy_part(defs, body, part, &mut result);
    }

    /// `HediffSet.AddDirect` for an injury: merge or add, then destroy the
    /// part if it has no health left (not the core part).
    fn add_injury(
        &mut self,
        view: &dyn Fn(&[Hediff]) -> f32,
        defs: &GameDefs,
        body: &BodyDef,
        h: Hediff,
        result: &mut DamageResult,
    ) {
        let part = h.part;
        let can_merge = defs.hediffs[h.def]
            .injury
            .as_ref()
            .is_some_and(|i| i.can_merge);
        if let Some(existing) = self
            .hediffs
            .iter_mut()
            .find(|e| can_merge && e.def == h.def && e.part == h.part)
        {
            existing.severity += h.severity;
        } else {
            self.hediffs.push(h);
        }
        if let Some(p) = part
            && p != 0
            && view(&self.hediffs) <= 0.0
        {
            self.destroy_part(defs, body, p, result);
        }
    }

    /// A part with no health left is destroyed (`Hediff_MissingPart`): its
    /// and its children's hediffs go.
    fn destroy_part(
        &mut self,
        defs: &GameDefs,
        body: &BodyDef,
        part: usize,
        result: &mut DamageResult,
    ) {
        let Some(missing) = defs.hediffs.id("MissingBodyPart") else {
            return;
        };
        let subtree = body.subtree(part);
        self.hediffs
            .retain(|h| !h.part.is_some_and(|p| subtree.contains(&p)));
        self.hediffs.push(Hediff {
            def: missing,
            part: Some(part),
            severity: 0.0,
            age_ticks: 0,
            comps: Default::default(),
        });
        result.destroyed.push(part);
    }
}

/// The hediff a damage adds to a part (`HealthUtility.GetHediffDefFromDamage`).
fn hediff_for(defs: &GameDefs, damage: &DamageDef, part: &BodyPartDef) -> Option<DefId<HediffDef>> {
    let name = if part.skin_covered && damage.hediff_skin.is_some() {
        damage.hediff_skin.as_deref()
    } else if part.solid && damage.hediff_solid.is_some() {
        damage.hediff_solid.as_deref()
    } else {
        damage.hediff.as_deref()
    };
    defs.hediffs.id(name?)
}

/// `DamageWorker_AddInjury.ApplyToPawn` without armor: choose a part (or
/// use `part`), keep outside non-core parts from being destroyed unless
/// the overkill roll allows it, add the injury, and for damage that harms
/// every layer, injure the outer parents too.
// COMPATIBILITY TODO: currently approximate — armor, damage propagation
// over several parts, the Cut/Blunt/Scratch/Bite workers' special effects,
// stun, colonist instant-kill protection and permanent injuries are not
// modelled.
#[allow(clippy::too_many_arguments)]
pub fn apply_damage(
    health: &mut Health,
    defs: &GameDefs,
    body: &BodyDef,
    health_scale: f32,
    bleed_rate_factor: f32,
    damage: DefId<DamageDef>,
    amount: f32,
    part: Option<usize>,
    depth: Option<PartDepth>,
    armor: &Armor,
    rng: &mut Rand,
) -> DamageResult {
    let mut result = DamageResult::default();
    if amount <= 0.0 || health.dead {
        return result;
    }
    let mut dd = &defs.damages[damage];
    fn mk<'v>(
        defs: &'v GameDefs,
        body: &'v BodyDef,
        h: &'v [Hediff],
        scale: f32,
        bleed: f32,
    ) -> HealthView<'v> {
        HealthView {
            defs,
            body,
            hediffs: h,
            health_scale: scale,
            bleed_rate_factor: bleed,
        }
    }
    macro_rules! view {
        ($h:expr) => {
            mk(defs, body, $h, health_scale, bleed_rate_factor)
        };
    }
    let hit = match part {
        Some(p) if !view!(&health.hediffs).missing(p) => p,
        Some(_) => return result,
        None => match view!(&health.hediffs).random_not_missing_part(depth, rng) {
            Some(p) => p,
            None => return result,
        },
    };
    let Some(pdef) = defs.body_parts.get(&body.parts[hit].def) else {
        return result;
    };
    // `ArmorUtility.GetPostArmorDamage`.
    let mut dmg = amount;
    if let Some(cat) = dd.armor_category.clone() {
        let groups = &body.parts[hit].groups;
        let mut blunted = false;
        for layer in armor.layers.iter().rev() {
            if !layer.groups.iter().any(|g| groups.contains(g)) {
                continue;
            }
            // The apparel takes a quarter of the damage (its hit points are
            // not modelled; the roll still happens).
            let _ = crate::plant::round_random(dmg * 0.25, rng);
            blunted |= apply_armor(&mut dmg, armor.penetration, layer.rating(&cat), &cat, rng);
            if dmg < 0.001 {
                result.deflected = true;
                return result;
            }
        }
        blunted |= apply_armor(
            &mut dmg,
            armor.penetration,
            armor.natural.rating(&cat),
            &cat,
            rng,
        );
        if dmg < 0.001 {
            result.deflected = true;
            return result;
        }
        if blunted && let Some(b) = defs.damages.id("Blunt") {
            dd = &defs.damages[b];
        }
    }
    // `ReduceDamageToPreserveOutsideParts`.
    let rec = &body.parts[hit];
    if rec.depth == PartDepth::Outside && rec.parent.is_some() {
        let hp = view!(&health.hediffs).part_health(hit);
        if dmg >= hp {
            let over = (dmg - hp) / pdef.max_health(health_scale);
            let (lo, hi) = dd.overkill_pct_to_destroy_part;
            if !rng.chance(inverse_lerp(lo, hi, over)) {
                dmg = hp - 1.0;
            }
        }
    }
    let Some(hdef) = hediff_for(defs, dd, pdef) else {
        return result;
    };
    let part_health_before = view!(&health.hediffs).part_health(hit);
    health.add_injury(
        &|h| view!(h).part_health(hit),
        defs,
        body,
        Hediff {
            def: hdef,
            part: Some(hit),
            severity: dmg,
            age_ticks: 0,
            comps: Default::default(),
        },
        &mut result,
    );
    result.parts_hit.push(hit);
    result.total_damage += dmg.min(part_health_before);
    // `CheckDuplicateDamageToOuterParts`.
    if dd.harm_all_layers_until_outside && rec.depth == PartDepth::Inside {
        let mut parent = rec.parent;
        while let Some(p) = parent {
            let v = view!(&health.hediffs);
            if v.part_health(p) != 0.0 && body.parts[p].coverage_abs > 0.0 {
                let pd = defs.body_parts.get(&body.parts[p].def);
                if let Some(hd) = pd.and_then(|pd| hediff_for(defs, dd, pd)) {
                    health.add_injury(
                        &|h| view!(h).part_health(p),
                        defs,
                        body,
                        Hediff {
                            def: hd,
                            part: Some(p),
                            severity: dmg.max(1.0),
                            age_ticks: 0,
                            comps: Default::default(),
                        },
                        &mut result,
                    );
                    result.parts_hit.push(p);
                }
            }
            if body.parts[p].depth == PartDepth::Outside {
                break;
            }
            parent = body.parts[p].parent;
        }
    }
    result
}

/// What the downed/dead checks depend on: each hediff's def, part and
/// stage, injuries' severities (pain, part health) and whether it is at
/// its lethal severity. The game re-checks the pawn's state only when a
/// hediff changes this way (`Notify_HediffChanged`); equal keys mean the
/// check would give the same answer.
pub fn state_key(health: &Health, defs: &GameDefs) -> Vec<StateKeyEntry> {
    health
        .hediffs
        .iter()
        .map(|h| {
            let d = &defs.hediffs[h.def];
            StateKeyEntry {
                def: h.def,
                part: h.part,
                stage: d.stages.iter().rposition(|s| h.severity >= s.min_severity),
                injury_severity: d.is_injury().then_some(h.severity.to_bits()),
                lethal: d.lethal_severity > 0.0 && h.severity >= d.lethal_severity,
            }
        })
        .collect()
}

/// One hediff's part of [`state_key`].
#[derive(Debug, Clone, PartialEq)]
pub struct StateKeyEntry {
    def: DefId<HediffDef>,
    part: Option<usize>,
    stage: Option<usize>,
    injury_severity: Option<u32>,
    lethal: bool,
}

/// Malnutrition change per need interval for a pawn
/// (`Need_Food.MalnutritionSeverityPerInterval`): 0.453 a day, scaled
/// per pawn between 0.8 and 1.2 by its thing id.
pub fn malnutrition_per_interval(id_number: i32) -> f32 {
    let t = Rand::value_seeded(id_number ^ 0x0026_EF7A);
    0.001_132_5 * (0.8 + 0.4 * t)
}

/// `HealthUtility.AdjustSeverity`: changes the severity of the pawn's
/// (first) hediff of `def`, adding it when the change is positive.
/// Severity floors at 0; a hediff at 0 goes at the next health interval.
pub fn adjust_severity(health: &mut Health, def: DefId<HediffDef>, change: f32) {
    match health.hediffs.iter_mut().find(|h| h.def == def) {
        Some(h) => h.severity = (h.severity + change).max(0.0),
        None if change > 0.0 => health.hediffs.push(Hediff {
            def,
            part: None,
            severity: change,
            age_ticks: 0,
            comps: Default::default(),
        }),
        None => {}
    }
}

/// `HediffGiver_Heat.TemperatureOverageAdjustmentCurve`.
const OVERAGE_CURVE: [(f32, f32); 7] = [
    (0.0, 0.0),
    (25.0, 25.0),
    (50.0, 40.0),
    (100.0, 60.0),
    (200.0, 80.0),
    (400.0, 100.0),
    (4000.0, 1000.0),
];

fn overage_adjusted(x: f32) -> f32 {
    let c = &OVERAGE_CURVE;
    if x <= c[0].0 {
        return c[0].1;
    }
    for w in c.windows(2) {
        if x <= w[1].0 {
            let t = (x - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + (w[1].1 - w[0].1) * t;
        }
    }
    c[c.len() - 1].1
}

/// `HediffGiver_Hypothermia` then `HediffGiver_Heat`, on the 60-tick giver
/// interval, for a pawn at `ambient` with comfortable range `comfy`
/// (`ComfyTemperatureMin` / `Max`); the safe range is 10 wider each side.
/// Returns the body part frostbite strikes, if it does.
// COMPATIBILITY TODO: currently approximate — insectoids' hypothermic
// slowdown, heated terrain and burns from air hotter than comfy max + 150
// are not modelled.
pub fn temperature_givers(
    health: &mut Health,
    defs: &GameDefs,
    body: &BodyDef,
    ambient: f32,
    comfy: (f32, f32),
    rng: &mut Rand,
) -> Option<usize> {
    let mut frostbite = None;
    let safe = (comfy.0 - 10.0, comfy.1 + 10.0);
    let recover = |h: &mut Hediff| {
        h.severity -= (h.severity * 0.027).clamp(0.0015, 0.015);
    };
    if let Some(hypo) = defs.hediffs.id("Hypothermia") {
        let had = health.hediffs.iter().any(|h| h.def == hypo);
        if ambient < safe.0 {
            let change = ((ambient - safe.0).abs() * 6.45e-5).max(0.00075);
            adjust_severity(health, hypo, change);
        }
        if had && let Some(h) = health.hediffs.iter_mut().find(|h| h.def == hypo) {
            if ambient > comfy.0 {
                recover(h);
            } else if ambient < 0.0 && h.severity > 0.37 {
                let chance = 0.025 * h.severity;
                if rng.value() < chance {
                    let parts: Vec<usize> = (0..body.parts.len())
                        .filter(|&p| {
                            defs.body_parts
                                .get(&body.parts[p].def)
                                .is_some_and(|d| d.frostbite_vulnerability > 0.0)
                        })
                        .filter(|&p| {
                            !health.hediffs.iter().any(|h| {
                                h.part == Some(p)
                                    && defs.hediffs[h.def].hediff_class.as_deref()
                                        == Some("Hediff_MissingPart")
                            })
                        })
                        .collect();
                    let weights: Vec<f32> = parts
                        .iter()
                        .map(|&p| {
                            defs.body_parts
                                .get(&body.parts[p].def)
                                .map_or(0.0, |d| d.frostbite_vulnerability)
                        })
                        .collect();
                    frostbite =
                        crate::region::random_element_by_weight(&weights, rng).map(|k| parts[k]);
                }
            }
        }
    }
    if let Some(heat) = defs.hediffs.id("Heatstroke") {
        let had = health.hediffs.iter().any(|h| h.def == heat);
        if ambient > safe.1 {
            let change = (overage_adjusted(ambient - safe.1) * 6.45e-5).max(0.000375);
            adjust_severity(health, heat, change);
        } else if had
            && ambient < comfy.1
            && let Some(h) = health.hediffs.iter_mut().find(|h| h.def == heat)
        {
            recover(h);
        }
    }
    health.hediffs.retain(|h| {
        defs.hediffs[h.def].hediff_class.as_deref() == Some("Hediff_MissingPart")
            || h.severity > 0.0
    });
    frostbite
}

/// `TendUtility.DoTend` without medicine: the most urgent tendable hediff
/// (by tend priority, then severity) gets a tend of `quality` ± 0.25,
/// clamped to `max_quality`. Returns the tended hediff's index.
// COMPATIBILITY TODO: currently approximate — medicine (several injuries
// per tend) and `tendAllAtOnce` are not modelled.
#[allow(clippy::too_many_arguments)]
pub fn tend(
    health: &mut Health,
    defs: &GameDefs,
    body: &BodyDef,
    health_scale: f32,
    bleed_rate_factor: f32,
    quality: f32,
    max_quality: f32,
    rng: &mut Rand,
) -> Option<usize> {
    let view = HealthView {
        defs,
        body,
        hediffs: &health.hediffs,
        health_scale,
        bleed_rate_factor,
    };
    let mut order: Vec<(usize, f32, f32)> = health
        .hediffs
        .iter()
        .enumerate()
        .filter(|(_, h)| view.tendable_now(h))
        .map(|(i, h)| (i, view.tend_priority(h), h.severity))
        .collect();
    // `SortByDescending` (priority, then severity; .NET `List.Sort`).
    crate::netsort::sort(&mut order, |a, b| {
        b.1.total_cmp(&a.1).then(b.2.total_cmp(&a.2))
    });
    let &(pick, _, _) = order.first()?;
    let h = &mut health.hediffs[pick];
    // `HediffComp_TendDuration.CompTended`.
    h.comps.tend_quality = (quality + rng.range_f32(-0.25, 0.25)).clamp(0.0, max_quality);
    if let Some(t) = defs.hediffs[h.def].tend_duration {
        h.comps.tend_ticks_left = if t.permanent() {
            1
        } else {
            h.comps.tend_ticks_left.max(0) + t.tend_ticks_full()
        };
    }
    Some(pick)
}

/// What stands between a hit and the body (`ArmorUtility`): worn apparel
/// from the skin outwards, the pawn's own armor, and the attack's armor
/// penetration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Armor {
    pub layers: Vec<ArmorLayer>,
    pub natural: ArmorLayer,
    pub penetration: f32,
}

/// One piece of armor: the body part groups it covers and its ratings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArmorLayer {
    pub groups: Vec<String>,
    pub sharp: f32,
    pub blunt: f32,
    pub heat: f32,
}

impl ArmorLayer {
    fn rating(&self, category: &str) -> f32 {
        match category {
            "Sharp" => self.sharp,
            "Blunt" => self.blunt,
            "Heat" => self.heat,
            _ => 0.0,
        }
    }
}

/// `ArmorUtility.ApplyArmor`: with r = max(rating − penetration, 0), a roll
/// under r/2 stops the hit, under r halves it (randomly rounded; a sharp
/// hit becomes blunt). Returns whether it turned blunt.
fn apply_armor(
    dmg: &mut f32,
    penetration: f32,
    rating: f32,
    category: &str,
    rng: &mut Rand,
) -> bool {
    let r = (rating - penetration).max(0.0);
    let roll = rng.value();
    if roll < r * 0.5 {
        *dmg = 0.0;
    } else if roll < r {
        *dmg = crate::plant::round_random(*dmg / 2.0, rng) as f32;
        return category == "Sharp";
    }
    false
}

/// Hediff ageing, natural and tended healing (every 600 ticks), the
/// chance to drop blood, then blood loss from bleeding (every 60 ticks), in
/// the game's order (`HealthTickInterval`), for an interval of `delta`
/// ticks. `bleed_tick` / `heal_tick` say whether those hash intervals came
/// up; `bed_heal_per_day` is the bed's `bed_healPerDay` when lying in one;
/// `blood_body_size` is the body size of a race that bleeds filth.
/// Returns whether the pawn drops blood filth.
// COMPATIBILITY TODO: currently approximate — infections, scars, stages'
// naturalHealingFactor and InjuryHealingFactor are not modelled.
#[allow(clippy::too_many_arguments)]
pub fn health_interval(
    health: &mut Health,
    defs: &GameDefs,
    body: &BodyDef,
    health_scale: f32,
    bleed_rate_factor: f32,
    delta: i32,
    bleed_tick: bool,
    heal_tick: bool,
    comp_tick: bool,
    lying_down: bool,
    bed_heal_per_day: Option<f32>,
    starving: bool,
    blood_body_size: Option<f32>,
    rng: &mut Rand,
) -> bool {
    if health.dead {
        return false;
    }
    for h in &mut health.hediffs {
        h.age_ticks += delta as i64;
        let d = &defs.hediffs[h.def];
        // `CompPostMake` / `CompPostPostAdd`: roll the comps' values once.
        if !h.comps.initialized {
            h.comps.initialized = true;
            if let Some((min, max)) = d.disappears_after_ticks {
                h.comps.disappears_after = rng.range_inclusive(min, max) as i64;
            }
            if let Some((base, range, reverse)) = d.severity_per_day {
                let mut v = base
                    + if range != (0.0, 0.0) {
                        rng.range_f32(range.0, range.1)
                    } else {
                        0.0
                    };
                if rng.chance(reverse) {
                    v = -v;
                }
                h.comps.severity_per_day = v;
            }
        }
        // `HediffComp_SeverityModifierBase`: per day / 300 on the 200-tick
        // interval.
        // COMPATIBILITY TODO: currently approximate — the stage's
        // severityGainFactor and the minimum age are not applied.
        if comp_tick && d.severity_per_day.is_some() {
            h.severity += h.comps.severity_per_day * 0.003_333_333_4;
        }
        // `HediffComp_Disappears`.
        if h.comps.disappears_after > 0 && h.age_ticks >= h.comps.disappears_after {
            h.severity = 0.0;
        }
        // `HediffComp_TendDuration`: a timed tend runs out.
        // COMPATIBILITY TODO: currently approximate — the tended severity
        // change (`severityPerDayTended`) and `disappearsAtTotalTendQuality`
        // are not modelled.
        if let Some(t) = d.tend_duration
            && !t.permanent()
            && h.comps.tend_ticks_left > 0
        {
            h.comps.tend_ticks_left = (h.comps.tend_ticks_left - delta).max(0);
        }
    }
    if heal_tick && !starving {
        let healing: Vec<usize> = health
            .hediffs
            .iter()
            .enumerate()
            .filter(|(_, h)| defs.hediffs[h.def].is_injury())
            .map(|(i, _)| i)
            .collect();
        if !healing.is_empty() {
            let per_day = if lying_down {
                12.0 + bed_heal_per_day.unwrap_or(0.0)
            } else {
                8.0
            };
            // `RandomElement`.
            let pick = healing[rng.range(0, healing.len() as i32) as usize];
            health.hediffs[pick].severity -= per_day * health_scale * 0.01;
        }
        // Tended injuries also heal by 8 a day scaled by the tend quality
        // (×0.5 at 0, ×1.5 at 1).
        let is_tended = |h: &Hediff| {
            defs.hediffs[h.def].is_injury()
                && defs.hediffs[h.def].tend_duration.is_some()
                && h.comps.tend_ticks_left > 0
        };
        if health
            .hediffs
            .iter()
            .any(|h| is_tended(h) && h.severity > 0.0)
        {
            let tended: Vec<usize> = health
                .hediffs
                .iter()
                .enumerate()
                .filter(|(_, h)| is_tended(h))
                .map(|(i, _)| i)
                .collect();
            let pick = tended[rng.range(0, tended.len() as i32) as usize];
            let q = health.hediffs[pick].comps.tend_quality.clamp(0.0, 1.0);
            health.hediffs[pick].severity -= 8.0 * lerp(0.5, 1.5, q) * health_scale * 0.01;
        }
    }
    // Bleeding pawns drop blood: a chance per tick, ten times lower when
    // not standing (crawling smears are not modelled).
    let mut drop_blood = false;
    if let Some(size) = blood_body_size {
        let bleed = HealthView {
            defs,
            body,
            hediffs: &health.hediffs,
            health_scale,
            bleed_rate_factor,
        }
        .bleed_rate_total();
        if bleed >= 0.1 {
            let per_tick = bleed * size * if lying_down { 0.0004 } else { 0.004 };
            drop_blood = rng.chance(per_tick * delta as f32);
        }
    }
    if bleed_tick && let Some(blood_loss) = defs.hediffs.id("BloodLoss") {
        let bleed = HealthView {
            defs,
            body,
            hediffs: &health.hediffs,
            health_scale,
            bleed_rate_factor,
        }
        .bleed_rate_total();
        // `HediffGiver_Bleeding` with `HealthUtility.AdjustSeverity`.
        let change = if bleed >= 0.1 {
            bleed * 0.001
        } else {
            -0.000_333_333_33
        };
        match health.hediffs.iter_mut().find(|h| h.def == blood_loss) {
            Some(h) => h.severity += change,
            None if change > 0.0 => health.hediffs.push(Hediff {
                def: blood_loss,
                part: None,
                severity: change,
                age_ticks: 0,
                comps: Default::default(),
            }),
            None => {}
        }
    }
    // `ShouldRemove`: healed injuries and recovered blood loss go.
    health.hediffs.retain(|h| {
        let d = &defs.hediffs[h.def];
        d.hediff_class.as_deref() == Some("Hediff_MissingPart") || h.severity > 0.0
    });
    drop_blood
}
