//! The main buttons and the Architect (`MainButtonsRoot`,
//! `MainTabWindow_Architect`, `ArchitectCategoryTab`,
//! `DesignationCategoryDef`; docs/research.md §66).
//!
//! The bottom bar holds the game's main buttons in `order` (the last three
//! minimized to half width). The Architect window (200 pixels wide, two
//! columns of 32-pixel category buttons, `DesignationCategoryDef`s by
//! descending `order`) opens a category's designators as a gizmo grid at
//! x = 210: its special designators first (the Def's
//! `specialDesignatorClasses`, in order), then a build designator for each
//! buildable of the category, by `uiOrder`. Above the window, an info box
//! describes the hovered (or selected) designator.

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::GameDefs;
use rimworld_sim::Sim;
use rimworld_sim::sim::{OrderDesignator, ZoneKind};

use super::designator::{Des, DesignatorManager, stored_count};
use super::gizmo::{Command, GizmoAction, Icon, draw_grid, select_designator};
use super::inspect::window_background;
use super::lang::cap;
use super::select::Selection;
use super::tabs::{MainTab, MainTabs};
use super::{
    Align, FONT_SMALL, FONT_TINY, Lang, Messages, Paint, Painter, UiHits, UiInput, layer,
    text_height,
};
use crate::SimState;
use crate::camera::PointerClicks;

/// The buildings the simulation runs (construction, use and function).
pub const SUPPORTED_BUILDINGS: [&str; 22] = [
    "Wall",
    "Door",
    "Bed",
    "Table1x2c",
    "DiningChair",
    "TorchLamp",
    "Campfire",
    "ButcherSpot",
    "TableButcher",
    "PowerConduit",
    "WoodFiredGenerator",
    "SolarGenerator",
    "Battery",
    "StandingLamp",
    "Heater",
    "Cooler",
    "PowerSwitch",
    "Autodoor",
    "SimpleResearchBench",
    "HiTechResearchBench",
    "FueledStove",
    "ElectricStove",
];

/// The Architect's open category.
#[derive(Resource, Default)]
pub struct ArchitectState {
    pub category: Option<String>,
}

/// A `DesignationCategoryDef`.
#[derive(Debug, Clone)]
pub struct Category {
    pub def_name: String,
    pub label: String,
    pub order: i32,
    pub research: Vec<String>,
    pub specials: Vec<String>,
}

/// The categories by descending `order`.
pub fn categories(defs: &GameDefs) -> Vec<Category> {
    let Some(table) = defs.raw.table("DesignationCategoryDef") else {
        return Vec::new();
    };
    let mut v: Vec<Category> = table
        .iter()
        .map(|d| Category {
            def_name: d.def_name.clone(),
            label: d
                .node
                .child_text("label")
                .unwrap_or(&d.def_name)
                .trim()
                .to_owned(),
            order: d
                .node
                .child_text("order")
                .and_then(|o| o.trim().parse().ok())
                .unwrap_or(0),
            research: d.node.child_list_texts("researchPrerequisites"),
            specials: d.node.child_list_texts("specialDesignatorClasses"),
        })
        .collect();
    v.sort_by_key(|c| std::cmp::Reverse(c.order));
    v
}

/// The special designator classes the simulation supports.
fn special(class: &str) -> Option<Des> {
    Some(match class.trim() {
        "Designator_Cancel" => Des::Order(OrderDesignator::Cancel),
        "Designator_Deconstruct" => Des::Order(OrderDesignator::Deconstruct),
        "Designator_Mine" => Des::Order(OrderDesignator::Mine),
        "Designator_PlantsHarvestWood" => Des::Order(OrderDesignator::HarvestWood),
        "Designator_PlantsCut" => Des::Order(OrderDesignator::CutPlants),
        "Designator_PlantsHarvest" => Des::Order(OrderDesignator::Harvest),
        "Designator_Hunt" => Des::Order(OrderDesignator::Hunt),
        "Designator_SmoothSurface" => Des::Order(OrderDesignator::SmoothSurface),
        "Designator_RemoveFloor" => Des::Order(OrderDesignator::RemoveFloor),
        "Designator_ZoneAddStockpile_Resources" => Des::Zone(ZoneKind::Stockpile),
        "Designator_ZoneAddStockpile_Dumping" => Des::Zone(ZoneKind::DumpingStockpile),
        "Designator_ZoneAdd_Growing" => Des::Zone(ZoneKind::Growing),
        "Designator_ZoneDelete" => Des::ZoneDelete,
        _ => return None,
    })
}

/// `ResolvedAllowedDesignators` (the supported ones, visible now), sorted
/// by gizmo order.
// COMPATIBILITY TODO: currently approximate — unsupported designators
// (Slaughter, Tame, Haul, Forbid, Claim, paint, plans, areas, smoothing
// walls or floors only, ...) and unsupported buildables are left out;
// dropdown groups (carpets, ...) show their first member only; the quick
// search box is not drawn.
pub fn designators(sim: &Sim, lang: &Lang, mgr: &DesignatorManager, cat: &Category) -> Vec<Des> {
    let defs = &sim.defs;
    let mut list: Vec<Des> = cat.specials.iter().filter_map(|c| special(c)).collect();
    let mut dropdowns: Vec<String> = Vec::new();
    // Floors only (bridges and foundations are not supported).
    for (id, t) in defs.terrain.iter() {
        if t.designation_category.as_deref().map(str::trim) != Some(cat.def_name.as_str())
            || cat.def_name != "Floors"
            || t.cost_list.is_empty()
        {
            continue;
        }
        if !t
            .research_prerequisites
            .iter()
            .all(|r| sim.research_finished(r))
        {
            continue;
        }
        if let Some(g) = &t.designator_dropdown {
            if dropdowns.contains(g) {
                continue;
            }
            dropdowns.push(g.clone());
        }
        list.push(Des::Floor(id));
    }
    for (id, d) in defs.things.iter() {
        if d.designation_category.as_deref().map(str::trim) != Some(cat.def_name.as_str())
            || !SUPPORTED_BUILDINGS.contains(&d.def_name.as_str())
            || !sim.research_unlocked(id)
        {
            continue;
        }
        list.push(Des::Build(id));
    }
    let order = |d: &Des| d.info(sim, lang, mgr).order;
    list.sort_by(|a, b| order(a).total_cmp(&order(b)));
    list
}

/// `DesignationCategoryDef.Visible`.
fn visible(sim: &Sim, cat: &Category) -> bool {
    cat.research.iter().all(|r| sim.research_finished(r))
}

/// A `MainButtonDef`.
struct MainButton {
    def_name: String,
    label: String,
    description: String,
    minimized: bool,
    hotkey: Option<KeyCode>,
}

/// `MainButtonDef.defaultHotKey` (Tab and the function keys).
fn hotkey(name: &str) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match name.trim() {
        "Tab" => Tab,
        "F1" => F1,
        "F2" => F2,
        "F3" => F3,
        "F4" => F4,
        "F5" => F5,
        "F6" => F6,
        "F7" => F7,
        "F8" => F8,
        "F9" => F9,
        "F10" => F10,
        "F11" => F11,
        "F12" => F12,
        _ => return None,
    })
}

/// The visible `MainButtonDef`s by order.
fn main_buttons(defs: &GameDefs) -> Vec<MainButton> {
    let Some(table) = defs.raw.table("MainButtonDef") else {
        return Vec::new();
    };
    let text = |d: &rimworld_defs::Def, k: &str| d.node.child_text(k).map(|s| s.trim().to_owned());
    let mut v: Vec<(i32, MainButton)> = table
        .iter()
        .filter(|d| d.node.child_text("buttonVisible").map(str::trim) != Some("false"))
        .map(|d| {
            (
                text(d, "order").and_then(|o| o.parse().ok()).unwrap_or(0),
                MainButton {
                    def_name: d.def_name.clone(),
                    label: text(d, "label").unwrap_or_else(|| d.def_name.clone()),
                    description: text(d, "description").unwrap_or_default(),
                    minimized: text(d, "minimized").as_deref() == Some("true"),
                    hotkey: text(d, "defaultHotKey").as_deref().and_then(hotkey),
                },
            )
        })
        .collect();
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|(_, b)| b).collect()
}

/// `Widgets.ButtonTextSubtle`; `left_margin`: the label sits that far in
/// (else centred).
fn button_subtle(
    p: &mut Painter,
    input: &mut UiInput,
    r: Rect,
    label: &str,
    enabled: bool,
    left_margin: Option<f32>,
) -> bool {
    let over = input.hovered(r);
    let col = if !enabled {
        Color::srgba(1.0, 1.0, 1.0, 0.3)
    } else if over {
        Color::srgb(1.0, 1.0, 1.0)
    } else {
        Color::srgb(0.75, 0.75, 0.75)
    };
    p.atlas(r, "UI/Widgets/ButtonSubtleAtlas", col);
    let text_col = if enabled {
        Color::WHITE
    } else {
        Color::srgba(1.0, 1.0, 1.0, 0.4)
    };
    match left_margin {
        Some(m) => p.label_mid(
            Rect::new(r.min.x + m, r.min.y, r.max.x, r.max.y),
            cap(label),
            FONT_SMALL,
            text_col,
            Align::Left,
        ),
        None => p.label_mid(r, cap(label), FONT_SMALL, text_col, Align::Center),
    }
    input.clicked(r)
}

/// The main button bar along the bottom; a button (or its hotkey) toggles
/// its tab.
#[allow(clippy::too_many_arguments)]
pub fn main_buttons_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    mut tabs: ResMut<MainTabs>,
    sim: Res<SimState>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut messages: ResMut<Messages>,
    mut mgr: ResMut<DesignatorManager>,
    window: Single<&Window>,
    mut hits: ResMut<UiHits>,
    time: Res<Time>,
) {
    let screen = super::screen(&window);
    let buttons = main_buttons(&sim.0.defs);
    let units: f32 = buttons
        .iter()
        .map(|b| if b.minimized { 0.5 } else { 1.0 })
        .sum();
    let full = (screen.x / units.max(1.0)).floor();
    let mut x = 0.0;
    let mut p = paint.painter(layer::MAIN_BUTTONS);
    let mut tip: Option<String> = None;
    // Ctrl + a function key is a debug key here.
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let mut activate: Option<usize> = None;
    for (k, b) in buttons.iter().enumerate() {
        let w = if k + 1 == buttons.len() {
            screen.x - x
        } else if b.minimized {
            (full / 2.0).floor()
        } else {
            full
        };
        let r = Rect::new(x, screen.y - 35.0, x + w, screen.y + 1.0);
        hits.name(format!("main:{}", b.def_name), r);
        let supported = MainTab::from_def_name(&b.def_name).is_some();
        // COMPATIBILITY TODO: currently approximate — only the Architect,
        // Work and Schedule open a window; the other main tabs are dimmed.
        let clicked = if b.minimized {
            let over = input.hovered(r);
            p.atlas(
                r,
                "UI/Widgets/ButtonSubtleAtlas",
                if over {
                    Color::WHITE
                } else {
                    Color::srgb(0.75, 0.75, 0.75)
                },
            );
            let icon = format!("UI/Buttons/MainButtons/{}", b.def_name);
            p.tex_fitted(r, &icon, 0.6, Color::srgba(1.0, 1.0, 1.0, 0.4));
            input.clicked(r)
        } else {
            button_subtle(&mut p, &mut input, r, &b.label, supported, None)
        };
        if input.hovered(r) && !b.description.is_empty() {
            tip = Some(format!("{}\n\n{}", cap(&b.label), b.description));
        }
        if clicked {
            activate = Some(k);
        }
        // `MainButtonDef.hotKey`: Tab (not Shift+Tab) and the F keys.
        if let Some(key) = b.hotkey
            && keys.just_pressed(key)
            && !ctrl
            && !(key == KeyCode::Tab && shift)
        {
            keys.clear_just_pressed(key);
            activate = Some(k);
        }
        x += w;
    }
    if let Some(k) = activate {
        let b = &buttons[k];
        match MainTab::from_def_name(&b.def_name) {
            Some(t) => tabs.toggle(t, &mut mgr),
            None => messages.add(
                format!("{}: not available yet", cap(&b.label)),
                time.elapsed_secs(),
            ),
        }
    }
    if let (Some(t), Some(cur)) = (tip, input.cursor) {
        p.tip(cur, &t, screen);
    }
}

/// The Architect window, its open category's designators and the info box.
#[allow(clippy::too_many_arguments)]
pub fn architect_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    mut state: ResMut<ArchitectState>,
    mut tabs: ResMut<MainTabs>,
    sim: Res<SimState>,
    mut mgr: ResMut<DesignatorManager>,
    mut menu: ResMut<super::float_menu::FloatMenuState>,
    mut messages: ResMut<Messages>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    lang: Res<Lang>,
    window: Single<&Window>,
    mut hits: ResMut<UiHits>,
    time: Res<Time>,
) {
    if !tabs.is_open(MainTab::Architect) {
        return;
    }
    let now = time.elapsed_secs();
    // Escape closes the window when nothing above it takes it.
    if keys.just_pressed(KeyCode::Escape) && mgr.selected.is_none() && menu.open.is_none() {
        keys.clear_just_pressed(KeyCode::Escape);
        tabs.set(None, &mut mgr);
        return;
    }
    let screen = super::screen(&window);
    let cats = categories(&sim.0.defs);
    let win_h = (cats.len() as f32 / 2.0).ceil() * 32.0 + 28.0;
    let win = Rect::new(0.0, screen.y - 35.0 - win_h, 200.0, screen.y - 35.0);
    hits.name("architect", win);
    input.absorb(win);
    let mut p = paint.painter(layer::ARCHITECT);
    window_background(&mut p, win);
    let bw = 100.0;
    for (i, cat) in cats.iter().enumerate() {
        let (col, row) = (i % 2, i / 2);
        let x = win.min.x + col as f32 * bw;
        let y = win.min.y + row as f32 * 32.0;
        let r = Rect::new(x, y, x + bw + if col == 0 { 1.0 } else { 0.0 }, y + 33.0);
        hits.name(format!("arch:{}", cap(&cat.label)), r);
        let vis = visible(&sim.0, cat);
        if button_subtle(&mut p, &mut input, r, &cat.label, vis, Some(8.0)) {
            if !vis {
                messages.add(
                    format!(
                        "{}: {}",
                        lang.tr("NothingAvailableInCategory"),
                        cap(&cat.label)
                    ),
                    now,
                );
            } else if state.category.as_deref() == Some(cat.def_name.as_str()) {
                state.category = None;
            } else {
                state.category = Some(cat.def_name.clone());
            }
        }
    }
    let Some(cat) = state
        .category
        .as_ref()
        .and_then(|c| cats.iter().find(|x| &x.def_name == c))
        .cloned()
    else {
        return;
    };
    let list = designators(&sim.0, &lang, &mgr, &cat);
    let commands: Vec<Command> = list
        .iter()
        .map(|&d| {
            let info = d.info(&sim.0, &lang, &mgr);
            let mut c = Command::new(info.label, GizmoAction::Select(d));
            c.desc = info.desc;
            c.icon = match d {
                Des::Build(def) => Some(Icon::Def(def, mgr.stuff_for(&sim.0, def))),
                Des::Floor(t) => Some(Icon::Terrain(t)),
                _ => info.icon.map(|s| Icon::Path(s.to_owned())),
            };
            c.hotkey = info.hotkey;
            c.order = info.order;
            c.extra_options =
                matches!(d, Des::Build(def) if sim.0.defs.things[def].made_from_stuff());
            c
        })
        .collect();
    let mut gp = paint.painter(layer::ARCHITECT);
    let result = draw_grid(
        &mut gp,
        &mut input,
        &mut hits,
        &mut keys,
        &mut messages,
        &lang,
        &sim.0,
        commands,
        210.0,
        screen,
        now,
    );
    let cursor = input.cursor.unwrap_or_default();
    if let Some(members) = &result.used
        && let Some(GizmoAction::Select(des)) = members.first().map(|c| c.action.clone())
        && let Some(msg) = select_designator(des, &sim.0, &mut mgr, &lang, &mut menu, cursor)
    {
        messages.add(msg, now);
    }
    // The info box (`ArchitectCategoryTab.DoInfoBox`), for the hovered
    // designator else the selected one.
    let shown: Option<Des> = result
        .mouseover
        .as_ref()
        .and_then(|c| match c.action {
            GizmoAction::Select(d) => Some(d),
            _ => None,
        })
        .or(mgr.selected);
    let info_rect = Rect::new(0.0, win.min.y - 270.0, 200.0, win.min.y);
    if let Some(des) = shown {
        input.absorb(info_rect);
        hits.name("architect:info", info_rect);
        let mut p = paint.painter(layer::ARCHITECT);
        window_background(&mut p, info_rect);
        info_box(&mut p, &sim.0, &lang, &mgr, des, info_rect);
    }
    // `DoExtraGuiControls` above the info box: rotation, draw style.
    if let Some(des) = mgr.selected {
        let mut p = paint.painter(layer::ARCHITECT);
        extra_controls(
            &mut p,
            &mut input,
            &mut hits,
            &sim.0,
            &mut mgr,
            &lang,
            des,
            info_rect.min.y,
            &mut menu,
            cursor,
        );
    }
}

/// `DoInfoBox`: the label, the cost readout, the description.
fn info_box(p: &mut Painter, sim: &Sim, lang: &Lang, mgr: &DesignatorManager, des: Des, r: Rect) {
    let inner = Rect::new(r.min.x + 7.0, r.min.y + 7.0, r.max.x - 7.0, r.max.y - 7.0);
    let info = des.info(sim, lang, mgr);
    let label = cap(&info.label);
    p.text(
        Rect::new(
            inner.min.x,
            inner.min.y,
            inner.max.x - 20.0,
            inner.min.y + 40.0,
        ),
        label.clone(),
        FONT_SMALL,
        Color::WHITE,
        Align::Left,
    );
    let mut y = inner.min.y + text_height(&label, FONT_SMALL, inner.width() - 20.0).max(24.0);
    // `Designator_Build.DrawPanelReadout`: the cost, largest first, red
    // when not enough is stored.
    match des {
        Des::Build(def) => {
            let stuff = mgr.stuff_for(sim, def);
            let mut cost = rimworld_sim::stats::cost_list(&sim.defs, &sim.defs.things[def], stuff);
            cost.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
            for (d, n) in cost {
                p.thing_icon_fitted(
                    Rect::new(inner.min.x, y, inner.min.x + 20.0, y + 20.0),
                    sim,
                    d,
                    Color::WHITE,
                );
                let short = sim.defs.things[d].count_as_resource && stored_count(sim, d) < n;
                let col = if short {
                    Color::srgb(1.0, 0.0, 0.0)
                } else {
                    Color::WHITE
                };
                p.label_mid(
                    Rect::new(inner.min.x + 26.0, y + 2.0, inner.min.x + 76.0, y + 20.0),
                    n.to_string(),
                    FONT_SMALL,
                    col,
                    Align::Left,
                );
                p.label_mid(
                    Rect::new(inner.min.x + 60.0, y + 2.0, inner.max.x, y + 20.0),
                    cap(&sim.defs.things[d].label),
                    FONT_SMALL,
                    Color::WHITE,
                    Align::Left,
                );
                y += 22.0;
            }
            if sim.defs.things[def].made_from_stuff() && stuff.is_none() {
                p.label_mid(
                    Rect::new(inner.min.x + 60.0, y, inner.max.x, y + 20.0),
                    format!("({})", lang.tr("UnchosenStuff")),
                    FONT_SMALL,
                    Color::WHITE,
                    Align::Left,
                );
                y += 22.0;
            }
            y += 4.0;
        }
        Des::Floor(t) => {
            for (name, n) in &sim.defs.terrain[t].cost_list {
                let label = sim
                    .defs
                    .things
                    .get(name)
                    .map_or(name.clone(), |d| cap(&d.label));
                p.label_mid(
                    Rect::new(inner.min.x + 26.0, y + 2.0, inner.max.x, y + 20.0),
                    format!("{n}   {label}"),
                    FONT_SMALL,
                    Color::WHITE,
                    Align::Left,
                );
                y += 22.0;
            }
            y += 4.0;
        }
        _ => {}
    }
    p.text(
        Rect::new(inner.min.x, y, inner.max.x, inner.max.y),
        info.desc,
        FONT_TINY,
        Color::WHITE,
        Align::Left,
    );
}

/// `Designator_Place.DoExtraGuiControls` (rotation buttons) or the draw
/// style button (`Designator.DoExtraGuiControls`), in a 200×90 strip above
/// the info box.
#[allow(clippy::too_many_arguments)]
fn extra_controls(
    p: &mut Painter,
    input: &mut UiInput,
    hits: &mut UiHits,
    sim: &Sim,
    mgr: &mut DesignatorManager,
    lang: &Lang,
    des: Des,
    bottom: f32,
    menu: &mut super::float_menu::FloatMenuState,
    cursor: Vec2,
) {
    let strip = Rect::new(0.0, bottom - 90.0, 200.0, bottom);
    if let Des::Build(def) = des
        && sim.defs.things[def].rotatable
    {
        input.absorb(strip);
        window_background(p, strip);
        let left = Rect::new(
            strip.min.x + 20.0,
            strip.min.y + 13.0,
            strip.min.x + 84.0,
            strip.min.y + 77.0,
        );
        let right = Rect::new(
            strip.max.x - 84.0,
            strip.min.y + 13.0,
            strip.max.x - 20.0,
            strip.min.y + 77.0,
        );
        hits.name("rotate:left", left);
        hits.name("rotate:right", right);
        p.tex(
            left,
            "UI/Widgets/RotLeft",
            if input.hovered(left) {
                Color::WHITE
            } else {
                Color::srgb(0.8, 0.8, 0.8)
            },
        );
        p.tex(
            right,
            "UI/Widgets/RotRight",
            if input.hovered(right) {
                Color::WHITE
            } else {
                Color::srgb(0.8, 0.8, 0.8)
            },
        );
        if input.clicked(left) {
            mgr.rot = match mgr.rot {
                rimworld_sim::Rot4::North => rimworld_sim::Rot4::West,
                rimworld_sim::Rot4::West => rimworld_sim::Rot4::South,
                rimworld_sim::Rot4::South => rimworld_sim::Rot4::East,
                rimworld_sim::Rot4::East => rimworld_sim::Rot4::North,
            };
        }
        if input.clicked(right) {
            mgr.rot = match mgr.rot {
                rimworld_sim::Rot4::North => rimworld_sim::Rot4::East,
                rimworld_sim::Rot4::East => rimworld_sim::Rot4::South,
                rimworld_sim::Rot4::South => rimworld_sim::Rot4::West,
                rimworld_sim::Rot4::West => rimworld_sim::Rot4::North,
            };
        }
        return;
    }
    let Some(style) = mgr.style else { return };
    let Some(cat) = des.info(sim, lang, mgr).style_category else {
        return;
    };
    let styles = super::designator::style_category(cat);
    if styles.len() <= 1 {
        return;
    }
    input.absorb(strip);
    window_background(p, strip);
    let b = Rect::from_center_size(strip.center() - Vec2::new(0.0, 8.0), Vec2::splat(48.0));
    hits.name("drawstyle", b);
    let icon = match style {
        super::designator::Style::Line => "UI/Widgets/DrawStyles/StraightLine",
        super::designator::Style::AngledLine => "UI/Widgets/DrawStyles/AngledLine",
        super::designator::Style::FilledRectangle => "UI/Widgets/DrawStyles/RectangleFilled",
        super::designator::Style::EmptyRectangle => "UI/Widgets/DrawStyles/RectangleEmpty",
        super::designator::Style::FilledOval => "UI/Widgets/DrawStyles/OvalFilled",
        super::designator::Style::EmptyOval => "UI/Widgets/DrawStyles/OvalEmpty",
    };
    p.tex(
        b,
        icon,
        if input.hovered(b) {
            Color::WHITE
        } else {
            Color::srgb(0.85, 0.85, 0.85)
        },
    );
    p.label_mid(
        Rect::new(strip.min.x, b.max.y, strip.max.x, strip.max.y),
        cap(style.label()),
        FONT_TINY,
        Color::WHITE,
        Align::Center,
    );
    if input.clicked(b) {
        // `DoExtraGuiControls`: a menu of the category's styles.
        let options = styles
            .iter()
            .map(|&st| {
                super::float_menu::MenuOption::new(
                    cap(st.label()),
                    Some(super::float_menu::MenuAction::SetStyle { style: st }),
                    super::float_menu::priority::DEFAULT,
                )
            })
            .collect();
        menu.open = Some(super::float_menu::FloatMenu::new(cursor, None, options));
    }
}

/// Low-priority map clicks (`MainTabsRoot.HandleLowPriorityShortcuts`),
/// with no designator or menu taking them.
pub fn low_priority_map_clicks(
    mut clicks: ResMut<PointerClicks>,
    mut tabs: ResMut<MainTabs>,
    mut sel: ResMut<Selection>,
    mut mgr: ResMut<DesignatorManager>,
    menu: Res<super::float_menu::FloatMenuState>,
) {
    if mgr.selected.is_some() || menu.open.is_some() {
        return;
    }
    // With nothing selected a right click toggles the Architect; a click
    // (not middle) closes any other open tab, a left click also clearing
    // the selection.
    if sel.is_empty() && clicks.secondary.is_some() {
        clicks.secondary = None;
        tabs.toggle(MainTab::Architect, &mut mgr);
        return;
    }
    if tabs.open.is_some()
        && (clicks.primary.is_some()
            || clicks.secondary.is_some()
            || clicks.primary_drag_end.is_some())
    {
        if clicks.primary.is_some() || clicks.primary_drag_end.is_some() {
            sel.clear();
        }
        tabs.set(None, &mut mgr);
        clicks.primary = None;
        clicks.secondary = None;
        clicks.primary_drag = None;
        clicks.primary_drag_end = None;
    }
}
