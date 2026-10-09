//! The prototype's designation shortcuts and the map drawing of zones,
//! structures, plants and mine marks. The shortcuts select the same
//! designators the Architect does (`ui::designator`); the simulation owns
//! the resulting state.
//!
//! Keys: Z stockpile (again: dumping stockpile), P growing zone (again:
//! the selected growing zone's next crop), X remove zone, B build (B again
//! cycles the material, G the building, R the rotation), L floor, C cancel,
//! N mine, V deconstruct, J remove floor, H smooth, T cut, Y harvest,
//! Ctrl+F2 hunt. Over a stockpile: `[` / `]` lower/raise its priority, F cycles
//! its filter.

use bevy::prelude::*;
use rimworld_defs::{DefId, GameDefs, TerrainDef, ThingDef};
use rimworld_sim::map::{Buildable, ConstructStage};
use rimworld_sim::sim::{OrderDesignator, ZoneKind, ZoneRef};
use rimworld_sim::{Cell, Sim};

use std::collections::HashMap;

use crate::SimState;
use crate::graphics::GameTextures;
use crate::thing_graphics::{ThingTextures, atlas_rect, draw_color, look};
use crate::ui::designator::{Des, DesignatorManager};
use crate::view::{CELL_SIZE, cell_center, world_to_cell};

/// Filter presets cycled with F (the supported minimum of a filter UI).
const FILTERS: [&str; 4] = [
    "default",
    "no food",
    "food only",
    "dumping (corpses, chunks)",
];

/// Index of each zone's preset, kept by the front-end.
#[derive(Resource, Default)]
struct FilterPreset(std::collections::HashMap<usize, usize>);

/// Crops the growing-zone tool offers (potatoes first, the game's default).
const CROPS: [&str; 3] = ["Plant_Potato", "Plant_Rice", "Plant_Corn"];

/// Index into [`floors`] of the floor the floor tool plans.
#[derive(Resource, Default)]
pub struct FloorChoice(pub usize);

/// The floors the floor tool offers: TerrainDefs in the Floors architect
/// category that cost something to lay (Def data), wood first.
pub fn floors(defs: &GameDefs) -> Vec<DefId<TerrainDef>> {
    let mut v: Vec<DefId<TerrainDef>> = defs
        .terrain
        .iter()
        .filter(|(_, t)| {
            t.designation_category.as_deref() == Some("Floors") && !t.cost_list.is_empty()
        })
        .map(|(id, _)| id)
        .collect();
    v.sort_by_key(|&id| {
        (
            defs.terrain[id].def_name != "WoodPlankFloor",
            defs.terrain[id].def_name.clone(),
        )
    });
    v
}

#[derive(Component)]
struct PlantSprite;

/// Plant revision, tick and plant count the plant sprites were drawn for
/// (growth alone redraws at most every 250 ticks).
#[derive(Resource, Default)]
struct PlantRevision(Option<(u64, u64, usize)>);

/// The buildings the build tool offers (a prototype stand-in for the
/// architect menu).
const BUILDINGS: [&str; 22] = [
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

/// The building the build tool places, its material and rotation.
#[derive(Resource)]
pub struct BuildChoice {
    pub buildings: Vec<DefId<ThingDef>>,
    pub building_index: usize,
    pub stuffs: Vec<DefId<ThingDef>>,
    pub index: usize,
    pub rotation: rimworld_sim::Rot4,
}

impl BuildChoice {
    pub fn new(defs: &GameDefs) -> Self {
        let buildings = BUILDINGS.iter().filter_map(|n| defs.things.id(n)).collect();
        let mut c = Self {
            buildings,
            building_index: 0,
            stuffs: Vec::new(),
            index: 0,
            rotation: rimworld_sim::Rot4::North,
        };
        c.refresh_stuffs(defs);
        c
    }

    /// Every material the building accepts (Def data: `stuffCategories` /
    /// `stuffProps`), in the game's default order.
    fn refresh_stuffs(&mut self, defs: &GameDefs) {
        self.stuffs = self
            .building()
            .map(|b| {
                let def = &defs.things[b];
                let mut v: Vec<DefId<ThingDef>> = defs
                    .things
                    .iter()
                    .filter(|(_, d)| def.accepts_stuff(d))
                    .map(|(id, _)| id)
                    .collect();
                // `GenStuff.DefaultStuffFor`'s preference: the Def's
                // `defaultStuff`, then wood, steel, plasteel, granite
                // blocks, cloth, leather.
                let order = [
                    "WoodLog",
                    "Steel",
                    "Plasteel",
                    "BlocksGranite",
                    "Cloth",
                    "Leather_Plain",
                ];
                v.sort_by_key(|&id| {
                    let name = defs.things[id].def_name.as_str();
                    if def.default_stuff.as_deref() == Some(name) {
                        0
                    } else {
                        order.iter().position(|&o| o == name).map_or(99, |p| p + 1)
                    }
                });
                v
            })
            .unwrap_or_default();
        // A newly chosen building starts with its default material.
        self.index = 0;
    }

    pub fn building(&self) -> Option<DefId<ThingDef>> {
        self.buildings.get(self.building_index).copied()
    }

    pub fn stuff(&self) -> Option<DefId<ThingDef>> {
        self.stuffs.get(self.index).copied()
    }

    /// The next (or, `back`, the previous) building in the list.
    fn next_building(&mut self, defs: &GameDefs, back: bool) {
        let n = self.buildings.len();
        if n > 0 {
            self.building_index = if back {
                (self.building_index + n - 1) % n
            } else {
                (self.building_index + 1) % n
            };
            self.refresh_stuffs(defs);
        }
    }

    fn rotate(&mut self) {
        use rimworld_sim::Rot4::*;
        self.rotation = match self.rotation {
            North => East,
            East => South,
            South => West,
            West => North,
        };
    }
}

#[derive(Component)]
struct ZoneOverlay;

#[derive(Component)]
struct StructureSprite;

/// Structure revision the structure sprites were drawn for.
#[derive(Resource, Default)]
struct StructureRevision(Option<u64>);

/// Storage revision the overlay was drawn for.
#[derive(Resource, Default)]
struct OverlayRevision(Option<u64>);

pub struct DesignatePlugin;

impl Plugin for DesignatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FilterPreset>()
            .init_resource::<OverlayRevision>()
            .init_resource::<StructureRevision>()
            .init_resource::<FloorChoice>()
            .init_resource::<PlantRevision>()
            .init_resource::<ThingTextures>()
            .add_systems(Startup, init_build_choice)
            .add_systems(
                Update,
                (legacy_keys, sync_zone_overlay, sync_structures, sync_plants)
                    .chain()
                    .after(crate::ui::UiSet::End)
                    .before(crate::ui::designator::DesignatorSet),
            );
    }
}

fn init_build_choice(mut commands: Commands, sim: Res<SimState>) {
    commands.insert_resource(BuildChoice::new(&sim.0.defs));
}

/// Applies filter preset `i` to a zone: the game's default and dumping
/// stockpile presets, and the default without or with only food.
fn apply_filter(sim: &mut Sim, zone: usize, i: usize) {
    use rimworld_sim::storage::{StoragePreset, ThingFilter};
    let defs = sim.defs.clone();
    let filter = match i {
        1 => {
            let mut f = ThingFilter::preset(&defs, StoragePreset::DefaultStockpile);
            for d in defs.things_in_category("Foods") {
                f.set(d, false);
            }
            f
        }
        2 => ThingFilter::category(&defs, "Foods"),
        3 => ThingFilter::preset(&defs, StoragePreset::DumpingStockpile),
        _ => ThingFilter::preset(&defs, StoragePreset::DefaultStockpile),
    };
    sim.map.storage.set_filter(zone, filter);
}

/// The old shortcuts, routed to the designators and settings the native
/// UI uses. Keys a drawn gizmo used this frame never reach here.
#[allow(clippy::too_many_arguments)]
fn legacy_keys(
    keys: Res<ButtonInput<KeyCode>>,
    camera: Single<(&Camera, &GlobalTransform)>,
    window: Single<&Window>,
    mut sim: ResMut<SimState>,
    mut mgr: ResMut<DesignatorManager>,
    mut presets: ResMut<FilterPreset>,
    mut build: ResMut<BuildChoice>,
    mut floor: ResMut<FloorChoice>,
    lang: Res<crate::ui::Lang>,
    sel: Res<crate::ui::select::Selection>,
    cursor: Res<crate::camera::ScriptCursor>,
) {
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let sel_des = |mgr: &mut DesignatorManager, sim: &Sim, d: Des| {
        if mgr.selected != Some(d) {
            mgr.select(d, sim, &lang);
        }
    };
    let orders = [
        (KeyCode::KeyC, OrderDesignator::Cancel),
        (KeyCode::KeyN, OrderDesignator::Mine),
        (KeyCode::KeyV, OrderDesignator::Deconstruct),
        (KeyCode::KeyJ, OrderDesignator::RemoveFloor),
        (KeyCode::KeyH, OrderDesignator::SmoothSurface),
        (KeyCode::KeyY, OrderDesignator::Harvest),
        (KeyCode::KeyT, OrderDesignator::CutPlants),
        (KeyCode::F2, OrderDesignator::Hunt),
    ];
    let ctrl = crate::interact::ctrl_held(&keys);
    for (k, d) in orders {
        // F2 alone opens the Schedule tab.
        if keys.just_pressed(k) && (k != KeyCode::F2 || ctrl) {
            sel_des(&mut mgr, &sim.0, Des::Order(d));
        }
    }
    if keys.just_pressed(KeyCode::KeyZ) {
        let d = if mgr.selected == Some(Des::Zone(ZoneKind::Stockpile)) {
            Des::Zone(ZoneKind::DumpingStockpile)
        } else {
            Des::Zone(ZoneKind::Stockpile)
        };
        sel_des(&mut mgr, &sim.0, d);
    }
    if keys.just_pressed(KeyCode::KeyX) {
        sel_des(&mut mgr, &sim.0, Des::ZoneDelete);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        // P again: the selected growing zone's next crop.
        if mgr.selected == Some(Des::Zone(ZoneKind::Growing))
            && let Some(ZoneRef::Growing(g)) = sel.selected_zone()
        {
            let crops: Vec<DefId<ThingDef>> = CROPS
                .iter()
                .filter_map(|n| sim.0.defs.things.id(n))
                .collect();
            if !crops.is_empty() {
                let cur = sim.0.map.growing_zone(g).plant;
                let i = crops
                    .iter()
                    .position(|&c| c == cur)
                    .unwrap_or(crops.len() - 1);
                let next = crops[(i + 1) % crops.len()];
                sim.0.map.growing_zone_mut(g).plant = next;
            }
        }
        sel_des(&mut mgr, &sim.0, Des::Zone(ZoneKind::Growing));
    }
    if keys.just_pressed(KeyCode::KeyL) {
        let list = floors(&sim.0.defs);
        if !list.is_empty() {
            if matches!(mgr.selected, Some(Des::Floor(_))) {
                floor.0 += 1;
            }
            sel_des(&mut mgr, &sim.0, Des::Floor(list[floor.0 % list.len()]));
        }
    }
    let building = matches!(mgr.selected, Some(Des::Build(_)));
    let mut rebuild = false;
    if keys.just_pressed(KeyCode::KeyB) {
        if building && !build.stuffs.is_empty() {
            build.index = (build.index + 1) % build.stuffs.len();
        }
        rebuild = true;
    }
    if building && keys.just_pressed(KeyCode::KeyG) {
        let defs = sim.0.defs.clone();
        build.next_building(&defs, shift);
        rebuild = true;
    }
    if rebuild && let Some(b) = build.building() {
        sel_des(&mut mgr, &sim.0, Des::Build(b));
        if let Some(st) = build.stuff() {
            mgr.set_stuff(b, st);
        }
        mgr.rot = build.rotation;
    }
    if building && keys.just_pressed(KeyCode::KeyR) {
        build.rotate();
        mgr.rot = build.rotation;
    }
    let (camera, cam_transform) = *camera;
    let to_cell = |p: Vec2| {
        camera
            .viewport_to_world_2d(cam_transform, p)
            .ok()
            .map(world_to_cell)
    };
    // Zone settings for the hovered stockpile.
    let hovered = cursor
        .get(&window)
        .and_then(to_cell)
        .and_then(|c| sim.0.map.storage.zone_at(c));
    if let Some(z) = hovered {
        let storage = &mut sim.0.map.storage;
        let p = storage.zone(z).priority;
        if keys.just_pressed(KeyCode::BracketRight) {
            storage.set_priority(z, p.raised());
        }
        if keys.just_pressed(KeyCode::BracketLeft) {
            storage.set_priority(z, p.lowered());
        }
        if keys.just_pressed(KeyCode::KeyF) {
            let i = (presets.0.get(&z).copied().unwrap_or(0) + 1) % FILTERS.len();
            presets.0.insert(z, i);
            apply_filter(&mut sim.0, z, i);
        }
    }
    // U: mark the hovered building's switch to be flicked.
    if keys.just_pressed(KeyCode::KeyU)
        && let Some(c) = cursor.get(&window).and_then(to_cell)
        && let Some(st) = sim
            .0
            .map
            .structures()
            .iter()
            .find(|st| st.footprint.contains(c) && sim.0.defs.things[st.def].flickable)
    {
        let id = st.id;
        sim.0.toggle_switch(id);
    }
    // Bills on the hovered work table: M adds the next available recipe,
    // `.` cycles the last bill's repeat mode, `,` deletes the last bill.
    if let Some(table) = cursor
        .get(&window)
        .and_then(to_cell)
        .and_then(|c| hovered_work_table(&sim.0, c))
    {
        if keys.just_pressed(KeyCode::KeyM) {
            let recipes: Vec<_> = rimworld_sim::bills::all_recipes(
                &sim.0.defs,
                sim.0.map.structure(table).map(|s| s.def).unwrap(),
            )
            .into_iter()
            .filter(|&r| sim.0.recipe_available(r))
            .collect();
            let last = sim.0.bills(table).last().map(|b| b.recipe);
            let next = match last.and_then(|l| recipes.iter().position(|&r| r == l)) {
                Some(i) => recipes.get((i + 1) % recipes.len().max(1)).copied(),
                None => recipes.first().copied(),
            };
            if let Some(r) = next {
                sim.0.add_bill(table, r);
            }
        }
        if keys.just_pressed(KeyCode::Period)
            && let Some(b) = sim.0.bills(table).last()
        {
            use rimworld_sim::bills::RepeatMode;
            let id = b.id;
            sim.0
                .edit_bill(table, id, |b| match (b.repeat_mode, b.repeat_count) {
                    (RepeatMode::RepeatCount, n) if n < 5 => b.repeat_count = 5,
                    (RepeatMode::RepeatCount, _) => b.repeat_mode = RepeatMode::Forever,
                    (RepeatMode::Forever, _) => {
                        b.repeat_mode = RepeatMode::TargetCount;
                        b.target_count = 10;
                    }
                    (RepeatMode::TargetCount, _) => {
                        b.repeat_mode = RepeatMode::RepeatCount;
                        b.repeat_count = 1;
                    }
                });
        }
        if keys.just_pressed(KeyCode::Comma)
            && let Some(b) = sim.0.bills(table).last()
        {
            let id = b.id;
            sim.0.delete_bill(table, id);
        }
    }
    // I: choose the next research project that can start now.
    if keys.just_pressed(KeyCode::KeyI) {
        let next = next_research_project(&sim.0, shift);
        sim.0.set_research_project(next.as_deref());
    }
    // O: forbid the things on the hovered cell, or allow them if any is
    // forbidden (the forbid gizmo).
    if keys.just_pressed(KeyCode::KeyO)
        && let Some(c) = cursor.get(&window).and_then(to_cell)
        && sim.0.map.size().contains(c)
    {
        let things: Vec<_> = sim
            .0
            .map
            .items_at(c)
            .filter(|i| !i.is_filth())
            .map(|i| (i.id, i.forbidden))
            .collect();
        let forbid = !things.iter().any(|&(_, f)| f);
        let defs = sim.0.defs.clone();
        for (id, _) in things {
            sim.0.map.set_forbidden(&defs, id, forbid);
        }
    }
}

/// Redraws zone cells when the storage changed.
// COMPATIBILITY TODO: currently approximate — the game draws zones with
// their own colour and edge lines.
fn sync_zone_overlay(
    mut commands: Commands,
    sim: Res<SimState>,
    mut drawn: ResMut<OverlayRevision>,
    old: Query<Entity, With<ZoneOverlay>>,
) {
    let storage = &sim.0.map.storage;
    if drawn.0 == Some(storage.revision) {
        return;
    }
    drawn.0 = Some(storage.revision);
    for e in &old {
        commands.entity(e).despawn();
    }
    // `Zone.Material`: each zone's own colour (`ZoneColorUtility`, 9%
    // opacity), over whole cells.
    for z in storage.live_zones() {
        let color = zone_color(sim.0.zone_color(ZoneRef::Stockpile(z)));
        for &c in &storage.zone(z).cells {
            commands.spawn((
                ZoneOverlay,
                Sprite {
                    color,
                    custom_size: Some(Vec2::splat(CELL_SIZE)),
                    ..default()
                },
                Transform::from_translation(cell_center(c).extend(3.0)),
            ));
        }
    }
}

/// A zone's colour; zones made without one (old saves) get the first of
/// the storage palette.
fn zone_color(c: Option<[f32; 4]>) -> Color {
    let [r, g, b, a] = c.unwrap_or([0.75, 0.25, 0.25, 0.09]);
    Color::srgba(r, g, b, a)
}

/// Draws growing zones and plants when they changed: zone cells as a
/// tint, plants as green squares growing with their growth (yellow when
/// harvestable).
// COMPATIBILITY TODO: currently approximate — one textured sprite per
// plant (the game draws several meshes per cell for small plants, with
// random offsets), no immature or leafless graphics.
#[allow(clippy::too_many_arguments)]
fn sync_plants(
    mut commands: Commands,
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut tt: ResMut<ThingTextures>,
    mut images: ResMut<Assets<Image>>,
    mut drawn: ResMut<PlantRevision>,
    old: Query<Entity, With<PlantSprite>>,
) {
    let sim = &sim.0;
    let rev = sim.map.plant_revision;
    let (tick, count) = (sim.tick_count(), sim.map.plants().len());
    if let Some((r, t, n)) = drawn.0
        && (r == rev || (n == count && tick < t + 250 && sim.map.cut_designations().is_empty()))
    {
        return;
    }
    drawn.0 = Some((rev, tick, count));
    for e in &old {
        commands.entity(e).despawn();
    }
    for z in sim.map.growing_zones() {
        let color = sim
            .zone_color(ZoneRef::Growing(z))
            .map_or(Color::srgba(0.25, 0.75, 0.25, 0.09), |[r, g, b, a]| {
                Color::srgba(r, g, b, a)
            });
        for &c in &sim.map.growing_zone(z).cells {
            commands.spawn((
                PlantSprite,
                Sprite {
                    color,
                    custom_size: Some(Vec2::splat(CELL_SIZE)),
                    ..default()
                },
                Transform::from_translation(cell_center(c).extend(3.0)),
            ));
        }
    }
    let designated = sim.map.cut_designations();
    let harvest = sim.map.harvest_designations();
    let lib = textures.0.as_ref();
    for p in sim.map.plants() {
        let def = &sim.defs.things[p.def];
        let marked = designated.contains(&p.id) || harvest.contains(&p.id);
        let tree = def.passability != rimworld_defs::Passability::Standable;
        let growth = p.growth.clamp(0.0, 1.0);
        let textured = lib.and_then(|lib| {
            let lk = look(
                &mut tt,
                lib,
                def,
                None,
                rimworld_sim::Rot4::North,
                p.id_number,
            )?;
            let tex = tt.get(lib, &lk.path, None, &mut images)?;
            Some((lk, tex))
        });
        match textured {
            Some((lk, tex)) => {
                let (lo, hi) = def
                    .plant
                    .as_ref()
                    .map_or((0.9, 1.1), |pp| pp.visual_size_range);
                let scale = lo + (hi - lo) * growth;
                let size = lk.size * scale * CELL_SIZE;
                // Plants stand on their cell: taller ones rise above it.
                let lift = (size.y - CELL_SIZE).max(0.0) * 0.35;
                let color = Color::WHITE;
                let z = if tree { 6.0 } else { 4.5 };
                commands.spawn((
                    PlantSprite,
                    Sprite {
                        image: tex.image,
                        color,
                        custom_size: Some(size),
                        flip_x: p.id_number % 2 == 1,
                        ..default()
                    },
                    Transform::from_translation(
                        (cell_center(p.position) + Vec2::new(0.0, lift)).extend(z),
                    ),
                ));
            }
            None => {
                let ready = p.sown && rimworld_sim::sim::plant_ready(sim, p.position);
                let color = if marked {
                    Color::srgb(0.85, 0.3, 0.25)
                } else if ready {
                    Color::srgb(0.85, 0.75, 0.2)
                } else if tree {
                    Color::srgb(0.1, 0.35, 0.12)
                } else if p.sown {
                    Color::srgb(0.2, 0.6, 0.2)
                } else {
                    Color::srgba(0.35, 0.6, 0.25, 0.6)
                };
                let base = if tree { 0.45 } else { 0.2 };
                let size = base + (0.8 - base) * growth;
                commands.spawn((
                    PlantSprite,
                    Sprite {
                        color,
                        custom_size: Some(Vec2::splat(CELL_SIZE * size)),
                        ..default()
                    },
                    Transform::from_translation(cell_center(p.position).extend(4.5)),
                ));
            }
        }
    }
}

/// Colour of a thing made of `stuff` (`stuffProps.color`), else the def's.
fn structure_color(sim: &Sim, building: DefId<ThingDef>, stuff: Option<DefId<ThingDef>>) -> Color {
    let defs = &sim.defs;
    let c = stuff
        .and_then(|s| defs.things[s].stuff_props.as_ref().and_then(|p| p.color))
        .or(defs.things[building].color);
    c.map_or(Color::srgb(0.5, 0.5, 0.5), |c| Color::srgb(c.r, c.g, c.b))
}

/// The textured sprite for a building (or its blueprint/frame), if its
/// texture loads: atlas tile for linked graphics, else the whole texture
/// at its draw size, facing and stuff colour.
#[allow(clippy::too_many_arguments)]
fn building_sprite(
    sim: &Sim,
    lib: &rimworld_assets::unity::TextureLibrary,
    tt: &mut ThingTextures,
    images: &mut Assets<Image>,
    links: &HashMap<Cell, Vec<String>>,
    building: DefId<ThingDef>,
    stuff: Option<DefId<ThingDef>>,
    fp: &rimworld_sim::geom::Footprint,
    id_number: i32,
    tint: Option<Color>,
) -> Option<(Sprite, Transform)> {
    let defs = &sim.defs;
    let def = &defs.things[building];
    let stuff_def = stuff.map(|s| &defs.things[s]);
    let lk = look(tt, lib, def, stuff_def, fp.rot, id_number)?;
    let color = draw_color(def, stuff_def);
    let tex = tt.get(lib, &lk.path, lk.masked.then_some(color), images)?;
    let sprite_color = tint.unwrap_or(if lk.masked { Color::WHITE } else { color });
    let r = fp.rect();
    let center = Vec2::new(
        (r.min_x + r.max_x + 1) as f32 * 0.5,
        (r.min_z + r.max_z + 1) as f32 * 0.5,
    ) * CELL_SIZE;
    if lk.linked {
        let flags = def
            .graphic
            .as_ref()
            .map(|g| g.link_flags.clone())
            .unwrap_or_default();
        let size = sim.map.size();
        let link = |c: Cell| {
            if !size.contains(c) {
                return flags.iter().any(|f| f == "MapEdge");
            }
            links
                .get(&c)
                .is_some_and(|fs| fs.iter().any(|f| flags.contains(f)))
        };
        let index = crate::graphics::link_index(link, fp.center);
        return Some((
            Sprite {
                image: tex.image,
                rect: Some(atlas_rect(tex.size, index)),
                color: sprite_color,
                custom_size: Some(Vec2::splat(CELL_SIZE)),
                ..default()
            },
            Transform::from_translation(cell_center(fp.center).extend(0.0)),
        ));
    }
    Some((
        Sprite {
            image: tex.image,
            color: sprite_color,
            custom_size: Some(lk.size * CELL_SIZE),
            flip_x: lk.flip_x,
            ..default()
        },
        Transform::from_translation(center.extend(0.0))
            .with_rotation(Quat::from_rotation_z(-lk.angle)),
    ))
}

/// Draws blueprints, frames and buildings when they changed: textured from
/// the install when possible, else flat squares in the stuff colour.
// COMPATIBILITY TODO: currently approximate — blueprints and frames are
// the building's texture tinted (not the game's blueprint/frame
// graphics), door leaves don't slide, and corner fillers of linked walls
// are not drawn.
#[allow(clippy::too_many_arguments)]
fn sync_structures(
    mut commands: Commands,
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut tt: ResMut<ThingTextures>,
    mut images: ResMut<Assets<Image>>,
    mut drawn: ResMut<StructureRevision>,
    old: Query<Entity, With<StructureSprite>>,
) {
    let sim = &sim.0;
    let rev = sim.map.structure_revision();
    if drawn.0 == Some(rev) {
        return;
    }
    drawn.0 = Some(rev);
    for e in &old {
        commands.entity(e).despawn();
    }
    let square = |color: Color, size: f32, c: Cell, z: f32| {
        (
            StructureSprite,
            Sprite {
                color,
                custom_size: Some(Vec2::splat(CELL_SIZE * size)),
                ..default()
            },
            Transform::from_translation(cell_center(c).extend(z)),
        )
    };
    let defs = &sim.defs;
    // Link flags per cell: buildings' `linkFlags`, natural rock as Rock.
    let mut links: HashMap<Cell, Vec<String>> = HashMap::new();
    for st in sim.map.structures() {
        if let Some(g) = &defs.things[st.def].graphic {
            for c in st.footprint.cells() {
                links
                    .entry(c)
                    .or_default()
                    .extend(g.link_flags.iter().cloned());
            }
        }
    }
    for (c, b) in sim.map.buildings.iter() {
        if let Some(b) = b
            && defs.things[*b]
                .building
                .as_ref()
                .is_some_and(|x| x.is_natural_rock)
        {
            links.entry(c).or_default().push("Rock".to_owned());
        }
    }
    let lib = textures.0.as_ref();
    for st in sim.map.structures() {
        let def = &defs.things[st.def];
        if def.building.as_ref().is_some_and(|b| b.is_natural_rock) {
            continue;
        }
        let z = if st.edifice { 2.5 } else { 2.4 };
        let sprite = lib.and_then(|lib| {
            building_sprite(
                sim,
                lib,
                &mut tt,
                &mut images,
                &links,
                st.def,
                st.stuff,
                &st.footprint,
                st.id_number,
                None,
            )
        });
        match sprite {
            Some((sprite, mut t)) => {
                t.translation.z = z;
                commands.spawn((StructureSprite, sprite, t));
            }
            None if st.edifice => {
                let size = if def.is_door() {
                    0.7
                } else if def.fill_percent > 0.99 {
                    1.0
                } else {
                    0.8
                };
                for c in st.footprint.cells() {
                    commands.spawn(square(structure_color(sim, st.def, st.stuff), size, c, z));
                }
            }
            None => {
                commands.spawn(square(
                    Color::srgb(0.35, 0.33, 0.25),
                    0.3,
                    st.footprint.center,
                    z,
                ));
            }
        }
    }
    for k in sim.map.constructibles() {
        let tint = match k.stage {
            ConstructStage::Blueprint => Color::srgba(0.45, 0.65, 1.0, 0.5),
            ConstructStage::Frame => match k.building {
                Buildable::Thing(b) => structure_color(sim, b, k.stuff).with_alpha(0.65),
                Buildable::Floor(f) => {
                    let [r, g, b, _] = crate::view::terrain_color(&sim.defs.terrain[f]);
                    Color::srgba_u8(r, g, b, 150)
                }
            },
        };
        let textured = match (k.building, lib) {
            (Buildable::Thing(b), Some(lib)) => building_sprite(
                sim,
                lib,
                &mut tt,
                &mut images,
                &links,
                b,
                k.stuff,
                &k.footprint(),
                0,
                Some(tint),
            ),
            _ => None,
        };
        match textured {
            Some((sprite, mut t)) => {
                t.translation.z = 4.0;
                commands.spawn((StructureSprite, sprite, t));
            }
            None => {
                let size = match k.stage {
                    ConstructStage::Blueprint => 0.9,
                    ConstructStage::Frame => 0.75,
                };
                for c in k.footprint().cells() {
                    commands.spawn(square(tint, size, c, 4.0));
                }
            }
        }
    }
}

/// The work table (a building with recipes) on cell `c`.
fn hovered_work_table(sim: &Sim, c: Cell) -> Option<rimworld_sim::map::ItemId> {
    sim.map
        .structures()
        .iter()
        .find(|s| {
            s.footprint.contains(c)
                && !rimworld_sim::bills::all_recipes(&sim.defs, s.def).is_empty()
        })
        .map(|s| s.id)
}

/// The project after the current one among those that can start now,
/// ordered by their place in the research tree; `None` after the last.
/// Projects that can start now, in research-tree order.
pub fn startable_projects(sim: &Sim) -> Vec<&rimworld_defs::ResearchProjectDef> {
    let mut list: Vec<&rimworld_defs::ResearchProjectDef> = sim
        .defs
        .research
        .iter()
        .map(|(_, p)| p)
        .filter(|p| sim.can_start_research(&p.def_name))
        .collect();
    list.sort_by(|a, b| {
        a.view
            .0
            .total_cmp(&b.view.0)
            .then(a.view.1.total_cmp(&b.view.1))
    });
    list
}

/// The next (or, `back`, previous) startable project after the current
/// one; none past either end.
fn next_research_project(sim: &Sim, back: bool) -> Option<String> {
    let list = startable_projects(sim);
    let at = sim
        .current_research()
        .and_then(|c| list.iter().position(|p| p.def_name == c));
    match (at, back) {
        (None, false) => list.first(),
        (None, true) => list.last(),
        (Some(i), false) => list.get(i + 1),
        (Some(i), true) => i.checked_sub(1).and_then(|i| list.get(i)),
    }
    .map(|p| p.def_name.clone())
}
