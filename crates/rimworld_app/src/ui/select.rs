//! Map selection as the game's `Selector` does it (docs/research.md §66):
//! a click selects what is under the cursor (pawns nearest first, then the
//! cell's things from the highest drawn, then pawns a little farther, then
//! the zone), clicking again cycles through them, Shift adds and removes,
//! empty ground clears; a box selects colonists first, else humanlikes,
//! resources, other pawns, any selectable thing, then zones. Selected
//! things get the game's corner brackets.
//!
//! Selection is front-end state only; the simulation never sees it.

use std::collections::HashMap;

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_sim::sim::{ThingRef, ZoneRef};
use rimworld_sim::{Cell, PawnId, Sim};

use crate::SimState;
use crate::camera::PointerClicks;
use crate::graphics::GameTextures;
use crate::thing_graphics::ThingTextures;
use crate::view::{CELL_SIZE, cell_center, sim_to_world, world_to_cell};

/// Something the player can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Selectable {
    Thing(ThingRef),
    Zone(ZoneRef),
}

/// The selection (`Selector.selected`), in selection order.
#[derive(Resource, Default)]
pub struct Selection {
    pub objects: Vec<Selectable>,
    /// When each was selected (the brackets' little jump).
    times: HashMap<Selectable, f32>,
}

/// `Selector.MaxNumSelected`.
const MAX_SELECTED: usize = 200;

impl Selection {
    pub fn pawns(&self) -> Vec<PawnId> {
        self.objects
            .iter()
            .filter_map(|o| match o {
                Selectable::Thing(ThingRef::Pawn(p)) => Some(*p),
                _ => None,
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn contains(&self, o: Selectable) -> bool {
        self.objects.contains(&o)
    }

    pub fn clear(&mut self) {
        self.objects.clear();
        self.times.clear();
    }

    /// `Selector.Select`: zones and things are not selected together.
    pub fn select(&mut self, o: Selectable, now: f32) {
        let is_zone = matches!(o, Selectable::Zone(_));
        let have_zone = matches!(self.objects.first(), Some(Selectable::Zone(_)));
        if have_zone != is_zone || is_zone {
            self.clear();
        }
        if self.objects.len() < MAX_SELECTED && !self.objects.contains(&o) {
            self.objects.push(o);
            self.times.insert(o, now);
        }
    }

    pub fn deselect(&mut self, o: Selectable) {
        self.objects.retain(|&x| x != o);
        self.times.remove(&o);
    }

    /// Replaces the selection with pawns (scripts, the old shortcuts).
    pub fn set_pawns(&mut self, pawns: &[PawnId], now: f32) {
        self.clear();
        for &p in pawns {
            self.select(Selectable::Thing(ThingRef::Pawn(p)), now);
        }
    }

    pub fn selected_zone(&self) -> Option<ZoneRef> {
        match self.objects.first() {
            Some(Selectable::Zone(z)) => Some(*z),
            _ => None,
        }
    }

    /// Drops what no longer exists (destroyed things, emptied zones).
    pub fn prune(&mut self, sim: &Sim) {
        self.objects.retain(|o| exists(sim, *o));
    }
}

/// Whether a selectable is still on the map.
pub fn exists(sim: &Sim, o: Selectable) -> bool {
    match o {
        Selectable::Thing(ThingRef::Pawn(p)) => sim.pawn(p).is_some_and(|p| !p.health.dead),
        Selectable::Thing(ThingRef::Rock(c)) => sim.map.buildings[c].is_some(),
        Selectable::Thing(ThingRef::Item(id)) => sim.thing_footprint(id).is_some(),
        Selectable::Zone(z) => {
            let alive = match z {
                ZoneRef::Stockpile(id) => sim.map.storage.live_zones().any(|x| x == id),
                ZoneRef::Growing(g) => sim.map.growing_zones().any(|x| x == g),
            };
            alive && !sim.zone_cells(z).is_empty()
        }
    }
}

/// `AltitudeLayer` order of a thing (higher is drawn above).
fn altitude(sim: &Sim, t: ThingRef) -> u8 {
    const LAYERS: [&str; 27] = [
        "BelowTerrain",
        "TerrainEdges",
        "Terrain",
        "TerrainScatter",
        "Floor",
        "Conduits",
        "FloorCoverings",
        "FloorEmplacement",
        "Filth",
        "Zone",
        "SmallWire",
        "LowPlant",
        "MoteLow",
        "Shadows",
        "DoorMoveable",
        "Building",
        "BuildingBelowTop",
        "BuildingOnTop",
        "Item",
        "ItemImportant",
        "LayingPawn",
        "PawnRope",
        "Projectile",
        "Pawn",
        "PawnUnused",
        "PawnState",
        "Blueprint",
    ];
    let of = |d: rimworld_defs::DefId<rimworld_defs::ThingDef>, fallback: &str| {
        let name = sim.defs.things[d]
            .altitude_layer
            .as_deref()
            .unwrap_or(fallback);
        LAYERS.iter().position(|&l| l == name).unwrap_or(15) as u8
    };
    match t {
        ThingRef::Pawn(_) => 23,
        ThingRef::Rock(c) => sim.map.buildings[c].map_or(15, |b| of(b, "Building")),
        ThingRef::Item(id) => {
            if let Some(it) = sim.map.item(id) {
                of(it.def, "Item")
            } else if let Some(s) = sim.map.structure(id) {
                of(s.def, "Building")
            } else if let Some(k) = sim.map.constructible(id) {
                match k.stage {
                    rimworld_sim::map::ConstructStage::Blueprint => 26,
                    rimworld_sim::map::ConstructStage::Frame => 15,
                }
            } else if let Some(p) = sim.map.plant(id) {
                of(p.def, "LowPlant")
            } else {
                0
            }
        }
    }
}

/// `ThingDef.selectable` for the thing.
fn selectable(sim: &Sim, t: ThingRef) -> bool {
    let def = match t {
        ThingRef::Pawn(_) => return true,
        ThingRef::Rock(c) => sim.map.buildings[c],
        ThingRef::Item(id) => sim
            .map
            .item(id)
            .map(|i| i.def)
            .or_else(|| sim.map.structure(id).map(|s| s.def))
            .or_else(|| sim.map.plant(id).map(|p| p.def)),
    };
    // Blueprints and frames are selectable (their implied defs are).
    def.is_none_or(|d| sim.defs.things[d].selectable)
        && !matches!(t, ThingRef::Item(id) if sim.map.item(id).is_some_and(|i| i.is_filth()))
}

/// Where a thing is drawn (world units).
pub fn draw_pos(sim: &Sim, t: ThingRef) -> Option<Vec2> {
    match t {
        ThingRef::Pawn(p) => sim.pawn(p).map(|p| sim_to_world(p.visual_position())),
        ThingRef::Rock(c) => Some(cell_center(c)),
        ThingRef::Item(id) => {
            let fp = sim.thing_footprint(id)?;
            let r = fp.rect();
            Some(
                Vec2::new(
                    (r.min_x + r.max_x + 1) as f32 * 0.5,
                    (r.min_z + r.max_z + 1) as f32 * 0.5,
                ) * CELL_SIZE,
            )
        }
    }
}

/// A thing's size on the map in cells (`RotatedSize`).
fn rotated_size(sim: &Sim, t: ThingRef) -> Vec2 {
    match t {
        ThingRef::Item(id) => sim.thing_footprint(id).map_or(Vec2::ONE, |fp| {
            let r = fp.rect();
            Vec2::new(
                (r.max_x - r.min_x + 1) as f32,
                (r.max_z - r.min_z + 1) as f32,
            )
        }),
        _ => Vec2::ONE,
    }
}

/// `SelectableObjectsUnderMouse`: pawns within 0.4 cells (nearest
/// first), the cell's things (highest first; items nearest first), pawns
/// within a cell, the zone.
pub fn objects_under(sim: &Sim, world: Vec2) -> Vec<Selectable> {
    let cell = world_to_cell(world);
    if !sim.map.size().contains(cell) {
        return Vec::new();
    }
    let dist = |t: ThingRef| draw_pos(sim, t).map_or(f32::MAX, |p| p.distance(world) / CELL_SIZE);
    let alive_pawns: Vec<PawnId> = sim
        .pawns()
        .iter()
        .filter(|p| !p.health.dead)
        .map(|p| p.id)
        .collect();
    let mut near: Vec<ThingRef> = alive_pawns
        .iter()
        .map(|&p| ThingRef::Pawn(p))
        .filter(|&t| dist(t) < 0.4)
        .collect();
    near.sort_by(|&a, &b| dist(a).total_cmp(&dist(b)));
    let mut cell_things: Vec<ThingRef> = sim
        .things_at(cell)
        .into_iter()
        .filter(|t| !matches!(t, ThingRef::Pawn(_)) && selectable(sim, *t))
        .collect();
    cell_things.sort_by(|&a, &b| {
        let items = |t: ThingRef| matches!(t, ThingRef::Item(id) if sim.map.item(id).is_some());
        if items(a) && items(b) {
            dist(a).total_cmp(&dist(b))
        } else {
            altitude(sim, b).cmp(&altitude(sim, a))
        }
    });
    near.extend(cell_things);
    let mut wide: Vec<ThingRef> = alive_pawns
        .iter()
        .map(|&p| ThingRef::Pawn(p))
        .filter(|&t| dist(t) < 1.0 && !near.contains(&t))
        .collect();
    wide.sort_by(|&a, &b| dist(a).total_cmp(&dist(b)));
    near.extend(wide);
    let mut out: Vec<Selectable> = near.into_iter().map(Selectable::Thing).collect();
    if let Some(z) = sim.zone_at(cell) {
        out.push(Selectable::Zone(z));
    }
    out
}

/// `Selector.SelectUnderMouse`.
pub fn select_under(sel: &mut Selection, sim: &Sim, world: Vec2, shift: bool, now: f32) {
    let list = objects_under(sim, world);
    match list.len() {
        0 => {
            if !shift {
                sel.clear();
            }
        }
        1 => {
            let o = list[0];
            if !shift {
                sel.clear();
                sel.select(o, now);
            } else if !sel.contains(o) {
                sel.select(o, now);
            } else {
                sel.deselect(o);
            }
        }
        _ => match list.iter().position(|&o| sel.contains(o)) {
            Some(i) => {
                if shift {
                    for &o in &list {
                        sel.deselect(o);
                    }
                    return;
                }
                // Cycle to the next one under the cursor.
                let next = list[(i + 1) % list.len()];
                sel.clear();
                sel.select(next, now);
            }
            None => {
                if !shift {
                    sel.clear();
                }
                sel.select(list[0], now);
            }
        },
    }
}

/// `Selector.SelectInsideDragBox` over a world rectangle.
pub fn select_in_box(sel: &mut Selection, sim: &Sim, a: Vec2, b: Vec2, shift: bool, now: f32) {
    if !shift {
        sel.clear();
    }
    let (min, max) = (a.min(b), a.max(b));
    let inside = |p: Vec2| p.x >= min.x && p.x <= max.x && p.y >= min.y && p.y <= max.y;
    // Things whose draw position lies in the box.
    let mut things: Vec<ThingRef> = Vec::new();
    for p in sim.pawns().iter().filter(|p| !p.health.dead) {
        if inside(sim_to_world(p.visual_position())) {
            things.push(ThingRef::Pawn(p.id));
        }
    }
    let (c0, c1) = (world_to_cell(min), world_to_cell(max));
    for z in c0.z.max(0)..=c1.z.min(sim.map.size().height - 1) {
        for x in c0.x.max(0)..=c1.x.min(sim.map.size().width - 1) {
            for t in sim.things_at(Cell::new(x, z)) {
                if !matches!(t, ThingRef::Pawn(_))
                    && !things.contains(&t)
                    && selectable(sim, t)
                    && draw_pos(sim, t).is_some_and(inside)
                {
                    things.push(t);
                }
            }
        }
    }
    let colonist = |t: &ThingRef| matches!(t, ThingRef::Pawn(p) if sim.pawn(*p).is_some_and(|p| p.is_colonist));
    let humanlike = |t: &ThingRef| matches!(t, ThingRef::Pawn(p) if !sim.is_animal(*p));
    let resource = |t: &ThingRef| matches!(t, ThingRef::Item(id) if sim.map.item(*id).is_some_and(|i| sim.defs.things[i.def].count_as_resource));
    let pawn = |t: &ThingRef| matches!(t, ThingRef::Pawn(_));
    // Colonists in colonist bar order (spawn order here).
    for filter in [
        &colonist as &dyn Fn(&ThingRef) -> bool,
        &humanlike,
        &resource,
        &pawn,
        &|_| true,
    ] {
        let picked: Vec<ThingRef> = things.iter().copied().filter(|t| filter(t)).collect();
        if !picked.is_empty() {
            for t in picked {
                sel.select(Selectable::Thing(t), now);
            }
            return;
        }
    }
    // Zones in the box.
    let mut zones: Vec<ZoneRef> = Vec::new();
    for z in c0.z.max(0)..=c1.z.min(sim.map.size().height - 1) {
        for x in c0.x.max(0)..=c1.x.min(sim.map.size().width - 1) {
            if let Some(zr) = sim.zone_at(Cell::new(x, z))
                && !zones.contains(&zr)
            {
                zones.push(zr);
            }
        }
    }
    if zones.is_empty() {
        select_under(sel, sim, (a + b) * 0.5, shift, now);
    } else {
        for z in zones {
            sel.select(Selectable::Zone(z), now);
        }
    }
}

/// World overlays drawn each frame (brackets, ghosts, highlights).
#[derive(Resource, Default)]
pub struct WorldFrame {
    items: Vec<WorldDraw>,
}

#[derive(Clone)]
struct WorldDraw {
    pos: Vec2,
    size: Vec2,
    angle: f32,
    z: f32,
    image: Option<Handle<Image>>,
    color: Color,
    rect: Option<Rect>,
    flip_x: bool,
}

impl WorldFrame {
    pub fn quad(&mut self, pos: Vec2, size: Vec2, z: f32, color: Color) {
        self.items.push(WorldDraw {
            pos,
            size,
            angle: 0.0,
            z,
            image: None,
            color,
            rect: None,
            flip_x: false,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn image(
        &mut self,
        pos: Vec2,
        size: Vec2,
        angle: f32,
        z: f32,
        image: Handle<Image>,
        color: Color,
        rect: Option<Rect>,
        flip_x: bool,
    ) {
        self.items.push(WorldDraw {
            pos,
            size,
            angle,
            z,
            image: Some(image),
            color,
            rect,
            flip_x,
        });
    }

    /// `GenDraw.DrawFieldEdges`: an outline around a set of cells.
    pub fn field_edges(&mut self, cells: &[Cell], color: Color, z: f32) {
        let set: std::collections::HashSet<Cell> = cells.iter().copied().collect();
        let t = CELL_SIZE * 0.06;
        for &c in cells {
            let p = cell_center(c);
            let h = CELL_SIZE * 0.5;
            if !set.contains(&(c + Cell::new(0, 1))) {
                self.quad(
                    p + Vec2::new(0.0, h - t * 0.5),
                    Vec2::new(CELL_SIZE, t),
                    z,
                    color,
                );
            }
            if !set.contains(&(c + Cell::new(0, -1))) {
                self.quad(
                    p - Vec2::new(0.0, h - t * 0.5),
                    Vec2::new(CELL_SIZE, t),
                    z,
                    color,
                );
            }
            if !set.contains(&(c + Cell::new(1, 0))) {
                self.quad(
                    p + Vec2::new(h - t * 0.5, 0.0),
                    Vec2::new(t, CELL_SIZE),
                    z,
                    color,
                );
            }
            if !set.contains(&(c + Cell::new(-1, 0))) {
                self.quad(
                    p - Vec2::new(h - t * 0.5, 0.0),
                    Vec2::new(t, CELL_SIZE),
                    z,
                    color,
                );
            }
        }
    }
}

#[derive(Resource, Default)]
struct WorldPool(Vec<Entity>);

#[derive(Component)]
struct WorldPoolSprite;

/// Map clicks for selection run after the UI and the designators.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectSet;

pub fn build(app: &mut App) {
    app.init_resource::<WorldFrame>()
        .init_resource::<WorldPool>()
        .configure_sets(Update, SelectSet.after(super::UiSet::End))
        .add_systems(
            Update,
            (
                map_selection,
                draw_designations,
                draw_brackets,
                render_world,
            )
                .chain()
                .in_set(SelectSet)
                .after(super::designator::DesignatorSet),
        );
}

/// Left clicks and boxes on the map select (when no designator took them).
#[allow(clippy::too_many_arguments)]
fn map_selection(
    clicks: Res<PointerClicks>,
    keys: Res<ButtonInput<KeyCode>>,
    camera: Single<(&Camera, &GlobalTransform)>,
    sim: Res<SimState>,
    mut sel: ResMut<Selection>,
    time: Res<Time>,
    mut box_draw: ResMut<WorldFrame>,
    manager: Res<super::designator::DesignatorManager>,
) {
    sel.prune(&sim.0);
    let now = time.elapsed_secs();
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let (camera, gt) = *camera;
    let to_world = |p: Vec2| camera.viewport_to_world_2d(gt, p).ok();
    if manager.selected.is_some() {
        return;
    }
    if let Some((a, b)) = clicks
        .primary_drag
        .and_then(|(a, b)| Some((to_world(a)?, to_world(b)?)))
    {
        // The drag box (`DragBox.DragBoxOnGUI`).
        let (min, max) = (a.min(b), a.max(b));
        let c = (min + max) * 0.5;
        let s = max - min;
        let col = Color::srgba(0.8, 0.8, 0.8, 0.8);
        let t = 1.5;
        box_draw.quad(Vec2::new(c.x, max.y), Vec2::new(s.x, t), 90.0, col);
        box_draw.quad(Vec2::new(c.x, min.y), Vec2::new(s.x, t), 90.0, col);
        box_draw.quad(Vec2::new(min.x, c.y), Vec2::new(t, s.y), 90.0, col);
        box_draw.quad(Vec2::new(max.x, c.y), Vec2::new(t, s.y), 90.0, col);
    }
    if let Some((a, b)) = clicks
        .primary_drag_end
        .and_then(|(a, b)| Some((to_world(a)?, to_world(b)?)))
    {
        select_in_box(&mut sel, &sim.0, a, b, shift, now);
    }
    if let Some(w) = clicks.primary.and_then(to_world) {
        select_under(&mut sel, &sim.0, w, shift, now);
    }
    if keys.just_pressed(KeyCode::Escape) && !sel.is_empty() {
        sel.clear();
    }
}

/// `SelectionDrawer.DrawSelectionBracketFor`: four corner brackets around
/// each selected thing (jumping out briefly when selected); a selected
/// zone's field edges.
fn draw_brackets(
    sim: Res<SimState>,
    sel: Res<Selection>,
    time: Res<Time>,
    textures: Res<GameTextures>,
    mut tt: ResMut<ThingTextures>,
    mut images: ResMut<Assets<Image>>,
    mut frame: ResMut<WorldFrame>,
) {
    let sim = &sim.0;
    let tex = textures
        .0
        .as_ref()
        .and_then(|lib| tt.get(lib, "UI/Overlays/SelectionBracket", None, &mut images));
    let now = time.elapsed_secs();
    for o in &sel.objects {
        match *o {
            Selectable::Zone(z) => {
                frame.field_edges(sim.zone_cells(z), Color::srgba(1.0, 1.0, 1.0, 0.9), 89.0);
            }
            Selectable::Thing(t) => {
                let Some(pos) = draw_pos(sim, t) else {
                    continue;
                };
                let since = sel.times.get(o).map_or(1.0, |s| now - s);
                let jump = (1.0 - since / 0.07).max(0.0) * 0.2;
                let size = rotated_size(sim, t);
                let off = (size - Vec2::ONE) * 0.5 + Vec2::splat(jump);
                let corners = [
                    Vec2::new(-off.x, -off.y),
                    Vec2::new(off.x, -off.y),
                    Vec2::new(off.x, off.y),
                    Vec2::new(-off.x, off.y),
                ];
                for (i, c) in corners.iter().enumerate() {
                    let p = pos + *c * CELL_SIZE;
                    let angle = std::f32::consts::FRAC_PI_2 * i as f32;
                    match &tex {
                        Some(t) => frame.image(
                            p,
                            Vec2::splat(CELL_SIZE),
                            angle,
                            90.0,
                            t.image.clone(),
                            Color::WHITE,
                            None,
                            false,
                        ),
                        None => frame.quad(
                            p + c.normalize_or_zero() * CELL_SIZE * 0.4,
                            Vec2::splat(CELL_SIZE * 0.15),
                            90.0,
                            Color::WHITE,
                        ),
                    }
                }
            }
        }
    }
}

/// `DesignationManager.DrawDesignations`: each designation's icon
/// (`DesignationDef.texturePath`) over its cell or thing, one cell large.
#[allow(clippy::too_many_arguments)]
fn draw_designations(
    sim: Res<SimState>,
    textures: Res<GameTextures>,
    mut tt: ResMut<ThingTextures>,
    mut images: ResMut<Assets<Image>>,
    mut frame: ResMut<WorldFrame>,
) {
    let sim = &sim.0;
    let Some(lib) = textures.0.as_ref() else {
        return;
    };
    let mut draw = |frame: &mut WorldFrame, path: &str, pos: Vec2| {
        if let Some(t) = tt.get(lib, path, None, &mut images) {
            frame.image(
                pos,
                Vec2::splat(CELL_SIZE),
                0.0,
                85.0,
                t.image,
                Color::WHITE,
                None,
                false,
            );
        }
    };
    let map = &sim.map;
    for &c in &map.mine_designations {
        draw(&mut frame, "Designations/Mine", cell_center(c));
    }
    for &c in map
        .smooth_wall_designations
        .iter()
        .chain(&map.smooth_floor_designations)
    {
        draw(&mut frame, "Designations/SmoothSurface", cell_center(c));
    }
    for &c in &map.remove_floor_designations {
        draw(&mut frame, "Designations/RemoveFloor", cell_center(c));
    }
    for &id in &map.deconstruct_designations {
        if let Some(p) = draw_pos(sim, ThingRef::Item(id)) {
            draw(&mut frame, "Designations/Deconstruct", p);
        }
    }
    for &id in &map.flick_designations {
        if let Some(p) = draw_pos(sim, ThingRef::Item(id)) {
            draw(&mut frame, "Designations/Flick", p);
        }
    }
    for &id in map.cut_designations() {
        if let Some(p) = draw_pos(sim, ThingRef::Item(id)) {
            draw(&mut frame, "Designations/CutPlant", p);
        }
    }
    for &id in map.harvest_designations() {
        if let Some(p) = draw_pos(sim, ThingRef::Item(id)) {
            draw(&mut frame, "Designations/HarvestPlant", p);
        }
    }
    for &id in &map.hunt_designations {
        if let Some(p) = draw_pos(sim, ThingRef::Pawn(id)) {
            draw(&mut frame, "Designations/Hunt", p);
        }
    }
}

/// Renders the world overlays through a sprite pool.
fn render_world(
    mut commands: Commands,
    mut frame: ResMut<WorldFrame>,
    mut pool: ResMut<WorldPool>,
    mut sprites: Query<(&mut Sprite, &mut Transform, &mut Visibility), With<WorldPoolSprite>>,
) {
    let items = std::mem::take(&mut frame.items);
    for (k, d) in items.iter().enumerate() {
        let Some(&e) = pool.0.get(k) else {
            let e = commands
                .spawn((
                    WorldPoolSprite,
                    Sprite::default(),
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id();
            pool.0.push(e);
            continue;
        };
        let Ok((mut s, mut t, mut v)) = sprites.get_mut(e) else {
            continue;
        };
        *v = Visibility::Inherited;
        t.translation = d.pos.extend(d.z);
        t.rotation = Quat::from_rotation_z(d.angle);
        s.custom_size = Some(d.size);
        s.color = d.color;
        s.rect = d.rect;
        s.flip_x = d.flip_x;
        s.image = d.image.clone().unwrap_or_default();
    }
    for &e in pool.0.iter().skip(items.len()) {
        if let Ok((_, _, mut v)) = sprites.get_mut(e) {
            v.set_if_neq(Visibility::Hidden);
        }
    }
}
