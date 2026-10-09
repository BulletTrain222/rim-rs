//! Float menus (`FloatMenu`, `FloatMenuMakerMap` and its option
//! providers; docs/research.md §66). Right-clicking the map with colonists
//! selected collects the providers' options for the clicked cell and
//! things, sorts them by priority, and either takes the one obvious order
//! (`GetAutoTakeOption`, e.g. a drafted pawn's "Go here") or opens the menu
//! at the cursor with the pawn's name above it. Every option is an order
//! the simulation offers: moves, attacks, equipping, prioritized work.

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::{DefId, ThingDef, WorkGiverDef};
use rimworld_sim::map::ItemId;
use rimworld_sim::sim::{ThingRef, WorkOrderBlock};
use rimworld_sim::work::WorkTarget;
use rimworld_sim::{Cell, Command, PawnId, Sim};

use super::lang::{cap, pawn_label, thing_label};
use super::select::{Selection, objects_under};
use super::{Align, FONT_SMALL, Lang, Messages, Paint, UiHits, UiInput, layer, text_width};
use crate::SimState;
use crate::view::world_to_cell;

/// `MenuOptionPriority`.
pub mod priority {
    pub const DISABLED: u8 = 0;
    pub const GO_HERE: u8 = 1;
    pub const VERY_LOW: u8 = 2;
    pub const DEFAULT: u8 = 4;
    pub const HIGH: u8 = 5;
    pub const ATTACK_ENEMY: u8 = 6;
}

/// What choosing an option does.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    /// The build designator's material (`Designator_Build.ProcessInput`).
    SetStuff {
        def: DefId<ThingDef>,
        stuff: DefId<ThingDef>,
    },
    /// The material menu closed: the label names the stuff (`writeStuff`).
    WriteStuff {
        def: DefId<ThingDef>,
    },
    /// A draw style chosen from the designator's style menu.
    SetStyle {
        style: super::designator::Style,
    },
    GoHere {
        pawns: Vec<PawnId>,
        cell: Cell,
    },
    Equip {
        pawn: PawnId,
        item: ItemId,
    },
    WorkOrder {
        pawn: PawnId,
        giver: DefId<WorkGiverDef>,
        target: WorkTarget,
    },
    FireAt {
        pawns: Vec<PawnId>,
        target: PawnId,
    },
    Melee {
        pawns: Vec<PawnId>,
        target: PawnId,
    },
}

/// A `FloatMenuOption`.
#[derive(Debug, Clone)]
pub struct MenuOption {
    pub label: String,
    /// `None`: disabled.
    pub action: Option<MenuAction>,
    priority: u8,
    /// A thing def drawn as the option's icon.
    pub icon: Option<DefId<ThingDef>>,
    auto_takeable: bool,
    auto_takeable_priority: f32,
    is_goto: bool,
    /// `revalidateClickTarget`: the menu closes when this thing is gone.
    revalidate: Option<ThingRef>,
}

impl MenuOption {
    pub fn new(label: impl Into<String>, action: Option<MenuAction>, priority: u8) -> Self {
        Self {
            label: label.into(),
            action,
            priority,
            icon: None,
            auto_takeable: false,
            auto_takeable_priority: 0.0,
            is_goto: false,
            revalidate: None,
        }
    }

    /// `FloatMenuOption.Priority`: a disabled option sinks to the bottom.
    fn effective_priority(&self) -> u8 {
        if self.action.is_none() {
            priority::DISABLED
        } else {
            self.priority
        }
    }
}

/// An open float menu.
#[derive(Debug, Clone)]
pub struct FloatMenu {
    pub pos: Vec2,
    pub title: Option<String>,
    pub options: Vec<MenuOption>,
    /// Ran when the menu closes either way (`onCloseCallback`).
    pub on_close: Option<MenuAction>,
}

impl FloatMenu {
    /// The menu sorts by priority, descending (stable).
    pub fn new(at: Vec2, title: Option<String>, mut options: Vec<MenuOption>) -> Self {
        options.sort_by_key(|o| std::cmp::Reverse(o.effective_priority()));
        Self {
            pos: at + Vec2::new(4.0, 0.0),
            title,
            options,
            on_close: None,
        }
    }
}

#[derive(Resource, Default)]
pub struct FloatMenuState {
    pub open: Option<FloatMenu>,
}

/// `FloatMenuOption` metrics (normal size).
const V_MARGIN: f32 = 4.0;
const H_MARGIN: f32 = 6.0;
const ICON: f32 = 27.0;

fn option_size(o: &MenuOption) -> Vec2 {
    let icon = if o.icon.is_some() { ICON + 4.0 } else { 0.0 };
    let w = H_MARGIN + 4.0 + text_width(&o.label, FONT_SMALL) + H_MARGIN + icon + 4.0;
    let h = (2.0 * V_MARGIN + super::line_height(FONT_SMALL)).max(if o.icon.is_some() {
        ICON + 2.0
    } else {
        0.0
    });
    Vec2::new(w, h)
}

/// Performs a chosen option.
pub fn perform(
    action: &MenuAction,
    sim: &mut Sim,
    mgr: &mut super::designator::DesignatorManager,
    messages: &mut Messages,
    lang: &Lang,
    now: f32,
) {
    match action {
        MenuAction::SetStuff { def, stuff } => mgr.set_stuff(*def, *stuff),
        MenuAction::WriteStuff { def } => {
            mgr.write_stuff.insert(*def);
        }
        MenuAction::SetStyle { style } => mgr.set_style(*style, sim, lang),
        MenuAction::GoHere { pawns, cell } => {
            for &pawn in pawns {
                // `PawnGotoAction`: already there, or already going there.
                if sim.pawn(pawn).is_some_and(|p| p.position == *cell) {
                    continue;
                }
                if let Err(e) = sim.apply(Command::MoveTo {
                    pawn,
                    target: *cell,
                }) {
                    messages.add(format!("{}: {e}", lang.tr("CannotGoNoPath")), now);
                }
            }
        }
        MenuAction::Equip { pawn, item } => {
            if let Some(it) = sim.map.item(*item).map(|i| i.id) {
                // `clickedThing.SetForbidden(false)` first.
                let defs = sim.defs.clone();
                sim.map.set_forbidden(&defs, it, false);
            }
            if let Err(e) = sim.order_equip(*pawn, *item) {
                messages.add(e, now);
            }
        }
        MenuAction::WorkOrder {
            pawn,
            giver,
            target,
        } => {
            sim.take_work_order(*pawn, *giver, *target);
        }
        MenuAction::FireAt { pawns, target } => {
            for &p in pawns {
                if let Err(e) = sim.order_ranged_attack(p, *target) {
                    messages.add(e, now);
                }
            }
        }
        MenuAction::Melee { pawns, target } => {
            for &p in pawns {
                sim.order_melee_attack(p, *target);
            }
        }
    }
}

/// Keyed reason for the simulation's ranged-attack refusals.
fn ranged_reason(e: &str) -> &'static str {
    match e {
        "out of range" => "OutOfRange",
        "too close" => "TooClose",
        "not drafted" => "IsNotDraftedLower",
        _ => "CannotHitTarget",
    }
}

/// `FloatMenuMakerMap.GetOptions` for the selected colonists at a world
/// position (`None`: no menu, with the message to show).
pub fn pawn_options(
    sim: &mut Sim,
    lang: &Lang,
    pawns: &[PawnId],
    world: Vec2,
) -> Result<Vec<MenuOption>, String> {
    let cell = world_to_cell(world);
    if !sim.map.size().contains(cell) {
        return Ok(Vec::new());
    }
    // `ShouldGenerateFloatMenuForPawn`.
    let mut valid: Vec<PawnId> = Vec::new();
    for &p in pawns {
        let Some(pawn) = sim.pawn(p) else { continue };
        if !pawn.is_colonist || pawn.health.dead {
            continue;
        }
        if pawn.health.downed {
            if pawns.len() == 1 {
                return Err(lang.tr_args("IsIncapped", &[&pawn.name, &pawn.name]));
            }
            continue;
        }
        valid.push(p);
    }
    if valid.is_empty() {
        return Ok(Vec::new());
    }
    let multi = valid.len() > 1;
    let first = valid[0];
    let drafted: Vec<PawnId> = valid
        .iter()
        .copied()
        .filter(|&p| sim.is_drafted(p))
        .collect();
    let clicked: Vec<ThingRef> = objects_under(sim, world)
        .into_iter()
        .filter_map(|o| match o {
            super::select::Selectable::Thing(t) => Some(t),
            _ => None,
        })
        .collect();
    let mut out: Vec<MenuOption> = Vec::new();
    // `FloatMenuOptionProvider_DraftedAttack` (drafted; multiselect).
    if !drafted.is_empty() {
        for &t in &clicked {
            let ThingRef::Pawn(target) = t else { continue };
            if drafted.contains(&target) || valid.contains(&target) {
                continue;
            }
            let hostile = sim.hostile_to_colony(target);
            let animal = sim.is_animal(target);
            if !hostile && !animal {
                continue;
            }
            let name = pawn_label(sim, target);
            let prio = if hostile {
                priority::ATTACK_ENEMY
            } else {
                priority::VERY_LOW
            };
            if multi {
                let ok: Vec<PawnId> = drafted
                    .iter()
                    .copied()
                    .filter(|&p| {
                        if sim.has_firearm(p) {
                            sim.ranged_attack_check(p, target).is_ok()
                        } else {
                            sim.melee_attack_check(p, target).is_ok()
                        }
                    })
                    .collect();
                if !ok.is_empty() {
                    let mut o = MenuOption::new(
                        lang.tr_args("Attack", &[&name, &name]),
                        Some(MenuAction::Melee {
                            pawns: ok.clone(),
                            target,
                        }),
                        prio,
                    );
                    if ok.iter().any(|&p| sim.has_firearm(p)) {
                        o.action = Some(MenuAction::FireAt { pawns: ok, target });
                    }
                    o.auto_takeable = hostile;
                    o.auto_takeable_priority = 40.0;
                    out.push(o);
                }
                continue;
            }
            if !drafted.contains(&first) {
                continue;
            }
            if sim.has_firearm(first) {
                let label = lang.tr_args("FireAt", &[&name, &name]);
                match sim.ranged_attack_check(first, target) {
                    Ok(()) => {
                        let mut o = MenuOption::new(
                            label,
                            Some(MenuAction::FireAt {
                                pawns: vec![first],
                                target,
                            }),
                            prio,
                        );
                        o.auto_takeable = hostile;
                        o.auto_takeable_priority = 40.0;
                        out.push(o);
                    }
                    Err(e) => out.push(MenuOption::new(
                        format!("{label}: {}", lang.tr(ranged_reason(e))),
                        None,
                        prio,
                    )),
                }
            }
            let downed = sim.pawn(target).is_some_and(|p| p.health.downed);
            let label = if downed {
                lang.tr_args("MeleeAttackToDeath", &[&name, &name])
            } else {
                lang.tr_args("MeleeAttack", &[&name, &name])
            };
            match sim.melee_attack_check(first, target) {
                Ok(()) => {
                    let mut o = MenuOption::new(
                        label,
                        Some(MenuAction::Melee {
                            pawns: vec![first],
                            target,
                        }),
                        prio,
                    );
                    o.auto_takeable = hostile;
                    o.auto_takeable_priority = 30.0;
                    out.push(o);
                }
                Err(e) => {
                    let who = sim.pawn(first).map(|p| p.name.clone()).unwrap_or_default();
                    out.push(MenuOption::new(
                        format!("{label}: {}", lang.tr_args(e, &[&who, &who])),
                        None,
                        prio,
                    ));
                }
            }
        }
    }
    // `FloatMenuOptionProvider_DraftedMove` (drafted; multiselect).
    if !drafted.is_empty()
        && let Some(dest) = sim.standable_cell_near(cell, 2.9)
    {
        if multi {
            let ok: Vec<PawnId> = drafted
                .iter()
                .copied()
                .filter(|&p| sim.can_reach_cell(p, dest))
                .collect();
            if !ok.is_empty() {
                let mut o = MenuOption::new(
                    lang.tr("GoHere"),
                    Some(MenuAction::GoHere {
                        pawns: ok,
                        cell: dest,
                    }),
                    priority::GO_HERE,
                );
                o.is_goto = true;
                o.auto_takeable = true;
                o.auto_takeable_priority = 10.0;
                out.push(o);
            }
        } else if drafted.contains(&first) && sim.pawn(first).is_some_and(|p| p.position != dest) {
            if sim.can_reach_cell(first, dest) {
                let mut o = MenuOption::new(
                    lang.tr("GoHere"),
                    Some(MenuAction::GoHere {
                        pawns: vec![first],
                        cell: dest,
                    }),
                    priority::GO_HERE,
                );
                o.is_goto = true;
                o.auto_takeable = true;
                o.auto_takeable_priority = 10.0;
                out.push(o);
            } else {
                out.push(MenuOption::new(
                    lang.tr("CannotGoNoPath"),
                    None,
                    priority::DEFAULT,
                ));
            }
        }
    }
    if multi {
        return Ok(out);
    }
    // `FloatMenuOptionProvider_Equip` (single pawn).
    for &t in &clicked {
        let ThingRef::Item(id) = t else { continue };
        let Some(def) = sim.map.item(id).map(|i| i.def) else {
            continue;
        };
        if !sim.is_firearm(def) {
            continue;
        }
        let label = sim.defs.things[def].label.clone();
        let mut o = match sim.equip_check(first, id) {
            Ok(()) => {
                let mut o = MenuOption::new(
                    lang.tr_args("Equip", &[&label]),
                    Some(MenuAction::Equip {
                        pawn: first,
                        item: id,
                    }),
                    priority::HIGH,
                );
                decorate_reserved(sim, lang, &mut o, first, WorkTarget::Thing(id));
                o
            }
            Err(e) => MenuOption::new(
                format!(
                    "{}: {}",
                    lang.tr_args("CannotEquip", &[&label]),
                    cap(&lang.tr(e))
                ),
                None,
                priority::HIGH,
            ),
        };
        o.icon = Some(def);
        out.push(o);
    }
    // `FloatMenuOptionProvider_WorkGivers` (single pawn): the cell, then
    // each clicked thing.
    let mut used: Vec<String> = Vec::new();
    let cell_opts = sim.work_order_options(first, WorkTarget::Cell(cell));
    out.extend(work_options(
        sim,
        lang,
        first,
        WorkTarget::Cell(cell),
        cell_opts,
        &mut used,
    ));
    for &t in &clicked {
        let target = match t {
            ThingRef::Item(id) => WorkTarget::Thing(id),
            ThingRef::Rock(c) => WorkTarget::Rock(c),
            ThingRef::Pawn(_) => continue,
        };
        let opts = sim.work_order_options(first, target);
        let mut made = work_options(sim, lang, first, target, opts, &mut used);
        for o in &mut made {
            o.icon = icon_def(sim, t);
        }
        out.extend(made);
    }
    Ok(out)
}

/// The thing def shown next to an option about a thing.
fn icon_def(sim: &Sim, t: ThingRef) -> Option<DefId<ThingDef>> {
    match t {
        ThingRef::Item(id) => sim
            .map
            .item(id)
            .map(|i| i.def)
            .or_else(|| sim.map.structure(id).map(|s| s.def))
            .or_else(|| sim.map.plant(id).map(|p| p.def)),
        ThingRef::Rock(c) => sim.map.buildings[c],
        ThingRef::Pawn(_) => None,
    }
}

/// `DecoratePrioritizedTask`: ": Reserved by X"; the option is about
/// this target (`revalidateClickTarget`).
fn decorate_reserved(sim: &Sim, lang: &Lang, o: &mut MenuOption, pawn: PawnId, target: WorkTarget) {
    if let WorkTarget::Thing(id) = target {
        o.revalidate = Some(ThingRef::Item(id));
    }
    let rt = match target {
        WorkTarget::Thing(id) => rimworld_sim::reservation::Target::Item(id),
        WorkTarget::Rock(c) | WorkTarget::Cell(c) => rimworld_sim::reservation::Target::Cell(c),
    };
    if let Some(other) = sim.reservations.reservers_of(rt, pawn).first() {
        let name = pawn_label(sim, *other);
        o.label = format!(
            "{}: {}",
            o.label,
            lang.tr_args("ReservedBy", &[&name, &name])
        );
    }
}

/// The work orders for one target, worded as the provider does, with
/// equivalence groups merged and repeated labels dropped.
fn work_options(
    sim: &Sim,
    lang: &Lang,
    pawn: PawnId,
    target: WorkTarget,
    opts: Vec<rimworld_sim::sim::WorkOrderOption>,
    used: &mut Vec<String>,
) -> Vec<MenuOption> {
    let defs = &sim.defs;
    let target_label = match target {
        WorkTarget::Thing(id) => thing_label(sim, lang, ThingRef::Item(id)),
        WorkTarget::Rock(c) => thing_label(sim, lang, ThingRef::Rock(c)),
        WorkTarget::Cell(_) => lang.tr("AreaLower"),
    };
    let is_thing = !matches!(target, WorkTarget::Cell(_));
    let mut plain: Vec<MenuOption> = Vec::new();
    let mut groups: Vec<(String, MenuOption)> = Vec::new();
    for o in opts {
        let g = &defs.work_givers[o.giver];
        let gerund = o
            .gerund_key
            .map_or_else(|| g.gerund.clone(), |k| lang.tr(k));
        let cannot = lang.tr_args("CannotGenericWork", &[&g.verb, &target_label]);
        let (label, action) = match &o.block {
            None => {
                let label = if is_thing {
                    cap(&lang.tr_args("PrioritizeGeneric", &[&gerund, &target_label]))
                } else {
                    cap(&lang.tr_args("PrioritizeGenericSimple", &[&gerund]))
                };
                (
                    label,
                    Some(MenuAction::WorkOrder {
                        pawn,
                        giver: o.giver,
                        target,
                    }),
                )
            }
            Some(b) => {
                let reason = match b {
                    WorkOrderBlock::MissingCapacity(c) => {
                        let label = defs
                            .capacities
                            .iter()
                            .find(|(_, d)| &d.def_name == c)
                            .map_or(c.clone(), |(_, d)| d.label.clone());
                        format!(
                            "{cannot}: {}",
                            lang.tr_args("CannotMissingHealthActivities", &[&label])
                        )
                    }
                    WorkOrderBlock::AlreadyDoing => {
                        if is_thing {
                            lang.tr_args(
                                "CannotGenericAlreadyAm",
                                &[&gerund, &target_label, &target_label],
                            )
                        } else {
                            lang.tr_args("CannotGenericAlreadyAmCustom", &[&gerund])
                        }
                    }
                    WorkOrderBlock::NotAssigned => {
                        let wt = g
                            .work_type
                            .as_deref()
                            .and_then(|w| defs.work_types.get(w))
                            .map_or(String::new(), |w| w.gerund_label.clone());
                        format!(
                            "{cannot}: {}",
                            lang.tr_args("CannotPrioritizeNotAssignedToWorkType", &[&wt])
                        )
                    }
                    WorkOrderBlock::Research => {
                        format!("{cannot}: {}", lang.tr("CannotPrioritizeResearch"))
                    }
                    WorkOrderBlock::Forbidden => {
                        format!(
                            "{cannot}: {}",
                            lang.tr_args(
                                "CannotPrioritizeForbidden",
                                &[&target_label, &target_label]
                            )
                        )
                    }
                    WorkOrderBlock::NoPath => format!("{cannot}: {}", cap(&lang.tr("NoPath"))),
                };
                (reason, None)
            }
        };
        let mut m = MenuOption::new(label, action, priority::DEFAULT);
        if m.action.is_some() {
            decorate_reserved(sim, lang, &mut m, pawn, target);
            if sim.is_drafted(pawn) && g.auto_takeable_priority_drafted != -1 {
                m.auto_takeable = true;
                m.auto_takeable_priority = g.auto_takeable_priority_drafted as f32;
            }
        }
        if used.contains(&m.label) {
            continue;
        }
        used.push(m.label.clone());
        match (&g.equivalence_group, is_thing) {
            (Some(group), true) => match groups.iter_mut().find(|(k, _)| k == group) {
                Some((_, existing)) => {
                    if existing.action.is_none() && m.action.is_some() {
                        *existing = m;
                    }
                }
                None => groups.push((group.clone(), m)),
            },
            _ => plain.push(m),
        }
    }
    plain.extend(groups.into_iter().map(|(_, m)| m));
    plain
}

/// `GetAutoTakeOption`: when every option is enabled and auto-takeable,
/// the one with the highest auto-take priority.
fn auto_take(options: &[MenuOption]) -> Option<&MenuOption> {
    if options
        .iter()
        .any(|o| o.action.is_none() || !o.auto_takeable)
    {
        return None;
    }
    let mut best: Option<&MenuOption> = None;
    for o in options {
        if best.is_none_or(|b| o.auto_takeable_priority > b.auto_takeable_priority) {
            best = Some(o);
        }
    }
    best
}

/// The open menu: draw it, choose an option, close it.
#[allow(clippy::too_many_arguments)]
pub fn float_menu_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    mut state: ResMut<FloatMenuState>,
    mut sim: ResMut<SimState>,
    mut mgr: ResMut<super::designator::DesignatorManager>,
    mut messages: ResMut<Messages>,
    lang: Res<Lang>,
    mut hits: ResMut<UiHits>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    window: Single<&Window>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs();
    let screen = super::screen(&window);
    if let Some(menu) = state.open.clone() {
        let sizes: Vec<Vec2> = menu.options.iter().map(option_size).collect();
        let width = sizes
            .iter()
            .map(|s| s.x)
            .fold(70.0f32, f32::max)
            .min(300.0)
            .round();
        let height: f32 = sizes.iter().map(|s| s.y - 1.0).sum::<f32>() + 1.0;
        let mut pos = menu.pos;
        pos.x = pos.x.min(screen.x - width).max(0.0);
        pos.y = pos.y.min(screen.y - height).max(0.0);
        let rect = Rect::new(pos.x, pos.y, pos.x + width, pos.y + height);
        // `vanishIfMouseDistant`: fades beyond 5 px, gone beyond 100 px.
        let dist = input.cursor.map_or(0.0, |c| {
            let dx = (rect.min.x - c.x).max(c.x - rect.max.x).max(0.0);
            let dy = (rect.min.y - c.y).max(c.y - rect.max.y).max(0.0);
            (dx * dx + dy * dy).sqrt()
        });
        let alpha = 1.0 - ((dist - 5.0) / 95.0).clamp(0.0, 1.0);
        // An option's click target despawned: the menu closes.
        let gone = menu.options.iter().any(|o| {
            o.revalidate.is_some_and(|t| {
                !super::select::exists(&sim.0, super::select::Selectable::Thing(t))
            })
        });
        let mut close = gone || dist > 100.0 || keys.just_pressed(KeyCode::Escape);
        if keys.just_pressed(KeyCode::Escape) {
            keys.clear_just_pressed(KeyCode::Escape);
        }
        let mut p = paint.painter(layer::FLOAT_MENU);
        if let Some(title) = &menu.title {
            // `FloatMenu.ExtraOnGUI`'s title: at (30, −25) from the menu, on
            // a 150-pixel `TextBGBlack`, the text 15 pixels in.
            let w = (15.0 + text_width(title, FONT_SMALL)).max(150.0);
            let tr = Rect::new(
                rect.min.x + 30.0,
                rect.min.y - 25.0,
                rect.min.x + 30.0 + w,
                rect.min.y - 2.0,
            );
            p.tex(
                Rect::new(tr.min.x, tr.min.y, tr.min.x + 150.0, tr.max.y),
                "UI/Widgets/TextBGBlack",
                Color::srgba(1.0, 1.0, 1.0, alpha),
            );
            p.label_mid(
                Rect::new(tr.min.x + 15.0, tr.min.y, tr.max.x, tr.max.y),
                title.clone(),
                FONT_SMALL,
                Color::srgba(1.0, 1.0, 1.0, alpha),
                Align::Left,
            );
        }
        let mut y = rect.min.y;
        let mut chosen: Option<MenuAction> = None;
        for (o, s) in menu.options.iter().zip(&sizes) {
            let r = Rect::new(rect.min.x, y, rect.max.x, y + s.y);
            hits.name(format!("menu:{}", o.label), r);
            let over = input.hovered(r);
            let bg = if o.action.is_none() {
                Color::srgb_u8(40, 40, 40)
            } else if over {
                Color::srgb_u8(29, 45, 50)
            } else {
                Color::srgb_u8(21, 25, 29)
            };
            p.rect(r, bg.with_alpha(alpha));
            p.outline(r, 1.0, Color::srgba(0.4, 0.4, 0.4, alpha));
            let mut tx = r.min.x + H_MARGIN + 4.0;
            if let Some(def) = o.icon {
                let ir = Rect::new(
                    tx - 2.0,
                    r.min.y + (s.y - ICON) * 0.5,
                    tx - 2.0 + ICON,
                    r.min.y + (s.y + ICON) * 0.5,
                );
                p.thing_icon_fitted(ir, &sim.0, def, Color::WHITE.with_alpha(alpha));
                tx += ICON + 4.0;
            }
            let shift = if over && o.action.is_some() { 4.0 } else { 0.0 };
            let col = if o.action.is_none() {
                Color::srgba(0.9, 0.9, 0.9, alpha)
            } else {
                Color::srgba(1.0, 1.0, 1.0, alpha)
            };
            p.label_mid(
                Rect::new(tx + shift, r.min.y, r.max.x, r.max.y),
                o.label.clone(),
                FONT_SMALL,
                col,
                Align::Left,
            );
            if input.clicked(r)
                && let Some(a) = &o.action
            {
                chosen = Some(a.clone());
                close = true;
            }
            y += s.y - 1.0;
        }
        input.absorb(rect);
        // `closeOnClickedOutside`.
        if input.click_free() || input.right_click_free() {
            input.take_click();
            input.take_right_click();
            close = true;
        }
        if let Some(a) = chosen {
            perform(&a, &mut sim.0, &mut mgr, &mut messages, &lang, now);
        }
        if close {
            if let Some(cb) = &menu.on_close {
                perform(cb, &mut sim.0, &mut mgr, &mut messages, &lang, now);
            }
            state.open = None;
        }
    }
}

/// A right click on the map with colonists selected and no designator
/// (`Selector.HandleMapClicks`); runs after the other widgets took theirs.
#[allow(clippy::too_many_arguments)]
pub fn map_float_menu(
    mut input: ResMut<UiInput>,
    mut state: ResMut<FloatMenuState>,
    mut sim: ResMut<SimState>,
    mut mgr: ResMut<super::designator::DesignatorManager>,
    mut messages: ResMut<Messages>,
    lang: Res<Lang>,
    sel: Res<Selection>,
    window: Single<&Window>,
    camera: Single<(&Camera, &GlobalTransform)>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs();
    let screen = super::screen(&window);
    if state.open.is_some() || mgr.selected.is_some() || !input.right_click_free() {
        return;
    }
    let pawns = sel.pawns();
    if pawns.is_empty() {
        return;
    }
    let Some(at) = input.right_click else { return };
    if input.cursor_over_ui() {
        return;
    }
    let (camera, gt) = *camera;
    let Ok(world) = camera.viewport_to_world_2d(gt, at) else {
        return;
    };
    if at.y > screen.y - 35.0 {
        return;
    }
    input.take_right_click();
    match pawn_options(&mut sim.0, &lang, &pawns, world) {
        Err(msg) => messages.add(msg, now),
        Ok(options) if options.is_empty() => {}
        Ok(options) => {
            let multi = pawns.len() > 1;
            if multi && options.len() == 1 && options[0].is_goto {
                let a = options[0].action.clone().unwrap();
                perform(&a, &mut sim.0, &mut mgr, &mut messages, &lang, now);
            } else if let Some(o) = auto_take(&options) {
                let a = o.action.clone().unwrap();
                perform(&a, &mut sim.0, &mut mgr, &mut messages, &lang, now);
            } else {
                let title = (!multi).then(|| cap(&pawn_label(&sim.0, pawns[0])));
                state.open = Some(FloatMenu::new(at, title, options));
            }
        }
    }
}
