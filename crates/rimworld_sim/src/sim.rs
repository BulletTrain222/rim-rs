//! The simulation root: map, pawns, tick counter and commands.
//!
//! Runs at a fixed 60 ticks per simulated second (RimWorld's 1× speed). The
//! front-end sends [`Command`]s and calls [`Sim::tick`]; it never mutates
//! simulation state directly.

use std::sync::Arc;

use rimworld_defs::{DefId, GameDefs, PawnKindDef, ThingDef};

use crate::cell_finder::{IngestionSpotOrder, MapView, Wanderer};
use crate::food::{chew_ticks, ingested_count, spot_to_chew_standing_near, unit_nutrition};
use crate::grid::Cell;
use crate::hash::{
    hash_offset, is_hash_interval_tick, is_hash_interval_tick_delta, is_tick_interval,
};
use crate::haul::{MIN_HAUL_TICKS, Storable, StoreView, max_carry};
use crate::job::{CleanStage, HaulStage, WanderRoot};
use crate::job::{IngestStage, Job, JobDefs, JobKind, Rot4, WanderParams, think_idle};
use crate::map::{ItemId, Map};
use crate::needs::{NEED_INTERVAL_TICKS, NeedKind, Needs};
use crate::path::{
    COLONIST_HEURISTIC_STRENGTH, LocomotionUrgency, MoveCosts, Path, PathError, PathGrid,
    cost_paid_per_tick, find_path,
};
use crate::pawn::{Carried, Pawn, PawnId, Step};
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{
    Claimant, DestinationManager, JobId, ReservationManager, STACK_ALL, Target,
};
use crate::rest::{can_fall_asleep, should_wake_up};
use crate::storage::StoragePriority;
use crate::think::{FoodFacts, PawnFacts, ThinkContext, think};
use crate::work::{WorkSettings, giver_lists};

mod apparel;
mod butchering;
mod collapse;
mod combat;
mod construction;
mod cooking;
mod corpses;
mod crawling;
mod deconstructing;
mod designators;
mod farming;
pub mod filth;
mod flooring;
mod hunting;
mod joy;
mod lighting;
mod mining;
mod mood;
mod orders;
mod placing;
mod work_settings;
mod zones;
pub use designators::{OrderDesignator, Rejection, ThingRef};
pub use orders::{WorkOrderBlock, WorkOrderOption};
pub use zones::{ZoneKind, ZoneLabel, ZoneRef, ZoneReject};
pub mod power;
mod predation;
mod ranged;
mod refueling;
mod rescuing;
mod researching;
mod roofing;
mod rot;
mod steady;
mod temperature;
mod tending;
mod wildlife;
pub use rot::rot_rate_at_temperature;
mod save;
pub use combat::Swing;
pub use construction::frame_ready;
pub use corpses::RotStage;
pub use farming::plant_ready;
pub use save::{LoadError, SAVE_VERSION};

pub const TICKS_PER_SECOND: u32 = 60;
pub const TICKS_PER_HOUR: u64 = 2_500;
pub const TICKS_PER_DAY: u64 = 60_000;
/// Hour of day at tick 0.
// COMPATIBILITY TODO: currently approximate — the game's start time depends
// on the scenario/world; 06:00 is assumed.
pub const START_HOUR: u64 = 6;
/// `Pawn.Tick` runs `TickRare` on its 250-tick hash interval.
const BODY_HEAT_INTERVAL: i64 = 250;
/// Ticks from a job starting to its first movement payment (the game's
/// asynchronous path request; observed in the running game).
const PATH_START_LATENCY_TICKS: u64 = 2;
/// Interval of the path follower's "need a new path?" check.
const PATH_RECHECK_INTERVAL_TICKS: i64 = 30;
pub use crate::camera::OFFSCREEN_UPDATE_RATE;
/// Interval of the lying-down job override check.
const JOB_OVERRIDE_CHECK_TICKS: i64 = 211;
/// Rest effectiveness without a bed when the StatDef is missing.
const DEFAULT_GROUND_REST_EFFECTIVENESS: f32 = 0.8;

/// Speed used when a race has no `MoveSpeed` stat (RimWorld's StatDef default
/// is unknown to us yet; 4.6 is the human value).
// COMPATIBILITY TODO: currently approximate — should be the MoveSpeed StatDef default.
const FALLBACK_MOVE_SPEED: f32 = 4.6;
/// `CarryingCapacity` when neither the race nor the StatDef gives one.
const DEFAULT_CARRYING_CAPACITY: f32 = 75.0;
/// How long a pawn waits after its path failed (`ErroredPather`).
const PATHER_ERROR_WAIT_TICKS: u32 = 250;
/// Pawns that may share reservations of one food stack.
const MAX_FOOD_RESERVERS: i32 = 10;
/// `EatingSpeed` when neither the race nor the StatDef gives one.
const DEFAULT_EATING_SPEED: f32 = 1.0;

#[derive(Debug, Clone)]
pub enum Command {
    MoveTo { pawn: PawnId, target: Cell },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("no such pawn {0:?}")]
    NoSuchPawn(PawnId),
    #[error("{0}")]
    Path(#[from] PathError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpawnError {
    #[error("PawnKindDef {0} has no valid race ThingDef")]
    NoRace(String),
    #[error("cell {0:?} is not walkable")]
    NotWalkable(Cell),
    #[error("cell {0:?} is already claimed by another pawn")]
    Occupied(Cell),
}

pub struct Sim {
    pub defs: Arc<GameDefs>,
    pub map: Map,
    path_grid: PathGrid,
    pawns: Vec<Pawn>,
    next_pawn_id: u32,
    tick: u64,
    job_defs: JobDefs,
    pub wander: WanderParams,
    /// The difficulty's `colonistMoodOffset` in mood points (0: Rough,
    /// "Strive to survive").
    pub colonist_mood_offset: f32,
    rng: Rand,
    /// Projectiles in flight.
    projectiles: Vec<crate::ranged::Projectile>,
    /// `BedRestEffectiveness.valueIfMissing` from the StatDef (sleeping on
    /// the ground or a spot without the stat).
    ground_rest_effectiveness: f32,
    default_update_rate: u32,
    /// Chew spots imposed by [`Sim::debug_force_next_chew_spot`].
    forced_chew_spots: Vec<(PawnId, Cell)>,
    /// Ordinary reservations of things and cells (research §18).
    pub reservations: ReservationManager,
    /// Where pawns intend to stand (research §18).
    pub destinations: DestinationManager,
    next_job_id: JobId,
    /// The map's regions (research §19).
    regions: Regions,
    /// The game's persistent ingestion-spot shuffle lists.
    ingest_order: IngestionSpotOrder,
    /// Manual work priorities (`PlaySettings.useWorkPriorities`; off by
    /// default: every enabled work type counts as priority 3).
    pub use_work_priorities: bool,
    /// Materials on their way to blueprints and frames.
    enroute: crate::construct::EnrouteManager,
    /// The map's latitude (sun glow).
    // COMPATIBILITY TODO: currently approximate — there is no world map;
    // the latitude is a scenario setting (default 0).
    pub latitude: f32,
    /// Outdoor temperature in °C, used for every cell. With a
    /// [`Sim::climate`] it follows the season and the sun, refreshed every
    /// 60 ticks; without one it stays as set.
    // COMPATIBILITY TODO: currently approximate — room temperatures are not
    // modelled.
    pub outdoor_temperature: f32,
    /// Debug: forces whether mined rock drops its chunk (a replay
    /// following the recorded game's random outcome).
    debug_mine_drops: Option<bool>,
    /// Animals that reached the map edge this tick, removed after the pawn
    /// loop (`Pawn.ExitMap`).
    pending_exits: Vec<PawnId>,
    /// A float menu "Prioritize" query for the next `think_for` (instead
    /// of thinking), and its answer.
    work_query: Option<crate::work::WorkTarget>,
    /// Next zone palette positions (storage, growing).
    zone_colors: [usize; 2],
    work_query_out: Vec<(
        rimworld_defs::DefId<rimworld_defs::WorkGiverDef>,
        Option<crate::work::WorkJob>,
    )>,
    /// The world tile's climate (`None`: a constant temperature).
    pub climate: Option<crate::climate::Climate>,
    /// The map's longitude (local time of day).
    pub longitude: f32,
    /// Tick at which the cached outdoor temperature is next refreshed.
    next_temperature_refresh: u64,
    /// Shapes of the current rooms (`room_signature`), to notice changes.
    room_signatures: std::collections::HashSet<u64>,
    /// Rooms whose shape changed, for the auto-roof check next tick.
    queued_roof_rooms: Vec<u64>,
    /// Roof cells marked to collapse at the start of the next tick
    /// (`RoofCollapseBuffer`).
    roof_collapse: Vec<Cell>,
    /// Steady environment effects: the next cell in the shuffled order,
    /// the shuffle seed and the order itself (rebuilt from the seed).
    steady_cycle: usize,
    pub steady_seed: u32,
    steady_order: Vec<Cell>,
    /// Debug: a fixed sky glow (replays of traces without time of day).
    sky_glow_override: Option<f32>,
    /// Room temperatures.
    room_temps: temperature::RoomTemps,
    /// The building revision the light grid was computed for (`None`:
    /// recompute).
    light_key: Option<u64>,
    /// Melee swings not yet taken by [`Sim::take_swings`].
    last_swings: Vec<(PawnId, combat::Swing)>,
    /// Wild plant regrowth (`WildPlantSpawner`).
    wild: farming::WildPlants,
    /// Day of the year at tick 0 (the scenario's start date).
    // COMPATIBILITY TODO: currently approximate — no world calendar; a
    // scenario setting (default 0).
    pub start_day_of_year: i32,
    /// Corpse items and the dead pawns inside them.
    corpses: std::collections::BTreeMap<ItemId, PawnId>,
    /// Power nets (`PowerNetManager`).
    power: power::PowerGrid,
    /// The colony's research (`ResearchManager`).
    research: researching::ResearchState,
    /// Wealth and outdoor-room caches for recreation.
    joy_cache: joy::JoyCache,
    /// Work tables' bills (`BillStack`); the bill work giver changes them
    /// while thinking.
    bill_stacks: std::sync::Mutex<crate::cook::BillStacks>,
    /// Resolved recipe filters (derived).
    recipe_filters: std::collections::BTreeMap<
        rimworld_defs::DefId<rimworld_defs::RecipeDef>,
        crate::bills::RecipeFilters,
    >,
    /// `ResourceCounter`: stored counts by def (derived).
    stored_counts: std::collections::BTreeMap<DefId<ThingDef>, i32>,
}

impl Sim {
    pub fn new(defs: Arc<GameDefs>, map: Map) -> Self {
        Self::with_seed(defs, map, 0)
    }

    /// Creates a simulation whose random choices derive from `seed`.
    pub fn with_seed(defs: Arc<GameDefs>, map: Map, seed: u64) -> Self {
        let path_grid = map.build_path_grid(&defs);
        let regions = Regions::build(&map, &defs, &path_grid);
        let job_defs = JobDefs::resolve(&defs);
        let ground_rest_effectiveness = defs
            .stat_value_if_missing("BedRestEffectiveness")
            .unwrap_or(DEFAULT_GROUND_REST_EFFECTIVENESS);
        Self {
            defs,
            map,
            path_grid,
            pawns: Vec::new(),
            next_pawn_id: 0,
            tick: 0,
            job_defs,
            wander: WanderParams::default(),
            colonist_mood_offset: 0.0,
            rng: Rand::new(seed as u32),
            projectiles: Vec::new(),
            ground_rest_effectiveness,
            default_update_rate: OFFSCREEN_UPDATE_RATE,
            forced_chew_spots: Vec::new(),
            reservations: ReservationManager::default(),
            destinations: DestinationManager::default(),
            next_job_id: 1,
            regions,
            ingest_order: IngestionSpotOrder::default(),
            use_work_priorities: false,
            enroute: Default::default(),
            latitude: 0.0,
            outdoor_temperature: 21.0,
            debug_mine_drops: None,
            pending_exits: Vec::new(),
            work_query: None,
            zone_colors: [0, 0],
            work_query_out: Vec::new(),
            climate: None,
            longitude: 0.0,
            next_temperature_refresh: 0,
            room_signatures: Default::default(),
            queued_roof_rooms: Vec::new(),
            roof_collapse: Vec::new(),
            steady_cycle: 0,
            steady_seed: 0,
            corpses: Default::default(),
            research: Default::default(),
            joy_cache: Default::default(),
            bill_stacks: Default::default(),
            recipe_filters: Default::default(),
            stored_counts: Default::default(),
            power: Default::default(),
            steady_order: Vec::new(),
            sky_glow_override: None,
            room_temps: Default::default(),
            light_key: None,
            last_swings: Vec::new(),
            wild: Default::default(),
            start_day_of_year: 0,
        }
        .with_room_signatures()
    }

    /// The MoveSpeed light factor curve for a race (`StatPart_Glow`, only
    /// for humanlikes where the part says so).
    fn move_glow_curve(&self, race: DefId<ThingDef>) -> Vec<(f32, f32)> {
        let humanlike = self.defs.things[race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            == Some("Humanlike");
        self.defs
            .stats
            .get("MoveSpeed")
            .and_then(|s| s.glow_part.as_ref())
            .filter(|g| humanlike || !g.humanlike_only)
            .map(|g| g.curve.clone())
            .unwrap_or_default()
    }

    /// Records the starting rooms' shapes: rooms of a new map are not
    /// auto-roofed (the game only queues rooms changed while playing).
    fn with_room_signatures(mut self) -> Self {
        let rooms = self.regions.rooms().len();
        self.room_temps.temps = vec![self.outdoor_temperature; rooms];
        self.room_temps.reset_cycles(rooms);
        self.room_signatures = self
            .regions
            .rooms()
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| crate::roof::room_signature(&self.regions, r))
            .collect();
        self
    }

    /// UI text for a pawn's current job: the JobDef's `reportString` with
    /// `TargetA` replaced by the target's label, or for eating the food's
    /// own `ingestReportString` (`JobDriver_Ingest.GetReport`).
    pub fn job_report(&self, pawn: &Pawn) -> String {
        let Some(job) = &pawn.job else {
            return "idle.".to_owned();
        };
        // `JobDriver_DoBill.GetReport`: the recipe's job string.
        if let JobKind::DoBill { giver, bill, .. } = job.kind
            && let Some(b) = self
                .bill_stacks
                .lock()
                .expect("bill stacks")
                .get(&giver)
                .and_then(|s| s.iter().find(|b| b.id == bill))
        {
            let s = &self.defs.recipes[b.recipe].job_string;
            if !s.is_empty() {
                return s.clone();
            }
        }
        let base = job
            .def
            .map_or("idle.", |d| self.defs.jobs[d].report_string.as_str());
        // A pawn target is named by its name.
        // A predator eating its kill reports as eating (the Ingest string).
        if let JobKind::PredatorHunt {
            corpse: Some(c), ..
        } = job.kind
            && let Some(def) = self.map.item(c).map(|it| it.def)
        {
            let s = self.job_defs.ingest.map_or("eating TargetA.", |d| {
                self.defs.jobs[d].report_string.as_str()
            });
            return s.replace("TargetA", &self.defs.things[def].label);
        }
        if let JobKind::AttackStatic { target, .. }
        | JobKind::AttackMelee { target }
        | JobKind::Hunt { victim: target, .. }
        | JobKind::PredatorHunt { prey: target, .. } = job.kind
            && let Some(t) = self.pawn(target)
        {
            return base.replace("TargetA", &t.name);
        }
        let target = match job.kind {
            JobKind::Equip { item } => self.map.item(item).map(|i| i.def),
            JobKind::Mine { cell, .. } => self.map.buildings[cell],
            JobKind::IngestInPlace { food, .. } => self
                .map
                .item(food)
                .map(|i| i.def)
                .or_else(|| self.map.plant(food).map(|p| p.def)),
            JobKind::Ingest { food, .. } => pawn
                .carried
                .map(|c| c.def)
                .or_else(|| self.map.item(food).map(|i| i.def)),
            JobKind::Haul { source, .. } => pawn
                .carried
                .map(|c| c.def)
                .or_else(|| self.map.item(source).map(|i| i.def)),
            JobKind::Clean { target, .. } => target
                .or_else(|| pawn.target_queue.first().copied())
                .and_then(|t| self.map.item(t))
                .map(|i| i.def),
            JobKind::HaulToContainer { source, .. } => pawn
                .carried
                .map(|c| c.def)
                .or_else(|| self.map.item(source).map(|i| i.def)),
            JobKind::FinishFrame { frame, .. }
            | JobKind::PlaceNoCostFrame {
                blueprint: frame, ..
            } => self
                .map
                .constructible(frame)
                .and_then(|k| k.building.thing()),
            JobKind::Sow { plant, .. } => Some(plant),
            JobKind::Harvest { target, .. } => target
                .or_else(|| pawn.target_queue.first().copied())
                .and_then(|t| self.map.plant(t))
                .map(|p| p.def),
            _ => None,
        };
        let Some(target) = target else {
            return base.to_owned();
        };
        let def = &self.defs.things[target];
        if let Some(ing) = &def.ingestible {
            let tool_user = self.defs.things[pawn.race]
                .race
                .as_ref()
                .and_then(|r| r.intelligence.as_deref())
                .is_some_and(|i| i != "Animal");
            let report = match (&ing.ingest_report_string, &ing.ingest_report_string_eat) {
                (Some(r), Some(eat)) => Some(if tool_user { r } else { eat }),
                (None, Some(eat)) => Some(eat),
                (Some(r), None) => Some(r),
                (None, None) => None,
            };
            if let Some(report) = report {
                return report.replace("{0}", &def.label);
            }
        }
        base.replace("TargetA", &def.label)
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    pub fn path_grid(&self) -> &PathGrid {
        &self.path_grid
    }

    pub fn pawns(&self) -> &[Pawn] {
        &self.pawns
    }

    pub fn pawn(&self, id: PawnId) -> Option<&Pawn> {
        self.pawns.iter().find(|p| p.id == id)
    }

    #[cfg(test)]
    pub(crate) fn pawn_mut_for_tests(&mut self, id: PawnId) -> &mut Pawn {
        self.pawn_mut(id).expect("pawn exists")
    }

    fn pawn_mut(&mut self, id: PawnId) -> Option<&mut Pawn> {
        self.pawns.iter_mut().find(|p| p.id == id)
    }

    /// Pawn standing in (or stepping out of) `cell`.
    pub fn pawn_at(&self, cell: Cell) -> Option<&Pawn> {
        self.pawns.iter().find(|p| p.position == cell)
    }

    pub fn spawn_pawn(
        &mut self,
        kind: DefId<PawnKindDef>,
        name: impl Into<String>,
        at: Cell,
    ) -> Result<PawnId, SpawnError> {
        let kind_def = &self.defs.pawn_kinds[kind];
        let race = self
            .defs
            .race_of(kind_def)
            .filter(|&r| self.defs.things[r].is_pawn())
            .ok_or_else(|| SpawnError::NoRace(kind_def.def_name.clone()))?;
        if !self.path_grid.walkable(at) {
            return Err(SpawnError::NotWalkable(at));
        }
        if self.claimed_cells(None).contains(&at) {
            return Err(SpawnError::Occupied(at));
        }
        let think_tree = self.defs.things[race]
            .race
            .as_ref()
            .and_then(|r| r.think_tree_main.as_deref())
            .and_then(|t| self.defs.think_trees.id(t));
        let is_colonist = kind_def.default_faction.as_deref() == Some("PlayerColony");
        let speed = self.defs.things[race]
            .stat("MoveSpeed")
            .unwrap_or(FALLBACK_MOVE_SPEED);
        let needs = Needs::for_race(
            &self.defs,
            &self.defs.things[race],
            is_colonist,
            &mut self.rng,
        );
        // COMPATIBILITY TODO: currently approximate — EatingSpeed is the base
        // stat; the game scales it by the Eating capacity and other factors.
        let eating_speed = self
            .defs
            .base_stat(&self.defs.things[race], "EatingSpeed")
            .unwrap_or(DEFAULT_EATING_SPEED);
        let id = PawnId(self.next_pawn_id);
        self.next_pawn_id += 1;
        self.pawns.push(Pawn {
            id,
            name: name.into(),
            kind,
            race,
            position: at,
            move_costs: MoveCosts::from_move_speed(speed),
            base_move_speed: speed,
            move_glow_curve: self.move_glow_curve(race),
            move_capacity_factor: 1.0,
            crawl_break_next: false,
            step: None,
            path: Default::default(),
            destination: None,
            rotation: Rot4::South,
            job: None,
            next_idle_is_wait: true,
            think_tree,
            is_colonist,
            timetable: None,
            think_trail: Vec::new(),
            needs,
            asleep: false,
            // COMPATIBILITY TODO: currently approximate — the game draws
            // thingIDNumbers from a global counter shared by all things.
            id_number: id.0 as i32,
            update_rate: self.default_update_rate,
            tick_delta: 0,
            move_ready_tick: 0,
            carried: None,
            eating_speed,
            job_id: 0,
            target_queue: Vec::new(),
            target_queue_b: Vec::new(),
            count_queue: Vec::new(),
            placed_things: Vec::new(),
            skills: Default::default(),
            path_failed: false,
            entered_cell: false,
            filth: Default::default(),
            apparel: Vec::new(),
            carried_pawn: None,
            carried_by: None,
            stance_until: 0,
            door_request: None,
            left_door: None,
            owned_bed: None,
            health: Default::default(),
            mood: self.race_has_mood(race).then(Default::default),
            mind: Default::default(),
            drafted: false,
            equipment: None,
            stance: None,
            stat_overrides: Default::default(),
            carrying_capacity: self
                .defs
                .base_stat(&self.defs.things[race], "CarryingCapacity")
                .unwrap_or(DEFAULT_CARRYING_CAPACITY),
            // Player colonists get work settings when generated.
            // COMPATIBILITY TODO: currently approximate — no work type is
            // disabled (backstories, traits and health are not modelled).
            work: is_colonist.then(|| WorkSettings::initialize(&self.defs, &|_| false)),
        });
        let i = self.pawns.len() - 1;
        self.mood_spawned(i);
        // Animals update every 15 ticks whatever the camera does.
        if self.is_animal_index(i) {
            self.pawns[i].update_rate = 15;
        }
        Ok(id)
    }

    /// Computes (without applying) the path a pawn would take to `target`.
    /// A pawn in the middle of a step finishes that step first.
    pub fn plan_path(&self, pawn: PawnId, target: Cell) -> Result<Path, CommandError> {
        let p = self.pawn(pawn).ok_or(CommandError::NoSuchPawn(pawn))?;
        Ok(find_path(
            &self.path_grid,
            p.next_stop(),
            target,
            p.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        )?)
    }

    /// Cells claimed by every pawn except `except`.
    // COMPATIBILITY TODO: currently approximate — used only for spawning and
    // player orders; the game checks physical occupancy and drafted pawns'
    // destinations there (`RCellFinder.BestOrderedGotoDestNear`).
    pub fn claimed_cells(&self, except: Option<PawnId>) -> Vec<Cell> {
        self.pawns
            .iter()
            .filter(|p| Some(p.id) != except)
            .map(Pawn::claimed_cell)
            .collect()
    }

    /// The destination a move order to `target` will actually use: the target
    /// itself, or the nearest walkable cell no other pawn has claimed
    /// (pawns ordered as a group spread out instead of stacking).
    // COMPATIBILITY TODO: currently approximate — group-order cell spreading is not the
    // game's algorithm (drafted-pawn destination selection).
    pub fn order_destination(&self, pawn: PawnId, target: Cell) -> Result<Cell, CommandError> {
        if !self.map.size().contains(target) {
            return Err(PathError::GoalOutOfBounds.into());
        }
        if !self.path_grid.walkable(target) {
            return Err(PathError::GoalImpassable.into());
        }
        let claimed = self.claimed_cells(Some(pawn));
        if !claimed.contains(&target) {
            return Ok(target);
        }
        let size = self.map.size();
        let max_r = size.width.max(size.height);
        (1..=max_r)
            .find_map(|r| {
                (-r..=r)
                    .flat_map(|dx| (-r..=r).map(move |dz| target + Cell::new(dx, dz)))
                    .filter(|c| c.chebyshev(target) == r)
                    .find(|c| self.path_grid.walkable(*c) && !claimed.contains(c))
            })
            .ok_or(CommandError::Path(PathError::NoPath))
    }

    /// Applies a command. For `MoveTo`, returns the destination actually used.
    pub fn apply(&mut self, command: Command) -> Result<Cell, CommandError> {
        match command {
            Command::MoveTo { pawn, target } => {
                self.pawn(pawn).ok_or(CommandError::NoSuchPawn(pawn))?;
                let target = self.order_destination(pawn, target)?;
                self.plan_path(pawn, target)?;
                let goto = self.job_defs.goto;
                // A player order interrupts whatever the pawn was doing.
                let i = self.index_of(pawn).expect("checked");
                self.drop_carried(i);
                self.start_job(
                    i,
                    Job {
                        def: goto,
                        kind: JobKind::Goto { target },
                        forced: true,
                        urgency: LocomotionUrgency::Jog,
                        start_tick: 0,
                    },
                    false,
                );
                // After an order, the pawn idles in place before wandering.
                // COMPATIBILITY TODO: currently approximate — idle behaviour after a player order
                // is assumed.
                self.pawns[i].next_idle_is_wait = true;
                Ok(target)
            }
        }
    }

    /// Ticks since the colony started, offset to the start hour.
    /// `TileTemperaturesComp.CheckCache`: with a climate, the outdoor
    /// temperature is recomputed when its 60-tick cache runs out.
    fn refresh_outdoor_temperature(&mut self) {
        let Some(climate) = self.climate else {
            return;
        };
        if self.tick < self.next_temperature_refresh {
            return;
        }
        self.next_temperature_refresh = self.tick + crate::climate::CACHE_TICKS;
        let abs = self.abs_tick();
        self.outdoor_temperature = crate::climate::outdoor_temperature(
            &climate,
            self.latitude,
            abs,
            crate::climate::local_day_percent(abs, self.longitude),
        );
    }

    /// Absolute ticks since the start of year 0 (`TicksAbs`).
    pub(crate) fn abs_tick(&self) -> i64 {
        self.start_day_of_year as i64 * 60_000 + self.ticks_abs() as i64
    }

    pub(crate) fn ticks_abs(&self) -> u64 {
        self.tick + START_HOUR * TICKS_PER_HOUR
    }

    /// Local hour of day (0–23).
    pub fn hour_of_day(&self) -> u32 {
        ((self.ticks_abs() % TICKS_PER_DAY) / TICKS_PER_HOUR) as u32
    }

    /// Day number, starting at 1.
    pub fn day(&self) -> u64 {
        self.ticks_abs() / TICKS_PER_DAY + 1
    }

    /// Advances the simulation by one tick.
    ///
    /// Per pawn, as in the game (docs/research.md §14): first the per-tick
    /// part (job driver: movement, lying down), then — when the pawn's
    /// interval comes up — the interval part (job expiry, finding a job when
    /// idle, needs).
    pub fn tick(&mut self) {
        self.tick += 1;
        let t = self.tick;
        self.map.sky_glow = self
            .sky_glow_override
            .unwrap_or_else(|| self.outdoor_glow());
        self.refresh_outdoor_temperature();
        self.resolve_queued_roofs();
        self.collapse_marked_roofs();
        self.tick_room_temperatures();
        for i in 0..self.pawns.len() {
            // Dead pawns do nothing.
            if self.pawns[i].health.dead {
                continue;
            }
            if is_hash_interval_tick(t, self.pawns[i].id_number, BODY_HEAT_INTERVAL) {
                self.push_body_heat(i);
            }
            // A carried pawn only lives (health, needs); its carrier moves it.
            if self.pawns[i].carried_by.is_some() {
                self.interval_part(i);
                continue;
            }
            self.refresh_move_costs(i);
            // Per-tick: job driver (melee needs the other pawns).
            if matches!(
                self.pawns[i].job.as_ref().map(|j| j.kind),
                Some(JobKind::AttackMelee { .. })
            ) {
                self.melee_tick(i);
            }
            self.predator_tick(i);
            // Stances (warmup, cooldown) tick before the job; a busy pawn's
            // path follower doesn't move it (`PatherTick`).
            self.stance_tick(i);
            let busy = self.pawns[i].stance.is_some();
            // A job that failed here was replaced; the new one starts
            // driving next tick.
            let event = if busy || self.joy_tick_checks(i) {
                JobEvent::None
            } else {
                tick_job(
                    &mut self.pawns[i],
                    &self.map,
                    &self.defs,
                    &self.path_grid,
                    t,
                )
            };
            // A path that became blocked and could not be replaced.
            let event = if std::mem::take(&mut self.pawns[i].path_failed) {
                JobEvent::Failed
            } else {
                event
            };
            if std::mem::take(&mut self.pawns[i].entered_cell) {
                self.pawn_entered_cell(i);
            }
            match event {
                JobEvent::None => {}
                JobEvent::Ended(succeeded) => self.end_job(i, succeeded),
                JobEvent::ArrivedAtFood => self.ingest_pick_up(i),
                JobEvent::ArrivedAtChewSpot => self.ingest_start_chewing(i),
                JobEvent::DoneChewing => self.ingest_finalize(i),
                JobEvent::ArrivedAtFoodInPlace => self.ingest_in_place_arrived(i),
                JobEvent::ArrivedAtCorpse => self.predator_start_chew(i),
                JobEvent::DoneChewingCorpse => self.predator_finish_chew(i),
                JobEvent::ArrivedAtExit => self.exit_map_arrived(i),
                JobEvent::DoneChewingInPlace => self.ingest_in_place_finish(i),
                JobEvent::ArrivedAtFilth => self.clean_arrived(i),
                JobEvent::CleanTargetLost => self.clean_extract(i),
                JobEvent::ArrivedAtHaulSource => self.haul_pick_up(i),
                JobEvent::ArrivedAtHaulCell => {
                    self.set_haul(i, |_, _, _, stage| *stage = HaulStage::Delay)
                }
                JobEvent::ArrivedAtContainerSource => self.container_pick_up(i),
                JobEvent::ArrivedAtContainer => self.container_arrived(i),
                JobEvent::ArrivedAtFrame => self.start_building(i),
                JobEvent::ArrivedToSow => self.start_sowing(i),
                JobEvent::ArrivedToRoof => self.start_roofing(i),
                JobEvent::ArrivedToFloor => self.start_affect_floor(i),
                JobEvent::ArrivedToWall => self.start_smooth_wall(i),
                JobEvent::ArrivedAtPatient => self.rescue_pick_up(i),
                JobEvent::ArrivedAtBedWithPatient => self.rescue_tuck(i),
                JobEvent::ArrivedAtFoodForPatient => self.feed_pick_up(i),
                JobEvent::ArrivedToFeed => self.feed_start(i),
                JobEvent::FedPatient => self.feed_finish(i),
                JobEvent::ArrivedToTend => self.start_tend_wait(i),
                JobEvent::TendDone => self.finish_tend(i),
                JobEvent::ArrivedAtFuel => self.refuel_pick_up(i),
                JobEvent::ArrivedToMine => self.start_mining(i),
                JobEvent::ArrivedToDeconstruct => self.start_deconstructing(i),
                JobEvent::ArrivedToFlick => self.start_flick_wait(i),
                JobEvent::ArrivedToEquip => self.equip_arrived(i),
                JobEvent::ArrivedToPlaceFrame => self.place_frame_arrived(i),
                JobEvent::HuntTick => self.hunt_tick(i),
                JobEvent::ArrivedToResearch => self.start_research_toil(i),
                JobEvent::ArrivedForJoy => self.joy_arrived(i),
                JobEvent::ArrivedAtIngredient => self.bill_pick_up(i),
                JobEvent::ArrivedAtTableWithIngredient => self.bill_deliver(i),
                JobEvent::ArrivedToWorkBill => self.start_bill_work(i),
                JobEvent::BillTableUsed(table) => self.table_used_this_tick(table),
                JobEvent::Flicked => self.finish_flick(i),
                JobEvent::ArrivedAtComponent => self.fix_pick_up(i),
                JobEvent::ArrivedToFix => self.start_fix_work(i),
                JobEvent::Fixed => self.finish_fix(i),
                JobEvent::ArrivedToRefuel => self.start_refuel_wait(i),
                JobEvent::RefuelDone => self.finish_refuel(i),
                JobEvent::ArrivedToHarvest => self.harvest_arrived(i),
                JobEvent::HarvestTargetLost => self.harvest_extract(i),
                JobEvent::Failed => self.end_job(i, false),
                JobEvent::PatherError => self.end_job_pather_error(i),
            }
            self.handle_door_contact(i);
            self.sync_carried_pawn(i);
            // The equipment's verb (burst shots) after the job.
            self.verb_tick(i);
            // Lying awake: the lay-down toil checks for a job override every
            // 211 ticks (hash-staggered).
            let pawn = &self.pawns[i];
            if pawn.is_lying_down()
                && !pawn.asleep
                && !pawn.is_moving()
                && !pawn.job.as_ref().is_some_and(|j| j.forced)
                && is_hash_interval_tick(t, pawn.id_number, JOB_OVERRIDE_CHECK_TICKS)
            {
                self.check_for_job_override(i);
            }

            self.interval_part(i);
        }
        self.tick_projectiles();
        self.tick_doors();
        self.tick_fuel();
        self.tick_power_comps();
        self.tick_heat_pushers();
        self.tick_temp_control();
        self.tick_door_temperatures();
        self.tick_rot();
        self.tick_plants();
        self.tick_wild_plants();
        self.process_exits();
        self.wild_animal_spawner_tick();
        self.tick_power_nets();
        self.tick_resource_counter();
        self.tick_breakdowns();
        self.tick_steady_effects();
        self.sync_corpses();
        // Power nets and the light grid update after the tick
        // (`UpdatePowerNetsAndConnections_First`, `GlowGridUpdate_First` run
        // in `MapUpdate`).
        self.update_power_nets();
        self.refresh_light();
    }

    /// The pawn's body, health scale and bleed factor.
    fn body_of(&self, i: usize) -> Option<(&rimworld_defs::health::BodyDef, f32, f32)> {
        let race = self.defs.things[self.pawns[i].race].race.as_ref()?;
        let body = self.defs.bodies.get(race.body.as_deref()?)?;
        // COMPATIBILITY TODO: currently approximate — the life stage's
        // health scale factor is taken as 1 (adults).
        Some((body, race.base_health_scale, race.bleed_rate_factor))
    }

    /// A pawn's health queries.
    pub fn health_view(&self, pawn: PawnId) -> Option<crate::health::HealthView<'_>> {
        let i = self.index_of(pawn)?;
        let (body, scale, bleed) = self.body_of(i)?;
        Some(crate::health::HealthView {
            defs: &self.defs,
            body,
            hediffs: &self.pawns[i].health.hediffs,
            health_scale: scale,
            bleed_rate_factor: bleed,
        })
    }

    /// Damages a pawn (`DamageWorker_AddInjury`), on `part` or a random
    /// part, then updates its downed/dead state.
    pub fn damage_pawn(
        &mut self,
        pawn: PawnId,
        damage: &str,
        amount: f32,
        part: Option<usize>,
    ) -> Option<crate::health::DamageResult> {
        self.damage_pawn_at(pawn, damage, amount, part, None)
    }

    /// [`Sim::damage_pawn`] restricted to parts of one depth when no part
    /// is given (melee hits outside parts: `SetBodyRegion(Undefined,
    /// Outside)`).
    pub fn damage_pawn_at(
        &mut self,
        pawn: PawnId,
        damage: &str,
        amount: f32,
        part: Option<usize>,
        depth: Option<rimworld_defs::health::PartDepth>,
    ) -> Option<crate::health::DamageResult> {
        self.damage_pawn_full(pawn, damage, amount, -1.0, part, depth)
    }

    /// [`Sim::damage_pawn_at`] with an armor penetration (negative: the
    /// damage's `defaultArmorPenetration`).
    pub fn damage_pawn_full(
        &mut self,
        pawn: PawnId,
        damage: &str,
        amount: f32,
        armor_penetration: f32,
        part: Option<usize>,
        depth: Option<rimworld_defs::health::PartDepth>,
    ) -> Option<crate::health::DamageResult> {
        let i = self.index_of(pawn)?;
        let damage = self.defs.damages.id(damage)?;
        let armor_penetration = if armor_penetration < 0.0 {
            self.defs.damages[damage].default_armor_penetration
        } else {
            armor_penetration
        };
        let defs = self.defs.clone();
        let race = defs.things[self.pawns[i].race].race.as_ref()?;
        let body = defs.bodies.get(race.body.as_deref()?)?;
        let armor = self.armor_of(i, damage, armor_penetration);
        let result = crate::health::apply_damage(
            &mut self.pawns[i].health,
            &defs,
            body,
            race.base_health_scale,
            race.bleed_rate_factor,
            damage,
            amount,
            part,
            depth,
            &armor,
            &mut self.rng,
        );
        self.check_health_state(i);
        Some(result)
    }

    /// The armor pawn `i` wears and has against `damage`.
    // COMPATIBILITY TODO: currently approximate — apparel hit points and
    // quality are not modelled.
    fn armor_of(
        &self,
        i: usize,
        _damage: rimworld_defs::DefId<rimworld_defs::health::DamageDef>,
        penetration: f32,
    ) -> crate::health::Armor {
        let defs = &self.defs;
        let layers = self.pawns[i]
            .apparel
            .iter()
            .map(|a| {
                let def = &defs.things[a.def];
                let stuff = a.stuff.map(|s| &defs.things[s]);
                let r = |stat: &str| crate::stats::def_stat(defs, def, stuff, stat);
                crate::health::ArmorLayer {
                    groups: def.apparel_groups.clone(),
                    sharp: r("ArmorRating_Sharp"),
                    blunt: r("ArmorRating_Blunt"),
                    heat: r("ArmorRating_Heat"),
                }
            })
            .collect();
        let natural = crate::health::ArmorLayer {
            groups: Vec::new(),
            sharp: self.pawn_stat_of(i, "ArmorRating_Sharp"),
            blunt: self.pawn_stat_of(i, "ArmorRating_Blunt"),
            heat: self.pawn_stat_of(i, "ArmorRating_Heat"),
        };
        crate::health::Armor {
            layers,
            natural,
            penetration,
        }
    }

    /// `Pawn.TickRare` (every 250 ticks, hash-staggered, before the rest of
    /// the pawn's tick): a flesh pawn in air below 40 °C warms its room by
    /// 0.3 × body size heat per second (60% for animals), 250/60 seconds'
    /// worth.
    // COMPATIBILITY TODO: currently approximate — a pawn outside any room
    // shares the heat among the rooms around it instead of picking one at
    // random.
    fn push_body_heat(&mut self, i: usize) {
        let Some(race) = self.defs.things[self.pawns[i].race].race.as_ref() else {
            return;
        };
        if race.flesh_type.as_deref() == Some("Mechanoid") {
            return;
        }
        let at = self.pawns[i].position;
        if self.cell_temperature(at) >= 40.0 {
            return;
        }
        let humanlike = race.intelligence.as_deref() == Some("Humanlike");
        let energy = 0.3 * race.base_body_size * 4.166_666_5 * if humanlike { 1.0 } else { 0.6 };
        self.push_heat(at, energy);
    }

    /// Health interval work (`HealthTickInterval`).
    fn health_interval(&mut self, i: usize, delta: i32) {
        let t = self.tick;
        let id = self.pawns[i].id_number;
        let bleed_tick = is_hash_interval_tick_delta(t, id, 60, delta);
        let heal_tick = is_hash_interval_tick_delta(t, id, 600, delta);
        let lying = self.pawns[i].is_lying_down() || self.pawns[i].health.downed;
        let starving = self.pawns[i].needs.is_starving();
        let defs = self.defs.clone();
        let Some(race) = defs.things[self.pawns[i].race].race.as_ref() else {
            return;
        };
        let Some(body) = race.body.as_deref().and_then(|b| defs.bodies.get(b)) else {
            return;
        };
        let before = crate::health::state_key(&self.pawns[i].health, &defs);
        if !self.pawns[i].health.hediffs.is_empty() {
            self.hediff_interval(i, delta, bleed_tick, heal_tick, lying, starving);
        }
        // The other givers of the 60-tick interval, after bleeding.
        if bleed_tick && !self.pawns[i].health.dead {
            let ambient = self.cell_temperature(self.pawns[i].position);
            let comfy = (
                self.pawn_stat_of(i, "ComfyTemperatureMin"),
                self.pawn_stat_of(i, "ComfyTemperatureMax"),
            );
            let frostbite = crate::health::temperature_givers(
                &mut self.pawns[i].health,
                &defs,
                body,
                ambient,
                comfy,
                &mut self.rng,
            );
            if let Some(part) = frostbite {
                let hp = defs
                    .body_parts
                    .get(&body.parts[part].def)
                    .map_or(10, |d| d.hit_points);
                let amount = (hp as f32 * 0.5).ceil();
                let id = self.pawns[i].id;
                self.damage_pawn_at(id, "Frostbite", amount, Some(part), None);
            }
        }
        // Only a changed hediff can change the pawn's state.
        if crate::health::state_key(&self.pawns[i].health, &defs) != before {
            self.check_health_state(i);
        }
    }

    /// The hediff part of the health interval: healing, blood drops and
    /// blood loss.
    fn hediff_interval(
        &mut self,
        i: usize,
        delta: i32,
        bleed_tick: bool,
        heal_tick: bool,
        lying: bool,
        starving: bool,
    ) {
        let bed_heal = self
            .current_bed(i)
            .and_then(|b| self.map.structure(b))
            .and_then(|s| self.defs.things[s.def].building.as_ref())
            .map(|b| b.bed_heal_per_day);
        let defs = self.defs.clone();
        let Some(race) = defs.things[self.pawns[i].race].race.as_ref() else {
            return;
        };
        let Some(body) = race.body.as_deref().and_then(|b| defs.bodies.get(b)) else {
            return;
        };
        let blood = race.blood_def.as_deref().and_then(|b| defs.things.id(b));
        let comp_tick = is_hash_interval_tick_delta(self.tick, self.pawns[i].id_number, 200, delta);
        let drop_blood = crate::health::health_interval(
            &mut self.pawns[i].health,
            &defs,
            body,
            race.base_health_scale,
            race.bleed_rate_factor,
            delta,
            bleed_tick,
            heal_tick,
            comp_tick,
            lying,
            bed_heal,
            starving,
            blood.map(|_| race.base_body_size),
            &mut self.rng,
        );
        if drop_blood && let Some(blood) = blood {
            let cell = self.pawns[i].position;
            self.try_make_filth(cell, blood, 0, true);
        }
    }

    /// `Pawn.Kill`: the job and claims end, a carrier lets go, the Hunt
    /// designation goes (a hunted animal's corpse is not forbidden outside
    /// the home area) and the corpse is placed.
    pub(super) fn die(&mut self, i: usize) {
        let id = self.pawns[i].id;
        let hunted = self.map.hunt_designations.contains(&id);
        self.map.hunt_designations.retain(|&p| p != id);
        self.drop_carried(i);
        self.cleanup_job(i);
        // A carried pawn leaves its carrier's hands (its corpse lands
        // where the carrier stands).
        if let Some(carrier) = self.pawns[i].carried_by.take()
            && let Some(c) = self.index_of(carrier)
        {
            self.pawns[c].carried_pawn = None;
            self.end_job(c, false);
        }
        let p = &mut self.pawns[i];
        p.job = None;
        p.path.clear();
        p.step = None;
        p.destination = None;
        p.health.dead = true;
        p.health.downed = false;
        p.stance = None;
        self.destinations.release_all_claimed_by(id);
        self.reservations.release_all_claimed_by(id);
        self.reservations.release_all_for_target(Target::Pawn(id));
        self.spawn_corpse_hunted(i, hunted);
    }

    /// `CheckForStateChange`: death, or becoming downed or getting up.
    // COMPATIBILITY TODO: currently approximate — no corpse is spawned
    // (the dead pawn stays in place, inert); death-on-downed chances and
    // mental breaks are not modelled.
    fn check_health_state(&mut self, i: usize) {
        let Some(view) = self.health_view(self.pawns[i].id) else {
            return;
        };
        let dead = view.should_be_dead();
        let threshold = {
            let p = &self.pawns[i];
            crate::stats::pawn_stat(
                &self.defs,
                &self.defs.things[p.race],
                &p.skills,
                "PainShockThreshold",
            )
        };
        let downed = !dead && view.should_be_downed(threshold);
        // MoveSpeed's health part, kept for the path follower.
        let move_factor = self.defs.stats.get("MoveSpeed").map_or(1.0, |s| {
            crate::stats::capacity_factor(s, &|c| view.capacity(c))
        });
        self.pawns[i].move_capacity_factor = move_factor;
        let was_downed = self.pawns[i].health.downed;
        if dead {
            self.die(i);
            return;
        }
        if downed && !was_downed {
            self.drop_carried(i);
            self.cleanup_job(i);
            let p = &mut self.pawns[i];
            // `MakeDowned` undrafts and drops the weapon.
            // COMPATIBILITY TODO: currently approximate — the dropped
            // weapon is not forbidden for non-colonists and the bed
            // policy is not checked.
            p.drafted = false;
            p.stance = None;
            self.drop_equipment(i);
            let p = &mut self.pawns[i];
            p.crawl_break_next = true;
            p.job = None;
            p.path.clear();
            p.step = None;
            p.destination = None;
            p.asleep = false;
        }
        self.pawns[i].health.downed = downed;
        // Crawling and walking pay different move costs.
        if downed != was_downed {
            self.refresh_move_costs(i);
        }
    }

    /// Pawns lying in beds now: (pawn, bed, cell).
    fn bed_occupancy(&self) -> Vec<(PawnId, ItemId, Cell)> {
        self.pawns
            .iter()
            .filter_map(|p| match p.job.as_ref().map(|j| j.kind) {
                Some(JobKind::LayDown {
                    bed: Some(b), spot, ..
                }) if p.position == spot => Some((p.id, b, spot)),
                _ => None,
            })
            .collect()
    }

    /// `Pawn_Ownership.ClaimBedIfNonMedical`: the pawn gives up its old
    /// bed; a full bed drops its last owner.
    // COMPATIBILITY TODO: currently approximate — medical beds and
    // sleeping-slot assignment are not modelled.
    fn claim_bed(&mut self, i: usize, bed: ItemId) {
        let pawn = self.pawns[i].id;
        let Some(s) = self.map.structure(bed) else {
            return;
        };
        if s.owners.contains(&pawn) {
            return;
        }
        let slots = s.footprint.sleeping_slots() as usize;
        if let Some(old) = self.pawns[i].owned_bed.take()
            && let Some(o) = self.map.structure_mut(old)
        {
            o.owners.retain(|&p| p != pawn);
        }
        let s = self.map.structure_mut(bed).expect("checked");
        if s.owners.len() >= slots
            && let Some(evicted) = s.owners.pop()
            && let Some(e) = self.pawns.iter_mut().find(|p| p.id == evicted)
        {
            e.owned_bed = None;
        }
        let s = self.map.structure_mut(bed).expect("checked");
        s.owners.push(pawn);
        self.pawns[i].owned_bed = Some(bed);
    }

    /// Doors the pawn's path follower touched this tick: opening a closed
    /// door (`StartManualOpenBy`, then a cooldown stance until it is fully
    /// open) and starting to close the door it left (`StartManualCloseBy`).
    // COMPATIBILITY TODO: currently approximate — the order of door and
    // pawn ticks within a tick and the stance's exact expiry tick are not
    // runtime-verified; `Notify_PawnApproaching` touches are not recorded.
    fn handle_door_contact(&mut self, i: usize) {
        let t = self.tick as i64;
        if let Some(c) = self.pawns[i].door_request.take()
            && let Some(door) = self.map.door_at_mut(c)
        {
            door.last_friendly_touch = t;
            if !door.open {
                door.open_door(crate::map::DOOR_CLOSE_DELAY_TICKS);
            }
            self.pawns[i].stance_until = self.tick + door.ticks_till_fully_opened() as u64;
        }
        if let Some(c) = self.pawns[i].left_door.take() {
            let blocked = self.door_blocked_open(c);
            if let Some(door) = self.map.door_at_mut(c) {
                door.last_friendly_touch = t;
                if !blocked && door.slows_pawns() {
                    door.ticks_until_close = door.close_delay;
                }
            }
        }
    }

    /// `BlockedOpenMomentary`: an item or a pawn in the doorway.
    fn door_blocked_open(&self, c: Cell) -> bool {
        self.map.items_at(c).any(|it| !it.is_filth()) || self.pawns.iter().any(|p| p.position == c)
    }

    /// `Building_Door.Tick` for every door (doors tick after the pawns,
    /// which were spawned first).
    fn tick_doors(&mut self) {
        let t = self.tick as i64;
        for n in 0..self.map.doors().len() {
            let c = self.map.doors()[n].cell;
            let occupied = self.pawns.iter().any(|p| p.position == c);
            let blocked = self.door_blocked_open(c);
            let door = &mut self.map.doors_mut()[n];
            if !door.open {
                if door.ticks_since_open > 0 {
                    door.ticks_since_open -= 1;
                }
                continue;
            }
            if door.ticks_since_open < door.ticks_to_open {
                door.ticks_since_open += 1;
            }
            if occupied {
                door.last_friendly_touch = t;
            }
            if door.ticks_until_close > 0 {
                if occupied {
                    door.ticks_until_close = door.close_delay;
                }
                door.ticks_until_close -= 1;
                if door.ticks_until_close <= 0 {
                    if blocked {
                        door.ticks_until_close = 1;
                    } else {
                        door.open = false;
                    }
                }
            } else if t < door.last_friendly_touch + crate::map::DOOR_TOUCH_MEMORY_TICKS {
                door.ticks_until_close = door.close_delay;
            }
        }
    }

    /// `TicksPerMove` now: MoveSpeed with the light on the pawn's cell
    /// (`StatPart_Glow`) and × 0.6 while carrying a pawn. Path searches
    /// started this tick use it.
    fn refresh_move_costs(&mut self, i: usize) {
        let p = &self.pawns[i];
        if p.base_move_speed <= 0.0 {
            return;
        }
        // Crawling: CrawlSpeed (no light factor) instead of MoveSpeed.
        if p.health.downed {
            let crawl = self.pawn_stat_of(i, "CrawlSpeed");
            self.pawns[i].move_costs = MoveCosts::from_move_speed(crawl.max(0.01));
            return;
        }
        let mut speed = p.base_move_speed * p.move_capacity_factor;
        if !p.move_glow_curve.is_empty() {
            speed *=
                crate::food::evaluate_curve(&p.move_glow_curve, self.map.ground_glow(p.position));
        }
        if p.carried_pawn.is_some() {
            speed *= 0.6;
        }
        self.pawns[i].move_costs = MoveCosts::from_move_speed(speed);
    }

    /// The interval part of a pawn's tick when its update rate comes up.
    fn interval_part(&mut self, i: usize) {
        let t = self.tick;
        let pawn = &mut self.pawns[i];
        pawn.tick_delta += 1;
        let rate = pawn.update_rate.clamp(1, 15);
        if pawn.tick_delta >= rate || is_tick_interval(t, hash_offset(pawn.id_number), rate as i32)
        {
            let delta = pawn.tick_delta;
            pawn.tick_delta = 0;
            self.tick_interval(i, delta as i32);
        }
    }

    /// The interval part of a pawn's tick, covering `delta` ticks.
    fn tick_interval(&mut self, i: usize, delta: i32) {
        // `SkillsTickInterval` on the 200-tick hash interval.
        if is_hash_interval_tick_delta(self.tick, self.pawns[i].id_number, 200, delta) {
            let hour = self.hour_of_day();
            let tick = self.tick as i64;
            self.pawns[i].skills.interval(tick, hour);
        }
        // Driver interval work (`JobDriver.DriverTickInterval`).
        self.clean_interval(i, delta);
        self.haul_interval(i);
        // A goto toil that ends here starts the build toil, whose interval
        // work begins on its next interval.
        if !self.goto_build_interval(i) {
            self.build_interval(i, delta);
        }
        self.sow_interval(i, delta);
        self.roof_interval(i, delta);
        self.mine_interval(i, delta);
        self.deconstruct_interval(i, delta);
        self.affect_floor_interval(i, delta);
        self.smooth_wall_interval(i, delta);
        self.harvest_interval(i, delta);
        self.research_interval(i, delta);
        self.joy_interval(i, delta);
        self.do_bill_interval(i, delta);
        self.attack_static_interval(i);
        self.melee_interval(i);
        let t = self.tick;
        // Job tracker: job expiry, then find a job if idle.
        let pawn = &self.pawns[i];
        if let Some(job) = &pawn.job
            && let JobKind::Wait { expiry_interval } = job.kind
            && t != job.start_tick
            && job.start_tick + expiry_interval as u64 <= t
            && is_hash_interval_tick_delta(t, pawn.id_number, expiry_interval.max(1) as i32, delta)
        {
            self.end_job(i, true);
        }
        // Health: bleeding, healing and the downed/dead checks.
        self.health_interval(i, delta);
        if self.pawns[i].health.dead {
            return;
        }
        // Mind state: mental states and breaks (before the needs).
        self.mind_state_interval(i, delta);
        if self.pawns[i].job.is_none() && self.pawns[i].carried_by.is_none() {
            self.find_and_start_job(i);
        }
        // A downed pawn that can no longer crawl stops crawling (the goto
        // toil's fail condition).
        if self.pawns[i].health.downed
            && self.pawns[i].is_moving()
            && matches!(
                self.pawns[i].job.as_ref().map(|j| j.kind),
                Some(JobKind::LayDown { .. })
            )
            && !self.can_crawl(i)
        {
            self.end_job(i, false);
        }
        // Needs.
        let bed = match self.pawns[i].job.as_ref().map(|j| j.kind) {
            Some(JobKind::LayDown { bed, spot }) if self.pawns[i].position == spot => bed,
            _ => None,
        };
        let effectiveness = crate::rest::rest_effectiveness(
            &self.defs,
            &self.map,
            bed,
            self.ground_rest_effectiveness,
        );
        let hunger_factor = self
            .health_view(self.pawns[i].id)
            .map_or(1.0, |v| v.hunger_rate_factor());
        let pawn = &mut self.pawns[i];
        if is_hash_interval_tick_delta(t, pawn.id_number, NEED_INTERVAL_TICKS as i32, delta) {
            // Mood comes first in the needs list (`listPriority` 1000).
            self.mood_need_interval(i);
            let pawn = &mut self.pawns[i];
            let resting = pawn.asleep.then_some(effectiveness);
            let exhausted = pawn.needs.interval(resting, hunger_factor, &mut self.rng);
            self.joy_need_interval(i);
            self.malnutrition_interval(i);
            if self.pawns[i].health.dead {
                return;
            }
            if exhausted && !self.pawns[i].asleep {
                self.start_involuntary_sleep(i);
            }
        }
    }

    /// The food need's malnutrition step (`Need_Food.NeedInterval`):
    /// starving adds malnutrition, eating takes it away; it can kill.
    fn malnutrition_interval(&mut self, i: usize) {
        let Some(def) = self.defs.hediffs.id("Malnutrition") else {
            return;
        };
        let pawn = &self.pawns[i];
        if pawn.needs.get(NeedKind::Food).is_none() {
            return;
        }
        let step = crate::health::malnutrition_per_interval(pawn.id_number);
        let change = if pawn.needs.is_starving() {
            step
        } else {
            -step
        };
        let before = crate::health::state_key(&pawn.health, &self.defs);
        crate::health::adjust_severity(&mut self.pawns[i].health, def, change);
        if crate::health::state_key(&self.pawns[i].health, &self.defs) != before {
            self.check_health_state(i);
        }
    }

    /// Ends pawn `i`'s job. After a succeeded job (other than a posture wait
    /// or a player Goto) a standing pawn first holds its posture until its
    /// next interval tick; otherwise it looks for a new job at once.
    fn end_job(&mut self, i: usize, succeeded: bool) {
        let posture = self.job_defs.wait_maintain_posture;
        let goto = self.job_defs.goto;
        self.drop_carried(i);
        self.cleanup_job(i);
        let pawn = &mut self.pawns[i];
        let ended = pawn.job.take().and_then(|j| j.def);
        pawn.asleep = false;
        let hold_posture =
            succeeded && ended != posture && (ended != goto || goto.is_none()) && !pawn.is_moving();
        if hold_posture {
            self.start_job(
                i,
                Job {
                    def: posture,
                    kind: JobKind::Wait { expiry_interval: 1 },
                    forced: false,
                    urgency: LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                false,
            );
        } else {
            self.find_and_start_job(i);
        }
    }

    /// Thinks and starts the resulting job (the game's TryFindAndStartJob).
    fn find_and_start_job(&mut self, i: usize) {
        if self.pawns[i].health.downed {
            if self.pawns[i].carried_by.is_none() {
                let job = self.downed_job(i);
                self.start_job(i, job, false);
            }
            return;
        }
        let hour = self.hour_of_day();
        let mut thought = self.think_for(i, hour);
        self.reserve_food(i, &mut thought);
        if let Some(flag) = thought.next_idle_is_wait {
            self.pawns[i].next_idle_is_wait = flag;
        }
        match thought.job {
            Some(job) => {
                self.pawns[i].think_trail = thought.trail;
                self.pawns[i].target_queue = thought.queue;
                if !thought.bill_queue.is_empty() {
                    let (ids, counts) = thought.bill_queue.into_iter().unzip();
                    self.pawns[i].target_queue_b = ids;
                    self.pawns[i].count_queue = counts;
                }
                self.start_job(i, job, false);
            }
            None => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
            }
        }
    }

    fn index_of(&self, pawn: PawnId) -> Option<usize> {
        self.pawns.iter().position(|p| p.id == pawn)
    }

    fn claimant(&self, i: usize) -> Claimant {
        let p = &self.pawns[i];
        Claimant {
            pawn: p.id,
            has_faction: p.is_colonist,
        }
    }

    /// Job cleanup (`Pawn.ClearReservationsForJob`): the current job's
    /// ordinary reservations go; its destination stays without a job.
    fn cleanup_job(&mut self, i: usize) {
        self.sow_cleanup(i);
        let p = &self.pawns[i];
        if p.job.is_some() {
            self.reservations.release_claimed_by(p.id, p.job_id);
            self.destinations.release_claimed_by(p.id, p.job_id);
            self.enroute.release_all_claimed_by(p.id);
        }
    }

    /// Starts `job` now (`Pawn_JobTracker.StartJob`): cleans up the current
    /// job, makes the new job's reservations (`TryMakePreToilReservations`)
    /// and starts its path. Returns `false` (no job) if a reservation or the
    /// path fails; the pawn thinks again on its next interval tick.
    fn start_job(&mut self, i: usize, mut job: Job, force_sleep: bool) -> bool {
        self.cleanup_job(i);
        // `Job.placedThings` belongs to the job.
        self.pawns[i].placed_things.clear();
        let id = self.next_job_id;
        self.next_job_id += 1;
        let claimant = self.claimant(i);
        let t = self.tick;
        let at = self.pawns[i].next_stop();
        let ok = match job.kind {
            JobKind::Goto { target } => {
                self.destinations.reserve(claimant, id, target);
                true
            }
            // A bed is reserved for its sleeping slots (stack 0); the ground
            // spot as a cell (`JobDriver_LayDown.TryMakePreToilReservations`).
            JobKind::LayDown { bed: Some(bed), .. } => {
                match self
                    .map
                    .structure(bed)
                    .map(|s| s.footprint.sleeping_slots())
                {
                    Some(slots) => {
                        self.reservations
                            .reserve(claimant, id, Target::Item(bed), 1, slots, 0)
                    }
                    None => false,
                }
            }
            JobKind::LayDown { spot, bed: None } => {
                force_sleep
                    || self
                        .reservations
                        .reserve(claimant, id, Target::Cell(spot), 1, 1, STACK_ALL)
            }
            JobKind::Ingest { food, reserved, .. } => match self.map.item(food) {
                Some(item) if claimant.has_faction => self.reservations.reserve(
                    claimant,
                    id,
                    Target::Item(food),
                    item.stack_count as i32,
                    MAX_FOOD_RESERVERS,
                    reserved as i32,
                ),
                Some(_) => true,
                None => false,
            },
            JobKind::Wait { .. }
            | JobKind::AttackMelee { .. }
            | JobKind::WaitCombat
            | JobKind::AttackStatic { .. } => true,
            JobKind::Hunt { victim, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Pawn(victim), 1, 1, STACK_ALL)
            }
            JobKind::Flee { .. } | JobKind::PredatorHunt { .. } => true,
            JobKind::ExitMap { dest } => {
                self.destinations.reserve(claimant, id, dest);
                true
            }
            JobKind::IngestInPlace { food, count, .. } => {
                let stack = self.map.item(food).map_or(1, |it| it.stack_count as i32);
                (self.map.item(food).is_some() || self.map.plant(food).is_some())
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(food),
                        stack,
                        10,
                        count as i32,
                    )
            }
            JobKind::Equip { item } => {
                self.reservations
                    .reserve(claimant, id, Target::Item(item), 1, 1, STACK_ALL)
            }
            JobKind::PlaceNoCostFrame { blueprint, .. } => {
                self.map.constructible(blueprint).is_some()
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(blueprint),
                        1,
                        1,
                        STACK_ALL,
                    )
            }
            // Destination cell first, then the whole source, each for one
            // pawn (`JobDriver_HaulToCell.TryMakePreToilReservations`).
            JobKind::Haul { source, dest, .. } => {
                let stack = self.map.item(source).map_or(1, |it| it.stack_count as i32);
                self.reservations
                    .reserve(claimant, id, Target::Cell(dest), 1, 1, STACK_ALL)
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(source),
                        stack,
                        1,
                        STACK_ALL,
                    )
            }
            JobKind::HaulToContainer { .. } => {
                let kind = job.kind;
                self.reserve_container_haul(i, id, &kind)
            }
            JobKind::Sow { cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL)
            }
            JobKind::BuildRoof { cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Ceiling(cell), 1, 1, STACK_ALL)
            }
            JobKind::AffectFloor { cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Floor(cell), 1, 1, STACK_ALL)
            }
            JobKind::SmoothWall { cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL)
            }
            // The patient, then the food (`Reserve(food, 10, count)`).
            JobKind::FeedPatient {
                food,
                patient,
                count,
                ..
            } => {
                self.reservations
                    .reserve(claimant, id, Target::Pawn(patient), 1, 1, STACK_ALL)
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(food),
                        10,
                        count as i32,
                        STACK_ALL,
                    )
            }
            JobKind::TendPatient { patient, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Pawn(patient), 1, 1, STACK_ALL)
            }
            // `TryMakePreToilReservations`: the patient, then the bed's
            // sleeping slots.
            JobKind::Rescue { patient, bed, .. } => {
                let slots = self
                    .map
                    .structure(bed)
                    .map_or(1, |s| s.footprint.sleeping_slots());
                self.reservations
                    .reserve(claimant, id, Target::Pawn(patient), 1, 1, STACK_ALL)
                    && self
                        .reservations
                        .reserve(claimant, id, Target::Item(bed), 1, slots, 0)
            }
            JobKind::Deconstruct { building, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Item(building), 1, 1, STACK_ALL)
            }
            JobKind::Flick { target, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Item(target), 1, 1, STACK_ALL)
            }
            // The table, its interaction cell, then as many queued
            // ingredients as possible (`ReserveAsManyAsPossible`).
            JobKind::DoBill { giver, .. } => {
                let cell = self.interaction_cell(giver);
                let ok =
                    self.reservations
                        .reserve(claimant, id, Target::Item(giver), 1, 1, STACK_ALL)
                        && cell.is_none_or(|c| {
                            self.reservations.reserve(
                                claimant,
                                id,
                                Target::Cell(c),
                                1,
                                1,
                                STACK_ALL,
                            )
                        });
                if ok {
                    for q in self.pawns[i].target_queue_b.clone() {
                        let stack = self.map.item(q).map_or(1, |it| it.stack_count as i32);
                        if self.reservations.can_reserve(
                            claimant,
                            Target::Item(q),
                            stack,
                            1,
                            STACK_ALL,
                        ) {
                            self.reservations.reserve(
                                claimant,
                                id,
                                Target::Item(q),
                                stack,
                                1,
                                STACK_ALL,
                            );
                        }
                    }
                }
                ok
            }
            // Relaxing reserves its spot; walks and skygazing reserve
            // nothing.
            JobKind::Joy {
                activity: crate::job::JoyActivity::Relax { cell },
                ..
            } => self
                .reservations
                .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL),
            JobKind::Joy { .. } => true,
            // The bench, then its interaction cell.
            JobKind::Research { bench, cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Item(bench), 1, 1, STACK_ALL)
                    && self
                        .reservations
                        .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL)
            }
            // The building, then the component.
            JobKind::FixBrokenDown {
                building,
                component,
                ..
            } => {
                self.reservations
                    .reserve(claimant, id, Target::Item(building), 1, 1, STACK_ALL)
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(component),
                        1,
                        1,
                        STACK_ALL,
                    )
            }
            JobKind::Mine { cell, .. } => {
                self.reservations
                    .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL)
            }
            // The building, then the whole fuel stack, for this pawn.
            JobKind::Refuel { building, fuel, .. } => {
                let stack = self.map.item(fuel).map_or(1, |it| it.stack_count as i32);
                self.reservations
                    .reserve(claimant, id, Target::Item(building), 1, 1, STACK_ALL)
                    && self.reservations.reserve(
                        claimant,
                        id,
                        Target::Item(fuel),
                        stack,
                        1,
                        STACK_ALL,
                    )
            }
            // `ReserveAsManyAsPossible` over the queued plants.
            JobKind::Harvest { .. } => {
                let queue = self.pawns[i].target_queue.clone();
                for t in queue {
                    if self.map.plant(t).is_some()
                        && self
                            .reservations
                            .can_reserve(claimant, Target::Item(t), 1, 1, STACK_ALL)
                    {
                        self.reservations
                            .reserve(claimant, id, Target::Item(t), 1, 1, STACK_ALL);
                    }
                }
                true
            }
            JobKind::FinishFrame { frame, .. } => {
                self.map.constructible(frame).is_some()
                    && self
                        .reservations
                        .reserve(claimant, id, Target::Item(frame), 1, 1, STACK_ALL)
            }
            // `ReserveAsManyAsPossible` over the queue; never fails.
            JobKind::Clean { .. } => {
                let queue = self.pawns[i].target_queue.clone();
                for t in queue {
                    if self.map.item(t).is_some()
                        && self
                            .reservations
                            .can_reserve(claimant, Target::Item(t), 1, 1, STACK_ALL)
                    {
                        self.reservations
                            .reserve(claimant, id, Target::Item(t), 1, 1, STACK_ALL);
                    }
                }
                true
            }
        };
        let haul_source = match job.kind {
            JobKind::Haul { source, .. } => self.map.item(source).map(|it| it.position),
            _ => None,
        };
        // Construction: the resource's cell, or a spot touching the frame.
        let construct_target = match job.kind {
            JobKind::HaulToContainer { source, .. } if ok => {
                self.map.item(source).map(|it| (it.position, true))
            }
            JobKind::FinishFrame { frame, .. } if ok => self
                .map
                .constructible(frame)
                .map(|k| k.footprint())
                .map(|fp| (self.touch_spot(i, &fp).unwrap_or(fp.center), false)),
            _ => None,
        };
        let (target, target_is_thing) = match job.kind {
            JobKind::Goto { target } => (Some(target), false),
            JobKind::HaulToContainer { .. } | JobKind::FinishFrame { .. } => match construct_target
            {
                Some((c, thing)) if c != at => (Some(c), thing),
                _ => (None, false),
            },
            // `GotoThing(ClosestTouch)`: onto the item's cell.
            JobKind::Haul { .. } => match haul_source {
                Some(c) if c != at => (Some(c), true),
                _ => (None, false),
            },
            JobKind::LayDown { spot, .. } if spot != at => (Some(spot), false),
            JobKind::Ingest {
                stage: IngestStage::GotoFood { dest },
                ..
            } if dest != at => (Some(dest), true),
            _ => (None, false),
        };
        let path = match target {
            Some(target) if ok => find_path(
                &self.path_grid,
                at,
                target,
                self.pawns[i].move_costs,
                COLONIST_HEURISTIC_STRENGTH,
            )
            .ok(),
            _ => None,
        };
        let pawn = &mut self.pawns[i];
        pawn.path.clear();
        pawn.destination = None;
        pawn.job_id = id;
        if !ok || (target.is_some() && path.is_none()) {
            // Not started: release whatever was reserved.
            pawn.job = Some(job);
            self.cleanup_job(i);
            self.pawns[i].job = None;
            return false;
        }
        job.start_tick = t;
        if let JobKind::Haul { start_tick, .. } = &mut job.kind {
            *start_tick = t;
        }
        let hauling_here = matches!(job.kind, JobKind::Haul { .. }) && target.is_none();
        let claim = match job.kind {
            JobKind::LayDown { bed: Some(bed), .. } => Some(bed),
            _ => None,
        };
        let container_here =
            matches!(job.kind, JobKind::HaulToContainer { .. }) && target.is_none();
        let building_here = matches!(job.kind, JobKind::FinishFrame { .. }) && target.is_none();
        pawn.job = Some(job);
        pawn.move_ready_tick = t + PATH_START_LATENCY_TICKS;
        pawn.asleep = force_sleep;
        let cleaning = matches!(
            pawn.job.as_ref().map(|j| &j.kind),
            Some(JobKind::Clean { .. })
        );
        if let (Some(target), Some(path)) = (target, path) {
            pawn.path = path.cells.into();
            pawn.destination = Some(target);
            self.on_start_path(i, target, target_is_thing);
        }
        if cleaning {
            self.clean_extract(i);
        }
        // Already at the source: the instant toils run on, so the pickup
        // happens while the job starts.
        if hauling_here {
            self.haul_pick_up(i);
        }
        if container_here {
            self.container_pick_up(i);
        }
        if building_here {
            self.start_building(i);
        }
        // `ClaimBedIfNonMedical` (the first toil of a bed lay-down).
        if let Some(bed) = claim {
            self.claim_bed(i, bed);
        }
        match self.pawns[i].job.as_ref().map(|j| j.kind) {
            Some(JobKind::Sow { .. }) if !self.begin_sow(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::BuildRoof { .. }) if !self.begin_roof(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::AffectFloor { .. }) if !self.begin_affect_floor(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::SmoothWall { .. }) if !self.begin_smooth_wall(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Rescue { .. }) if !self.begin_rescue(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::FeedPatient { .. }) if !self.begin_feed(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::TendPatient { .. }) if !self.begin_tend(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::FixBrokenDown { .. }) if !self.begin_fix(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::DoBill { .. }) if !self.begin_do_bill(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Joy { .. }) if !self.begin_joy(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Research { .. }) if !self.begin_research(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Hunt { .. }) if !self.begin_hunt(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::ExitMap { .. }) if !self.begin_exit_map(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::IngestInPlace { .. }) if !self.begin_ingest_in_place(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Flee { .. }) if !self.begin_flee(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Equip { .. }) if !self.begin_equip(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::PlaceNoCostFrame { .. }) if !self.begin_place_no_cost_frame(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Flick { .. }) if !self.begin_flick(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Deconstruct { .. }) if !self.begin_deconstruct(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Mine { .. }) if !self.begin_mine(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Refuel { .. }) if !self.begin_refuel(i) => {
                self.cleanup_job(i);
                self.pawns[i].job = None;
                return false;
            }
            Some(JobKind::Harvest { .. }) => self.harvest_extract(i),
            _ => {}
        }
        true
    }

    /// Cleaning: drop queued filth that is gone, then take the next one and
    /// walk to touch it; with nothing left the job succeeds
    /// (`ClearDespawnedNullOrForbiddenQueuedTargets`,
    /// `SucceedOnNoTargetInQueue`, `ExtractNextTargetFromQueue`, `GotoThing`).
    fn clean_extract(&mut self, i: usize) {
        let map = &self.map;
        self.pawns[i]
            .target_queue
            .retain(|&t| map.item(t).is_some());
        if self.pawns[i].target_queue.is_empty() {
            self.end_job(i, true);
            return;
        }
        let t = self.pawns[i].target_queue.remove(0);
        let cell = self.map.item(t).expect("kept above").position;
        let tick = self.tick;
        let pawn = &mut self.pawns[i];
        set_clean(pawn, Some(t), CleanStage::Goto);
        pawn.path.clear();
        pawn.destination = None;
        let at = pawn.next_stop();
        if at.chebyshev(cell) <= 1 {
            self.clean_arrived(i);
            return;
        }
        match find_path(
            &self.path_grid,
            at,
            cell,
            pawn.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) => {
                // `PathEndMode.Touch`: stop on the first cell touching it.
                // COMPATIBILITY TODO: currently approximate — the game's
                // path search ends at any touching cell; we cut the path to
                // the thing's own cell instead.
                let mut cells: Vec<Cell> = Vec::new();
                for c in path.cells {
                    cells.push(c);
                    if c.chebyshev(cell) <= 1 {
                        break;
                    }
                }
                let end = *cells.last().expect("non-empty");
                pawn.path = cells.into();
                pawn.destination = Some(end);
                pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, cell, true);
            }
            Err(_) => self.clean_extract(i),
        }
    }

    /// Cleaning: next to the filth; cleaning starts.
    fn clean_arrived(&mut self, i: usize) {
        let pawn = &mut self.pawns[i];
        if let Some(Job {
            kind: JobKind::Clean { target, .. },
            ..
        }) = pawn.job
        {
            set_clean(pawn, target, CleanStage::Cleaning { work_done: 0.0 });
        }
    }

    /// Cleaning progress, in the pawn's interval update covering `delta`
    /// ticks: `CleaningSpeed × delta / CleaningTimeFactor` of the terrain;
    /// each time the work exceeds the filth's `cleaningWorkToReduceThickness`
    /// one thickness level goes and the count restarts.
    // COMPATIBILITY TODO: currently approximate — CleaningSpeed is the base
    // stat without stat parts.
    fn clean_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Clean {
                    target: Some(t),
                    stage: CleanStage::Cleaning { work_done },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(item) = self.map.item(t) else {
            return;
        };
        let defs = &self.defs;
        let per_level = defs.things[item.def]
            .filth
            .as_ref()
            .map_or(35.0, |f| f.cleaning_work_to_reduce_thickness);
        let terrain = &defs.terrain[self.map.terrain[item.position]];
        let factor = terrain
            .stat_bases
            .get("CleaningTimeFactor")
            .copied()
            .or_else(|| defs.stat_default("CleaningTimeFactor"))
            .unwrap_or(1.0);
        let speed = defs
            .base_stat(&defs.things[self.pawns[i].race], "CleaningSpeed")
            .unwrap_or(1.0);
        let mut work = speed * delta as f32;
        if factor != 0.0 {
            work /= factor;
        }
        let mut done = work_done + work;
        if done > per_level {
            done = 0.0;
            if self.map.thin_filth(t) {
                self.reservations.release_all_for_target(Target::Item(t));
                self.refresh_path_grid();
                self.clean_extract(i);
                return;
            }
        }
        set_clean(
            &mut self.pawns[i],
            Some(t),
            CleanStage::Cleaning { work_done: done },
        );
    }

    /// `Pawn_PathFollower.StartPath`: the pawn's older destinations become
    /// obsolete when its newest one is elsewhere and either the path leads
    /// to a thing or that destination belongs to another job.
    fn on_start_path(&mut self, i: usize, dest: Cell, dest_is_thing: bool) {
        let p = &self.pawns[i];
        let obsolete = self
            .destinations
            .most_recent_for(p.id)
            .is_some_and(|d| d.cell != dest && (dest_is_thing || d.job != Some(p.job_id)));
        if obsolete {
            self.destinations.obsolete_all_claimed_by(p.id);
        }
    }

    /// Exhaustion: the pawn lies down where it is and falls asleep at once
    /// (a forced-sleep LayDown, which reserves nothing).
    fn start_involuntary_sleep(&mut self, i: usize) {
        let spot = self.pawns[i].next_stop();
        let lay_down = self.job_defs.lay_down;
        self.drop_carried(i);
        self.start_job(
            i,
            Job {
                def: lay_down,
                kind: JobKind::LayDown { spot, bed: None },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            true,
        );
        self.pawns[i].think_trail = vec!["(involuntary sleep: exhausted)".to_owned()];
    }

    /// Starting an Ingest job reserves min(job count, stack, units not held
    /// by other pawns) of the food; when that is not zero it also becomes
    /// the job's count (`TryMakePreToilReservations` and the `ReserveFood`
    /// toil, both at job start). Selection itself reserves nothing.
    fn reserve_food(&self, i: usize, thought: &mut Thought) {
        let Some(Job {
            kind:
                JobKind::Ingest {
                    food,
                    count,
                    reserved,
                    ..
                },
            ..
        }) = &mut thought.job
        else {
            return;
        };
        let Some(stack) = self.map.item(*food).map(|it| it.stack_count) else {
            return;
        };
        let available = self.reservations.can_reserve_stack(
            self.claimant(i),
            Target::Item(*food),
            stack as i32,
            MAX_FOOD_RESERVERS,
        ) as u32;
        *reserved = (*count).min(stack).min(available);
        if *reserved != 0 {
            *count = *reserved;
        }
    }

    /// `CheckForJobOverride` while lying awake (minimum priority 0): the
    /// tree's job replaces the current one (`ShouldStartJobFromThinkTree`)
    /// unless it is the same kind of job continuing it (`IsContinuation`:
    /// a LayDown on the same bed or spot) from the same think node.
    fn check_for_job_override(&mut self, i: usize) {
        let hour = self.hour_of_day();
        let mut thought = self.think_for(i, hour);
        self.reserve_food(i, &mut thought);
        if let Some(flag) = thought.next_idle_is_wait {
            self.pawns[i].next_idle_is_wait = flag;
        }
        let Some(job) = thought.job else {
            return;
        };
        let continuing = self.pawns[i].job.as_ref().is_some_and(|cur| {
            cur.def == job.def
                && match (cur.kind, job.kind) {
                    (
                        JobKind::LayDown { bed: a, spot: s },
                        JobKind::LayDown { bed: b, spot: t },
                    ) => {
                        if a.is_some() {
                            a == b
                        } else {
                            b.is_none() && s == t
                        }
                    }
                    _ => true,
                }
        });
        if continuing && thought.trail == self.pawns[i].think_trail {
            return;
        }
        self.pawns[i].think_trail = thought.trail;
        self.pawns[i].target_queue = thought.queue;
        if !thought.bill_queue.is_empty() {
            let (ids, counts) = thought.bill_queue.into_iter().unzip();
            self.pawns[i].target_queue_b = ids;
            self.pawns[i].count_queue = counts;
        }
        self.start_job(i, job, false);
    }

    // DIFFERENTIAL VERIFIED: pick-up, carry, chew countdown and finalize
    // timing, tick by tick against 3 original-game eating traces.
    /// Ingest: the pawn reached the food. Picks up the job's count (or what
    /// is left of the stack), then sets off to a spot to eat standing.
    fn ingest_pick_up(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::Ingest { food, count, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some((def, stack, rot)) = self
            .map
            .item(food)
            .map(|it| (it.def, it.stack_count, it.rot))
        else {
            self.end_job(i, false); // the food is gone
            return;
        };
        let food_hp = self.map.item(food).and_then(|it| it.hit_points);
        let taken = self.map.take_from_item(food, count);
        // A whole stack keeps its identity; a split-off part is a new thing.
        let carried_id = if taken < stack {
            let id = self.map.allocate_item_id();
            self.map.copy_meta(food, id);
            id
        } else {
            food
        };
        self.refresh_path_grid();
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        if taken < stack {
            // Split off: the carried part is a new thing; the original
            // stack's row goes (`Toils_Ingest.PickupIngestible`).
            // COMPATIBILITY TODO: currently approximate — the carried thing's
            // own exclusive reservation is not recorded (carried items have
            // no identity yet).
            self.reservations
                .release(Target::Item(food), claimant.pawn, job_id);
        }
        let reservations = &self.reservations;
        let destinations = &self.destinations;
        let t = self.tick;
        let pawn = &mut self.pawns[i];
        pawn.carried = Some(Carried {
            id: carried_id,
            def,
            count: taken,
            rot,
            hit_points: food_hp,
        });
        let at = pawn.position;
        let forced = self
            .forced_chew_spots
            .iter()
            .position(|(p, _)| *p == pawn.id)
            .map(|k| self.forced_chew_spots.remove(k).1);
        let chair_radius = self.defs.things[def]
            .ingestible
            .as_ref()
            .map_or(0.0, |ing| ing.chair_search_radius);
        let spot = forced
            .or_else(|| {
                let view = MapView {
                    map: &self.map,
                    defs: &self.defs,
                    grid: &self.path_grid,
                    regions: &self.regions,
                };
                crate::food::chair_spot(
                    &view,
                    at,
                    chair_radius,
                    &|c| reservations.can_reserve(claimant, Target::Cell(c), 1, 1, STACK_ALL),
                    &|id| reservations.can_reserve(claimant, Target::Item(id), 1, 1, STACK_ALL),
                )
            })
            .or_else(|| {
                let view = MapView {
                    map: &self.map,
                    defs: &self.defs,
                    grid: &self.path_grid,
                    regions: &self.regions,
                };
                spot_to_chew_standing_near(
                    &view,
                    &mut self.ingest_order,
                    at,
                    def,
                    &|c| destinations.can_reserve(c, claimant, false),
                    &|c| reservations.can_reserve(claimant, Target::Cell(c), 1, 1, STACK_ALL),
                    &mut self.rng,
                )
            })
            .unwrap_or(at);
        let path = (spot != at)
            .then(|| {
                find_path(
                    &self.path_grid,
                    at,
                    spot,
                    pawn.move_costs,
                    COLONIST_HEURISTIC_STRENGTH,
                )
                .ok()
            })
            .flatten();
        set_ingest_stage(pawn, IngestStage::CarryToChewSpot { spot });
        // `CarryIngestibleToChewSpot`: reserve the spot (the result is not
        // checked) and record it as the destination.
        self.reservations
            .reserve(claimant, job_id, Target::Cell(spot), 1, 1, STACK_ALL);
        self.destinations.reserve(claimant, job_id, spot);
        let pawn = &mut self.pawns[i];
        match path {
            Some(path) => {
                pawn.path = path.cells.into();
                pawn.destination = Some(spot);
                pawn.move_ready_tick = t + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, spot, false);
            }
            // Already there (or no way there): eat here at once.
            // COMPATIBILITY TODO: currently approximate — the game's path
            // to an unreachable spot fails the job instead.
            None => self.ingest_start_chewing(i),
        }
    }

    /// Ingest: at the eating spot. Starts the chew countdown; it is paid
    /// for the first time in this same tick (as in the game, where the
    /// toil starts on arrival and the driver ticks afterwards).
    fn ingest_start_chewing(&mut self, i: usize) {
        let eating_factor = self.capacity_factor(i, "EatingSpeed");
        let defs = &self.defs;
        let pawn = &mut self.pawns[i];
        let Some(carried) = pawn.carried else {
            self.end_job(i, false);
            return;
        };
        let ticks = defs.things[carried.def]
            .ingestible
            .as_ref()
            .map_or(0, |ing| {
                chew_ticks(
                    ing.base_ingest_ticks,
                    pawn.eating_speed * eating_factor,
                    ing.use_eating_speed_stat,
                )
            });
        set_ingest_stage(pawn, IngestStage::Chew { ticks_left: ticks });
        if chew_tick(pawn) {
            self.ingest_finalize(i);
        }
    }

    /// Ingest: chewing finished. Eats what the food need wants (rounded up
    /// to whole units, capped by what is carried) and ends the job.
    fn ingest_finalize(&mut self, i: usize) {
        let eaten_from = self.pawns[i].carried;
        if let Some(c) = eaten_from {
            self.table_thoughts(i, c);
        }
        self.ingest_finalize_nutrition(i);
        if let Some(c) = eaten_from {
            self.ingestion_food_poisoning(i, c);
            self.ingestion_thoughts(i, c);
        }
        // COMPATIBILITY TODO: currently approximate — eating joy (e.g.
        // lavish meals' joy) is not applied.
        self.end_job(i, true);
    }

    /// `Thing.Ingested`'s food poisoning: the food's fixed chance
    /// (`FoodPoisonChanceFixedHuman`, humanlikes only), then a cooked
    /// meal's own poison percent (`CompFoodPoisonable.PostIngested`); each
    /// × the difficulty's factor (1).
    // COMPATIBILITY TODO: currently approximate — hediffs' and traits'
    // food-poisoning factors are not applied.
    fn ingestion_food_poisoning(&mut self, i: usize, food: Carried) {
        let defs = self.defs.clone();
        let humanlike = defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            == Some("Humanlike");
        let fixed = crate::stats::def_stat(
            &defs,
            &defs.things[food.def],
            None,
            "FoodPoisonChanceFixedHuman",
        );
        if humanlike && self.rng.chance(fixed) {
            self.add_food_poisoning(i);
        }
        if let Some(meta) = self.map.item_meta.get(&food.id).cloned()
            && self.has_comp(food.def, "CompProperties_FoodPoisonable")
            && self.rng.chance(meta.poison_pct)
        {
            self.add_food_poisoning(i);
        }
    }

    /// `FoodUtility.AddFoodPoisoningHediff`: a new FoodPoisoning, or an
    /// existing one set back to just below its third stage.
    fn add_food_poisoning(&mut self, i: usize) {
        let Some(def) = self.defs.hediffs.id("FoodPoisoning") else {
            return;
        };
        let stage2 = self.defs.hediffs[def].stages.get(2).map(|s| s.min_severity);
        let id = self.pawns[i].id;
        if let Some(h) = self.pawns[i]
            .health
            .hediffs
            .iter_mut()
            .find(|h| h.def == def)
        {
            let stages = &self.defs.hediffs[def].stages;
            let index = stages.iter().rposition(|s| h.severity >= s.min_severity);
            if index != Some(2)
                && let Some(min) = stage2
            {
                h.severity = min - 0.001;
            }
        } else {
            let initial = self
                .defs
                .raw
                .get("HediffDef", "FoodPoisoning")
                .and_then(|d| d.node.child_text("initialSeverity"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(1.0);
            self.debug_add_hediff(id, "FoodPoisoning", initial);
        }
    }

    /// The nutrition part of finishing a meal.
    fn ingest_finalize_nutrition(&mut self, i: usize) {
        let defs = &self.defs;
        let pawn = &mut self.pawns[i];
        if let Some(carried) = pawn.carried {
            let def = &defs.things[carried.def];
            let unit = unit_nutrition(defs, def);
            let max_at_once = def
                .ingestible
                .as_ref()
                .map_or(0, |ing| ing.max_num_to_ingest_at_once);
            if let Some(food) = pawn.needs.get_mut(NeedKind::Food) {
                let eaten = ingested_count(food.max - food.level, unit, carried.count, max_at_once);
                food.level = (food.level + eaten as f32 * unit).clamp(0.0, food.max);
                let left = carried.count - eaten.min(carried.count);
                pawn.carried = (left > 0).then_some(Carried {
                    count: left,
                    ..carried
                });
            }
        }
    }

    /// Puts down whatever the pawn carries on its cell.
    // COMPATIBILITY TODO: currently approximate — the game drops carried
    // things through its placement search (nearby free cells, stacking).
    fn drop_carried(&mut self, i: usize) {
        self.drop_carried_pawn(i);
        let Some(mut c) = self.pawns[i].carried.take() else {
            return;
        };
        let at = self.pawns[i].position;
        // `TryDropCarriedThing(Near)`.
        if !self.place_thing_near(&mut c, at) {
            self.map.spawn_carried(&c, at);
            self.refresh_path_grid();
        }
    }

    /// `GenPlace.TryPlaceDirect` for an item: merge into compatible stacks
    /// on the cell (largest first), then use a free item slot. Returns
    /// `true` when everything was placed; a partial merge leaves the rest
    /// in `thing` and returns `false`.
    // COMPATIBILITY TODO: currently approximate — equal-count stacks keep
    // spawn order (the game's sort is unstable); hit points and comps are
    // not merged.
    fn place_direct(&mut self, thing: &mut Carried, cell: Cell) -> bool {
        self.place_direct_recording(thing, cell, &mut Vec::new())
    }

    /// [`Sim::place_direct`], recording which things received how many
    /// (`placedThings`).
    fn place_direct_recording(
        &mut self,
        thing: &mut Carried,
        cell: Cell,
        placed: &mut Vec<(ItemId, u32)>,
    ) -> bool {
        let defs = &self.defs;
        let limit = defs.things[thing.def].stack_limit as u32;
        let mut stacks: Vec<(ItemId, u32)> = self
            .map
            .items_at(cell)
            .filter(|i| !i.is_filth())
            .map(|i| (i.id, i.stack_count))
            .collect();
        // Largest first (.NET `List.Sort`, unstable).
        crate::netsort::sort(&mut stacks, |a, b| b.1.cmp(&a.1));
        if limit > 1 {
            for (id, n) in stacks.clone() {
                let same = self.map.item(id).is_some_and(|i| i.def == thing.def);
                if !same || n >= limit {
                    continue;
                }
                let taken = thing.count.min(limit - n);
                let max_hp = self.max_hit_points(thing.def);
                self.map.merge_meta(id, n, thing.id, taken);
                self.map
                    .add_to_stack(id, taken, thing.rot, thing.hit_points, max_hp);
                placed.push((id, taken));
                thing.count -= taken;
                if thing.count == 0 {
                    return true;
                }
            }
        }
        // A thing that doesn't stack goes down even on a full cell.
        if stacks.len() < crate::haul::MAX_ITEMS_IN_CELL || limit <= 1 {
            // `SplitAndSpawnOneStackOnCell`: at most a full stack per cell;
            // the split-off part is a new thing.
            if limit > 0 && thing.count > limit {
                let part = Carried {
                    id: self.map.allocate_item_id(),
                    count: limit,
                    ..*thing
                };
                self.map.copy_meta(thing.id, part.id);
                self.map.spawn_carried(&part, cell);
                placed.push((part.id, limit));
                thing.count -= limit;
                return false;
            }
            self.map.spawn_carried(thing, cell);
            placed.push((thing.id, thing.count));
            return true;
        }
        false
    }

    /// The capacities pawn `i` is not capable of (`CapableOf`: level above
    /// the capacity's `minForCapable`).
    fn incapable_capacities(&self, i: usize) -> Vec<String> {
        let p = &self.pawns[i];
        if p.health.hediffs.is_empty() {
            return Vec::new();
        }
        let Some(view) = self.health_view(p.id) else {
            return Vec::new();
        };
        self.defs
            .capacities
            .iter()
            .filter(|(_, c)| view.capacity(&c.def_name) <= c.min_for_capable)
            .map(|(_, c)| c.def_name.clone())
            .collect()
    }

    pub(super) fn carrying_capacity(&self, i: usize) -> f32 {
        self.pawns[i].carrying_capacity * self.capacity_factor(i, "CarryingCapacity")
    }

    /// The health part of a pawn stat (`capacityFactors`) for pawn `i`; 1
    /// for a healthy pawn. Applied over the per-pawn base values (move,
    /// eating speed, carrying capacity) that replays may set.
    pub(super) fn capacity_factor(&self, i: usize, stat: &str) -> f32 {
        let p = &self.pawns[i];
        if p.health.hediffs.is_empty() {
            return 1.0;
        }
        let (Some(view), Some(s)) = (self.health_view(p.id), self.defs.stats.get(stat)) else {
            return 1.0;
        };
        crate::stats::capacity_factor(s, &|c| view.capacity(c))
    }

    /// Debug tool: overrides a pawn's CarryingCapacity stat.
    pub fn set_carrying_capacity(&mut self, pawn: PawnId, capacity: f32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.carrying_capacity = capacity;
        }
    }

    /// Debug tool (as the hauling research diagnostic did): asks the
    /// HaulGeneral work giver for a job on `source`, without starting it.
    /// Draws from the random stream like the real query.
    pub fn debug_haul_job(&mut self, pawn: PawnId, source: ItemId) -> Option<Job> {
        let i = self.index_of(pawn)?;
        let giver = crate::haul::HaulGeneral {
            regions: &self.regions,
            haul_job: self.job_defs.haul_to_cell,
            carrying_capacity: self.carrying_capacity(i),
        };
        let ctx = crate::work::WorkContext {
            defs: &self.defs,
            map: &self.map,
            grid: &self.path_grid,
            regions: &self.regions,
            rng: std::cell::RefCell::new(&mut self.rng),
            reservations: &self.reservations,
            claimant: Claimant {
                pawn,
                has_faction: self.pawns[i].is_colonist,
            },
            position: self.pawns[i].position,
            costs: self.pawns[i].move_costs,
            tick: self.tick,
            incapable: &[],
        };
        use crate::work::WorkGiver;
        giver.job_on_thing(&ctx, source).map(|(job, _)| job)
    }

    /// Debug tool: starts `job` now (like the game's `StartJob` with
    /// pre-toil reservations allowed to fail; a failed start thinks anew).
    /// Returns whether the job started.
    pub fn debug_start_job(&mut self, pawn: PawnId, job: Job) -> bool {
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        self.drop_carried(i);
        if self.start_job(i, job, false) {
            true
        } else {
            self.find_and_start_job(i);
            false
        }
    }

    /// Debug tool: interrupts the pawn with a forced Wait (as a player
    /// order would); what it carries is dropped near it.
    pub fn debug_interrupt_with_wait(&mut self, pawn: PawnId) {
        let Some(i) = self.index_of(pawn) else { return };
        self.drop_carried(i);
        let wait = self.job_defs.wait;
        self.start_job(
            i,
            Job {
                def: wait,
                kind: JobKind::Wait {
                    expiry_interval: PATHER_ERROR_WAIT_TICKS,
                },
                forced: true,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        );
    }

    /// Debug tool: destroys an item on the map (its reservations go).
    pub fn debug_destroy_item(&mut self, id: ItemId) {
        if let Some(n) = self.map.item(id).map(|i| i.stack_count) {
            self.map.take_from_item(id, n);
            self.reservations.release_all_for_target(Target::Item(id));
            self.refresh_path_grid();
        }
    }

    /// Designates a stockpile over `cells` (walkable cells only). Cells
    /// touching or inside an existing stockpile extend the first such zone;
    /// otherwise a new Normal-priority zone is made. Returns the zone.
    // COMPATIBILITY TODO: currently approximate — the game's zone designator
    // extends the selected zone and has its own placement rules.
    pub fn designate_stockpile(&mut self, cells: &[Cell]) -> Option<crate::storage::ZoneId> {
        self.designate_stockpile_with(cells, crate::storage::StoragePreset::DefaultStockpile)
    }

    /// [`Sim::designate_stockpile`] with the given filter preset for a new
    /// zone (the dumping stockpile takes corpses and chunks).
    pub fn designate_stockpile_with(
        &mut self,
        cells: &[Cell],
        preset: crate::storage::StoragePreset,
    ) -> Option<crate::storage::ZoneId> {
        let cells: Vec<Cell> = cells
            .iter()
            .copied()
            .filter(|&c| self.map.size().contains(c) && self.path_grid.walkable(c))
            .filter(|&c| self.map.storage.zone_at(c).is_none())
            .collect();
        if cells.is_empty() {
            return None;
        }
        for &c in &cells {
            self.mark_home_around_zone_cell(c);
        }
        let storage = &mut self.map.storage;
        let touching = cells.iter().find_map(|&c| {
            std::iter::once(Cell::new(0, 0))
                .chain(Cell::NEIGHBORS_8)
                .find_map(|d| storage.zone_at(c + d))
        });
        Some(match touching {
            Some(z) => {
                storage.add_cells(z, &cells);
                z
            }
            None => {
                let z = storage.add_stockpile(
                    crate::storage::StoragePriority::Normal,
                    &cells,
                    crate::storage::ThingFilter::preset(&self.defs, preset),
                );
                self.name_new_zone(
                    zones::ZoneRef::Stockpile(z),
                    preset == crate::storage::StoragePreset::DumpingStockpile,
                );
                z
            }
        })
    }

    /// Removes cells from whatever zones hold them.
    pub fn remove_zone_cells(&mut self, cells: &[Cell]) {
        for &c in cells {
            self.map.storage.remove_cell(c);
        }
    }

    /// Storage queries over the current map.
    pub fn store_view(&self) -> StoreView<'_> {
        StoreView {
            defs: &self.defs,
            map: &self.map,
            grid: &self.path_grid,
            regions: &self.regions,
            reservations: &self.reservations,
        }
    }

    fn set_haul(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut ItemId, &mut Cell, &mut i32, &mut HaulStage),
    ) {
        if let Some(Job {
            kind:
                JobKind::Haul {
                    source,
                    dest,
                    count,
                    stage,
                    ..
                },
            ..
        }) = &mut self.pawns[i].job
        {
            f(source, dest, count, stage);
        }
    }

    /// Haul: at the source; pick it up at once (`StartCarryThing`), then
    /// carry it to the destination cell.
    fn haul_pick_up(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::Haul { source, count, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some((def, stack, src_rot, src_hp)) = self
            .map
            .item(source)
            .map(|it| (it.def, it.stack_count, it.rot, it.hit_points))
        else {
            self.end_job(i, false);
            return;
        };
        let space = max_carry(&self.defs, def, self.carrying_capacity(i))
            - self.pawns[i].carried.map_or(0, |c| c.count as i32);
        let wanted = count.max(1).min(space).min(stack as i32);
        if wanted <= 0 {
            self.end_job(i, false);
            return;
        }
        let taken = self.map.take_from_item(source, wanted as u32);
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        let carried_id = if let Some(c) = self.pawns[i].carried {
            // Already carrying (an opportunistic duplicate): the stack is
            // absorbed into the carried thing (`TryStartCarry`).
            self.reservations
                .release(Target::Item(source), claimant.pawn, job_id);
            c.id
        } else if taken < stack {
            // Partial: a new carried thing; the remainder keeps the source's
            // identity and this job's claim on it is released.
            let id = self.map.allocate_item_id();
            self.map.copy_meta(source, id);
            self.reservations
                .reserve(claimant, job_id, Target::Item(id), 1, 1, STACK_ALL);
            self.reservations
                .release(Target::Item(source), claimant.pawn, job_id);
            id
        } else {
            // Whole: the same thing, already reserved by this job.
            source
        };
        self.refresh_path_grid();
        let max_hp = self.max_hit_points(def);
        let (already, rot, hit_points) = self.pawns[i].carried.map_or((0, src_rot, src_hp), |c| {
            (
                c.count,
                crate::map::blend_rot(c.rot, c.count, src_rot, taken),
                crate::map::blend_hit_points(c.hit_points, c.count, src_hp, taken, max_hp),
            )
        });
        self.pawns[i].carried = Some(Carried {
            id: carried_id,
            def,
            count: already + taken,
            rot,
            hit_points,
        });
        self.set_haul(i, |src, _, cnt, stage| {
            *src = carried_id;
            *cnt -= taken as i32;
            *stage = HaulStage::CarryToCell;
        });
        if !self.haul_get_duplicate(i) {
            self.haul_start_carry(i);
        }
    }

    /// `CheckForGetOpportunityDuplicate`: with room left to carry and job
    /// count left, a storage haul goes for the closest stack of the same
    /// thing within 8 cells that the destination accepts, and picks it up
    /// too. Returns whether it went for one.
    // COMPATIBILITY TODO: currently approximate — the game's search is
    // region-wise (`ClosestThingReachable`); we take the nearest by
    // straight-line distance with a path, ties in spawn order. Forbidding
    // and social propriety are not modelled.
    fn haul_get_duplicate(&mut self, i: usize) -> bool {
        let Some(Job {
            kind:
                JobKind::Haul {
                    dest,
                    count,
                    aside: false,
                    ..
                },
            ..
        }) = self.pawns[i].job
        else {
            return false;
        };
        let Some(carried) = self.pawns[i].carried else {
            return false;
        };
        let limit = self.defs.things[carried.def].stack_limit;
        let space =
            max_carry(&self.defs, carried.def, self.carrying_capacity(i)) - carried.count as i32;
        if limit == 1 || space <= 0 || count <= 0 {
            return false;
        }
        let at = self.pawns[i].next_stop();
        let claimant = self.claimant(i);
        let found = self
            .map
            .items()
            .iter()
            .filter(|t| {
                t.def == carried.def
                    && !t.is_filth()
                    && !t.forbidden
                    && (t.stack_count as i32) < limit
                    && t.position != dest
                    && t.position.distance_squared(at) <= 64
                    && self
                        .reservations
                        .can_reserve(claimant, Target::Item(t.id), 1, 1, STACK_ALL)
            })
            .map(|t| (t.position.distance_squared(at), t.id, t.position))
            .filter(|_| storage_valid(&self.map, &self.defs, dest, carried.def))
            .filter(|&(_, _, c)| {
                c == at
                    || find_path(
                        &self.path_grid,
                        at,
                        c,
                        self.pawns[i].move_costs,
                        COLONIST_HEURISTIC_STRENGTH,
                    )
                    .is_ok()
            })
            .min_by_key(|&(d, _, _)| d);
        let Some((_, id, cell)) = found else {
            return false;
        };
        let stack = self.map.item(id).map_or(1, |t| t.stack_count as i32);
        let job_id = self.pawns[i].job_id;
        self.reservations
            .reserve(claimant, job_id, Target::Item(id), stack, 1, STACK_ALL);
        self.set_haul(i, |src, _, _, stage| {
            *src = id;
            *stage = HaulStage::GotoSource;
        });
        if cell == at {
            self.haul_pick_up(i);
            return true;
        }
        let tick = self.tick;
        let path = find_path(
            &self.path_grid,
            at,
            cell,
            self.pawns[i].move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        )
        .expect("checked above");
        let pawn = &mut self.pawns[i];
        pawn.path = path.cells.into();
        pawn.destination = Some(cell);
        pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
        self.on_start_path(i, cell, true);
        true
    }

    /// Haul: walk to the destination cell (`CarryHauledThingToCell`).
    fn haul_start_carry(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::Haul { dest, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let tick = self.tick;
        let pawn = &mut self.pawns[i];
        pawn.path.clear();
        pawn.destination = None;
        let at = pawn.next_stop();
        if at == dest {
            self.set_haul(i, |_, _, _, stage| *stage = HaulStage::Delay);
            return;
        }
        match find_path(
            &self.path_grid,
            at,
            dest,
            pawn.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) => {
                pawn.path = path.cells.into();
                pawn.destination = Some(dest);
                pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, dest, false);
            }
            Err(_) => self.end_job(i, false),
        }
    }

    /// Haul interval work: once 30 ticks have passed since the driver
    /// started, the pawn at the cell places what it carries
    /// (`PossiblyDelay`, then `PlaceHauledThingInCell`).
    fn haul_interval(&mut self, i: usize) {
        let Some(Job {
            kind:
                JobKind::Haul {
                    stage: HaulStage::Delay,
                    start_tick,
                    dest,
                    ..
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        if self.tick < start_tick + MIN_HAUL_TICKS {
            return;
        }
        let Some(mut carried) = self.pawns[i].carried.take() else {
            self.end_job(i, false);
            return;
        };
        let complete = self.place_direct(&mut carried, dest);
        self.refresh_path_grid();
        if complete {
            self.end_job(i, true);
            return;
        }
        self.pawns[i].carried = Some(carried);
        // COMPATIBILITY TODO: currently approximate — hauling aside, the
        // game looks for another spot close by; we end the job.
        if matches!(
            self.pawns[i].job.as_ref().map(|j| j.kind),
            Some(JobKind::Haul { aside: true, .. })
        ) {
            self.end_job(i, false);
            return;
        }
        // Partly placed: look for another storage cell for the rest and
        // send the same job there (keeping the old cell's reservation).
        let claimant = self.claimant(i);
        let position = self.pawns[i].position;
        let thing = Storable {
            def: carried.def,
            position,
        };
        let found = {
            let view = StoreView {
                defs: &self.defs,
                map: &self.map,
                grid: &self.path_grid,
                regions: &self.regions,
                reservations: &self.reservations,
            };
            view.best_better_store_cell(
                thing,
                Some(claimant),
                StoragePriority::Unstored,
                true,
                &mut self.rng,
            )
        };
        match found {
            Some(cell) => {
                let job_id = self.pawns[i].job_id;
                if self
                    .reservations
                    .can_reserve(claimant, Target::Cell(cell), 1, 1, STACK_ALL)
                {
                    self.reservations.reserve(
                        claimant,
                        job_id,
                        Target::Cell(cell),
                        1,
                        1,
                        STACK_ALL,
                    );
                }
                self.set_haul(i, |_, d, _, stage| {
                    *d = cell;
                    *stage = HaulStage::CarryToCell;
                });
                self.haul_start_carry(i);
            }
            // COMPATIBILITY TODO: currently approximate — the game then
            // hauls the rest aside (`CanHaulAside`) or, failing that,
            // destroys it; we end the job (the pawn drops it).
            None => self.end_job(i, false),
        }
    }

    /// The path to a destroyed target failed (`ErroredPather`): the job
    /// ends and the pawn waits 250 ticks.
    fn end_job_pather_error(&mut self, i: usize) {
        self.drop_carried(i);
        self.cleanup_job(i);
        self.pawns[i].job = None;
        let wait = self.job_defs.wait;
        self.start_job(
            i,
            Job {
                def: wait,
                kind: JobKind::Wait {
                    expiry_interval: PATHER_ERROR_WAIT_TICKS,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        );
    }

    /// Recomputes path costs after things changed. Regions only depend on
    /// walkability, so they are rebuilt only when that changed.
    // COMPATIBILITY TODO: currently approximate — the game regenerates only
    // the dirtied regions (`RegionDirtyer`), which can order regions and
    // links differently from a full rebuild.
    /// Debug tool: rebuilds path costs and regions after editing the map
    /// directly (terrain set by a test).
    pub fn debug_refresh_path_grid(&mut self) {
        self.refresh_path_grid();
        self.rebuild_regions();
    }

    fn refresh_path_grid(&mut self) {
        let grid = self.map.build_path_grid(&self.defs);
        let size = self.map.size();
        let changed = size
            .cells()
            .any(|c| grid.walkable(c) != self.path_grid.walkable(c));
        self.path_grid = grid;
        if changed {
            self.rebuild_regions();
        }
    }

    /// Rebuilds regions and rooms: room temperatures carry over and
    /// changed rooms are queued for auto-roofing.
    pub(super) fn rebuild_regions(&mut self) {
        let old = std::mem::replace(
            &mut self.regions,
            Regions::build(&self.map, &self.defs, &self.path_grid),
        );
        self.remap_room_temps(&old);
        self.queue_changed_rooms();
    }

    /// Places a stack of items on the map (updating path costs).
    pub fn spawn_item(&mut self, def: DefId<ThingDef>, cell: Cell, count: u32) -> ItemId {
        let id = self.map.spawn_item(def, cell, count);
        self.refresh_path_grid();
        id
    }

    /// Debug tool: forces whether mined rock drops its chunk (`None`: the
    /// random roll decides).
    pub fn debug_set_mine_drops(&mut self, drops: Option<bool>) {
        self.debug_mine_drops = drops;
    }

    /// Debug tool: gives a pawn a whole-body hediff at `severity`.
    pub fn debug_add_hediff(&mut self, pawn: PawnId, hediff: &str, severity: f32) {
        let Some(def) = self.defs.hediffs.id(hediff) else {
            return;
        };
        if let Some(p) = self.pawn_mut(pawn) {
            p.health.hediffs.push(crate::health::Hediff {
                def,
                part: None,
                severity,
                age_ticks: 0,
                comps: Default::default(),
            });
        }
        if let Some(i) = self.index_of(pawn) {
            self.check_health_state(i);
        }
    }

    /// Debug tool: whether the downed pawn's next crawl order is a break.
    pub fn debug_set_crawl_break(&mut self, pawn: PawnId, next_is_break: bool) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.crawl_break_next = next_is_break;
        }
    }

    /// Debug tool: [`Sim::debug_add_injury`] on the `nth` body part of def
    /// `part` (0 = the first).
    pub fn debug_add_injury_nth(
        &mut self,
        pawn: PawnId,
        hediff: &str,
        part: &str,
        nth: usize,
        severity: f32,
    ) {
        let Some(def) = self.defs.hediffs.id(hediff) else {
            return;
        };
        let Some(i) = self.index_of(pawn) else {
            return;
        };
        let defs = self.defs.clone();
        let Some(index) = defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .and_then(|r| r.body.as_deref())
            .and_then(|b| defs.bodies.get(b))
            .and_then(|b| {
                b.parts
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.def == part)
                    .nth(nth)
                    .map(|(k, _)| k)
            })
        else {
            return;
        };
        self.pawns[i].health.hediffs.push(crate::health::Hediff {
            def,
            part: Some(index),
            severity,
            age_ticks: 0,
            comps: Default::default(),
        });
        self.check_health_state(i);
    }

    /// Debug tool: adds `hediff` at `severity` on the first body part of
    /// def `part` (`HediffMaker.MakeHediff` + `AddHediff`).
    pub fn debug_add_injury(&mut self, pawn: PawnId, hediff: &str, part: &str, severity: f32) {
        let Some(def) = self.defs.hediffs.id(hediff) else {
            return;
        };
        let Some(i) = self.index_of(pawn) else {
            return;
        };
        let defs = self.defs.clone();
        let Some(index) = defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .and_then(|r| r.body.as_deref())
            .and_then(|b| defs.bodies.get(b))
            .and_then(|b| b.parts.iter().position(|p| p.def == part))
        else {
            return;
        };
        self.pawns[i].health.hediffs.push(crate::health::Hediff {
            def,
            part: Some(index),
            severity,
            age_ticks: 0,
            comps: Default::default(),
        });
        self.check_health_state(i);
    }

    /// Debug tool (like the game ending a pawn's job by force): drops the
    /// current job and thinks for a new one now.
    pub fn debug_find_and_start_job(&mut self, pawn: PawnId) {
        if let Some(i) = self.pawns.iter().position(|p| p.id == pawn) {
            self.drop_carried(i);
            self.cleanup_job(i);
            let p = &mut self.pawns[i];
            p.job = None;
            p.path.clear();
            p.destination = None;
            p.asleep = false;
            self.find_and_start_job(i);
        }
    }

    /// Debug tool: overrides a pawn's EatingSpeed stat.
    pub fn set_eating_speed(&mut self, pawn: PawnId, eating_speed: f32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.eating_speed = eating_speed;
        }
    }

    /// Debug tool: makes the pawn's next chew-spot search return `spot`
    /// (once). Used to replay recorded games, whose random choices we do not
    /// reproduce yet.
    pub fn debug_force_next_chew_spot(&mut self, pawn: PawnId, spot: Cell) {
        self.forced_chew_spots.retain(|(p, _)| *p != pawn);
        self.forced_chew_spots.push((pawn, spot));
    }

    /// Debug tool: sets a need level (as a fraction of its maximum).
    pub fn debug_set_need(&mut self, pawn: PawnId, kind: NeedKind, fraction: f32) {
        if let Some(need) = self.pawn_mut(pawn).and_then(|p| p.needs.get_mut(kind)) {
            need.level = fraction * need.max;
        }
    }

    /// Sets how often a pawn's interval logic runs (1-15 ticks). Front-ends
    /// pass the game's camera-based rate (15 off-screen).
    pub fn set_update_rate(&mut self, pawn: PawnId, rate: u32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.update_rate = rate.clamp(1, 15);
        }
    }

    /// Debug: sets a pawn's interval accumulator (`Thing` tick delta), for
    /// replays that start mid-cadence.
    pub fn debug_set_tick_delta(&mut self, pawn: PawnId, delta: u32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.tick_delta = delta;
        }
    }

    /// Sets a pawn's thing id number (the game draws these from a global
    /// counter; it determines the pawn's hash-staggered ticks).
    pub fn set_thing_id_number(&mut self, pawn: PawnId, id_number: i32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.id_number = id_number;
        }
    }

    /// Debug tool (like the game's dev-mode job starting): starts a Goto, or
    /// with `wander` a GotoWander, to `target` with the given urgency now.
    pub fn debug_start_goto(
        &mut self,
        pawn: PawnId,
        target: Cell,
        urgency: LocomotionUrgency,
        wander: bool,
    ) -> Result<(), CommandError> {
        self.plan_path(pawn, target)?;
        let def = if wander {
            self.job_defs.goto_wander
        } else {
            self.job_defs.goto
        };
        let i = self.index_of(pawn).ok_or(CommandError::NoSuchPawn(pawn))?;
        self.start_job(
            i,
            Job {
                def,
                kind: JobKind::Goto { target },
                forced: false,
                urgency,
                start_tick: 0,
            },
            false,
        );
        Ok(())
    }

    /// Debug tool: sets the tick counter (like loading a game at that tick),
    /// so hash-staggered timing lines up with a recorded game.
    pub fn debug_set_tick(&mut self, tick: u64) {
        self.tick = tick;
    }

    /// Debug tool: fixes the sky glow (`None`: the sun's).
    pub fn debug_set_sky_glow(&mut self, glow: Option<f32>) {
        self.sky_glow_override = glow;
        self.map.sky_glow = glow.unwrap_or_else(|| self.outdoor_glow());
    }

    /// Debug tool: overrides a pawn's MoveSpeed (the game's stat value,
    /// including modifiers we do not compute yet).
    pub fn set_move_speed(&mut self, pawn: PawnId, move_speed: f32) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.move_costs = MoveCosts::from_move_speed(move_speed);
            p.base_move_speed = move_speed;
        }
    }

    /// Update rate given to newly spawned pawns.
    pub fn set_default_update_rate(&mut self, rate: u32) {
        self.default_update_rate = rate.clamp(1, 15);
    }

    /// What the safe-temperature giver needs to know about pawn `i`.
    fn temperature_facts(&self, i: usize) -> crate::think::TemperatureFacts {
        let defs = &self.defs;
        let serious_injury = self.pawns[i].health.hediffs.iter().any(|h| {
            let d = &defs.hediffs[h.def];
            matches!(d.def_name.as_str(), "Hypothermia" | "Heatstroke")
                && d.stages
                    .iter()
                    .rposition(|st| h.severity >= st.min_severity)
                    >= Some(3)
        });
        crate::think::TemperatureFacts {
            ambient: self.cell_temperature(self.pawns[i].position),
            comfy: (
                self.pawn_stat_of(i, "ComfyTemperatureMin"),
                self.pawn_stat_of(i, "ComfyTemperatureMax"),
            ),
            serious_injury,
        }
    }

    /// Runs pawn `i`'s think tree (or the built-in idle fallback).
    fn think_for(&mut self, i: usize, hour: u32) -> Thought {
        let query = self.work_query.take();
        // A forced (player) order ignores other pawns' reservations of its
        // target (`CanReserve(..., forced)`).
        let forced_reservations = query.map(|t| {
            let mut r = self.reservations.clone();
            r.release_others_on(orders::reservation_target(t), self.pawns[i].id);
            r
        });
        let hunt = self.hunt_work_target(i).map(|t| self.hunt_job(t));
        let animal_food = self.animal_food_job(i, false);
        let animal_food_whole_map = self.pawns[i]
            .needs
            .is_starving()
            .then(|| self.animal_food_job(i, true))
            .flatten();
        let leave = self.leave_facts(i);
        let abs_tick = self.abs_tick();
        let manhunter_target = self.pawns[i]
            .mind
            .mental_state
            .is_some()
            .then(|| self.manhunter_target(i))
            .flatten();
        let temperature = self.temperature_facts(i);
        let rescue = self.rescue_candidates(i);
        let feed = self.feed_candidates(i);
        let tend = self.tend_candidates(i);
        let research = self.research_benches();
        let bill_tables = self.table_infos();
        let bill_filters = self.bill_recipe_filters();
        let product_counts = self.product_counts();
        let chosen_out = std::cell::RefCell::new(Vec::new());
        let outdoor_rooms = self.outdoor_rooms();
        let owned_room = self.owned_room_cells(i);
        let patient = self.patient_facts(i);
        let carrying_capacity = self.carrying_capacity(i);
        let incapable = self.incapable_capacities(i);
        let claimant = self.claimant(i);
        // `FreeColonistsSpawned`, in spawn order.
        let colonists: Vec<Cell> = self
            .pawns
            .iter()
            .filter(|p| p.is_colonist)
            .map(|p| p.position)
            .collect();
        let defs = &*self.defs;
        let skills = self.pawns[i].skills.clone();
        let skill_of = |name: &str| skills.level(name);
        let rot_defs = self.defs.clone();
        let rot_of = move |it: &crate::map::Item| {
            let p = rot_defs.things[it.def].rottable.as_ref()?;
            Some(if it.rot >= p.days_to_dessicated * 60_000.0 {
                crate::sim::RotStage::Dessicated
            } else if it.rot >= p.ticks_to_rot_start() as f32 {
                crate::sim::RotStage::Rotting
            } else {
                crate::sim::RotStage::Fresh
            })
        };
        let bill_env = crate::cook::BillEnv {
            tables: &bill_tables,
            stacks: &self.bill_stacks,
            filters: &bill_filters,
            product_counts: &product_counts,
            stored_counts: &self.stored_counts,
            skill: &skill_of,
            rot_stage: &rot_of,
            chosen_out: &chosen_out,
        };
        let pawn = &self.pawns[i];
        let at = pawn.next_stop();
        let Some(tree) = pawn.think_tree else {
            let view = MapView {
                map: &self.map,
                defs,
                grid: &self.path_grid,
                regions: &self.regions,
            };
            let wanderer = Wanderer {
                position: at,
                costs: pawn.move_costs,
            };
            let destinations = &self.destinations;
            let (job, flag) = think_idle(
                &view,
                &wanderer,
                pawn.next_idle_is_wait,
                &self.job_defs,
                self.wander,
                WanderRoot::Own,
                &[],
                &|c| destinations.can_reserve(c, claimant, false),
                &mut self.rng,
            );
            return Thought {
                job,
                trail: vec!["(built-in idle)".to_owned()],
                next_idle_is_wait: Some(flag),
                queue: Vec::new(),
                bill_queue: Vec::new(),
            };
        };
        let humanlike = defs.things[pawn.race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            == Some("Humanlike");
        let lists = pawn
            .work
            .as_ref()
            .map(|w| giver_lists(defs, w, self.use_work_priorities, humanlike));
        let pawn_cells = self.pawn_cells();
        let delivery_jobs = self.delivery_job_ids();
        let construction_level = pawn.skills.level("Construction");
        let owned_bed = pawn.owned_bed;
        let occupancy = self.bed_occupancy();
        let mut ctx = ThinkContext {
            defs,
            pawn: PawnFacts {
                kind_def_name: &defs.pawn_kinds[pawn.kind].def_name,
                is_colonist: pawn.is_colonist,
                at,
                next_idle_is_wait: pawn.next_idle_is_wait,
                move_costs: pawn.move_costs,
                rest_level: pawn.needs.rest_level(),
                starving: pawn.needs.is_starving(),
                humanlike,
                hour,
                assignment: crate::rest::assignment_for(pawn, hour, humanlike),
                food: pawn.needs.get(NeedKind::Food).map(|f| FoodFacts {
                    level: f.level,
                    max: f.max,
                    want_eat: f.want_eat,
                    hunger: f.hunger_category(),
                }),
                race: defs.things[pawn.race].race.as_ref(),
                ever_work: pawn.work.is_some(),
                carrying_capacity,
                incapable: &incapable,
                temperature: Some(temperature),
                joy: joy::joy_facts(
                    defs,
                    pawn,
                    self.tick,
                    self.outdoor_temperature,
                    hour,
                    temperature.comfy,
                    owned_room.as_deref(),
                    &outdoor_rooms,
                    &incapable,
                ),
                mental_state: pawn
                    .mind
                    .mental_state
                    .as_ref()
                    .map(|s| defs.mental_states[s.def].def_name.as_str()),
                drafted: pawn.drafted,
                mental_state_class: pawn
                    .mind
                    .mental_state
                    .as_ref()
                    .and_then(|s| defs.mental_states[s.def].state_class.as_deref()),
                manhunter_target,
                wrong_season: leave.wrong_season,
                dangerous_temperature: leave.dangerous_temperature,
                outdoor: leave.outdoor,
                can_reach_map_edge: leave.can_reach_map_edge,
                id_number: pawn.id_number,
                downed: pawn.health.downed,
            },
            map: &self.map,
            grid: &self.path_grid,
            job_defs: &self.job_defs,
            default_wander: self.wander,
            claimant,
            reservations: forced_reservations.as_ref().unwrap_or(&self.reservations),
            destinations: &self.destinations,
            regions: &self.regions,
            colonists: &colonists,
            ingest_order: &mut self.ingest_order,
            work: lists.as_ref(),
            tick: self.tick,
            job_queue_out: Vec::new(),
            rng: &mut self.rng,
            unsupported: Default::default(),
            next_idle_is_wait_out: None,
            construction: Some(crate::think::ConstructionEnv {
                enroute: &self.enroute,
                pawn_cells: &pawn_cells,
                delivery_jobs: &delivery_jobs,
                construction_level,
                rescue: &rescue,
                feed: &feed,
                tend: &tend,
                patient,
                outdoor_temperature: self.outdoor_temperature,
                room_temperatures: &self.room_temps.temps,
                research: &research,
                bills: Some(&bill_env),
                hunt,
                animal_food,
                animal_food_whole_map,
            }),
            beds: Some(crate::think::BedEnv {
                owned: owned_bed,
                occupancy: &occupancy,
            }),
            room_temperatures: &self.room_temps.temps,
            abs_tick,
            think_data: pawn.mind.think_data.clone(),
        };
        if let Some(target) = query {
            self.work_query_out = crate::think::prioritized_work(&mut ctx, target);
            return Thought {
                job: None,
                trail: Vec::new(),
                next_idle_is_wait: None,
                queue: Vec::new(),
                bill_queue: Vec::new(),
            };
        }
        let result = think(&defs.think_trees[tree], &mut ctx);
        let think_data = std::mem::take(&mut ctx.think_data);
        let unsupported_next = ctx.next_idle_is_wait_out;
        let bill_queue = std::mem::take(&mut *chosen_out.borrow_mut());
        let thought = match result {
            Some(r) => Thought {
                bill_queue: if matches!(r.job.kind, JobKind::DoBill { .. }) {
                    bill_queue
                } else {
                    Vec::new()
                },
                job: Some(r.job),
                trail: r.trail,
                next_idle_is_wait: r.next_idle_is_wait,
                queue: r.queue,
            },
            None => Thought {
                job: None,
                trail: Vec::new(),
                next_idle_is_wait: unsupported_next,
                queue: Vec::new(),
                bill_queue: Vec::new(),
            },
        };
        self.pawns[i].mind.think_data = think_data;
        thought
    }

    pub fn advance(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.tick();
        }
    }
}

/// The outcome of thinking: a job (if any), the think-tree trail that
/// produced it, and the pawn's new wander alternation flag.
struct Thought {
    job: Option<Job>,
    trail: Vec<String>,
    next_idle_is_wait: Option<bool>,
    queue: Vec<ItemId>,
    /// The bill job's chosen ingredients (`targetQueueB`, `countQueue`).
    bill_queue: Vec<(ItemId, u32)>,
}

/// What happened in a job driver's tick that needs the whole simulation.
enum JobEvent {
    None,
    Ended(bool),
    ArrivedAtFood,
    ArrivedAtChewSpot,
    DoneChewing,
    ArrivedAtFoodInPlace,
    DoneChewingInPlace,
    ArrivedAtCorpse,
    DoneChewingCorpse,
    ArrivedAtExit,
    ArrivedAtFilth,
    /// The filth being cleaned is gone or left the home area.
    CleanTargetLost,
    ArrivedAtHaulSource,
    ArrivedAtHaulCell,
    ArrivedAtContainerSource,
    ArrivedAtContainer,
    ArrivedAtFrame,
    ArrivedToSow,
    ArrivedToRoof,
    ArrivedToFloor,
    ArrivedToWall,
    ArrivedAtPatient,
    ArrivedAtBedWithPatient,
    ArrivedAtFoodForPatient,
    ArrivedToFeed,
    FedPatient,
    ArrivedToTend,
    TendDone,
    ArrivedAtFuel,
    ArrivedToMine,
    ArrivedToDeconstruct,
    ArrivedToFlick,
    ArrivedToEquip,
    ArrivedToPlaceFrame,
    HuntTick,
    Flicked,
    ArrivedToResearch,
    ArrivedForJoy,
    ArrivedAtIngredient,
    ArrivedAtTableWithIngredient,
    ArrivedToWorkBill,
    /// The bill job's work toil used the table this tick.
    BillTableUsed(ItemId),
    ArrivedAtComponent,
    ArrivedToFix,
    Fixed,
    ArrivedToRefuel,
    RefuelDone,
    ArrivedToHarvest,
    /// The plant being harvested is gone.
    HarvestTargetLost,
    /// A fail condition ended the job (`Incompletable`).
    Failed,
    /// The path's target disappeared (`ErroredPather`).
    PatherError,
}

fn set_clean(pawn: &mut Pawn, new_target: Option<ItemId>, new_stage: CleanStage) {
    if let Some(Job {
        kind: JobKind::Clean { target, stage },
        ..
    }) = &mut pawn.job
    {
        *target = new_target;
        *stage = new_stage;
    }
}

fn set_ingest_stage(pawn: &mut Pawn, new_stage: IngestStage) {
    if let Some(Job {
        kind: JobKind::Ingest { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new_stage;
    }
}

/// One tick of the chew countdown; `true` when chewing is done.
fn chew_tick(pawn: &mut Pawn) -> bool {
    match &mut pawn.job {
        Some(Job {
            kind:
                JobKind::Ingest {
                    stage: IngestStage::Chew { ticks_left },
                    ..
                },
            ..
        }) => {
            *ticks_left -= 1;
            *ticks_left <= 0
        }
        _ => false,
    }
}

/// Runs the current job's per-tick driver.
/// `IsValidStorageFor` from the map alone.
fn storage_valid(map: &Map, defs: &GameDefs, cell: Cell, def: DefId<ThingDef>) -> bool {
    let limit = defs.things[def].stack_limit as u32;
    if !map.standable_things(defs, cell) {
        return false;
    }
    let items: Vec<_> = map.items_at(cell).filter(|i| !i.is_filth()).collect();
    let room = items
        .iter()
        .any(|i| i.def == def && i.stack_count < limit && defs.things[def].ever_storable());
    (room || items.len() < crate::haul::MAX_ITEMS_IN_CELL)
        && map
            .storage
            .zone_at(cell)
            .is_some_and(|z| map.storage.zone(z).accepts(defs, def))
}

fn tick_job(pawn: &mut Pawn, map: &Map, defs: &GameDefs, grid: &PathGrid, t: u64) -> JobEvent {
    if pawn.job.is_none() {
        return JobEvent::None;
    }
    if let Some(event) = construction::tick_construct(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = farming::tick_farm(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = roofing::tick_roof(pawn, map, defs, grid, t) {
        return event;
    }
    if let Some(event) = refueling::tick_refuel(pawn, map, defs, grid, t) {
        return event;
    }
    if let Some(event) = mining::tick_mine(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = power::tick_fix(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = cooking::tick_do_bill(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = joy::tick_joy(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = researching::tick_research(pawn, map, grid, t) {
        return event;
    }
    match pawn.job.as_ref().map(|j| j.kind) {
        Some(JobKind::Hunt { stage, .. }) => {
            if matches!(
                stage,
                crate::job::HuntStage::GotoCast { .. } | crate::job::HuntStage::GotoVictim
            ) {
                tick_movement(pawn, grid, map, t);
            }
            return JobEvent::HuntTick;
        }
        Some(JobKind::Flee { .. }) => {
            tick_movement(pawn, grid, map, t);
            return if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::Ended(true)
            };
        }
        _ => {}
    }
    if let Some(event) = wildlife::tick_ingest_in_place(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = predation::tick_predator(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = wildlife::tick_exit_map(pawn, map, grid, t) {
        return event;
    }
    if let Some(JobKind::PlaceNoCostFrame { blueprint, .. }) = pawn.job.as_ref().map(|j| j.kind) {
        if map.constructible(blueprint).is_none() {
            return JobEvent::Failed;
        }
        tick_movement(pawn, grid, map, t);
        return if pawn.is_moving() {
            JobEvent::None
        } else {
            JobEvent::ArrivedToPlaceFrame
        };
    }
    if let Some(JobKind::Equip { item }) = pawn.job.as_ref().map(|j| j.kind) {
        if map.item(item).is_none() {
            return JobEvent::Failed;
        }
        tick_movement(pawn, grid, map, t);
        return if pawn.is_moving() {
            JobEvent::None
        } else {
            JobEvent::ArrivedToEquip
        };
    }
    if let Some(event) = power::tick_flick(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = deconstructing::tick_deconstruct(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = flooring::tick_affect_floor(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = flooring::tick_smooth_wall(pawn, map, defs, grid, t) {
        return event;
    }
    if let Some(event) = rescuing::tick_rescue(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = rescuing::tick_feed(pawn, map, grid, t) {
        return event;
    }
    if let Some(event) = tending::tick_tend(pawn, map, grid, t) {
        return event;
    }
    let Some(job) = pawn.job.as_ref() else {
        return JobEvent::None;
    };
    match job.kind {
        // Waits end through expiry in the interval logic.
        JobKind::Wait { .. } => JobEvent::None,
        // Handled by the roof and refuel drivers above.
        JobKind::BuildRoof { .. }
        | JobKind::AffectFloor { .. }
        | JobKind::SmoothWall { .. }
        | JobKind::Rescue { .. }
        | JobKind::FeedPatient { .. }
        | JobKind::TendPatient { .. }
        | JobKind::Refuel { .. }
        | JobKind::Mine { .. }
        | JobKind::Deconstruct { .. }
        | JobKind::FixBrokenDown { .. }
        | JobKind::Flick { .. }
        | JobKind::Research { .. }
        | JobKind::Joy { .. }
        | JobKind::DoBill { .. } => JobEvent::None,
        // The goto and clean toils jump back to the queue when the target
        // is gone or outside the home area.
        JobKind::Clean { target, stage } => {
            let valid = target
                .and_then(|t| map.item(t))
                .is_some_and(|item| map.home[item.position]);
            match stage {
                _ if !valid => JobEvent::CleanTargetLost,
                CleanStage::Goto => {
                    tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtFilth
                    }
                }
                _ => JobEvent::None,
            }
        }
        JobKind::Haul {
            source,
            dest,
            stage,
            aside,
            ..
        } => {
            // Storage hauls fail when the cell stops being valid storage.
            let view = |def| aside || storage_valid(map, defs, dest, def);
            match stage {
                HaulStage::GotoSource => {
                    // A destroyed source fails its pending path first.
                    let Some(item) = map.item(source) else {
                        return JobEvent::PatherError;
                    };
                    if !view(item.def) {
                        return JobEvent::Failed;
                    }
                    tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtHaulSource
                    }
                }
                HaulStage::CarryToCell => {
                    let Some(c) = pawn.carried else {
                        return JobEvent::Failed;
                    };
                    if !view(c.def) {
                        return JobEvent::Failed;
                    }
                    tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtHaulCell
                    }
                }
                HaulStage::Delay => {
                    if pawn.carried.is_none() {
                        JobEvent::Failed
                    } else {
                        JobEvent::None
                    }
                }
            }
        }
        JobKind::Ingest { stage, .. } => match stage {
            IngestStage::GotoFood { .. } | IngestStage::CarryToChewSpot { .. } => {
                tick_movement(pawn, grid, map, t);
                if pawn.is_moving() {
                    JobEvent::None
                } else if matches!(stage, IngestStage::GotoFood { .. }) {
                    JobEvent::ArrivedAtFood
                } else {
                    JobEvent::ArrivedAtChewSpot
                }
            }
            IngestStage::Chew { .. } => {
                if chew_tick(pawn) {
                    JobEvent::DoneChewing
                } else {
                    JobEvent::None
                }
            }
        },
        JobKind::LayDown { .. } => {
            if pawn.is_moving() {
                tick_movement(pawn, grid, map, t);
                // Arriving starts the lay-down toil within the same tick, so
                // its tick action (falling asleep) runs at once.
                if pawn.is_moving() {
                    return JobEvent::None;
                }
            }
            // Lying on the spot: fall asleep / wake up. The job never ends
            // by itself (see the override check in `Sim::tick`).
            let rest = pawn.needs.rest_level().unwrap_or(1.0);
            if !pawn.asleep {
                if can_fall_asleep(rest, pawn.needs.is_starving()) {
                    pawn.asleep = true;
                }
            } else if should_wake_up(rest) {
                pawn.asleep = false;
            }
            JobEvent::None
        }
        JobKind::Goto { .. } => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::Ended(true)
            }
        }
        JobKind::HaulToContainer { .. }
        | JobKind::FinishFrame { .. }
        | JobKind::Sow { .. }
        | JobKind::Harvest { .. }
        | JobKind::AttackMelee { .. }
        | JobKind::WaitCombat
        | JobKind::Equip { .. }
        | JobKind::PlaceNoCostFrame { .. }
        | JobKind::IngestInPlace { .. }
        | JobKind::PredatorHunt { .. }
        | JobKind::ExitMap { .. }
        | JobKind::Hunt { .. }
        | JobKind::Flee { .. }
        | JobKind::AttackStatic { .. } => JobEvent::None,
    }
}

// DIFFERENTIAL VERIFIED: per-tick payment, overshoot carry on every cell
// transition, first-step delay (2, or 3 with a path re-check) and arrival
// tick against 21 original-game traces.
/// Walks the pawn along its path for one tick: pay this tick's cost; when
/// the remaining cost reaches zero, enter the cell and set up the next one in
/// the same tick, carrying any overshoot. Movement starts
/// `PATH_START_LATENCY_TICKS` after the job (the game's asynchronous path
/// request).
fn tick_movement(pawn: &mut Pawn, grid: &PathGrid, map: &Map, t: u64) {
    let doors = map.doors();
    // A busy stance (waiting for a door) stops the path follower.
    if t < pawn.stance_until {
        return;
    }
    if t < pawn.move_ready_tick {
        // While its path request is pending the follower re-checks on its
        // 30-tick hash tick; with no path yet it needs a new one, so the
        // request is replaced and the result arrives two ticks later
        // (verified against the original game).
        if is_hash_interval_tick(t, pawn.id_number, PATH_RECHECK_INTERVAL_TICKS) {
            pawn.move_ready_tick = t + PATH_START_LATENCY_TICKS;
        }
        return;
    }
    let urgency = pawn
        .job
        .as_ref()
        .map_or(LocomotionUrgency::Jog, |j| j.urgency);
    if pawn.step.is_none() && !setup_next_step(pawn, grid, map, urgency, 0.0) {
        return;
    }
    let Some(step) = &mut pawn.step else { return };
    // Payment stops once the cell is paid for; a pawn waiting at a door
    // keeps its overshoot.
    if step.cost_left > 0.0 {
        step.cost_left -= cost_paid_per_tick(step.cost_total);
    }
    if step.cost_left <= 0.0 {
        // `TryEnterNextPathCell`: a door that is closed or still opening
        // makes the pawn open it and wait until it is fully open.
        if doors.iter().any(|d| d.cell == step.to && d.must_wait()) {
            pawn.door_request = Some(step.to);
            return;
        }
        let overshoot = step.cost_left;
        let left = pawn.position;
        let total = step.cost_total;
        pawn.position = step.to;
        pawn.step = None;
        pawn.entered_cell = true;
        if doors.iter().any(|d| d.cell == left) {
            // Leaving a door the follower starts closing it and returns
            // before setting up the next cell (`TryEnterNextPathCell`); the
            // next step begins on the following tick with the overshoot.
            // At the end of the path too: the follower is still moving
            // until that next tick finds no more cells (recorded walking
            // into a room through its door).
            pawn.left_door = Some(left);
            pawn.step = Some(Step {
                to: pawn.position,
                cost_left: overshoot,
                cost_total: total,
            });
            return;
        }
        if pawn.path.is_empty() {
            pawn.destination = None;
        } else {
            setup_next_step(pawn, grid, map, urgency, overshoot);
        }
    }
}

/// Starts walking into the next path cell. `carry` (<= 0) is the overshoot
/// from the previous cell. Returns false if there is no next cell or it is
/// no longer reachable.
fn setup_next_step(
    pawn: &mut Pawn,
    grid: &PathGrid,
    map: &Map,
    urgency: LocomotionUrgency,
    carry: f32,
) -> bool {
    let Some(next) = pawn.path.pop_front() else {
        return false;
    };
    let dir = Cell::new(next.x - pawn.position.x, next.z - pawn.position.z);
    if !grid.can_step(pawn.position, dir) {
        // The path became blocked (e.g. a wall was built): look for a new
        // one to the same destination; without one the job fails.
        // COMPATIBILITY TODO: currently approximate — the game requests a
        // new path asynchronously (`PathFollower.NeedNewPath`), which takes
        // a few ticks; we repath at once.
        pawn.path.clear();
        let new_path = pawn.destination.and_then(|d| {
            find_path(
                grid,
                pawn.position,
                d,
                pawn.move_costs,
                COLONIST_HEURISTIC_STRENGTH,
            )
            .ok()
        });
        match new_path {
            Some(path) if !path.cells.is_empty() => {
                pawn.path = path.cells.into();
                return setup_next_step(pawn, grid, map, urgency, carry);
            }
            _ => {
                pawn.destination = None;
                pawn.path_failed = true;
                return false;
            }
        }
    }
    pawn.rotation = Rot4::from_step(dir);
    // MoveSpeed is evaluated as the step starts, with the light on the cell
    // the pawn is in (`StatPart_Glow`).
    // Carrying a pawn: MoveSpeed × 0.6 (`Pawn.TicksPerMove`).
    let carrying = pawn.carried_pawn.is_some();
    let costs = if pawn.base_move_speed <= 0.0 || pawn.health.downed {
        pawn.move_costs
    } else if pawn.move_glow_curve.is_empty() {
        if carrying {
            MoveCosts::from_move_speed(pawn.base_move_speed * pawn.move_capacity_factor * 0.6)
        } else {
            pawn.move_costs
        }
    } else {
        let factor =
            crate::food::evaluate_curve(&pawn.move_glow_curve, map.ground_glow(pawn.position));
        let speed = pawn.base_move_speed * pawn.move_capacity_factor * factor;
        MoveCosts::from_move_speed(if carrying { speed * 0.6 } else { speed })
    };
    let total = costs.step_cost(dir, grid.cost(next).unwrap_or(0), urgency);
    pawn.step = Some(Step {
        to: next,
        cost_left: (total + carry.min(0.0)).max(1.0),
        cost_total: total,
    });
    true
}
