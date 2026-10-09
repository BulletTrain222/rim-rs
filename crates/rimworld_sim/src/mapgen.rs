//! A simple generated test map built from real Defs.
//!
//! This is NOT RimWorld's map generator (that is driven by `MapGeneratorDef` /
//! `GenStepDef` and comes later). It only arranges real TerrainDefs and rock
//! ThingDefs with value noise so the prototype has something to walk on.

use rimworld_defs::{DefId, GameDefs, TerrainDef, ThingDef};

use crate::grid::{Cell, GridSize};
use crate::map::Map;

#[derive(Debug, thiserror::Error)]
pub enum MapGenError {
    #[error("no TerrainDefs loaded")]
    NoTerrain,
}

/// Defs used by the test map. Missing ones fall back to `base`.
#[derive(Debug, Clone)]
pub struct TestMapPalette {
    pub base: DefId<TerrainDef>,
    pub rich: DefId<TerrainDef>,
    pub gravel: DefId<TerrainDef>,
    pub sand: DefId<TerrainDef>,
    pub marsh: DefId<TerrainDef>,
    pub shallow_water: DefId<TerrainDef>,
    pub deep_water: DefId<TerrainDef>,
    pub rock: Option<DefId<ThingDef>>,
    /// defNames that were requested but not found.
    pub missing: Vec<&'static str>,
}

impl TestMapPalette {
    pub fn from_defs(defs: &GameDefs) -> Result<Self, MapGenError> {
        let mut missing = Vec::new();
        let base = defs
            .terrain
            .id("Soil")
            .or_else(|| {
                defs.terrain
                    .iter()
                    .find(|(_, t)| t.is_walkable())
                    .map(|(id, _)| id)
            })
            .ok_or(MapGenError::NoTerrain)?;
        let mut t = |name: &'static str| {
            defs.terrain.id(name).unwrap_or_else(|| {
                missing.push(name);
                base
            })
        };
        let rich = t("SoilRich");
        let gravel = t("Gravel");
        let sand = t("Sand");
        let marsh = t("MarshyTerrain");
        let shallow_water = t("WaterShallow");
        let deep_water = t("WaterDeep");
        let rock = defs.things.id("Granite");
        if rock.is_none() {
            missing.push("Granite");
        }
        Ok(Self {
            base,
            rich,
            gravel,
            sand,
            marsh,
            shallow_water,
            deep_water,
            rock,
            missing,
        })
    }
}

/// Generates a `size` map deterministically from `seed`.
pub fn generate_test_map(size: GridSize, seed: u64, palette: &TestMapPalette) -> Map {
    let mut map = Map::new(size, palette.base);
    let elevation = ValueNoise::new(seed, 14.0);
    let moisture = ValueNoise::new(seed ^ 0x9E37_79B9_7F4A_7C15, 18.0);
    let detail = ValueNoise::new(seed.wrapping_add(7), 5.0);
    let center = Cell::new(size.width / 2, size.height / 2);

    for c in size.cells() {
        // Keep the middle of the map open so the pawn has room to start.
        let d = c.chebyshev(center) as f32 / (size.width.min(size.height) as f32 / 2.0);
        let open_bias = (1.0 - d).max(0.0) * 0.35;
        let e = elevation.sample(c) - open_bias;
        let m = moisture.sample(c);
        let fine = detail.sample(c);

        if e > 0.68 {
            map.terrain[c] = palette.gravel;
            map.buildings[c] = palette.rock;
        } else if e > 0.62 {
            map.terrain[c] = palette.gravel;
        } else if m > 0.80 {
            map.terrain[c] = palette.deep_water;
        } else if m > 0.72 {
            map.terrain[c] = palette.shallow_water;
        } else if m > 0.66 {
            map.terrain[c] = palette.marsh;
        } else if m < 0.22 {
            map.terrain[c] = palette.sand;
        } else if fine > 0.7 {
            map.terrain[c] = palette.rich;
        }
    }
    map
}

/// Smooth 2D value noise in 0..1, from a hashed lattice.
struct ValueNoise {
    seed: u64,
    scale: f32,
}

impl ValueNoise {
    fn new(seed: u64, scale: f32) -> Self {
        Self { seed, scale }
    }

    fn lattice(&self, x: i32, z: i32) -> f32 {
        let mut h = self.seed
            ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        // splitmix64 finaliser
        h ^= h >> 30;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 27;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 31;
        (h >> 40) as f32 / (1u64 << 24) as f32
    }

    fn sample(&self, c: Cell) -> f32 {
        let fx = c.x as f32 / self.scale;
        let fz = c.z as f32 / self.scale;
        let (x0, z0) = (fx.floor() as i32, fz.floor() as i32);
        let (tx, tz) = (smooth(fx - x0 as f32), smooth(fz - z0 as f32));
        let a = self.lattice(x0, z0);
        let b = self.lattice(x0 + 1, z0);
        let c2 = self.lattice(x0, z0 + 1);
        let d = self.lattice(x0 + 1, z0 + 1);
        let top = a + (b - a) * tx;
        let bottom = c2 + (d - c2) * tx;
        top + (bottom - top) * tz
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Dresses a generated map with a biome (after `generate_test_map`):
/// fertile ground follows the biome's `terrainsByFertility`, and wild
/// plants are spread as `GenStep_Plants` does — every cell in random
/// order, skipping 0.1%, spawning where the desired plant count allows,
/// choosing among the biome's plants that can grow there by commonality,
/// with random initial growth (0.15–1.5, clamped) and age.
// COMPATIBILITY TODO: currently approximate — the game's fertility noise,
// region saturation, plant clusters, lower-order plant checks, cave plants
// and the map's random cell order are not reproduced; the RNG stream is
// seeded from the map seed only.
pub fn apply_biome(map: &mut Map, defs: &GameDefs, biome: &rimworld_defs::BiomeDef, seed: u64) {
    let fertility_noise = ValueNoise::new(seed.wrapping_mul(31).wrapping_add(17), 9.0);
    let size = map.size();
    for c in size.cells() {
        let t = &defs.terrain[map.terrain[c]];
        // Only plain fertile ground is re-terrained (not water, sand, rock).
        if t.fertility < 0.5 || map.buildings[c].is_some() {
            continue;
        }
        let v = fertility_noise.sample(c) * 1.3 - 0.15;
        if let Some(id) = biome
            .terrain_at_fertility(v)
            .and_then(|n| defs.terrain.id(n))
        {
            map.terrain[c] = id;
        }
    }
    spawn_biome_plants(map, defs, biome, seed);
}

/// The plant half of [`apply_biome`]: wild plants spread over the map's
/// own terrain.
pub fn spawn_biome_plants(
    map: &mut Map,
    defs: &GameDefs,
    biome: &rimworld_defs::BiomeDef,
    seed: u64,
) {
    let size = map.size();
    let mut rng = crate::rand::Rand::new(seed as u32 ^ 578_415_222);
    let mut cells: Vec<Cell> = size.cells().collect();
    rng.shuffle(&mut cells);
    let plants: Vec<(rimworld_defs::DefId<rimworld_defs::ThingDef>, f32)> = biome
        .wild_plants
        .iter()
        .filter_map(|(n, w)| Some((defs.things.id(n)?, *w)))
        .filter(|(id, _)| defs.things[*id].plant.is_some())
        .collect();
    for c in cells {
        if rng.chance(0.001) {
            continue;
        }
        spawn_wild_plant(map, defs, biome, &plants, c, true, &mut rng);
    }
}

/// `WildPlantSpawner.CheckSpawnWildPlantAt` (simplified): returns whether
/// a plant was spawned.
pub fn spawn_wild_plant(
    map: &mut Map,
    defs: &GameDefs,
    biome: &rimworld_defs::BiomeDef,
    plants: &[(rimworld_defs::DefId<rimworld_defs::ThingDef>, f32)],
    c: Cell,
    random_growth: bool,
    rng: &mut crate::rand::Rand,
) -> bool {
    let fertility = defs.terrain[map.terrain[c]].fertility;
    if biome.plant_density <= 0.0
        || map.plant_at(c).is_some()
        || map.buildings[c].is_some()
        || fertility <= 0.0
    {
        return false;
    }
    // `GetDesiredPlantsCountAt`: fertility × (density × fertility), at most 1.
    let desired = (fertility * biome.plant_density * fertility).min(1.0);
    if !rng.chance(desired) {
        return false;
    }
    let candidates: Vec<(rimworld_defs::DefId<rimworld_defs::ThingDef>, f32)> = plants
        .iter()
        .copied()
        .filter(|(id, _)| {
            defs.things[*id]
                .plant
                .as_ref()
                .is_some_and(|p| fertility >= p.fertility_min)
        })
        .collect();
    let weights: Vec<f32> = candidates.iter().map(|(_, w)| *w).collect();
    let Some(k) = crate::region::random_element_by_weight(&weights, rng) else {
        return false;
    };
    let def = candidates[k].0;
    let d = &defs.things[def];
    let max_hp = d.stat("MaxHitPoints").unwrap_or(100.0);
    let id = map.spawn_plant(def, c, 0.0, max_hp);
    if random_growth {
        let growth = rng.range_f32(0.15, 1.5).clamp(0.0, 1.0);
        let lifespan = d.plant.as_ref().map_or(0, |p| p.lifespan_ticks());
        let limited = d.plant.as_ref().is_some_and(|p| p.limited_lifespan());
        let age = if limited {
            rng.range(0, (lifespan - 50).max(0) as i32) as i64
        } else {
            0
        };
        if let Some(p) = map.plant_mut(id) {
            p.growth = growth;
            p.age = age;
        }
    } else if let Some(p) = map.plant_mut(id) {
        // A new plant's default growth (`growthInt` = 0.15).
        p.growth = 0.15;
    }
    true
}
