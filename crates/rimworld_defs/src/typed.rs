//! Typed views of the Def types today's prototype needs.
//!
//! Only the fields we use are read; everything else stays available in the
//! generic [`DefDatabase`]. Unknown or malformed values produce warnings and
//! fall back to RimWorld's defaults rather than failing the whole load.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;

use crate::database::{Def, DefDatabase};
use crate::think::ThinkTreeDef;
use crate::values::{Rgba, parse_bool, parse_color};
use crate::xml::XmlNode;

/// Compact, typed handle to a Def inside a [`DefTable`]. Cheap to copy and
/// store in simulation data instead of `defName` strings.
pub struct DefId<T> {
    index: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> DefId<T> {
    fn new(index: usize) -> Self {
        Self {
            index: index as u32,
            _marker: PhantomData,
        }
    }
    pub fn index(self) -> usize {
        self.index as usize
    }

    /// A handle from a raw table index (tests and tools; the index must
    /// belong to the table it is used with).
    pub fn from_index(index: usize) -> Self {
        Self::new(index)
    }
}

/// Saved as the table index; a save records a fingerprint of the Def
/// tables so it is only loaded with the same data.
impl<T> serde::Serialize for DefId<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.index)
    }
}

impl<'de, T> serde::Deserialize<'de> for DefId<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::new(u32::deserialize(d)? as usize))
    }
}

impl<T> Clone for DefId<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for DefId<T> {}
impl<T> PartialEq for DefId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}
impl<T> Eq for DefId<T> {}
impl<T> PartialOrd for DefId<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for DefId<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.index.cmp(&other.index)
    }
}
impl<T> std::hash::Hash for DefId<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}
impl<T> fmt::Debug for DefId<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DefId({})", self.index)
    }
}

/// Typed Defs of one kind with a `defName` index.
#[derive(Debug, Clone)]
pub struct DefTable<T> {
    items: Vec<T>,
    index: HashMap<String, usize>,
}

impl<T> Default for DefTable<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<T: HasDefName> DefTable<T> {
    pub(crate) fn push(&mut self, item: T) {
        self.index
            .insert(item.def_name().to_owned(), self.items.len());
        self.items.push(item);
    }
    pub fn id(&self, def_name: &str) -> Option<DefId<T>> {
        self.index.get(def_name).map(|&i| DefId::new(i))
    }
    pub fn get(&self, def_name: &str) -> Option<&T> {
        self.index.get(def_name).map(|&i| &self.items[i])
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (DefId<T>, &T)> {
        self.items
            .iter()
            .enumerate()
            .map(|(i, t)| (DefId::new(i), t))
    }
}

impl<T> std::ops::Index<DefId<T>> for DefTable<T> {
    type Output = T;
    fn index(&self, id: DefId<T>) -> &T {
        &self.items[id.index()]
    }
}

pub trait HasDefName {
    fn def_name(&self) -> &str;
}

/// RimWorld's `Traversability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Passability {
    #[default]
    Standable,
    PassThroughOnly,
    Impassable,
}

impl Passability {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "Standable" => Some(Self::Standable),
            "PassThroughOnly" => Some(Self::PassThroughOnly),
            "Impassable" => Some(Self::Impassable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerrainDef {
    pub def_name: String,
    pub label: String,
    pub texture_path: Option<String>,
    /// Extra movement cost (ticks) to enter a cell with this terrain.
    pub path_cost: i32,
    pub passability: Passability,
    pub fertility: f32,
    pub affordances: Vec<String>,
    /// Explicit colour (only some terrains have one).
    pub color: Option<Rgba>,
    pub category_type: Option<String>,
    pub render_precedence: i32,
    pub natural: bool,
    /// Pawns avoid wandering to / sleeping on this terrain.
    pub avoid_wander: bool,
    pub stat_bases: BTreeMap<String, f32>,
    /// Materials to lay it as a floor (`costList`).
    pub cost_list: Vec<(String, u32)>,
    /// The architect category; floors have `Floors`.
    pub designation_category: Option<String>,
    /// What the ground under it must afford (`terrainAffordanceNeeded`).
    pub terrain_affordance_needed: Option<String>,
    pub construction_skill_prerequisite: i32,
    /// Laid over the terrain beneath, which comes back when it is removed
    /// (`layerable`).
    pub layerable: bool,
    /// Filth pawns pick up walking on it (`generatedFilth`).
    pub generated_filth: Option<String>,
    /// What smoothing turns it into (`smoothedTerrain`).
    pub smoothed_terrain: Option<String>,
    /// Share of the cost left when the floor is removed
    /// (`resourcesFractionWhenDeconstructed`, default 0.5).
    pub resources_fraction_when_deconstructed: f32,
    /// Extra deterioration of things lying on it (`extraDeteriorationFactor`).
    pub extra_deterioration_factor: f32,
    /// Filth sources it accepts (`filthAcceptanceMask`, default Any).
    pub filth_acceptance_mask: u8,
    /// `uiOrder` (default 2999): Architect order.
    pub ui_order: f32,
    /// `drawStyleCategory`.
    pub draw_style_category: Option<String>,
    pub research_prerequisites: Vec<String>,
    /// `designatorDropdown`: Architect dropdown group.
    pub designator_dropdown: Option<String>,
}

/// `TerrainDefGenerator_Stone`: each natural rock (not ore) implies rough,
/// rough-hewn and smooth stone terrains (path cost 2, 1 and 0), unless its
/// building names them; the rock leaves the rough-hewn one when mined.
fn add_implied_stone_terrains(defs: &mut GameDefs) {
    let rocks: Vec<usize> = defs
        .things
        .items
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.building
                .as_ref()
                .is_some_and(|b| b.is_natural_rock && !b.is_resource_rock)
        })
        .map(|(i, _)| i)
        .collect();
    // Textures and draw order as the generator sets them: rough stone 190 + i,
    // rough-hewn 50 + i, smooth 140 + i (i = the rock's position among the
    // natural rocks).
    let stone =
        |name: String, label: String, cost: i32, color: Option<Rgba>, natural: bool, i: i32| {
            let (texture, precedence) = match cost {
                2 => ("Terrain/Surfaces/RoughStone", 190 + i),
                1 => ("Terrain/Surfaces/RoughHewnRock", 50 + i),
                _ => ("Terrain/Surfaces/SmoothStone", 140 + i),
            };
            let mut affordances: Vec<String> = ["Light", "Medium", "Heavy"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            if cost > 0 {
                affordances.push("SmoothableStone".to_owned());
            }
            affordances.push("Walkable".to_owned());
            TerrainDef {
                def_name: name,
                label,
                texture_path: Some(texture.to_owned()),
                path_cost: cost,
                passability: Passability::Standable,
                fertility: 0.0,
                affordances,
                color,
                category_type: Some("Stone".to_owned()),
                render_precedence: precedence,
                natural,
                avoid_wander: false,
                stat_bases: BTreeMap::new(),
                cost_list: Vec::new(),
                designation_category: None,
                terrain_affordance_needed: None,
                construction_skill_prerequisite: 0,
                layerable: false,
                generated_filth: None,
                smoothed_terrain: None,
                resources_fraction_when_deconstructed: 0.5,
                extra_deterioration_factor: 0.0,
                // Rough stone: Terrain | Unnatural; hewn and smooth: Any.
                filth_acceptance_mask: if natural {
                    filth_flags::TERRAIN | filth_flags::UNNATURAL
                } else {
                    filth_flags::ANY
                },
                ui_order: 2999.0,
                draw_style_category: None,
                research_prerequisites: Vec::new(),
                designator_dropdown: None,
            }
        };
    for (n_rock, i) in rocks.into_iter().enumerate() {
        let n_rock = n_rock as i32;
        let (name, label, color) = {
            let t = &defs.things.items[i];
            (
                t.def_name.clone(),
                t.label.clone(),
                t.graphic.as_ref().and_then(|g| g.color),
            )
        };
        let b = defs.things.items[i].building.clone().expect("a rock");
        let smooth = format!("{name}_Smooth");
        if b.natural_terrain.is_none() {
            let n = format!("{name}_Rough");
            if defs.terrain.id(&n).is_none() {
                let mut t = stone(n.clone(), format!("rough {label}"), 2, color, true, n_rock);
                t.smoothed_terrain = Some(smooth.clone());
                defs.terrain.push(t);
            }
            defs.things.items[i]
                .building
                .as_mut()
                .unwrap()
                .natural_terrain = Some(n);
        }
        if b.leave_terrain.is_none() {
            let n = format!("{name}_RoughHewn");
            if defs.terrain.id(&n).is_none() {
                let mut t = stone(
                    n.clone(),
                    format!("rough-hewn {label}"),
                    1,
                    color,
                    false,
                    n_rock,
                );
                t.smoothed_terrain = Some(smooth.clone());
                defs.terrain.push(t);
            }
            defs.things.items[i]
                .building
                .as_mut()
                .unwrap()
                .leave_terrain = Some(n);
        }
        if defs.terrain.id(&smooth).is_none() {
            defs.terrain.push(stone(
                smooth,
                format!("smooth {label}"),
                0,
                color,
                false,
                n_rock,
            ));
        }
    }
}

/// `ThingDefGenerator_Meat`: every flesh race with meat and a corpse that
/// doesn't share another race's meat implies an item `Meat_<race>`: raw
/// meat (RawBad, nutrition 0.05, 75 per stack, rots away after 2 days),
/// in MeatRaw, with the race's meat label, colour and market value and
/// the small/big/human/insect texture. Then `RaceProperties`'
/// references resolve: `specificMeatDef` or `useMeatFrom`'s meat;
/// non-flesh races butcher into Steel; `useLeatherFrom`'s leather.
// COMPATIBILITY TODO: currently approximate — the flesh type's special
// eating thoughts and the ingredient merge tags are not copied.
fn add_implied_meat(defs: &mut GameDefs, raw: &DefDatabase) {
    let organic = |flesh: &str| -> bool {
        raw.get("FleshTypeDef", flesh)
            .and_then(|d| d.node.child_text("isOrganic"))
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    };
    let races: Vec<ThingDef> = defs
        .things
        .items
        .iter()
        .filter(|t| t.is_pawn() && t.race.is_some())
        .cloned()
        .collect();
    let mut meat_of: Vec<(String, String)> = Vec::new();
    for pawn in &races {
        let race = pawn.race.as_ref().expect("a race");
        if !race.has_meat
            || !race.has_corpse
            || race.use_meat_from.is_some()
            || race.specific_meat_def.is_some()
        {
            continue;
        }
        let flesh = race.flesh_type.as_deref().unwrap_or("Normal");
        if !organic(flesh) {
            meat_of.push((pawn.def_name.clone(), "Steel".to_owned()));
            continue;
        }
        let name = format!("Meat_{}", pawn.def_name);
        meat_of.push((pawn.def_name.clone(), name.clone()));
        if defs.things.id(&name).is_some() {
            continue;
        }
        let humanlike = race.intelligence.as_deref() == Some("Humanlike");
        let tex = if humanlike {
            "Meat_Human"
        } else if flesh == "Insectoid" {
            "Meat_Insect"
        } else if race.base_body_size < 0.7 {
            "Meat_Small"
        } else {
            "Meat_Big"
        };
        let mut stat_bases = BTreeMap::new();
        for (k, v) in [
            ("MaxHitPoints", 60.0),
            ("Beauty", -4.0),
            ("DeteriorationRate", 6.0),
            ("Mass", 0.03),
            ("Flammability", 0.5),
            ("Nutrition", 0.05),
            ("FoodPoisonChanceFixedHuman", 0.02),
            ("MarketValue", race.meat_market_value),
        ] {
            stat_bases.insert(k.to_owned(), v);
        }
        let mut m = pawn.clone();
        m.label = race
            .meat_label
            .clone()
            .unwrap_or_else(|| format!("{} meat", pawn.label));
        m.graphic = Some(GraphicData {
            tex_path: Some(format!("Things/Item/Resource/MeatFoodRaw/{tex}")),
            graphic_class: Some("Graphic_StackCount".to_owned()),
            color: race.meat_color,
            draw_size: (1.0, 1.0),
            draw_rotated: true,
            ..Default::default()
        });
        m.def_name = name;
        m.category = Some("Item".to_owned());
        m.thing_class = Some("ThingWithComps".to_owned());
        m.passability = Passability::Standable;
        m.path_cost = 14;
        m.selectable = true;
        m.stat_bases = stat_bases;
        m.meat_of = Some(pawn.def_name.clone());
        m.race = None;
        m.ingestible = Some(IngestibleProperties {
            preferability: FoodPreferability::RawBad,
            food_type: food_type::MEAT,
            taste_thought: Some("AteRawFood".to_owned()),
            ..Default::default()
        });
        m.tools = Vec::new();
        m.stack_limit = 75;
        m.always_haulable = true;
        m.thing_categories = vec!["MeatRaw".to_owned()];
        m.use_hit_points = true;
        m.deteriorate_from_environmental_effects = true;
        m.ticker_type = Some("Rare".to_owned());
        m.forbiddable = true;
        m.rottable = Some(RottableProperties {
            days_to_rot_start: 2.0,
            rot_destroys: true,
            rot_damage_per_day: 40.0,
            days_to_dessicated: 999.0,
            dessicated_damage_per_day: 0.0,
        });
        defs.things.push(m);
    }
    // `RaceProperties.ResolveReferences`.
    let lookup = |name: &str, defs: &GameDefs| defs.things.get(name).and_then(|t| t.race.clone());
    let n = defs.things.items.len();
    for k in 0..n {
        let Some(race) = defs.things.items[k].race.clone() else {
            continue;
        };
        let own = meat_of
            .iter()
            .find(|(r, _)| *r == defs.things.items[k].def_name)
            .map(|(_, m)| m.clone());
        let meat = race.specific_meat_def.clone().or(own).or_else(|| {
            let from = race.use_meat_from.as_deref()?;
            meat_of
                .iter()
                .find(|(r, _)| r == from)
                .map(|(_, m)| m.clone())
        });
        let leather = match race.use_leather_from.as_deref() {
            Some(from) => lookup(from, defs).and_then(|r| r.leather_def),
            None => race.leather_def.clone(),
        };
        if let Some(r) = defs.things.items[k].race.as_mut() {
            r.meat_def = meat;
            r.leather_def = leather;
        }
    }
}

/// `ThingDefGenerator_Corpses`: every pawn race with `hasCorpse` implies
/// an item `Corpse_<race>`: path cost 14, always haulable, one per stack,
/// MaxHitPoints and Mass from the race, beauty −50, deterioration rate 1,
/// in CorpsesHumanlike or its flesh type's `corpseCategory`; flesh corpses
/// rot (2.5 days to start, dessicated at 5) without vanishing.
// COMPATIBILITY TODO: currently approximate — corpses have no market value
// or bile filth spawner; the flesh type's special eating thoughts are not
// copied.
fn add_implied_corpses(defs: &mut GameDefs, raw: &DefDatabase) {
    let corpse_category = |flesh: &str| -> Option<String> {
        raw.get("FleshTypeDef", flesh)
            .and_then(|d| d.node.child_text("corpseCategory"))
            .map(|t| t.trim().to_owned())
    };
    let default_hp = defs
        .stats
        .get("MaxHitPoints")
        .map_or(100.0, |s| s.default_base_value);
    let races: Vec<ThingDef> = defs
        .things
        .items
        .iter()
        .filter(|t| t.is_pawn() && t.race.as_ref().is_some_and(|r| r.has_corpse))
        .cloned()
        .collect();
    for pawn in races {
        let name = format!("Corpse_{}", pawn.def_name);
        if defs.things.id(&name).is_some() {
            continue;
        }
        let race = pawn.race.clone().expect("a pawn race");
        let flesh = race
            .flesh_type
            .clone()
            .unwrap_or_else(|| "Normal".to_owned());
        let category = if race.intelligence.as_deref() == Some("Humanlike") {
            Some("CorpsesHumanlike".to_owned())
        } else {
            corpse_category(&flesh)
        };
        let max_hp = pawn
            .stat_bases
            .get("MaxHitPoints")
            .copied()
            .unwrap_or(default_hp)
            .round_ties_even();
        let mut stat_bases = BTreeMap::new();
        stat_bases.insert("Beauty".to_owned(), -50.0);
        stat_bases.insert("DeteriorationRate".to_owned(), 1.0);
        stat_bases.insert("MaxHitPoints".to_owned(), max_hp);
        stat_bases.insert(
            "Mass".to_owned(),
            pawn.stat_bases.get("Mass").copied().unwrap_or(0.0),
        );
        stat_bases.insert("Nutrition".to_owned(), 5.2);
        if let Some(&f) = pawn.stat_bases.get("Flammability") {
            stat_bases.insert("Flammability".to_owned(), f);
        }
        let mut c = pawn;
        c.def_name = name;
        c.label = format!("{} corpse", c.label);
        c.category = Some("Item".to_owned());
        c.thing_class = Some("Corpse".to_owned());
        c.passability = Passability::Standable;
        c.path_cost = 14;
        c.selectable = true;
        c.graphic = None;
        c.stat_bases = stat_bases;
        c.corpse_of = Some(c.def_name["Corpse_".len()..].to_owned());
        // Corpses are food: DesperateOnly for flesh (NeverForNutrition
        // otherwise), of the Corpse food type, one at a time (the AteCorpse
        // taste).
        let flesh_corpse = flesh != "Mechanoid"
            && raw
                .get("FleshTypeDef", &flesh)
                .and_then(|d| d.node.child_text("isOrganic"))
                .is_none_or(|v| v.trim().eq_ignore_ascii_case("true"));
        c.race = None;
        c.ingestible = Some(IngestibleProperties {
            preferability: if flesh_corpse {
                FoodPreferability::DesperateOnly
            } else {
                FoodPreferability::NeverForNutrition
            },
            food_type: food_type::CORPSE,
            max_num_to_ingest_at_once: 1,
            default_num_to_ingest_at_once: 1,
            taste_thought: Some("AteCorpse".to_owned()),
            ..Default::default()
        });
        c.tools = Vec::new();
        c.stack_limit = 1;
        c.always_haulable = true;
        c.thing_categories = category.into_iter().collect();
        c.use_hit_points = true;
        c.deteriorate_from_environmental_effects = true;
        c.ticker_type = Some("Rare".to_owned());
        c.forbiddable = true;
        c.rottable = (flesh != "Mechanoid").then_some(RottableProperties {
            days_to_rot_start: 2.5,
            rot_destroys: false,
            rot_damage_per_day: 2.0,
            days_to_dessicated: 5.0,
            dessicated_damage_per_day: 0.7,
        });
        defs.things.push(c);
    }
}

impl TerrainDef {
    pub fn is_walkable(&self) -> bool {
        self.passability != Passability::Impassable
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GraphicData {
    pub tex_path: Option<String>,
    pub graphic_class: Option<String>,
    pub color: Option<Rgba>,
    /// `drawSize` in cells (default 1×1).
    pub draw_size: (f32, f32),
    /// `shaderType` (`CutoutComplex` uses a `…m` mask for the stuff colour).
    pub shader_type: Option<String>,
    /// `drawRotated` (default true): single textures turn with the thing.
    pub draw_rotated: bool,
    /// `linkType` (`Basic`, `CornerFiller`, `Transmitter`, …) and
    /// `linkFlags`.
    pub link_type: Option<String>,
    pub link_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RaceProperties {
    pub intelligence: Option<String>,
    /// Reference to a `BodyDef`.
    pub body: Option<String>,
    pub base_body_size: f32,
    /// Reference to the main `ThinkTreeDef` (e.g. `Humanlike`).
    pub think_tree_main: Option<String>,
    /// Multiplier on food consumption (`baseHungerRate`, default 1).
    pub base_hunger_rate: f32,
    /// `baseHealthScale` (default 1).
    pub base_health_scale: f32,
    /// `bleedRateFactor` (default 1).
    pub bleed_rate_factor: f32,
    /// `fleshType` (default Normal).
    pub flesh_type: Option<String>,
    /// `bloodDef`: the filth a bleeding pawn drops (none if absent).
    pub blood_def: Option<String>,
    /// What the race can eat (`foodType`, [`food_type`] flags; default none).
    pub food_type: u32,
    /// Leaves a corpse when it dies (`hasCorpse`, default true).
    pub has_corpse: bool,
    /// Chance to turn manhunter when harmed (`manhunterOnDamageChance`).
    pub manhunter_on_damage_chance: f32,
    pub herd_animal: bool,
    /// Distance for hunting execution (`executionRange`, default 2).
    pub execution_range: f32,
    pub think_tree_constant: Option<String>,
    /// Butchering yields meat (`hasMeat`, default true).
    pub has_meat: bool,
    /// `meatLabel`, `meatColor` (default white), `meatMarketValue`
    /// (default 2).
    pub meat_label: Option<String>,
    pub meat_color: Option<Rgba>,
    pub meat_market_value: f32,
    /// `useMeatFrom` / `specificMeatDef` / `useLeatherFrom` (races).
    pub use_meat_from: Option<String>,
    pub specific_meat_def: Option<String>,
    pub use_leather_from: Option<String>,
    /// `leatherDef` (resolved through `useLeatherFrom`).
    pub leather_def: Option<String>,
    /// The resolved `meatDef`: the implied `Meat_<race>`, a shared one, or
    /// Steel for non-flesh races.
    pub meat_def: Option<String>,
    /// `wildBiomes`: (biome, commonality) where this race lives wild.
    pub wild_biomes: Vec<(String, f32)>,
    /// `canFlyIntoMap`, `waterSeeker`.
    pub can_fly_into_map: bool,
    pub water_seeker: bool,
    /// Hunts live prey when hungry (`predator`).
    pub predator: bool,
    /// `maxPreyBodySize` (default 99999).
    pub max_prey_body_size: f32,
    /// `canBePredatorPrey` (default true).
    pub can_be_predator_prey: bool,
}

impl RaceProperties {
    pub fn eats_food(&self) -> bool {
        self.food_type != food_type::NONE
    }

    pub fn eats(&self, food: u32) -> bool {
        self.food_type & food != 0
    }

    /// The race's diet category, derived from its food types as the game
    /// does (`RaceProperties.ResolvedDietCategory`).
    pub fn diet(&self) -> DietCategory {
        use food_type::*;
        if !self.eats_food() {
            DietCategory::NeverEats
        } else if self.eats(TREE) {
            DietCategory::Dendrovorous
        } else if self.eats(MEAT) {
            if self.eats(VEGETABLE_OR_FRUIT) || self.eats(PLANT) {
                DietCategory::Omnivorous
            } else {
                DietCategory::Carnivorous
            }
        } else if self.eats(ANIMAL_PRODUCT) {
            DietCategory::Ovivorous
        } else {
            DietCategory::Herbivorous
        }
    }

    /// Food level fraction below which the pawn wants to eat
    /// (`FoodLevelPercentageWantEat`).
    pub fn food_level_percentage_want_eat(&self) -> f32 {
        match self.diet() {
            DietCategory::NeverEats | DietCategory::Omnivorous | DietCategory::Carnivorous => 0.3,
            DietCategory::Ovivorous => 0.4,
            DietCategory::Herbivorous | DietCategory::Dendrovorous => 0.45,
        }
    }

    /// Whether the race can ever eat `thing` (`RaceProperties.CanEverEat`,
    /// without the per-race `willNeverEat` list).
    pub fn can_ever_eat(&self, thing: &ThingDef) -> bool {
        self.eats_food()
            && thing.ingestible.as_ref().is_some_and(|i| {
                i.preferability != FoodPreferability::Undefined && self.eats(i.food_type)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DietCategory {
    NeverEats,
    Herbivorous,
    Dendrovorous,
    Ovivorous,
    Omnivorous,
    Carnivorous,
}

/// The game's `FoodTypeFlags` bit values.
pub mod food_type {
    pub const NONE: u32 = 0;
    pub const VEGETABLE_OR_FRUIT: u32 = 0x1;
    pub const MEAT: u32 = 0x2;
    pub const FLUID: u32 = 0x4;
    pub const CORPSE: u32 = 0x8;
    pub const SEED: u32 = 0x10;
    pub const ANIMAL_PRODUCT: u32 = 0x20;
    pub const PLANT: u32 = 0x40;
    pub const TREE: u32 = 0x80;
    pub const MEAL: u32 = 0x100;
    pub const PROCESSED: u32 = 0x200;
    pub const LIQUOR: u32 = 0x400;
    pub const KIBBLE: u32 = 0x800;
    pub const FUNGUS: u32 = 0x1001;
    pub const VEGETARIAN_ANIMAL: u32 = FUNGUS | SEED | MEAL | PROCESSED | LIQUOR | KIBBLE;
    pub const VEGETARIAN_ROUGH_ANIMAL: u32 = VEGETARIAN_ANIMAL | PLANT;
    pub const CARNIVORE_ANIMAL: u32 = MEAT | CORPSE | MEAL | PROCESSED | KIBBLE;
    pub const CARNIVORE_ANIMAL_STRICT: u32 = MEAT | CORPSE;
    pub const OMNIVORE_ANIMAL: u32 = VEGETARIAN_ANIMAL | CARNIVORE_ANIMAL_STRICT;
    pub const OMNIVORE_ROUGH_ANIMAL: u32 = OMNIVORE_ANIMAL | PLANT;
    pub const DENDROVORE_ANIMAL: u32 = FUNGUS | SEED | TREE | PROCESSED | KIBBLE;
    pub const OVIVORE_ANIMAL: u32 = ANIMAL_PRODUCT | MEAL | PROCESSED | KIBBLE;
    pub const OMNIVORE_HUMAN: u32 = OMNIVORE_ANIMAL | FLUID | ANIMAL_PRODUCT;

    /// Parses a flags value as written in XML: names separated by commas.
    pub fn parse(text: &str) -> Option<u32> {
        text.split(',').map(str::trim).try_fold(0, |acc, name| {
            Some(
                acc | match name {
                    "None" => NONE,
                    "VegetableOrFruit" => VEGETABLE_OR_FRUIT,
                    "Meat" => MEAT,
                    "Fluid" => FLUID,
                    "Corpse" => CORPSE,
                    "Seed" => SEED,
                    "AnimalProduct" => ANIMAL_PRODUCT,
                    "Plant" => PLANT,
                    "Tree" => TREE,
                    "Meal" => MEAL,
                    "Processed" => PROCESSED,
                    "Liquor" => LIQUOR,
                    "Kibble" => KIBBLE,
                    "Fungus" => FUNGUS,
                    "VegetarianAnimal" => VEGETARIAN_ANIMAL,
                    "VegetarianRoughAnimal" => VEGETARIAN_ROUGH_ANIMAL,
                    "CarnivoreAnimal" => CARNIVORE_ANIMAL,
                    "CarnivoreAnimalStrict" => CARNIVORE_ANIMAL_STRICT,
                    "OmnivoreAnimal" => OMNIVORE_ANIMAL,
                    "OmnivoreRoughAnimal" => OMNIVORE_ROUGH_ANIMAL,
                    "DendrovoreAnimal" => DENDROVORE_ANIMAL,
                    "OvivoreAnimal" => OVIVORE_ANIMAL,
                    "OmnivoreHuman" => OMNIVORE_HUMAN,
                    _ => return None,
                },
            )
        })
    }
}

/// The game's `FoodPreferability`, in its order (later = preferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum FoodPreferability {
    #[default]
    Undefined,
    NeverForNutrition,
    DesperateOnly,
    DesperateOnlyForHumanlikes,
    RawBad,
    RawTasty,
    MealTerrible,
    MealAwful,
    MealSimple,
    MealFine,
    MealLavish,
}

impl FoodPreferability {
    pub fn parse(text: &str) -> Option<Self> {
        use FoodPreferability::*;
        Some(match text {
            "Undefined" => Undefined,
            "NeverForNutrition" => NeverForNutrition,
            "DesperateOnly" => DesperateOnly,
            "DesperateOnlyForHumanlikes" => DesperateOnlyForHumanlikes,
            "RawBad" => RawBad,
            "RawTasty" => RawTasty,
            "MealTerrible" => MealTerrible,
            "MealAwful" => MealAwful,
            "MealSimple" => MealSimple,
            "MealFine" => MealFine,
            "MealLavish" => MealLavish,
            _ => return None,
        })
    }
}

/// `<ingestible>`: how a thing is eaten. Defaults are the game's
/// `IngestibleProperties` field defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestibleProperties {
    pub preferability: FoodPreferability,
    pub food_type: u32,
    pub base_ingest_ticks: i32,
    pub chair_search_radius: f32,
    pub default_num_to_ingest_at_once: i32,
    /// 0 = no limit.
    pub max_num_to_ingest_at_once: i32,
    pub use_eating_speed_stat: bool,
    pub table_desired: bool,
    pub optimality_offset_humanlikes: f32,
    pub optimality_offset_feeding_animals: f32,
    /// ThoughtDef gained from the taste (e.g. `AteRawFood`).
    pub taste_thought: Option<String>,
    pub special_thought_direct: Option<String>,
    /// Thought for eating a meal made with it (`specialThoughtAsIngredient`).
    pub special_thought_as_ingredient: Option<String>,
    /// Job report while ingesting, `{0}` = the thing's label.
    pub ingest_report_string: Option<String>,
    /// Report for eaters that are not tool users (animals).
    pub ingest_report_string_eat: Option<String>,
}

impl Default for IngestibleProperties {
    fn default() -> Self {
        Self {
            preferability: FoodPreferability::Undefined,
            food_type: food_type::NONE,
            base_ingest_ticks: 500,
            chair_search_radius: 32.0,
            default_num_to_ingest_at_once: 20,
            max_num_to_ingest_at_once: 0,
            use_eating_speed_stat: true,
            table_desired: true,
            optimality_offset_humanlikes: 0.0,
            optimality_offset_feeding_animals: 0.0,
            taste_thought: None,
            special_thought_direct: None,
            special_thought_as_ingredient: None,
            ingest_report_string: None,
            ingest_report_string_eat: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BuildingProperties {
    pub is_natural_rock: bool,
    /// Plants can be sown in it (`SupportsPlants`: a `sowTag`).
    pub supports_plants: bool,
    /// Ore-bearing rock (`isResourceRock`).
    pub is_resource_rock: bool,
    /// Terrain under the rock (`naturalTerrain`; implied stone terrains
    /// fill it in).
    pub natural_terrain: Option<String>,
    /// Terrain left when it is destroyed (`leaveTerrain`; for natural rock,
    /// its rough-hewn stone).
    pub leave_terrain: Option<String>,
    /// Reference to a `ThingDef`.
    pub mineable_thing: Option<String>,
    /// Chance a mined cell leaves its `mineableThing` (default 1).
    pub mineable_drop_chance: f32,
    /// Units left per mined cell (`mineableYield`, default 1).
    pub mineable_yield: i32,
    /// The yield shrinks with mining-yield losses (default true).
    pub mineable_yield_wasteable: bool,
    /// What smoothing turns this rock into (`smoothedThing`).
    pub smoothed_thing: Option<String>,
    /// Largest body size a bed takes (`bed_maxBodySize`, default 9999).
    pub bed_max_body_size: f32,
    /// A bed for humanlikes (`bed_humanlike`, default true).
    pub bed_humanlike: bool,
    /// Extra natural healing per day in this bed (`bed_healPerDay`).
    pub bed_heal_per_day: f32,
    /// The colony's home area grows around it (`expandHomeArea`, default
    /// true).
    pub expand_home_area: bool,
    /// Occupies the cell's building slot (`isEdifice`, default true);
    /// conduits don't, so they lie under walls.
    pub is_edifice: bool,
    /// Can be fixed when broken down (`repairable`, default true).
    pub repairable: bool,
    /// Pawns can sit on it (`isSittable`).
    pub is_sittable: bool,
    /// Rooms it borders may get roofs automatically (`allowAutoroof`,
    /// default true).
    pub allow_autoroof: bool,
    /// Can be deconstructed (`deconstructible`, default true).
    pub deconstructible: bool,
    /// Factor on an unpowered door's opening time.
    pub unpowered_door_open_speed_factor: f32,
    /// Factor on an unpowered door's close delay.
    pub unpowered_door_close_speed_factor: f32,
    /// Factors for a powered door (`poweredDoorOpenSpeedFactor`,
    /// `poweredDoorCloseSpeedFactor`, default 1).
    pub powered_door_open_speed_factor: f32,
    pub powered_door_close_speed_factor: f32,
    /// The room role a work table wants (`workTableRoomRole`).
    pub work_table_room_role: Option<String>,
    /// Work speed factor outside that role (`workTableNotInRoomRoleFactor`,
    /// default 1).
    pub work_table_not_in_room_role_factor: f32,
    /// Counts towards bedroom and barracks roles
    /// (`bed_countsForBedroomOrBarracks`, default true).
    pub bed_counts_for_bedroom_or_barracks: bool,
    /// An unowned bed counts as barracks (`bed_emptyCountsForBarracks`,
    /// default true).
    pub bed_empty_counts_for_barracks: bool,
}

#[derive(Debug, Clone)]
pub struct ThingDef {
    pub def_name: String,
    pub label: String,
    /// `ThingCategory`: `Building`, `Item`, `Pawn`, `Plant`, `Filth`, ...
    pub category: Option<String>,
    pub thing_class: Option<String>,
    pub passability: Passability,
    pub path_cost: i32,
    /// `fillPercent` (0–1); above 0.99 counts as "Full" fill.
    pub fill_percent: f32,
    pub selectable: bool,
    /// `altitudeLayer` (draw height; the selector prefers higher things).
    pub altitude_layer: Option<String>,
    /// `resourceReadoutPriority` is not `Uncounted` (`CountAsResource`).
    pub count_as_resource: bool,
    /// `uiOrder` (default 2999): Architect order.
    pub ui_order: f32,
    /// `designationHotKey`.
    pub designation_hot_key: Option<String>,
    pub graphic: Option<GraphicData>,
    pub stat_bases: BTreeMap<String, f32>,
    pub race: Option<RaceProperties>,
    pub building: Option<BuildingProperties>,
    pub ingestible: Option<IngestibleProperties>,
    /// Plant properties (`plant`), for plants.
    pub plant: Option<PlantProperties>,
    /// Melee tools (`tools`).
    pub tools: Vec<Tool>,
    /// Verbs (`verbs`): e.g. a firearm's shot.
    pub verbs: Vec<VerbProperties>,
    /// A projectile's properties (`projectile`).
    pub projectile: Option<ProjectileProperties>,
    pub stack_limit: i32,
    pub filth: Option<FilthProperties>,
    /// Hauled to storage without a haul designation.
    pub always_haulable: bool,
    pub designate_haulable: bool,
    /// Counts as 0.1 volume per unit when carried (`smallVolume`).
    pub small_volume: bool,
    pub thing_categories: Vec<String>,
    /// Units of stuff a building made of stuff costs (`costStuffCount`).
    pub cost_stuff_count: u32,
    /// Fixed costs (`costList`): (def name, count).
    pub cost_list: Vec<(String, u32)>,
    /// Stuff categories the thing can be made of (`stuffCategories`).
    pub stuff_categories: Vec<String>,
    /// Properties as a material (`stuffProps`).
    pub stuff_props: Option<StuffProperties>,
    /// Architect menu category (`designationCategory`).
    pub designation_category: Option<String>,
    pub construction_skill_prerequisite: i32,
    /// Items must be moved off the site first
    /// (`forceMoveItemsBeforeConstruction`).
    pub force_move_items_before_construction: bool,
    /// Footprint in cells (`size`, x by z; default 1x1).
    pub size: (i32, i32),
    /// `surfaceType` (`Eat` for tables).
    pub surface_type: Option<String>,
    /// How dragging places it (`drawStyleCategory`, e.g. Walls): none means
    /// one placement per click.
    pub draw_style_category: Option<String>,
    /// The material the build tool starts with (`defaultStuff`).
    pub default_stuff: Option<String>,
    /// `description`.
    pub description: Option<String>,
    /// `rotatable` (default true): the placing designator rotates it.
    pub rotatable: bool,
    /// `defaultPlacingRot`.
    pub default_placing_rot: Option<String>,
    /// Holds up roofs within 6.9 cells (`holdsRoof`).
    pub holds_roof: bool,
    /// Stops light (`blockLight`).
    pub block_light: bool,
    /// Can be mined (`mineable`).
    pub mineable: bool,
    /// Share of the cost refunded when deconstructed
    /// (`resourcesFractionWhenDeconstructed`, default 0.5).
    pub resources_fraction_when_deconstructed: f32,
    /// `CompProperties_Glower`.
    pub glower: Option<GlowerProperties>,
    /// `CompProperties_Refuelable`.
    pub refuelable: Option<RefuelableProperties>,
    /// `CompProperties_HeatPusher`.
    pub heat_pusher: Option<HeatPusherProperties>,
    /// Has hit points (`useHitPoints`, default true).
    pub use_hit_points: bool,
    /// Apparel: the body part groups it covers (`apparel.bodyPartGroups`).
    pub apparel_groups: Vec<String>,
    /// Hit points it is made with, as a fraction of the maximum
    /// (`startingHpRange`, default 1~1).
    pub starting_hp_range: (f32, f32),
    /// Weather and exposure wear it down (`deteriorateFromEnvironmentalEffects`,
    /// default true).
    pub deteriorate_from_environmental_effects: bool,
    /// Has a `CompProperties_Power` (needs a power grid to run).
    pub needs_power: bool,
    /// `tickerType` (`Normal`, `Rare`, `Long`, `Never`).
    pub ticker_type: Option<String>,
    pub color: Option<Rgba>,
    /// `CompProperties_Rottable` among the comps.
    pub rottable: Option<RottableProperties>,
    /// A generated corpse def: the race (pawn `ThingDef`) it is the corpse
    /// of.
    pub corpse_of: Option<String>,
    /// A generated meat def: the race it comes from
    /// (`ingestible.sourceDef`).
    pub meat_of: Option<String>,
    /// Has `CompProperties_Forbiddable`: can be forbidden.
    pub forbiddable: bool,
    /// `CompProperties_Power` / `CompProperties_Battery`.
    pub power: Option<PowerProperties>,
    /// Has `CompProperties_Flickable` (an on/off switch).
    pub flickable: bool,
    /// Has `CompProperties_Breakdownable`.
    pub breakdownable: bool,
    /// `CompProperties_TempControl` (heaters, coolers).
    pub temp_control: Option<TempControlProperties>,
    /// Research needed before it can be built (`researchPrerequisites`).
    pub research_prerequisites: Vec<String>,
    /// Recipes done at it (`recipes`; `AllRecipes` adds recipes naming it
    /// among their `recipeUsers`).
    pub recipes: Vec<String>,
    /// `building.heatPerTickWhileWorking` (work tables).
    pub heat_per_tick_while_working: f32,
    /// Where a pawn stands to use it (`interactionCellOffset`, for a north
    /// rotation), when it has one (`hasInteractionCell`).
    pub interaction_cell_offset: Option<(i32, i32)>,
}

/// `CompProperties_Glower` (class field defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct GlowerProperties {
    /// `glowRadius` (default 14).
    pub radius: f32,
    /// `glowColor` r, g, b (default white × 1.45: 369 each).
    pub color: [i32; 3],
    /// `overlightRadius` (default 0).
    pub overlight_radius: f32,
}

/// `CompProperties_HeatPusher` (class field defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct HeatPusherProperties {
    pub heat_per_second: f32,
    /// Pushes only while the ambient temperature is below this
    /// (`heatPushMaxTemperature`, default 99999).
    pub max_temperature: f32,
    /// ... and above this (`heatPushMinTemperature`, default -99999).
    pub min_temperature: f32,
    /// `CompHeatPusherPowered`: also needs fuel and power.
    pub powered: bool,
}

/// `CompProperties_Refuelable` (the fields we use; class defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct RefuelableProperties {
    /// Fuel used per day (`fuelConsumptionRate`, default 1).
    pub consumption_rate: f32,
    /// `fuelCapacity` (default 2).
    pub capacity: f32,
    /// Fuel at spawn as a fraction of capacity (`initialFuelPercent`).
    pub initial_fuel_percent: f32,
    /// ThingDefs accepted as fuel (`fuelFilter` `thingDefs`).
    pub fuel_defs: Vec<String>,
    /// Burns only while used (`consumeFuelOnlyWhenUsed`: stoves).
    pub consume_fuel_only_when_used: bool,
}

/// `CompProperties_Power` (or `CompProperties_Battery`): how a building
/// takes part in a power net.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerProperties {
    /// `compClass` (`CompPowerTransmitter`, `CompPowerTrader`,
    /// `CompPowerPlant`, `CompPowerPlantSolar`, ..., `CompPowerBattery`).
    pub comp_class: String,
    /// `basePowerConsumption` in W (negative: produces).
    pub base_power_consumption: f32,
    /// `transmitsPower`: links to adjacent transmitters and carries a net.
    pub transmits_power: bool,
    /// `idlePowerDraw` (-1: none).
    pub idle_power_draw: f32,
    /// Batteries: `storedEnergyMax` (W·days) and `efficiency`.
    pub battery: Option<(f32, f32)>,
}

impl PowerProperties {
    pub fn is_battery(&self) -> bool {
        self.battery.is_some()
    }

    /// A `CompPowerTrader` (consumers and plants).
    pub fn is_trader(&self) -> bool {
        self.comp_class.starts_with("CompPowerTrader")
            || self.comp_class.starts_with("CompPowerPlant")
    }

    pub fn is_plant(&self) -> bool {
        self.comp_class.starts_with("CompPowerPlant")
    }
}

/// `CompProperties_TempControl`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempControlProperties {
    /// `energyPerSecond` (negative cools).
    pub energy_per_second: f32,
    pub default_target_temperature: f32,
    pub low_power_consumption_factor: f32,
}

/// `CompProperties_Rottable`: how a thing rots (class field defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct RottableProperties {
    pub days_to_rot_start: f32,
    /// Destroyed once it starts rotting (food) instead of turning rotten
    /// (corpses).
    pub rot_destroys: bool,
    pub rot_damage_per_day: f32,
    pub days_to_dessicated: f32,
    pub dessicated_damage_per_day: f32,
}

impl RottableProperties {
    /// `TicksToRotStart`.
    pub fn ticks_to_rot_start(&self) -> i32 {
        (self.days_to_rot_start * 60_000.0).round_ties_even() as i32
    }
}

/// `plant`: how a plant grows and is sown and harvested
/// (`PlantProperties`; defaults are the class field initializers).
#[derive(Debug, Clone, PartialEq)]
pub struct PlantProperties {
    pub sow_tags: Vec<String>,
    pub sow_work: f32,
    pub sow_min_skill: i32,
    pub harvest_work: f32,
    pub harvest_yield: f32,
    /// Reference to a `ThingDef`.
    pub harvested_thing_def: Option<String>,
    pub harvest_min_growth: f32,
    pub harvest_after_growth: f32,
    pub harvest_failable: bool,
    pub auto_harvestable: bool,
    pub grow_days: f32,
    pub lifespan_days_per_grow_days: f32,
    pub grow_min_glow: f32,
    pub grow_optimal_glow: f32,
    pub min_growth_temperature: f32,
    pub min_optimal_growth_temperature: f32,
    pub max_optimal_growth_temperature: f32,
    pub max_growth_temperature: f32,
    pub fertility_min: f32,
    pub fertility_sensitivity: f32,
    pub completely_ignore_fertility: bool,
    pub dies_to_light: bool,
    pub human_food_plant: bool,
    /// Dies instead of losing its leaves in the cold (`dieIfLeafless`).
    pub die_if_leafless: bool,
    /// Can't stand under a roof (`interferesWithRoof`, trees).
    pub interferes_with_roof: bool,
    /// What felling it leaves (`choppedThingDef`, a stump).
    pub chopped_thing_def: Option<String>,
    /// `visualSizeRange` (drawn size from no growth to full; default
    /// 0.9~1.1).
    pub visual_size_range: (f32, f32),
    /// A stump (`isStump`): a dead plant that doesn't grow.
    pub is_stump: bool,
    /// Weathers like an item (`canDeteriorate`).
    pub can_deteriorate: bool,
    /// `harvestTag` ("Standard" crops, "Wood" trees).
    pub harvest_tag: Option<String>,
    /// `forceIsTree`.
    pub force_is_tree: bool,
}

impl PlantProperties {
    /// `IsTree`: harvested for wood (or forced).
    pub fn is_tree(&self) -> bool {
        self.harvest_tag.as_deref() == Some("Wood") || self.force_is_tree
    }

    /// `Sowable`.
    pub fn sowable(&self) -> bool {
        !self.sow_tags.is_empty()
    }

    /// `Harvestable`.
    pub fn harvestable(&self) -> bool {
        self.harvest_yield > 0.001
    }

    /// `HarvestDestroys`.
    pub fn harvest_destroys(&self) -> bool {
        self.harvest_after_growth <= 0.0
    }

    /// `LifespanTicks` (truncated).
    pub fn lifespan_ticks(&self) -> i64 {
        (self.grow_days * self.lifespan_days_per_grow_days * 60_000.0) as i64
    }

    /// `LimitedLifespan`.
    pub fn limited_lifespan(&self) -> bool {
        self.lifespan_days_per_grow_days > 0.0
    }
}

/// `stuffProps`: how a material modifies what is made of it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StuffProperties {
    pub categories: Vec<String>,
    pub stat_factors: BTreeMap<String, f32>,
    pub stat_offsets: BTreeMap<String, f32>,
    pub color: Option<Rgba>,
    /// `appearance` (`StuffAppearanceDef`: Smooth, Planks, Bricks, Metal).
    pub appearance: Option<String>,
    /// `stuffAdjective` ("granite" for granite blocks; else the label).
    pub stuff_adjective: Option<String>,
    /// `commonality` (default 1).
    pub commonality: f32,
    /// `canSuggestUseDefaultStuff`.
    pub can_suggest_use_default_stuff: bool,
}

/// A skill's influence on a stat (`SkillNeed_BaseBonus` / `SkillNeed_Direct`).
#[derive(Debug, Clone, PartialEq)]
pub enum SkillNeed {
    /// baseValue + bonusPerLevel × level.
    BaseBonus {
        skill: String,
        base_value: f32,
        bonus_per_level: f32,
    },
    /// valuesPerLevel[level] (last entry beyond the list).
    Direct {
        skill: String,
        values_per_level: Vec<f32>,
    },
}

impl SkillNeed {
    pub fn skill(&self) -> &str {
        match self {
            SkillNeed::BaseBonus { skill, .. } | SkillNeed::Direct { skill, .. } => skill,
        }
    }

    pub fn value_at(&self, level: i32) -> f32 {
        match self {
            SkillNeed::BaseBonus {
                base_value,
                bonus_per_level,
                ..
            } => base_value + bonus_per_level * level as f32,
            SkillNeed::Direct {
                values_per_level, ..
            } => values_per_level
                .get(level.max(0) as usize)
                .or(values_per_level.last())
                .copied()
                .unwrap_or(1.0),
        }
    }
}

/// A stat (`StatDef`): the parts of its definition we evaluate.
/// `PawnCapacityFactor`.
#[derive(Debug, Clone, PartialEq)]
pub struct CapacityFactor {
    pub capacity: String,
    /// `weight` (default 1): how much of the factor applies.
    pub weight: f32,
    /// `max` (default 9999).
    pub max: f32,
    pub use_reciprocal: bool,
    /// `allowedDefect`: levels from 1 − this up count as whole.
    pub allowed_defect: f32,
}

impl CapacityFactor {
    /// `PawnCapacityFactor.GetFactor`.
    pub fn factor(&self, level: f32) -> f32 {
        let mut v = level;
        if self.allowed_defect != 0.0 && v < 1.0 {
            let span = 1.0 - self.allowed_defect;
            v = if span != 0.0 {
                (v / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        v = v.min(self.max);
        if self.use_reciprocal {
            v = if v.abs() < 0.001 {
                5.0
            } else {
                (1.0 / v).min(5.0)
            };
        }
        v
    }
}

#[derive(Debug, Clone)]
pub struct StatDef {
    pub def_name: String,
    pub default_base_value: f32,
    pub min_value: f32,
    pub max_value: f32,
    /// Values above this round to multiples of 5 (`roundToFiveOver`).
    pub round_to_five_over: f32,
    /// Other stats this one is multiplied by (`statFactors`).
    pub stat_factors: Vec<String>,
    pub skill_need_factors: Vec<SkillNeed>,
    pub skill_need_offsets: Vec<SkillNeed>,
    /// `capacityOffsets`: (capacity, scale, max).
    pub capacity_offsets: Vec<(String, f32, f32)>,
    /// `capacityFactors`.
    pub capacity_factors: Vec<CapacityFactor>,
    /// `postProcessCurve` points (x, y).
    pub post_process_curve: Vec<(f32, f32)>,
    /// `StatPart_Glow`: factor by the light on the thing's cell.
    pub glow_part: Option<GlowPart>,
    /// `StatPart_GearStatOffset`: (apparel stat, subtract) — worn apparel's
    /// value of that stat is added (or subtracted).
    pub gear_offset: Option<(String, bool)>,
    /// `StatPart_Stuff`: (stuff power stat, multiplier stat) — adds the
    /// stuff's power times the thing's multiplier.
    pub stuff_part: Option<(String, String)>,
    /// `StatPart_EnvironmentalEffects`: (factorOffsetUnroofed,
    /// factorOffsetOutdoors).
    pub environmental_effects: Option<(f32, f32)>,
}

/// `StatPart_Glow`.
#[derive(Debug, Clone, PartialEq)]
pub struct GlowPart {
    pub humanlike_only: bool,
    /// `factorFromGlowCurve` points (glow, factor).
    pub curve: Vec<(f32, f32)>,
}

/// Parses SimpleCurve `<points>` (`(x, y)` items).
fn curve_points(points: &XmlNode) -> Vec<(f32, f32)> {
    points
        .children
        .iter()
        .filter_map(|li| {
            let t = li
                .text
                .as_deref()?
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')');
            let (x, y) = t.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect()
}

/// `VerbProperties` (the parts ranged verbs use).
#[derive(Debug, Clone)]
pub struct VerbProperties {
    pub verb_class: Option<String>,
    /// `isPrimary` (default true).
    pub is_primary: bool,
    pub warmup_time: f32,
    pub range: f32,
    pub min_range: f32,
    /// `burstShotCount` (default 1).
    pub burst_shot_count: i32,
    /// `ticksBetweenBurstShots` (default 15).
    pub ticks_between_burst_shots: i32,
    pub default_projectile: Option<String>,
    /// `requireLineOfSight` (default true).
    pub require_line_of_sight: bool,
    /// `stopBurstWithoutLos` (default true).
    pub stop_burst_without_los: bool,
    /// `canGoWild` (default true).
    pub can_go_wild: bool,
    pub forced_miss_radius: f32,
    pub only_manual_cast: bool,
    /// `defaultCooldownTime` (used without equipment).
    pub default_cooldown_time: f32,
}

/// `ProjectileProperties` (ordinary bullets).
#[derive(Debug, Clone)]
pub struct ProjectileProperties {
    pub damage_def: Option<String>,
    /// `damageAmountBase` (-1: the DamageDef's default).
    pub damage_amount_base: i32,
    /// `armorPenetrationBase` (-1: derived).
    pub armor_penetration_base: f32,
    /// `speed` (default 5; tiles per tick = speed / 100).
    pub speed: f32,
    /// `stoppingPower` (default 0.5).
    pub stopping_power: f32,
    pub fly_overhead: bool,
    pub always_free_intercept: bool,
    pub explosion_radius: f32,
}

/// A melee tool of a thing or race (`Tool`).
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub label: String,
    /// `ToolCapacityDef`s (e.g. Blunt, Bite).
    pub capacities: Vec<String>,
    pub power: f32,
    pub cooldown_time: f32,
    pub chance_factor: f32,
    /// `armorPenetration` (-1: derived from damage).
    pub armor_penetration: f32,
    /// Reference to a `BodyPartGroupDef`.
    pub linked_body_parts_group: Option<String>,
    pub ensure_linked_body_parts_group_always_usable: bool,
}

/// `BiomeDef` (the parts map generation uses).
#[derive(Debug, Clone)]
pub struct BiomeDef {
    pub def_name: String,
    pub label: String,
    pub plant_density: f32,
    pub wild_plant_regrow_days: f32,
    /// `terrainsByFertility`: (terrain, min, max) in order.
    pub terrains_by_fertility: Vec<(String, f32, f32)>,
    /// `wildPlants`: (plant, commonality) in order.
    pub wild_plants: Vec<(String, f32)>,
    /// `animalDensity`.
    pub animal_density: f32,
    /// `wildAnimals`: (pawn kind, commonality) in order.
    pub wild_animals: Vec<(String, f32)>,
    /// `wildAnimalScariaChance`.
    pub wild_animal_scaria_chance: f32,
    /// `constantOutdoorTemperature`.
    pub constant_outdoor_temperature: Option<f32>,
}

impl BiomeDef {
    fn from_def(def: &Def) -> Self {
        let n = &def.node;
        let num = |k: &str, d: f32| {
            n.child_text(k)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d)
        };
        Self {
            def_name: def.def_name.clone(),
            label: n.child_text("label").unwrap_or(&def.def_name).to_owned(),
            plant_density: num("plantDensity", 0.0),
            wild_plant_regrow_days: num("wildPlantRegrowDays", 25.0),
            terrains_by_fertility: n
                .child("terrainsByFertility")
                .map(|t| {
                    t.children
                        .iter()
                        .map(|li| {
                            let f = |k: &str, d: f32| {
                                li.child_text(k)
                                    .and_then(|v| v.trim().parse().ok())
                                    .unwrap_or(d)
                            };
                            (
                                li.child_text("terrain").unwrap_or_default().to_owned(),
                                f("min", -1000.0),
                                f("max", 1000.0),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            animal_density: num("animalDensity", 0.0),
            wild_animals: n
                .child("wildAnimals")
                .map(|w| {
                    w.children
                        .iter()
                        .filter_map(|c| {
                            Some((c.name.clone(), c.text.as_deref()?.trim().parse().ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            wild_animal_scaria_chance: num("wildAnimalScariaChance", 0.0),
            constant_outdoor_temperature: n
                .child_text("constantOutdoorTemperature")
                .and_then(|v| v.trim().parse().ok()),
            wild_plants: n
                .child("wildPlants")
                .map(|w| {
                    w.children
                        .iter()
                        .filter_map(|c| {
                            Some((c.name.clone(), c.text.as_deref()?.trim().parse().ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// `TerrainThreshold.TerrainAtValue` over `terrainsByFertility`.
    pub fn terrain_at_fertility(&self, value: f32) -> Option<&str> {
        self.terrains_by_fertility
            .iter()
            .find(|(_, min, max)| *min <= value && *max >= value)
            .map(|(t, _, _)| t.as_str())
    }
}

/// `ManeuverDef`: which damage a tool capacity deals in melee.
#[derive(Debug, Clone)]
pub struct ManeuverDef {
    pub def_name: String,
    pub required_capacity: String,
    /// Reference to a `DamageDef`.
    pub melee_damage_def: Option<String>,
}

impl ManeuverDef {
    fn from_def(def: &Def) -> Self {
        Self {
            def_name: def.def_name.clone(),
            required_capacity: def
                .node
                .child_text("requiredCapacity")
                .unwrap_or_default()
                .to_owned(),
            melee_damage_def: def
                .node
                .path_text(&["verb", "meleeDamageDef"])
                .map(str::to_owned),
        }
    }
}

/// `<filth>`: how a filth thing is cleaned (game defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct FilthProperties {
    /// Cleaning work per thickness level (default 35).
    pub cleaning_work_to_reduce_thickness: f32,
    /// Days until it disappears by itself (0~0 = never).
    pub disappears_in_days: (f32, f32),
    /// Thickest it can get (default 100; thickening also stops at 5).
    pub max_thickness: u32,
    /// Where it may be placed (`placementMask`, default Unnatural).
    pub placement_mask: u8,
    /// `ignoreFilthMultiplierStat`.
    pub ignore_filth_multiplier_stat: bool,
    /// Pawns walking through pick it up (`canFilthAttach`).
    pub can_filth_attach: bool,
}

/// `FilthSourceFlags`.
pub mod filth_flags {
    pub const TERRAIN: u8 = 1;
    pub const NATURAL: u8 = 2;
    pub const UNNATURAL: u8 = 4;
    pub const PAWN: u8 = 8;
    pub const ANY: u8 = 15;
}

/// Parses a `FilthSourceFlags` list (`<li>Terrain</li>...`); `None` if the
/// node is absent.
fn filth_flags_in(node: Option<&crate::xml::XmlNode>) -> Option<u8> {
    let n = node?;
    Some(
        n.children
            .iter()
            .filter_map(|c| c.text.as_deref())
            .map(|t| match t.trim() {
                "Terrain" => filth_flags::TERRAIN,
                "Natural" => filth_flags::NATURAL,
                "Unnatural" => filth_flags::UNNATURAL,
                "Pawn" => filth_flags::PAWN,
                "Any" => filth_flags::ANY,
                _ => 0,
            })
            .fold(0, |a, b| a | b),
    )
}

impl ThingDef {
    /// `ConnectToPower`: a power user that doesn't transmit connects to a
    /// nearby transmitter.
    pub fn connect_to_power(&self) -> bool {
        self.power
            .as_ref()
            .is_some_and(|p| !p.transmits_power && (p.is_trader() || p.is_battery()))
    }

    /// `building.isEdifice` (things without building properties count as
    /// edifices when they are buildings).
    pub fn is_edifice(&self) -> bool {
        self.building.as_ref().is_none_or(|b| b.is_edifice)
    }

    /// A bed (`Building_Bed` or a subclass).
    pub fn is_bed(&self) -> bool {
        self.thing_class
            .as_deref()
            .is_some_and(|c| c.ends_with("Building_Bed"))
    }

    /// A door (`Building_Door` or a subclass).
    pub fn is_door(&self) -> bool {
        self.thing_class
            .as_deref()
            .is_some_and(|c| c.ends_with("Building_Door") || c.ends_with("Building_MultiTileDoor"))
    }

    /// `MadeFromStuff`.
    pub fn made_from_stuff(&self) -> bool {
        !self.stuff_categories.is_empty()
    }

    /// Whether `stuff` is a valid material for this thing.
    pub fn accepts_stuff(&self, stuff: &ThingDef) -> bool {
        stuff.stuff_props.as_ref().is_some_and(|p| {
            p.categories
                .iter()
                .any(|c| self.stuff_categories.contains(c))
        })
    }

    /// `EverHaulable`: always haulable or haulable by designation.
    pub fn ever_haulable(&self) -> bool {
        self.always_haulable || self.designate_haulable
    }

    /// `EverStorable` (without minifying): an item with categories.
    pub fn ever_storable(&self) -> bool {
        // Minified things (and their MinifiedTree subclass) are storable
        // by class.
        self.thing_class
            .as_deref()
            .is_some_and(|c| c.ends_with("MinifiedThing") || c.ends_with("MinifiedTree"))
            || (!self.thing_categories.is_empty() && self.category.as_deref() == Some("Item"))
    }

    /// `VolumePerUnit`.
    pub fn volume_per_unit(&self) -> f32 {
        if self.small_volume { 0.1 } else { 1.0 }
    }

    pub fn stat(&self, stat: &str) -> Option<f32> {
        self.stat_bases.get(stat).copied()
    }
    pub fn is_pawn(&self) -> bool {
        self.category.as_deref() == Some("Pawn") && self.race.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct PawnKindDef {
    pub def_name: String,
    pub label: String,
    /// Reference to the race `ThingDef`.
    pub race: Option<String>,
    pub combat_power: f32,
    /// Reference to a `FactionDef`, e.g. `PlayerColony` for colonists.
    pub default_faction: Option<String>,
    /// The last life stage's `bodyGraphicData` (animals): texture path and
    /// draw size.
    pub adult_body: Option<(String, (f32, f32))>,
    /// `ecoSystemWeight` (default 1): a wild animal's share of the map's
    /// ecosystem.
    pub eco_system_weight: f32,
    /// `wildGroupSize` (default 1~1).
    pub wild_group_size: (i32, i32),
}

/// A pawn need (`NeedDef`), e.g. Food or Rest. Fall/gain rates live in the
/// game's code (`needClass`); the simulation implements them per class.
#[derive(Debug, Clone)]
pub struct NeedDef {
    pub def_name: String,
    pub label: String,
    /// e.g. `Need_Food`, `Need_Rest`.
    pub need_class: Option<String>,
    pub major: bool,
    /// Display order (higher first).
    pub list_priority: i32,
    /// Seekers (mood): the level with no thoughts (`baseLevel`, default
    /// 0.5) and the hourly approach rates.
    pub base_level: f32,
    pub seeker_rise_per_hour: f32,
    pub seeker_fall_per_hour: f32,
    pub freeze_while_sleeping: bool,
    pub freeze_in_mental_state: bool,
}

/// What a pawn is doing (`JobDef`). Behaviour is implemented in the
/// simulation's job drivers; the Def supplies identity and UI text.
#[derive(Debug, Clone)]
pub struct JobDef {
    pub def_name: String,
    /// e.g. "moving.", "wandering." (shown in the inspector).
    pub report_string: String,
    /// The game's driver class name, e.g. `JobDriver_Goto`.
    pub driver_class: Option<String>,
    pub is_idle: bool,
    /// Recreation: how long the activity lasts (`joyDuration`, default
    /// 4000), its gain rate (`joyGainRate`, default 1), its `joyKind`, how
    /// many may share it (`joyMaxParticipants`, default 1) and the skill it
    /// trains (`joySkill`, `joyXpPerTick`).
    pub joy_duration: i32,
    pub joy_gain_rate: f32,
    pub joy_kind: Option<String>,
    pub joy_max_participants: i32,
    pub joy_skill: Option<String>,
    pub joy_xp_per_tick: f32,
}

/// `JoyGiverDef`: a kind of recreation a pawn may pick.
#[derive(Debug, Clone)]
pub struct JoyGiverDef {
    pub def_name: String,
    /// The worker class (`giverClass`).
    pub giver_class: String,
    pub base_chance: f32,
    /// Share of pawns that ever do it (`pctPawnsEverDo`, default 1).
    pub pct_pawns_ever_do: f32,
    pub joy_kind: Option<String>,
    pub job: Option<String>,
    /// Buildings or items it uses (`thingDefs`).
    pub thing_defs: Vec<String>,
    pub required_capacities: Vec<String>,
    pub requires_enjoy_outdoors: bool,
    pub can_do_while_in_bed: bool,
    pub desire_sit: bool,
    pub unroofed_only: bool,
}

/// `JoyKindDef`.
#[derive(Debug, Clone)]
pub struct JoyKindDef {
    pub def_name: String,
    pub label: String,
    /// Only available with a thing that provides it (`needsThing`, default
    /// true).
    pub needs_thing: bool,
}

/// `ExpectationDef` (wealth-triggered ones).
#[derive(Debug, Clone)]
pub struct ExpectationDef {
    pub def_name: String,
    pub label: String,
    pub order: i32,
    /// Below this map wealth (`maxMapWealth`; none for role expectations).
    pub max_map_wealth: Option<f32>,
    pub joy_kinds_needed: i32,
    pub joy_tolerance_drop_per_day: f32,
    /// The `Expectations` thought's stage (`thoughtStage`).
    pub thought_stage: Option<usize>,
}

/// One stage of a `ThoughtDef` (`ThoughtStage`).
#[derive(Debug, Clone)]
pub struct ThoughtStage {
    pub label: String,
    pub base_mood_effect: f32,
    pub visible: bool,
}

/// `ThoughtDef`: a memory or situational thought.
#[derive(Debug, Clone)]
pub struct ThoughtDef {
    pub def_name: String,
    /// `thoughtClass` (none: `Thought_Memory` for memories, else
    /// `Thought_Situational`).
    pub thought_class: Option<String>,
    /// `workerClass`: situational thoughts have one.
    pub worker_class: Option<String>,
    /// Stages; `None` for the Defs' `IsNull` slots (indices stay meaningful).
    pub stages: Vec<Option<ThoughtStage>>,
    /// `stackLimit` (default 1).
    pub stack_limit: i32,
    /// `stackedEffectMultiplier` (default 0.75).
    pub stacked_effect_multiplier: f32,
    pub stages_stack: bool,
    pub duration_days: f32,
    pub invert: bool,
    pub valid_while_despawned: bool,
    pub next_thought: Option<String>,
    pub produces_memory_thought: Option<String>,
    pub nullifying_traits: Vec<String>,
    pub nullifying_hediffs: Vec<String>,
    pub required_traits: Vec<String>,
    pub required_hediffs: Vec<String>,
    /// The hediff a `ThoughtWorker_Hediff` looks for (`hediff`).
    pub hediff: Option<String>,
    pub nullified_if_not_colonist: bool,
    pub thought_to_make: Option<String>,
    pub show_bubble: bool,
    pub min_expectation: Option<String>,
    pub replace_thoughts: Vec<String>,
    /// `developmentalStageFilter` includes adults (default Child, Adult).
    pub for_adults: bool,
    pub lerp_mood_to_zero: bool,
    pub stack_limit_for_same_other_pawn: i32,
    /// `gender` set (only that gender gets it).
    pub gender: Option<String>,
    pub effect_multiplying_stat: Option<String>,
}

impl ThoughtDef {
    /// `DurationTicks`: `durationDays × 60000`, truncated.
    pub fn duration_ticks(&self) -> i32 {
        (self.duration_days * 60000.0) as i32
    }

    /// `IsMemory`: a duration, or a memory class.
    pub fn is_memory(&self) -> bool {
        self.duration_days > 0.0
            || self
                .thought_class
                .as_deref()
                .is_some_and(|c| c.contains("Thought_Memory") || c == "Thought_FoodEaten")
    }

    /// `IsSituational`: has a worker.
    pub fn is_situational(&self) -> bool {
        self.worker_class.is_some()
    }

    /// `IsSocial`: a social thought class.
    pub fn is_social(&self) -> bool {
        self.thought_class
            .as_deref()
            .is_some_and(|c| c.contains("Social"))
    }

    pub fn stage(&self, index: usize) -> Option<&ThoughtStage> {
        self.stages.get(index).and_then(|s| s.as_ref())
    }
}

/// `MentalBreakIntensity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MentalBreakIntensity {
    None,
    Minor,
    Major,
    Extreme,
}

/// `MentalBreakDef`.
#[derive(Debug, Clone)]
pub struct MentalBreakDef {
    pub def_name: String,
    pub worker_class: Option<String>,
    pub mental_state: Option<String>,
    pub base_commonality: f32,
    /// `commonalityFactorPerPopulationCurve` points.
    pub commonality_factor_per_population: Vec<(f32, f32)>,
    pub intensity: MentalBreakIntensity,
    pub anomalous_break: bool,
    pub required_trait: Option<String>,
}

/// `MentalStateDef`.
#[derive(Debug, Clone)]
pub struct MentalStateDef {
    pub def_name: String,
    pub label: String,
    pub state_class: Option<String>,
    pub worker_class: Option<String>,
    pub colonists_only: bool,
    pub downed_can_do: bool,
    pub stops_jobs: bool,
    /// `recoveryMtbDays` (default 1).
    pub recovery_mtb_days: f32,
    /// `minTicksBeforeRecovery` (default 500).
    pub min_ticks_before_recovery: i32,
    /// `maxTicksBeforeRecovery` (default 99,999,999).
    pub max_ticks_before_recovery: i32,
    pub recover_from_sleep: bool,
    pub recover_from_downed: bool,
    pub mood_recovery_thought: Option<String>,
    pub base_inspect_line: Option<String>,
    pub required_capacities: Vec<String>,
}

/// A kind of work colonists can be assigned (`WorkTypeDef`).
#[derive(Debug, Clone)]
pub struct WorkTypeDef {
    pub def_name: String,
    pub label: String,
    /// Orders work types of equal player priority (higher first).
    pub natural_priority: i32,
    /// Enabled for every new colonist that can do it.
    pub always_start_active: bool,
    pub work_tags: Vec<String>,
    pub relevant_skills: Vec<String>,
    /// This type's work givers by descending `priorityInType`, ties in
    /// WorkGiverDef database order (`WorkTypeDef.ResolveReferences`).
    pub givers_by_priority: Vec<DefId<WorkGiverDef>>,
    /// `gerundLabel` (e.g. "mining"), for float menu reasons.
    pub gerund_label: String,
    /// `pawnLabel` (e.g. "miner").
    pub pawn_label: String,
    /// `labelShort` (the Work tab's column label; default the label).
    pub label_short: String,
    pub description: String,
    /// `visible` (default true): has a Work tab column.
    pub visible: bool,
}

/// One source of work within a work type (`WorkGiverDef`).
#[derive(Debug, Clone)]
pub struct WorkGiverDef {
    pub def_name: String,
    pub label: String,
    /// The game's worker class, e.g. `WorkGiver_CleanFilth`.
    pub giver_class: Option<String>,
    /// Reference to a `WorkTypeDef`.
    pub work_type: Option<String>,
    pub priority_in_type: i32,
    pub emergency: bool,
    /// Scan things for work (default true).
    pub scan_things: bool,
    /// Scan cells for work (default false).
    pub scan_cells: bool,
    pub non_colonists_can_do: bool,
    pub work_tags: Vec<String>,
    pub required_capacities: Vec<String>,
    /// Job tag for the produced job (default `MiscWork`).
    pub tag_to_give: String,
    /// `WorkGiver_DoBill`: the buildings whose bills it does
    /// (`fixedBillGiverDefs`).
    pub fixed_bill_giver_defs: Vec<String>,
    /// Float menu wording: `verb` ("mine") and `gerund` ("mining").
    pub verb: String,
    pub gerund: String,
    /// Offered as a "Prioritize" float menu order (default true).
    pub direct_orderable: bool,
    /// Offered to drafted pawns too.
    pub can_be_done_while_drafted: bool,
    /// `prioritizeSustains`: a prioritized job keeps the pawn on this
    /// giver's work near the cell.
    pub prioritize_sustains: bool,
    /// `WorkGiverEquivalenceGroupDef`: one float menu option per group.
    pub equivalence_group: Option<String>,
    /// Drafted float menus take this option without asking (-1: never).
    pub auto_takeable_priority_drafted: i32,
}

macro_rules! impl_def_name {
    ($($t:ty),*) => {$(
        impl HasDefName for $t {
            fn def_name(&self) -> &str { &self.def_name }
        }
    )*};
}
/// A `Vector2` written as `(x,y)`.
fn parse_vec2(v: &str) -> Option<(f32, f32)> {
    let (x, y) = v
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// An `IntVec3` written as `(x,y,z)`: its x and z.
fn parse_int_vec3_xz(v: &str) -> Option<(i32, i32)> {
    let parts: Vec<&str> = v
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(',')
        .map(str::trim)
        .collect();
    match parts.as_slice() {
        [x, _, z] | [x, z] => Some((x.parse().ok()?, z.parse().ok()?)),
        _ => None,
    }
}

/// `TechLevel`, in enum order (`Undefined` 0 to `Archotech` 7).
pub fn tech_level(name: &str) -> i32 {
    match name.trim() {
        "Animal" => 1,
        "Neolithic" => 2,
        "Medieval" => 3,
        "Industrial" => 4,
        "Spacer" => 5,
        "Ultra" => 6,
        "Archotech" => 7,
        _ => 0,
    }
}

/// A `ThingFilter` as written in XML (resolved by the simulation, in
/// `ThingFilter.ResolveReferences` order).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThingFilterSpec {
    pub thing_defs: Vec<String>,
    pub categories: Vec<String>,
    pub disallowed_categories: Vec<String>,
    pub special_filters_to_allow: Vec<String>,
    pub special_filters_to_disallow: Vec<String>,
    pub disallowed_thing_defs: Vec<String>,
    /// `disallowDoesntProduceMeat`: races (and their corpses) without
    /// meat are disallowed.
    pub disallow_doesnt_produce_meat: bool,
}

impl ThingFilterSpec {
    pub fn from_node(n: &XmlNode) -> Self {
        Self {
            thing_defs: n.child_list_texts("thingDefs"),
            categories: n.child_list_texts("categories"),
            disallowed_categories: n.child_list_texts("disallowedCategories"),
            special_filters_to_allow: n.child_list_texts("specialFiltersToAllow"),
            special_filters_to_disallow: n.child_list_texts("specialFiltersToDisallow"),
            disallowed_thing_defs: n.child_list_texts("disallowedThingDefs"),
            disallow_doesnt_produce_meat: n
                .child_text("disallowDoesntProduceMeat")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
        }
    }

    /// `IngredientCount.IsFixedIngredient`-style test: exactly one ThingDef
    /// and nothing else.
    pub fn single_def(&self) -> Option<&str> {
        (self.thing_defs.len() == 1
            && self.categories.is_empty()
            && self.disallowed_categories.is_empty()
            && self.special_filters_to_allow.is_empty()
            && self.special_filters_to_disallow.is_empty()
            && self.disallowed_thing_defs.is_empty())
        .then(|| self.thing_defs[0].as_str())
    }
}

/// `SpecialThingFilterDef`.
#[derive(Debug, Clone)]
pub struct SpecialThingFilterDef {
    pub def_name: String,
    pub parent_category: Option<String>,
    pub allowed_by_default: bool,
    pub worker_class: Option<String>,
}

/// One `IngredientCount` of a recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct IngredientCountDef {
    pub filter: ThingFilterSpec,
    /// `count` (nutrition for nutrition recipes).
    pub count: f32,
}

/// `RecipeDef`.
#[derive(Debug, Clone)]
pub struct RecipeDef {
    pub def_name: String,
    pub label: String,
    pub job_string: String,
    /// `workAmount` (negative: from the product's `WorkToMake`).
    pub work_amount: f32,
    pub work_speed_stat: Option<String>,
    /// `workTableSpeedStat` (default `WorkTableWorkSpeedFactor`).
    pub work_table_speed_stat: Option<String>,
    pub efficiency_stat: Option<String>,
    /// `workTableEfficiencyStat` (default `WorkTableEfficiencyFactor`).
    pub work_table_efficiency_stat: Option<String>,
    pub work_skill: Option<String>,
    pub work_skill_learn_factor: f32,
    pub allow_mixing_ingredients: bool,
    pub ingredient_value_getter_class: Option<String>,
    pub ingredients: Vec<IngredientCountDef>,
    /// `products`: (ThingDef, count).
    pub products: Vec<(String, i32)>,
    pub fixed_ingredient_filter: ThingFilterSpec,
    pub default_ingredient_filter: Option<ThingFilterSpec>,
    pub required_giver_work_type: Option<String>,
    /// `skillRequirements`: (skill, minimum level).
    pub skill_requirements: Vec<(String, i32)>,
    pub research_prerequisite: Option<String>,
    pub research_prerequisites: Vec<String>,
    pub recipe_users: Vec<String>,
    pub unfinished_thing_def: Option<String>,
    pub special_products: Vec<String>,
    pub display_priority: i32,
    /// `workerCounterClass` (e.g. `RecipeWorkerCounter_ButcherAnimals`).
    pub worker_counter_class: Option<String>,
    /// `ignoreIngredientCountTakeEntireStacks`.
    pub ignore_ingredient_count_take_entire_stacks: bool,
}

/// `ResearchProjectDef`.
#[derive(Debug, Clone)]
pub struct ResearchProjectDef {
    pub def_name: String,
    pub label: String,
    pub description: String,
    /// `baseCost` (research points).
    pub base_cost: f32,
    /// `techLevel` as [`tech_level`].
    pub tech_level: i32,
    pub prerequisites: Vec<String>,
    pub hidden_prerequisites: Vec<String>,
    /// Only researchable at this bench (`requiredResearchBuilding`).
    pub required_research_building: Option<String>,
    /// Facilities linked to the bench (`requiredResearchFacilities`).
    pub required_research_facilities: Vec<String>,
    pub tags: Vec<String>,
    /// Position in the research tree (`researchViewX`, `researchViewY`).
    pub view: (f32, f32),
}

/// `ThingCategoryDef`: a node of the item category tree (storage filters).
#[derive(Debug, Clone)]
pub struct ThingCategoryDef {
    pub def_name: String,
    /// The parent category (`parent`; none for Root).
    pub parent: Option<String>,
}

impl_def_name!(
    RecipeDef,
    SpecialThingFilterDef,
    JoyGiverDef,
    JoyKindDef,
    ThoughtDef,
    MentalBreakDef,
    MentalStateDef,
    ResearchProjectDef,
    ThingCategoryDef,
    BiomeDef,
    ManeuverDef,
    StatDef,
    TerrainDef,
    ThingDef,
    PawnKindDef,
    JobDef,
    NeedDef,
    WorkTypeDef,
    WorkGiverDef
);

/// Reads typed fields from a Def, collecting warnings for malformed values.
struct Fields<'a> {
    def: &'a Def,
    warnings: &'a mut Vec<String>,
}

impl<'a> Fields<'a> {
    fn node(&self) -> &'a XmlNode {
        &self.def.node
    }

    fn warn(&mut self, field: &str, value: &str, expected: &str) {
        self.warnings.push(format!(
            "{} {} ({}): field {field} = {value:?} is not a valid {expected}",
            self.def.def_type, self.def.def_name, self.def.file
        ));
    }

    fn string(&self, field: &str) -> Option<String> {
        self.node().child_text(field).map(str::to_owned)
    }

    fn parse_in<T: std::str::FromStr>(&mut self, node: Option<&XmlNode>, field: &str) -> Option<T> {
        let text = node?.child_text(field)?;
        match text.parse() {
            Ok(v) => Some(v),
            Err(_) => {
                let text = text.to_owned();
                self.warn(field, &text, std::any::type_name::<T>());
                None
            }
        }
    }

    fn parse<T: std::str::FromStr>(&mut self, field: &str) -> Option<T> {
        let node = &self.def.node;
        self.parse_in(Some(node), field)
    }

    fn bool_in(&mut self, node: Option<&XmlNode>, field: &str) -> Option<bool> {
        let text = node?.child_text(field)?;
        let v = parse_bool(text);
        if v.is_none() {
            let text = text.to_owned();
            self.warn(field, &text, "bool");
        }
        v
    }

    fn bool(&mut self, field: &str) -> Option<bool> {
        let node = &self.def.node;
        self.bool_in(Some(node), field)
    }

    fn color_in(&mut self, node: Option<&XmlNode>, field: &str) -> Option<Rgba> {
        let text = node?.child_text(field)?;
        let v = parse_color(text);
        if v.is_none() {
            let text = text.to_owned();
            self.warn(field, &text, "color");
        }
        v
    }

    fn food_type_in(&mut self, node: &XmlNode) -> u32 {
        let Some(text) = node.child_text("foodType") else {
            return food_type::NONE;
        };
        food_type::parse(text).unwrap_or_else(|| {
            let text = text.to_owned();
            self.warn("foodType", &text, "FoodTypeFlags");
            food_type::NONE
        })
    }

    fn preferability_in(&mut self, node: &XmlNode) -> FoodPreferability {
        let Some(text) = node.child_text("preferability") else {
            return FoodPreferability::default();
        };
        FoodPreferability::parse(text).unwrap_or_else(|| {
            let text = text.to_owned();
            self.warn("preferability", &text, "FoodPreferability");
            FoodPreferability::default()
        })
    }

    fn passability(&mut self) -> Passability {
        match self.node().child_text("passability") {
            None => Passability::default(),
            Some(text) => Passability::parse(text).unwrap_or_else(|| {
                let text = text.to_owned();
                self.warn("passability", &text, "Traversability");
                Passability::default()
            }),
        }
    }

    fn label(&self) -> String {
        self.string("label")
            .unwrap_or_else(|| self.def.def_name.clone())
    }

    fn float_map(&mut self, field: &str) -> BTreeMap<String, f32> {
        let mut map = BTreeMap::new();
        let Some(node) = self.def.node.child(field) else {
            return map;
        };
        for entry in &node.children {
            match entry.text.as_deref().map(str::parse::<f32>) {
                Some(Ok(v)) => {
                    map.insert(entry.name.clone(), v);
                }
                _ => self.warnings.push(format!(
                    "{} {}: {field}/{} is not a number",
                    self.def.def_type, self.def.def_name, entry.name
                )),
            }
        }
        map
    }
}

impl TerrainDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            texture_path: f.string("texturePath"),
            path_cost: f.parse("pathCost").unwrap_or(0),
            passability: f.passability(),
            fertility: f.parse("fertility").unwrap_or(0.0),
            affordances: def.node.child_list_texts("affordances"),
            color: {
                let node = &def.node;
                f.color_in(Some(node), "color")
            },
            category_type: f.string("categoryType"),
            render_precedence: f.parse("renderPrecedence").unwrap_or(0),
            natural: f.bool("natural").unwrap_or(false),
            avoid_wander: f.bool("avoidWander").unwrap_or(false),
            stat_bases: f.float_map("statBases"),
            cost_list: def
                .node
                .child("costList")
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|c| Some((c.name.clone(), c.text.as_deref()?.parse().ok()?)))
                        .collect()
                })
                .unwrap_or_default(),
            designation_category: f.string("designationCategory"),
            terrain_affordance_needed: f.string("terrainAffordanceNeeded"),
            construction_skill_prerequisite: f.parse("constructionSkillPrerequisite").unwrap_or(0),
            layerable: f.bool("layerable").unwrap_or(false),
            generated_filth: f.string("generatedFilth"),
            smoothed_terrain: f.string("smoothedTerrain"),
            resources_fraction_when_deconstructed: f
                .parse("resourcesFractionWhenDeconstructed")
                .unwrap_or(0.5),
            extra_deterioration_factor: f.parse("extraDeteriorationFactor").unwrap_or(0.0),
            filth_acceptance_mask: filth_flags_in(def.node.child("filthAcceptanceMask"))
                .unwrap_or(filth_flags::ANY),
            ui_order: f.parse("uiOrder").unwrap_or(2999.0),
            draw_style_category: f.string("drawStyleCategory").map(|s| s.trim().to_owned()),
            research_prerequisites: def.node.child_list_texts("researchPrerequisites"),
            designator_dropdown: f.string("designatorDropdown").map(|s| s.trim().to_owned()),
        }
    }
}

impl ThingDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        let graphic = def
            .node
            .child("graphicData")
            .filter(|n| !n.is_null())
            .map(|g| GraphicData {
                tex_path: g.child_text("texPath").map(str::to_owned),
                graphic_class: g.child_text("graphicClass").map(str::to_owned),
                color: f.color_in(Some(g), "color"),
                draw_size: g
                    .child_text("drawSize")
                    .and_then(parse_vec2)
                    .unwrap_or((1.0, 1.0)),
                shader_type: g.child_text("shaderType").map(|s| s.trim().to_owned()),
                draw_rotated: g
                    .child_text("drawRotated")
                    .is_none_or(|v| v.trim() != "false"),
                link_type: g.child_text("linkType").map(|s| s.trim().to_owned()),
                link_flags: g.child_list_texts("linkFlags"),
            });
        let race = def
            .node
            .child("race")
            .filter(|n| !n.is_null())
            .map(|r| RaceProperties {
                intelligence: r.child_text("intelligence").map(str::to_owned),
                body: r.child_text("body").map(str::to_owned),
                base_body_size: f.parse_in(Some(r), "baseBodySize").unwrap_or(1.0),
                think_tree_main: r.child_text("thinkTreeMain").map(str::to_owned),
                base_hunger_rate: f.parse_in(Some(r), "baseHungerRate").unwrap_or(1.0),
                base_health_scale: f.parse_in(Some(r), "baseHealthScale").unwrap_or(1.0),
                bleed_rate_factor: f.parse_in(Some(r), "bleedRateFactor").unwrap_or(1.0),
                flesh_type: r.child_text("fleshType").map(str::to_owned),
                blood_def: r.child_text("bloodDef").map(str::to_owned),
                food_type: f.food_type_in(r),
                has_corpse: f.bool_in(Some(r), "hasCorpse").unwrap_or(true),
                manhunter_on_damage_chance: f
                    .parse_in(Some(r), "manhunterOnDamageChance")
                    .unwrap_or(0.0),
                herd_animal: f.bool_in(Some(r), "herdAnimal").unwrap_or(false),
                execution_range: f.parse_in(Some(r), "executionRange").unwrap_or(2.0),
                think_tree_constant: r.child_text("thinkTreeConstant").map(str::to_owned),
                has_meat: f.bool_in(Some(r), "hasMeat").unwrap_or(true),
                meat_label: r.child_text("meatLabel").map(|s| s.trim().to_owned()),
                meat_color: f.color_in(Some(r), "meatColor"),
                meat_market_value: f.parse_in(Some(r), "meatMarketValue").unwrap_or(2.0),
                use_meat_from: r.child_text("useMeatFrom").map(|s| s.trim().to_owned()),
                specific_meat_def: r.child_text("specificMeatDef").map(|s| s.trim().to_owned()),
                use_leather_from: r.child_text("useLeatherFrom").map(|s| s.trim().to_owned()),
                leather_def: r.child_text("leatherDef").map(|s| s.trim().to_owned()),
                meat_def: None,
                wild_biomes: r
                    .child("wildBiomes")
                    .map(|w| {
                        w.children
                            .iter()
                            .filter_map(|li| {
                                Some((
                                    li.child_text("biome")?.trim().to_owned(),
                                    li.child_text("commonality")?.trim().parse().ok()?,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                can_fly_into_map: f.bool_in(Some(r), "canFlyIntoMap").unwrap_or(false),
                water_seeker: f.bool_in(Some(r), "waterSeeker").unwrap_or(false),
                predator: f.bool_in(Some(r), "predator").unwrap_or(false),
                max_prey_body_size: f.parse_in(Some(r), "maxPreyBodySize").unwrap_or(99_999.0),
                can_be_predator_prey: f.bool_in(Some(r), "canBePredatorPrey").unwrap_or(true),
            });
        let ingestible = def
            .node
            .child("ingestible")
            .filter(|n| !n.is_null())
            .map(|i| {
                let d = IngestibleProperties::default();
                let n = Some(i);
                IngestibleProperties {
                    preferability: f.preferability_in(i),
                    food_type: f.food_type_in(i),
                    base_ingest_ticks: f
                        .parse_in(n, "baseIngestTicks")
                        .unwrap_or(d.base_ingest_ticks),
                    chair_search_radius: f
                        .parse_in(n, "chairSearchRadius")
                        .unwrap_or(d.chair_search_radius),
                    default_num_to_ingest_at_once: f
                        .parse_in(n, "defaultNumToIngestAtOnce")
                        .unwrap_or(d.default_num_to_ingest_at_once),
                    max_num_to_ingest_at_once: f
                        .parse_in(n, "maxNumToIngestAtOnce")
                        .unwrap_or(d.max_num_to_ingest_at_once),
                    use_eating_speed_stat: f
                        .bool_in(n, "useEatingSpeedStat")
                        .unwrap_or(d.use_eating_speed_stat),
                    table_desired: f.bool_in(n, "tableDesired").unwrap_or(d.table_desired),
                    optimality_offset_humanlikes: f
                        .parse_in(n, "optimalityOffsetHumanlikes")
                        .unwrap_or(0.0),
                    optimality_offset_feeding_animals: f
                        .parse_in(n, "optimalityOffsetFeedingAnimals")
                        .unwrap_or(0.0),
                    taste_thought: i.child_text("tasteThought").map(str::to_owned),
                    special_thought_direct: i.child_text("specialThoughtDirect").map(str::to_owned),
                    special_thought_as_ingredient: i
                        .child_text("specialThoughtAsIngredient")
                        .map(str::to_owned),
                    ingest_report_string: i.child_text("ingestReportString").map(str::to_owned),
                    ingest_report_string_eat: i
                        .child_text("ingestReportStringEat")
                        .map(str::to_owned),
                }
            });
        let plant = def.node.child("plant").filter(|n| !n.is_null()).map(|p| {
            let num = |field: &str, default: f32| {
                p.child_text(field)
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(default)
            };
            let flag = |field: &str, default: bool| {
                p.child_text(field)
                    .map(|v| v.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(default)
            };
            PlantProperties {
                sow_tags: p.child_list_texts("sowTags"),
                sow_work: num("sowWork", 10.0),
                sow_min_skill: num("sowMinSkill", 0.0) as i32,
                harvest_work: num("harvestWork", 10.0),
                harvest_yield: num("harvestYield", 0.0),
                harvested_thing_def: p.child_text("harvestedThingDef").map(str::to_owned),
                harvest_min_growth: num("harvestMinGrowth", 0.65),
                harvest_after_growth: num("harvestAfterGrowth", 0.0),
                harvest_failable: flag("harvestFailable", true),
                auto_harvestable: flag("autoHarvestable", true),
                grow_days: num("growDays", 2.0),
                lifespan_days_per_grow_days: num("lifespanDaysPerGrowDays", 8.0),
                grow_min_glow: num("growMinGlow", 0.51),
                grow_optimal_glow: num("growOptimalGlow", 1.0),
                min_growth_temperature: num("minGrowthTemperature", 0.0),
                min_optimal_growth_temperature: num("minOptimalGrowthTemperature", 6.0),
                max_optimal_growth_temperature: num("maxOptimalGrowthTemperature", 42.0),
                max_growth_temperature: num("maxGrowthTemperature", 58.0),
                fertility_min: num("fertilityMin", 0.9),
                fertility_sensitivity: num("fertilitySensitivity", 0.5),
                completely_ignore_fertility: flag("completelyIgnoreFertility", false),
                dies_to_light: flag("diesToLight", false),
                human_food_plant: flag("humanFoodPlant", false),
                die_if_leafless: flag("dieIfLeafless", false),
                interferes_with_roof: flag("interferesWithRoof", false),
                chopped_thing_def: p.child_text("choppedThingDef").map(str::to_owned),
                visual_size_range: p
                    .child_text("visualSizeRange")
                    .and_then(|v| {
                        let (a, b) = v.trim().split_once('~')?;
                        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                    })
                    .unwrap_or((0.9, 1.1)),
                is_stump: flag("isStump", false),
                can_deteriorate: flag("canDeteriorate", false),
                harvest_tag: p
                    .child_text("harvestTag")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned),
                force_is_tree: flag("forceIsTree", false),
            }
        });
        let building = def
            .node
            .child("building")
            .filter(|n| !n.is_null())
            .map(|b| BuildingProperties {
                is_natural_rock: f.bool_in(Some(b), "isNaturalRock").unwrap_or(false),
                supports_plants: b.child_text("sowTag").is_some_and(|t| !t.trim().is_empty()),
                bed_heal_per_day: f.parse_in(Some(b), "bed_healPerDay").unwrap_or(0.0),
                expand_home_area: f.bool_in(Some(b), "expandHomeArea").unwrap_or(true),
                is_edifice: f.bool_in(Some(b), "isEdifice").unwrap_or(true),
                repairable: f.bool_in(Some(b), "repairable").unwrap_or(true),
                is_resource_rock: f.bool_in(Some(b), "isResourceRock").unwrap_or(false),
                natural_terrain: b.child_text("naturalTerrain").map(str::to_owned),
                leave_terrain: b.child_text("leaveTerrain").map(str::to_owned),
                mineable_thing: b.child_text("mineableThing").map(str::to_owned),
                mineable_drop_chance: f.parse_in(Some(b), "mineableDropChance").unwrap_or(1.0),
                mineable_yield: f.parse_in(Some(b), "mineableYield").unwrap_or(1),
                mineable_yield_wasteable: f
                    .bool_in(Some(b), "mineableYieldWasteable")
                    .unwrap_or(true),
                smoothed_thing: b
                    .child_text("smoothedThing")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned),
                bed_max_body_size: b
                    .child_text("bed_maxBodySize")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9999.0),
                bed_humanlike: f.bool_in(Some(b), "bed_humanlike").unwrap_or(true),
                is_sittable: f.bool_in(Some(b), "isSittable").unwrap_or(false),
                allow_autoroof: f.bool_in(Some(b), "allowAutoroof").unwrap_or(true),
                deconstructible: f.bool_in(Some(b), "deconstructible").unwrap_or(true),
                unpowered_door_open_speed_factor: b
                    .child_text("unpoweredDoorOpenSpeedFactor")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1.0),
                unpowered_door_close_speed_factor: b
                    .child_text("unpoweredDoorCloseSpeedFactor")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1.0),
                powered_door_open_speed_factor: f
                    .parse_in(Some(b), "poweredDoorOpenSpeedFactor")
                    .unwrap_or(1.0),
                powered_door_close_speed_factor: f
                    .parse_in(Some(b), "poweredDoorCloseSpeedFactor")
                    .unwrap_or(1.0),
                work_table_room_role: b
                    .child_text("workTableRoomRole")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned),
                work_table_not_in_room_role_factor: f
                    .parse_in(Some(b), "workTableNotInRoomRoleFactor")
                    .unwrap_or(1.0),
                bed_counts_for_bedroom_or_barracks: f
                    .bool_in(Some(b), "bed_countsForBedroomOrBarracks")
                    .unwrap_or(true),
                bed_empty_counts_for_barracks: f
                    .bool_in(Some(b), "bed_emptyCountsForBarracks")
                    .unwrap_or(true),
            });
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            corpse_of: None,
            meat_of: None,
            category: f.string("category"),
            thing_class: f.string("thingClass"),
            passability: f.passability(),
            path_cost: f.parse("pathCost").unwrap_or(0),
            fill_percent: f.parse("fillPercent").unwrap_or(0.0),
            selectable: f.bool("selectable").unwrap_or(false),
            altitude_layer: f.string("altitudeLayer").map(|s| s.trim().to_owned()),
            count_as_resource: f
                .string("resourceReadoutPriority")
                .is_some_and(|s| s.trim() != "Uncounted")
                || f.bool("countAsResource").unwrap_or(false),
            ui_order: f.parse("uiOrder").unwrap_or(2999.0),
            designation_hot_key: f.string("designationHotKey").map(|s| s.trim().to_owned()),
            graphic,
            stat_bases: f.float_map("statBases"),
            race,
            building,
            ingestible,
            plant,
            verbs: def
                .node
                .child("verbs")
                .map(|v| v.children.iter().map(verb_properties).collect())
                .unwrap_or_default(),
            projectile: def.node.child("projectile").map(|p| {
                let f = |k: &str, d: f32| {
                    p.child_text(k)
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(d)
                };
                let b = |k: &str| {
                    p.child_text(k)
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
                };
                ProjectileProperties {
                    damage_def: p.child_text("damageDef").map(|s| s.trim().to_owned()),
                    damage_amount_base: f("damageAmountBase", -1.0) as i32,
                    armor_penetration_base: f("armorPenetrationBase", -1.0),
                    speed: f("speed", 5.0),
                    stopping_power: f("stoppingPower", 0.5),
                    fly_overhead: b("flyOverhead"),
                    always_free_intercept: b("alwaysFreeIntercept"),
                    explosion_radius: f("explosionRadius", 0.0),
                }
            }),
            tools: def
                .node
                .child("tools")
                .map(|t| {
                    t.children
                        .iter()
                        .map(|li| {
                            let f = |k: &str, d: f32| {
                                li.child_text(k)
                                    .and_then(|v| v.trim().parse().ok())
                                    .unwrap_or(d)
                            };
                            Tool {
                                label: li.child_text("label").unwrap_or_default().to_owned(),
                                capacities: li.child_list_texts("capacities"),
                                power: f("power", 0.0),
                                cooldown_time: f("cooldownTime", 0.0),
                                chance_factor: f("chanceFactor", 1.0),
                                armor_penetration: f("armorPenetration", -1.0),
                                linked_body_parts_group: li
                                    .child_text("linkedBodyPartsGroup")
                                    .map(str::to_owned),
                                ensure_linked_body_parts_group_always_usable: li
                                    .child_text("ensureLinkedBodyPartsGroupAlwaysUsable")
                                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            stack_limit: f.parse("stackLimit").unwrap_or(1),
            always_haulable: f.bool("alwaysHaulable").unwrap_or(false),
            designate_haulable: f.bool("designateHaulable").unwrap_or(false),
            small_volume: f.bool("smallVolume").unwrap_or(false),
            thing_categories: def.node.child_list_texts("thingCategories"),
            cost_stuff_count: f.parse("costStuffCount").unwrap_or(0),
            cost_list: def
                .node
                .child("costList")
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|c| Some((c.name.clone(), c.text.as_deref()?.parse().ok()?)))
                        .collect()
                })
                .unwrap_or_default(),
            stuff_categories: def.node.child_list_texts("stuffCategories"),
            stuff_props: def
                .node
                .child("stuffProps")
                .filter(|n| !n.is_null())
                .map(|n| {
                    let floats = |field: &str| -> BTreeMap<String, f32> {
                        n.child(field)
                            .map(|m| {
                                m.children
                                    .iter()
                                    .filter_map(|c| {
                                        Some((c.name.clone(), c.text.as_deref()?.parse().ok()?))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    StuffProperties {
                        categories: n.child_list_texts("categories"),
                        stat_factors: floats("statFactors"),
                        stat_offsets: floats("statOffsets"),
                        color: n.child_text("color").and_then(parse_color),
                        appearance: n.child_text("appearance").map(|s| s.trim().to_owned()),
                        stuff_adjective: n
                            .child_text("stuffAdjective")
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty()),
                        commonality: n
                            .child_text("commonality")
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(1.0),
                        can_suggest_use_default_stuff: n
                            .child_text("canSuggestUseDefaultStuff")
                            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
                    }
                }),
            designation_category: f.string("designationCategory"),
            construction_skill_prerequisite: f.parse("constructionSkillPrerequisite").unwrap_or(0),
            force_move_items_before_construction: f
                .parse("forceMoveItemsBeforeConstruction")
                .unwrap_or(false),
            size: def
                .node
                .child_text("size")
                .and_then(|v| {
                    let (x, z) = v
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .split_once(',')?;
                    Some((x.trim().parse().ok()?, z.trim().parse().ok()?))
                })
                .unwrap_or((1, 1)),
            surface_type: f.string("surfaceType"),
            draw_style_category: f.string("drawStyleCategory").map(|s| s.trim().to_owned()),
            default_stuff: f.string("defaultStuff").map(|s| s.trim().to_owned()),
            description: f
                .string("description")
                .map(|s| s.trim().replace("\\n", "\n")),
            rotatable: f.bool("rotatable").unwrap_or(true),
            default_placing_rot: f.string("defaultPlacingRot").map(|s| s.trim().to_owned()),
            research_prerequisites: def.node.child_list_texts("researchPrerequisites"),
            recipes: def.node.child_list_texts("recipes"),
            heat_per_tick_while_working: f
                .parse_in(def.node.child("building"), "heatPerTickWhileWorking")
                .unwrap_or(0.0),
            interaction_cell_offset: f
                .bool_in(Some(&def.node), "hasInteractionCell")
                .unwrap_or(false)
                .then(|| {
                    def.node
                        .child_text("interactionCellOffset")
                        .and_then(parse_int_vec3_xz)
                        .unwrap_or((0, 0))
                }),
            holds_roof: f.bool_in(Some(&def.node), "holdsRoof").unwrap_or(false),
            block_light: f.bool_in(Some(&def.node), "blockLight").unwrap_or(false),
            mineable: f.bool_in(Some(&def.node), "mineable").unwrap_or(false),
            resources_fraction_when_deconstructed: f
                .parse_in(Some(&def.node), "resourcesFractionWhenDeconstructed")
                .unwrap_or(0.5),
            glower: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find(|li| li.attr("Class") == Some("CompProperties_Glower"))
                .map(|n| GlowerProperties {
                    radius: f.parse_in(Some(n), "glowRadius").unwrap_or(14.0),
                    color: n
                        .child_text("glowColor")
                        .and_then(|t| {
                            let v: Vec<i32> = t
                                .trim()
                                .trim_start_matches('(')
                                .trim_end_matches(')')
                                .split(',')
                                .filter_map(|x| x.trim().parse().ok())
                                .collect();
                            (v.len() >= 3).then(|| [v[0], v[1], v[2]])
                        })
                        .unwrap_or([369, 369, 369]),
                    overlight_radius: f.parse_in(Some(n), "overlightRadius").unwrap_or(0.0),
                }),
            refuelable: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find(|li| li.attr("Class") == Some("CompProperties_Refuelable"))
                .map(|n| RefuelableProperties {
                    consumption_rate: f.parse_in(Some(n), "fuelConsumptionRate").unwrap_or(1.0),
                    capacity: f.parse_in(Some(n), "fuelCapacity").unwrap_or(2.0),
                    initial_fuel_percent: f.parse_in(Some(n), "initialFuelPercent").unwrap_or(0.0),
                    consume_fuel_only_when_used: f
                        .bool_in(Some(n), "consumeFuelOnlyWhenUsed")
                        .unwrap_or(false),
                    fuel_defs: n
                        .path(&["fuelFilter", "thingDefs"])
                        .map(|l| {
                            l.children
                                .iter()
                                .filter_map(|c| c.text.as_deref().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                }),
            heat_pusher: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find(|li| li.attr("Class") == Some("CompProperties_HeatPusher"))
                .map(|n| HeatPusherProperties {
                    heat_per_second: f.parse_in(Some(n), "heatPerSecond").unwrap_or(0.0),
                    max_temperature: f
                        .parse_in(Some(n), "heatPushMaxTemperature")
                        .unwrap_or(99999.0),
                    min_temperature: f
                        .parse_in(Some(n), "heatPushMinTemperature")
                        .unwrap_or(-99999.0),
                    powered: n.child_text("compClass").map(str::trim)
                        == Some("CompHeatPusherPowered"),
                }),
            needs_power: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .any(|li| {
                    li.attr("Class")
                        .is_some_and(|c| c.starts_with("CompProperties_Power"))
                }),
            ticker_type: f.string("tickerType"),
            use_hit_points: f.bool("useHitPoints").unwrap_or(true),
            apparel_groups: def
                .node
                .child("apparel")
                .map(|a| a.child_list_texts("bodyPartGroups"))
                .unwrap_or_default(),
            starting_hp_range: def
                .node
                .child("startingHpRange")
                .and_then(|n| {
                    Some((
                        n.child_text("min")?.trim().parse().ok()?,
                        n.child_text("max")?.trim().parse().ok()?,
                    ))
                })
                .unwrap_or((1.0, 1.0)),
            deteriorate_from_environmental_effects: f
                .bool("deteriorateFromEnvironmentalEffects")
                .unwrap_or(true),
            color: def
                .node
                .child("graphicData")
                .and_then(|g| g.child_text("color"))
                .and_then(parse_color),
            filth: def
                .node
                .child("filth")
                .filter(|n| !n.is_null())
                .map(|n| FilthProperties {
                    cleaning_work_to_reduce_thickness: f
                        .parse_in(Some(n), "cleaningWorkToReduceThickness")
                        .unwrap_or(35.0),
                    disappears_in_days: n
                        .child_text("disappearsInDays")
                        .and_then(crate::values::parse_float_range)
                        .unwrap_or((0.0, 0.0)),
                    max_thickness: f.parse_in(Some(n), "maxThickness").unwrap_or(100),
                    placement_mask: filth_flags_in(n.child("placementMask"))
                        .unwrap_or(filth_flags::UNNATURAL),
                    ignore_filth_multiplier_stat: n
                        .child_text("ignoreFilthMultiplierStat")
                        .is_some_and(|v| v.trim() == "true"),
                    can_filth_attach: n
                        .child_text("canFilthAttach")
                        .is_some_and(|v| v.trim() == "true"),
                }),
            power: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find_map(|li| {
                    let class = li.attr("Class")?;
                    let n = Some(li);
                    match class {
                        "CompProperties_Power" => Some(PowerProperties {
                            comp_class: li
                                .child_text("compClass")
                                .map(|t| t.trim().to_owned())
                                .unwrap_or_else(|| "CompPowerTrader".to_owned()),
                            base_power_consumption: f
                                .parse_in(n, "basePowerConsumption")
                                .unwrap_or(0.0),
                            transmits_power: f.bool_in(n, "transmitsPower").unwrap_or(false),
                            idle_power_draw: f.parse_in(n, "idlePowerDraw").unwrap_or(-1.0),
                            battery: None,
                        }),
                        "CompProperties_Battery" => Some(PowerProperties {
                            comp_class: li
                                .child_text("compClass")
                                .map(|t| t.trim().to_owned())
                                .unwrap_or_else(|| "CompPowerBattery".to_owned()),
                            base_power_consumption: 0.0,
                            transmits_power: f.bool_in(n, "transmitsPower").unwrap_or(false),
                            idle_power_draw: -1.0,
                            battery: Some((
                                f.parse_in(n, "storedEnergyMax").unwrap_or(1000.0),
                                f.parse_in(n, "efficiency").unwrap_or(0.5),
                            )),
                        }),
                        _ => None,
                    }
                }),
            flickable: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .any(|li| li.attr("Class") == Some("CompProperties_Flickable")),
            breakdownable: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .any(|li| li.attr("Class") == Some("CompProperties_Breakdownable")),
            temp_control: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find(|li| li.attr("Class") == Some("CompProperties_TempControl"))
                .map(|li| TempControlProperties {
                    energy_per_second: f.parse_in(Some(li), "energyPerSecond").unwrap_or(12.0),
                    default_target_temperature: f
                        .parse_in(Some(li), "defaultTargetTemperature")
                        .unwrap_or(21.0),
                    low_power_consumption_factor: f
                        .parse_in(Some(li), "lowPowerConsumptionFactor")
                        .unwrap_or(0.1),
                }),
            forbiddable: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .any(|li| li.attr("Class") == Some("CompProperties_Forbiddable")),
            rottable: def
                .node
                .child("comps")
                .into_iter()
                .flat_map(|c| c.children.iter())
                .find(|li| li.attr("Class") == Some("CompProperties_Rottable"))
                .map(|n| RottableProperties {
                    days_to_rot_start: f.parse_in(Some(n), "daysToRotStart").unwrap_or(2.0),
                    rot_destroys: f.parse_in(Some(n), "rotDestroys").unwrap_or(false),
                    rot_damage_per_day: f.parse_in(Some(n), "rotDamagePerDay").unwrap_or(40.0),
                    days_to_dessicated: f.parse_in(Some(n), "daysToDessicated").unwrap_or(999.0),
                    dessicated_damage_per_day: f
                        .parse_in(Some(n), "dessicatedDamagePerDay")
                        .unwrap_or(0.0),
                }),
        }
    }
}

impl PawnKindDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            race: f.string("race"),
            combat_power: f.parse("combatPower").unwrap_or(0.0),
            default_faction: f.string("defaultFactionDef"),
            eco_system_weight: f.parse("ecoSystemWeight").unwrap_or(1.0),
            wild_group_size: def
                .node
                .child_text("wildGroupSize")
                .and_then(crate::values::parse_float_range)
                .map_or((1, 1), |(a, b)| (a as i32, b as i32)),
            adult_body: def
                .node
                .child("lifeStages")
                .and_then(|l| l.children.iter().rev().find(|c| c.name == "li"))
                .and_then(|li| li.child("bodyGraphicData"))
                .and_then(|g| {
                    let tex = g.child_text("texPath")?.trim().to_owned();
                    let size = g
                        .child_text("drawSize")
                        .and_then(parse_vec2)
                        .unwrap_or((1.0, 1.0));
                    Some((tex, size))
                }),
        }
    }
}

impl NeedDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            need_class: f.string("needClass"),
            major: f.bool("major").unwrap_or(false),
            list_priority: f.parse("listPriority").unwrap_or(0),
            base_level: f.parse("baseLevel").unwrap_or(0.5),
            seeker_rise_per_hour: f.parse("seekerRisePerHour").unwrap_or(0.0),
            seeker_fall_per_hour: f.parse("seekerFallPerHour").unwrap_or(0.0),
            freeze_while_sleeping: f.bool("freezeWhileSleeping").unwrap_or(false),
            freeze_in_mental_state: f.bool("freezeInMentalState").unwrap_or(false),
        }
    }
}

/// `VerbProperties` from a `verbs` list item.
fn verb_properties(li: &XmlNode) -> VerbProperties {
    let f = |k: &str, d: f32| {
        li.child_text(k)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(d)
    };
    let b = |k: &str, d: bool| {
        li.child_text(k)
            .map_or(d, |v| v.trim().eq_ignore_ascii_case("true"))
    };
    VerbProperties {
        verb_class: li.child_text("verbClass").map(|s| s.trim().to_owned()),
        is_primary: b("isPrimary", true),
        warmup_time: f("warmupTime", 0.0),
        range: f("range", 1.42),
        min_range: f("minRange", 0.0),
        burst_shot_count: f("burstShotCount", 1.0) as i32,
        ticks_between_burst_shots: f("ticksBetweenBurstShots", 15.0) as i32,
        default_projectile: li
            .child_text("defaultProjectile")
            .map(|s| s.trim().to_owned()),
        require_line_of_sight: b("requireLineOfSight", true),
        stop_burst_without_los: b("stopBurstWithoutLos", true),
        can_go_wild: b("canGoWild", true),
        forced_miss_radius: f("forcedMissRadius", 0.0),
        only_manual_cast: b("onlyManualCast", false),
        default_cooldown_time: f("defaultCooldownTime", 0.0),
    }
}

/// A `ThoughtDef` from its XML.
fn thought_def(def: &Def, warnings: &mut Vec<String>) -> ThoughtDef {
    let mut f = Fields { def, warnings };
    let n = &def.node;
    let trimmed = |s: Option<String>| s.map(|s| s.trim().to_owned());
    let stages = n
        .child("stages")
        .map(|l| {
            l.children
                .iter()
                .map(|li| {
                    if li.attr_is_true("IsNull") {
                        return None;
                    }
                    Some(ThoughtStage {
                        label: li.child_text("label").unwrap_or("").trim().to_owned(),
                        base_mood_effect: li
                            .child_text("baseMoodEffect")
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0.0),
                        visible: li
                            .child_text("visible")
                            .is_none_or(|v| !v.trim().eq_ignore_ascii_case("false")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let stage_filter = f.string("developmentalStageFilter");
    ThoughtDef {
        def_name: def.def_name.clone(),
        thought_class: trimmed(f.string("thoughtClass")),
        worker_class: trimmed(f.string("workerClass")),
        stages,
        stack_limit: f.parse("stackLimit").unwrap_or(1),
        stacked_effect_multiplier: f.parse("stackedEffectMultiplier").unwrap_or(0.75),
        stages_stack: f.bool("stagesStack").unwrap_or(false),
        duration_days: f.parse("durationDays").unwrap_or(0.0),
        invert: f.bool("invert").unwrap_or(false),
        valid_while_despawned: f.bool("validWhileDespawned").unwrap_or(false),
        next_thought: trimmed(f.string("nextThought")),
        produces_memory_thought: trimmed(f.string("producesMemoryThought")),
        nullifying_traits: n.child_list_texts("nullifyingTraits"),
        nullifying_hediffs: n.child_list_texts("nullifyingHediffs"),
        required_traits: n.child_list_texts("requiredTraits"),
        required_hediffs: n.child_list_texts("requiredHediffs"),
        hediff: trimmed(f.string("hediff")),
        nullified_if_not_colonist: f.bool("nullifiedIfNotColonist").unwrap_or(false),
        thought_to_make: trimmed(f.string("thoughtToMake")),
        show_bubble: f.bool("showBubble").unwrap_or(false),
        min_expectation: trimmed(f.string("minExpectation")),
        replace_thoughts: n.child_list_texts("replaceThoughts"),
        for_adults: stage_filter.is_none_or(|s| s.contains("Adult")),
        lerp_mood_to_zero: f.bool("lerpMoodToZero").unwrap_or(false),
        stack_limit_for_same_other_pawn: f.parse("stackLimitForSameOtherPawn").unwrap_or(-1),
        gender: trimmed(f.string("gender")).filter(|g| g != "None"),
        effect_multiplying_stat: trimmed(f.string("effectMultiplyingStat")),
    }
}

impl StatDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        let needs = |field: &str| -> Vec<SkillNeed> {
            def.node
                .child(field)
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|li| {
                            let skill = li.child_text("skill")?.to_owned();
                            match li.attr("Class") {
                                Some("SkillNeed_BaseBonus") => Some(SkillNeed::BaseBonus {
                                    skill,
                                    base_value: li
                                        .child_text("baseValue")
                                        .and_then(|v| v.parse().ok())
                                        .unwrap_or(0.5),
                                    bonus_per_level: li
                                        .child_text("bonusPerLevel")
                                        .and_then(|v| v.parse().ok())
                                        .unwrap_or(0.05),
                                }),
                                Some("SkillNeed_Direct") => Some(SkillNeed::Direct {
                                    skill,
                                    values_per_level: li
                                        .child_list_texts("valuesPerLevel")
                                        .iter()
                                        .filter_map(|v| v.parse().ok())
                                        .collect(),
                                }),
                                _ => None,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            def_name: def.def_name.clone(),
            default_base_value: f.parse("defaultBaseValue").unwrap_or(1.0),
            min_value: f.parse("minValue").unwrap_or(-9_999_999.0),
            max_value: f.parse("maxValue").unwrap_or(9_999_999.0),
            round_to_five_over: f.parse("roundToFiveOver").unwrap_or(f32::INFINITY),
            stat_factors: def.node.child_list_texts("statFactors"),
            skill_need_factors: needs("skillNeedFactors"),
            skill_need_offsets: needs("skillNeedOffsets"),
            capacity_offsets: def
                .node
                .child("capacityOffsets")
                .map(|c| {
                    c.children
                        .iter()
                        .map(|li| {
                            let f = |k: &str, d: f32| {
                                li.child_text(k)
                                    .and_then(|v| v.trim().parse().ok())
                                    .unwrap_or(d)
                            };
                            (
                                li.child_text("capacity").unwrap_or_default().to_owned(),
                                f("scale", 1.0),
                                f("max", 9999.0),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            capacity_factors: def
                .node
                .child("capacityFactors")
                .map(|c| {
                    c.children
                        .iter()
                        .map(|li| {
                            let f = |k: &str, d: f32| {
                                li.child_text(k)
                                    .and_then(|v| v.trim().parse().ok())
                                    .unwrap_or(d)
                            };
                            CapacityFactor {
                                capacity: li.child_text("capacity").unwrap_or_default().to_owned(),
                                weight: f("weight", 1.0),
                                max: f("max", 9999.0),
                                use_reciprocal: li
                                    .child_text("useReciprocal")
                                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
                                allowed_defect: f("allowedDefect", 0.0),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            post_process_curve: def
                .node
                .path(&["postProcessCurve", "points"])
                .map(|pts| {
                    pts.children
                        .iter()
                        .filter_map(|li| {
                            let t = li
                                .text
                                .as_deref()?
                                .trim()
                                .trim_start_matches('(')
                                .trim_end_matches(')');
                            let (x, y) = t.split_once(',')?;
                            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            glow_part: def
                .node
                .child("parts")
                .into_iter()
                .flat_map(|p| p.children.iter())
                .find(|li| li.attr("Class") == Some("StatPart_Glow"))
                .map(|li| GlowPart {
                    humanlike_only: li
                        .child_text("humanlikeOnly")
                        .is_some_and(|v| v.eq_ignore_ascii_case("true")),
                    curve: li
                        .path(&["factorFromGlowCurve", "points"])
                        .map(curve_points)
                        .unwrap_or_default(),
                }),
            gear_offset: def
                .node
                .child("parts")
                .into_iter()
                .flat_map(|p| p.children.iter())
                .find(|li| li.attr("Class") == Some("StatPart_GearStatOffset"))
                .and_then(|li| {
                    Some((
                        li.child_text("apparelStat")?.trim().to_owned(),
                        li.child_text("subtract")
                            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
                    ))
                }),
            environmental_effects: def
                .node
                .child("parts")
                .into_iter()
                .flat_map(|p| p.children.iter())
                .find(|li| li.attr("Class") == Some("StatPart_EnvironmentalEffects"))
                .map(|li| {
                    let num = |k: &str| {
                        li.child_text(k)
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0.0)
                    };
                    (num("factorOffsetUnroofed"), num("factorOffsetOutdoors"))
                }),
            stuff_part: def
                .node
                .child("parts")
                .into_iter()
                .flat_map(|p| p.children.iter())
                .find(|li| li.attr("Class") == Some("StatPart_Stuff"))
                .and_then(|li| {
                    Some((
                        li.child_text("stuffPowerStat")?.trim().to_owned(),
                        li.child_text("multiplierStat")?.trim().to_owned(),
                    ))
                }),
        }
    }
}

impl WorkTypeDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            natural_priority: f.parse("naturalPriority").unwrap_or(0),
            always_start_active: f.bool("alwaysStartActive").unwrap_or(false),
            work_tags: def.node.child_list_texts("workTags"),
            relevant_skills: def.node.child_list_texts("relevantSkills"),
            givers_by_priority: Vec::new(),
            gerund_label: f.string("gerundLabel").unwrap_or_default(),
            pawn_label: f.string("pawnLabel").unwrap_or_default(),
            label_short: f
                .string("labelShort")
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| def.node.child_text("label").unwrap_or("").trim().to_owned()),
            description: f
                .string("description")
                .map(|s| s.trim().replace("\\n", "\n"))
                .unwrap_or_default(),
            visible: f.bool("visible").unwrap_or(true),
        }
    }
}

impl WorkGiverDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            label: f.label(),
            giver_class: f.string("giverClass"),
            work_type: f.string("workType"),
            priority_in_type: f.parse("priorityInType").unwrap_or(0),
            emergency: f.bool("emergency").unwrap_or(false),
            scan_things: f.bool("scanThings").unwrap_or(true),
            scan_cells: f.bool("scanCells").unwrap_or(false),
            non_colonists_can_do: f.bool("nonColonistsCanDo").unwrap_or(false),
            work_tags: def.node.child_list_texts("workTags"),
            required_capacities: def.node.child_list_texts("requiredCapacities"),
            tag_to_give: f
                .string("tagToGive")
                .unwrap_or_else(|| "MiscWork".to_owned()),
            fixed_bill_giver_defs: def.node.child_list_texts("fixedBillGiverDefs"),
            verb: f.string("verb").unwrap_or_default(),
            gerund: f.string("gerund").unwrap_or_default(),
            direct_orderable: f.bool("directOrderable").unwrap_or(true),
            can_be_done_while_drafted: f.bool("canBeDoneWhileDrafted").unwrap_or(false),
            prioritize_sustains: f.bool("prioritizeSustains").unwrap_or(false),
            equivalence_group: f.string("equivalenceGroup"),
            auto_takeable_priority_drafted: f.parse("autoTakeablePriorityDrafted").unwrap_or(-1),
        }
    }
}

impl JobDef {
    fn from_def(def: &Def, warnings: &mut Vec<String>) -> Self {
        let mut f = Fields { def, warnings };
        Self {
            def_name: def.def_name.clone(),
            report_string: f.string("reportString").unwrap_or_default(),
            driver_class: f.string("driverClass"),
            is_idle: f.bool("isIdle").unwrap_or(false),
            joy_duration: f.parse("joyDuration").unwrap_or(4000),
            joy_gain_rate: f.parse("joyGainRate").unwrap_or(1.0),
            joy_kind: f.string("joyKind").map(|s| s.trim().to_owned()),
            joy_max_participants: f.parse("joyMaxParticipants").unwrap_or(1),
            joy_skill: f.string("joySkill").map(|s| s.trim().to_owned()),
            joy_xp_per_tick: f.parse("joyXpPerTick").unwrap_or(0.0),
        }
    }
}

/// Typed Defs used by the simulation, plus the generic database for
/// everything else.
#[derive(Debug, Default, Clone)]
pub struct GameDefs {
    pub terrain: DefTable<TerrainDef>,
    pub things: DefTable<ThingDef>,
    pub pawn_kinds: DefTable<PawnKindDef>,
    pub jobs: DefTable<JobDef>,
    pub think_trees: DefTable<ThinkTreeDef>,
    pub needs: DefTable<NeedDef>,
    pub work_types: DefTable<WorkTypeDef>,
    pub work_givers: DefTable<WorkGiverDef>,
    pub stats: DefTable<StatDef>,
    pub bodies: DefTable<crate::health::BodyDef>,
    pub body_parts: DefTable<crate::health::BodyPartDef>,
    pub hediffs: DefTable<crate::health::HediffDef>,
    pub damages: DefTable<crate::health::DamageDef>,
    pub capacities: DefTable<crate::health::PawnCapacityDef>,
    pub maneuvers: DefTable<ManeuverDef>,
    pub biomes: DefTable<BiomeDef>,
    pub thing_categories: DefTable<ThingCategoryDef>,
    pub research: DefTable<ResearchProjectDef>,
    pub joy_givers: DefTable<JoyGiverDef>,
    pub recipes: DefTable<RecipeDef>,
    pub special_filters: DefTable<SpecialThingFilterDef>,
    pub joy_kinds: DefTable<JoyKindDef>,
    pub thoughts: DefTable<ThoughtDef>,
    pub mental_breaks: DefTable<MentalBreakDef>,
    pub mental_states: DefTable<MentalStateDef>,
    /// Wealth-triggered expectations, by `order`.
    pub expectations: Vec<ExpectationDef>,
    pub raw: DefDatabase,
}

impl GameDefs {
    /// Builds typed tables and validates cross-references. Returns warnings
    /// for malformed fields and unresolved references.
    pub fn from_database(raw: DefDatabase) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut defs = GameDefs::default();
        for def in raw.table("TerrainDef").into_iter().flat_map(|t| t.iter()) {
            defs.terrain.push(TerrainDef::from_def(def, &mut warnings));
        }
        for def in raw.table("ThingDef").into_iter().flat_map(|t| t.iter()) {
            defs.things.push(ThingDef::from_def(def, &mut warnings));
        }
        add_implied_stone_terrains(&mut defs);
        for def in raw.table("PawnKindDef").into_iter().flat_map(|t| t.iter()) {
            defs.pawn_kinds
                .push(PawnKindDef::from_def(def, &mut warnings));
        }
        for def in raw.table("JobDef").into_iter().flat_map(|t| t.iter()) {
            defs.jobs.push(JobDef::from_def(def, &mut warnings));
        }
        for def in raw.table("NeedDef").into_iter().flat_map(|t| t.iter()) {
            defs.needs.push(NeedDef::from_def(def, &mut warnings));
        }
        for def in raw.table("BodyDef").into_iter().flat_map(|t| t.iter()) {
            defs.bodies.push(crate::health::BodyDef::from_def(def));
        }
        for def in raw.table("BodyPartDef").into_iter().flat_map(|t| t.iter()) {
            defs.body_parts
                .push(crate::health::BodyPartDef::from_def(def));
        }
        for def in raw.table("HediffDef").into_iter().flat_map(|t| t.iter()) {
            defs.hediffs.push(crate::health::HediffDef::from_def(def));
        }
        for def in raw.table("DamageDef").into_iter().flat_map(|t| t.iter()) {
            defs.damages.push(crate::health::DamageDef::from_def(def));
        }
        for def in raw.table("BiomeDef").into_iter().flat_map(|t| t.iter()) {
            defs.biomes.push(BiomeDef::from_def(def));
        }
        for def in raw
            .table("ThingCategoryDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            defs.thing_categories.push(ThingCategoryDef {
                def_name: def.def_name.clone(),
                parent: def.node.child_text("parent").map(|t| t.trim().to_owned()),
            });
        }
        for def in raw
            .table("ResearchProjectDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            let n = &def.node;
            defs.research.push(ResearchProjectDef {
                def_name: def.def_name.clone(),
                label: f.label(),
                description: f.string("description").unwrap_or_default(),
                base_cost: f.parse("baseCost").unwrap_or(0.0),
                tech_level: n.child_text("techLevel").map_or(0, tech_level),
                prerequisites: n.child_list_texts("prerequisites"),
                hidden_prerequisites: n.child_list_texts("hiddenPrerequisites"),
                required_research_building: n
                    .child_text("requiredResearchBuilding")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned),
                required_research_facilities: n.child_list_texts("requiredResearchFacilities"),
                tags: n.child_list_texts("tags"),
                view: (
                    f.parse("researchViewX").unwrap_or(0.0),
                    f.parse("researchViewY").unwrap_or(0.0),
                ),
            });
        }
        for def in raw.table("RecipeDef").into_iter().flat_map(|t| t.iter()) {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            let n = &def.node;
            let spec = |name: &str| n.child(name).map(ThingFilterSpec::from_node);
            let stat = |f: &mut Fields<'_>, name: &str, default: Option<&str>| {
                f.string(name)
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .or(default.map(str::to_owned))
            };
            defs.recipes.push(RecipeDef {
                def_name: def.def_name.clone(),
                label: f.label(),
                job_string: f.string("jobString").unwrap_or_default(),
                work_amount: f.parse("workAmount").unwrap_or(-1.0),
                work_speed_stat: stat(&mut f, "workSpeedStat", None),
                work_table_speed_stat: stat(
                    &mut f,
                    "workTableSpeedStat",
                    Some("WorkTableWorkSpeedFactor"),
                ),
                efficiency_stat: stat(&mut f, "efficiencyStat", None),
                work_table_efficiency_stat: stat(
                    &mut f,
                    "workTableEfficiencyStat",
                    Some("WorkTableEfficiencyFactor"),
                ),
                work_skill: stat(&mut f, "workSkill", None),
                work_skill_learn_factor: f.parse("workSkillLearnFactor").unwrap_or(1.0),
                allow_mixing_ingredients: f.bool("allowMixingIngredients").unwrap_or(false),
                ingredient_value_getter_class: stat(&mut f, "ingredientValueGetterClass", None),
                ingredients: n
                    .child("ingredients")
                    .map(|l| {
                        l.children
                            .iter()
                            .filter(|c| c.name == "li")
                            .map(|li| IngredientCountDef {
                                filter: li
                                    .child("filter")
                                    .map(ThingFilterSpec::from_node)
                                    .unwrap_or_default(),
                                count: li
                                    .child_text("count")
                                    .and_then(|c| c.trim().parse().ok())
                                    .unwrap_or(1.0),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                products: n
                    .child("products")
                    .map(|l| {
                        l.children
                            .iter()
                            .map(|c| {
                                (
                                    c.name.clone(),
                                    c.text
                                        .as_deref()
                                        .and_then(|t| t.trim().parse().ok())
                                        .unwrap_or(1),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                fixed_ingredient_filter: spec("fixedIngredientFilter").unwrap_or_default(),
                default_ingredient_filter: spec("defaultIngredientFilter"),
                required_giver_work_type: stat(&mut f, "requiredGiverWorkType", None),
                skill_requirements: n
                    .child("skillRequirements")
                    .map(|l| {
                        l.children
                            .iter()
                            .filter_map(|c| {
                                Some((c.name.clone(), c.text.as_deref()?.trim().parse().ok()?))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                research_prerequisite: stat(&mut f, "researchPrerequisite", None),
                research_prerequisites: n.child_list_texts("researchPrerequisites"),
                recipe_users: n.child_list_texts("recipeUsers"),
                unfinished_thing_def: stat(&mut f, "unfinishedThingDef", None),
                special_products: n.child_list_texts("specialProducts"),
                display_priority: f.parse("displayPriority").unwrap_or(99999),
                worker_counter_class: f.string("workerCounterClass"),
                ignore_ingredient_count_take_entire_stacks: f
                    .bool("ignoreIngredientCountTakeEntireStacks")
                    .unwrap_or(false),
            });
        }
        for def in raw
            .table("SpecialThingFilterDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            defs.special_filters.push(SpecialThingFilterDef {
                def_name: def.def_name.clone(),
                parent_category: f.string("parentCategory").map(|s| s.trim().to_owned()),
                allowed_by_default: f.bool("allowedByDefault").unwrap_or(false),
                worker_class: f.string("workerClass").map(|s| s.trim().to_owned()),
            });
        }
        for def in raw.table("JoyGiverDef").into_iter().flat_map(|t| t.iter()) {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            let n = &def.node;
            defs.joy_givers.push(JoyGiverDef {
                def_name: def.def_name.clone(),
                giver_class: f.string("giverClass").unwrap_or_default(),
                base_chance: f.parse("baseChance").unwrap_or(0.0),
                pct_pawns_ever_do: f.parse("pctPawnsEverDo").unwrap_or(1.0),
                joy_kind: f.string("joyKind").map(|s| s.trim().to_owned()),
                job: f.string("jobDef").map(|s| s.trim().to_owned()),
                thing_defs: n.child_list_texts("thingDefs"),
                required_capacities: n.child_list_texts("requiredCapacities"),
                requires_enjoy_outdoors: f.bool("requiresEnjoyOutdoors").unwrap_or(false),
                can_do_while_in_bed: f.bool("canDoWhileInBed").unwrap_or(false),
                desire_sit: f.bool("desireSit").unwrap_or(true),
                unroofed_only: f.bool("unroofedOnly").unwrap_or(false),
            });
        }
        for def in raw.table("JoyKindDef").into_iter().flat_map(|t| t.iter()) {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            defs.joy_kinds.push(JoyKindDef {
                def_name: def.def_name.clone(),
                label: f.label(),
                needs_thing: f.bool("needsThing").unwrap_or(true),
            });
        }
        for def in raw
            .table("ExpectationDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            defs.expectations.push(ExpectationDef {
                def_name: def.def_name.clone(),
                label: f.label(),
                order: f.parse("order").unwrap_or(0),
                max_map_wealth: f.parse("maxMapWealth"),
                joy_kinds_needed: f.parse("joyKindsNeeded").unwrap_or(0),
                joy_tolerance_drop_per_day: f.parse("joyToleranceDropPerDay").unwrap_or(0.0),
                thought_stage: f.parse("thoughtStage"),
            });
        }
        defs.expectations.sort_by_key(|e| e.order);
        for def in raw.table("ThoughtDef").into_iter().flat_map(|t| t.iter()) {
            defs.thoughts.push(thought_def(def, &mut warnings));
        }
        for def in raw
            .table("MentalBreakDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            let intensity = match f.string("intensity").as_deref().map(str::trim) {
                Some("Minor") => MentalBreakIntensity::Minor,
                Some("Major") => MentalBreakIntensity::Major,
                Some("Extreme") => MentalBreakIntensity::Extreme,
                _ => MentalBreakIntensity::None,
            };
            defs.mental_breaks.push(MentalBreakDef {
                def_name: def.def_name.clone(),
                worker_class: f.string("workerClass").map(|s| s.trim().to_owned()),
                mental_state: f.string("mentalState").map(|s| s.trim().to_owned()),
                base_commonality: f.parse("baseCommonality").unwrap_or(0.0),
                commonality_factor_per_population: def
                    .node
                    .child("commonalityFactorPerPopulationCurve")
                    .and_then(|c| c.child("points"))
                    .map(curve_points)
                    .unwrap_or_default(),
                intensity,
                anomalous_break: f.bool("anomalousBreak").unwrap_or(false),
                required_trait: f.string("requiredTrait").map(|s| s.trim().to_owned()),
            });
        }
        for def in raw
            .table("MentalStateDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            let mut f = Fields {
                def,
                warnings: &mut warnings,
            };
            defs.mental_states.push(MentalStateDef {
                def_name: def.def_name.clone(),
                label: f.label(),
                state_class: f.string("stateClass").map(|s| s.trim().to_owned()),
                worker_class: f.string("workerClass").map(|s| s.trim().to_owned()),
                colonists_only: f.bool("colonistsOnly").unwrap_or(false),
                downed_can_do: f.bool("downedCanDo").unwrap_or(false),
                stops_jobs: f.bool("stopsJobs").unwrap_or(true),
                recovery_mtb_days: f.parse("recoveryMtbDays").unwrap_or(1.0),
                min_ticks_before_recovery: f.parse("minTicksBeforeRecovery").unwrap_or(500),
                max_ticks_before_recovery: f.parse("maxTicksBeforeRecovery").unwrap_or(99_999_999),
                recover_from_sleep: f.bool("recoverFromSleep").unwrap_or(false),
                recover_from_downed: f.bool("recoverFromDowned").unwrap_or(true),
                mood_recovery_thought: f.string("moodRecoveryThought").map(|s| s.trim().to_owned()),
                base_inspect_line: f.string("baseInspectLine"),
                required_capacities: def.node.child_list_texts("requiredCapacities"),
            });
        }
        for def in raw.table("ManeuverDef").into_iter().flat_map(|t| t.iter()) {
            defs.maneuvers.push(ManeuverDef::from_def(def));
        }
        for def in raw
            .table("PawnCapacityDef")
            .into_iter()
            .flat_map(|t| t.iter())
        {
            defs.capacities
                .push(crate::health::PawnCapacityDef::from_def(def));
        }
        for def in raw.table("StatDef").into_iter().flat_map(|t| t.iter()) {
            defs.stats.push(StatDef::from_def(def, &mut warnings));
        }
        for def in raw.table("WorkGiverDef").into_iter().flat_map(|t| t.iter()) {
            defs.work_givers
                .push(WorkGiverDef::from_def(def, &mut warnings));
        }
        for def in raw.table("WorkTypeDef").into_iter().flat_map(|t| t.iter()) {
            defs.work_types
                .push(WorkTypeDef::from_def(def, &mut warnings));
        }
        // Group givers per work type: descending priorityInType, stable in
        // WorkGiverDef database order.
        let mut by_type: Vec<Vec<(i32, DefId<WorkGiverDef>)>> =
            vec![Vec::new(); defs.work_types.len()];
        for (id, giver) in defs.work_givers.iter() {
            match giver
                .work_type
                .as_deref()
                .and_then(|w| defs.work_types.id(w))
            {
                Some(t) => by_type[t.index()].push((giver.priority_in_type, id)),
                None => {
                    if let Some(w) = &giver.work_type {
                        warnings.push(format!(
                            "WorkGiverDef {}: workType {w} is not a WorkTypeDef",
                            giver.def_name
                        ));
                    }
                }
            }
        }
        for (i, mut givers) in by_type.into_iter().enumerate() {
            givers.sort_by_key(|&(p, _)| std::cmp::Reverse(p));
            defs.work_types.items[i].givers_by_priority =
                givers.into_iter().map(|(_, id)| id).collect();
        }
        for def in raw.table("ThinkTreeDef").into_iter().flat_map(|t| t.iter()) {
            defs.think_trees
                .push(ThinkTreeDef::from_def(def, &mut warnings));
        }
        add_implied_meat(&mut defs, &raw);
        add_implied_corpses(&mut defs, &raw);
        defs.raw = raw;
        warnings.extend(defs.validate_references());
        warnings.extend(defs.validate_think_trees());
        (defs, warnings)
    }

    /// Whether category `name` is `ancestor` or below it.
    pub fn category_within(&self, name: &str, ancestor: &str) -> bool {
        let mut cur = Some(name);
        let mut steps = 0;
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            steps += 1;
            if steps > 64 {
                return false;
            }
            cur = self
                .thing_categories
                .get(c)
                .and_then(|d| d.parent.as_deref());
        }
        false
    }

    /// `ThingCategoryDef.DescendantThingDefs`: things listed in the category
    /// or any category below it.
    pub fn things_in_category(&self, category: &str) -> Vec<DefId<ThingDef>> {
        self.things
            .iter()
            .filter(|(_, t)| {
                t.thing_categories
                    .iter()
                    .any(|c| self.category_within(c, category))
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Checks that every reference field we read points at an existing Def of
    /// the right type.
    pub fn validate_references(&self) -> Vec<String> {
        let mut w = Vec::new();
        for (_, kind) in self.pawn_kinds.iter() {
            match kind.race.as_deref() {
                None => w.push(format!("PawnKindDef {} has no race", kind.def_name)),
                Some(race) => match self.things.get(race) {
                    None => w.push(format!(
                        "PawnKindDef {}: race {race} is not a ThingDef",
                        kind.def_name
                    )),
                    Some(t) if !t.is_pawn() => w.push(format!(
                        "PawnKindDef {}: race {race} is not a pawn ThingDef",
                        kind.def_name
                    )),
                    Some(_) => {}
                },
            }
        }
        for (_, thing) in self.things.iter() {
            if let Some(body) = thing.race.as_ref().and_then(|r| r.body.as_deref())
                && self.raw.get("BodyDef", body).is_none()
            {
                w.push(format!(
                    "ThingDef {}: body {body} is not a BodyDef",
                    thing.def_name
                ));
            }
            if let Some(t) = thing
                .race
                .as_ref()
                .and_then(|r| r.think_tree_main.as_deref())
                && self.think_trees.get(t).is_none()
            {
                w.push(format!(
                    "ThingDef {}: thinkTreeMain {t} is not a ThinkTreeDef",
                    thing.def_name
                ));
            }
            if let Some(m) = thing
                .building
                .as_ref()
                .and_then(|b| b.mineable_thing.as_deref())
                && self.things.get(m).is_none()
            {
                w.push(format!(
                    "ThingDef {}: mineableThing {m} is not a ThingDef",
                    thing.def_name
                ));
            }
        }
        w
    }

    /// Validates `ThinkNode_Subtree` references between think trees.
    fn validate_think_trees(&self) -> Vec<String> {
        let mut w = Vec::new();
        for (_, tree) in self.think_trees.iter() {
            for r in tree.subtree_refs() {
                if self.think_trees.get(r).is_none() {
                    w.push(format!(
                        "ThinkTreeDef {}: subtree {r} not found",
                        tree.def_name
                    ));
                }
            }
        }
        w
    }

    /// Work types in database order.
    pub fn work_type_ids(&self) -> impl Iterator<Item = DefId<WorkTypeDef>> + '_ {
        self.work_types.iter().map(|(id, _)| id)
    }

    /// A StatDef's `defaultBaseValue` (`None` if the StatDef is missing).
    pub fn stat_default(&self, stat: &str) -> Option<f32> {
        self.raw
            .get("StatDef", stat)?
            .node
            .child_text("defaultBaseValue")?
            .parse()
            .ok()
    }

    /// A StatDef's `valueIfMissing` (used e.g. for rest effectiveness when
    /// sleeping without a bed).
    pub fn stat_value_if_missing(&self, stat: &str) -> Option<f32> {
        self.raw
            .get("StatDef", stat)?
            .node
            .child_text("valueIfMissing")?
            .parse()
            .ok()
    }

    /// A thing's base stat: its own `statBases` entry, else the StatDef
    /// default. (Stat parts, offsets and factors are not applied yet.)
    // COMPATIBILITY TODO: currently approximate — stat parts, offsets, factors and
    // post-processing (StatWorker) are not applied.
    pub fn base_stat(&self, thing: &ThingDef, stat: &str) -> Option<f32> {
        thing.stat(stat).or_else(|| self.stat_default(stat))
    }

    /// `baseMoodEffect` of a ThoughtDef's first stage (used to judge food).
    pub fn thought_first_stage_mood(&self, thought: &str) -> Option<f32> {
        self.raw
            .get("ThoughtDef", thought)?
            .node
            .child("stages")?
            .children
            .first()?
            .child_text("baseMoodEffect")?
            .parse()
            .ok()
    }

    /// Resolves a PawnKindDef's race to its ThingDef.
    pub fn race_of(&self, kind: &PawnKindDef) -> Option<DefId<ThingDef>> {
        self.things.id(kind.race.as_deref()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_documents;
    use crate::xml::ActivePackages;

    const XML: &str = r#"<Defs>
      <TerrainDef Name="NaturalTerrainBase" Abstract="True">
        <affordances><li>Walkable</li></affordances>
        <natural>true</natural>
      </TerrainDef>
      <TerrainDef ParentName="NaturalTerrainBase">
        <defName>Soil</defName><label>soil</label>
        <texturePath>Terrain/Surfaces/Soil</texturePath>
        <pathCost>2</pathCost><fertility>1.0</fertility>
        <affordances><li>Light</li></affordances>
      </TerrainDef>
      <TerrainDef ParentName="NaturalTerrainBase">
        <defName>WaterDeep</defName><passability>Impassable</passability>
        <color>(0.2, 0.3, 0.5)</color><pathCost>oops</pathCost>
      </TerrainDef>
      <ThingDef Name="BasePawn" Abstract="True">
        <category>Pawn</category><selectable>true</selectable>
        <statBases><Mass>60</Mass></statBases>
        <race><baseBodySize>2</baseBodySize></race>
      </ThingDef>
      <ThingDef ParentName="BasePawn">
        <defName>Human</defName><label>human</label>
        <statBases><MoveSpeed>4.6</MoveSpeed></statBases>
        <race><body>Human</body><baseBodySize>1</baseBodySize><intelligence>Humanlike</intelligence></race>
      </ThingDef>
      <ThingDef Name="RockBase" Abstract="True">
        <passability>Impassable</passability>
        <graphicData><texPath>Things/Building/Linked/Rock_Atlas</texPath></graphicData>
        <building><isNaturalRock>true</isNaturalRock></building>
      </ThingDef>
      <ThingDef ParentName="RockBase">
        <defName>Granite</defName><category>Building</category>
        <graphicData><color>(105,95,97)</color></graphicData>
        <building><mineableThing>ChunkGranite</mineableThing></building>
      </ThingDef>
      <BodyDef><defName>Human</defName></BodyDef>
      <PawnKindDef Name="BasePlayerPawnKind" Abstract="True"><race>Human</race></PawnKindDef>
      <PawnKindDef ParentName="BasePlayerPawnKind"><defName>Colonist</defName><label>colonist</label></PawnKindDef>
      <NeedDef><defName>Rest</defName><needClass>Need_Rest</needClass><label>sleep</label>
        <major>true</major><listPriority>700</listPriority></NeedDef>
      <StatDef><defName>RestRateMultiplier</defName><defaultBaseValue>1.0</defaultBaseValue></StatDef>
      <PawnKindDef><defName>Ghost</defName><race>Granite</race></PawnKindDef>
      <JobDef><defName>Wait</defName><driverClass>JobDriver_Wait</driverClass>
        <reportString>standing.</reportString><isIdle>true</isIdle></JobDef>
    </Defs>"#;

    fn load() -> (GameDefs, Vec<String>) {
        let (db, report) =
            load_documents("core", &[("x.xml", XML)], &ActivePackages::new(["core"]));
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        GameDefs::from_database(db)
    }

    #[test]
    fn terrain_fields() {
        let (defs, _) = load();
        let soil = defs.terrain.get("Soil").unwrap();
        assert_eq!(soil.path_cost, 2);
        assert_eq!(soil.affordances, vec!["Walkable", "Light"]);
        assert!(soil.natural && soil.is_walkable());
        assert_eq!(soil.texture_path.as_deref(), Some("Terrain/Surfaces/Soil"));
        let water = defs.terrain.get("WaterDeep").unwrap();
        assert!(!water.is_walkable());
        assert_eq!(water.label, "WaterDeep"); // falls back to defName
        assert!(water.color.is_some());
    }

    #[test]
    fn thing_fields_with_inheritance() {
        let (defs, _) = load();
        let human = defs.things.get("Human").unwrap();
        assert!(human.is_pawn() && human.selectable);
        assert_eq!(human.stat("MoveSpeed"), Some(4.6));
        assert_eq!(human.stat("Mass"), Some(60.0));
        let race = human.race.as_ref().unwrap();
        assert_eq!(race.base_body_size, 1.0);
        assert_eq!(race.body.as_deref(), Some("Human"));

        let granite = defs.things.get("Granite").unwrap();
        assert_eq!(granite.passability, Passability::Impassable);
        let g = granite.graphic.as_ref().unwrap();
        assert_eq!(
            g.tex_path.as_deref(),
            Some("Things/Building/Linked/Rock_Atlas")
        );
        assert!(g.color.is_some());
        assert!(granite.building.as_ref().unwrap().is_natural_rock);
    }

    #[test]
    fn references_resolve_and_bad_ones_warn() {
        let (defs, warnings) = load();
        let colonist = defs.pawn_kinds.get("Colonist").unwrap();
        let race = defs.race_of(colonist).unwrap();
        assert_eq!(defs.things[race].def_name, "Human");
        assert!(
            warnings.iter().any(|w| w.contains("pathCost")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("Ghost") && w.contains("not a pawn"))
        );
        assert!(warnings.iter().any(|w| w.contains("ChunkGranite")));
        assert!(!warnings.iter().any(|w| w.contains("body Human")));
    }

    #[test]
    fn need_defs_and_stat_defaults() {
        let (defs, _) = load();
        let rest = defs.needs.get("Rest").unwrap();
        assert_eq!(rest.need_class.as_deref(), Some("Need_Rest"));
        assert_eq!(rest.label, "sleep");
        assert!(rest.major);
        assert_eq!(rest.list_priority, 700);
        let human = defs.things.get("Human").unwrap();
        assert_eq!(defs.base_stat(human, "RestRateMultiplier"), Some(1.0));
        assert_eq!(defs.base_stat(human, "MoveSpeed"), Some(4.6));
        assert_eq!(defs.base_stat(human, "Nope"), None);
        assert_eq!(human.race.as_ref().unwrap().base_hunger_rate, 1.0);
    }

    #[test]
    fn job_defs() {
        let (defs, _) = load();
        let wait = defs.jobs.get("Wait").unwrap();
        assert_eq!(wait.report_string, "standing.");
        assert_eq!(wait.driver_class.as_deref(), Some("JobDriver_Wait"));
        assert!(wait.is_idle);
    }

    #[test]
    fn ingestible_and_diet() {
        let xml = r#"<Defs>
          <ThingDef Name="FoodBase" Abstract="True"><stackLimit>75</stackLimit>
            <ingestible><foodType>VegetableOrFruit</foodType></ingestible></ThingDef>
          <ThingDef ParentName="FoodBase"><defName>RawThing</defName>
            <ingestible><preferability>RawBad</preferability><tasteThought>Yuck</tasteThought></ingestible></ThingDef>
          <ThingDef><defName>Meal</defName><stackLimit>10</stackLimit>
            <ingestible><foodType>Meal</foodType><preferability>MealSimple</preferability>
              <maxNumToIngestAtOnce>1</maxNumToIngestAtOnce><optimalityOffsetHumanlikes>16</optimalityOffsetHumanlikes>
            </ingestible></ThingDef>
          <ThingDef><defName>Eater</defName><category>Pawn</category>
            <race><foodType>OmnivoreHuman</foodType></race></ThingDef>
          <ThingDef><defName>Grazer</defName><category>Pawn</category>
            <race><foodType>VegetarianRoughAnimal, Seed</foodType></race></ThingDef>
          <ThingDef><defName>Hunter</defName><category>Pawn</category>
            <race><foodType>CarnivoreAnimal</foodType></race></ThingDef>
          <ThoughtDef><defName>Yuck</defName><stages><li><baseMoodEffect>-7</baseMoodEffect></li></stages></ThoughtDef>
        </Defs>"#;
        let (db, r) = load_documents("core", &[("f.xml", xml)], &ActivePackages::new(["core"]));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let (defs, w) = GameDefs::from_database(db);
        assert!(w.is_empty(), "{w:?}");
        let raw = defs.things.get("RawThing").unwrap();
        let i = raw.ingestible.as_ref().unwrap();
        assert_eq!(i.preferability, FoodPreferability::RawBad);
        assert_eq!(i.food_type, food_type::VEGETABLE_OR_FRUIT);
        assert_eq!((i.base_ingest_ticks, i.max_num_to_ingest_at_once), (500, 0));
        assert_eq!(raw.stack_limit, 75);
        let meal = defs.things.get("Meal").unwrap();
        let mi = meal.ingestible.as_ref().unwrap();
        assert_eq!(mi.max_num_to_ingest_at_once, 1);
        assert_eq!(mi.optimality_offset_humanlikes, 16.0);
        assert!(FoodPreferability::MealSimple > FoodPreferability::RawBad);

        let human = defs.things.get("Eater").unwrap().race.as_ref().unwrap();
        assert_eq!(human.diet(), DietCategory::Omnivorous);
        assert_eq!(human.food_level_percentage_want_eat(), 0.3);
        assert!(human.can_ever_eat(raw) && human.can_ever_eat(meal));
        let grazer = defs.things.get("Grazer").unwrap().race.as_ref().unwrap();
        assert_eq!(grazer.diet(), DietCategory::Herbivorous);
        assert_eq!(grazer.food_level_percentage_want_eat(), 0.45);
        // Fungus shares the VegetableOrFruit bit, so grazers eat raw plants.
        assert!(grazer.can_ever_eat(raw) && grazer.can_ever_eat(meal));
        let hunter = defs.things.get("Hunter").unwrap().race.as_ref().unwrap();
        assert_eq!(hunter.diet(), DietCategory::Carnivorous);
        assert!(!hunter.can_ever_eat(raw) && hunter.can_ever_eat(meal));
        assert_eq!(defs.thought_first_stage_mood("Yuck"), Some(-7.0));
    }

    #[test]
    fn natural_rock_implies_stone_terrains() {
        let (defs, _) = load();
        let rock = defs
            .things
            .iter()
            .find(|(_, t)| t.building.as_ref().is_some_and(|b| b.is_natural_rock))
            .map(|(_, t)| t.clone())
            .expect("the fixture has a natural rock");
        let b = rock.building.as_ref().unwrap();
        let hewn = defs
            .terrain
            .get(b.leave_terrain.as_deref().unwrap())
            .unwrap();
        assert_eq!(hewn.def_name, format!("{}_RoughHewn", rock.def_name));
        assert_eq!(hewn.path_cost, 1);
        let rough = defs
            .terrain
            .get(b.natural_terrain.as_deref().unwrap())
            .unwrap();
        assert_eq!(rough.path_cost, 2);
        let smooth = defs
            .terrain
            .get(&format!("{}_Smooth", rock.def_name))
            .unwrap();
        assert_eq!(smooth.path_cost, 0);
    }

    #[test]
    fn def_ids_are_stable_handles() {
        let (defs, _) = load();
        let id = defs.terrain.id("Soil").unwrap();
        assert_eq!(defs.terrain[id].def_name, "Soil");
        // Two from the XML, three implied by the natural rock.
        assert_eq!(defs.terrain.iter().count(), 5);
        assert!(defs.terrain.id("Nope").is_none());
    }
}
