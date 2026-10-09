//! Scripted play (`--play <file>`): drives the app through the same inputs
//! a player uses — key presses, clicks, drags and hovers on map cells —
//! without moving the real mouse, so a whole colony can be played and
//! checked unattended.
//!
//! One command per line (`#` at the start of a line, or ` # `, starts a
//! comment):
//!
//! ```text
//! key Z              press a key for one frame (A–Z, 0–9, Space, Tab, Escape,
//!                    F1–F12, Enter, Period, Comma, BracketLeft,
//!                    BracketRight); `Ctrl+F5`, `Shift+Tab`: with modifiers
//! hover x,z          put the cursor on a cell (tools act on the hovered cell)
//! click x,z          left click a cell
//! rclick x,z         right click a cell
//! drag x,z x,z       left drag from one cell to another
//! sdrag x,z x,z      the same with Shift held
//! focus x,z          centre the camera on a cell
//! select Name        select the pawn with that name
//! uiclick ID         left click a UI element by name (the UI registers
//!                    names such as `gizmo:Draft`, `arch:Structure`,
//!                    `menu:Prioritize mining granite`, `bar:Name`,
//!                    `main:Architect`, `rotate:right`; a prefix matches)
//! uirclick ID        right click a UI element
//! uisclick ID / uisrclick ID
//!                    the same with Shift held
//! uihold ID [ID2]    hold the left button over a UI element (sweeping to
//!                    the second one over a few frames), as when painting
//!                    a schedule
//! gizmo L / arch L / menu L / bar L / main L
//!                    shorthands for `uiclick gizmo:L` etc.
//! uilog              print the UI element names of the last frame
//! logmap             print blueprints, frames, buildings (with their
//!                    stuff), zones and designations
//!
//! A cell may also be `%DefName`: the exposed natural rock of that def
//! (one with a walkable neighbour) nearest to the first colonist; or
//! `^DefName`: the plant of that def nearest to the first colonist.
//! A cell may also be given relative to a pawn (`@Name+dx,dz`) or as the
//! first item of a ThingDef (`#DefName`).
//! tick N             wait until the game reaches tick N
//! frames N           wait N frames
//! shot path.png      save a screenshot
//! log text           print text with the game state
//! quit               exit
//! ```

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use rimworld_sim::Cell;

use crate::SimState;
use crate::camera::{PointerClicks, ScriptCursor};

#[derive(Debug, Clone)]
pub enum Cmd {
    /// Keys pressed together (modifiers first).
    Key(Vec<KeyCode>),
    Hover(At),
    Click(At),
    RightClick(At),
    Drag(At, At, bool),
    Focus(At),
    Select(String),
    /// Click a named UI element (right click, Shift held).
    Ui(String, bool, bool),
    /// Hold the left button over a UI element, sweeping to another.
    UiHold(String, Option<String>),
    UiLog,
    LogMap,
    Tick(u64),
    Frames(u32),
    Shot(PathBuf),
    Log(String),
    Quit,
}

#[derive(Resource, Default)]
pub struct PlayScript {
    pub cmds: Vec<Cmd>,
    pub next: usize,
    pub wait_frames: u32,
    /// Keys pressed last frame, released this frame.
    pub held: Vec<KeyCode>,
    /// Keys to press at the start of the next frame (with real input).
    pub pending: Vec<KeyCode>,
    /// Cursor positions left to visit with the left button held.
    pub sweep: Vec<Vec2>,
    /// Frames spent waiting for a UI element to appear.
    pub ui_wait: u32,
}

fn key_code(name: &str) -> Option<KeyCode> {
    use KeyCode::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    let digits = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    let b = name.as_bytes();
    if b.len() == 1 && b[0].is_ascii_uppercase() {
        return Some(letters[(b[0] - b'A') as usize]);
    }
    if b.len() == 1 && b[0].is_ascii_digit() {
        return Some(digits[(b[0] - b'0') as usize]);
    }
    Some(match name {
        "Space" => Space,
        "Tab" => Tab,
        "Escape" => Escape,
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
        "Shift" => ShiftLeft,
        "Ctrl" => ControlLeft,
        "Alt" => AltLeft,
        "Enter" => Enter,
        "Period" => Period,
        "Comma" => Comma,
        "BracketLeft" => BracketLeft,
        "BracketRight" => BracketRight,
        _ => return None,
    })
}

/// A cell: absolute (`x,z`) or relative to a pawn (`@Name+dx,dz`).
#[derive(Debug, Clone)]
pub enum At {
    Cell(Cell),
    Pawn(String, Cell),
    /// The first item (else building) of a ThingDef (`#DefName`).
    Item(String),
    /// The nearest natural rock of a ThingDef (`%DefName`, optionally
    /// `+dx,dz`).
    Rock(String, Cell),
    /// The nearest plant of a ThingDef (`^DefName`).
    Plant(String),
}

fn at(s: &str) -> Result<At, String> {
    if let Some(def) = s.strip_prefix('#') {
        return Ok(At::Item(def.to_owned()));
    }
    if let Some(def) = s.strip_prefix('^') {
        return Ok(At::Plant(def.to_owned()));
    }
    if let Some(rest) = s.strip_prefix('%') {
        let (name, off) = match rest.find(['+', '-']) {
            Some(i) => (&rest[..i], cell(rest[i..].trim_start_matches('+'))?),
            None => (rest, Cell::new(0, 0)),
        };
        return Ok(At::Rock(name.to_owned(), off));
    }
    if let Some(rest) = s.strip_prefix('@') {
        let (name, off) = match rest.find(['+', '-']) {
            Some(i) => (&rest[..i], cell(rest[i..].trim_start_matches('+'))?),
            None => (rest, Cell::new(0, 0)),
        };
        return Ok(At::Pawn(name.to_owned(), off));
    }
    cell(s).map(At::Cell)
}

fn cell(s: &str) -> Result<Cell, String> {
    let (x, z) = s.split_once(',').ok_or_else(|| format!("bad cell {s:?}"))?;
    Ok(Cell::new(
        x.trim().parse().map_err(|_| format!("bad cell {s:?}"))?,
        z.trim().parse().map_err(|_| format!("bad cell {s:?}"))?,
    ))
}

/// Parses a play script.
pub fn parse(text: &str) -> Result<Vec<Cmd>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let line = line.split(" # ").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let err = |e: String| format!("line {}: {e}", n + 1);
        let args: Vec<&str> = rest.split_whitespace().collect();
        out.push(match cmd {
            "key" => Cmd::Key(
                rest.split('+')
                    .map(|k| key_code(k.trim()).ok_or_else(|| err(format!("unknown key {k:?}"))))
                    .collect::<Result<_, _>>()?,
            ),
            "hover" => Cmd::Hover(at(rest).map_err(err)?),
            "click" => Cmd::Click(at(rest).map_err(err)?),
            "rclick" => Cmd::RightClick(at(rest).map_err(err)?),
            "drag" | "sdrag" if args.len() == 2 => Cmd::Drag(
                at(args[0]).map_err(err)?,
                at(args[1]).map_err(err)?,
                cmd == "sdrag",
            ),
            "focus" => Cmd::Focus(at(rest).map_err(err)?),
            "select" => Cmd::Select(rest.to_owned()),
            "uiclick" => Cmd::Ui(rest.to_owned(), false, false),
            "uirclick" => Cmd::Ui(rest.to_owned(), true, false),
            "uisclick" => Cmd::Ui(rest.to_owned(), false, true),
            "uisrclick" => Cmd::Ui(rest.to_owned(), true, true),
            "uihold" if !args.is_empty() => {
                Cmd::UiHold(args[0].to_owned(), args.get(1).map(|s| (*s).to_owned()))
            }
            "gizmo" | "arch" | "menu" | "bar" | "main" => {
                Cmd::Ui(format!("{cmd}:{rest}"), false, false)
            }
            "uilog" => Cmd::UiLog,
            "logmap" => Cmd::LogMap,
            "tick" => Cmd::Tick(rest.parse().map_err(|_| err("bad tick".into()))?),
            "frames" => Cmd::Frames(rest.parse().map_err(|_| err("bad frames".into()))?),
            "shot" => Cmd::Shot(rest.into()),
            "log" => Cmd::Log(rest.into()),
            "quit" => Cmd::Quit,
            _ => return Err(err(format!("unknown command {line:?}"))),
        });
    }
    Ok(out)
}

pub struct ScriptPlugin;

impl Plugin for ScriptPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayScript>()
            .add_systems(PreUpdate, apply_keys.after(bevy::input::InputSystems))
            .add_systems(
                Update,
                run_script
                    .in_set(crate::camera::CameraInputSet)
                    .after(crate::camera::mouse_input),
            );
    }
}

/// Presses a scripted key where real input lands (so every system sees it
/// this frame) and releases it the next frame.
fn apply_keys(mut script: ResMut<PlayScript>, mut keys: ResMut<ButtonInput<KeyCode>>) {
    for k in std::mem::take(&mut script.held) {
        keys.release(k);
    }
    for k in std::mem::take(&mut script.pending) {
        keys.press(k);
        script.held.push(k);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_script(
    mut script: ResMut<PlayScript>,
    keys: Res<ButtonInput<KeyCode>>,
    mut clicks: ResMut<PointerClicks>,
    mut cursor: ResMut<ScriptCursor>,
    mut selection: ResMut<crate::ui::select::Selection>,
    status: Res<crate::interact::StatusMessage>,
    hits: Res<crate::ui::UiHits>,
    messages: Res<crate::ui::Messages>,
    sim: Res<SimState>,
    mut camera: Single<(&Camera, &GlobalTransform, &mut Transform)>,
    mut commands: Commands,
) {
    if script.cmds.is_empty() {
        return;
    }
    if script.wait_frames > 0 {
        script.wait_frames -= 1;
        return;
    }
    if !script.sweep.is_empty() {
        let at = script.sweep.remove(0);
        cursor.0 = Some(at);
        clicks.primary_held = true;
        return;
    }
    let (cam, gt, ref mut transform) = *camera;
    let resolve = |a: &At| match a {
        At::Cell(c) => Some(*c),
        At::Pawn(name, off) => sim
            .0
            .pawns()
            .iter()
            .find(|p| &p.name == name && !p.health.dead)
            .map(|p| p.position + *off),
        At::Plant(def) => {
            let d = sim.0.defs.things.id(def)?;
            let from = sim.0.pawns().iter().find(|p| p.is_colonist)?.position;
            sim.0
                .map
                .plants()
                .iter()
                .filter(|p| p.def == d)
                .map(|p| p.position)
                .min_by_key(|c| c.distance_squared(from))
        }
        At::Rock(def, off) => {
            let d = sim.0.defs.things.id(def)?;
            let from = sim.0.pawns().iter().find(|p| p.is_colonist)?.position;
            let size = sim.0.map.size();
            (0..size.height)
                .flat_map(|z| (0..size.width).map(move |x| Cell::new(x, z)))
                .filter(|&c| sim.0.map.buildings[c] == Some(d))
                .filter(|&c| {
                    Cell::NEIGHBORS_8
                        .iter()
                        .any(|&n| size.contains(c + n) && sim.0.path_grid().walkable(c + n))
                })
                .min_by_key(|c| c.distance_squared(from))
                .map(|c| c + *off)
        }
        At::Item(def) => {
            let d = sim.0.defs.things.id(def)?;
            sim.0
                .map
                .items()
                .iter()
                .find(|i| i.def == d)
                .map(|i| i.position)
                .or_else(|| {
                    sim.0
                        .map
                        .structures()
                        .iter()
                        .find(|b| b.def == d)
                        .map(|b| b.footprint.center)
                })
        }
    };
    let to_screen = |a: &At| {
        let c = resolve(a)?;
        cam.world_to_viewport(gt, crate::view::cell_center(c).extend(0.0))
            .ok()
    };
    // At most one input per frame, so each lands in its own frame.
    while let Some(cmd) = script.cmds.get(script.next).cloned() {
        script.next += 1;
        match cmd {
            Cmd::Key(k) => {
                script.pending = k;
                script.wait_frames = 1;
                return;
            }
            Cmd::Hover(c) => {
                cursor.0 = to_screen(&c);
                script.wait_frames = 1;
                return;
            }
            Cmd::Click(c) => {
                cursor.0 = to_screen(&c);
                clicks.primary = cursor.0;
                return;
            }
            Cmd::RightClick(c) => {
                cursor.0 = to_screen(&c);
                println!(
                    "play: rclick {:?} -> {:?} at {:?}",
                    c,
                    resolve(&c),
                    cursor.0
                );
                clicks.secondary = cursor.0;
                return;
            }
            Cmd::Drag(a, b, shift) => {
                // Shift goes down first; the drag lands the next frame.
                if shift && !keys.pressed(KeyCode::ShiftLeft) {
                    script.pending = vec![KeyCode::ShiftLeft];
                    script.next -= 1;
                    return;
                }
                if let (Some(a), Some(b)) = (to_screen(&a), to_screen(&b)) {
                    cursor.0 = Some(b);
                    clicks.primary_drag_end = Some((a, b));
                }
                return;
            }
            Cmd::Select(name) => {
                let pawns = crate::interact::pawns_named(&sim.0, &name);
                selection.set_pawns(&pawns, 0.0);
            }
            Cmd::UiHold(a, b) => {
                match (hits.find(&a), b.map(|b| hits.find(&b))) {
                    (Some(ra), None) => script.sweep = vec![ra.center(); 2],
                    (Some(ra), Some(Some(rb))) => {
                        let steps = 16;
                        script.sweep = (0..=steps)
                            .map(|i| ra.center().lerp(rb.center(), i as f32 / steps as f32))
                            .collect();
                    }
                    _ if script.ui_wait < 120 => {
                        script.ui_wait += 1;
                        script.next -= 1;
                        return;
                    }
                    _ => println!("play: ui element {a:?} not found"),
                }
                script.ui_wait = 0;
                println!("play: ui hold {a}");
                return;
            }
            Cmd::Ui(id, right, shift) => {
                // Shift goes down first; the click lands the next frame.
                if shift && !keys.pressed(KeyCode::ShiftLeft) {
                    script.pending = vec![KeyCode::ShiftLeft];
                    script.next -= 1;
                    return;
                }
                // The element is from last frame's layout; wait for it to
                // appear (up to two seconds).
                match hits.find(&id) {
                    Some(r) => {
                        script.ui_wait = 0;
                        let at = r.center();
                        cursor.0 = Some(at);
                        if right {
                            clicks.secondary = Some(at);
                        } else {
                            clicks.primary = Some(at);
                        }
                        println!("play: ui {}click {id}", if right { "r" } else { "" });
                    }
                    None if script.ui_wait < 120 => {
                        script.ui_wait += 1;
                        script.next -= 1;
                    }
                    None => {
                        script.ui_wait = 0;
                        println!("play: ui element {id:?} not found");
                    }
                }
                return;
            }
            Cmd::LogMap => {
                let s = &sim.0;
                let name = |d: rimworld_defs::DefId<rimworld_defs::ThingDef>| {
                    s.defs.things[d].def_name.clone()
                };
                for k in s.map.constructibles() {
                    let what = match k.building {
                        rimworld_sim::map::Buildable::Thing(b) => name(b),
                        rimworld_sim::map::Buildable::Floor(f) => {
                            s.defs.terrain[f].def_name.clone()
                        }
                    };
                    let stuff = k.stuff.map(name).unwrap_or_default();
                    let need: Vec<String> = rimworld_sim::construct::total_cost(&s.defs, k)
                        .iter()
                        .map(|&(d, n)| format!("{}/{} {}", k.delivered(d), n, name(d)))
                        .collect();
                    println!(
                        "play:   {:?} {what} stuff={stuff} at ({}, {}) rot {:?} [{}]",
                        k.stage,
                        k.position.x,
                        k.position.z,
                        k.rotation,
                        need.join(", ")
                    );
                }
                for st in s.map.structures() {
                    if s.defs.things[st.def]
                        .building
                        .as_ref()
                        .is_some_and(|b| b.is_natural_rock)
                    {
                        continue;
                    }
                    println!(
                        "play:   Building {} stuff={} at ({}, {}) rot {:?}",
                        name(st.def),
                        st.stuff.map(name).unwrap_or_default(),
                        st.footprint.center.x,
                        st.footprint.center.z,
                        st.footprint.rot
                    );
                }
                for z in s.zones() {
                    let (label, n) = s.zone_label(z);
                    println!(
                        "play:   Zone {label:?} {n}: {} cells, color {:?}",
                        s.zone_cells(z).len(),
                        s.zone_color(z)
                    );
                }
                println!("play:   mine designations: {:?}", s.map.mine_designations);
            }
            Cmd::UiLog => {
                let names: Vec<&str> = hits.last.iter().map(|(n, _)| n.as_str()).collect();
                println!("play: ui elements: {}", names.join(" | "));
            }
            Cmd::Focus(c) => {
                let Some(c) = resolve(&c) else {
                    return;
                };
                let p = crate::view::cell_center(c);
                transform.translation.x = p.x;
                transform.translation.y = p.y;
                script.wait_frames = 1;
                return;
            }
            Cmd::Tick(t) => {
                if sim.0.tick_count() < t {
                    script.next -= 1;
                    return;
                }
            }
            Cmd::Frames(n) => {
                script.wait_frames = n;
                return;
            }
            Cmd::Shot(path) => {
                println!(
                    "play: tick {} screenshot -> {}",
                    sim.0.tick_count(),
                    path.display()
                );
                commands
                    .spawn(Screenshot::primary_window())
                    .observe(save_to_disk(path));
                script.wait_frames = 5;
                return;
            }
            Cmd::Log(text) => {
                println!(
                    "play: tick {} {text} (status: {}; messages: {:?})",
                    sim.0.tick_count(),
                    status.0,
                    messages
                        .0
                        .iter()
                        .map(|(t, _)| t.as_str())
                        .collect::<Vec<_>>()
                );
                for p in sim.0.pawns() {
                    println!(
                        "play:   {} at ({}, {}) {}",
                        p.name,
                        p.position.x,
                        p.position.z,
                        sim.0.job_report(p)
                    );
                }
            }
            Cmd::Quit => {
                commands.write_message(AppExit::Success);
                return;
            }
        }
    }
}
