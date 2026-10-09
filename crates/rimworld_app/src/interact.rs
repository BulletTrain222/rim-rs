//! Debug overlays and the prototype's keyboard shortcuts.
//!
//! Selection, orders and designators live in `ui` (the native-style UI).
//! This module keeps what came before it: the shortcuts (Q draft, K
//! attack, Ctrl+F3/F4/F6 spawns, Shift+Tab/E work settings), path and
//! combat debug drawing, and the debug HUD (Ctrl+F1). The function keys
//! alone open the game's main tabs, so the debug keys take Ctrl. The UI
//! never moves pawns itself: it sends the simulation's commands.

use bevy::prelude::*;
use rimworld_sim::{Cell, PawnId};

use crate::ui::select::Selection;
use crate::view::{CELL_SIZE, cell_center, sim_to_world, world_to_cell};
use crate::{GameInfo, SimSpeed, SimState};

/// Cell under the cursor and the path the (single) selected pawn would take.
#[derive(Resource, Default)]
struct Hover {
    cell: Option<Cell>,
    preview: Option<(Cell, Result<Vec<Cell>, String>)>,
}

/// Last user-facing message (e.g. "destination is impassable").
#[derive(Resource, Default)]
pub struct StatusMessage(pub String);

#[derive(Component)]
struct HudText;

/// Systems that read the keyboard after the UI and the designators.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractSet;

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Hover>()
            .init_resource::<StatusMessage>()
            .init_resource::<WorkCursor>()
            .init_resource::<HelpShown>()
            .add_systems(Startup, spawn_hud)
            .configure_sets(
                Update,
                InteractSet
                    .after(crate::ui::designator::DesignatorSet)
                    .after(crate::ui::select::SelectSet),
            )
            .add_systems(
                Update,
                (
                    update_hover,
                    attack_order,
                    combat_keys,
                    work_toggle,
                    draw_debug,
                    update_hud,
                    forward_status,
                )
                    .chain()
                    .in_set(InteractSet),
            );
    }
}

fn cursor_world(camera: &Camera, transform: &GlobalTransform, screen: Vec2) -> Option<Vec2> {
    camera.viewport_to_world_2d(transform, screen).ok()
}

fn update_hover(
    window: Single<&Window>,
    camera: Single<(&Camera, &GlobalTransform)>,
    sim: Res<SimState>,
    selection: Res<Selection>,
    mut hover: ResMut<Hover>,
    cursor: Res<crate::camera::ScriptCursor>,
    input: Res<crate::ui::UiInput>,
) {
    let (camera, cam_transform) = *camera;
    hover.cell = cursor
        .get(&window)
        .and_then(|p| cursor_world(camera, cam_transform, p))
        .map(world_to_cell)
        .filter(|&c| sim.0.map.size().contains(c));
    if input.cursor_over_ui() {
        hover.cell = None;
    }
    // Preview only for a single selected drafted pawn (where "Go here"
    // would send it); recompute when the cell changes.
    match (selection.pawns().as_slice(), hover.cell) {
        ([pawn], Some(cell)) if sim.0.is_drafted(*pawn) => {
            if hover.preview.as_ref().map(|(c, _)| *c) != Some(cell) {
                let path = sim
                    .0
                    .plan_path(*pawn, cell)
                    .map(|p| p.cells)
                    .map_err(|e| e.to_string());
                hover.preview = Some((cell, path));
            }
        }
        _ => hover.preview = None,
    }
}

/// Combat keys: Q drafts the selected colonists (or undrafts them if all
/// are drafted); Ctrl+F6 spawns a hostile target pawn (a drifter) on the
/// hovered cell for testing; Ctrl+F3 / Ctrl+F4 spawn a wild Hare /
/// Muffalo there.
fn combat_keys(
    keys: Res<ButtonInput<KeyCode>>,
    selection: Res<Selection>,
    hover: Res<Hover>,
    mut sim: ResMut<SimState>,
    mut status: ResMut<StatusMessage>,
    mut help: ResMut<HelpShown>,
) {
    let ctrl = ctrl_held(&keys);
    if ctrl && keys.just_pressed(KeyCode::F1) {
        help.0 = !help.0;
    }
    let pawns = selection.pawns();
    if keys.just_pressed(KeyCode::KeyQ) && !pawns.is_empty() {
        let draft = pawns.iter().any(|&p| !sim.0.is_drafted(p));
        let mut n = 0;
        for &p in &pawns {
            if sim.0.set_drafted(p, draft) && sim.0.is_drafted(p) == draft {
                n += 1;
            }
        }
        status.0 = format!(
            "{n} pawn(s) {}",
            if draft { "drafted" } else { "undrafted" }
        );
    }
    if ctrl
        && keys.just_pressed(KeyCode::F6)
        && let Some(c) = hover.cell
    {
        let kind = sim.0.defs.pawn_kinds.id("Drifter");
        status.0 = match kind.map(|k| sim.0.spawn_pawn(k, "Target", c)) {
            Some(Ok(_)) => format!("spawned a hostile target at ({}, {})", c.x, c.z),
            _ => "can't spawn a target there".to_owned(),
        };
    }
    for (key, kind) in [(KeyCode::F3, "Hare"), (KeyCode::F4, "Muffalo")] {
        if ctrl
            && keys.just_pressed(key)
            && let Some(c) = hover.cell
        {
            status.0 = match sim.0.spawn_animal(kind, c) {
                Some(_) => format!("spawned a wild {kind} at ({}, {})", c.x, c.z),
                None => format!("can't spawn a {kind} there"),
            };
        }
    }
}

/// Either Ctrl key is down (the debug function keys).
pub fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
}

/// Whether the debug HUD with the full control list is shown (Ctrl+F1).
#[derive(Resource, Default)]
pub struct HelpShown(pub bool);

/// The work type Shift+Tab points at in the selected pawn's work settings.
#[derive(Resource, Default)]
pub struct WorkCursor(pub usize);

/// Work settings for the selected pawn: Shift+Tab picks the next work
/// type (Tab alone opens the Architect, as in the game), E turns it on
/// (priority 3) or off.
fn work_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    selection: Res<Selection>,
    mut cursor: ResMut<WorkCursor>,
    mut sim: ResMut<SimState>,
    mut status: ResMut<StatusMessage>,
) {
    let [pawn] = selection.pawns()[..] else {
        return;
    };
    let count = sim.0.defs.work_types.len();
    if count == 0 {
        return;
    }
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if keys.just_pressed(KeyCode::Tab) && shift {
        cursor.0 = (cursor.0 + 1) % count;
    }
    if keys.just_pressed(KeyCode::KeyE) {
        let (id, def) = sim.0.defs.work_types.iter().nth(cursor.0 % count).unwrap();
        let name = def.def_name.clone();
        let label = def.label.clone();
        let on = sim
            .0
            .pawn(pawn)
            .and_then(|p| p.work.as_ref())
            .is_some_and(|w| w.raw(id) > 0);
        let ok = sim.0.set_work_priority(pawn, &name, if on { 0 } else { 3 });
        status.0 = if ok {
            format!("{label}: {}", if on { "off" } else { "on" })
        } else {
            format!("{label} can't be changed for this pawn")
        };
    }
}

/// K: the selected pawns attack the hovered pawn in melee.
fn attack_order(
    keys: Res<ButtonInput<KeyCode>>,
    hover: Res<Hover>,
    selection: Res<Selection>,
    mut sim: ResMut<SimState>,
    mut status: ResMut<StatusMessage>,
) {
    if !keys.just_pressed(KeyCode::KeyK) {
        return;
    }
    let Some(target) = hover.cell.and_then(|c| sim.0.pawn_at(c)).map(|p| p.id) else {
        status.0 = "K: hover a pawn to attack".to_owned();
        return;
    };
    // Drafted gunners shoot; everyone else fights in melee.
    let mut n = 0;
    let mut why = None;
    for p in selection.pawns() {
        if p == target {
            continue;
        }
        if sim.0.is_drafted(p) && sim.0.has_firearm(p) {
            match sim.0.order_ranged_attack(p, target) {
                Ok(()) => n += 1,
                Err(e) => why = Some(e),
            }
        } else if sim.0.order_melee_attack(p, target) {
            n += 1;
        }
    }
    status.0 = match (n, why) {
        (0, Some(e)) => format!("can't attack: {e}"),
        (n, _) => format!("{n} pawn(s) attacking"),
    };
}

/// The shortcuts' status lines appear as the game's messages.
fn forward_status(
    status: Res<StatusMessage>,
    mut last: Local<String>,
    mut messages: ResMut<crate::ui::Messages>,
    time: Res<Time>,
) {
    if status.0 != *last {
        last.clone_from(&status.0);
        messages.add(status.0.clone(), time.elapsed_secs());
    }
    if status.is_changed() && status.0.is_empty() {
        last.clear();
    }
}

fn draw_debug(
    sim: Res<SimState>,
    selection: Res<Selection>,
    hover: Res<Hover>,
    mut gizmos: Gizmos,
) {
    let path_color = Color::srgb(1.0, 0.85, 0.2);
    // Bullets in flight, and the aim line of pawns warming up.
    for p in sim.0.projectiles() {
        let pos = sim.0.projectile_position(p);
        gizmos.circle_2d(
            Isometry2d::from_translation(sim_to_world((pos.x - 0.5, pos.z - 0.5))),
            CELL_SIZE * 0.12,
            Color::srgb(1.0, 0.9, 0.3),
        );
    }
    for pawn in sim.0.pawns() {
        if let Some(s) = &pawn.stance
            && s.kind == rimworld_sim::ranged::StanceKind::Warmup
            && let Some(t) = s.target.and_then(|t| sim.0.pawn(t))
        {
            gizmos.line_2d(
                sim_to_world(pawn.visual_position()),
                sim_to_world(t.visual_position()),
                Color::srgba(1.0, 0.4, 0.3, 0.7),
            );
        }
    }
    // The path of selected moving pawns (the game draws drafted pawns'
    // destinations).
    for pawn in selection.pawns().iter().filter_map(|&id| sim.0.pawn(id)) {
        let here = sim_to_world(pawn.visual_position());
        if pawn.is_moving() && pawn.drafted {
            let remaining = pawn
                .step
                .map(|s| s.to)
                .into_iter()
                .chain(pawn.path.iter().copied());
            let points: Vec<Vec2> = std::iter::once(here)
                .chain(remaining.map(cell_center))
                .collect();
            gizmos.linestrip_2d(points.iter().copied(), path_color.with_alpha(0.5));
        } else if let Some((_, Ok(cells))) = &hover.preview {
            let points = std::iter::once(here).chain(cells.iter().map(|&c| cell_center(c)));
            gizmos.linestrip_2d(points, Color::srgba(1.0, 1.0, 1.0, 0.25));
        }
    }
}

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        HudText,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(13.0),
            ..default()
        },
        TextColor(Color::WHITE),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
        Node {
            position_type: PositionType::Absolute,
            top: px(90),
            left: px(8),
            padding: UiRect::all(px(6)),
            ..default()
        },
        GlobalZIndex(5000),
        Visibility::Hidden,
    ));
}

/// Every shortcut, so nothing that works is hidden.
const HELP: &str = "Native UI: LMB select (Shift add, again cycles)  drag: box  RMB: orders menu (nothing selected: Architect)  Tab: Architect  F1: Work  F2: Schedule  Esc: back
Gizmo keys while shown: R draft  F forbid  V power  C cancel  X deconstruct  (Architect: B/H/Y/L/O/U per designator)  Q/E rotate or draw style while placing
Camera: MMB/WASD pan  wheel zoom   Space pause  1-4 speed  Ctrl+F5/Ctrl+F9 save/load   Ctrl+F1 this panel
Old shortcuts (same designators): Z stockpile (again: dumping)  P growing zone (again: next crop of the selected zone)  X remove zone  B build (again: material; G building, R rotate)
  L floor (again: next)  C cancel  V deconstruct  N mine  J remove floor  H smooth  T cut plants  Y harvest  Ctrl+F2 hunt
Hovered: [ ] stockpile priority  F stockpile filter  U flick  O forbid  M/./, bills   I research   Q draft  K attack hovered  Shift+Tab/E work types  Ctrl+F3/F4/F6 spawn hare/muffalo/target";

/// What the colony has in stock: items in stockpiles or lying around.
fn stock_line(sim: &rimworld_sim::Sim) -> String {
    use std::collections::BTreeMap;
    let defs = &sim.defs;
    let mut groups: BTreeMap<&str, u32> = BTreeMap::new();
    for item in sim.map.items() {
        if item.is_filth() || item.forbidden {
            continue;
        }
        let def = &defs.things[item.def];
        let group = match def.def_name.as_str() {
            "WoodLog" => "wood",
            "Steel" => "steel",
            "ComponentIndustrial" => "components",
            n if n.starts_with("Meal") => "meals",
            _ if def.meat_of.is_some() => "meat",
            _ if def.corpse_of.is_some() => "corpses",
            n if n.starts_with("Leather_") => "leather",
            _ if def.ingestible.is_some() => "raw food",
            _ if def.stuff_props.is_some() => "other materials",
            _ => continue,
        };
        *groups.entry(group).or_default() += item.stack_count;
    }
    if groups.is_empty() {
        "Stock: nothing".to_owned()
    } else {
        let parts: Vec<String> = groups.iter().map(|(g, n)| format!("{g} {n}")).collect();
        format!("Stock: {}", parts.join(", "))
    }
}

#[allow(clippy::too_many_arguments)]
fn update_hud(
    sim: Res<SimState>,
    info: Res<GameInfo>,
    selection: Res<Selection>,
    hover: Res<Hover>,
    speed: Res<SimSpeed>,
    work_cursor: Res<WorkCursor>,
    help: Res<HelpShown>,
    hud: Single<(&mut Text, &mut Visibility), With<HudText>>,
) {
    let (mut text, mut vis) = hud.into_inner();
    if !help.0 {
        vis.set_if_neq(Visibility::Hidden);
        return;
    }
    vis.set_if_neq(Visibility::Inherited);
    let sim = &sim.0;
    let speed_label = if speed.paused {
        "paused".to_owned()
    } else {
        format!("{}x", speed.ticks_per_step)
    };
    let mut s = format!(
        "{}\nDay {}, {:02}:00  {:.0} C  [{}]  tick {}  {} pawns\n{}\n",
        info.summary,
        sim.day(),
        sim.hour_of_day(),
        sim.outdoor_temperature,
        speed_label,
        sim.tick_count(),
        sim.pawns().len(),
        stock_line(sim)
    );
    if let [pawn] = selection.pawns()[..]
        && let Some(p) = sim.pawn(pawn)
    {
        if !p.think_trail.is_empty() {
            s += &format!("think: {}\n", p.think_trail.join(" > "));
        }
        if let Some(w) = p.work.as_ref() {
            let types = &sim.defs.work_types;
            let n = types.len().max(1);
            if let Some((t, d)) = types.iter().nth(work_cursor.0 % n) {
                s += &format!(
                    "work: [Shift+Tab: {} is {}, E: toggle]\n",
                    d.label,
                    if w.raw(t) > 0 { "on" } else { "off" }
                );
            }
        }
    }
    if let Some(c) = hover.cell {
        let terrain = &sim.defs.terrain[sim.map.terrain[c]];
        s += &format!(
            "Cell ({}, {}): {} [{}] pathCost {}",
            c.x, c.z, terrain.label, terrain.def_name, terrain.path_cost
        );
        if let Some(b) = sim.map.buildings[c] {
            s += &format!(
                ", {} [{}]",
                sim.defs.things[b].label, sim.defs.things[b].def_name
            );
        }
        if let Some((_, Err(e))) = &hover.preview {
            s += &format!("  ({e})");
        }
        s += "\n";
    }
    s += HELP;
    text.0 = s;
}

/// Pawn ids by name (scripts).
pub fn pawns_named(sim: &rimworld_sim::Sim, name: &str) -> Vec<PawnId> {
    sim.pawns()
        .iter()
        .filter(|p| p.name == name)
        .map(|p| p.id)
        .collect()
}
