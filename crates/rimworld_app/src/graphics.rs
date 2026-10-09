//! Real RimWorld textures, loaded at runtime from the user's install.
//!
//! - Terrain: one mesh per TerrainDef, world-space UVs on a repeating texture.
//! - Natural rock: the linked rock atlas, one sub-tile chosen per cell from
//!   which neighbours are also rock, tinted with the ThingDef's colour.
//! - Pawns: body + head sprites for the facing direction, tinted skin colour.
//! - Items: one sprite per stack, texture chosen by the graphic class
//!   (stack-count and meal-variant folders); carried items drawn on the pawn.
//!
//! Exact RimWorld rendering (edge blending, shaders) comes
//! later; assumptions are noted in docs/research.md §9-10.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rimworld_assets::unity::{RgbaImage, TextureLibrary};
use rimworld_defs::{DefId, TerrainDef, ThingDef};
use rimworld_sim::{Cell, ItemId, PawnId, Rot4};

use crate::SimState;
use crate::view::{CELL_SIZE, PawnView, cell_center, sim_to_world};

/// Cells covered by one repeat of a terrain texture. Assumption: RimWorld's
/// 64 px/cell density applied to the 1024 px terrain textures (to verify).
// COMPATIBILITY TODO: currently approximate — terrain texture scale and the lack of
// edge blending are not verified.
const TERRAIN_TILE_CELLS: f32 = 16.0;
/// Humanlike body/head quad size in cells (128 px textures).
// COMPATIBILITY TODO: currently approximate — pawn draw size, head offset and skin
// colour are estimates; the game uses the PawnRenderTree.
const PAWN_GRAPHIC_CELLS: f32 = 1.5;
/// Head offset above the body centre, in cells (approximation).
pub const HEAD_OFFSET: Vec2 = Vec2::new(0.0, 0.34);
pub const SKIN_TINT: Color = Color::srgb(0.95, 0.82, 0.70);

const Z_TERRAIN: f32 = 1.0;
const Z_ROCK: f32 = 2.0;
const Z_ITEM: f32 = 5.0;
const Z_BODY: f32 = 10.0;
const Z_CARRIED: f32 = 10.5;
/// Carried things are drawn this far below the pawn centre (cells).
// COMPATIBILITY TODO: currently approximate — the game offsets carried
// things by facing (Pawn_CarryTracker / PawnRenderer).
const CARRIED_OFFSET: Vec2 = Vec2::new(0.0, -0.25);
/// Placeholder colour for items without a texture.
const ITEM_PLACEHOLDER: Color = Color::srgb(0.85, 0.75, 0.35);
const Z_HEAD: f32 = 10.1;

/// Debug: a texture path to draw at the screen centre (`--view-texture`).
#[derive(Resource, Default)]
pub struct ViewTexture(pub Option<String>);

/// The texture library, if the install's Unity data could be opened.
#[derive(Resource)]
pub struct GameTextures(pub Option<TextureLibrary>);

/// Texture suffix for a facing. RimWorld draws west as the mirrored east
/// graphic when no `_west` texture exists.
fn facing_suffix(rot: Rot4) -> &'static str {
    match rot {
        Rot4::North => "north",
        Rot4::South => "south",
        Rot4::East | Rot4::West => "east",
    }
}

#[derive(Resource)]
pub struct PawnGraphics {
    pub body: HashMap<&'static str, Handle<Image>>,
    pub head: HashMap<&'static str, Handle<Image>>,
}

#[derive(Component)]
struct BodySprite;

/// An animal's body: one texture per facing from its kind's adult life
/// stage (`bodyGraphicData`), or none (drawn as a plain square).
#[derive(Component)]
struct AnimalBody;

/// Loaded animal textures by pawn kind (`None`: textures missing).
#[derive(Resource, Default)]
struct AnimalGraphics(HashMap<String, Option<AnimalLook>>);

#[derive(Clone)]
struct AnimalLook {
    dirs: HashMap<&'static str, Handle<Image>>,
    size: Vec2,
}

impl AnimalGraphics {
    fn look(
        &mut self,
        sim: &rimworld_sim::Sim,
        kind: rimworld_defs::DefId<rimworld_defs::PawnKindDef>,
        textures: &GameTextures,
        images: &mut Assets<Image>,
    ) -> Option<AnimalLook> {
        let def = &sim.defs.pawn_kinds[kind];
        if let Some(l) = self.0.get(&def.def_name) {
            return l.clone();
        }
        let look = (|| {
            let lib = textures.0.as_ref()?;
            let (tex, (w, h)) = def.adult_body.as_ref()?;
            let mut dirs = HashMap::new();
            for dir in ["north", "east", "south"] {
                dirs.insert(dir, load(lib, &format!("{tex}_{dir}"), false, images)?);
            }
            Some(AnimalLook {
                dirs,
                size: Vec2::new(*w, *h) * CELL_SIZE,
            })
        })();
        self.0.insert(def.def_name.clone(), look.clone());
        look
    }
}

#[derive(Component)]
struct HeadSprite;

pub struct GraphicsPlugin;

impl Plugin for GraphicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewTexture>()
            .add_systems(Startup, (spawn_pawn_sprites, spawn_view_texture))
            .init_resource::<ItemSprites>()
            .init_resource::<AnimalGraphics>()
            .add_systems(
                Update,
                (
                    spawn_textured_terrain,
                    spawn_rock,
                    sync_pawn_views,
                    update_pawn_sprites,
                    sync_item_sprites,
                ),
            );
    }
}

/// Which of a folder graphic's `n` textures (sorted by name) a stack of
/// `stack` items uses: `Graphic_StackCount.SubGraphicForStackCount`, and
/// `Graphic_MealVariants` (base-game meal kind, no Ideology food kinds).
pub fn stack_graphic_index(class: Option<&str>, n: usize, stack: u32, stack_limit: i32) -> usize {
    if n == 0 {
        return 0;
    }
    let full = stack as i32 == stack_limit;
    match class {
        Some("Graphic_MealVariants") => match n / 3 {
            2 if stack != 1 => 1,
            3 if stack != 1 && full => 2,
            3 if stack != 1 => 1,
            _ => 0,
        },
        Some("Graphic_StackCount") => match n {
            1 => 0,
            2 => usize::from(stack != 1),
            3 if stack == 1 => 0,
            3 if full => 2,
            3 => 1,
            _ if stack == 1 => 0,
            _ if full => n - 1,
            _ => {
                let i = 1
                    + (stack as f32 / stack_limit as f32 * (n as f32 - 3.0) + 1e-5)
                        .round_ties_even() as usize;
                i.min(n - 2)
            }
        },
        _ => 0,
    }
}

#[derive(Component)]
struct ItemView;

#[derive(Component)]
struct CarriedView;

/// Sprites for items on the map, and the textures they use.
#[derive(Resource, Default)]
struct ItemSprites {
    on_map: HashMap<ItemId, (Entity, String)>,
    carried: HashMap<PawnId, (Entity, String)>,
    images: HashMap<String, Option<Handle<Image>>>,
    folders: HashMap<DefId<ThingDef>, Vec<String>>,
}

impl ItemSprites {
    /// Texture path for a stack of `def`, if it has a graphic.
    fn texture_for(
        &mut self,
        lib: &TextureLibrary,
        def_id: DefId<ThingDef>,
        def: &ThingDef,
        stack: u32,
    ) -> Option<String> {
        let graphic = def.graphic.as_ref()?;
        let path = graphic.tex_path.as_deref()?;
        let class = graphic.graphic_class.as_deref();
        match class {
            Some("Graphic_StackCount") | Some("Graphic_MealVariants") => {
                let list = self
                    .folders
                    .entry(def_id)
                    .or_insert_with(|| lib.textures_in_folder(path));
                let i = stack_graphic_index(class, list.len(), stack, def.stack_limit);
                list.get(i).cloned()
            }
            _ => Some(path.to_owned()),
        }
    }

    fn image(
        &mut self,
        lib: &TextureLibrary,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        self.images
            .entry(path.to_owned())
            .or_insert_with(|| load(lib, path, false, images))
            .clone()
    }

    /// The texture path and image for a stack (empty path: placeholder).
    fn lookup(
        &mut self,
        lib: Option<&TextureLibrary>,
        def_id: DefId<ThingDef>,
        def: &ThingDef,
        stack: u32,
        images: &mut Assets<Image>,
    ) -> (String, Option<Handle<Image>>) {
        let Some(lib) = lib else {
            return (String::new(), None);
        };
        match self.texture_for(lib, def_id, def, stack) {
            Some(path) => {
                let image = self.image(lib, &path, images);
                (path, image)
            }
            None => (String::new(), None),
        }
    }
}

fn item_sprite(image: Option<Handle<Image>>) -> Sprite {
    match image {
        Some(image) => Sprite {
            image,
            custom_size: Some(Vec2::splat(CELL_SIZE)),
            ..default()
        },
        None => Sprite {
            color: ITEM_PLACEHOLDER,
            custom_size: Some(Vec2::splat(CELL_SIZE * 0.5)),
            ..default()
        },
    }
}

type ItemQuery<'w, 's, M, N> =
    Query<'w, 's, (&'static mut Sprite, &'static mut Transform), (With<M>, Without<N>)>;

/// Creates or updates the sprite for one stack.
fn place_item_sprite<M: Component, N: Component>(
    commands: &mut Commands,
    entry: Option<&mut (Entity, String)>,
    query: &mut ItemQuery<M, N>,
    (path, image): (String, Option<Handle<Image>>),
    pos: Vec3,
    tint: Color,
    spawn: impl FnOnce(&mut Commands, Sprite, Transform) -> Entity,
) -> Option<(Entity, String)> {
    // Placeholders keep their own colour.
    let tint = if image.is_some() {
        tint
    } else {
        ITEM_PLACEHOLDER
    };
    match entry {
        Some((entity, current)) => {
            if let Ok((mut sprite, mut transform)) = query.get_mut(*entity) {
                if *current != path {
                    *sprite = item_sprite(image);
                    *current = path;
                }
                sprite.color = tint;
                transform.translation = pos;
            }
            None
        }
        None => {
            let mut sprite = item_sprite(image);
            sprite.color = tint;
            Some((
                spawn(commands, sprite, Transform::from_translation(pos)),
                path,
            ))
        }
    }
}

/// Keeps one sprite per item stack on the map and per carried stack.
// COMPATIBILITY TODO: currently approximate — draw size, colour and
// per-item random rotation/offset (`Graphic.DrawOffset`) are not applied.
fn sync_item_sprites(
    mut commands: Commands,
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut sprites: ResMut<ItemSprites>,
    mut images: ResMut<Assets<Image>>,
    mut placed: ItemQuery<ItemView, CarriedView>,
    mut carried: ItemQuery<CarriedView, ItemView>,
) {
    let sim = &sim.0;
    let defs = &sim.defs;
    let lib = textures.0.as_ref();

    let mut seen = Vec::new();
    for item in sim.map.items() {
        let def = &defs.things[item.def];
        // Corpses are drawn as their dead pawn.
        if def.corpse_of.is_some() {
            continue;
        }
        seen.push(item.id);
        let tex = sprites.lookup(lib, item.def, def, item.stack_count, &mut images);
        let pos = cell_center(item.position).extend(Z_ITEM);
        let name = format!("Item {}", def.def_name);
        // Forbidden things are tinted red.
        let tint = if item.forbidden {
            Color::srgb(1.0, 0.55, 0.55)
        } else {
            Color::WHITE
        };
        let new = place_item_sprite(
            &mut commands,
            sprites.on_map.get_mut(&item.id),
            &mut placed,
            tex,
            pos,
            tint,
            |c, sprite, transform| c.spawn((Name::new(name), ItemView, sprite, transform)).id(),
        );
        if let Some(entry) = new {
            sprites.on_map.insert(item.id, entry);
        }
    }
    sprites.on_map.retain(|id, (entity, _)| {
        let keep = seen.contains(id);
        if !keep {
            commands.entity(*entity).despawn();
        }
        keep
    });

    let mut carriers = Vec::new();
    for pawn in sim.pawns() {
        let Some(c) = pawn.carried else { continue };
        carriers.push(pawn.id);
        let tex = sprites.lookup(lib, c.def, &defs.things[c.def], c.count, &mut images);
        let pos =
            (sim_to_world(pawn.visual_position()) + CARRIED_OFFSET * CELL_SIZE).extend(Z_CARRIED);
        let name = format!("Carried by {}", pawn.name);
        let new = place_item_sprite(
            &mut commands,
            sprites.carried.get_mut(&pawn.id),
            &mut carried,
            tex,
            pos,
            Color::WHITE,
            |c, sprite, transform| {
                c.spawn((Name::new(name), CarriedView, sprite, transform))
                    .id()
            },
        );
        if let Some(entry) = new {
            sprites.carried.insert(pawn.id, entry);
        }
    }
    sprites.carried.retain(|id, (entity, _)| {
        let keep = carriers.contains(id);
        if !keep {
            commands.entity(*entity).despawn();
        }
        keep
    });
}

/// Converts decoded mip levels into a Bevy image.
pub fn to_bevy_image(mips: &[RgbaImage], repeat: bool) -> Image {
    let base = &mips[0];
    let data: Vec<u8> = mips.iter().flat_map(|m| m.pixels.iter().copied()).collect();
    let mut image = Image::new_uninit(
        Extent3d {
            width: base.width,
            height: base.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = mips.len() as u32;
    let address = if repeat {
        ImageAddressMode::Repeat
    } else {
        ImageAddressMode::ClampToEdge
    };
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: address,
        address_mode_v: address,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

fn load(
    lib: &TextureLibrary,
    path: &str,
    repeat: bool,
    images: &mut Assets<Image>,
) -> Option<Handle<Image>> {
    match lib.load_mips(path) {
        Ok(Some(mips)) => Some(images.add(to_bevy_image(&mips, repeat))),
        Ok(None) => {
            warn!("texture {path} not found");
            None
        }
        Err(e) => {
            warn!("texture {path}: {e}");
            None
        }
    }
}

/// Accumulates textured quads into a single mesh.
#[derive(Default)]
struct QuadBuilder {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl QuadBuilder {
    /// Adds an axis-aligned quad. `uv_min`/`uv_max` map to the bottom-left and
    /// top-right corners in world space (image v grows downwards).
    fn quad(&mut self, min: Vec2, max: Vec2, uv_min: Vec2, uv_max: Vec2) {
        let i = self.positions.len() as u32;
        self.positions.extend([
            [min.x, min.y, 0.0],
            [max.x, min.y, 0.0],
            [max.x, max.y, 0.0],
            [min.x, max.y, 0.0],
        ]);
        self.uvs.extend([
            [uv_min.x, uv_max.y],
            [uv_max.x, uv_max.y],
            [uv_max.x, uv_min.y],
            [uv_min.x, uv_min.y],
        ]);
        self.indices.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    }

    fn build(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

#[derive(Component)]
struct TerrainMesh;

/// Each terrain's texture material, loaded once (`None`: not found).
type TerrainMaterials = HashMap<DefId<TerrainDef>, Option<(Handle<Image>, Handle<ColorMaterial>)>>;

/// Builds the terrain meshes, again whenever terrain changes in play
/// (mined rock, laid floors).
#[allow(clippy::too_many_arguments)]
fn spawn_textured_terrain(
    mut commands: Commands,
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    old: Query<Entity, With<TerrainMesh>>,
    mut revision: Local<Option<u64>>,
    mut cache: Local<TerrainMaterials>,
) {
    let Some(lib) = &textures.0 else { return };
    let sim = &sim.0;
    let now = sim.map.terrain_revision();
    if *revision == Some(now) {
        return;
    }
    let first = revision.is_none();
    *revision = Some(now);
    for e in &old {
        commands.entity(e).despawn();
    }
    let mut by_terrain: HashMap<DefId<TerrainDef>, QuadBuilder> = HashMap::new();
    for c in sim.map.size().cells() {
        let id = sim.map.terrain[c];
        let (x, z) = (c.x as f32, c.z as f32);
        // World-space UVs: v is negated so the texture is upright.
        let uv_min = Vec2::new(x, -(z + 1.0)) / TERRAIN_TILE_CELLS;
        let uv_max = Vec2::new(x + 1.0, -z) / TERRAIN_TILE_CELLS;
        let min = Vec2::new(x, z) * CELL_SIZE;
        by_terrain.entry(id).or_default().quad(
            min,
            min + Vec2::splat(CELL_SIZE),
            Vec2::new(uv_min.x, uv_min.y),
            Vec2::new(uv_max.x, uv_max.y),
        );
    }
    let mut loaded = 0;
    for (id, quads) in by_terrain {
        let terrain = &sim.defs.terrain[id];
        let Some(path) = &terrain.texture_path else {
            continue;
        };
        let entry = cache.entry(id).or_insert_with(|| {
            let image = load(lib, path, true, &mut images)?;
            // Generated stone floors use greyscale textures tinted with their
            // rock's colour (`TerrainDef.color`).
            let color = terrain
                .color
                .map_or(Color::WHITE, |c| Color::srgba(c.r, c.g, c.b, c.a));
            let material = materials.add(ColorMaterial {
                color,
                texture: Some(image.clone()),
                ..default()
            });
            Some((image, material))
        });
        let Some((_, material)) = entry.clone() else {
            continue;
        };
        loaded += 1;
        commands.spawn((
            TerrainMesh,
            Name::new(format!("Terrain {}", terrain.def_name)),
            Mesh2d(meshes.add(quads.build())),
            MeshMaterial2d(material),
            Transform::from_xyz(0.0, 0.0, Z_TERRAIN),
        ));
    }
    if first {
        info!("terrain: {loaded} TerrainDef textures loaded from the install");
    }
}

/// Index into a 4x4 linked atlas from which cardinal neighbours link
/// (bits: north 1, east 2, south 4, west 8 — assumption, see research §10).
pub fn link_index(links: impl Fn(Cell) -> bool, c: Cell) -> u32 {
    let mut i = 0;
    for (bit, d) in [
        (1, Cell::new(0, 1)),
        (2, Cell::new(1, 0)),
        (4, Cell::new(0, -1)),
        (8, Cell::new(-1, 0)),
    ] {
        if links(c + d) {
            i |= bit;
        }
    }
    i
}

/// UV rectangle (min, max in image space, v down) of an atlas sub-tile.
/// Unity's atlas row 0 is the bottom row; our images are top-down.
pub fn atlas_uv(index: u32) -> (Vec2, Vec2) {
    let col = (index % 4) as f32;
    let row_from_top = 3.0 - (index / 4) as f32;
    let inset = 0.5 / 80.0 * 0.25; // half a texel of an 80 px sub-tile
    let min = Vec2::new(col, row_from_top) * 0.25 + Vec2::splat(inset);
    let max = Vec2::new(col + 1.0, row_from_top + 1.0) * 0.25 - Vec2::splat(inset);
    (min, max)
}

/// `Graphic_LinkedCornerFiller`: a 0.5-cell cover square for each corner
/// whose diagonal and two adjoining cardinal neighbours link, centred
/// |(0.5,0.5)| − |(0.5,0.5)|/2 along the diagonal (0.25 per axis) and
/// shifted 0.09 cells up.
const CORNER_COVER_CELLS: f32 = 0.5;
const CORNER_COVER_OFFSET: f32 = 0.25;
const CORNER_COVER_SHIFT_UP: f32 = 0.09;

/// The cover's texture point: (0.5, 0.6) of the cell's own atlas sub-tile
/// (`CornerFillUVs`, v from the bottom).
fn corner_filler_uv(index: u32) -> Vec2 {
    let (min, max) = atlas_uv(index);
    Vec2::new(min.x + (max.x - min.x) * 0.5, max.y - (max.y - min.y) * 0.6)
}

/// The natural-rock meshes.
#[derive(Component)]
struct RockMesh;

/// Draws natural rock, again whenever rock was mined or added (checked when
/// structures change).
#[allow(clippy::too_many_arguments)]
fn spawn_rock(
    mut commands: Commands,
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut drawn: Local<Option<(u64, usize)>>,
    old: Query<Entity, With<RockMesh>>,
) {
    let Some(lib) = &textures.0 else { return };
    let sim = &sim.0;
    let rev = sim.map.structure_revision();
    if drawn.is_some_and(|(r, _)| r == rev) {
        return;
    }
    let is_rock = |b: &DefId<ThingDef>| {
        sim.defs.things[*b]
            .building
            .as_ref()
            .is_some_and(|x| x.is_natural_rock)
    };
    let count = sim
        .map
        .buildings
        .iter()
        .filter(|(_, b)| b.as_ref().is_some_and(is_rock))
        .count();
    let unchanged = drawn.is_some_and(|(_, n)| n == count);
    *drawn = Some((rev, count));
    if unchanged {
        return;
    }
    for e in &old {
        commands.entity(e).despawn();
    }
    let size_cells = sim.map.size();
    // Rock links to rock and to the map edge (`linkFlags: Rock, MapEdge`).
    let links = |c: Cell| !size_cells.contains(c) || sim.map.buildings[c].is_some();
    let mut by_thing: HashMap<(DefId<ThingDef>, bool), QuadBuilder> = HashMap::new();
    // Only natural rock: built buildings are drawn as structures.
    let rock = |b: &DefId<ThingDef>| {
        sim.defs.things[*b]
            .building
            .as_ref()
            .is_some_and(|x| x.is_natural_rock)
    };
    for (c, b) in sim.map.buildings.iter() {
        let Some(b) = b.as_ref().filter(|b| rock(b)) else {
            continue;
        };
        let (uv_min, uv_max) = atlas_uv(link_index(links, c));
        let min = Vec2::new(c.x as f32, c.z as f32) * CELL_SIZE;
        by_thing.entry((*b, false)).or_default().quad(
            min,
            min + Vec2::splat(CELL_SIZE),
            uv_min,
            uv_max,
        );
    }
    // Corner fillers (`Graphic_LinkedCornerFiller.Print`): the atlas tile
    // keeps shading in its corners; each corner whose diagonal and both
    // adjoining cardinals link is covered. Off the map the cover grows ×5
    // and moves out a cell, so rock reaching the edge stays solid.
    for (c, b) in sim.map.buildings.iter() {
        let Some(b) = b.as_ref().filter(|b| rock(b)) else {
            continue;
        };
        let fill_uv = corner_filler_uv(link_index(links, c));
        let centre = cell_center(c);
        for (dx, dz) in [(-1, -1), (-1, 1), (1, 1), (1, -1)] {
            let around = [Cell::new(dx, dz), Cell::new(dx, 0), Cell::new(0, dz)];
            if !around.iter().all(|&d| links(c + d)) {
                continue;
            }
            let mut at = centre
                + Vec2::new(dx as f32, dz as f32) * CORNER_COVER_OFFSET * CELL_SIZE
                + Vec2::new(0.0, CORNER_COVER_SHIFT_UP * CELL_SIZE);
            let mut size = Vec2::splat(CORNER_COVER_CELLS * CELL_SIZE);
            let d = c + Cell::new(dx, dz);
            if d.x == -1 || d.x == size_cells.width {
                at.x += dx as f32 * CELL_SIZE;
                size.x *= 5.0;
            }
            if d.z == -1 || d.z == size_cells.height {
                at.y += dz as f32 * CELL_SIZE;
                size.y *= 5.0;
            }
            by_thing.entry((*b, true)).or_default().quad(
                at - size / 2.0,
                at + size / 2.0,
                fill_uv,
                fill_uv,
            );
        }
    }
    for ((id, cover), quads) in by_thing {
        let thing = &sim.defs.things[id];
        let Some(graphic) = &thing.graphic else {
            continue;
        };
        let Some(path) = &graphic.tex_path else {
            continue;
        };
        let Some(image) = load(lib, path, false, &mut images) else {
            continue;
        };
        let tint = graphic
            .color
            .map_or(Color::WHITE, |c| Color::srgba(c.r, c.g, c.b, c.a));
        commands.spawn((
            RockMesh,
            Name::new(format!("Rock {}", thing.def_name)),
            Mesh2d(meshes.add(quads.build())),
            MeshMaterial2d(materials.add(ColorMaterial {
                color: tint,
                texture: Some(image),
                ..default()
            })),
            // Covers sit one altitude step above the tiles (`AltIncVect`).
            Transform::from_xyz(0.0, 0.0, Z_ROCK + if cover { 0.01 } else { 0.0 }),
        ));
    }
}

/// Loads the pawn textures (or prepares placeholder discs).
fn spawn_pawn_sprites(
    mut commands: Commands,
    textures: Res<GameTextures>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let placeholder = |commands: &mut Commands,
                       meshes: &mut Assets<Mesh>,
                       materials: &mut Assets<ColorMaterial>| {
        commands.insert_resource(PlaceholderPawn {
            mesh: meshes.add(Circle::new(CELL_SIZE * 0.38)),
            material: materials.add(Color::srgb(0.95, 0.80, 0.55)),
        });
    };
    let Some(lib) = &textures.0 else {
        placeholder(&mut commands, &mut meshes, &mut materials);
        return;
    };
    let mut body = HashMap::new();
    let mut head = HashMap::new();
    for dir in ["north", "east", "south"] {
        let b = load(
            lib,
            &format!("Things/Pawn/Humanlike/Bodies/Naked_Male_{dir}"),
            false,
            &mut images,
        );
        let h = load(
            lib,
            &format!("Things/Pawn/Humanlike/Heads/Male/Male_Average_Normal_{dir}"),
            false,
            &mut images,
        );
        if let (Some(b), Some(h)) = (b, h) {
            body.insert(dir, b);
            head.insert(dir, h);
        }
    }
    if body.len() < 3 {
        warn!("pawn textures incomplete; using placeholder discs");
        placeholder(&mut commands, &mut meshes, &mut materials);
        return;
    }
    commands.insert_resource(PawnGraphics { body, head });
}

/// Placeholder pawn disc (no textures).
#[derive(Resource)]
struct PlaceholderPawn {
    mesh: Handle<Mesh>,
    material: Handle<ColorMaterial>,
}

/// Gives every pawn a view (pawns spawned while playing too) and removes
/// views of pawns that no longer exist (e.g. after loading a save).
#[allow(clippy::too_many_arguments)]
fn sync_pawn_views(
    mut commands: Commands,
    sim: Res<SimState>,
    graphics: Option<Res<PawnGraphics>>,
    placeholder: Option<Res<PlaceholderPawn>>,
    views: Query<(Entity, &PawnView)>,
    textures: Res<GameTextures>,
    mut images: ResMut<Assets<Image>>,
    mut animals: ResMut<AnimalGraphics>,
) {
    let have: std::collections::HashSet<rimworld_sim::PawnId> =
        views.iter().map(|(_, v)| v.0).collect();
    for (e, v) in &views {
        if sim.0.pawn(v.0).is_none() {
            commands.entity(e).despawn();
        }
    }
    for pawn in sim.0.pawns() {
        if have.contains(&pawn.id) {
            continue;
        }
        let pos = sim_to_world(pawn.visual_position());
        if sim.0.is_animal(pawn.id) {
            let look = animals.look(&sim.0, pawn.kind, &textures, &mut images);
            let sprite = match &look {
                Some(l) => Sprite {
                    image: l.dirs["south"].clone(),
                    custom_size: Some(l.size),
                    ..default()
                },
                None => Sprite {
                    color: Color::srgb(0.55, 0.4, 0.25),
                    custom_size: Some(Vec2::splat(CELL_SIZE * 0.6)),
                    ..default()
                },
            };
            commands
                .spawn((
                    Name::new(format!("Animal {}", pawn.name)),
                    PawnView(pawn.id),
                    Transform::from_translation(pos.extend(Z_BODY)),
                    Visibility::default(),
                ))
                .with_children(|p| {
                    p.spawn((BodySprite, AnimalBody, sprite, Transform::default()));
                });
            continue;
        }
        if let Some(g) = &graphics {
            let size = Some(Vec2::splat(PAWN_GRAPHIC_CELLS * CELL_SIZE));
            commands
                .spawn((
                    Name::new(format!("Pawn {}", pawn.name)),
                    PawnView(pawn.id),
                    Transform::from_translation(pos.extend(Z_BODY)),
                    Visibility::default(),
                ))
                .with_children(|p| {
                    p.spawn((
                        BodySprite,
                        Sprite {
                            image: g.body["south"].clone(),
                            custom_size: size,
                            color: SKIN_TINT,
                            ..default()
                        },
                        Transform::default(),
                    ));
                    p.spawn((
                        HeadSprite,
                        Sprite {
                            image: g.head["south"].clone(),
                            custom_size: size,
                            color: SKIN_TINT,
                            ..default()
                        },
                        Transform::from_translation(
                            (HEAD_OFFSET * CELL_SIZE).extend(Z_HEAD - Z_BODY),
                        ),
                    ));
                });
        } else if let Some(ph) = &placeholder {
            commands.spawn((
                Name::new(format!("Pawn {}", pawn.name)),
                PawnView(pawn.id),
                Mesh2d(ph.mesh.clone()),
                MeshMaterial2d(ph.material.clone()),
                Transform::from_translation(pos.extend(Z_BODY)),
            ));
        }
    }
}

fn spawn_view_texture(
    mut commands: Commands,
    view: Res<ViewTexture>,
    textures: Res<GameTextures>,
    sim: Res<SimState>,
    mut images: ResMut<Assets<Image>>,
) {
    let (Some(path), Some(lib)) = (&view.0, &textures.0) else {
        return;
    };
    let Some(handle) = load(lib, path, false, &mut images) else {
        return;
    };
    let centre = sim
        .0
        .pawns()
        .first()
        .map(|p| sim_to_world(p.visual_position()))
        .unwrap_or_default();
    commands.spawn((
        Name::new(format!("Debug texture {path}")),
        Sprite {
            image: handle,
            custom_size: Some(Vec2::splat(12.0 * CELL_SIZE)),
            ..default()
        },
        Transform::from_translation(centre.extend(50.0)),
    ));
}

type BodyQuery<'w, 's> =
    Query<'w, 's, (&'static mut Sprite, Has<AnimalBody>), (With<BodySprite>, Without<HeadSprite>)>;

type HeadQuery<'w, 's> = Query<
    'w,
    's,
    (&'static mut Sprite, &'static mut Transform),
    (With<HeadSprite>, Without<BodySprite>),
>;

/// Draws each pawn's body and head for the facing owned by the simulation.
#[allow(clippy::too_many_arguments)]
fn update_pawn_sprites(
    sim: Res<SimState>,
    graphics: Option<Res<PawnGraphics>>,
    animals: Res<AnimalGraphics>,
    mut pawns: Query<(&PawnView, &Children, &mut Transform), Without<HeadSprite>>,
    mut bodies: BodyQuery,
    mut heads: HeadQuery,
) {
    for (view, children, mut root) in &mut pawns {
        let Some(pawn) = sim.0.pawn(view.0) else {
            continue;
        };
        // COMPATIBILITY TODO: currently approximate — lying posture (rotation/offset on
        // the ground) is not the game's.
        // Pawns lying down are drawn rotated a quarter turn.
        let angle = if pawn.is_lying_down() || pawn.health.downed || pawn.health.dead {
            std::f32::consts::FRAC_PI_2
        } else {
            0.0
        };
        root.rotation = Quat::from_rotation_z(angle);
        let suffix = facing_suffix(pawn.rotation);
        let flip = pawn.rotation == Rot4::West;
        // The dead are drawn as their corpse: greyed, green when rotting,
        // pale when dessicated, gone with the corpse.
        let tint = if pawn.health.dead {
            use rimworld_sim::sim::RotStage;
            let corpse = sim.0.corpse_of(pawn.id);
            match corpse.and_then(|c| sim.0.rot_stage(c)) {
                _ if corpse.is_none() => Color::NONE,
                Some(RotStage::Rotting) => Color::srgb(0.4, 0.5, 0.3),
                Some(RotStage::Dessicated) => Color::srgb(0.8, 0.78, 0.7),
                _ => Color::srgb(0.45, 0.45, 0.45),
            }
        } else {
            Color::WHITE
        };
        let animal_look = animals
            .0
            .get(&sim.0.defs.pawn_kinds[pawn.kind].def_name)
            .and_then(|l| l.as_ref());
        for child in children.iter() {
            if let Ok((mut s, animal)) = bodies.get_mut(child) {
                if animal {
                    if let Some(l) = animal_look {
                        s.image = l.dirs[suffix].clone();
                        s.color = tint;
                    } else if pawn.health.dead {
                        s.color = tint;
                    }
                } else if let Some(g) = &graphics {
                    s.image = g.body[suffix].clone();
                    s.color = tint;
                }
                s.flip_x = flip;
            }
            if let Ok((mut s, mut t)) = heads.get_mut(child)
                && let Some(graphics) = &graphics
            {
                s.image = graphics.head[suffix].clone();
                s.flip_x = flip;
                s.color = tint;
                let x = match pawn.rotation {
                    Rot4::East => 0.09,
                    Rot4::West => -0.09,
                    _ => 0.0,
                };
                t.translation.x = x * CELL_SIZE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_graphics_follow_the_game_rules() {
        let sc = Some("Graphic_StackCount");
        assert_eq!(stack_graphic_index(sc, 1, 40, 75), 0);
        assert_eq!(stack_graphic_index(sc, 3, 1, 75), 0);
        assert_eq!(stack_graphic_index(sc, 3, 40, 75), 1);
        assert_eq!(stack_graphic_index(sc, 3, 75, 75), 2);
        // Four or more: interpolate between the ends.
        assert_eq!(stack_graphic_index(sc, 5, 1, 100), 0);
        assert_eq!(stack_graphic_index(sc, 5, 100, 100), 4);
        assert_eq!(stack_graphic_index(sc, 5, 50, 100), 2);
        assert_eq!(stack_graphic_index(sc, 5, 99, 100), 3);
        // Meals: three textures per food kind; base game uses the first kind.
        let meal = Some("Graphic_MealVariants");
        assert_eq!(stack_graphic_index(meal, 9, 1, 10), 0);
        assert_eq!(stack_graphic_index(meal, 9, 5, 10), 1);
        assert_eq!(stack_graphic_index(meal, 9, 10, 10), 2);
        assert_eq!(stack_graphic_index(None, 9, 10, 10), 0);
    }

    #[test]
    fn link_bits() {
        let rocks = [Cell::new(1, 1), Cell::new(1, 2), Cell::new(2, 1)];
        let links = |c: Cell| rocks.contains(&c);
        assert_eq!(link_index(links, Cell::new(1, 1)), 1 | 2);
        assert_eq!(link_index(links, Cell::new(1, 2)), 4);
        assert_eq!(link_index(links, Cell::new(5, 5)), 0);
    }

    #[test]
    fn atlas_uvs_stay_inside_their_tile() {
        for i in 0..16 {
            let (min, max) = atlas_uv(i);
            assert!(min.x >= 0.0 && min.y >= 0.0 && max.x <= 1.0 && max.y <= 1.0);
            assert!((max.x - min.x) < 0.25 && (max.x - min.x) > 0.24);
        }
        // Index 0 is Unity's bottom-left tile = our bottom row.
        assert!(atlas_uv(0).0.y >= 0.75);
        assert!(atlas_uv(15).1.y <= 0.25);
    }
}
