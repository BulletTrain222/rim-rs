//! Growing job drivers (docs/research.md §24): `JobDriver_PlantSow` and
//! `JobDriver_PlantHarvest`, and the plants' long ticks.

use super::{JobEvent, PATH_START_LATENCY_TICKS, Sim};
use crate::grid::Cell;

use crate::job::{Job, JobKind, PlantWorkStage, SowStage};
use crate::map::{ItemId, Map};
use crate::path::{COLONIST_HEURISTIC_STRENGTH, PathGrid, find_path_touch};
use crate::pawn::Pawn;
use crate::plant::{GrowthConditions, LONG_TICK, round_random, sun_glow, tick_long};
use crate::reservation::{STACK_ALL, Target};

/// `WildPlantSpawner` state: the biome, the position in the map's random
/// cell order, and the chance from density measured over the last cycle.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WildPlants {
    pub biome: Option<rimworld_defs::DefId<rimworld_defs::BiomeDef>>,
    pub order_seed: u32,
    pub cycle: usize,
    pub chance_from_density: f32,
    pub desired_tmp: f32,
    pub fertile_tmp: u32,
    #[serde(skip)]
    order: Vec<Cell>,
}

/// Outcome of starting a walk to touch a cell.
pub(super) enum Touch {
    /// Already touching it.
    Here,
    Walking,
    NoPath,
}

/// `GenDate.DayOfYear` at the local time of `longitude`.
fn local_day_of_year(abs_tick: i64, longitude: f32) -> i32 {
    let local = abs_tick + crate::climate::time_zone(longitude) * 2500;
    (local.div_euclid(60_000)).rem_euclid(60) as i32
}

impl Sim {
    /// Day of the year and fraction of the day now, local to the map's
    /// longitude (`GenLocalDate`).
    // COMPATIBILITY TODO: currently approximate — no world calendar: the
    // start date and longitude are scenario settings.
    pub fn day_percent(&self) -> f32 {
        crate::climate::local_day_percent(self.abs_tick(), self.longitude)
    }

    pub fn day_of_year(&self) -> i32 {
        local_day_of_year(self.abs_tick(), self.longitude)
    }

    /// Glow on an unroofed cell now (the sky's). The game refreshes the
    /// sky glow in `MapUpdate`, once a frame after that frame's ticks, so
    /// a tick sees the previous tick's sun: at one tick per frame exactly
    /// the previous tick, which every probe trace showed.
    // COMPATIBILITY TODO: currently approximate — at higher speeds the
    // game runs several ticks per frame and the lag varies with frame
    // timing; roofs, lamps and weather/eclipses are not modelled.
    pub fn outdoor_glow(&self) -> f32 {
        let abs = self.abs_tick() - 1;
        let day_percent = crate::climate::local_day_percent(abs, self.longitude);
        sun_glow(
            self.latitude,
            local_day_of_year(abs, self.longitude),
            day_percent,
        )
    }

    /// Walks pawn `i` to touch `cell` (`PathEndMode.Touch`): the search
    /// ends at the first cell around (or on) it from which it may be
    /// touched (`PathFinder.MakeDestination`).
    pub(super) fn walk_to_touch(&mut self, i: usize, cell: Cell, thing: bool) -> Touch {
        let tick = self.tick;
        let grid = &self.path_grid;
        let map = &self.map;
        // Walls as target things can always be touched diagonally.
        let wall = thing && crate::roof::holds_roof(map, &self.defs, cell);
        let touches = |c: Cell| {
            (wall && c.chebyshev(cell) <= 1) || crate::roof::touch_allowed(grid, map, c, cell)
        };
        let pawn = &mut self.pawns[i];
        pawn.path.clear();
        pawn.destination = None;
        let at = pawn.next_stop();
        if touches(at) {
            return Touch::Here;
        }
        match find_path_touch(
            grid,
            at,
            cell,
            touches,
            pawn.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) if !path.cells.is_empty() => {
                let end = *path.cells.last().expect("non-empty");
                pawn.path = path.cells.into();
                pawn.destination = Some(end);
                pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, cell, thing);
                Touch::Walking
            }
            Ok(_) => Touch::Here,
            Err(_) => Touch::NoPath,
        }
    }

    /// Starts the sow job's walk (`GotoCell` Touch).
    pub(super) fn begin_sow(&mut self, i: usize) -> bool {
        let Some(JobKind::Sow { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        match self.walk_to_touch(i, cell, false) {
            Touch::Here => {
                self.start_sowing(i);
                true
            }
            Touch::Walking => true,
            Touch::NoPath => false,
        }
    }

    /// The sow toil starts: the plant appears unsown-grown (growth 0) and
    /// is reserved.
    pub(super) fn start_sowing(&mut self, i: usize) {
        let Some(JobKind::Sow { cell, plant, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let def = &self.defs.things[plant];
        let max_hp = def.stat("MaxHitPoints").unwrap_or(100.0);
        let id = self.map.spawn_plant(plant, cell, 0.0, max_hp);
        if let Some(p) = self.map.plant_mut(id) {
            p.sown = true;
        }
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        self.reservations
            .reserve(claimant, job_id, Target::Item(id), 1, 1, STACK_ALL);
        self.refresh_path_grid();
        if let Some(Job {
            kind: JobKind::Sow { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = SowStage::Sowing {
                plant: id,
                work_done: 0.0,
            };
        }
    }

    /// Sowing progress: `PlantWorkSpeed × delta` until the plant's
    /// `sowWork`; the plant then starts growing (growth 0.0001).
    pub(super) fn sow_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Sow {
                    plant: def,
                    stage: SowStage::Sowing { plant, work_done },
                    ..
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        self.learn(i, "Plants", 0.085 * delta as f32);
        let defs = self.defs.clone();
        let speed = self.pawn_stat_of(i, "PlantWorkSpeed");
        let done = work_done + speed * delta as f32;
        let sow_work = defs.things[def].plant.as_ref().map_or(10.0, |p| p.sow_work);
        if done >= sow_work {
            if let Some(p) = self.map.plant_mut(plant) {
                p.growth = 0.0001;
            }
            self.end_job(i, true);
            return;
        }
        if let Some(Job {
            kind:
                JobKind::Sow {
                    stage: SowStage::Sowing { work_done, .. },
                    ..
                },
            ..
        }) = &mut self.pawns[i].job
        {
            *work_done = done;
        }
    }

    /// The sow toil's finish action: an unfinished sowing destroys the
    /// plant.
    pub(super) fn sow_cleanup(&mut self, i: usize) {
        let Some(JobKind::Sow {
            stage: SowStage::Sowing { plant, .. },
            ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        if self
            .map
            .plant(plant)
            .is_some_and(|p| p.life_stage() == crate::plant::LifeStage::Sowing)
        {
            self.map.remove_plant(plant);
            self.reservations
                .release_all_for_target(Target::Item(plant));
            self.refresh_path_grid();
        }
    }

    /// Harvest: drop queued plants that are gone, take the next and walk to
    /// touch it; with none left the job succeeds.
    pub(super) fn harvest_extract(&mut self, i: usize) {
        let map = &self.map;
        self.pawns[i]
            .target_queue
            .retain(|&t| map.plant(t).is_some());
        if self.pawns[i].target_queue.is_empty() {
            self.end_job(i, true);
            return;
        }
        let t = self.pawns[i].target_queue.remove(0);
        let cell = self.map.plant(t).expect("kept").position;
        if let Some(Job {
            kind: JobKind::Harvest { target, stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *target = Some(t);
            *stage = PlantWorkStage::Goto;
        }
        match self.walk_to_touch(i, cell, true) {
            Touch::Here => self.harvest_arrived(i),
            Touch::Walking => {}
            Touch::NoPath => self.harvest_extract(i),
        }
    }

    pub(super) fn harvest_arrived(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::Harvest { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = PlantWorkStage::Cutting { work_done: 0.0 };
        }
    }

    /// Harvest progress: `PlantWorkSpeed × lerp(3.3, 1, growth) × delta`
    /// until the plant's `harvestWork`. Then the yield (or a failed
    /// harvest for humanlikes when `Rand.Value > PlantHarvestYield`) is put
    /// down near the pawn and the plant is collected.
    // COMPATIBILITY TODO: currently approximate — additional comp yields,
    // stumps and designations are not modelled.
    pub(super) fn harvest_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Harvest {
                    target: Some(t),
                    stage: PlantWorkStage::Cutting { work_done },
                    cut,
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(plant) = self.map.plant(t).cloned() else {
            return;
        };
        let defs = self.defs.clone();
        let Some(props) = defs.things[plant.def].plant.as_ref() else {
            return;
        };
        // `xpPerTick`: harvesting teaches; cutting only a plant that would
        // yield now.
        // COMPATIBILITY TODO: currently approximate — the cut job decides
        // per plant from the harvested thing and minimum growth only.
        let teaches = !cut
            || (props.harvested_thing_def.is_some() && plant.growth >= props.harvest_min_growth);
        if teaches {
            self.learn(i, "Plants", 0.085 * delta as f32);
        }
        let pawn = &self.pawns[i];
        let race = &defs.things[pawn.race];
        let speed = self.pawn_stat_of(i, "PlantWorkSpeed");
        let factor = 3.3 + (1.0 - 3.3) * plant.growth.clamp(0.0, 1.0);
        let done = work_done + speed * factor * delta as f32;
        if done < props.harvest_work {
            if let Some(Job {
                kind:
                    JobKind::Harvest {
                        stage: PlantWorkStage::Cutting { work_done },
                        ..
                    },
                ..
            }) = &mut self.pawns[i].job
            {
                *work_done = done;
            }
            return;
        }
        if let Some(product) = props
            .harvested_thing_def
            .as_deref()
            .and_then(|n| defs.things.id(n))
        {
            // COMPATIBILITY TODO: currently approximate — drugs use
            // DrugHarvestYield in the game.
            let stat = self.pawn_stat_of(i, "PlantHarvestYield");
            let humanlike =
                race.race.as_ref().and_then(|r| r.intelligence.as_deref()) == Some("Humanlike");
            if humanlike && props.harvest_failable && self.rng.value() > stat {
                // Failed harvest: nothing.
            } else {
                let mut n = plant.yield_now(props, &mut self.rng);
                if stat > 1.0 {
                    n = round_random(n as f32 * stat, &mut self.rng);
                }
                if n > 0 {
                    let at = self.pawns[i].position;
                    self.place_near(product, n, at);
                }
            }
        }
        // `PlantCollected`; cutting then destroys what is left
        // (`Toils_Interact.DestroyThing`).
        if props.harvest_destroys() || cut {
            let cell = plant.position;
            self.map.remove_plant(t);
            self.reservations.release_all_for_target(Target::Item(t));
            // `TrySpawnStump`: a felled tree that was harvestable leaves its
            // stump; a stump from cutting is designated for cutting too and
            // joins this job's queue.
            if plant.growth >= props.harvest_min_growth
                && let Some(stump) = props
                    .chopped_thing_def
                    .as_deref()
                    .and_then(|n| defs.things.id(n))
            {
                let id = self.spawn_stump(stump, cell, plant.growth);
                if cut {
                    self.map.designate_cut(id);
                    self.pawns[i].target_queue.push(id);
                }
            }
            self.refresh_path_grid();
        } else if let Some(p) = self.map.plant_mut(t) {
            p.growth = props.harvest_after_growth;
        }
        // `PlantCollected` takes the designation off what is left.
        self.map.clear_harvest_designation(t);
        self.harvest_extract(i);
    }

    /// Spawns a stump (`DeadPlant`) at `growth`, with hit points from its
    /// `startingHpRange` (`ThingMaker` post-make).
    fn spawn_stump(
        &mut self,
        def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
        cell: Cell,
        growth: f32,
    ) -> ItemId {
        let max = crate::stats::def_stat(&self.defs, &self.defs.things[def], None, "MaxHitPoints");
        let (lo, hi) = self.defs.things[def].starting_hp_range;
        let frac = self.rng.range_f32(lo, hi).clamp(0.0, 1.0);
        let id = self.map.spawn_plant(def, cell, growth, max);
        if let Some(p) = self.map.plant_mut(id) {
            p.hit_points = (max * frac).round_ties_even();
        }
        id
    }

    /// Plants' long ticks (`Plant.TickLong` from the long tick list: a
    /// plant ticks when `TicksGame % 2000` is its `thingIDNumber` modulo
    /// 2000; plants tick after the pawns).
    // COMPATIBILITY TODO: currently approximate — the cell temperature is
    // the scenario's constant outdoor temperature and every cell is
    // outdoors; a newly spawned plant joins its bucket at once (the game
    // registers it at the next tick list pass).
    pub(super) fn tick_plants(&mut self) {
        let t = self.tick;
        let glow = self.outdoor_glow();
        let day_percent = self.day_percent();
        let defs = self.defs.clone();
        let mut died: Vec<ItemId> = Vec::new();
        let terrain: Vec<f32> = self
            .map
            .plants()
            .iter()
            .map(|p| defs.terrain[self.map.terrain[p.position]].fertility)
            .collect();
        // No sun under a roof (`GlowGrid`: roofed cells get no sky glow).
        let roofed: Vec<bool> = self
            .map
            .plants()
            .iter()
            .map(|p| self.map.roofed(p.position))
            .collect();
        // Each plant has its cell's (room's) temperature.
        let temps: Vec<f32> = self
            .map
            .plants()
            .iter()
            .map(|p| self.cell_temperature(p.position))
            .collect();
        let due: Vec<usize> = self
            .map
            .plants()
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                (t % LONG_TICK as u64) as i32 == p.id_number.rem_euclid(LONG_TICK as i32)
            })
            .map(|(n, _)| n)
            .collect();
        if due.is_empty() {
            return;
        }
        let plants = self.map.plants_mut();
        for n in due {
            let p = &mut plants[n];
            let Some(props) = defs.things[p.def].plant.as_ref() else {
                continue;
            };
            // Stumps don't grow or die of age.
            // COMPATIBILITY TODO: currently approximate — `DeadPlant` long
            // ticks are not modelled at all.
            if props.is_stump {
                continue;
            }
            let alive = tick_long(
                p,
                props,
                GrowthConditions {
                    now: self.tick as i64,
                    fertility: terrain[n],
                    glow: if roofed[n] { 0.0 } else { glow },
                    temperature: temps[n],
                    day_percent,
                },
            );
            if !alive {
                died.push(p.id);
            }
        }
        for id in died {
            self.map.remove_plant(id);
            self.reservations.release_all_for_target(Target::Item(id));
        }
    }

    /// Sets the map's biome (wild plant regrowth follows it).
    pub fn set_biome(
        &mut self,
        biome: Option<rimworld_defs::DefId<rimworld_defs::BiomeDef>>,
        seed: u32,
    ) {
        self.wild.biome = biome;
        self.wild.order_seed = seed;
        self.wild.cycle = 0;
        self.wild.order.clear();
        // Start with the density the map has now.
        let Some(b) = biome.map(|b| &self.defs.biomes[b]) else {
            return;
        };
        let (mut desired, mut fertile) = (0.0, 0u32);
        for c in self.map.size().cells() {
            let f = self.defs.terrain[self.map.terrain[c]].fertility;
            desired += (f * b.plant_density * f).min(1.0);
            fertile += u32::from(f > 0.0);
        }
        self.wild.chance_from_density = if fertile > 0 {
            desired / fertile as f32
        } else {
            0.0
        };
    }

    /// `WildPlantSpawnerTick`: each tick, ceil(area × 0.0001) cells of the
    /// map's random cell order get a regrowth check: the chance from density,
    /// then an MTB event of the biome's `wildPlantRegrowDays`.
    // COMPATIBILITY TODO: currently approximate — the spawn check itself is
    // simplified (see `mapgen::spawn_wild_plant`), and the cell order is
    // not the game's `cellsInRandomOrder`.
    pub(super) fn tick_wild_plants(&mut self) {
        let Some(b) = self.wild.biome else {
            return;
        };
        let defs = self.defs.clone();
        let biome = &defs.biomes[b];
        let area = self.map.size().area();
        if self.wild.order.len() != area {
            let mut order: Vec<Cell> = self.map.size().cells().collect();
            crate::rand::Rand::new(self.wild.order_seed).shuffle(&mut order);
            self.wild.order = order;
        }
        let plants: Vec<(rimworld_defs::DefId<rimworld_defs::ThingDef>, f32)> = biome
            .wild_plants
            .iter()
            .filter_map(|(n, w)| Some((defs.things.id(n)?, *w)))
            .filter(|(id, _)| defs.things[*id].plant.is_some())
            .collect();
        let n = (area as f32 * 0.0001).ceil() as usize;
        let mut spawned = false;
        for _ in 0..n {
            if self.wild.cycle >= area {
                self.wild.chance_from_density = if self.wild.fertile_tmp > 0 {
                    self.wild.desired_tmp / self.wild.fertile_tmp as f32
                } else {
                    0.0
                };
                self.wild.desired_tmp = 0.0;
                self.wild.fertile_tmp = 0;
                self.wild.cycle = 0;
            }
            let c = self.wild.order[self.wild.cycle];
            let f = defs.terrain[self.map.terrain[c]].fertility;
            self.wild.desired_tmp += (f * biome.plant_density * f).min(1.0);
            self.wild.fertile_tmp += u32::from(f > 0.0);
            if self.rng.chance(self.wild.chance_from_density)
                && self
                    .rng
                    .mtb_event_occurs(biome.wild_plant_regrow_days, 60_000.0, 10_000.0)
                && self.path_grid.walkable(c)
                && crate::mapgen::spawn_wild_plant(
                    &mut self.map,
                    &defs,
                    biome,
                    &plants,
                    c,
                    false,
                    &mut self.rng,
                )
            {
                spawned = true;
            }
            self.wild.cycle += 1;
        }
        if spawned {
            self.refresh_path_grid();
        }
    }

    /// Debug/replay tool: sets the `thingIDNumber` of the plant on `cell`.
    pub fn debug_set_plant_id_number(&mut self, cell: Cell, id_number: i32) {
        if let Some(id) = self.map.plant_at(cell).map(|p| p.id)
            && let Some(p) = self.map.plant_mut(id)
        {
            p.id_number = id_number;
        }
    }

    /// Designates every plant on `cells` for cutting (the cut designator).
    pub fn designate_cut_plants(&mut self, cells: &[Cell]) -> usize {
        let ids: Vec<ItemId> = cells
            .iter()
            .filter(|c| self.map.size().contains(**c))
            .filter_map(|&c| self.map.plant_at(c).map(|p| p.id))
            .collect();
        for &id in &ids {
            self.map.designate_cut(id);
        }
        ids.len()
    }

    /// Designates the plants on `cells` that can be harvested now and
    /// yield something for harvest (`Designator_PlantsHarvest`, also
    /// chopping trees for wood).
    // COMPATIBILITY TODO: currently approximate — harvest tags (wood versus
    // standard) are not told apart.
    pub fn designate_harvest_plants(&mut self, cells: &[Cell]) -> usize {
        let ids: Vec<ItemId> = cells
            .iter()
            .filter(|c| self.map.size().contains(**c))
            .filter_map(|&c| self.map.plant_at(c))
            .filter(|p| {
                self.defs.things[p.def].plant.as_ref().is_some_and(|pp| {
                    pp.harvested_thing_def.is_some() && p.growth >= pp.harvest_min_growth
                })
            })
            .map(|p| p.id)
            .collect();
        for &id in &ids {
            self.map.designate_harvest(id);
        }
        ids.len()
    }

    /// Designates a growing zone growing `plant` over `cells` (cells free of
    /// other zones, walkable, fertile for the plant). Cells touching an
    /// existing growing zone extend it.
    // COMPATIBILITY TODO: currently approximate — the zone designator's
    // rules (e.g. joining, terrain affordances) are simplified.
    pub fn designate_growing_zone(
        &mut self,
        plant: rimworld_defs::DefId<rimworld_defs::ThingDef>,
        cells: &[Cell],
    ) -> Option<usize> {
        let min = self.defs.things[plant]
            .plant
            .as_ref()
            .map_or(0.0, |p| p.fertility_min);
        let cells: Vec<Cell> = cells
            .iter()
            .copied()
            .filter(|&c| {
                self.map.size().contains(c)
                    && self.path_grid.walkable(c)
                    && self.map.storage.zone_at(c).is_none()
                    && self.map.growing_zone_at(c).is_none()
                    && self.defs.terrain[self.map.terrain[c]].fertility >= min
            })
            .collect();
        if cells.is_empty() {
            return None;
        }
        for &c in &cells {
            self.mark_home_around_zone_cell(c);
        }
        let touching = cells.iter().find_map(|&c| {
            Cell::NEIGHBORS_8
                .iter()
                .find_map(|&d| self.map.growing_zone_at(c + d))
        });
        Some(match touching {
            Some(z) => {
                self.map.add_growing_cells(z, &cells);
                z
            }
            None => {
                let z = self.map.add_growing_zone(plant, &cells);
                self.name_new_zone(super::zones::ZoneRef::Growing(z), false);
                z
            }
        })
    }
}

/// The per-tick part of the growing drivers.
pub(super) fn tick_farm(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let kind = pawn.job.as_ref()?.kind;
    Some(match kind {
        JobKind::Sow { cell, plant, stage } => match stage {
            SowStage::Goto => {
                // `FailOn`: the cell cannot take the plant any more.
                if map.plant_at(cell).is_some_and(|p| p.def == plant)
                    || map.plant_at(cell).is_some()
                    || map.buildings[cell].is_some()
                {
                    return Some(JobEvent::Failed);
                }
                super::tick_movement(pawn, grid, map, t);
                if pawn.is_moving() {
                    JobEvent::None
                } else {
                    JobEvent::ArrivedToSow
                }
            }
            SowStage::Sowing { plant, .. } => {
                if map.plant(plant).is_none() {
                    JobEvent::Failed
                } else {
                    JobEvent::None
                }
            }
        },
        JobKind::Harvest { target, stage, .. } => {
            let valid = target.and_then(|t| map.plant(t)).is_some();
            match stage {
                PlantWorkStage::Extract => JobEvent::None,
                _ if !valid => JobEvent::HarvestTargetLost,
                PlantWorkStage::Goto => {
                    super::tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedToHarvest
                    }
                }
                PlantWorkStage::Cutting { .. } => JobEvent::None,
            }
        }
        _ => return None,
    })
}

/// Harvested yield helper for UI: whether a plant is ready.
pub fn plant_ready(sim: &Sim, cell: Cell) -> bool {
    sim.map.plant_at(cell).is_some_and(|p| {
        sim.defs.things[p.def]
            .plant
            .as_ref()
            .is_some_and(|props| p.harvestable_now(props))
    })
}
