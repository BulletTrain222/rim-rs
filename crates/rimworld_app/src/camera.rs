//! Camera control with device-independent intents.
//!
//! Input devices (mouse/keyboard today; touch pan/pinch later) only translate
//! raw input into [`CameraIntent`] and [`PointerClicks`]. A single system
//! applies intents to the camera, so adding touch means adding one more
//! producer system, not touching camera logic.

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;

use rimworld_sim::camera::{SIZE_RANGE_MAX, SIZE_RANGE_MIN};

use crate::SimState;
use crate::view::CELL_SIZE;

/// Movement (logical pixels) before a press counts as a drag, not a click.
const DRAG_THRESHOLD: f32 = 6.0;
/// The game's camera zooms between these orthographic sizes (half the
/// visible height, in cells) and starts at `StartingSize` 24.
const GAME_STARTING_ROOT_SIZE: f32 = 24.0;

/// Orthographic scale for a game `RootSize` at the given window height.
fn scale_for_root_size(root_size: f32, window_height: f32) -> f32 {
    root_size * 2.0 * CELL_SIZE / window_height.max(1.0)
}

/// Clamps a scale to the game's camera size range.
fn clamp_scale(scale: f32, window_height: f32) -> f32 {
    scale.clamp(
        scale_for_root_size(SIZE_RANGE_MIN, window_height),
        scale_for_root_size(SIZE_RANGE_MAX, window_height),
    )
}
const KEY_PAN_SPEED: f32 = 700.0;

/// What the camera should do this frame, independent of input device.
#[derive(Resource, Debug, Default)]
pub struct CameraIntent {
    /// Pan in screen space (logical pixels; +x right, +y down), i.e. how far
    /// the world under the pointer was dragged.
    pub pan_screen: Vec2,
    /// Multiplicative zoom (`> 1` zooms out). `1.0` or `0.0` = none.
    pub zoom: f32,
    /// Screen position that stays fixed while zooming (cursor / pinch centre).
    pub zoom_anchor: Option<Vec2>,
}

/// Pointer gestures recognised this frame, in screen coordinates.
#[derive(Resource, Debug, Default)]
pub struct PointerClicks {
    /// Primary press + release without dragging.
    pub primary: Option<Vec2>,
    pub secondary: Option<Vec2>,
    /// Primary drag in progress: (start, current). Used for box selection.
    pub primary_drag: Option<(Vec2, Vec2)>,
    /// Primary drag finished this frame: (start, end).
    pub primary_drag_end: Option<(Vec2, Vec2)>,
    /// The primary button is down this frame.
    pub primary_held: bool,
}

/// A cursor position set by scripted play; tools read the cursor through
/// it so scripts never move the real mouse.
#[derive(Resource, Default)]
pub struct ScriptCursor(pub Option<Vec2>);

impl ScriptCursor {
    pub fn get(&self, window: &Window) -> Option<Vec2> {
        self.0.or_else(|| window.cursor_position())
    }
}

#[derive(Resource, Default)]
pub(crate) struct DragState {
    /// Primary (left) button: press position and whether it became a drag.
    primary_press: Option<Vec2>,
    primary_dragging: bool,
    /// Pan (middle) button: last cursor position while held.
    pan_last: Option<Vec2>,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraInputSet;

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraIntent>()
            .init_resource::<PointerClicks>()
            .init_resource::<ScriptCursor>()
            .init_resource::<DragState>()
            .add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (mouse_input, keyboard_input, apply_camera_intent)
                    .chain()
                    .in_set(CameraInputSet),
            );
    }
}

/// Initial orthographic scale (`None`: the game's starting camera size).
#[derive(Resource)]
pub struct InitialZoom(pub Option<f32>);

/// Initial focus cell, if not the first pawn.
#[derive(Resource, Default)]
pub struct InitialFocus(pub Option<rimworld_sim::Cell>);

fn spawn_camera(
    mut commands: Commands,
    window: Single<&Window>,
    sim: Res<SimState>,
    zoom: Res<InitialZoom>,
    focus: Res<InitialFocus>,
) {
    // Start centred on the requested cell, else the first colonist.
    let focus = focus.0.map(crate::view::cell_center).unwrap_or_else(|| {
        let pawns = sim.0.pawns();
        pawns
            .iter()
            .find(|p| p.is_colonist)
            .or(pawns.first())
            .map(|p| crate::view::sim_to_world(p.visual_position()))
            .unwrap_or_default()
    });
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scale: match zoom.0 {
                Some(z) => clamp_scale(z, window.height()),
                None => scale_for_root_size(GAME_STARTING_ROOT_SIZE, window.height()),
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::from_translation(focus.extend(100.0)),
    ));
}

/// Mouse, RimWorld-style: middle-drag pans, wheel zooms around the cursor,
/// left click/drag selects (reported via [`PointerClicks`]), right click
/// orders.
pub(crate) fn mouse_input(
    window: Single<&Window>,
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    mut drag: ResMut<DragState>,
    mut intent: ResMut<CameraIntent>,
    mut clicks: ResMut<PointerClicks>,
) {
    *intent = CameraIntent::default();
    *clicks = PointerClicks::default();
    let cursor = window.cursor_position();

    // Pan with the middle button.
    if buttons.just_pressed(MouseButton::Middle) {
        drag.pan_last = cursor;
    }
    if buttons.pressed(MouseButton::Middle)
        && let (Some(cur), Some(last)) = (cursor, drag.pan_last)
    {
        intent.pan_screen += cur - last;
        drag.pan_last = Some(cur);
    }
    if buttons.just_released(MouseButton::Middle) {
        drag.pan_last = None;
    }

    // Primary: click vs. drag (box selection).
    clicks.primary_held = buttons.pressed(MouseButton::Left);
    if buttons.just_pressed(MouseButton::Left) {
        drag.primary_press = cursor;
        drag.primary_dragging = false;
    }
    if let (Some(start), Some(cur)) = (drag.primary_press, cursor) {
        if buttons.pressed(MouseButton::Left) {
            if start.distance(cur) > DRAG_THRESHOLD {
                drag.primary_dragging = true;
            }
            if drag.primary_dragging {
                clicks.primary_drag = Some((start, cur));
            }
        }
        if buttons.just_released(MouseButton::Left) {
            if drag.primary_dragging {
                clicks.primary_drag_end = Some((start, cur));
            } else {
                clicks.primary = Some(cur);
            }
            drag.primary_press = None;
            drag.primary_dragging = false;
        }
    }
    if buttons.just_pressed(MouseButton::Right) {
        clicks.secondary = cursor;
    }

    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 40.0,
    };
    if lines != 0.0 {
        intent.zoom = 0.85f32.powf(lines);
        intent.zoom_anchor = cursor;
    }
}

/// Keyboard: WASD / arrow keys pan.
fn keyboard_input(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut intent: ResMut<CameraIntent>,
) {
    let mut dir = Vec2::ZERO;
    if keys.any_pressed([KeyCode::KeyW, KeyCode::ArrowUp]) {
        dir.y += 1.0;
    }
    if keys.any_pressed([KeyCode::KeyS, KeyCode::ArrowDown]) {
        dir.y -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        dir.x -= 1.0;
    }
    if keys.any_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        dir.x += 1.0;
    }
    // Moving the camera right = dragging the world left.
    intent.pan_screen += Vec2::new(-dir.x, dir.y) * KEY_PAN_SPEED * time.delta_secs();
}

fn apply_camera_intent(
    intent: Res<CameraIntent>,
    window: Single<&Window>,
    sim: Res<SimState>,
    camera: Single<(&mut Transform, &mut Projection), With<Camera2d>>,
) {
    let (mut transform, mut projection) = camera.into_inner();
    let Projection::Orthographic(ortho) = &mut *projection else {
        return;
    };

    // Screen → world offset from the screen centre (y flipped).
    let half = Vec2::new(window.width(), window.height()) / 2.0;
    let to_world = |screen: Vec2| Vec2::new(screen.x - half.x, half.y - screen.y);

    let mut pos = transform.translation.truncate();
    pos += Vec2::new(-intent.pan_screen.x, intent.pan_screen.y) * ortho.scale;

    if intent.zoom > 0.0 && intent.zoom != 1.0 {
        let new_scale = clamp_scale(ortho.scale * intent.zoom, window.height());
        if let Some(anchor) = intent.zoom_anchor {
            // Keep the world point under the anchor fixed.
            let offset = to_world(anchor);
            let world = pos + offset * ortho.scale;
            pos = world - offset * new_scale;
        }
        ortho.scale = new_scale;
    }

    // Keep the map centre reachable: clamp to the map bounds plus a margin.
    let size = sim.0.map.size();
    let max = Vec2::new(size.width as f32, size.height as f32) * CELL_SIZE;
    pos = pos.clamp(Vec2::splat(-CELL_SIZE * 10.0), max + CELL_SIZE * 10.0);
    transform.translation = pos.extend(transform.translation.z);
}
