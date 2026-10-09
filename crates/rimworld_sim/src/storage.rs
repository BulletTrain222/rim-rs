//! Storage (docs/research.md §21): stockpile zones with a priority and a
//! filter, kept in the game's priority order (`HaulDestinationManager`).

use rimworld_defs::{DefId, GameDefs, ThingDef};

use crate::grid::{Cell, Grid, GridSize};

/// `StoragePriority`; higher is better.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum StoragePriority {
    Unstored,
    Low,
    #[default]
    Normal,
    Preferred,
    Important,
    Critical,
}

impl StoragePriority {
    /// One step better (saturating at Critical; never back to Unstored).
    pub fn raised(self) -> Self {
        use StoragePriority::*;
        match self {
            Unstored | Low => Normal,
            Normal => Preferred,
            Preferred => Important,
            Important | Critical => Critical,
        }
    }

    /// One step worse (saturating at Low).
    pub fn lowered(self) -> Self {
        use StoragePriority::*;
        match self {
            Critical => Important,
            Important => Preferred,
            Preferred => Normal,
            Normal | Low | Unstored => Low,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StoragePriority::Unstored => "unstored",
            StoragePriority::Low => "low",
            StoragePriority::Normal => "normal",
            StoragePriority::Preferred => "preferred",
            StoragePriority::Important => "important",
            StoragePriority::Critical => "critical",
        }
    }
}

/// `StorageSettingsPreset`: how a new stockpile's filter starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePreset {
    /// Foods, Manufactured, ResourcesRaw, Items, Buildings, Weapons,
    /// Apparel and BodyParts (no chunks or corpses).
    DefaultStockpile,
    /// Corpses and Chunks.
    DumpingStockpile,
    /// Corpses.
    CorpseStockpile,
}

/// `ThingFilter` over defs: which things a storage takes.
// COMPATIBILITY TODO: currently approximate — special filters (rotten,
// large corpses, ...), hit point and quality ranges are not modelled.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThingFilter {
    /// One flag per `ThingDef`.
    allowed: Vec<bool>,
    /// Disallowed special filters (`disallowedSpecialFilters`), by def name.
    #[serde(default)]
    disallowed_special: Vec<String>,
}

impl ThingFilter {
    /// `ThingFilter.SetFromPreset` on an empty filter.
    pub fn preset(defs: &GameDefs, preset: StoragePreset) -> Self {
        let categories: &[&str] = match preset {
            StoragePreset::DefaultStockpile => &[
                "Foods",
                "Manufactured",
                "ResourcesRaw",
                "Items",
                "Buildings",
                "Weapons",
                "Apparel",
                "BodyParts",
            ],
            StoragePreset::DumpingStockpile => &["Corpses", "Chunks"],
            StoragePreset::CorpseStockpile => &["Corpses"],
        };
        let mut f = Self::default();
        for c in categories {
            for def in defs.things_in_category(c) {
                f.set(def, true);
            }
        }
        f
    }

    /// Every def (a test convenience: stockpiles of synthetic defs).
    pub fn everything(defs: &GameDefs) -> Self {
        Self {
            allowed: vec![true; defs.things.len()],
            disallowed_special: Vec::new(),
        }
    }

    /// The defs of a category and below (`SetAllow(category)`).
    pub fn category(defs: &GameDefs, category: &str) -> Self {
        let mut f = Self::default();
        for def in defs.things_in_category(category) {
            f.set(def, true);
        }
        f
    }

    /// A filter resolved from its XML (`ThingFilter.ResolveReferences`):
    /// special filters not allowed by default off, then thing defs and
    /// categories allowed, categories disallowed, special filters allowed
    /// and disallowed, thing defs disallowed.
    // COMPATIBILITY TODO: currently approximate — trade/thing-set tags,
    // stuff categories, `allowAllWhoCanMake` and the preferability,
    // edibility and drug options are not read.
    pub fn from_spec(defs: &GameDefs, spec: &rimworld_defs::ThingFilterSpec) -> Self {
        let mut f = Self::default();
        for (_, s) in defs.special_filters.iter() {
            if !s.allowed_by_default {
                f.set_special(&s.def_name, false);
            }
        }
        for name in &spec.thing_defs {
            if let Some(d) = defs.things.id(name) {
                f.set(d, true);
            }
        }
        for c in &spec.categories {
            f.set_category(defs, c, true);
        }
        for c in &spec.disallowed_categories {
            f.set_category(defs, c, false);
        }
        for s in &spec.special_filters_to_allow {
            f.set_special(s, true);
        }
        for s in &spec.special_filters_to_disallow {
            f.set_special(s, false);
        }
        if spec.disallow_doesnt_produce_meat {
            // Races without meat, and their corpses (which share the race).
            for (id, t) in defs.things.iter() {
                let race = t.race.as_ref().or_else(|| {
                    t.corpse_of
                        .as_deref()
                        .and_then(|r| defs.things.get(r)?.race.as_ref())
                });
                if race.is_some_and(|r| !r.has_meat) {
                    f.set(id, false);
                }
            }
        }
        for name in &spec.disallowed_thing_defs {
            if let Some(d) = defs.things.id(name) {
                f.set(d, false);
            }
        }
        f
    }

    /// `SetAllow(category)`: its defs and its configurable special filters.
    pub fn set_category(&mut self, defs: &GameDefs, category: &str, allow: bool) {
        for d in defs.things_in_category(category) {
            self.set(d, allow);
        }
        let specials: Vec<String> = defs
            .special_filters
            .iter()
            .filter(|(_, s)| {
                s.parent_category
                    .as_deref()
                    .is_some_and(|p| defs.category_within(p, category))
                    && !defs
                        .raw
                        .get("SpecialThingFilterDef", &s.def_name)
                        .and_then(|r| r.node.child_text("configurable"))
                        .is_some_and(|v| v.trim() == "false")
            })
            .map(|(_, s)| s.def_name.clone())
            .collect();
        for s in specials {
            self.set_special(&s, allow);
        }
    }

    pub fn set_special(&mut self, name: &str, allow: bool) {
        self.disallowed_special.retain(|s| s != name);
        if !allow {
            self.disallowed_special.push(name.to_owned());
        }
    }

    /// The disallowed special filters.
    pub fn disallowed_special(&self) -> &[String] {
        &self.disallowed_special
    }

    pub fn allows(&self, def: DefId<ThingDef>) -> bool {
        self.allowed.get(def.index()).copied().unwrap_or(false)
    }

    pub fn set(&mut self, def: DefId<ThingDef>, allow: bool) {
        if self.allowed.len() <= def.index() {
            self.allowed.resize(def.index() + 1, false);
        }
        self.allowed[def.index()] = allow;
    }

    /// How many defs it allows.
    pub fn count(&self) -> usize {
        self.allowed.iter().filter(|&&a| a).count()
    }
}

/// A stockpile zone (`Zone_Stockpile` with its `StorageSettings`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stockpile {
    pub priority: StoragePriority,
    /// Cells in the zone's list order (painting order; see research §21).
    pub cells: Vec<Cell>,
    /// What the zone takes.
    pub filter: ThingFilter,
    /// Made by the dumping stockpile designator (its name).
    #[serde(default)]
    pub dumping: bool,
    /// `Zone.label`'s number ("Stockpile zone 2"); 0: unnamed.
    #[serde(default)]
    pub label_number: u32,
    /// `Zone.color` (RGBA), if one was given.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
}

impl Stockpile {
    /// `StorageSettings.AllowedToAccept` for a def.
    pub fn accepts(&self, defs: &GameDefs, def: DefId<ThingDef>) -> bool {
        defs.things[def].ever_storable() && self.filter.allows(def)
    }
}

pub type ZoneId = usize;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Storage {
    zones: Vec<Stockpile>,
    /// Zone ids by descending priority, equal priorities in their current
    /// list order (stable insertion sort on every change).
    order: Vec<ZoneId>,
    grid: Grid<Option<ZoneId>>,
    /// Bumped on every change (front-ends redraw zones when it changes).
    pub revision: u64,
}

impl Storage {
    pub fn new(size: GridSize) -> Self {
        Self {
            zones: Vec::new(),
            order: Vec::new(),
            grid: Grid::new(size, None),
            revision: 0,
        }
    }

    /// Adds a stockpile over `cells` (cells already in a zone are skipped).
    pub fn add_stockpile(
        &mut self,
        priority: StoragePriority,
        cells: &[Cell],
        filter: ThingFilter,
    ) -> ZoneId {
        let id = self.zones.len();
        let mut own = Vec::new();
        for &c in cells {
            if self.grid.get(c).is_some_and(|z| z.is_none()) {
                self.grid[c] = Some(id);
                own.push(c);
            }
        }
        self.zones.push(Stockpile {
            priority,
            cells: own,
            filter,
            dumping: false,
            label_number: 0,
            color: None,
        });
        self.order.push(id);
        self.resort();
        id
    }

    /// Replaces a zone's filter.
    pub fn set_filter(&mut self, id: ZoneId, filter: ThingFilter) {
        if let Some(z) = self.zones.get_mut(id) {
            z.filter = filter;
            self.revision += 1;
        }
    }

    /// Appends free cells to a zone (`Zone.AddCell`: list order is the
    /// order cells were added).
    pub fn add_cells(&mut self, zone: ZoneId, cells: &[Cell]) {
        for &c in cells {
            if self.grid.get(c).is_some_and(|z| z.is_none()) {
                self.grid[c] = Some(zone);
                self.zones[zone].cells.push(c);
            }
        }
        self.revision += 1;
    }

    /// Removes a cell from its zone, keeping the other cells' order; a zone
    /// left without cells is deleted (it leaves the priority list).
    pub fn remove_cell(&mut self, c: Cell) {
        let Some(z) = self.zone_at(c) else { return };
        self.grid[c] = None;
        self.zones[z].cells.retain(|&x| x != c);
        if self.zones[z].cells.is_empty() {
            self.order.retain(|&x| x != z);
        }
        self.revision += 1;
    }

    /// Zones still in use.
    pub fn live_zones(&self) -> impl Iterator<Item = ZoneId> + '_ {
        self.order.iter().copied()
    }

    pub fn set_priority(&mut self, zone: ZoneId, priority: StoragePriority) {
        self.zones[zone].priority = priority;
        self.resort();
    }

    pub fn set_allowed(&mut self, zone: ZoneId, def: DefId<ThingDef>, allowed: bool) {
        self.zones[zone].filter.set(def, allowed);
        self.revision += 1;
    }

    fn resort(&mut self) {
        self.revision += 1;
        let zones = &self.zones;
        // Stable: equal priorities keep their current order.
        self.order
            .sort_by(|&a, &b| zones[b].priority.cmp(&zones[a].priority));
    }

    pub fn zone(&self, id: ZoneId) -> &Stockpile {
        &self.zones[id]
    }

    /// Sets a zone's name and colour.
    pub fn set_meta(&mut self, id: ZoneId, dumping: bool, label_number: u32, color: [f32; 4]) {
        let z = &mut self.zones[id];
        z.dumping = dumping;
        z.label_number = label_number;
        z.color = Some(color);
        self.revision += 1;
    }

    pub fn zones(&self) -> &[Stockpile] {
        &self.zones
    }

    /// Zones by descending priority (`AllGroupsListInPriorityOrder`).
    pub fn in_priority_order(&self) -> &[ZoneId] {
        &self.order
    }

    pub fn zone_at(&self, c: Cell) -> Option<ZoneId> {
        self.grid.get(c).copied().flatten()
    }
}
