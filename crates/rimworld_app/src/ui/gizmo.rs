//! Gizmos and commands (`Gizmo`, `Command`, `Command_Action`,
//! `Command_Toggle`, `GizmoGridDrawer`; docs/research.md §66): what the
//! selected things offer, drawn as 75-pixel buttons in rows from the
//! bottom left (right of the inspect pane), sorted by order; the same
//! commands of several selected things merge into one, which acts for all
//! of them. Toggles show a check box; disabled commands are dimmed and say
//! why. Each command's hot key works while it is drawn.

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::{DefId, TerrainDef, ThingDef};
use rimworld_sim::Sim;
use rimworld_sim::sim::{OrderDesignator, ThingRef};

use super::designator::{Des, DesignatorManager, key_binding, key_label};
use super::lang::cap;
use super::select::{Selectable, Selection};
use super::{
    Align, FONT_TINY, Lang, Messages, Paint, Painter, UiHits, UiInput, layer, text_height,
};
use crate::SimState;
use crate::thing_graphics::{draw_color, look};

/// A gizmo's icon.
#[derive(Debug, Clone, PartialEq)]
pub enum Icon {
    Path(String),
    /// A buildable drawn as it looks (`Widgets.DefIcon`).
    Def(DefId<ThingDef>, Option<DefId<ThingDef>>),
    Terrain(DefId<TerrainDef>),
}

/// What a command does when used.
#[derive(Debug, Clone, PartialEq)]
pub enum GizmoAction {
    /// `Pawn_DraftController`'s toggle.
    Draft,
    /// A reverse designator on the owner.
    Designate(OrderDesignator),
    /// `CompForbiddable`'s toggle.
    Forbid,
    /// `CompFlickable`'s toggle (a colonist flicks it).
    Flick,
    /// `Zone.Delete`.
    DeleteZone,
    /// Select a designator (the Architect, "Copy").
    Select(Des),
}

/// A command gizmo.
#[derive(Debug, Clone)]
pub struct Command {
    pub label: String,
    pub desc: String,
    pub icon: Option<Icon>,
    pub hotkey: Option<KeyCode>,
    pub order: f32,
    /// Disabled, and why.
    pub disabled: Option<String>,
    /// `Command_Toggle`: whether it is on.
    pub toggle: Option<bool>,
    /// `Command_Toggle.activateIfAmbiguous` (default true).
    pub activate_if_ambiguous: bool,
    pub action: GizmoAction,
    /// Whose command it is.
    pub owner: Option<Selectable>,
    /// `groupKeyIgnoreContent`: commands with the same key merge.
    pub group: Option<u64>,
    /// Build designators made from stuff show the options mark.
    pub extra_options: bool,
    /// Highlighted (the Architect's selected designator).
    pub highlight: bool,
}

impl Command {
    pub fn new(label: impl Into<String>, action: GizmoAction) -> Self {
        Self {
            label: label.into(),
            desc: String::new(),
            icon: None,
            hotkey: None,
            order: 0.0,
            disabled: None,
            toggle: None,
            activate_if_ambiguous: true,
            action,
            owner: None,
            group: None,
            extra_options: false,
            highlight: false,
        }
    }

    /// `Command.GroupsWith`.
    fn groups_with(&self, o: &Command) -> bool {
        if let (Some(a), Some(b)) = (self.group, o.group) {
            return a == b;
        }
        self.hotkey == o.hotkey
            && self.label == o.label
            && self.icon == o.icon
            && self.action == o.action
    }
}

/// Gizmo state across frames: the mouse-over for the Architect's info.
#[derive(Resource, Default)]
pub struct GizmoState {
    pub mouseover: Option<Command>,
}

/// `Gizmo.Height` and the grid spacing.
pub const GIZMO: f32 = 75.0;
const SPACING: Vec2 = Vec2::new(5.0, 14.0);

/// The result of drawing a grid.
pub struct GridResult {
    /// The group a click or hot key used.
    pub used: Option<Vec<Command>>,
    pub mouseover: Option<Command>,
}

/// `GizmoGridDrawer.DrawGizmoGrid`.
#[allow(clippy::too_many_arguments)]
pub fn draw_grid(
    p: &mut Painter,
    input: &mut UiInput,
    hits: &mut UiHits,
    keys: &mut ButtonInput<KeyCode>,
    messages: &mut Messages,
    lang: &Lang,
    sim: &Sim,
    mut commands: Vec<Command>,
    start_x: f32,
    screen: Vec2,
    now: f32,
) -> GridResult {
    // Stable sort by order, then merge groups.
    commands.sort_by(|a, b| a.order.total_cmp(&b.order));
    let mut groups: Vec<Vec<Command>> = Vec::new();
    for c in commands {
        match groups.iter_mut().find(|g| g[0].groups_with(&c)) {
            Some(g) => g.push(c),
            None => groups.push(vec![c]),
        }
    }
    let max_x = screen.x - 147.0;
    let top = screen.y - 35.0 - SPACING.y - GIZMO;
    let mut at = Vec2::new(start_x, top);
    let mut result = GridResult {
        used: None,
        mouseover: None,
    };
    let mut drawn_keys: Vec<KeyCode> = Vec::new();
    for group in &groups {
        // The representative: the first enabled; toggles prefer one that
        // the click would turn the way `activateIfAmbiguous` says.
        let mut rep = group
            .iter()
            .find(|c| c.disabled.is_none())
            .unwrap_or(&group[0]);
        if rep.disabled.is_none()
            && let Some(active) = rep.toggle
        {
            if !rep.activate_if_ambiguous
                && !active
                && let Some(c) = group
                    .iter()
                    .find(|c| c.disabled.is_none() && c.toggle == Some(true))
            {
                rep = c;
            }
            if rep.activate_if_ambiguous
                && active
                && let Some(c) = group
                    .iter()
                    .find(|c| c.disabled.is_none() && c.toggle == Some(false))
            {
                rep = c;
            }
        }
        if at.x + GIZMO > max_x {
            at.x = start_x;
            at.y -= GIZMO + SPACING.y;
        }
        let r = Rect::new(at.x, at.y, at.x + GIZMO, at.y + GIZMO);
        hits.name(format!("gizmo:{}", cap(&rep.label)), r);
        let mut fired = draw_command(p, input, rep, r, &mut drawn_keys, keys, sim);
        if input.hovered(r) {
            result.mouseover = Some(rep.clone());
            tooltip(p, input, rep, lang, screen);
        }
        input.absorb(Rect::new(
            r.min.x - 12.0,
            r.min.y,
            r.max.x,
            r.max.y + SPACING.y,
        ));
        if fired && let Some(why) = &rep.disabled {
            if !why.is_empty() {
                messages.add(format!("{}: {why}", lang.tr("DisabledCommand")), now);
            }
            fired = false;
        }
        if fired {
            // `InheritInteractionsFrom`: the other members act too (toggles
            // only those in the same state).
            let members: Vec<Command> = group
                .iter()
                .filter(|c| {
                    c.disabled.is_none() && (rep.toggle.is_none() || c.toggle == rep.toggle)
                })
                .cloned()
                .collect();
            result.used = Some(members);
        }
        at.x += GIZMO + SPACING.x;
    }
    result
}

/// `Command.GizmoOnGUIInt`: background, icon, hot key, label, check box.
fn draw_command(
    p: &mut Painter,
    input: &mut UiInput,
    c: &Command,
    r: Rect,
    drawn_keys: &mut Vec<KeyCode>,
    keys: &mut ButtonInput<KeyCode>,
    sim: &Sim,
) -> bool {
    let over = input.hovered(r);
    let low = c.disabled.is_some();
    if c.highlight {
        p.rect(
            Rect::new(r.min.x - 4.0, r.min.y - 4.0, r.max.x + 4.0, r.max.y + 4.0),
            Color::srgba(1.0, 1.0, 1.0, 0.25),
        );
    }
    // `GenUI.MouseoverColor`; low light dims everything.
    let bg = if low {
        Color::srgba(0.8, 0.8, 0.7, 0.5)
    } else if over {
        Color::srgb(0.3, 0.7, 0.9)
    } else {
        Color::WHITE
    };
    p.tex(r, "UI/Widgets/DesButBG", bg);
    let icon_alpha = if low { 0.6 } else { 1.0 };
    match &c.icon {
        Some(Icon::Path(path)) => p.tex_fitted(r, path, 0.85, Color::WHITE.with_alpha(icon_alpha)),
        Some(Icon::Def(def, stuff)) => def_icon(p, sim, *def, *stuff, r, icon_alpha),
        Some(Icon::Terrain(t)) => {
            let d = &sim.defs.terrain[*t];
            match d.texture_path.clone() {
                Some(path) => p.tex_fitted(r, &path, 0.85, Color::WHITE.with_alpha(icon_alpha)),
                None => {
                    let [cr, cg, cb, _] = crate::view::terrain_color(d);
                    p.rect(
                        super::fit(r, Vec2::ONE, 0.6),
                        Color::srgba_u8(cr, cg, cb, (255.0 * icon_alpha) as u8),
                    );
                }
            }
        }
        None => {}
    }
    let mut fired = false;
    if let Some(k) = c.hotkey
        && !drawn_keys.contains(&k)
    {
        drawn_keys.push(k);
        p.label_mid(
            Rect::new(
                r.min.x + 5.0,
                r.min.y + 3.0,
                r.max.x - 5.0,
                r.min.y + 3.0 + super::line_height(FONT_TINY),
            ),
            key_label(k),
            FONT_TINY,
            Color::WHITE,
            Align::Left,
        );
        if keys.just_pressed(k) {
            keys.clear_just_pressed(k);
            fired = true;
        }
    }
    if input.clicked(r) {
        fired = true;
    }
    if c.extra_options {
        // `Designator_Dropdown.DrawExtraOptionsIcon`.
        p.tex(
            Rect::new(r.max.x - 18.0, r.min.y + 2.0, r.max.x - 2.0, r.min.y + 18.0),
            "UI/Widgets/PlusOptions",
            Color::WHITE,
        );
    }
    if let Some(on) = c.toggle {
        p.tex(
            Rect::new(r.max.x - 24.0, r.min.y, r.max.x, r.min.y + 24.0),
            if on {
                "UI/Widgets/CheckOn"
            } else {
                "UI/Widgets/CheckOff"
            },
            Color::WHITE,
        );
    }
    let label = cap(&c.label);
    if !label.is_empty() {
        let h = text_height(&label, FONT_TINY, r.width());
        let lr = Rect::new(r.min.x, r.max.y - h + 12.0, r.max.x, r.max.y + 12.0);
        // `TexUI.GrayTextBG`.
        p.rect(lr, Color::srgba(0.0, 0.0, 0.0, 0.45));
        p.text(lr, label, FONT_TINY, Color::WHITE, Align::Center);
    }
    fired
}

/// `Widgets.DefIcon` for a buildable (its texture in its stuff colour).
pub fn def_icon(
    p: &mut Painter,
    sim: &Sim,
    def: DefId<ThingDef>,
    stuff: Option<DefId<ThingDef>>,
    r: Rect,
    alpha: f32,
) {
    let d = &sim.defs.things[def];
    let s = stuff.map(|s| &sim.defs.things[s]);
    let color = draw_color(d, s);
    let (tt, lib, images) = p.textures();
    let Some(lib) = lib else { return };
    let rot = match d.default_placing_rot.as_deref() {
        Some("East") => rimworld_sim::Rot4::East,
        Some("South") => rimworld_sim::Rot4::South,
        Some("West") => rimworld_sim::Rot4::West,
        _ => rimworld_sim::Rot4::North,
    };
    let Some(lk) = look(tt, lib, d, s, rot, 0) else {
        return;
    };
    let Some(tex) = tt.get(lib, &lk.path, lk.masked.then_some(color), images) else {
        return;
    };
    let tint = if lk.masked { Color::WHITE } else { color };
    if lk.linked {
        // A linked atlas: its lone-piece tile.
        let uv = crate::thing_graphics::atlas_rect(tex.size, 0);
        let fitted = super::fit(r, Vec2::ONE, 0.85);
        p.image_ex(fitted, tex.image, tint.with_alpha(alpha), Some(uv), false);
    } else {
        let fitted = super::fit(r, lk.size, 0.85);
        p.image_ex(fitted, tex.image, tint.with_alpha(alpha), None, lk.flip_x);
    }
}

/// A tooltip box (`TooltipHandler.TipRegion`): the description, and the
/// disabled reason in red.
fn tooltip(p: &mut Painter, input: &UiInput, c: &Command, lang: &Lang, screen: Vec2) {
    let Some(cur) = input.cursor else { return };
    let mut text = c.desc.clone();
    if let Some(why) = &c.disabled
        && !why.is_empty()
    {
        if !text.is_empty() {
            text += "\n\n";
        }
        text += &format!("{}: {why}", lang.tr("DisabledCommand"));
    }
    if text.is_empty() {
        return;
    }
    let width = 260.0;
    let h = text_height(&text, super::FONT_SMALL, width - 8.0) + 8.0;
    let mut pos = cur + Vec2::new(16.0, -h - 8.0);
    pos.x = pos.x.min(screen.x - width);
    pos.y = pos.y.max(0.0);
    let r = Rect::new(pos.x, pos.y, pos.x + width, pos.y + h);
    let layer = p.layer;
    p.layer = super::layer::TOOLTIP;
    p.rect(r, Color::srgba(0.08, 0.09, 0.1, 0.95));
    p.outline(r, 1.0, Color::srgba(0.5, 0.5, 0.5, 0.8));
    let red = c.disabled.as_ref().is_some_and(|w| !w.is_empty());
    p.text(
        Rect::new(r.min.x + 4.0, r.min.y + 4.0, r.max.x - 4.0, r.max.y - 4.0),
        text,
        super::FONT_SMALL,
        if red && c.desc.is_empty() {
            Color::srgb(1.0, 0.45, 0.45)
        } else {
            Color::WHITE
        },
        Align::Left,
    );
    p.layer = layer;
}

/// The reverse designators (`ReverseDesignatorDatabase`) a thing offers.
fn reverse_designators(
    sim: &Sim,
    lang: &Lang,
    mgr: &DesignatorManager,
    t: ThingRef,
) -> Vec<Command> {
    // In the database's order: Cancel, Deconstruct, Hunt, plants cut /
    // harvest / chop, Mine, Smooth.
    let list = [
        OrderDesignator::Cancel,
        OrderDesignator::Deconstruct,
        OrderDesignator::Hunt,
        OrderDesignator::CutPlants,
        OrderDesignator::Harvest,
        OrderDesignator::HarvestWood,
        OrderDesignator::Mine,
        OrderDesignator::SmoothSurface,
    ];
    let mut out = Vec::new();
    for d in list {
        if sim.can_designate_thing(d, t).is_err() {
            continue;
        }
        let info = Des::Order(d).info(sim, lang, mgr);
        let mut c = Command::new(info.label, GizmoAction::Designate(d));
        c.desc = info.desc;
        c.icon = info.icon.map(|p| Icon::Path(p.to_owned()));
        c.hotkey = info.hotkey;
        c.order = -20.0;
        c.owner = Some(Selectable::Thing(t));
        out.push(c);
    }
    out
}

/// The gizmos of one selected object (`GetGizmos` and the reverse
/// designators).
pub fn gizmos_for(sim: &Sim, lang: &Lang, mgr: &DesignatorManager, o: Selectable) -> Vec<Command> {
    let mut out = Vec::new();
    match o {
        Selectable::Thing(ThingRef::Pawn(p)) => {
            if let Some(pawn) = sim.pawn(p)
                && pawn.is_colonist
            {
                // `Pawn_DraftController.GetGizmos`.
                let label = if pawn.drafted {
                    "CommandUndraftLabel"
                } else {
                    "CommandDraftLabel"
                };
                let mut c = Command::new(lang.tr(label), GizmoAction::Draft);
                c.desc = lang.tr("CommandToggleDraftDesc");
                c.icon = Some(Icon::Path("UI/Commands/Draft".to_owned()));
                c.hotkey = key_binding("Command_ColonistDraft");
                c.toggle = Some(pawn.drafted);
                c.group = Some(81_729_172);
                if pawn.health.downed {
                    c.disabled = Some(lang.tr_args("IsIncapped", &[&pawn.name, &pawn.name]));
                }
                c.owner = Some(o);
                out.push(c);
            }
            if let Some(pawn) = sim.pawn(p) {
                out.extend(reverse_designators(sim, lang, mgr, ThingRef::Pawn(pawn.id)));
            }
        }
        Selectable::Thing(t @ ThingRef::Item(id)) => {
            if let Some(it) = sim.map.item(id) {
                if sim.defs.things[it.def].forbiddable {
                    out.push(forbid_command(lang, it.forbidden, o));
                }
            } else if let Some(s) = sim.map.structure(id) {
                let def = &sim.defs.things[s.def];
                if def.flickable {
                    // `CompFlickable.CompGetGizmosExtra`.
                    let mut c = Command::new(
                        lang.tr("CommandDesignateTogglePowerLabel"),
                        GizmoAction::Flick,
                    );
                    c.desc = lang.tr("CommandDesignateTogglePowerDesc");
                    c.icon = Some(Icon::Path("UI/Commands/DesirePower".to_owned()));
                    c.hotkey = key_binding("Command_TogglePower");
                    c.toggle = Some(s.power.want_switch_on);
                    c.owner = Some(o);
                    out.push(c);
                }
                // `BuildCopyCommandUtility.BuildCopyCommand`.
                if def.designation_category.is_some() && sim.research_unlocked(s.def) {
                    let mut c = Command::new(
                        lang.tr("CommandBuildCopy"),
                        GizmoAction::Select(Des::Build(s.def)),
                    );
                    c.desc = lang.tr("CommandBuildCopyDesc");
                    c.icon = Some(Icon::Def(s.def, s.stuff));
                    c.order = def.ui_order;
                    c.owner = Some(o);
                    out.push(c);
                }
            }
            out.extend(reverse_designators(sim, lang, mgr, t));
        }
        Selectable::Thing(t @ ThingRef::Rock(_)) => {
            out.extend(reverse_designators(sim, lang, mgr, t))
        }
        Selectable::Zone(_) => {
            // `Zone.GetGizmos`: Delete (X).
            let mut c = Command::new(lang.tr("CommandDeleteZoneLabel"), GizmoAction::DeleteZone);
            c.desc = lang.tr("CommandDeleteZoneDesc");
            c.icon = Some(Icon::Path("UI/Buttons/Delete".to_owned()));
            c.hotkey = key_binding("Designator_Deconstruct");
            c.owner = Some(o);
            out.push(c);
        }
    }
    out
}

fn forbid_command(lang: &Lang, forbidden: bool, o: Selectable) -> Command {
    // `CompForbiddable.CompGetGizmosExtra`.
    let mut c = Command::new(lang.tr("CommandAllow"), GizmoAction::Forbid);
    c.desc = lang.tr(if forbidden {
        "CommandForbiddenDesc"
    } else {
        "CommandNotForbiddenDesc"
    });
    c.icon = Some(Icon::Path("UI/Designators/ForbidOff".to_owned()));
    c.hotkey = key_binding("Command_ItemForbid");
    c.toggle = Some(!forbidden);
    c.activate_if_ambiguous = false;
    c.owner = Some(o);
    c
}

/// Uses a command for each of its owners.
#[allow(clippy::too_many_arguments)]
pub fn use_commands(
    members: &[Command],
    sim: &mut Sim,
    sel: &mut Selection,
    mgr: &mut DesignatorManager,
    lang: &Lang,
    menu: &mut super::float_menu::FloatMenuState,
    cursor: Vec2,
) {
    let Some(first) = members.first() else { return };
    match &first.action {
        GizmoAction::Draft => {
            for c in members {
                if let Some(Selectable::Thing(ThingRef::Pawn(p))) = c.owner {
                    let on = sim.is_drafted(p);
                    sim.set_drafted(p, !on);
                }
            }
        }
        GizmoAction::Designate(d) => {
            for c in members {
                if let Some(Selectable::Thing(t)) = c.owner
                    && sim.can_designate_thing(*d, t).is_ok()
                {
                    sim.designate_thing(*d, t);
                }
            }
        }
        GizmoAction::Forbid => {
            let defs = sim.defs.clone();
            for c in members {
                if let Some(Selectable::Thing(ThingRef::Item(id))) = c.owner
                    && let Some(f) = sim.map.item(id).map(|i| i.forbidden)
                {
                    sim.map.set_forbidden(&defs, id, !f);
                }
            }
        }
        GizmoAction::Flick => {
            for c in members {
                if let Some(Selectable::Thing(ThingRef::Item(id))) = c.owner {
                    sim.toggle_switch(id);
                }
            }
        }
        GizmoAction::DeleteZone => {
            for c in members {
                if let Some(Selectable::Zone(z)) = c.owner {
                    sim.delete_zone(z);
                    sel.deselect(Selectable::Zone(z));
                }
            }
        }
        GizmoAction::Select(des) => {
            select_designator(*des, sim, mgr, lang, menu, cursor);
        }
    }
}

/// `Designator.ProcessInput`: selects the designator; a build designator
/// made from stuff opens its material menu (`Designator_Build`), or says
/// there is nothing to build it from.
pub fn select_designator(
    des: Des,
    sim: &Sim,
    mgr: &mut DesignatorManager,
    lang: &Lang,
    menu: &mut super::float_menu::FloatMenuState,
    cursor: Vec2,
) -> Option<String> {
    if let Des::Build(def) = des
        && sim.defs.things[def].made_from_stuff()
    {
        let stuffs = super::designator::stuff_options(sim, def);
        if stuffs.is_empty() {
            return Some(lang.tr("NoStuffsToBuildWith"));
        }
        let options: Vec<super::float_menu::MenuOption> = stuffs
            .iter()
            .map(|&s| {
                let mut o = super::float_menu::MenuOption::new(
                    super::designator::stuff_option_label(sim, lang, def, s),
                    Some(super::float_menu::MenuAction::SetStuff { def, stuff: s }),
                    super::float_menu::priority::DEFAULT,
                );
                o.icon = Some(s);
                o
            })
            .collect();
        let mut m = super::float_menu::FloatMenu::new(cursor, None, options);
        // `onCloseCallback`: the label names the stuff from now on.
        m.on_close = Some(super::float_menu::MenuAction::WriteStuff { def });
        menu.open = Some(m);
    }
    mgr.select(des, sim, lang);
    None
}

/// The selection's gizmo grid, right of the inspect pane (hidden while
/// the Architect is open).
#[allow(clippy::too_many_arguments)]
pub fn gizmos_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    mut hits: ResMut<UiHits>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut messages: ResMut<Messages>,
    lang: Res<Lang>,
    mut sim: ResMut<SimState>,
    mut sel: ResMut<Selection>,
    mut mgr: ResMut<DesignatorManager>,
    mut menu: ResMut<super::float_menu::FloatMenuState>,
    tabs: Res<super::tabs::MainTabs>,
    window: Single<&Window>,
    time: Res<Time>,
    mut state: ResMut<GizmoState>,
) {
    state.mouseover = None;
    if tabs.open.is_some() || sel.is_empty() {
        return;
    }
    let screen = super::screen(&window);
    let mut commands = Vec::new();
    for &o in &sel.objects {
        commands.extend(gizmos_for(&sim.0, &lang, &mgr, o));
    }
    let start = 14.0 + super::inspect::PANE_SIZE.x;
    let mut p = paint.painter(layer::GIZMOS);
    let r = draw_grid(
        &mut p,
        &mut input,
        &mut hits,
        &mut keys,
        &mut messages,
        &lang,
        &sim.0,
        commands,
        start,
        screen,
        time.elapsed_secs(),
    );
    if let Some(members) = r.used {
        let cursor = input.cursor.unwrap_or_default();
        use_commands(
            &members, &mut sim.0, &mut sel, &mut mgr, &lang, &mut menu, cursor,
        );
    }
}
