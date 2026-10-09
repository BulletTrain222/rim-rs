//! The colony map: terrain per cell, at most one building per cell, and
//! loose items (food, resources) lying on cells.

use rimworld_defs::{DefId, GameDefs, Passability, TerrainDef, ThingDef};

use crate::geom::Footprint;
use crate::grid::{Cell, Grid, GridSize};
use crate::job::Rot4;
use crate::path::{IMPASSABLE, PathGrid};
use crate::pawn::PawnId;
use crate::storage::Storage;

/// Path cost of a frame (the generated `Frame_*` defs' `pathCost`; the
/// defs are generated in code, see `ThingDefGenerator_Buildings`).
pub const FRAME_PATH_COST: u32 = 14;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ItemId(pub u32);

/// A stack of items lying on the map (the game's spawned item `Thing`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub position: Cell,
    pub stack_count: u32,
    /// Filth only: thickness (cleaning removes one level at a time); 0 for
    /// other things.
    pub thickness: u32,
    /// Filth only: tick it appeared or last thickened (`growTick`).
    pub grow_tick: i64,
    /// Rottable things: ticks of rot so far (`CompRottable.RotProgress`).
    #[serde(default)]
    pub rot: f32,
    /// The game's `thingIDNumber`, which buckets its rare ticks; the item
    /// id unless a replay sets it.
    #[serde(default)]
    pub id_number: i32,
    /// Hit points when damaged (`None`: full).
    #[serde(default)]
    pub hit_points: Option<i32>,
    /// Filth only: it vanishes this many ticks after it last thickened
    /// (`disappearAfterTicks`; 0 = never).
    #[serde(default)]
    pub disappear_after: i64,
    /// Forbidden to the colony (`CompForbiddable.Forbidden`).
    #[serde(default)]
    pub forbidden: bool,
}

impl Item {
    pub fn is_filth(&self) -> bool {
        self.thickness > 0
    }
}

/// Hit points of a stack of `n` after absorbing `m` (`TryAbsorbStack`:
/// the count-weighted mean, rounded up); `None` stands for full `max`.
pub fn blend_hit_points(a: Option<i32>, n: u32, b: Option<i32>, m: u32, max: i32) -> Option<i32> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let (a, b) = (a.unwrap_or(max), b.unwrap_or(max));
    let hp = ((a as f32 * n as f32 + b as f32 * m as f32) / (n + m) as f32).ceil() as i32;
    (hp < max).then_some(hp)
}

/// Rot of a stack of `n` after absorbing `m` with rot `rot_m`
/// (`Mathf.Lerp(RotProgress, other, m / (n + m))`).
pub fn blend_rot(rot_n: f32, n: u32, rot_m: f32, m: u32) -> f32 {
    if n + m == 0 {
        return rot_n;
    }
    let t = (m as f32 / (n + m) as f32).clamp(0.0, 1.0);
    rot_n + (rot_m - rot_n) * t
}

/// Whether a constructible is still a plan or already a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConstructStage {
    /// `Blueprint_Build`: a plan; ethereal, holds nothing.
    Blueprint,
    /// `Frame`: receives the materials and the construction work.
    Frame,
}

/// What a blueprint or frame builds (`entityDefToBuild`): a building or a
/// floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Buildable {
    Thing(DefId<ThingDef>),
    Floor(DefId<TerrainDef>),
}

impl Buildable {
    pub fn thing(self) -> Option<DefId<ThingDef>> {
        match self {
            Buildable::Thing(d) => Some(d),
            Buildable::Floor(_) => None,
        }
    }

    pub fn floor(self) -> Option<DefId<TerrainDef>> {
        match self {
            Buildable::Floor(d) => Some(d),
            Buildable::Thing(_) => None,
        }
    }
}

/// A building under construction (`Blueprint_Build` or `Frame`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Constructible {
    /// Thing identity (shared numbering with items).
    pub id: ItemId,
    pub stage: ConstructStage,
    /// The building or floor to build (`entityDefToBuild`).
    pub building: Buildable,
    /// The material (`stuffToUse` / the frame's stuff).
    pub stuff: Option<DefId<ThingDef>>,
    pub position: Cell,
    pub rotation: Rot4,
    /// The building's size (x, z).
    pub size: (i32, i32),
    /// Materials delivered into a frame (`resourceContainer`), by def.
    pub resources: Vec<(DefId<ThingDef>, u32)>,
    /// Construction work done on a frame (`workDone`).
    pub work_done: f32,
}

impl Constructible {
    pub fn footprint(&self) -> Footprint {
        Footprint {
            center: self.position,
            rot: self.rotation,
            size: self.size,
        }
    }

    /// Delivered count of `def`.
    pub fn delivered(&self, def: DefId<ThingDef>) -> u32 {
        self.resources
            .iter()
            .filter(|(d, _)| *d == def)
            .map(|(_, n)| n)
            .sum()
    }

    /// Adds delivered material.
    pub fn add_resource(&mut self, def: DefId<ThingDef>, count: u32) {
        match self.resources.iter_mut().find(|(d, _)| *d == def) {
            Some((_, n)) => *n += count,
            None => self.resources.push((def, count)),
        }
    }
}

/// Ticks a door stays open after a pawn passed (`CloseDelayTicks`).
pub const DOOR_CLOSE_DELAY_TICKS: i32 = 110;
/// A door closes on its own only while touched this recently.
pub const DOOR_TOUCH_MEMORY_TICKS: i64 = 120;

/// State of a door (`Building_Door`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Door {
    pub cell: Cell,
    pub open: bool,
    /// Opening progress (`ticksSinceOpen`).
    pub ticks_since_open: i32,
    pub ticks_until_close: i32,
    /// `lastFriendlyTouchTick`.
    pub last_friendly_touch: i64,
    /// `TicksToOpenNow` (from DoorOpenSpeed with the stuff).
    pub ticks_to_open: i32,
    /// `CloseDelayAdjusted`.
    pub close_delay: i32,
    /// `thingIDNumber` (its temperature equalization interval is hashed).
    #[serde(default)]
    pub id_number: i32,
    /// `DoorPowerOn`: a powered door (autodoor) with power.
    #[serde(default)]
    pub powered: bool,
}

impl Door {
    /// `SlowsPawns`: an unpowered door makes pawns wait; a powered one only
    /// when opening takes more than 20 ticks.
    pub fn slows_pawns(&self) -> bool {
        !self.powered || self.ticks_to_open > 20
    }

    /// `TicksTillFullyOpened`.
    pub fn ticks_till_fully_opened(&self) -> i32 {
        (self.ticks_to_open - self.ticks_since_open).max(0)
    }

    /// `NextCellDoorToWaitForOrManuallyOpen` (for a pawn that can open
    /// it): closed, or still opening.
    pub fn must_wait(&self) -> bool {
        self.slows_pawns() && (!self.open || self.ticks_till_fully_opened() > 0)
    }

    /// `DoorOpen(ticksToClose)`.
    pub fn open_door(&mut self, ticks_to_close: i32) {
        if self.open {
            self.ticks_until_close = ticks_to_close;
        } else {
            self.ticks_until_close = self.ticks_to_open + ticks_to_close;
            self.open = true;
        }
    }

    /// `FreePassage` for a door nobody holds open: open and not about to
    /// close.
    pub fn will_close_soon(&self, now: i64) -> bool {
        !self.open
            || (self.ticks_until_close > 0 && self.ticks_until_close <= self.close_delay + 1)
            || now < self.last_friendly_touch + DOOR_TOUCH_MEMORY_TICKS
    }
}

/// A finished building placed by the colony (walls, doors, furniture),
/// with its identity and footprint.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Structure {
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub stuff: Option<DefId<ThingDef>>,
    pub footprint: Footprint,
    /// Pawns assigned to it (bed owners), in assignment order.
    pub owners: Vec<PawnId>,
    /// Fuel left (`CompRefuelable.fuel`).
    #[serde(default)]
    pub fuel: f32,
    /// `thingIDNumber` (staggers its periodic work).
    #[serde(default)]
    pub id_number: i32,
    /// Takes the cell's building slot (`isEdifice`); conduits don't.
    #[serde(default = "edifice_default")]
    pub edifice: bool,
    /// Power state (`CompPower*`, `CompFlickable`, `CompTempControl`).
    #[serde(default)]
    pub power: StructurePower,
}

fn edifice_default() -> bool {
    true
}

fn first_bill_id() -> u32 {
    1
}

/// `FoodPoisonCause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PoisonCause {
    #[default]
    Unknown,
    IncompetentCook,
    FilthyKitchen,
}

/// A cooked thing's hidden state.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ItemMeta {
    /// `CompIngredients.ingredients`, in registration order.
    pub ingredients: Vec<DefId<ThingDef>>,
    /// `CompFoodPoisonable.poisonPct` and `cause`.
    pub poison_pct: f32,
    pub poison_cause: PoisonCause,
}

/// A building's power and switch state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StructurePower {
    /// `CompPowerTrader.PowerOn`.
    pub on: bool,
    /// `PowerOutput` in W (negative: draws).
    pub output: f32,
    /// Batteries: `storedEnergy` in W·days.
    pub stored: f32,
    /// `CompFlickable.SwitchIsOn`.
    pub switch_on: bool,
    /// The transmitter a connector is wired to (`connectParent`).
    pub connect_parent: Option<ItemId>,
    /// `CompTempControl.targetTemperature`.
    pub target_temperature: f32,
    /// `CompFlickable.wantSwitchOn`: what the player asked for.
    #[serde(default = "edifice_default")]
    pub want_switch_on: bool,
    /// `operatingAtHighPower`.
    pub high_power: bool,
    /// `CompBreakdownable.BrokenDown`.
    #[serde(default)]
    pub broken_down: bool,
}

impl Default for StructurePower {
    fn default() -> Self {
        Self {
            on: false,
            output: 0.0,
            stored: 0.0,
            switch_on: true,
            want_switch_on: true,
            connect_parent: None,
            target_temperature: 21.0,
            high_power: false,
            broken_down: false,
        }
    }
}

/// A growing zone (`Zone_Growing`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GrowingZone {
    /// Cells in the order they were added.
    pub cells: Vec<Cell>,
    /// The plant to grow (`plantDefToGrow`; potatoes by default).
    pub plant: DefId<ThingDef>,
    pub allow_sow: bool,
    pub allow_cut: bool,
    /// `Zone.label`'s number ("Growing zone 2"); 0: unnamed.
    #[serde(default)]
    pub label_number: u32,
    /// `Zone.color` (RGBA), if one was given.
    #[serde(default)]
    pub color: Option<[f32; 4]>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Map {
    pub terrain: Grid<DefId<TerrainDef>>,
    /// Building (e.g. natural rock) occupying the cell, if any.
    pub buildings: Grid<Option<DefId<ThingDef>>>,
    /// Items in spawn order (the order the game's thing lists keep).
    items: Vec<Item>,
    next_item_id: u32,
    /// The home area (`Area_Home`).
    pub home: Grid<bool>,
    /// Filth inside the home area, in the game's list order
    /// (`ListerFilthInHomeArea`).
    filth_in_home: Vec<ItemId>,
    /// Stockpile zones.
    pub storage: Storage,
    /// Cooked things' ingredients and food poisoning (`CompIngredients`,
    /// `CompFoodPoisonable`), by item.
    #[serde(default)]
    pub item_meta: std::collections::BTreeMap<ItemId, ItemMeta>,
    /// The next bill's `loadID`.
    #[serde(default = "first_bill_id")]
    pub next_bill_id: u32,
    /// Material of the building on each cell (for buildings made of stuff).
    pub building_stuff: Grid<Option<DefId<ThingDef>>>,
    /// Blueprints and frames, in spawn order.
    constructibles: Vec<Constructible>,
    /// Changes whenever a blueprint, frame or building appears or goes
    /// (for front-ends redrawing them).
    structure_revision: u64,
    /// Changes whenever terrain changes in play (mining, floors).
    #[serde(default)]
    terrain_revision: u64,
    /// Doors, in spawn order (the order they tick).
    doors: Vec<Door>,
    /// Buildings the colony built, in spawn order.
    structures: Vec<Structure>,
    /// Plants, in spawn order.
    plants: Vec<crate::plant::Plant>,
    /// The plant on each cell.
    plant_grid: Grid<Option<ItemId>>,
    /// Position of each plant in `plants` (rebuilt after loading).
    #[serde(skip)]
    plant_index: std::collections::HashMap<ItemId, usize>,
    /// Growing zones (removed zones stay as empty entries).
    growing: Vec<GrowingZone>,
    growing_at: Grid<Option<usize>>,
    /// Changes when plants or growing zones change (for front-ends).
    pub plant_revision: u64,
    /// Plants designated for cutting (`DesignationDefOf.CutPlant`), in
    /// designation order.
    cut_designations: Vec<ItemId>,
    /// Plants designated for harvest (`HarvestPlant`), in order.
    #[serde(default)]
    harvest_designations: Vec<ItemId>,
    /// Constructed roofs (`RoofDefOf.RoofConstructed`); `None` until the
    /// first roof.
    #[serde(default)]
    roofs: Option<Grid<bool>>,
    /// Which roofs are natural rock roofs (1 thin, 2 thick); `None` while
    /// there are none.
    #[serde(default)]
    natural_roofs: Option<Grid<u8>>,
    /// The build roof area (`Area_BuildRoof`); `None` while empty.
    #[serde(default)]
    build_roof: Option<Grid<bool>>,
    /// Changes when roofs or the build roof area change (front-ends).
    #[serde(default)]
    pub roof_revision: u64,
    /// The sky's glow this tick (`SkyManager.CurSkyGlow`), set by the
    /// simulation.
    #[serde(default = "full_glow")]
    pub sky_glow: f32,
    /// Lamp light per cell, recomputed by the simulation when lamps or
    /// light blockers change.
    #[serde(skip)]
    pub light: Option<Grid<crate::light::LightCell>>,
    /// Damaged rock: (cell, hit points left, mining yield so far).
    #[serde(default)]
    pub mined: Vec<(Cell, i32, f32)>,
    /// Cells designated for mining (`DesignationDefOf.Mine`), in order.
    #[serde(default)]
    pub mine_designations: Vec<Cell>,
    /// Cells designated for floor removal (`RemoveFloor`), in order.
    #[serde(default)]
    pub remove_floor_designations: Vec<Cell>,
    /// Cells designated for smoothing (`SmoothFloor`), in order.
    #[serde(default)]
    pub smooth_floor_designations: Vec<Cell>,
    /// Rock walls designated for smoothing (`SmoothWall`), in order.
    #[serde(default)]
    pub smooth_wall_designations: Vec<Cell>,
    /// The terrain beneath a laid floor (`TerrainGrid.underGrid`); `None`
    /// until the first floor.
    #[serde(default)]
    under_terrain: Option<Grid<Option<DefId<TerrainDef>>>>,
    /// Buildings designated for deconstruction, in order.
    #[serde(default)]
    pub deconstruct_designations: Vec<ItemId>,
    /// Buildings designated to have their switch flicked, in order.
    #[serde(default)]
    pub flick_designations: Vec<ItemId>,
    /// Animals designated to be hunted, in order (`DesignationManager`).
    #[serde(default)]
    pub hunt_designations: Vec<crate::pawn::PawnId>,
}

fn full_glow() -> f32 {
    1.0
}

impl Map {
    pub fn new(size: GridSize, fill: DefId<TerrainDef>) -> Self {
        Self {
            roofs: None,
            natural_roofs: None,
            build_roof: None,
            roof_revision: 0,
            sky_glow: 1.0,
            light: None,
            mined: Vec::new(),
            mine_designations: Vec::new(),
            remove_floor_designations: Vec::new(),
            smooth_floor_designations: Vec::new(),
            smooth_wall_designations: Vec::new(),
            under_terrain: None,
            deconstruct_designations: Vec::new(),
            item_meta: Default::default(),
            next_bill_id: 1,
            flick_designations: Vec::new(),
            hunt_designations: Vec::new(),
            terrain: Grid::new(size, fill),
            buildings: Grid::new(size, None),
            items: Vec::new(),
            next_item_id: 0,
            home: Grid::new(size, false),
            filth_in_home: Vec::new(),
            storage: Storage::new(size),
            building_stuff: Grid::new(size, None),
            constructibles: Vec::new(),
            structure_revision: 0,
            terrain_revision: 0,
            doors: Vec::new(),
            structures: Vec::new(),
            plants: Vec::new(),
            plant_grid: Grid::new(size, None),
            plant_index: Default::default(),
            growing: Vec::new(),
            growing_at: Grid::new(size, None),
            plant_revision: 0,
            cut_designations: Vec::new(),
            harvest_designations: Vec::new(),
        }
    }

    pub fn size(&self) -> GridSize {
        self.terrain.size()
    }

    /// A new thing identity (`thingIDNumber`), e.g. for a split-off stack.
    /// Debug tool: the next id handed out (replaying a recorded thing id).
    pub fn debug_set_next_item_id(&mut self, id: u32) {
        self.next_item_id = id;
    }

    pub fn allocate_item_id(&mut self) -> ItemId {
        let id = ItemId(self.next_item_id);
        self.next_item_id += 1;
        id
    }

    /// Places a stack of `count` items of `def` on `cell`.
    pub fn spawn_item(&mut self, def: DefId<ThingDef>, cell: Cell, count: u32) -> ItemId {
        let id = self.allocate_item_id();
        self.spawn_item_with_id(id, def, cell, count);
        id
    }

    /// Spawns a thing that already has an identity (e.g. put down after
    /// being carried).
    pub fn spawn_item_with_id(&mut self, id: ItemId, def: DefId<ThingDef>, cell: Cell, count: u32) {
        self.items.push(Item {
            id,
            def,
            position: cell,
            stack_count: count.max(1),
            thickness: 0,
            grow_tick: 0,
            rot: 0.0,
            id_number: id.0 as i32,
            hit_points: None,
            disappear_after: 0,
            forbidden: false,
        });
    }

    /// Spawns a carried thing put down, with its rot.
    pub fn spawn_carried(&mut self, c: &crate::pawn::Carried, cell: Cell) {
        self.spawn_item_with_id(c.id, c.def, cell, c.count);
        let item = self.items.last_mut().expect("just spawned");
        item.rot = c.rot;
        item.hit_points = c.hit_points;
    }

    /// Adds `count` with rot progress `rot` to a stack (`TryAbsorbStack`'s
    /// receiving side; `CompRottable.PreAbsorbStack` blends the rot by
    /// count).
    pub fn add_to_stack(
        &mut self,
        id: ItemId,
        count: u32,
        rot: f32,
        hit_points: Option<i32>,
        max_hp: i32,
    ) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.rot = blend_rot(i.rot, i.stack_count, rot, count);
            i.hit_points = blend_hit_points(i.hit_points, i.stack_count, hit_points, count, max_hp);
            i.stack_count += count;
        }
    }

    /// Sets an item's `thingIDNumber` (replays).
    pub fn set_item_id_number(&mut self, id: ItemId, id_number: i32) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.id_number = id_number;
        }
    }

    /// Sets an item's rot progress (replays, debugging).
    pub fn set_item_rot(&mut self, id: ItemId, rot: f32) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.rot = rot;
        }
    }

    /// Spawns filth of `thickness` on `cell` at tick `now`; it joins the
    /// home-area filth list if the cell is in the home area.
    // COMPATIBILITY TODO: currently approximate — filth does not merge with
    // existing filth of the same def, thicken, or disappear by itself.
    pub fn spawn_filth(
        &mut self,
        def: DefId<ThingDef>,
        cell: Cell,
        thickness: u32,
        now: u64,
    ) -> ItemId {
        let id = self.spawn_item(def, cell, 1);
        let item = self.items.last_mut().expect("just spawned");
        item.thickness = thickness.max(1);
        item.grow_tick = now as i64;
        if self.home[cell] {
            self.filth_in_home.push(id);
        }
        id
    }

    /// `GlowGrid.GroundGlowAt`: the sky's glow, none under a roof.
    // COMPATIBILITY TODO: currently approximate — lamps, fire and other
    // light sources are not modelled.
    pub fn ground_glow(&self, c: Cell) -> f32 {
        let sky = if self.roofed(c) { 0.0 } else { self.sky_glow };
        if sky >= 1.0 {
            return sky;
        }
        let lamps = self
            .light
            .as_ref()
            .and_then(|g| g.get(c))
            .map_or(0.0, |l| crate::light::lamp_glow(*l));
        sky.max(lamps)
    }

    /// Whether the cell has a roof.
    pub fn roofed(&self, c: Cell) -> bool {
        self.roofs
            .as_ref()
            .is_some_and(|g| g.get(c).copied().unwrap_or(false))
    }

    /// Puts up (a constructed roof) or takes down the roof over a cell.
    pub fn set_roof(&mut self, c: Cell, roofed: bool) {
        let size = self.size();
        if let Some(n) = self.natural_roofs.as_mut()
            && n[c] != 0
        {
            n[c] = 0;
            self.roof_revision += 1;
        }
        let g = self.roofs.get_or_insert_with(|| Grid::new(size, false));
        if g[c] != roofed {
            g[c] = roofed;
            self.roof_revision += 1;
        }
    }

    /// The natural rock roof over a cell (`RoofRockThin`/`RoofRockThick`).
    pub fn natural_roof(&self, c: Cell) -> Option<crate::native_mapgen::NaturalRoof> {
        use crate::native_mapgen::NaturalRoof;
        match self.natural_roofs.as_ref().and_then(|g| g.get(c).copied()) {
            Some(1) => Some(NaturalRoof::Thin),
            Some(2) => Some(NaturalRoof::Thick),
            _ => None,
        }
    }

    /// Puts a natural rock roof over a cell (or takes the roof away).
    pub fn set_natural_roof(&mut self, c: Cell, roof: Option<crate::native_mapgen::NaturalRoof>) {
        use crate::native_mapgen::NaturalRoof;
        self.set_roof(c, roof.is_some());
        if let Some(r) = roof {
            let size = self.size();
            let g = self.natural_roofs.get_or_insert_with(|| Grid::new(size, 0));
            g[c] = match r {
                NaturalRoof::Thin => 1,
                NaturalRoof::Thick => 2,
            };
            self.roof_revision += 1;
        }
    }

    /// Whether the cell is in the build roof area.
    pub fn build_roof(&self, c: Cell) -> bool {
        self.build_roof
            .as_ref()
            .is_some_and(|g| g.get(c).copied().unwrap_or(false))
    }

    pub fn set_build_roof(&mut self, c: Cell, on: bool) {
        let size = self.size();
        let g = self
            .build_roof
            .get_or_insert_with(|| Grid::new(size, false));
        if g[c] != on {
            g[c] = on;
            self.roof_revision += 1;
        }
    }

    /// The build roof area's cells in map order (`ActiveCells`).
    pub fn build_roof_cells(&self) -> Vec<Cell> {
        self.build_roof
            .as_ref()
            .map(|g| g.iter().filter(|(_, v)| **v).map(|(c, _)| c).collect())
            .unwrap_or_default()
    }

    /// Sets when a filth appeared or last thickened (replays of older
    /// filth).
    pub fn set_filth_grow_tick(&mut self, id: ItemId, tick: i64) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.grow_tick = tick;
        }
    }

    pub fn filth_in_home(&self) -> &[ItemId] {
        &self.filth_in_home
    }

    /// Adds or removes a cell from the home area, updating the filth list
    /// as the game does (`Notify_HomeAreaChanged`).
    /// `SetForbidden`: only things with the forbiddable comp can be
    /// forbidden.
    pub fn set_forbidden(&mut self, defs: &GameDefs, id: ItemId, forbidden: bool) {
        if let Some(n) = self.items.iter().position(|i| i.id == id)
            && defs.things[self.items[n].def].forbiddable
        {
            self.items[n].forbidden = forbidden;
        }
    }

    pub fn set_home(&mut self, cell: Cell, home: bool) {
        if self.home[cell] == home {
            return;
        }
        self.home[cell] = home;
        if home {
            let filth: Vec<ItemId> = self
                .items
                .iter()
                .filter(|i| i.position == cell && i.is_filth())
                .map(|i| i.id)
                .collect();
            self.filth_in_home.extend(filth);
        } else {
            let items = &self.items;
            self.filth_in_home.retain(|id| {
                items
                    .iter()
                    .find(|i| i.id == *id)
                    .is_none_or(|i| i.position != cell)
            });
        }
    }

    /// Cleans one thickness level off a filth (`Filth.ThinFilth`). Returns
    /// `true` when the filth is gone.
    pub fn thin_filth(&mut self, id: ItemId) -> bool {
        let Some(at) = self.items.iter().position(|i| i.id == id) else {
            return true;
        };
        let item = &mut self.items[at];
        item.thickness = item.thickness.saturating_sub(1);
        if item.thickness == 0 {
            self.remove_item_at(at);
            true
        } else {
            false
        }
    }

    fn remove_item_at(&mut self, at: usize) {
        let id = self.items[at].id;
        self.items.remove(at);
        self.filth_in_home.retain(|&f| f != id);
    }

    /// Places a blueprint for `building` made of `stuff` on `cell`
    /// (`GenConstruct.PlaceBlueprintForBuild`).
    pub fn place_blueprint(
        &mut self,
        building: Buildable,
        stuff: Option<DefId<ThingDef>>,
        footprint: Footprint,
    ) -> ItemId {
        let id = self.allocate_item_id();
        self.structure_revision += 1;
        self.constructibles.push(Constructible {
            id,
            stage: ConstructStage::Blueprint,
            building,
            stuff,
            position: footprint.center,
            rotation: footprint.rot,
            size: footprint.size,
            resources: Vec::new(),
            work_done: 0.0,
        });
        id
    }

    pub fn constructibles(&self) -> &[Constructible] {
        &self.constructibles
    }

    pub fn constructible(&self, id: ItemId) -> Option<&Constructible> {
        self.constructibles.iter().find(|c| c.id == id)
    }

    pub fn constructible_mut(&mut self, id: ItemId) -> Option<&mut Constructible> {
        self.constructibles.iter_mut().find(|c| c.id == id)
    }

    /// The blueprint or frame occupying `cell`.
    pub fn constructible_at(&self, cell: Cell) -> Option<&Constructible> {
        self.constructibles
            .iter()
            .find(|c| c.footprint().contains(cell))
    }

    /// Spawns a finished building over its footprint.
    pub fn spawn_structure(
        &mut self,
        def: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        footprint: Footprint,
    ) -> ItemId {
        self.spawn_structure_with(def, stuff, footprint, true)
    }

    /// [`Map::spawn_structure`]; a non-edifice (a conduit) leaves the
    /// cells' building slot alone.
    pub fn spawn_structure_with(
        &mut self,
        def: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        footprint: Footprint,
        edifice: bool,
    ) -> ItemId {
        let id = self.allocate_item_id();
        if edifice {
            for c in footprint.cells() {
                if self.size().contains(c) {
                    self.buildings[c] = Some(def);
                    self.building_stuff[c] = stuff;
                }
            }
        }
        self.structure_revision += 1;
        self.structures.push(Structure {
            id,
            def,
            stuff,
            footprint,
            owners: Vec::new(),
            fuel: 0.0,
            id_number: id.0 as i32,
            edifice,
            power: StructurePower::default(),
        });
        id
    }

    /// Removes a finished building.
    pub fn remove_structure(&mut self, id: ItemId) -> Option<Structure> {
        let at = self.structures.iter().position(|s| s.id == id)?;
        let s = self.structures.remove(at);
        if s.edifice {
            for c in s.footprint.cells() {
                if self.size().contains(c) {
                    self.buildings[c] = None;
                    self.building_stuff[c] = None;
                }
            }
        }
        self.doors.retain(|d| !s.footprint.contains(d.cell));
        self.structure_revision += 1;
        Some(s)
    }

    pub fn plants(&self) -> &[crate::plant::Plant] {
        &self.plants
    }

    /// Designates a plant for cutting.
    pub fn designate_cut(&mut self, id: ItemId) {
        if self.plant(id).is_some() && !self.cut_designations.contains(&id) {
            self.cut_designations.push(id);
            self.plant_revision += 1;
        }
    }

    pub fn cut_designations(&self) -> &[ItemId] {
        &self.cut_designations
    }

    /// Designates a plant for harvest.
    pub fn designate_harvest(&mut self, id: ItemId) {
        if self.plant(id).is_some() && !self.harvest_designations.contains(&id) {
            self.harvest_designations.push(id);
            self.plant_revision += 1;
        }
    }

    pub fn harvest_designations(&self) -> &[ItemId] {
        &self.harvest_designations
    }

    /// Drops a plant's harvest designation (it was harvested).
    pub fn clear_harvest_designation(&mut self, id: ItemId) {
        self.harvest_designations.retain(|&d| d != id);
    }

    /// Drops a plant's cut designation.
    pub fn clear_cut_designation(&mut self, id: ItemId) {
        self.cut_designations.retain(|&d| d != id);
        self.plant_revision += 1;
    }

    /// Rebuilds the plant lookup (after loading a save).
    pub fn reindex_plants(&mut self) {
        self.plant_index = self
            .plants
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id, i))
            .collect();
    }

    pub fn plant(&self, id: ItemId) -> Option<&crate::plant::Plant> {
        self.plant_index.get(&id).map(|&i| &self.plants[i])
    }

    pub fn plant_mut(&mut self, id: ItemId) -> Option<&mut crate::plant::Plant> {
        self.plant_revision += 1;
        let i = *self.plant_index.get(&id)?;
        Some(&mut self.plants[i])
    }

    /// Plants for in-place changes (growth, age, hit points; not position).
    pub fn plants_mut(&mut self) -> &mut [crate::plant::Plant] {
        self.plant_revision += 1;
        &mut self.plants
    }

    /// The plant on `cell` (`GetPlant`).
    pub fn plant_at(&self, cell: Cell) -> Option<&crate::plant::Plant> {
        self.plant_grid
            .get(cell)
            .copied()
            .flatten()
            .and_then(|id| self.plant(id))
    }

    /// Spawns a plant of `def` with `growth` on `cell`.
    pub fn spawn_plant(
        &mut self,
        def: DefId<ThingDef>,
        cell: Cell,
        growth: f32,
        max_hit_points: f32,
    ) -> ItemId {
        let id = self.allocate_item_id();
        self.plant_index.insert(id, self.plants.len());
        self.plant_grid[cell] = Some(id);
        self.plants.push(crate::plant::Plant {
            id,
            def,
            position: cell,
            growth,
            age: 0,
            sown: false,
            hit_points: max_hit_points,
            max_hit_points,
            id_number: id.0 as i32,
            made_leafless_tick: -99_999,
        });
        self.plant_revision += 1;
        id
    }

    pub fn remove_plant(&mut self, id: ItemId) -> Option<crate::plant::Plant> {
        let at = *self.plant_index.get(&id)?;
        self.plant_revision += 1;
        let p = self.plants.remove(at);
        self.plant_grid[p.position] = None;
        self.cut_designations.retain(|&d| d != id);
        self.harvest_designations.retain(|&d| d != id);
        self.reindex_plants();
        Some(p)
    }

    /// Adds a growing zone over `cells`; returns its index.
    pub fn add_growing_zone(&mut self, plant: DefId<ThingDef>, cells: &[Cell]) -> usize {
        let z = self.growing.len();
        self.growing.push(GrowingZone {
            cells: Vec::new(),
            plant,
            allow_sow: true,
            allow_cut: true,
            label_number: 0,
            color: None,
        });
        self.add_growing_cells(z, cells);
        z
    }

    pub fn add_growing_cells(&mut self, z: usize, cells: &[Cell]) {
        for &c in cells {
            if self.growing_at[c].is_none() {
                self.growing_at[c] = Some(z);
                self.growing[z].cells.push(c);
            }
        }
        self.plant_revision += 1;
    }

    pub fn remove_growing_cell(&mut self, c: Cell) {
        if let Some(z) = self.growing_at[c].take() {
            self.growing[z].cells.retain(|&x| x != c);
            self.plant_revision += 1;
        }
    }

    pub fn growing_zone_at(&self, c: Cell) -> Option<usize> {
        self.growing_at.get(c).copied().flatten()
    }

    pub fn growing_zone(&self, z: usize) -> &GrowingZone {
        &self.growing[z]
    }

    pub fn growing_zone_mut(&mut self, z: usize) -> &mut GrowingZone {
        self.plant_revision += 1;
        &mut self.growing[z]
    }

    /// Growing zones with cells, in creation order.
    pub fn growing_zones(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.growing.len()).filter(|&z| !self.growing[z].cells.is_empty())
    }

    pub fn structures(&self) -> &[Structure] {
        &self.structures
    }

    pub fn structure(&self, id: ItemId) -> Option<&Structure> {
        self.structures.iter().find(|s| s.id == id)
    }

    /// Changes the terrain of a cell in play (front-ends redraw).
    pub fn set_terrain(&mut self, c: Cell, terrain: DefId<TerrainDef>) {
        self.terrain[c] = terrain;
        self.terrain_revision += 1;
    }

    /// `TerrainGrid.SetTerrain` in play: a layerable floor keeps what it
    /// covers (the old terrain if passable, else sand); anything else
    /// clears the layer beneath.
    pub fn set_terrain_layered(&mut self, defs: &GameDefs, c: Cell, terrain: DefId<TerrainDef>) {
        let size = self.size();
        if defs.terrain[terrain].layerable {
            let under = self
                .under_terrain
                .get_or_insert_with(|| Grid::new(size, None));
            if under[c].is_none() {
                let old = self.terrain[c];
                under[c] = if defs.terrain[old].is_walkable() {
                    Some(old)
                } else {
                    defs.terrain.id("Sand")
                };
            }
        } else if let Some(under) = &mut self.under_terrain {
            under[c] = None;
        }
        self.set_terrain(c, terrain);
    }

    /// The terrain under a floor, if any.
    pub fn under_terrain(&self, c: Cell) -> Option<DefId<TerrainDef>> {
        self.under_terrain.as_ref().and_then(|g| g[c])
    }

    /// `TerrainGrid.CanRemoveTopLayerAt`: a removable (layerable) floor
    /// with something beneath.
    pub fn can_remove_top_layer(&self, defs: &GameDefs, c: Cell) -> bool {
        defs.terrain[self.terrain[c]].layerable && self.under_terrain(c).is_some()
    }

    /// `TerrainGrid.RemoveTopLayer`: the terrain beneath comes back.
    pub fn remove_top_layer(&mut self, c: Cell) {
        if let Some(under) = self.under_terrain.as_mut().and_then(|g| g[c].take()) {
            self.set_terrain(c, under);
        }
    }

    pub fn terrain_revision(&self) -> u64 {
        self.terrain_revision
    }

    /// Marks the buildings as changed (front-ends, light blockers).
    pub fn bump_structure_revision(&mut self) {
        self.structure_revision += 1;
    }

    pub fn structures_mut(&mut self) -> &mut [Structure] {
        &mut self.structures
    }

    pub fn structure_mut(&mut self, id: ItemId) -> Option<&mut Structure> {
        self.structures.iter_mut().find(|s| s.id == id)
    }

    /// The building on `cell`, the edifice before a conduit beneath it.
    pub fn structure_at(&self, cell: Cell) -> Option<&Structure> {
        self.structures
            .iter()
            .find(|s| s.edifice && s.footprint.contains(cell))
            .or_else(|| self.structures.iter().find(|s| s.footprint.contains(cell)))
    }

    /// Spawns a constructible with an existing identity.
    pub fn spawn_constructible(&mut self, c: Constructible) {
        self.structure_revision += 1;
        self.constructibles.push(c);
    }

    pub fn remove_constructible(&mut self, id: ItemId) -> Option<Constructible> {
        let at = self.constructibles.iter().position(|c| c.id == id)?;
        self.structure_revision += 1;
        Some(self.constructibles.remove(at))
    }

    /// Spawns (or with `None` removes) the building on `cell`.
    pub fn set_building(
        &mut self,
        cell: Cell,
        def: Option<DefId<ThingDef>>,
        stuff: Option<DefId<ThingDef>>,
    ) {
        self.buildings[cell] = def;
        self.building_stuff[cell] = stuff;
        self.structure_revision += 1;
        if def.is_none() {
            self.doors.retain(|d| d.cell != cell);
        }
    }

    /// Registers the door state of a door building on `cell`.
    pub fn add_door(&mut self, cell: Cell, ticks_to_open: i32, close_delay: i32) {
        self.doors.retain(|d| d.cell != cell);
        let id_number = self.allocate_item_id().0 as i32;
        self.doors.push(Door {
            cell,
            open: false,
            ticks_since_open: 0,
            ticks_until_close: 0,
            last_friendly_touch: i64::MIN / 2,
            ticks_to_open,
            close_delay,
            id_number,
            powered: false,
        });
    }

    pub fn doors(&self) -> &[Door] {
        &self.doors
    }

    pub fn door_at(&self, cell: Cell) -> Option<&Door> {
        self.doors.iter().find(|d| d.cell == cell)
    }

    pub fn door_at_mut(&mut self, cell: Cell) -> Option<&mut Door> {
        self.doors.iter_mut().find(|d| d.cell == cell)
    }

    pub fn doors_mut(&mut self) -> &mut [Door] {
        &mut self.doors
    }

    pub fn structure_revision(&self) -> u64 {
        self.structure_revision
    }

    /// Where a thing (item or constructible) is.
    pub fn thing_position(&self, id: ItemId) -> Option<Cell> {
        self.item(id)
            .map(|i| i.position)
            .or_else(|| self.constructible(id).map(|c| c.position))
            .or_else(|| self.structure(id).map(|s| s.footprint.center))
            .or_else(|| self.plant(id).map(|p| p.position))
    }

    /// Passability of a frame of `building`: what the building would
    /// block, a frame only lets pass through.
    /// A floor frame can be stood on (`NewFrameDef_Terrain`).
    pub fn frame_passability(defs: &GameDefs, building: Buildable) -> Passability {
        let Buildable::Thing(building) = building else {
            return Passability::Standable;
        };
        match defs.things[building].passability {
            Passability::Impassable => Passability::PassThroughOnly,
            p => p,
        }
    }

    /// `GenGrid.Standable` apart from walkability: no building or frame
    /// that cannot be stood on.
    pub fn standable_things(&self, defs: &GameDefs, c: Cell) -> bool {
        self.buildings[c].is_none_or(|b| defs.things[b].passability == Passability::Standable)
            // `GenGrid.Standable`: every thing there must be standable (a
            // chunk, PassThroughOnly, is not).
            && self
                .items_at(c)
                .all(|i| defs.things[i.def].passability == Passability::Standable)
            && self
                .plant_at(c)
                .is_none_or(|p| defs.things[p.def].passability == Passability::Standable)
            && self.constructibles.iter().all(|k| {
                !k.footprint().contains(c)
                    || k.stage == ConstructStage::Blueprint
                    || Self::frame_passability(defs, k.building) == Passability::Standable
            })
    }

    pub fn items_mut(&mut self) -> &mut [Item] {
        &mut self.items
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Takes up to `count` items from a stack; the stack disappears when
    /// emptied. Returns the number taken.
    /// `PostSplitOff`: a split-off part copies the cooked state.
    pub fn copy_meta(&mut self, from: ItemId, to: ItemId) {
        if let Some(m) = self.item_meta.get(&from).cloned() {
            self.item_meta.insert(to, m);
        }
    }

    /// `PreAbsorbStack`: `count` of `from` merging into `into` (which holds
    /// `into_count`): ingredients united in order, poison percent weighted
    /// by count, the cause of the larger share (the incoming one on ties,
    /// a known one over Unknown).
    // COMPATIBILITY TODO: currently approximate — CompIngredients' merge
    // pruning (important ingredients, at most three listed) is not
    // modelled.
    pub fn merge_meta(&mut self, into: ItemId, into_count: u32, from: ItemId, count: u32) {
        let a = self.item_meta.get(&into).cloned().unwrap_or_default();
        let Some(b) = self.item_meta.get(&from).cloned() else {
            if a != ItemMeta::default() && a.poison_pct > 0.0 {
                let mut m = a;
                m.poison_pct = m.poison_pct * into_count as f32 / (into_count + count) as f32;
                self.item_meta.insert(into, m);
            }
            return;
        };
        let mut m = a.clone();
        for d in &b.ingredients {
            if !m.ingredients.contains(d) {
                m.ingredients.push(*d);
            }
        }
        let wa = a.poison_pct * into_count as f32;
        let wb = b.poison_pct * count as f32;
        m.poison_cause = if a.poison_cause == PoisonCause::Unknown {
            b.poison_cause
        } else if b.poison_cause != PoisonCause::Unknown {
            if wa > wb {
                a.poison_cause
            } else {
                b.poison_cause
            }
        } else {
            a.poison_cause
        };
        m.poison_pct = (wa + wb) / (into_count + count) as f32;
        self.item_meta.insert(into, m);
    }

    pub fn take_from_item(&mut self, id: ItemId, count: u32) -> u32 {
        let Some(at) = self.items.iter().position(|i| i.id == id) else {
            return 0;
        };
        let item = &mut self.items[at];
        let taken = count.min(item.stack_count);
        item.stack_count -= taken;
        if item.stack_count == 0 {
            self.remove_item_at(at);
        }
        taken
    }

    pub fn items_at(&self, cell: Cell) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(move |i| i.position == cell)
    }

    /// The path grid derived from Def data (docs/research.md §13).
    pub fn build_path_grid(&self, defs: &GameDefs) -> PathGrid {
        // Cell cost as the game's path grid computes it: impassable terrain or
        // things block; otherwise terrain pathCost, raised to the highest
        // pathCost of the things on the cell (not summed).
        // COMPATIBILITY TODO: currently approximate — snow/sand build-up,
        // door transitions, fire and the pathCostIgnoreRepeat rule are not
        // modelled (none exist on the map yet).
        let mut costs = Grid::from_fn(self.size(), |c| {
            let terrain = &defs.terrain[self.terrain[c]];
            if terrain.passability == Passability::Impassable {
                return IMPASSABLE;
            }
            let mut cost = terrain.path_cost.max(0) as u32;
            if let Some(b) = self.buildings[c] {
                let thing = &defs.things[b];
                if thing.passability == Passability::Impassable {
                    return IMPASSABLE;
                }
                cost = cost.max(thing.path_cost.max(0) as u32);
            }
            cost.min(IMPASSABLE)
        });
        for plant in &self.plants {
            let thing = &defs.things[plant.def];
            let cell = &mut costs[plant.position];
            *cell = if thing.passability == Passability::Impassable {
                IMPASSABLE
            } else {
                (*cell).max(thing.path_cost.max(0) as u32).min(IMPASSABLE)
            };
        }
        for item in &self.items {
            let thing = &defs.things[item.def];
            let cell = &mut costs[item.position];
            *cell = if thing.passability == Passability::Impassable {
                IMPASSABLE
            } else {
                (*cell).max(thing.path_cost.max(0) as u32).min(IMPASSABLE)
            };
        }
        // Frames: the frame def's path cost; blueprints and floor frames
        // (`NewFrameDef_Terrain`: ethereal, no path cost) add nothing.
        for k in &self.constructibles {
            if k.stage != ConstructStage::Frame || k.building.floor().is_some() {
                continue;
            }
            for c in k.footprint().cells() {
                if !self.size().contains(c) {
                    continue;
                }
                let cell = &mut costs[c];
                if Self::frame_passability(defs, k.building) == Passability::Impassable {
                    *cell = IMPASSABLE;
                } else {
                    *cell = (*cell).clamp(FRAME_PATH_COST, IMPASSABLE);
                }
            }
        }
        // Buildings with "Full" fill (fillPercent > 0.99) block diagonal moves.
        let full = Grid::from_fn(self.size(), |c| {
            self.buildings[c].is_some_and(|b| defs.things[b].fill_percent > 0.99)
        });
        PathGrid::with_buildings(costs, full)
    }

    /// Nearest walkable cell to `around` (by Chebyshev rings), if any.
    pub fn nearest_walkable(&self, grid: &PathGrid, around: Cell) -> Option<Cell> {
        let size = self.size();
        let max_r = size.width.max(size.height);
        (0..=max_r).find_map(|r| {
            (-r..=r)
                .flat_map(|dx| (-r..=r).map(move |dz| Cell::new(around.x + dx, around.z + dz)))
                .filter(|c| c.chebyshev(around) == r)
                .find(|&c| grid.walkable(c))
        })
    }
}
