//! rim-rs prototype front-end.
//!
//! Boot order: resolve the user's RimWorld install → load base-game Defs →
//! build the simulation → open the Bevy window. The simulation advances at a
//! fixed 60 ticks/second in `FixedUpdate`; rendering only reads it.

mod autotest;
mod boot;
mod camera;
mod designate;
mod graphics;
mod interact;
mod script;
mod showcase;
mod thing_graphics;
mod ui;
mod view;

use bevy::prelude::*;
use rimworld_sim::{Sim, TICKS_PER_SECOND};

/// The simulation, owned by the Bevy world as a resource.
#[derive(Resource)]
pub struct SimState(pub Sim);

/// Static text about what was loaded, for the overlay.
#[derive(Resource)]
pub struct GameInfo {
    pub summary: String,
}

fn main() {
    let args = match boot::Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let boot = match boot::boot(&args) {
        Ok(b) => b,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };
    if args.check {
        return;
    }

    let play = match &args.play {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| script::parse(&t))
        {
            Ok(cmds) => cmds,
            Err(e) => {
                eprintln!("--play {}: {e}", path.display());
                std::process::exit(2);
            }
        },
        None => Vec::new(),
    };
    let defs = &boot.sim.defs;
    let summary = format!(
        "rim-rs on RimWorld data (Version.txt {}) | {} ThingDefs, {} TerrainDefs, {} PawnKindDefs",
        boot.install
            .version
            .as_ref()
            .map_or("(unknown version)".to_owned(), |v| v.to_string()),
        defs.things.len(),
        defs.terrain.len(),
        defs.pawn_kinds.len()
    );
    let colonist = boot.colonists[0];
    // Keyed UI strings from the install (never copied anywhere).
    let lang = rimworld_defs::keyed::KeyedStrings::load(
        &boot.install.root.join("Data/Core/Languages/English/Keyed"),
    );
    println!("UI strings: {} keyed entries", lang.len());
    let autotest = autotest::AutoTest {
        order: args.order.map(|c| (colonist, c)),
        screenshot: args.screenshot.clone(),
        capture_after: args.capture_after,
        capture_tick: args.capture_tick,
        clicks: if args.click_test {
            // Pawn starts under the window centre; then click up-right of it.
            vec![
                (20, autotest::Gesture::Click(Vec2::ZERO)),
                (40, autotest::Gesture::Click(Vec2::new(360.0, -220.0))),
            ]
        } else if args.box_test {
            // Pawns spawn around the centre; box them, then order the group.
            vec![
                (
                    20,
                    autotest::Gesture::Box(Vec2::splat(-150.0), Vec2::splat(150.0)),
                ),
                (40, autotest::Gesture::Click(Vec2::new(360.0, -220.0))),
            ]
        } else {
            Vec::new()
        },
    };

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "rim-rs".into(),
                resolution: (1400, 900).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.08, 0.08, 0.1)))
        .insert_resource(Time::<Fixed>::from_hz(TICKS_PER_SECOND as f64))
        .insert_resource(graphics::GameTextures(boot.textures))
        .insert_resource(graphics::ViewTexture(args.view_texture.clone()))
        .insert_resource(camera::InitialZoom(args.zoom))
        .insert_resource(camera::InitialFocus(args.focus))
        .insert_resource(SimState(boot.sim))
        .insert_resource(GameInfo { summary })
        .insert_resource(ui::Lang(lang))
        .insert_resource({
            // In the click test the pawn must be selected by clicking it.
            let mut sel = ui::select::Selection::default();
            if !args.click_test && !args.box_test && args.play.is_none() {
                sel.set_pawns(&[colonist], 0.0);
            }
            sel
        })
        .insert_resource(autotest)
        .add_plugins((
            view::ViewPlugin,
            graphics::GraphicsPlugin,
            camera::CameraPlugin,
            interact::InteractPlugin,
            designate::DesignatePlugin,
            ui::UiPlugin,
            autotest::AutoTestPlugin,
            script::ScriptPlugin,
        ))
        .insert_resource(script::PlayScript {
            cmds: play,
            ..Default::default()
        })
        .insert_resource(SimSpeed {
            paused: false,
            ticks_per_step: args.speed.max(1),
        })
        .add_systems(Update, (speed_controls, quick_save_load))
        .add_systems(FixedUpdate, tick_sim)
        .run();
}

/// Game speed, as in RimWorld: pause, 1x, 3x, 6x (and 15x, like the
/// game's developer "ultrafast" speed). Ticks run per fixed 60 Hz step.
#[derive(Resource)]
pub struct SimSpeed {
    pub paused: bool,
    pub ticks_per_step: u32,
}

impl Default for SimSpeed {
    fn default() -> Self {
        Self {
            paused: false,
            ticks_per_step: 1,
        }
    }
}

fn speed_controls(keys: Res<ButtonInput<KeyCode>>, mut speed: ResMut<SimSpeed>) {
    if keys.just_pressed(KeyCode::Space) {
        speed.paused = !speed.paused;
    }
    for (key, ticks) in [
        (KeyCode::Digit1, 1),
        (KeyCode::Digit2, 3),
        (KeyCode::Digit3, 6),
        (KeyCode::Digit4, 15),
    ] {
        if keys.just_pressed(key) {
            speed.ticks_per_step = ticks;
            speed.paused = false;
        }
    }
}

/// Where Ctrl+F5 / Ctrl+F9 save and load (gitignored by the whitelist).
const QUICKSAVE: &str = "saves/quicksave.json";

/// Ctrl+F5 saves the simulation, Ctrl+F9 loads it back (same game data);
/// the function keys alone open main tabs.
#[allow(clippy::too_many_arguments)]
fn quick_save_load(
    keys: Res<ButtonInput<KeyCode>>,
    mut sim: ResMut<SimState>,
    mut status: ResMut<interact::StatusMessage>,
    mut sel: ResMut<ui::select::Selection>,
    mut mgr: ResMut<ui::designator::DesignatorManager>,
    mut tabs: ResMut<ui::tabs::MainTabs>,
    mut menu: ResMut<ui::float_menu::FloatMenuState>,
) {
    let ctrl = interact::ctrl_held(&keys);
    if ctrl && keys.just_pressed(KeyCode::F5) {
        let data = sim.0.save();
        let result = std::fs::create_dir_all("saves").and_then(|_| std::fs::write(QUICKSAVE, data));
        status.0 = match result {
            Ok(()) => format!("saved to {QUICKSAVE}"),
            Err(e) => format!("save failed: {e}"),
        };
    }
    if ctrl && keys.just_pressed(KeyCode::F9) {
        status.0 = match std::fs::read_to_string(QUICKSAVE) {
            Ok(data) => match Sim::load(sim.0.defs.clone(), &data) {
                Ok(loaded) => {
                    let rate = loaded.pawns().len();
                    sim.0 = loaded;
                    // A loaded game starts with a fresh interface: nothing
                    // selected, no designator, no window open.
                    sel.clear();
                    mgr.deselect();
                    tabs.open = None;
                    menu.open = None;
                    format!("loaded {QUICKSAVE} ({rate} pawns)")
                }
                Err(e) => format!("load failed: {e}"),
            },
            Err(e) => format!("load failed: {e}"),
        };
    }
}

fn tick_sim(mut sim: ResMut<SimState>, speed: Res<SimSpeed>) {
    if speed.paused {
        return;
    }
    for _ in 0..speed.ticks_per_step {
        sim.0.tick();
    }
}
