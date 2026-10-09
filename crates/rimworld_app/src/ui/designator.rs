//! Designators as the game runs them (`Designator`, `DesignatorManager`,
//! `DesignationDragger`, `DrawStyle*`; docs/research.md §66). One is
//! selected at a time and stays selected after use; right click or Escape
//! deselects it. With a draw style a left drag designates the style's cells
//! that the designator accepts (a click is a one-cell drag); without one
//! (most buildings) a click places at once. Placing designators show a
//! green or red ghost and rotate with Q/E.
//!
//! Every designation goes through the simulation's designator rules
//! (`Sim::can_designate_cell`, `zone_add`, `can_place_blueprint`, ...).

use std::collections::{HashMap, HashSet};

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::{DefId, GameDefs, TerrainDef, ThingDef};
use rimworld_sim::sim::{OrderDesignator, ZoneKind};
use rimworld_sim::{Cell, Rot4, Sim};

use super::lang::{cap, made_of};
use super::select::{Selectable, Selection, WorldFrame};
use super::{Align, FONT_SMALL, Lang, Messages, Paint, UiInput, layer};
use crate::SimState;
use crate::camera::PointerClicks;
use crate::graphics::GameTextures;
use crate::thing_graphics::{ThingTextures, draw_color, look};
use crate::view::{CELL_SIZE, cell_center, world_to_cell};

/// A designator (`Designator_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Des {
    Order(OrderDesignator),
    Zone(ZoneKind),
    ZoneDelete,
    Build(DefId<ThingDef>),
    Floor(DefId<TerrainDef>),
}

/// `DrawStyleDef`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    FilledRectangle,
    EmptyRectangle,
    FilledOval,
    EmptyOval,
    Line,
    AngledLine,
}

impl Style {
    pub fn label(self) -> &'static str {
        match self {
            Style::FilledRectangle => "filled rectangle",
            Style::EmptyRectangle => "empty rectangle",
            Style::FilledOval => "filled oval",
            Style::EmptyOval => "empty oval",
            Style::Line => "line",
            Style::AngledLine => "angled line",
        }
    }

    /// `drawOutline`.
    fn outline(self) -> bool {
        !matches!(self, Style::Line | Style::AngledLine)
    }

    /// `DrawStyle.Update`: the cells from `a` (drag start) to `b`.
    pub fn cells(self, a: Cell, b: Cell) -> Vec<Cell> {
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (z0, z1) = (a.z.min(b.z), a.z.max(b.z));
        let rect = || (z0..=z1).flat_map(move |z| (x0..=x1).map(move |x| Cell::new(x, z)));
        let mut out: Vec<Cell> = match self {
            Style::FilledRectangle => rect().collect(),
            Style::EmptyRectangle => {
                // `CellRect.EdgeCells`: bottom left→right, right side up, top
                // right→left, left side down.
                let mut v = Vec::new();
                for x in x0..=x1 {
                    v.push(Cell::new(x, z0));
                }
                for z in z0 + 1..=z1 {
                    v.push(Cell::new(x1, z));
                }
                if z1 > z0 {
                    for x in (x0..x1).rev() {
                        v.push(Cell::new(x, z1));
                    }
                }
                if x1 > x0 {
                    for z in (z0 + 1..z1).rev() {
                        v.push(Cell::new(x0, z));
                    }
                }
                v
            }
            Style::Line => {
                if (a.x - b.x).abs() >= (a.z - b.z).abs() {
                    (x0..=x1).map(|x| Cell::new(x, a.z)).collect()
                } else {
                    (z0..=z1).map(|z| Cell::new(a.x, z)).collect()
                }
            }
            Style::AngledLine => angled_line(a, b),
            Style::FilledOval | Style::EmptyOval => {
                let (w, h) = ((x1 - x0 + 1) as f32, (z1 - z0 + 1) as f32);
                let radius = w / 2.0;
                let ratio = w / h;
                let mut cx = (x0 + (x1 - x0 + 1) / 2) as f32;
                let mut cz = (z0 + (z1 - z0 + 1) / 2) as f32;
                if (x1 - x0 + 1) % 2 == 0 {
                    cx -= 0.5;
                }
                if (z1 - z0 + 1) % 2 == 0 {
                    cz -= 0.5;
                }
                let inside = |x: f32, z: f32| {
                    (z * ratio).powi(2) + x.powi(2) <= (radius + 0.4) * (radius + 0.4)
                };
                rect()
                    .filter(|c| {
                        let (x, z) = (c.x as f32 - cx, c.z as f32 - cz);
                        if !inside(x, z) {
                            return false;
                        }
                        if self == Style::FilledOval {
                            return true;
                        }
                        !(inside(x + 1.0, z)
                            && inside(x - 1.0, z)
                            && inside(x, z + 1.0)
                            && inside(x, z - 1.0)
                            && inside(x + 1.0, z + 1.0)
                            && inside(x + 1.0, z - 1.0)
                            && inside(x - 1.0, z - 1.0))
                            || !inside(x - 1.0, z + 1.0)
                    })
                    .collect()
            }
        };
        // Styles that may repeat a cell drop the repeats.
        let mut seen = HashSet::new();
        out.retain(|c| seen.insert(*c));
        out
    }
}

/// `DrawStyle_AngledLine`: a padded Bresenham line, middle out.
fn angled_line(a: Cell, b: Cell) -> Vec<Cell> {
    let (mut x, mut z) = (a.x, a.z);
    let (dx, dz) = ((b.x - a.x).abs(), (b.z - a.z).abs());
    let (sx, sz) = (
        if a.x < b.x { 1 } else { -1 },
        if a.z < b.z { 1 } else { -1 },
    );
    let x_major = dx >= dz;
    let (dx4, dz4) = (dx * 4, dz * 4);
    let mut err = dx4 / 2 - dz4 / 2;
    let n = dx + dz + 1;
    let mut line = Vec::new();
    for _ in 0..n {
        line.push(Cell::new(x, z));
        if err > 0 || (err == 0 && x_major) {
            x += sx;
            err -= dz4;
        } else {
            z += sz;
            err += dx4;
        }
    }
    let count = line.len();
    let mid = (count - 1) / 2;
    let mut out = vec![line[mid]];
    for i in 1..=mid {
        out.push(line[mid - i]);
        if mid + i < count {
            out.push(line[mid + i]);
        }
    }
    if count % 2 == 0 {
        out.push(line[count - 1]);
    }
    out
}

/// `DrawStyleCategoryDef`s: their styles, the first selected by default.
pub fn style_category(name: &str) -> &'static [Style] {
    use Style::*;
    match name {
        "Walls" | "Conduits" | "Defenses" => &[Line, AngledLine, EmptyRectangle, EmptyOval],
        "Plants" | "RemoveZones" => &[FilledRectangle, FilledOval],
        "FilledRectangle" => &[FilledRectangle],
        // Default2D: Zones, Areas, Orders, Mine, Floors, Cancel, ...
        _ => &[
            FilledRectangle,
            EmptyRectangle,
            FilledOval,
            EmptyOval,
            Line,
            AngledLine,
        ],
    }
}

/// What the Architect and the info box show for a designator.
pub struct DesInfo {
    pub label: String,
    pub desc: String,
    /// Icon texture path (build designators draw the thing instead).
    pub icon: Option<&'static str>,
    pub hotkey: Option<KeyCode>,
    /// `Gizmo.Order` (`uiOrder` for buildables).
    pub order: f32,
    pub style_category: Option<&'static str>,
}

/// The game's `KeyBindingDef`s used by designators.
pub fn key_binding(name: &str) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match name {
        "Misc1" => KeyB,
        "Misc2" => KeyH,
        "Misc3" => KeyY,
        "Misc4" => KeyN,
        "Misc5" => KeyJ,
        "Misc6" => KeyU,
        "Misc7" => KeyM,
        "Misc8" => KeyK,
        "Misc9" => KeyI,
        "Misc10" => KeyL,
        "Misc11" => KeyO,
        "Misc12" => KeyP,
        "Designator_Cancel" => KeyC,
        "Designator_Deconstruct" => KeyX,
        "Command_ColonistDraft" => KeyR,
        "Command_ItemForbid" => KeyF,
        "Command_TogglePower" => KeyV,
        _ => return None,
    })
}

/// The name a key is shown with on a gizmo.
pub fn key_label(k: KeyCode) -> String {
    let s = format!("{k:?}");
    s.strip_prefix("Key").unwrap_or(&s).to_owned()
}

impl Des {
    pub fn info(self, sim: &Sim, lang: &Lang, mgr: &DesignatorManager) -> DesInfo {
        let defs = &sim.defs;
        let order_info = |label: &str,
                          desc: &str,
                          icon: &'static str,
                          key: Option<&str>,
                          style: &'static str| DesInfo {
            label: lang.tr(label),
            desc: lang.tr(desc),
            icon: Some(icon),
            hotkey: key.and_then(key_binding),
            order: 0.0,
            style_category: Some(style),
        };
        match self {
            Des::Order(d) => match d {
                OrderDesignator::Cancel => order_info(
                    "DesignatorCancel",
                    "DesignatorCancelDesc",
                    "UI/Designators/Cancel",
                    Some("Designator_Cancel"),
                    "Cancel",
                ),
                OrderDesignator::Deconstruct => order_info(
                    "DesignatorDeconstruct",
                    "DesignatorDeconstructDesc",
                    "UI/Designators/Deconstruct",
                    Some("Designator_Deconstruct"),
                    "Orders",
                ),
                OrderDesignator::Mine => order_info(
                    "DesignatorMine",
                    "DesignatorMineDesc",
                    "UI/Designators/Mine",
                    Some("Misc10"),
                    "Mine",
                ),
                OrderDesignator::HarvestWood => order_info(
                    "DesignatorHarvestWood",
                    "DesignatorHarvestWoodDesc",
                    "UI/Designators/HarvestWood",
                    Some("Misc1"),
                    "Plants",
                ),
                OrderDesignator::CutPlants => order_info(
                    "DesignatorCutPlants",
                    "DesignatorCutPlantsDesc",
                    "UI/Designators/CutPlants",
                    Some("Misc3"),
                    "Plants",
                ),
                OrderDesignator::Harvest => order_info(
                    "DesignatorHarvest",
                    "DesignatorHarvestDesc",
                    "UI/Designators/Harvest",
                    Some("Misc2"),
                    "Plants",
                ),
                OrderDesignator::Hunt => order_info(
                    "DesignatorHunt",
                    "DesignatorHuntDesc",
                    "UI/Designators/Hunt",
                    Some("Misc11"),
                    "FilledRectangle",
                ),
                OrderDesignator::SmoothSurface => order_info(
                    "DesignatorSmoothSurface",
                    "DesignatorSmoothSurfaceDesc",
                    "UI/Designators/SmoothSurface",
                    None,
                    "Orders",
                ),
                OrderDesignator::RemoveFloor => order_info(
                    "DesignatorRemoveFloor",
                    "DesignatorRemoveFloorDesc",
                    "UI/Designators/RemoveFloor",
                    Some("Misc1"),
                    "Floors",
                ),
            },
            Des::Zone(ZoneKind::Stockpile) => order_info(
                "Stockpile",
                "DesignatorZoneCreateStorageResourcesDesc",
                "UI/Designators/ZoneCreate_Stockpile",
                Some("Misc1"),
                "Zones",
            ),
            Des::Zone(ZoneKind::DumpingStockpile) => order_info(
                "DumpingStockpile",
                "DesignatorZoneCreateStorageDumpingDesc",
                "UI/Designators/ZoneCreate_DumpingStockpile",
                Some("Misc6"),
                "Zones",
            ),
            Des::Zone(ZoneKind::Growing) => order_info(
                "GrowingZone",
                "DesignatorGrowingZoneDesc",
                "UI/Designators/ZoneCreate_Growing",
                Some("Misc2"),
                "Zones",
            ),
            Des::ZoneDelete => order_info(
                "DesignatorZoneDelete",
                "DesignatorZoneDeleteDesc",
                "UI/Designators/ZoneDelete",
                Some("Misc3"),
                "RemoveZones",
            ),
            Des::Build(def) => {
                let d = &defs.things[def];
                // `Designator_Build.Label`: "wall..." until a stuff is
                // chosen, then "granite wall".
                let label = if d.made_from_stuff() {
                    if mgr.write_stuff.contains(&def) {
                        made_of(sim, lang, def, mgr.stuff_for(sim, def))
                    } else {
                        format!("{}...", d.label)
                    }
                } else {
                    d.label.clone()
                };
                DesInfo {
                    label,
                    desc: d.description.clone().unwrap_or_default(),
                    icon: None,
                    hotkey: d.designation_hot_key.as_deref().and_then(key_binding),
                    order: d.ui_order,
                    style_category: d.draw_style_category.as_deref().map(static_style_name),
                }
            }
            Des::Floor(t) => {
                let d = &defs.terrain[t];
                DesInfo {
                    label: d.label.clone(),
                    desc: String::new(),
                    icon: None,
                    hotkey: None,
                    order: d.ui_order,
                    style_category: Some(
                        d.draw_style_category
                            .as_deref()
                            .map_or("Floors", static_style_name),
                    ),
                }
            }
        }
    }
}

fn static_style_name(s: &str) -> &'static str {
    match s {
        "Walls" => "Walls",
        "Conduits" => "Conduits",
        "Defenses" => "Defenses",
        "Floors" => "Floors",
        "Plants" => "Plants",
        "FilledRectangle" => "FilledRectangle",
        _ => "Default2D",
    }
}

/// `DesignatorManager`: the selected designator, its draw style, the
/// placing rotation and each build designator's chosen stuff.
#[derive(Resource)]
pub struct DesignatorManager {
    pub selected: Option<Des>,
    pub style: Option<Style>,
    pub rot: Rot4,
    /// `Designator_Build.stuffDef`, by building.
    stuff: HashMap<DefId<ThingDef>, DefId<ThingDef>>,
    /// `writeStuff`: the designator's label names its stuff.
    pub write_stuff: HashSet<DefId<ThingDef>>,
    /// `previouslySelected` styles by category (`Prefs.RememberDrawStyles`).
    remembered: HashMap<&'static str, Style>,
    /// The drag's start cell (`DesignationDragger.startDragCell`).
    pub drag_start: Option<Cell>,
}

impl Default for DesignatorManager {
    fn default() -> Self {
        Self {
            selected: None,
            style: None,
            rot: Rot4::North,
            stuff: HashMap::new(),
            write_stuff: HashSet::new(),
            remembered: HashMap::new(),
            drag_start: None,
        }
    }
}

impl DesignatorManager {
    /// `DesignatorManager.Select`.
    pub fn select(&mut self, des: Des, sim: &Sim, lang: &Lang) {
        self.deselect();
        let cat = des.info(sim, lang, self).style_category;
        self.style = cat.and_then(|c| {
            let styles = style_category(c);
            self.remembered
                .get(c)
                .copied()
                .filter(|s| styles.contains(s))
                .or(styles.first().copied())
        });
        if let Des::Build(def) = des {
            // `Designator_Place.Selected`: the def's default rotation.
            self.rot = rotation_from(sim.defs.things[def].default_placing_rot.as_deref());
        }
        self.selected = Some(des);
    }

    pub fn deselect(&mut self) {
        self.selected = None;
        self.drag_start = None;
    }

    /// `DesignatorManager.ChangeDrawStyle`.
    pub fn change_style(&mut self, sim: &Sim, lang: &Lang, delta: i32) {
        let (Some(des), Some(style)) = (self.selected, self.style) else {
            return;
        };
        let Some(cat) = des.info(sim, lang, self).style_category else {
            return;
        };
        let styles = style_category(cat);
        let i = styles.iter().position(|&s| s == style).unwrap_or(0) as i32;
        let n = styles.len() as i32;
        let next = styles[(i + delta).rem_euclid(n) as usize];
        self.style = Some(next);
        self.remembered.insert(cat, next);
    }

    /// `DesignatorManager.SelectedStyle` set from the style menu.
    pub fn set_style(&mut self, style: Style, sim: &Sim, lang: &Lang) {
        let Some(des) = self.selected else { return };
        let Some(cat) = des.info(sim, lang, self).style_category else {
            return;
        };
        if style_category(cat).contains(&style) {
            self.style = Some(style);
            self.remembered.insert(cat, style);
        }
    }

    /// `Designator_Build.StuffDef`: the chosen stuff, else the default
    /// (`GenStuff.DefaultStuffFor`), else the first stock that can make it
    /// in the cost's amount.
    pub fn stuff_for(&self, sim: &Sim, def: DefId<ThingDef>) -> Option<DefId<ThingDef>> {
        if let Some(&s) = self.stuff.get(&def) {
            return Some(s);
        }
        let d = &sim.defs.things[def];
        if !d.made_from_stuff() {
            return None;
        }
        let default = default_stuff_for(&sim.defs, def)?;
        let need = d.cost_stuff_count;
        if stored_count(sim, default) < need {
            let alt = stuff_options(sim, def).into_iter().find(|&s| {
                sim.defs.things[s]
                    .stuff_props
                    .as_ref()
                    .is_some_and(|p| p.can_suggest_use_default_stuff)
                    && stored_count(sim, s) >= need
            });
            if let Some(a) = alt {
                return Some(a);
            }
        }
        Some(default)
    }

    /// Choosing a stuff in the material menu.
    pub fn set_stuff(&mut self, def: DefId<ThingDef>, stuff: DefId<ThingDef>) {
        self.stuff.insert(def, stuff);
        self.write_stuff.insert(def);
    }

    fn rotate(&mut self, clockwise: bool) {
        use Rot4::*;
        self.rot = match (self.rot, clockwise) {
            (North, true) => East,
            (East, true) => South,
            (South, true) => West,
            (West, true) => North,
            (North, false) => West,
            (West, false) => South,
            (South, false) => East,
            (East, false) => North,
        };
    }
}

fn rotation_from(s: Option<&str>) -> Rot4 {
    match s.map(str::trim) {
        Some("East") => Rot4::East,
        Some("South") => Rot4::South,
        Some("West") => Rot4::West,
        _ => Rot4::North,
    }
}

/// `GenStuff.DefaultStuffFor`: the Def's `defaultStuff`, else wood, steel,
/// plasteel, granite blocks, cloth, leather — the first it accepts.
pub fn default_stuff_for(defs: &GameDefs, def: DefId<ThingDef>) -> Option<DefId<ThingDef>> {
    let d = &defs.things[def];
    let accepts = |s: DefId<ThingDef>| d.accepts_stuff(&defs.things[s]);
    if let Some(s) = d.default_stuff.as_deref().and_then(|n| defs.things.id(n))
        && accepts(s)
    {
        return Some(s);
    }
    [
        "WoodLog",
        "Steel",
        "Plasteel",
        "BlocksGranite",
        "Cloth",
        "Leather_Plain",
    ]
    .iter()
    .filter_map(|n| defs.things.id(n))
    .find(|&s| accepts(s))
}

/// `ResourceCounter.GetCount`: what the colony has stored (in stockpiles).
// COMPATIBILITY TODO: currently approximate — the resource counter counts
// things in storage; this counts unforbidden things in stockpiles.
pub fn stored_count(sim: &Sim, def: DefId<ThingDef>) -> u32 {
    sim.map
        .items()
        .iter()
        .filter(|i| i.def == def && !i.forbidden && sim.map.storage.zone_at(i.position).is_some())
        .map(|i| i.stack_count)
        .sum()
}

/// `Designator_Build.ProcessInput`'s menu: stuffs that can make the
/// building and exist on the map, by commonality (descending) then market
/// value.
pub fn stuff_options(sim: &Sim, def: DefId<ThingDef>) -> Vec<DefId<ThingDef>> {
    let d = &sim.defs.things[def];
    let mut v: Vec<DefId<ThingDef>> = sim
        .defs
        .things
        .iter()
        .filter(|(_, s)| s.stuff_props.is_some() && s.count_as_resource && d.accepts_stuff(s))
        .map(|(id, _)| id)
        .filter(|&s| sim.map.items().iter().any(|i| i.def == s))
        .collect();
    let key = |s: DefId<ThingDef>| {
        let t = &sim.defs.things[s];
        (
            t.stuff_props
                .as_ref()
                .map_or(f32::INFINITY, |p| p.commonality),
            t.stat_bases.get("MarketValue").copied().unwrap_or(0.0),
        )
    };
    v.sort_by(|&a, &b| {
        let (ca, ma) = key(a);
        let (cb, mb) = key(b);
        cb.total_cmp(&ca).then(ma.total_cmp(&mb))
    });
    v
}

/// The cells a drag covers, accepted by the designator
/// (`DesignationDragger.DragCells`), and the last refusal's reason.
fn drag_cells(
    sim: &Sim,
    mgr: &DesignatorManager,
    des: Des,
    style: Style,
    a: Cell,
    b: Cell,
) -> (Vec<Cell>, Option<&'static str>) {
    let mut ok = Vec::new();
    let mut why = None;
    for c in style.cells(a, b) {
        match can_designate(sim, mgr, des, c) {
            Ok(()) => ok.push(c),
            Err(Some(r)) => why = Some(r),
            Err(None) => {}
        }
    }
    (ok, why)
}

/// `CanDesignateCell` of any designator.
pub fn can_designate(
    sim: &Sim,
    mgr: &DesignatorManager,
    des: Des,
    c: Cell,
) -> Result<(), Option<&'static str>> {
    match des {
        Des::Order(d) => sim.can_designate_cell(d, c),
        Des::Zone(k) => sim.can_zone_cell(k, c).map_err(|e| match e {
            rimworld_sim::sim::ZoneReject::TooCloseToMapEdge => Some("TooCloseToMapEdge"),
            rimworld_sim::sim::ZoneReject::Silent => None,
        }),
        Des::ZoneDelete => {
            if sim.can_delete_zone_cell(c) {
                Ok(())
            } else {
                Err(None)
            }
        }
        Des::Build(def) => sim
            .can_place_blueprint(def, mgr.stuff_for(sim, def), c, mgr.rot)
            .map_err(Some),
        Des::Floor(t) => sim.can_place_floor(t, c).map_err(Some),
    }
}

/// `DesignateMultiCell` (or `DesignateSingleCell`) on accepted cells.
fn designate(
    sim: &mut Sim,
    mgr: &DesignatorManager,
    sel: &mut Selection,
    des: Des,
    cells: &[Cell],
    now: f32,
) -> bool {
    if cells.is_empty() {
        return false;
    }
    match des {
        Des::Order(d) => sim.designate_cells(d, cells) > 0,
        Des::Zone(k) => {
            let zone = sim.zone_add(k, sel.selected_zone(), cells);
            // The zone the drag left selected (`SelectedZone = ...`).
            if let Some(z) = zone
                && sel.selected_zone() != Some(z)
            {
                sel.clear();
                sel.select(Selectable::Zone(z), now);
            }
            zone.is_some()
        }
        Des::ZoneDelete => {
            sim.zone_delete_cells(cells);
            true
        }
        Des::Build(def) => {
            let stuff = mgr.stuff_for(sim, def);
            let mut any = false;
            for &c in cells {
                if sim.can_place_blueprint(def, stuff, c, mgr.rot).is_ok()
                    && sim
                        .place_blueprint_rotated(def, stuff, c, mgr.rot)
                        .is_some()
                {
                    any = true;
                }
            }
            any
        }
        Des::Floor(t) => {
            let mut any = false;
            for &c in cells {
                if sim.can_place_floor(t, c).is_ok() && sim.place_floor_blueprint(t, c).is_some() {
                    any = true;
                }
            }
            any
        }
    }
}

/// Designator map input runs after the UI took its clicks.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DesignatorSet;

pub fn build(app: &mut App) {
    app.configure_sets(Update, DesignatorSet.after(super::UiSet::End))
        .add_systems(
            Update,
            (designator_input, designator_preview)
                .chain()
                .in_set(DesignatorSet),
        );
}

/// `DesignatorManager.ProcessInputEvents` and the rotation keys.
#[allow(clippy::too_many_arguments)]
fn designator_input(
    mut clicks: ResMut<PointerClicks>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    camera: Single<(&Camera, &GlobalTransform)>,
    mut sim: ResMut<SimState>,
    mut mgr: ResMut<DesignatorManager>,
    mut sel: ResMut<Selection>,
    lang: Res<Lang>,
    mut messages: ResMut<Messages>,
    time: Res<Time>,
    mut menu: ResMut<super::float_menu::FloatMenuState>,
) {
    let Some(des) = mgr.selected else {
        return;
    };
    let now = time.elapsed_secs();
    let (camera, gt) = *camera;
    let to_cell = |p: Vec2| camera.viewport_to_world_2d(gt, p).ok().map(world_to_cell);
    // Right click or Escape deselects (`SoundDefOf.CancelMode`).
    if clicks.secondary.is_some() || keys.just_pressed(KeyCode::Escape) {
        if keys.just_pressed(KeyCode::Escape) {
            keys.clear_just_pressed(KeyCode::Escape);
        }
        clicks.secondary = None;
        mgr.deselect();
        return;
    }
    let rotatable = matches!(des, Des::Build(d) if sim.0.defs.things[d].rotatable);
    // Q/E: rotate a rotatable building, else change the draw style.
    let q = keys.just_pressed(KeyCode::KeyQ);
    let e = keys.just_pressed(KeyCode::KeyE);
    if q || e {
        keys.clear_just_pressed(KeyCode::KeyQ);
        keys.clear_just_pressed(KeyCode::KeyE);
        if rotatable {
            mgr.rotate(e);
        } else {
            mgr.change_style(&sim.0, &lang, if e { 1 } else { -1 });
        }
    }
    let Some(style) = mgr.style else {
        // No draw style: a click places on the cell at once.
        if let Some(c) = clicks.primary.and_then(to_cell) {
            clicks.primary = None;
            match can_designate(&sim.0, &mgr, des, c) {
                Ok(()) => {
                    designate(&mut sim.0, &mgr, &mut sel, des, &[c], now);
                }
                Err(Some(r)) => messages.add(lang.tr(r), now),
                Err(None) => {}
            }
        }
        clicks.primary_drag = None;
        clicks.primary_drag_end = None;
        let _ = &mut menu;
        return;
    };
    // A drag (or a click, a one-cell drag).
    mgr.drag_start = clicks.primary_drag.and_then(|(a, _)| to_cell(a));
    let finished = clicks
        .primary_drag_end
        .and_then(|(a, b)| Some((to_cell(a)?, to_cell(b)?)))
        .or_else(|| clicks.primary.and_then(to_cell).map(|c| (c, c)));
    clicks.primary = None;
    clicks.primary_drag = None;
    clicks.primary_drag_end = None;
    if let Some((a, b)) = finished {
        let (cells, why) = drag_cells(&sim.0, &mgr, des, style, a, b);
        let ok = designate(&mut sim.0, &mgr, &mut sel, des, &cells, now);
        if !ok && let Some(r) = why {
            messages.add(lang.tr(r), now);
        }
    }
}

/// The selected designator's map preview: drag highlight and outline,
/// the placing ghost, the mouse-over bracket.
#[allow(clippy::too_many_arguments)]
fn designator_preview(
    sim: Res<SimState>,
    mgr: Res<DesignatorManager>,
    input: Res<UiInput>,
    camera: Single<(&Camera, &GlobalTransform)>,
    textures: Res<GameTextures>,
    mut tt: ResMut<ThingTextures>,
    mut images: ResMut<Assets<Image>>,
    mut frame: ResMut<WorldFrame>,
    sel: Res<Selection>,
) {
    let Some(des) = mgr.selected else {
        return;
    };
    let sim = &sim.0;
    let (camera, gt) = *camera;
    // Zone designators show the selected zone's edges.
    if matches!(des, Des::Zone(_) | Des::ZoneDelete)
        && let Some(z) = sel.selected_zone()
    {
        frame.field_edges(sim.zone_cells(z), Color::srgba(1.0, 1.0, 1.0, 0.9), 88.0);
    }
    if input.cursor_over_ui() {
        return;
    }
    let Some(mouse) = input
        .cursor
        .and_then(|p| camera.viewport_to_world_2d(gt, p).ok())
        .map(world_to_cell)
        .filter(|&c| sim.map.size().contains(c))
    else {
        return;
    };
    // The drag: highlighted accepted cells and the outline.
    if let (Some(style), Some(start)) = (mgr.style, mgr.drag_start) {
        let (cells, _) = drag_cells(sim, &mgr, des, style, start, mouse);
        for c in &cells {
            frame.quad(
                cell_center(*c),
                Vec2::splat(CELL_SIZE * 0.92),
                87.0,
                Color::srgba(0.6, 0.85, 0.5, 0.33),
            );
        }
        if style.outline() {
            let (x0, x1) = (start.x.min(mouse.x), start.x.max(mouse.x));
            let (z0, z1) = (start.z.min(mouse.z), start.z.max(mouse.z));
            let min = Vec2::new(x0 as f32, z0 as f32) * CELL_SIZE;
            let max = Vec2::new((x1 + 1) as f32, (z1 + 1) as f32) * CELL_SIZE;
            let c = (min + max) * 0.5;
            let s = max - min;
            // `DesignationDragger.OutlineTex` (109, 139, 79, 100).
            let col = Color::srgba_u8(109, 139, 79, 200);
            frame.quad(Vec2::new(c.x, max.y), Vec2::new(s.x, 1.5), 88.0, col);
            frame.quad(Vec2::new(c.x, min.y), Vec2::new(s.x, 1.5), 88.0, col);
            frame.quad(Vec2::new(min.x, c.y), Vec2::new(1.5, s.y), 88.0, col);
            frame.quad(Vec2::new(max.x, c.y), Vec2::new(1.5, s.y), 88.0, col);
        }
    }
    match des {
        Des::Build(def) => {
            // `Designator_Place.DrawGhost`: green where it can go, red
            // where it can't.
            let accepted = can_designate(sim, &mgr, des, mouse).is_ok();
            let ghost = if accepted {
                Color::srgba(0.5, 1.0, 0.6, 0.4)
            } else {
                Color::srgba(1.0, 0.0, 0.0, 0.4)
            };
            let d = &sim.defs.things[def];
            let stuff = mgr.stuff_for(sim, def).map(|s| &sim.defs.things[s]);
            let fp = rimworld_sim::geom::Footprint {
                center: mouse,
                rot: mgr.rot,
                size: d.size,
            };
            let r = fp.rect();
            let center = Vec2::new(
                (r.min_x + r.max_x + 1) as f32 * 0.5,
                (r.min_z + r.max_z + 1) as f32 * 0.5,
            ) * CELL_SIZE;
            let textured = textures.0.as_ref().and_then(|lib| {
                let lk = look(&mut tt, lib, d, stuff, mgr.rot, 0)?;
                let color = draw_color(d, stuff);
                let tex = tt.get(lib, &lk.path, lk.masked.then_some(color), &mut images)?;
                Some((lk, tex))
            });
            match textured {
                Some((lk, tex)) if !lk.linked => frame.image(
                    center,
                    lk.size * CELL_SIZE,
                    -lk.angle,
                    86.0,
                    tex.image,
                    ghost,
                    None,
                    lk.flip_x,
                ),
                Some((_, tex)) => frame.image(
                    center,
                    Vec2::splat(CELL_SIZE),
                    0.0,
                    86.0,
                    tex.image,
                    ghost,
                    Some(crate::thing_graphics::atlas_rect(tex.size, 0)),
                    false,
                ),
                None => {
                    for c in fp.cells() {
                        frame.quad(cell_center(c), Vec2::splat(CELL_SIZE * 0.9), 86.0, ghost);
                    }
                }
            }
            // The interaction cell and size outline are not drawn.
        }
        _ => {
            // `GenUI.RenderMouseoverBracket`.
            let p = cell_center(mouse);
            let col = Color::srgba(1.0, 1.0, 1.0, 0.5);
            let h = CELL_SIZE * 0.5;
            let t = 1.5;
            frame.quad(p + Vec2::new(0.0, h), Vec2::new(CELL_SIZE, t), 88.0, col);
            frame.quad(p - Vec2::new(0.0, h), Vec2::new(CELL_SIZE, t), 88.0, col);
            frame.quad(p + Vec2::new(h, 0.0), Vec2::new(t, CELL_SIZE), 88.0, col);
            frame.quad(p - Vec2::new(h, 0.0), Vec2::new(t, CELL_SIZE), 88.0, col);
        }
    }
}

/// The mouse attachment (`DrawMouseAttachments`): the designator's icon
/// and text next to the cursor; for buildings the cost of the drag
/// (`DrawPlaceMouseAttachments`), red when not enough is stored; drag
/// sizes on the map.
#[allow(clippy::too_many_arguments)]
pub fn designator_mouse_ui(
    mut paint: Paint,
    input: Res<UiInput>,
    sim: Res<SimState>,
    mgr: Res<DesignatorManager>,
    lang: Res<Lang>,
    sel: Res<Selection>,
    camera: Single<(&Camera, &GlobalTransform)>,
) {
    let Some(des) = mgr.selected else {
        return;
    };
    let Some(cursor) = input.cursor else {
        return;
    };
    if input.cursor_over_ui() {
        return;
    }
    let sim = &sim.0;
    let (camera, gt) = *camera;
    let mut p = paint.painter(layer::MOUSE_ATTACHMENT);
    let mouse = camera
        .viewport_to_world_2d(gt, cursor)
        .ok()
        .map(world_to_cell);
    let info = des.info(sim, &lang, &mgr);
    let x = cursor.x + 19.0;
    let mut y = cursor.y + 17.0;
    match des {
        Des::Build(def) => {
            let n = match (mgr.style, mgr.drag_start, mouse) {
                (Some(style), Some(a), Some(b)) => {
                    drag_cells(sim, &mgr, des, style, a, b).0.len().max(1)
                }
                _ => 1,
            } as u32;
            let stuff = mgr.stuff_for(sim, def);
            for (d, count) in
                rimworld_sim::stats::cost_list(&sim.defs, &sim.defs.things[def], stuff)
            {
                let total = count * n;
                let short = stored_count(sim, d) < total;
                p.thing_icon_fitted(Rect::new(x, y, x + 27.0, y + 27.0), sim, d, Color::WHITE);
                let mut text = total.to_string();
                if short {
                    text += &format!(" ({})", lang.tr("NotEnoughStoredLower"));
                }
                let col = if short {
                    Color::srgb(1.0, 0.0, 0.0)
                } else {
                    Color::WHITE
                };
                p.label_mid(
                    Rect::new(x + 29.0, y, x + 400.0, y + 29.0),
                    text,
                    FONT_SMALL,
                    col,
                    Align::Left,
                );
                y += 29.0;
            }
        }
        Des::Floor(t) => {
            for (name, count) in &sim.defs.terrain[t].cost_list {
                p.label_mid(
                    Rect::new(x + 29.0, y, x + 400.0, y + 29.0),
                    format!(
                        "{count} {}",
                        sim.defs
                            .things
                            .get(name)
                            .map_or(name.as_str(), |d| d.label.as_str())
                    ),
                    FONT_SMALL,
                    Color::WHITE,
                    Align::Left,
                );
                y += 29.0;
            }
        }
        Des::Zone(_) => {
            // `Designator_ZoneAdd.DrawMouseAttachments`.
            if let Some(icon) = info.icon {
                p.tex_fitted(
                    Rect::new(
                        cursor.x + 12.0,
                        cursor.y + 8.0,
                        cursor.x + 44.0,
                        cursor.y + 40.0,
                    ),
                    icon,
                    1.0,
                    Color::WHITE,
                );
            }
            if input.drag.is_none() {
                let new_label = info.label.clone();
                let text = match sel.selected_zone() {
                    Some(z) => lang.tr_args(
                        "ExpandOrCreateZone",
                        &[&super::lang::zone_label(sim, &lang, z), &new_label],
                    ),
                    None => lang.tr_args("CreateNewZone", &[&new_label]),
                };
                p.label_mid(
                    Rect::new(
                        cursor.x + 48.0,
                        cursor.y + 10.0,
                        cursor.x + 600.0,
                        cursor.y + 36.0,
                    ),
                    text,
                    FONT_SMALL,
                    Color::WHITE,
                    Align::Left,
                );
            }
        }
        _ => {
            if let Some(icon) = info.icon {
                p.tex_fitted(
                    Rect::new(
                        cursor.x + 12.0,
                        cursor.y + 8.0,
                        cursor.x + 44.0,
                        cursor.y + 40.0,
                    ),
                    icon,
                    1.0,
                    Color::WHITE,
                );
            }
        }
    }
    // Drag sizes (`DesignationDragger.DraggerOnGUI`, `DragDrawMeasurements`).
    if let (Some(style), Some(a), Some(b)) = (mgr.style, mgr.drag_start, mouse) {
        let w = (a.x - b.x).abs() + 1;
        let h = (a.z - b.z).abs() + 1;
        let measured = matches!(
            des,
            Des::Build(_)
                | Des::Floor(_)
                | Des::Zone(_)
                | Des::ZoneDelete
                | Des::Order(OrderDesignator::Mine)
        );
        let to_screen = |c: Vec2| camera.world_to_viewport(gt, c.extend(0.0)).ok();
        if measured && (w >= 5 || h >= 5) {
            let long_x = w >= h;
            let short_side = style.outline();
            if w >= 5
                && (short_side || long_x)
                && let Some(s) = to_screen(
                    (cell_center(a) + cell_center(b)) * 0.5 * Vec2::X + Vec2::Y * cell_center(a).y,
                )
            {
                number_on_map(&mut p, s, w);
            }
            if h >= 5
                && (short_side || !long_x)
                && let Some(s) = to_screen(Vec2::new(
                    cell_center(a).x,
                    (cell_center(a).y + cell_center(b).y) * 0.5,
                ))
            {
                number_on_map(&mut p, s, h);
            }
        }
        if style.outline() && w >= 3 && h >= 3 {
            let n = drag_cells(sim, &mgr, des, style, a, b).0.len();
            if n > 0
                && let Some(s) = to_screen((cell_center(a) + cell_center(b)) * 0.5)
            {
                number_on_map(&mut p, s, n as i32);
            }
        }
    }
}

/// `Widgets.DrawNumberOnMap`.
fn number_on_map(p: &mut super::Painter, at: Vec2, n: i32) {
    let text = n.to_string();
    let w = super::text_width(&text, FONT_SMALL) + 8.0;
    let r = Rect::from_center_size(at, Vec2::new(w, 22.0));
    p.rect(r, Color::srgba(0.0, 0.0, 0.0, 0.6));
    p.label_mid(r, text, FONT_SMALL, Color::WHITE, Align::Center);
}

/// The label for the designator's material menu option ("Granite wall").
pub fn stuff_option_label(
    sim: &Sim,
    lang: &Lang,
    def: DefId<ThingDef>,
    stuff: DefId<ThingDef>,
) -> String {
    cap(&made_of(sim, lang, def, Some(stuff)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(x: i32, z: i32) -> Cell {
        Cell::new(x, z)
    }

    #[test]
    fn line_follows_the_longer_axis_from_the_start() {
        assert_eq!(
            Style::Line.cells(c(0, 0), c(3, 1)),
            vec![c(0, 0), c(1, 0), c(2, 0), c(3, 0)]
        );
        // `DrawStyle_Line` keeps the start's row (z read before the swap).
        assert_eq!(
            Style::Line.cells(c(3, 5), c(0, 6)),
            vec![c(0, 5), c(1, 5), c(2, 5), c(3, 5)]
        );
        assert_eq!(
            Style::Line.cells(c(2, 4), c(3, 1)),
            vec![c(2, 1), c(2, 2), c(2, 3), c(2, 4)]
        );
    }

    #[test]
    fn rectangles_list_cells_in_the_games_order() {
        // `CellRect.Cells`: rows bottom to top.
        assert_eq!(
            Style::FilledRectangle.cells(c(1, 1), c(0, 0)),
            vec![c(0, 0), c(1, 0), c(0, 1), c(1, 1)]
        );
        // `CellRect.EdgeCells`: bottom, right side up, top leftwards, left
        // side down.
        assert_eq!(
            Style::EmptyRectangle.cells(c(0, 0), c(2, 2)),
            vec![
                c(0, 0),
                c(1, 0),
                c(2, 0),
                c(2, 1),
                c(2, 2),
                c(1, 2),
                c(0, 2),
                c(0, 1)
            ]
        );
        assert_eq!(Style::EmptyRectangle.cells(c(0, 0), c(0, 0)), vec![c(0, 0)]);
    }

    #[test]
    fn angled_line_starts_from_its_middle() {
        // Padded Bresenham (0,0)→(2,1): (0,0) (1,0) (1,1) (2,1); middle out.
        assert_eq!(
            Style::AngledLine.cells(c(0, 0), c(2, 1)),
            vec![c(1, 0), c(0, 0), c(1, 1), c(2, 1)]
        );
    }

    #[test]
    fn ovals() {
        // `DrawStyle_FilledOval`: within radius + 0.4 of the centre cell. A
        // 5×5 drag keeps its corners (2² + 2² ≤ 2.9²); a 7×7 drops them.
        assert_eq!(Style::FilledOval.cells(c(0, 0), c(4, 4)).len(), 25);
        let filled = Style::FilledOval.cells(c(0, 0), c(6, 6));
        assert_eq!(filled.len(), 45);
        assert!(!filled.contains(&c(0, 0)));
        let empty = Style::EmptyOval.cells(c(0, 0), c(6, 6));
        assert!(!empty.contains(&c(3, 3)), "the middle is open");
        assert!(empty.contains(&c(0, 3)));
    }
}
