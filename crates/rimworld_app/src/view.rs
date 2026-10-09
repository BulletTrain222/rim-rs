//! Rendering of simulation state. Reads the sim; never writes it.
//!
//! This module owns coordinates, the debug colour map (drawn underneath the
//! real textures and used alone when textures are unavailable) and pawn
//! position sync. Textured visuals live in `graphics`.

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rimworld_defs::{Rgba, TerrainDef};
use rimworld_sim::camera::CameraView;
use rimworld_sim::{Cell, PawnId};

use crate::SimState;

/// World units per map cell.
pub const CELL_SIZE: f32 = 32.0;

const Z_MAP: f32 = 0.0;

/// World-space centre of a cell.
pub fn cell_center(c: Cell) -> Vec2 {
    Vec2::new(
        (c.x as f32 + 0.5) * CELL_SIZE,
        (c.z as f32 + 0.5) * CELL_SIZE,
    )
}

/// World-space position for a continuous `(x, z)` cell coordinate.
pub fn sim_to_world((x, z): (f32, f32)) -> Vec2 {
    Vec2::new((x + 0.5) * CELL_SIZE, (z + 0.5) * CELL_SIZE)
}

pub fn world_to_cell(p: Vec2) -> Cell {
    Cell::new(
        (p.x / CELL_SIZE).floor() as i32,
        (p.y / CELL_SIZE).floor() as i32,
    )
}

/// Links a render entity to the simulation pawn it displays.
#[derive(Component)]
pub struct PawnView(pub PawnId);

pub struct ViewPlugin;

impl Plugin for ViewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_map)
            .add_systems(Update, (sync_pawns, update_rates_from_camera));
    }
}

fn to_color(c: Rgba) -> [u8; 4] {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [f(c.r), f(c.g), f(c.b), 255]
}

/// Debug colour for a terrain: explicit Def colour if present, else a
/// hand-picked colour for common base-game terrains, else a hash colour.
pub fn terrain_color(t: &TerrainDef) -> [u8; 4] {
    if let Some(c) = t.color {
        return to_color(c);
    }
    let rgb = match t.def_name.as_str() {
        "Soil" => [108, 84, 58],
        "SoilRich" => [76, 58, 38],
        "MossyTerrain" => [94, 102, 64],
        "MarshyTerrain" => [84, 92, 58],
        "Gravel" => [128, 122, 112],
        "Sand" => [196, 178, 128],
        "SoftSand" => [210, 190, 140],
        "Mud" => [88, 70, 52],
        "Ice" => [200, 220, 235],
        "Riverbank" => [100, 92, 70],
        "Marsh" => [66, 84, 66],
        "WaterShallow" | "WaterOceanShallow" | "WaterMovingShallow" => [70, 108, 132],
        "WaterDeep" | "WaterOceanDeep" | "WaterMovingChestDeep" => [36, 62, 104],
        name => {
            let h = name.bytes().fold(0x811c_9dc5u32, |h, b| {
                (h ^ b as u32).wrapping_mul(0x0100_0193)
            });
            [
                90 + (h & 0x3f) as u8,
                90 + ((h >> 8) & 0x3f) as u8,
                90 + ((h >> 16) & 0x3f) as u8,
            ]
        }
    };
    [rgb[0], rgb[1], rgb[2], 255]
}

fn spawn_map(mut commands: Commands, sim: Res<SimState>, mut images: ResMut<Assets<Image>>) {
    let sim = &sim.0;
    let size = sim.map.size();
    let mut data = Vec::with_capacity(size.area() * 4);
    // Image rows go top to bottom; map z grows upwards.
    for row in 0..size.height {
        let z = size.height - 1 - row;
        for x in 0..size.width {
            let c = Cell::new(x, z);
            let px = match sim.map.buildings[c] {
                Some(b) => {
                    let thing = &sim.defs.things[b];
                    thing
                        .graphic
                        .as_ref()
                        .and_then(|g| g.color)
                        .map_or([60, 60, 60, 255], to_color)
                }
                None => terrain_color(&sim.defs.terrain[sim.map.terrain[c]]),
            };
            data.extend_from_slice(&px);
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: size.width as u32,
            height: size.height as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    let world_size = Vec2::new(size.width as f32, size.height as f32) * CELL_SIZE;
    commands.spawn((
        Name::new("Map"),
        Sprite {
            image: images.add(image),
            custom_size: Some(world_size),
            ..default()
        },
        Transform::from_translation((world_size / 2.0).extend(Z_MAP)),
    ));
}

fn sync_pawns(sim: Res<SimState>, mut views: Query<(&PawnView, &mut Transform)>) {
    for (view, mut transform) in &mut views {
        if let Some(pawn) = sim.0.pawn(view.0) {
            let z = transform.translation.z;
            transform.translation = sim_to_world(pawn.visual_position()).extend(z);
        }
    }
}

/// Feeds the game's camera-based update rate to the simulation. Our camera
/// is described in the game's terms (centre in cells, `RootSize` = half the
/// visible height in cells, aspect ratio) and the sim applies the game's
/// zoom thresholds and view rectangle (docs/research.md §14).
fn update_rates_from_camera(
    mut sim: ResMut<SimState>,
    window: Single<&Window>,
    camera: Single<(&Transform, &Projection), With<Camera2d>>,
) {
    let (transform, projection) = *camera;
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    if window.height() <= 0.0 {
        return;
    }
    let view = CameraView {
        centre_x: transform.translation.x / CELL_SIZE,
        centre_z: transform.translation.y / CELL_SIZE,
        root_size: window.height() / 2.0 * ortho.scale / CELL_SIZE,
        aspect: window.width() / window.height(),
    };
    let map = sim.0.map.size();
    let rates: Vec<_> = sim
        .0
        .pawns()
        .iter()
        .map(|p| (p.id, view.update_rate(p.position, map)))
        .collect();
    for (id, rate) in rates {
        sim.0.set_update_rate(id, rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_world_roundtrip() {
        for c in [Cell::new(0, 0), Cell::new(5, 9), Cell::new(99, 0)] {
            assert_eq!(world_to_cell(cell_center(c)), c);
        }
        assert_eq!(world_to_cell(Vec2::new(-1.0, 0.0)), Cell::new(-1, 0));
    }
}
