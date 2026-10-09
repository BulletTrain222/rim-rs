//! Health Defs: bodies (`BodyDef` with its part tree), body parts
//! (`BodyPartDef`), hediffs (`HediffDef`), damage (`DamageDef`) and pawn
//! capacities (`PawnCapacityDef`). Defaults are the game's field
//! initializers.

use crate::database::Def;
use crate::typed::{DefTable, HasDefName};
use crate::xml::XmlNode;

/// A comp's properties by its class (`<comps><li Class="...">`).
fn comp<'n>(n: &'n XmlNode, class: &str) -> Option<&'n XmlNode> {
    n.child("comps")?
        .children
        .iter()
        .find(|li| li.attr("Class") == Some(class))
}

fn num(n: &XmlNode, field: &str, default: f32) -> f32 {
    n.child_text(field)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn flag(n: &XmlNode, field: &str, default: bool) -> bool {
    n.child_text(field)
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

/// `BodyPartDepth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartDepth {
    Inside,
    Outside,
}

/// `BodyPartHeight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartHeight {
    Bottom,
    Middle,
    Top,
}

/// One node of a body's part tree (`BodyPartRecord`), flattened in the
/// game's `AllParts` order (pre-order from the core part).
#[derive(Debug, Clone, PartialEq)]
pub struct BodyPartRecord {
    /// Reference to a `BodyPartDef`.
    pub def: String,
    pub custom_label: Option<String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub coverage: f32,
    /// Coverage of this part and its children (`coverageAbsWithChildren`).
    pub coverage_abs_with_children: f32,
    /// Coverage of this part alone (`coverageAbs`): what can be hit.
    pub coverage_abs: f32,
    pub depth: PartDepth,
    pub height: PartHeight,
    pub groups: Vec<String>,
}

/// `BodyDef`.
#[derive(Debug, Clone)]
pub struct BodyDef {
    pub def_name: String,
    pub parts: Vec<BodyPartRecord>,
}

impl BodyDef {
    pub(crate) fn from_def(def: &Def) -> Self {
        let mut parts = Vec::new();
        if let Some(core) = def.node.child("corePart") {
            // `CacheDataRecursive`: undefined height/depth become Middle and
            // Outside at the root and are inherited by children.
            add_part(core, None, 1.0, None, None, &mut parts);
        }
        Self {
            def_name: def.def_name.clone(),
            parts,
        }
    }

    /// Parts carrying a body part tag (via their def).
    pub fn parts_with_tag<'a>(
        &'a self,
        part_defs: &'a DefTable<BodyPartDef>,
        tag: &'a str,
    ) -> impl Iterator<Item = usize> + 'a {
        (0..self.parts.len()).filter(move |&i| {
            part_defs
                .get(&self.parts[i].def)
                .is_some_and(|d| d.tags.iter().any(|t| t == tag))
        })
    }

    /// The part and every part below it.
    pub fn subtree(&self, part: usize) -> Vec<usize> {
        let mut out = vec![part];
        let mut n = 0;
        while n < out.len() {
            let p = out[n];
            out.extend(self.parts[p].children.iter().copied());
            n += 1;
        }
        out
    }

    /// Whether `ancestor` is `part` or one of its ancestors.
    pub fn is_ancestor_or_self(&self, ancestor: usize, part: usize) -> bool {
        let mut p = Some(part);
        while let Some(x) = p {
            if x == ancestor {
                return true;
            }
            p = self.parts[x].parent;
        }
        false
    }
}

fn add_part(
    node: &XmlNode,
    parent: Option<usize>,
    parent_cov_with_children: f32,
    parent_height: Option<PartHeight>,
    parent_depth: Option<PartDepth>,
    out: &mut Vec<BodyPartRecord>,
) -> usize {
    let coverage = num(node, "coverage", 1.0);
    let cov_with_children = if parent.is_some() {
        parent_cov_with_children * coverage
    } else {
        1.0
    };
    let children_nodes: Vec<&XmlNode> = node
        .child("parts")
        .map(|p| p.children.iter().collect())
        .unwrap_or_default();
    let mut own = 1.0
        - children_nodes
            .iter()
            .map(|c| num(c, "coverage", 1.0))
            .sum::<f32>();
    if own.abs() < 1e-5 || own <= 0.0 {
        own = 0.0;
    }
    let height = match node.child_text("height") {
        Some("Bottom") => PartHeight::Bottom,
        Some("Top") => PartHeight::Top,
        Some("Middle") => PartHeight::Middle,
        _ => parent_height.unwrap_or(PartHeight::Middle),
    };
    let depth = match node.child_text("depth") {
        Some("Inside") => PartDepth::Inside,
        Some("Outside") => PartDepth::Outside,
        _ => parent_depth.unwrap_or(PartDepth::Outside),
    };
    let index = out.len();
    out.push(BodyPartRecord {
        def: node.child_text("def").unwrap_or_default().to_owned(),
        custom_label: node.child_text("customLabel").map(str::to_owned),
        parent,
        children: Vec::new(),
        coverage,
        coverage_abs_with_children: cov_with_children,
        coverage_abs: cov_with_children * own,
        depth,
        height,
        groups: node.child_list_texts("groups"),
    });
    for c in children_nodes {
        let child = add_part(
            c,
            Some(index),
            cov_with_children,
            Some(height),
            Some(depth),
            out,
        );
        out[index].children.push(child);
    }
    index
}

/// `BodyPartDef`.
#[derive(Debug, Clone)]
pub struct BodyPartDef {
    pub def_name: String,
    pub label: String,
    pub hit_points: i32,
    pub bleed_rate: f32,
    pub tags: Vec<String>,
    pub destroyable_by_damage: bool,
    pub skin_covered: bool,
    pub solid: bool,
    pub alive: bool,
    /// Weight of this part when frostbite strikes (`frostbiteVulnerability`).
    pub frostbite_vulnerability: f32,
}

impl BodyPartDef {
    pub(crate) fn from_def(def: &Def) -> Self {
        let n = &def.node;
        Self {
            def_name: def.def_name.clone(),
            label: n.child_text("label").unwrap_or(&def.def_name).to_owned(),
            hit_points: num(n, "hitPoints", 10.0) as i32,
            bleed_rate: num(n, "bleedRate", 1.0),
            tags: n.child_list_texts("tags"),
            destroyable_by_damage: flag(n, "destroyableByDamage", true),
            skin_covered: flag(n, "skinCovered", false),
            solid: flag(n, "solid", false),
            alive: flag(n, "alive", true),
            frostbite_vulnerability: num(n, "frostbiteVulnerability", 0.0),
        }
    }

    /// `GetMaxHealth`: ceil(hitPoints × the pawn's health scale).
    pub fn max_health(&self, health_scale: f32) -> f32 {
        (self.hit_points as f32 * health_scale).ceil()
    }
}

/// A hediff stage's effect on a capacity (`PawnCapacityModifier`).
#[derive(Debug, Clone, PartialEq)]
pub struct CapacityModifier {
    pub capacity: String,
    pub offset: f32,
    pub post_factor: f32,
    pub set_max: f32,
}

/// `HediffStage` (the parts we use).
#[derive(Debug, Clone, PartialEq)]
pub struct HediffStage {
    pub min_severity: f32,
    pub label: Option<String>,
    pub pain_offset: f32,
    /// `painFactor` (default 1): multiplies the pawn's total pain.
    pub pain_factor: f32,
    pub cap_mods: Vec<CapacityModifier>,
    /// `hungerRateFactor` (default 1) and `hungerRateFactorOffset`.
    pub hunger_rate_factor: f32,
    pub hunger_rate_factor_offset: f32,
    /// `lifeThreatening`.
    pub life_threatening: bool,
}

/// `HediffCompProperties_TendDuration`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TendDurationProps {
    /// `baseTendDurationHours`; negative: a tend lasts for good.
    pub base_tend_duration_hours: f32,
    /// `tendOverlapHours` (default 3).
    pub tend_overlap_hours: f32,
    /// `severityPerDayTended`.
    pub severity_per_day_tended: f32,
}

impl TendDurationProps {
    /// `TendIsPermanent`.
    pub fn permanent(&self) -> bool {
        self.base_tend_duration_hours < 0.0
    }

    /// `TendTicksFull`: duration plus overlap, in ticks.
    pub fn tend_ticks_full(&self) -> i32 {
        ((self.base_tend_duration_hours + self.tend_overlap_hours) * 2500.0).round_ties_even()
            as i32
    }

    /// `TendTicksOverlap`.
    pub fn tend_ticks_overlap(&self) -> i32 {
        (self.tend_overlap_hours * 2500.0).round_ties_even() as i32
    }
}

/// `InjuryProps`.
#[derive(Debug, Clone, PartialEq)]
pub struct InjuryProps {
    pub pain_per_severity: f32,
    pub bleed_rate: f32,
    pub can_merge: bool,
}

/// `HediffDef`.
#[derive(Debug, Clone)]
pub struct HediffDef {
    pub def_name: String,
    pub label: String,
    pub hediff_class: Option<String>,
    pub injury: Option<InjuryProps>,
    /// Severity at which it kills (`lethalSeverity`; -1 = never).
    pub lethal_severity: f32,
    pub stages: Vec<HediffStage>,
    /// `HediffCompProperties_SeverityPerDay`: (severityPerDay,
    /// severityPerDayRange, reverseSeverityChangeChance).
    pub severity_per_day: Option<(f32, (f32, f32), f32)>,
    /// `HediffCompProperties_Disappears.disappearsAfterTicks` range.
    pub disappears_after_ticks: Option<(i32, i32)>,
    /// `tendable`.
    pub tendable: bool,
    /// `preventsCrawling`.
    pub prevents_crawling: bool,
    /// Gives the Sick thought (`makesSickThought`).
    pub makes_sick_thought: bool,
    pub tend_duration: Option<TendDurationProps>,
}

impl HediffDef {
    pub(crate) fn from_def(def: &Def) -> Self {
        let n = &def.node;
        let injury = n
            .child("injuryProps")
            .filter(|i| !i.is_null())
            .map(|i| InjuryProps {
                pain_per_severity: num(i, "painPerSeverity", 0.0),
                bleed_rate: num(i, "bleedRate", 0.0),
                can_merge: flag(i, "canMerge", false),
            });
        let stages = n
            .child("stages")
            .map(|s| {
                s.children
                    .iter()
                    .map(|st| HediffStage {
                        min_severity: num(st, "minSeverity", 0.0),
                        label: st.child_text("label").map(str::to_owned),
                        pain_offset: num(st, "painOffset", 0.0),
                        pain_factor: num(st, "painFactor", 1.0),
                        hunger_rate_factor: num(st, "hungerRateFactor", 1.0),
                        hunger_rate_factor_offset: num(st, "hungerRateFactorOffset", 0.0),
                        life_threatening: flag(st, "lifeThreatening", false),
                        cap_mods: st
                            .child("capMods")
                            .map(|c| {
                                c.children
                                    .iter()
                                    .map(|m| CapacityModifier {
                                        capacity: m
                                            .child_text("capacity")
                                            .unwrap_or_default()
                                            .to_owned(),
                                        offset: num(m, "offset", 0.0),
                                        post_factor: num(m, "postFactor", 1.0),
                                        set_max: num(m, "setMax", 999.0),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            def_name: def.def_name.clone(),
            label: n.child_text("label").unwrap_or(&def.def_name).to_owned(),
            hediff_class: n.child_text("hediffClass").map(str::to_owned),
            injury,
            lethal_severity: num(n, "lethalSeverity", -1.0),
            stages,
            severity_per_day: comp(n, "HediffCompProperties_SeverityPerDay").map(|c| {
                (
                    num(c, "severityPerDay", 0.0),
                    c.child_text("severityPerDayRange")
                        .and_then(crate::values::parse_float_range)
                        .unwrap_or((0.0, 0.0)),
                    num(c, "reverseSeverityChangeChance", 0.0),
                )
            }),
            disappears_after_ticks: comp(n, "HediffCompProperties_Disappears").and_then(|c| {
                c.child_text("disappearsAfterTicks")
                    .and_then(crate::values::parse_float_range)
                    .map(|(a, b)| (a as i32, b as i32))
            }),
            tendable: flag(n, "tendable", false),
            prevents_crawling: flag(n, "preventsCrawling", false),
            makes_sick_thought: flag(n, "makesSickThought", false),
            tend_duration: comp(n, "HediffCompProperties_TendDuration").map(|c| {
                TendDurationProps {
                    base_tend_duration_hours: num(c, "baseTendDurationHours", -1.0),
                    tend_overlap_hours: num(c, "tendOverlapHours", 3.0),
                    severity_per_day_tended: num(c, "severityPerDayTended", 0.0),
                }
            }),
        }
    }

    /// `CurStage`: the last stage whose minimum the severity reaches.
    pub fn stage_at(&self, severity: f32) -> Option<&HediffStage> {
        self.stages
            .iter()
            .rev()
            .find(|s| severity >= s.min_severity)
    }

    pub fn is_injury(&self) -> bool {
        self.hediff_class.as_deref() == Some("Hediff_Injury")
    }
}

/// `DamageDef`.
#[derive(Debug, Clone)]
pub struct DamageDef {
    pub def_name: String,
    pub label: String,
    pub worker_class: Option<String>,
    /// References to `HediffDef`s.
    pub hediff: Option<String>,
    pub hediff_skin: Option<String>,
    pub hediff_solid: Option<String>,
    pub harm_all_layers_until_outside: bool,
    /// `overkillPctToDestroyPart` (min, max).
    pub overkill_pct_to_destroy_part: (f32, f32),
    pub external_violence: bool,
    pub is_ranged: bool,
    /// Animals hit by it run away (`makesAnimalsFlee`).
    pub makes_animals_flee: bool,
    /// `armorCategory` (Sharp, Blunt, Heat); `None` ignores armor.
    pub armor_category: Option<String>,
    /// `defaultArmorPenetration`.
    pub default_armor_penetration: f32,
}

impl DamageDef {
    pub(crate) fn from_def(def: &Def) -> Self {
        let n = &def.node;
        let range = n
            .child_text("overkillPctToDestroyPart")
            .and_then(|v| {
                let (a, b) = v.split_once('~')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            })
            .unwrap_or((0.0, 0.7));
        Self {
            def_name: def.def_name.clone(),
            label: n.child_text("label").unwrap_or(&def.def_name).to_owned(),
            worker_class: n.child_text("workerClass").map(str::to_owned),
            hediff: n.child_text("hediff").map(str::to_owned),
            hediff_skin: n.child_text("hediffSkin").map(str::to_owned),
            hediff_solid: n.child_text("hediffSolid").map(str::to_owned),
            harm_all_layers_until_outside: flag(n, "harmAllLayersUntilOutside", false),
            armor_category: n.child_text("armorCategory").map(|t| t.trim().to_owned()),
            default_armor_penetration: num(n, "defaultArmorPenetration", 0.0),
            overkill_pct_to_destroy_part: range,
            external_violence: flag(n, "externalViolence", false),
            is_ranged: flag(n, "isRanged", false),
            makes_animals_flee: flag(n, "makesAnimalsFlee", false),
        }
    }
}

/// `PawnCapacityDef`.
#[derive(Debug, Clone)]
pub struct PawnCapacityDef {
    pub def_name: String,
    pub label: String,
    pub lethal_flesh: bool,
    pub min_for_capable: f32,
    pub min_value: f32,
    pub zero_if_cannot_be_awake: bool,
}

impl PawnCapacityDef {
    pub(crate) fn from_def(def: &Def) -> Self {
        let n = &def.node;
        Self {
            def_name: def.def_name.clone(),
            label: n.child_text("label").unwrap_or(&def.def_name).to_owned(),
            lethal_flesh: flag(n, "lethalFlesh", false),
            min_for_capable: num(n, "minForCapable", 0.0),
            min_value: num(n, "minValue", 0.0),
            zero_if_cannot_be_awake: flag(n, "zeroIfCannotBeAwake", false),
        }
    }
}

macro_rules! def_name {
    ($($t:ty),*) => {$(
        impl HasDefName for $t {
            fn def_name(&self) -> &str { &self.def_name }
        }
    )*};
}
def_name!(BodyDef, BodyPartDef, HediffDef, DamageDef, PawnCapacityDef);
