//! Scripted runs for verification without a human at the keyboard:
//! `--order x,z` issues a move order at startup, `--screenshot <file>` saves
//! the window after a short delay and exits.
//!
//! Screenshots show game data rendered by this program; keep them under
//! `local/` unless reviewed (see docs/provenance.md).

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use rimworld_sim::{Cell, Command, PawnId};

use crate::SimState;

#[derive(Resource, Clone)]
pub struct AutoTest {
    pub order: Option<(PawnId, Cell)>,
    pub screenshot: Option<PathBuf>,
    /// Frames to wait before capturing.
    pub capture_after: u32,
    /// If set, capture when the simulation reaches this tick instead.
    pub capture_tick: Option<u64>,
    /// Synthetic pointer events `(frame, event)` with positions as offsets
    /// from the window centre (logical px), injected exactly where real mouse
    /// input enters.
    pub clicks: Vec<(u32, Gesture)>,
}

#[derive(Clone, Copy, Debug)]
pub enum Gesture {
    Click(Vec2),
    Box(Vec2, Vec2),
}

pub struct AutoTestPlugin;

impl Plugin for AutoTestPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, issue_order)
            .add_systems(
                Update,
                inject_clicks
                    .after(crate::camera::CameraInputSet)
                    .before(crate::ui::UiSet::Begin),
            )
            .add_systems(Update, capture);
    }
}

fn issue_order(test: Res<AutoTest>, mut sim: ResMut<SimState>) {
    if let Some((pawn, target)) = test.order {
        match sim.0.apply(Command::MoveTo { pawn, target }) {
            Ok(dest) => println!("autotest: ordered pawn to ({}, {})", dest.x, dest.z),
            Err(e) => println!("autotest: order failed: {e}"),
        }
    }
}

fn inject_clicks(
    test: Res<AutoTest>,
    mut frame: Local<u32>,
    window: Single<&Window>,
    mut clicks: ResMut<crate::camera::PointerClicks>,
) {
    *frame += 1;
    let centre = Vec2::new(window.width(), window.height()) / 2.0;
    for (at, gesture) in &test.clicks {
        if *at == *frame {
            match *gesture {
                Gesture::Click(o) => clicks.primary = Some(centre + o),
                Gesture::Box(a, b) => clicks.primary_drag_end = Some((centre + a, centre + b)),
            }
            println!("autotest: frame {} {:?}", *frame, gesture);
        }
    }
}

fn capture(test: Res<AutoTest>, mut frame: Local<u32>, mut commands: Commands, sim: Res<SimState>) {
    let Some(path) = &test.screenshot else { return };
    let due = match test.capture_tick {
        Some(t) => sim.0.tick_count() >= t,
        None => *frame + 1 >= test.capture_after,
    };
    // `frame` counts frames since capture (0 = not captured yet).
    if *frame == 0 && !due {
        return;
    }
    *frame += 1;
    if *frame == 1 {
        for p in sim.0.pawns() {
            println!(
                "autotest: tick {} {} at ({}, {}) {} dest={:?}",
                sim.0.tick_count(),
                p.name,
                p.position.x,
                p.position.z,
                sim.0.job_report(p),
                p.destination.map(|d| (d.x, d.z))
            );
        }
        println!("autotest: screenshot -> {}", path.display());
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path.clone()));
    }
    if *frame == 30 {
        commands.write_message(AppExit::Success);
    }
}
