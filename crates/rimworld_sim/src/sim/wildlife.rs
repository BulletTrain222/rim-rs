//! Wild animals (docs/research.md §63): the biome's animal ecosystem
//! (`WildAnimalSpawner`, `GenStep_Animals`) and herbivores grazing
//! (`FoodUtility.BestFoodSourceOnMap` for animals, `JobDriver_Ingest` for
//! non-tool-users, `Plant.IngestedCalculateAmounts`).

use rimworld_defs::{DefId, FoodPreferability, PawnKindDef};

use super::Sim;
use crate::cell_finder::MapView;
use crate::grid::Cell;
use crate::job::{IngestStage, Job, JobKind};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::reservation::Target;

/// `WildAnimalSpawner.AnimalCheckInterval`.
const ANIMAL_CHECK_INTERVAL: u64 = 1213;
/// `WildAnimalSpawner.BaseAnimalSpawnChancePerInterval`.
const BASE_ANIMAL_SPAWN_CHANCE: f32 = 0.026955556;
/// `GenStep_Animals`' iteration limit.
const MAP_GEN_MAX_ITERATIONS: u32 = 10_000;
/// `PawnLocalAwareness.SightRadius` (squared).
const ANIMAL_SIGHT_RADIUS_SQ: i32 = 900;
/// `FoodUtility.GetMaxRegionsToScan` for wild animals.
const WILD_FOOD_REGIONS: usize = 30;

/// What an animal can eat where it lies: an item or a plant.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Food {
    Item(ItemId),
    Plant(ItemId),
}

impl Sim {
    /// `BiomeDef.CommonalityOfAnimal`: the biome's `wildAnimals` entry,
    /// else the race's `wildBiomes` entry for it.
    fn commonality_of_animal(&self, kind: DefId<PawnKindDef>) -> f32 {
        let Some(b) = self.wild.biome.map(|b| &self.defs.biomes[b]) else {
            return 0.0;
        };
        let k = &self.defs.pawn_kinds[kind];
        if let Some((_, c)) = b.wild_animals.iter().find(|(n, _)| *n == k.def_name) {
            return *c;
        }
        k.race
            .as_deref()
            .and_then(|r| self.defs.things.get(r))
            .and_then(|t| t.race.as_ref())
            .and_then(|r| r.wild_biomes.iter().find(|(n, _)| *n == b.def_name))
            .map_or(0.0, |(_, c)| *c)
    }

    /// `BiomeDef.AllWildAnimals`: pawn kinds in database order with a
    /// commonality.
    // COMPATIBILITY TODO: currently approximate — pollution and coastal
    // animal lists (Biotech, coastal tiles) are not used.
    fn all_wild_animals(&self) -> Vec<DefId<PawnKindDef>> {
        self.defs
            .pawn_kinds
            .iter()
            .map(|(id, _)| id)
            .filter(|&id| self.commonality_of_animal(id) > 0.0)
            .collect()
    }

    /// `MapTemperature.SeasonalTemp`: the biome's constant temperature, else
    /// the tile's average plus the season (no daily swing).
    fn seasonal_temperature(&self) -> f32 {
        if let Some(t) = self
            .wild
            .biome
            .and_then(|b| self.defs.biomes[b].constant_outdoor_temperature)
        {
            return t;
        }
        match &self.climate {
            Some(c) => {
                c.tile_temperature
                    + crate::climate::season_offset(
                        self.abs_tick().max(1),
                        crate::climate::seasonal_amplitude(self.latitude),
                    )
            }
            None => self.outdoor_temperature,
        }
    }

    /// `MapTemperature.SeasonAcceptableFor`: the seasonal temperature is
    /// strictly within the race's comfortable range.
    fn season_acceptable_for(&self, kind: DefId<PawnKindDef>) -> bool {
        let Some(race) = self.defs.pawn_kinds[kind]
            .race
            .as_deref()
            .and_then(|r| self.defs.things.get(r))
        else {
            return false;
        };
        let t = self.seasonal_temperature();
        let min = crate::stats::def_stat(&self.defs, race, None, "ComfyTemperatureMin");
        let max = crate::stats::def_stat(&self.defs, race, None, "ComfyTemperatureMax");
        t > min && t < max
    }

    /// `DesiredAnimalDensity`: the biome's animal density × the share of
    /// its animals' commonality that the season suits.
    // COMPATIBILITY TODO: currently approximate — game conditions' density
    // factors and pollution are not applied.
    pub fn desired_animal_density(&self) -> f32 {
        let Some(b) = self.wild.biome.map(|b| &self.defs.biomes[b]) else {
            return 0.0;
        };
        let (mut ok, mut all) = (0.0f32, 0.0f32);
        for kind in self.all_wild_animals() {
            let c = self.commonality_of_animal(kind);
            all += c;
            if self.season_acceptable_for(kind) {
                ok += c;
            }
        }
        if all <= 0.0 {
            return 0.0;
        }
        b.animal_density * (ok / all)
    }

    /// `DesiredTotalAnimalWeight`: the map's area / (10000 / density).
    fn desired_total_animal_weight(&self) -> f32 {
        let density = self.desired_animal_density();
        if density == 0.0 {
            return 0.0;
        }
        let size = self.map.size();
        (size.width * size.height) as f32 / (10_000.0 / density)
    }

    /// `CurrentTotalAnimalWeight`: the `ecoSystemWeight` of every spawned
    /// factionless pawn (our wild animals).
    fn current_total_animal_weight(&self) -> f32 {
        self.pawns
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !p.health.dead && p.carried_by.is_none() && self.is_wild_animal_index(*i)
            })
            .map(|(_, p)| self.defs.pawn_kinds[p.kind].eco_system_weight)
            .sum()
    }

    /// `AnimalEcosystemFull`.
    pub fn animal_ecosystem_full(&self) -> bool {
        self.current_total_animal_weight() >= self.desired_total_animal_weight()
    }

    /// `WildAnimalSpawnerTick`: every 1213 ticks, if the ecosystem has
    /// room, a Chance(0.027 × density) roll, then an entry cell on the map
    /// edge, then a random wild animal (group) spawns there.
    pub(super) fn wild_animal_spawner_tick(&mut self) {
        if !self.tick.is_multiple_of(ANIMAL_CHECK_INTERVAL) || self.wild.biome.is_none() {
            return;
        }
        if self.animal_ecosystem_full() {
            return;
        }
        let chance = BASE_ANIMAL_SPAWN_CHANCE * self.desired_animal_density();
        if !self.rng.chance(chance) {
            return;
        }
        if let Some(c) = self.random_animal_entry_cell() {
            self.spawn_random_wild_animal_at(c, true);
        }
    }

    /// `GenStep_Animals`: wild animals spawn at random cells until the
    /// ecosystem is full.
    pub fn generate_wild_animals(&mut self) -> u32 {
        let mut n = 0;
        let mut iterations = 0;
        while !self.animal_ecosystem_full() {
            iterations += 1;
            if iterations >= MAP_GEN_MAX_ITERATIONS {
                break;
            }
            let loc = self.random_animal_spawn_cell_map_gen();
            if !self.spawn_random_wild_animal_at(loc, false) {
                break;
            }
            n += 1;
        }
        n
    }

    /// The districts (our rooms: no door crossed) touching the map edge.
    fn edge_rooms(&self) -> Vec<usize> {
        let size = self.map.size();
        let mut out = Vec::new();
        let edge = (0..size.width)
            .flat_map(|x| [Cell::new(x, 0), Cell::new(x, size.height - 1)])
            .chain((0..size.height).flat_map(|z| [Cell::new(0, z), Cell::new(size.width - 1, z)]));
        for c in edge {
            if let Some(r) = self.regions.room_at(c)
                && !out.contains(&r)
            {
                out.push(r);
            }
        }
        out
    }

    fn view(&self) -> MapView<'_> {
        MapView {
            map: &self.map,
            defs: &self.defs,
            grid: &self.path_grid,
            regions: &self.regions,
        }
    }

    /// `CellFinder.RandomCell`: x then z.
    fn random_cell(&mut self) -> Cell {
        let size = self.map.size();
        let x = self.rng.range(0, size.width);
        let z = self.rng.range(0, size.height);
        Cell::new(x, z)
    }

    /// `RCellFinder.RandomAnimalSpawnCell_MapGen`: up to 1000 random cells
    /// standable, not avoided by wanderers, in a district touching the map
    /// edge; else any random cell.
    // COMPATIBILITY TODO: currently approximate — dangerous terrain is not
    // modelled (Core temperate maps have none).
    fn random_animal_spawn_cell_map_gen(&mut self) -> Cell {
        let edge_rooms = self.edge_rooms();
        for _ in 0..1000 {
            let c = self.random_cell();
            let ok = self.view().standable(c)
                && !self.defs.terrain[self.map.terrain[c]].avoid_wander
                && self
                    .regions
                    .room_at(c)
                    .is_some_and(|r| edge_rooms.contains(&r));
            if ok {
                return c;
            }
        }
        self.random_cell()
    }

    /// `RCellFinder.TryFindRandomPawnEntryCell` for animals (road chance
    /// 0, fogged allowed): `CellFinder.TryFindRandomEdgeCellWith` — 100
    /// random edge cells (`RandomEdgeCell`), then the shuffled edge — for a
    /// standable, unroofed cell whose district touches the edge and that
    /// reaches the colony and the map edge.
    // COMPATIBILITY TODO: currently approximate — dangerous terrain is not
    // modelled; "reaches the colony" is connection to any colonist (or
    // true without colonists); the game's shuffled edge list persists
    // between calls.
    fn random_animal_entry_cell(&mut self) -> Option<Cell> {
        let edge_rooms = self.edge_rooms();
        let colonists: Vec<Cell> = self
            .pawns
            .iter()
            .filter(|p| p.is_colonist && !p.health.dead && p.carried_by.is_none())
            .map(|p| p.position)
            .collect();
        let valid = |s: &Sim, c: Cell| {
            s.view().standable(c)
                && !s.map.roofed(c)
                && s.regions
                    .room_at(c)
                    .is_some_and(|r| edge_rooms.contains(&r))
                && (colonists.is_empty() || colonists.iter().any(|&p| s.regions.connected(c, p)))
        };
        let size = self.map.size();
        for _ in 0..100 {
            // `RandomEdgeCell`.
            let c = if self.rng.value() < 0.5 {
                let x = if self.rng.value() < 0.5 {
                    0
                } else {
                    size.width - 1
                };
                Cell::new(x, self.rng.range(0, size.height))
            } else {
                let z = if self.rng.value() < 0.5 {
                    0
                } else {
                    size.height - 1
                };
                Cell::new(self.rng.range(0, size.width), z)
            };
            if valid(self, c) {
                return Some(c);
            }
        }
        let mut edge: Vec<Cell> = (0..size.width)
            .flat_map(|x| [Cell::new(x, 0), Cell::new(x, size.height - 1)])
            .chain(
                (1..size.height - 1).flat_map(|z| [Cell::new(0, z), Cell::new(size.width - 1, z)]),
            )
            .collect();
        self.rng.shuffle(&mut edge);
        edge.into_iter().find(|&c| valid(self, c))
    }

    /// `WildAnimalSpawner.SpawnRandomWildAnimalAt`: a kind the season suits,
    /// by commonality / average group size (lazy weighted choice), then a
    /// group of `wildGroupSize` spawned near `loc` (within ceil(√max)),
    /// each with a scaria roll.
    // COMPATIBILITY TODO: currently approximate — pawn generation's random
    // draws (gender, age, traits, needs) are ours; scaria is rolled but not
    // given (it has no effect here yet); animals that fly in land without
    // the skyfaller; coastal and mutator factors are not applied.
    pub fn spawn_random_wild_animal_at(&mut self, loc: Cell, can_fly_in: bool) -> bool {
        let candidates: Vec<DefId<PawnKindDef>> = self
            .all_wild_animals()
            .into_iter()
            .filter(|&k| self.season_acceptable_for(k))
            .collect();
        let any_water = self.any_water_cells();
        let mut pick = crate::rand::Reservoir::new();
        for k in candidates {
            let race = self.defs.pawn_kinds[k]
                .race
                .as_deref()
                .and_then(|r| self.defs.things.get(r))
                .and_then(|t| t.race.as_ref());
            // `CommonalityOfAnimalNow`.
            let w = if race.is_some_and(|r| r.water_seeker) && !any_water {
                0.0
            } else {
                let (lo, hi) = self.defs.pawn_kinds[k].wild_group_size;
                self.commonality_of_animal(k) / ((lo + hi) as f32 / 2.0)
            };
            pick.offer(k, w, &mut self.rng);
        }
        let Some(kind) = pick.into_choice() else {
            return false;
        };
        let def = self.defs.pawn_kinds[kind].clone();
        let flies = can_fly_in
            && def
                .race
                .as_deref()
                .and_then(|r| self.defs.things.get(r))
                .and_then(|t| t.race.as_ref())
                .is_some_and(|r| r.can_fly_into_map);
        let mut loc = loc;
        if flies {
            // `CellFinderLoose.TryGetRandomCellWith`, 1000 tries.
            for _ in 0..1000 {
                let c = self.random_cell();
                if self.path_grid.walkable(c)
                    && !self.map.roofed(c)
                    && self.map.buildings[c].is_none()
                {
                    loc = c;
                    break;
                }
            }
        }
        let (lo, hi) = def.wild_group_size;
        let count = self.rng.range_inclusive(lo, hi);
        let radius = (hi.max(1) as f32).sqrt().ceil() as i32;
        let scaria = self
            .wild
            .biome
            .map_or(0.0, |b| self.defs.biomes[b].wild_animal_scaria_chance);
        for _ in 0..count {
            let view = MapView {
                map: &self.map,
                defs: &self.defs,
                grid: &self.path_grid,
                regions: &self.regions,
            };
            let at = crate::cell_finder::try_random_closewalk_cell_near(
                &view,
                loc,
                radius,
                |_| true,
                &mut self.rng,
            )
            .unwrap_or(loc);
            let id = self.spawn_pawn(kind, def.label.clone(), at).ok();
            let _ = self.rng.chance(scaria);
            if id.is_none() {
                continue;
            }
        }
        true
    }

    /// `TerrainGrid.AnyWaterCells`: some cell's terrain is tagged Water.
    fn any_water_cells(&self) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        for c in self.map.size().cells() {
            seen.insert(self.map.terrain[c]);
        }
        seen.into_iter().any(|t| {
            self.defs
                .raw
                .get("TerrainDef", &self.defs.terrain[t].def_name)
                .is_some_and(|d| d.node.child_list_texts("tags").iter().any(|g| g == "Water"))
        })
    }

    /// A wild animal (factionless).
    pub(super) fn is_wild_animal_index(&self, i: usize) -> bool {
        self.is_animal_index(i) && !self.pawns[i].is_colonist
    }

    // ---- Grazing ----------------------------------------------------

    /// `PawnLocalAwareness.AnimalAwareOf` for a wild animal: within 30
    /// cells, in the same room, in sight.
    fn animal_aware_of(&self, i: usize, c: Cell) -> bool {
        let at = self.pawns[i].position;
        let (dx, dz) = (at.x - c.x, at.z - c.z);
        dx * dx + dz * dz <= ANIMAL_SIGHT_RADIUS_SQ
            && self.regions.room_at(at) == self.regions.room_at(c)
            && crate::ranged::line_of_sight(&super::ranged::SimShotMap(self), at, c, false)
    }

    /// The food an item or plant gives (`IsNutritionGivingIngestible`),
    /// its preferability and whether it can be eaten now (`IngestibleNow`).
    fn food_facts(&self, food: Food) -> Option<(f32, FoodPreferability, bool, bool)> {
        match food {
            Food::Item(id) if self.corpse_pawn(id).is_some() => {
                // `Corpse.IngestibleNow`: fresh flesh only.
                let it = self.map.item(id)?;
                let ing = self.defs.things[it.def].ingestible.as_ref()?;
                let fresh = self
                    .rot_stage(id)
                    .is_none_or(|s| s == crate::sim::RotStage::Fresh);
                let flesh = ing.preferability != FoodPreferability::NeverForNutrition;
                Some((
                    self.corpse_nutrition(id),
                    ing.preferability,
                    fresh && flesh,
                    fresh,
                ))
            }
            Food::Item(id) => {
                let it = self.map.item(id)?;
                let def = &self.defs.things[it.def];
                let ing = def.ingestible.as_ref()?;
                let fresh = self
                    .rot_stage(id)
                    .is_none_or(|s| s == crate::sim::RotStage::Fresh);
                let dessicated = self.rot_stage(id) == Some(crate::sim::RotStage::Dessicated);
                Some((
                    crate::food::unit_nutrition(&self.defs, def),
                    ing.preferability,
                    !dessicated,
                    fresh,
                ))
            }
            Food::Plant(id) => {
                let p = self.map.plant(id)?;
                let def = &self.defs.things[p.def];
                let ing = def.ingestible.as_ref()?;
                let props = def.plant.as_ref()?;
                let tree = def.passability != rimworld_defs::Passability::Standable;
                let now = tree
                    || (p.growth >= props.harvest_min_growth
                        && p.growth >= 0.1
                        && !p.leafless_now(self.tick as i64));
                Some((
                    crate::stats::def_stat(&self.defs, def, None, "Nutrition"),
                    ing.preferability,
                    now,
                    true,
                ))
            }
        }
    }

    fn food_def(&self, food: Food) -> Option<&rimworld_defs::ThingDef> {
        let def = match food {
            Food::Item(id) => self.map.item(id)?.def,
            Food::Plant(id) => self.map.plant(id)?.def,
        };
        Some(&self.defs.things[def])
    }

    fn food_cell(&self, food: Food) -> Option<Cell> {
        match food {
            Food::Item(id) => self.map.item(id).map(|it| it.position),
            Food::Plant(id) => self.map.plant(id).map(|p| p.position),
        }
    }

    /// `FoodUtility.BestFoodSourceOnMap` for a wild animal eating for
    /// itself: breadth-first over at most 30 regions from its own, the
    /// nearest item or plant (plants only if it eats plants or trees) on
    /// each region's cells that it will eat, gives nutrition, can be eaten
    /// now, is not dessicated, that it is aware of and can reserve; on the
    /// first pass also fresh, better than DesperateOnly and not being eaten
    /// by another animal within 2 cells. With nothing found, a desperate
    /// pass without those three checks.
    // COMPATIBILITY TODO: currently approximate — corpses aren't edible
    // and predators don't hunt (`BestPawnToHuntForPredator`); a region's
    // things are taken in list order; socially-proper checks are skipped.
    fn animal_food_source(&self, i: usize, whole_map: bool) -> Option<Food> {
        let p = &self.pawns[i];
        let race = self.defs.things[p.race].race.as_ref()?;
        let eats_plants =
            race.eats(rimworld_defs::food_type::PLANT | rimworld_defs::food_type::TREE);
        let starving = p
            .needs
            .get(crate::needs::NeedKind::Food)
            .is_some_and(|f| f.level <= 0.0);
        let root = self.regions.region_at(p.position)?;
        let claimant = self.claimant(i);
        // Things other animals nearby are eating.
        let eaten: Vec<ItemId> = self
            .pawns
            .iter()
            .enumerate()
            .filter(|(k, o)| {
                *k != i
                    && self.is_animal_index(*k)
                    && (o.position.x - p.position.x).abs() <= 2
                    && (o.position.z - p.position.z).abs() <= 2
                    && {
                        let (dx, dz) = (o.position.x - p.position.x, o.position.z - p.position.z);
                        dx * dx + dz * dz <= 4
                    }
            })
            .filter_map(|(_, o)| match o.job.as_ref().map(|j| j.kind) {
                Some(JobKind::IngestInPlace { food, .. }) => Some(food),
                _ => None,
            })
            .collect();
        // A region's things: the items and plants on its cells.
        let region_food = |r: usize| -> Vec<Food> {
            let e = self.regions.region(r).extents;
            let mut out = Vec::new();
            for z in e.min_z..=e.max_z {
                for x in e.min_x..=e.max_x {
                    let c = Cell::new(x, z);
                    if self.regions.region_at(c) != Some(r) {
                        continue;
                    }
                    for it in self.map.items_at(c) {
                        if !it.is_filth() {
                            out.push(Food::Item(it.id));
                        }
                    }
                    if eats_plants && let Some(pl) = self.map.plant_at(c) {
                        out.push(Food::Plant(pl.id));
                    }
                }
            }
            out
        };
        let valid = |food: Food, desperate: bool| -> bool {
            let Some(def) = self.food_def(food) else {
                return false;
            };
            let Some((nutrition, pref, now, fresh)) = self.food_facts(food) else {
                return false;
            };
            let (id, stack) = match food {
                Food::Item(id) => (id, self.map.item(id).map_or(1, |it| it.stack_count as i32)),
                Food::Plant(id) => (id, 1),
            };
            let Some(cell) = self.food_cell(food) else {
                return false;
            };
            if pref < FoodPreferability::NeverForNutrition
                || !race.can_ever_eat(def)
                || nutrition <= 0.0
                || !now
                || (!desperate && !fresh)
                || (!whole_map && !self.animal_aware_of(i, cell))
                || !self
                    .reservations
                    .can_reserve(claimant, Target::Item(id), stack, 10, 1)
            {
                return false;
            }
            desperate || (!eaten.contains(&id) && pref > FoodPreferability::DesperateOnly)
        };
        let search = |desperate: bool| -> Option<Food> {
            let mut best: Option<(Food, i32)> = None;
            self.regions.traverse(
                root,
                |_, r| self.regions.region(r).kind.passable(),
                |r| {
                    for f in region_food(r) {
                        let Some(c) = self.food_cell(f) else {
                            continue;
                        };
                        let (dx, dz) = (c.x - p.position.x, c.z - p.position.z);
                        let d = dx * dx + dz * dz;
                        if best.is_none_or(|(_, bd)| d < bd) && valid(f, desperate) {
                            best = Some((f, d));
                        }
                    }
                    best.is_some()
                },
                if whole_map {
                    usize::MAX
                } else {
                    WILD_FOOD_REGIONS
                },
            );
            best.map(|(f, _)| f)
        };
        search(starving).or_else(|| search(true))
    }

    /// `JobGiver_GetFood` for a wild animal: the Ingest job on its food;
    /// with none on the map, a predator hunts live prey (`PredatorHunt`).
    /// `whole_map`: `forceScanWholeMap` (the starving branch).
    // COMPATIBILITY TODO: currently approximate — the whole-map prey search
    // scans the same 30 regions.
    pub(super) fn animal_food_job(&self, i: usize, whole_map: bool) -> Option<Job> {
        if !self.is_wild_animal_index(i) {
            return None;
        }
        // Only when hungry enough for the food giver (`GetPriority`).
        let need = self.pawns[i].needs.get(crate::needs::NeedKind::Food)?;
        if !whole_map && need.percent() >= need.want_eat {
            return None;
        }
        let Some(food) = self.animal_food_source(i, whole_map) else {
            let predator = self.defs.things[self.pawns[i].race]
                .race
                .as_ref()
                .is_some_and(|r| r.predator);
            if !predator {
                return None;
            }
            let t = self.best_prey_for(i)?;
            return Some(self.predator_hunt_job(self.pawns[t].id));
        };
        let (id, count) = match food {
            Food::Plant(id) => (id, 1),
            Food::Item(id) if self.corpse_pawn(id).is_some() => (id, 1),
            Food::Item(id) => {
                let def = self.food_def(food)?;
                let need = self.pawns[i].needs.get(crate::needs::NeedKind::Food)?;
                let max_at_once = def.ingestible.as_ref()?.max_num_to_ingest_at_once;
                (
                    id,
                    crate::food::will_ingest_stack_count(
                        need.max - need.level,
                        crate::food::unit_nutrition(&self.defs, def),
                        max_at_once,
                    ),
                )
            }
        };
        let dest = self.food_cell(food)?;
        Some(Job {
            def: self.job_defs.ingest,
            kind: JobKind::IngestInPlace {
                food: id,
                count,
                stage: IngestStage::GotoFood { dest },
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        })
    }

    fn in_place_food(&self, id: ItemId) -> Option<Food> {
        if self.map.item(id).is_some() {
            Some(Food::Item(id))
        } else if self.map.plant(id).is_some() {
            Some(Food::Plant(id))
        } else {
            None
        }
    }

    /// The non-tool-user ingest job starts: walk to touch the food.
    pub(super) fn begin_ingest_in_place(&mut self, i: usize) -> bool {
        let Some(JobKind::IngestInPlace { food, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(cell) = self.in_place_food(food).and_then(|f| self.food_cell(f)) else {
            return false;
        };
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => {
                self.ingest_in_place_arrived(i);
                true
            }
            super::farming::Touch::Walking => true,
            super::farming::Touch::NoPath => false,
        }
    }

    /// At the food: chewing starts (`ChewIngestible`: base ingest ticks
    /// / eating speed).
    pub(super) fn ingest_in_place_arrived(&mut self, i: usize) {
        let Some(JobKind::IngestInPlace { food, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(f) = self.in_place_food(food) else {
            self.end_job(i, false);
            return;
        };
        if !self.food_facts(f).is_some_and(|(_, _, now, _)| now) {
            self.end_job(i, false);
            return;
        }
        let eating_factor = self.capacity_factor(i, "EatingSpeed");
        let speed = self.pawns[i].eating_speed * eating_factor;
        let ticks = self
            .food_def(f)
            .and_then(|d| d.ingestible.as_ref())
            .map_or(0, |ing| {
                crate::food::chew_ticks(ing.base_ingest_ticks, speed, ing.use_eating_speed_stat)
            });
        // The chew toil starts on arrival and is paid for in this tick.
        let left = ticks - 1;
        if let Some(Job {
            kind: JobKind::IngestInPlace { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = IngestStage::Chew { ticks_left: left };
        }
        if left <= 0 {
            self.ingest_in_place_finish(i);
        }
    }

    /// Chewing finished (`FinalizeIngest`): a plant gives growth × its
    /// nutrition up to what is wanted (dying if eaten whole, else losing
    /// the growth eaten); an item gives whole units.
    pub(super) fn ingest_in_place_finish(&mut self, i: usize) {
        let Some(JobKind::IngestInPlace { food, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(need) = self.pawns[i]
            .needs
            .get(crate::needs::NeedKind::Food)
            .cloned()
        else {
            self.end_job(i, false);
            return;
        };
        let wanted = need.max - need.level;
        let gained = match self.in_place_food(food) {
            Some(Food::Plant(id)) => {
                let (def, growth) = {
                    let p = self.map.plant(id).expect("a plant");
                    (p.def, p.growth)
                };
                let nutrition =
                    crate::stats::def_stat(&self.defs, &self.defs.things[def], None, "Nutrition");
                let whole = growth * nutrition;
                let eaten = wanted.min(whole);
                if eaten >= whole {
                    self.map.remove_plant(id);
                    self.reservations.release_all_for_target(Target::Item(id));
                    self.refresh_path_grid();
                } else if let Some(p) = self.map.plant_mut(id) {
                    p.growth -= eaten / nutrition;
                    self.map.plant_revision += 1;
                }
                eaten
            }
            Some(Food::Item(id)) if self.corpse_pawn(id).is_some() => self.eat_corpse(id, wanted),
            Some(Food::Item(id)) => {
                let (def, stack) = {
                    let it = self.map.item(id).expect("an item");
                    (it.def, it.stack_count)
                };
                let d = &self.defs.things[def];
                let unit = crate::food::unit_nutrition(&self.defs, d);
                let max_at_once = d
                    .ingestible
                    .as_ref()
                    .map_or(0, |g| g.max_num_to_ingest_at_once);
                let n = crate::food::ingested_count(wanted, unit, stack, max_at_once);
                self.map.take_from_item(id, n);
                self.refresh_path_grid();
                n as f32 * unit
            }
            None => {
                self.end_job(i, false);
                return;
            }
        };
        if let Some(f) = self.pawns[i].needs.get_mut(crate::needs::NeedKind::Food) {
            f.level = (f.level + gained).clamp(0.0, f.max);
        }
        self.end_job(i, true);
    }
}

/// What the animal leave branches read.
pub(super) struct LeaveFacts {
    pub wrong_season: bool,
    pub dangerous_temperature: bool,
    pub outdoor: bool,
    pub can_reach_map_edge: bool,
}

impl Sim {
    /// The facts the wild leave branches check (`LeaveIfWrongSeason`,
    /// `LeaveIfStarving`).
    pub(super) fn leave_facts(&mut self, i: usize) -> LeaveFacts {
        if !self.is_wild_animal_index(i) {
            return LeaveFacts {
                wrong_season: false,
                dangerous_temperature: false,
                outdoor: true,
                can_reach_map_edge: true,
            };
        }
        let temp = self.temperature_facts(i);
        // `SafeTemperatureRange`: the comfortable range widened by 10 on
        // both ends, ends included.
        let (lo, hi) = (temp.comfy.0 - 10.0, temp.comfy.1 + 10.0);
        let at = self.pawns[i].position;
        let outdoor = match self.room_at(at) {
            Some(r) => self.room_uses_outdoor(r),
            None => true,
        };
        let size = self.map.size();
        let can_reach_map_edge = (0..size.width)
            .flat_map(|x| [Cell::new(x, 0), Cell::new(x, size.height - 1)])
            .chain((0..size.height).flat_map(|z| [Cell::new(0, z), Cell::new(size.width - 1, z)]))
            .any(|c| self.path_grid.walkable(c) && (c == at || self.regions.connected(at, c)));
        LeaveFacts {
            wrong_season: !self.season_acceptable_for(self.pawns[i].kind),
            dangerous_temperature: !(lo <= temp.ambient && temp.ambient <= hi),
            outdoor,
            can_reach_map_edge,
        }
    }

    /// The exit job starts: walk to the edge cell (OnCell).
    pub(super) fn begin_exit_map(&mut self, i: usize) -> bool {
        let Some(JobKind::ExitMap { dest }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        if self.pawns[i].next_stop() == dest {
            self.exit_map_arrived(i);
            return true;
        }
        self.walk_to(i, dest, false)
    }

    /// `JobDriver_Goto`'s end with `exitMapOnArrival`: on the map edge the
    /// pawn leaves (`Pawn.ExitMap`, after this tick's pawns), else the job
    /// just ends.
    pub(super) fn exit_map_arrived(&mut self, i: usize) {
        let at = self.pawns[i].position;
        let size = self.map.size();
        let on_edge = at.x == 0 || at.z == 0 || at.x == size.width - 1 || at.z == size.height - 1;
        if on_edge {
            let id = self.pawns[i].id;
            if !self.pending_exits.contains(&id) {
                self.pending_exits.push(id);
            }
        } else {
            self.end_job(i, true);
        }
    }

    /// `Pawn.ExitMap` for pawns that reached the edge: the job and its
    /// claims end, marks on it go, and it leaves the simulation (no corpse,
    /// nothing dropped; the game keeps it as a world pawn).
    // COMPATIBILITY TODO: currently approximate — world pawns don't exist:
    // a departed animal is gone for good.
    pub(super) fn process_exits(&mut self) {
        for id in std::mem::take(&mut self.pending_exits) {
            let Some(i) = self.index_of(id) else {
                continue;
            };
            self.cleanup_job(i);
            self.pawns[i].job = None;
            self.reservations.release_all_claimed_by(id);
            self.destinations.release_all_claimed_by(id);
            self.map.hunt_designations.retain(|&p| p != id);
            self.pawns.remove(i);
        }
    }

    /// Debug: the food job a wild animal would take now (`GetFood`).
    pub fn debug_animal_food_job(&self, pawn: crate::pawn::PawnId) -> Option<JobKind> {
        let i = self.index_of(pawn)?;
        self.animal_food_job(i, false).map(|j| j.kind)
    }

    /// The ecosystem weight of the spawned wild animals.
    pub fn wild_animal_weight(&self) -> f32 {
        self.current_total_animal_weight()
    }

    /// Debug: makes a pawn leave the map now.
    pub fn debug_exit_map(&mut self, pawn: crate::pawn::PawnId) {
        self.pending_exits.push(pawn);
        self.process_exits();
    }
}

/// The exit job's tick: walking to the edge cell.
pub(super) fn tick_exit_map(
    pawn: &mut crate::pawn::Pawn,
    map: &crate::map::Map,
    grid: &crate::path::PathGrid,
    t: u64,
) -> Option<super::JobEvent> {
    let Some(JobKind::ExitMap { .. }) = pawn.job.as_ref().map(|j| j.kind) else {
        return None;
    };
    super::tick_movement(pawn, grid, map, t);
    Some(if pawn.is_moving() {
        super::JobEvent::None
    } else {
        super::JobEvent::ArrivedAtExit
    })
}

/// The in-place ingest job's tick: walking, then chewing.
pub(super) fn tick_ingest_in_place(
    pawn: &mut crate::pawn::Pawn,
    map: &crate::map::Map,
    grid: &crate::path::PathGrid,
    t: u64,
) -> Option<super::JobEvent> {
    let Some(JobKind::IngestInPlace { food, stage, .. }) = pawn.job.as_ref().map(|j| j.kind) else {
        return None;
    };
    if map.item(food).is_none() && map.plant(food).is_none() {
        return Some(super::JobEvent::Failed);
    }
    Some(match stage {
        IngestStage::GotoFood { .. } | IngestStage::CarryToChewSpot { .. } => {
            super::tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                super::JobEvent::None
            } else {
                super::JobEvent::ArrivedAtFoodInPlace
            }
        }
        IngestStage::Chew { ticks_left } => {
            let left = ticks_left - 1;
            if let Some(Job {
                kind: JobKind::IngestInPlace { stage, .. },
                ..
            }) = &mut pawn.job
            {
                *stage = IngestStage::Chew { ticks_left: left };
            }
            if left <= 0 {
                super::JobEvent::DoneChewingInPlace
            } else {
                super::JobEvent::None
            }
        }
    })
}
