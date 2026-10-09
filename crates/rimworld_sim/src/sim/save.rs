//! Saving and loading (our own format: JSON of the simulation state).
//!
//! Only authoritative state is saved. Derived state (path grid, regions,
//! JobDefs, giver lists) is rebuilt on load, so a loaded game continues
//! exactly as the saved one would have. Def references are table indices;
//! the save records a fingerprint of the Def tables and refuses to load
//! with different game data.
// COMPATIBILITY TODO: currently approximate — not the game's XML save
// format (`Scribe`), and saves cannot be moved between different Def data
// (indices are not remapped by defName).

use std::sync::Arc;

use rimworld_defs::{GameDefs, HasDefName};
use serde::{Deserialize, Serialize};

use super::Sim;
use crate::cell_finder::IngestionSpotOrder;
use crate::construct::EnrouteManager;
use crate::job::{JobDefs, WanderParams};
use crate::map::Map;
use crate::pawn::Pawn;
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{DestinationManager, JobId, ReservationManager};

/// Format version of [`SaveGame`].
pub const SAVE_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("not a rim-rs save: {0}")]
    Format(#[from] serde_json::Error),
    #[error("save format version {0} is not supported (expected {SAVE_VERSION})")]
    Version(u32),
    #[error("the save was made with different game data (Def fingerprint mismatch)")]
    Defs,
}

#[derive(Serialize, Deserialize)]
struct SaveGame {
    version: u32,
    defs_fingerprint: u64,
    tick: u64,
    map: Map,
    pawns: Vec<Pawn>,
    next_pawn_id: u32,
    rng: Rand,
    reservations: ReservationManager,
    destinations: DestinationManager,
    next_job_id: JobId,
    ingest_order: IngestionSpotOrder,
    use_work_priorities: bool,
    enroute: EnrouteManager,
    wander: WanderParams,
    default_update_rate: u32,
    latitude: f32,
    outdoor_temperature: f32,
    wild: super::farming::WildPlants,
    #[serde(default)]
    start_day_of_year: i32,
    #[serde(default)]
    climate: Option<crate::climate::Climate>,
    #[serde(default)]
    longitude: f32,
    #[serde(default)]
    next_temperature_refresh: u64,
    #[serde(default)]
    room_signatures: Vec<u64>,
    #[serde(default)]
    queued_roof_rooms: Vec<u64>,
    #[serde(default)]
    roof_collapse: Vec<crate::grid::Cell>,
    #[serde(default)]
    steady_cycle: usize,
    #[serde(default)]
    steady_seed: u32,
    #[serde(default)]
    room_temps: Option<super::temperature::RoomTemps>,
    #[serde(default)]
    corpses: Vec<(crate::map::ItemId, crate::pawn::PawnId)>,
    #[serde(default)]
    power: super::power::PowerGrid,
    #[serde(default)]
    research: super::researching::ResearchState,
    #[serde(default)]
    projectiles: Vec<crate::ranged::Projectile>,
    #[serde(default)]
    bills: crate::cook::BillStacks,
}

/// FNV-1a over every Def table's names in order.
fn fingerprint(defs: &GameDefs) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |s: &str| {
        for b in s.bytes().chain(std::iter::once(0)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    fn names<T: HasDefName>(t: &rimworld_defs::DefTable<T>) -> impl Iterator<Item = &str> {
        t.iter().map(|(_, d)| d.def_name())
    }
    for n in names(&defs.terrain)
        .chain(names(&defs.things))
        .chain(names(&defs.pawn_kinds))
        .chain(names(&defs.jobs))
        .chain(names(&defs.think_trees))
        .chain(names(&defs.needs))
        .chain(names(&defs.work_types))
        .chain(names(&defs.work_givers))
        .chain(names(&defs.stats))
        .chain(names(&defs.bodies))
        .chain(names(&defs.body_parts))
        .chain(names(&defs.hediffs))
        .chain(names(&defs.damages))
        .chain(names(&defs.capacities))
        .chain(names(&defs.maneuvers))
        .chain(names(&defs.biomes))
    {
        feed(n);
    }
    h
}

impl Sim {
    /// The simulation state as a save file.
    /// Resets the state the game does not save, as loading does: the
    /// mood's observer countdown, situational-thought and instant-mood
    /// caches, the joy need's last gain tick and the bills' failed-search
    /// deadlines. A running game after this continues exactly like one
    /// loaded from [`Sim::save`] at this moment.
    pub fn forget_unsaved_state(&mut self) {
        for stack in self.bill_stacks.get_mut().unwrap().values_mut() {
            for bill in stack {
                bill.next_search_tick = 0;
            }
        }
        for p in &mut self.pawns {
            if let Some(m) = p.mood.as_mut() {
                let saved = crate::mood::MoodState {
                    level: m.level,
                    memories: std::mem::take(&mut m.memories),
                    last_light_tick: m.last_light_tick,
                    last_outdoor_tick: m.last_outdoor_tick,
                    ..Default::default()
                };
                *m = saved;
            }
            p.needs.joy.last_gain_tick = None;
        }
    }

    pub fn save(&self) -> String {
        let save = SaveGame {
            version: SAVE_VERSION,
            defs_fingerprint: fingerprint(&self.defs),
            tick: self.tick,
            map: self.map.clone(),
            pawns: self.pawns.clone(),
            next_pawn_id: self.next_pawn_id,
            rng: self.rng.clone(),
            reservations: self.reservations.clone(),
            destinations: self.destinations.clone(),
            next_job_id: self.next_job_id,
            ingest_order: self.ingest_order.clone(),
            use_work_priorities: self.use_work_priorities,
            enroute: self.enroute.clone(),
            wander: self.wander,
            default_update_rate: self.default_update_rate,
            latitude: self.latitude,
            outdoor_temperature: self.outdoor_temperature,
            wild: self.wild.clone(),
            start_day_of_year: self.start_day_of_year,
            climate: self.climate,
            longitude: self.longitude,
            next_temperature_refresh: self.next_temperature_refresh,
            room_signatures: {
                let mut v: Vec<u64> = self.room_signatures.iter().copied().collect();
                v.sort_unstable();
                v
            },
            queued_roof_rooms: self.queued_roof_rooms.clone(),
            roof_collapse: self.roof_collapse.clone(),
            steady_cycle: self.steady_cycle,
            steady_seed: self.steady_seed,
            room_temps: Some(self.room_temps.clone()),
            corpses: self.corpses.iter().map(|(&k, &v)| (k, v)).collect(),
            power: self.power.clone(),
            research: self.research.clone(),
            projectiles: self.projectiles.clone(),
            bills: self.bill_stacks.lock().expect("bill stacks").clone(),
        };
        serde_json::to_string(&save).expect("simulation state serializes")
    }

    /// Restores a simulation from [`Sim::save`] output, with the same game
    /// data.
    pub fn load(defs: Arc<GameDefs>, data: &str) -> Result<Sim, LoadError> {
        let save: SaveGame = serde_json::from_str(data)?;
        if save.version != SAVE_VERSION {
            return Err(LoadError::Version(save.version));
        }
        if save.defs_fingerprint != fingerprint(&defs) {
            return Err(LoadError::Defs);
        }
        let mut map = save.map;
        map.reindex_plants();
        // `DesignationManager.ExposeData` reloads its list backward: mine
        // designations come back in reverse order.
        // COMPATIBILITY TODO: currently approximate — the game reverses every
        // designation list on load; only mining (verified) is reversed here.
        map.mine_designations.reverse();
        let mut sim = Sim::with_seed(defs.clone(), map, 0);
        sim.path_grid = sim.map.build_path_grid(&defs);
        sim.regions = Regions::build(&sim.map, &defs, &sim.path_grid);
        sim.job_defs = JobDefs::resolve(&defs);
        sim.tick = save.tick;
        sim.pawns = save.pawns;
        sim.next_pawn_id = save.next_pawn_id;
        sim.rng = save.rng;
        sim.reservations = save.reservations;
        sim.destinations = save.destinations;
        sim.next_job_id = save.next_job_id;
        sim.ingest_order = save.ingest_order;
        sim.use_work_priorities = save.use_work_priorities;
        sim.enroute = save.enroute;
        sim.wander = save.wander;
        sim.default_update_rate = save.default_update_rate;
        sim.latitude = save.latitude;
        sim.outdoor_temperature = save.outdoor_temperature;
        sim.wild = save.wild;
        sim.start_day_of_year = save.start_day_of_year;
        sim.climate = save.climate;
        sim.longitude = save.longitude;
        sim.next_temperature_refresh = save.next_temperature_refresh;
        if !save.room_signatures.is_empty() {
            sim.room_signatures = save.room_signatures.into_iter().collect();
        }
        sim.queued_roof_rooms = save.queued_roof_rooms;
        sim.roof_collapse = save.roof_collapse;
        sim.steady_cycle = save.steady_cycle;
        sim.steady_seed = save.steady_seed;
        if let Some(t) = save.room_temps {
            sim.room_temps = t;
        }
        sim.corpses = save.corpses.into_iter().collect();
        sim.power = save.power;
        sim.research = save.research;
        sim.projectiles = save.projectiles;
        sim.bill_stacks = std::sync::Mutex::new(save.bills);
        sim.update_resource_counts();
        Ok(sim)
    }
}
