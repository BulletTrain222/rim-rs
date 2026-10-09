//! The game's map generation for a bounded slice (docs/research.md §64):
//! seeds (`MapGenerator`), `RockNoises`, `GenStep_ElevationFertility`,
//! `GenStep_RocksFromGrid` (solid rock, natural roofs, inline
//! `GenStep_ScatterLumpsMineable`) and `GenStep_Terrain`.
//!
//! Supported: a surface tile with no water cover, rivers, coast, caves,
//! mutators or pollution, for any hilliness and the biome's own terrain
//! patch makers. Everything else of the game's map generator (plants,
//! ruins, geysers, the player's start, fog) is outside this module.

use rimworld_defs::{BiomeDef, DefId, GameDefs, TerrainDef, ThingDef};

use crate::grid::{Cell, GridSize};
use crate::noise::{Displace, Module, Perlin, Quality, RotateY, ScaleBias, StretchX, Times};
use crate::rand::Rand;

/// `GenStep_ElevationFertility.SeedPart`.
const SEED_PART_ELEVATION: i32 = 826_504_671;
/// `GenStep_RocksFromGrid.SeedPart`.
const SEED_PART_ROCKS: i32 = 1_182_952_823;
/// `GenStep_Terrain.SeedPart`.
const SEED_PART_TERRAIN: i32 = 262_606_459;
/// Solid rock above this elevation (strictly).
const ROCK_ELEVATION: f32 = 0.7;
/// Natural roof groups smaller than this are removed.
const MIN_ROOFED_CELLS_PER_GROUP: usize = 20;

/// `GenAdj.CardinalDirections` (N, E, S, W).
const CARDINALS: [Cell; 4] = [
    Cell::new(0, 1),
    Cell::new(1, 0),
    Cell::new(0, -1),
    Cell::new(-1, 0),
];

/// Builds a map from the generated layers: terrain, natural rock and ore
/// walls (with their stuffless rock defs) and natural roofs.
pub fn to_map(g: &Generated) -> crate::map::Map {
    let mut map = crate::map::Map::new(g.size, g.terrain[0]);
    for z in 0..g.size.height {
        for x in 0..g.size.width {
            let c = Cell::new(x, z);
            let k = g.index(c);
            map.terrain[c] = g.terrain[k];
            map.buildings[c] = g.rock[k];
            if let Some(r) = g.roof[k] {
                map.set_natural_roof(c, Some(r));
            }
        }
    }
    map
}

/// A tile's hilliness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hilliness {
    Flat,
    SmallHills,
    LargeHills,
    Mountainous,
    Impassable,
}

impl Hilliness {
    /// `MapGenTuning.ElevationFactor*`.
    fn elevation_factor(self) -> f32 {
        match self {
            Hilliness::Flat => 0.8,
            Hilliness::SmallHills => 0.9,
            Hilliness::LargeHills => 1.0,
            Hilliness::Mountainous => 1.1,
            Hilliness::Impassable => 1.2,
        }
    }

    /// `GetResourceBlotchesPer10KCellsForMap`.
    fn ore_blotches_per_10k(self) -> f32 {
        match self {
            Hilliness::Flat => 4.0,
            Hilliness::SmallHills => 8.0,
            Hilliness::LargeHills => 11.0,
            Hilliness::Mountainous => 15.0,
            Hilliness::Impassable => 16.0,
        }
    }
}

/// Natural roof over a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NaturalRoof {
    Thin,
    Thick,
}

/// What generation needs to know about the world tile.
#[derive(Debug, Clone)]
pub struct TileInput {
    /// The world's seed string (`WorldInfo.seedString`).
    pub world_seed: String,
    /// The surface tile id.
    pub tile: i32,
    pub size: GridSize,
    pub hilliness: Hilliness,
    pub biome: DefId<BiomeDef>,
    /// The starting map (no nomadic resource factor).
    pub starting_map: bool,
    /// The rock types to use instead of the biome/tile's (a test input,
    /// like a biome's `forceRockTypes`).
    pub forced_rocks: Option<Vec<DefId<ThingDef>>>,
    /// `overrideBlotchesPer10kCells` (Some(0): no ore).
    pub ore_blotches_override: Option<f32>,
}

/// The generated layers.
#[derive(Debug, Clone)]
pub struct Generated {
    pub size: GridSize,
    /// z-major, x-minor.
    pub elevation: Vec<f32>,
    pub fertility: Vec<f32>,
    pub rock_types: Vec<DefId<ThingDef>>,
    /// `RockNoises` seeds, per rock type.
    pub rock_noise_seeds: Vec<i32>,
    /// The natural rock or ore on each cell.
    pub rock: Vec<Option<DefId<ThingDef>>>,
    pub roof: Vec<Option<NaturalRoof>>,
    pub terrain: Vec<DefId<TerrainDef>>,
    /// Rand (seed, counter) at the end of each step: elevation, rocks,
    /// terrain.
    pub step_streams: [(u32, u32); 3],
}

impl Generated {
    pub fn index(&self, c: Cell) -> usize {
        (c.z * self.size.width + c.x) as usize
    }
}

/// `GenText.StableStringHash`: 23, then × 31 + each UTF-16 unit.
pub fn stable_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(23i32, |h, u| h.wrapping_mul(31).wrapping_add(u as i32))
}

/// `Gen.HashCombineInt(seed, value)`.
pub fn hash_combine(seed: i32, value: i32) -> i32 {
    seed ^ value
        .wrapping_add(0x9E37_79B9u32 as i32)
        .wrapping_add(seed.wrapping_shl(6))
        .wrapping_add(seed >> 2)
}

/// The map seed: the world seed combined with the tile's hash (id × 397).
pub fn map_seed(world_seed: &str, tile: i32) -> i32 {
    hash_combine(stable_string_hash(world_seed), tile.wrapping_mul(397))
}

fn raw_text<'a>(defs: &'a GameDefs, kind: &str, name: &str, field: &str) -> Option<&'a str> {
    defs.raw
        .get(kind, name)?
        .node
        .child_text(field)
        .map(str::trim)
}

fn raw_bool(defs: &GameDefs, kind: &str, name: &str, field: &str) -> Option<bool> {
    raw_text(defs, kind, name, field).map(|v| v.eq_ignore_ascii_case("true"))
}

fn raw_list(defs: &GameDefs, kind: &str, name: &str, field: &str) -> Option<Vec<String>> {
    let node = defs.raw.get(kind, name)?.node.child(field)?;
    Some(
        node.children
            .iter()
            .filter_map(|c| c.text.as_deref().map(|t| t.trim().to_owned()))
            .collect(),
    )
}

/// `World.NaturalRockTypesIn`: the biome's `forceRockTypes`, else, on a
/// stream seeded by the tile hash, 2–3 distinct non-resource natural rocks
/// (database order, biome-specific ones only if the biome lists them) in
/// random order.
pub fn natural_rock_types(
    defs: &GameDefs,
    biome: DefId<BiomeDef>,
    tile: i32,
) -> Vec<DefId<ThingDef>> {
    let biome_name = &defs.biomes[biome].def_name;
    if let Some(forced) = raw_list(defs, "BiomeDef", biome_name, "forceRockTypes") {
        return forced.iter().filter_map(|n| defs.things.id(n)).collect();
    }
    let all: Vec<DefId<ThingDef>> = defs
        .things
        .iter()
        .filter(|(_, t)| {
            t.category.as_deref() == Some("Building")
                && t.building
                    .as_ref()
                    .is_some_and(|b| b.is_natural_rock && !b.is_resource_rock)
                && !raw_bool(
                    defs,
                    "ThingDef",
                    &t.def_name,
                    "mineablePreventNaturalRockOnSurface",
                )
                .unwrap_or(false)
                && defs
                    .raw
                    .get("ThingDef", &t.def_name)
                    .and_then(|d| d.node.path_text(&["building", "unsmoothedThing"]))
                    .is_none()
        })
        .map(|(id, _)| id)
        .collect();
    let extra = raw_list(defs, "BiomeDef", biome_name, "extraRockTypes").unwrap_or_default();
    let mut rng = Rand::new(0);
    rng.push_state_seeded(tile.wrapping_mul(397));
    let count = (rng.range_inclusive(2, 3) as usize).min(all.len());
    let mut candidates: Vec<DefId<ThingDef>> = all
        .into_iter()
        .filter(|&id| {
            let t = &defs.things[id];
            let specific = defs
                .raw
                .get("ThingDef", &t.def_name)
                .and_then(|d| d.node.path_text(&["building", "biomeSpecific"]))
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
            !specific || extra.contains(&t.def_name)
        })
        .collect();
    // `TakeRandomDistinct` over `InRandomOrder`: pick an index among the
    // unchosen, swap it to the end.
    let mut out = Vec::new();
    let mut unchosen = candidates.len();
    while unchosen > 0 && out.len() < count {
        let r = rng.range(0, unchosen as i32) as usize;
        out.push(candidates[r]);
        candidates.swap(r, unchosen - 1);
        unchosen -= 1;
    }
    out
}

/// The elevation module (`GenStep_ElevationFertility`), constructed with
/// its 14 draws in the game's order.
fn elevation_module(rng: &mut Rand, hilliness: Hilliness) -> Box<dyn Module> {
    let base_freq = rng.range_f32(0.015, 0.0225);
    let base_seed = rng.range(0, i32::MAX);
    let base = Perlin::new(base_freq as f64, 2.0, 0.5, 3, base_seed, Quality::High);
    let detail_freq = rng.range_f32(0.03, 0.06);
    let detail_strength = rng.range_f32(5.0, 10.0);
    let detail_octaves = rng.range_inclusive(4, 5);
    let detail_seed = rng.int();
    let m = Displace::new(
        base,
        detail_freq,
        detail_strength,
        detail_octaves,
        detail_seed,
    );
    let m = StretchX(m, rng.range_f32(1.0, 1.15) as f64);
    let m = RotateY::new(m, rng.range_f32(0.0, 180.0) as f64);
    let warp_freq = rng.range_f32(0.01, 0.02);
    let warp_strength = rng.range_f32(0.0, 15.0);
    let warp_octaves = rng.range_inclusive(3, 4);
    let warp_seed = rng.int();
    let m = Displace::new(m, warp_freq, warp_strength, warp_octaves, warp_seed);
    let m = StretchX(m, rng.range_f32(1.0, 1.15) as f64);
    let m = RotateY::new(m, rng.range_f32(0.0, 180.0) as f64);
    let m = ScaleBias {
        source: m,
        scale: 0.5,
        bias: 0.5,
    };
    Box::new(Times(m, hilliness.elevation_factor() as f64))
}

/// The game's per-rock score noises (`RockNoises`).
struct RockNoises {
    noises: Vec<(DefId<ThingDef>, Perlin)>,
}

impl RockNoises {
    /// `RockDefAt`: the highest binary32 score, strictly (first wins ties).
    fn rock_at(&self, c: Cell, defs: &GameDefs) -> DefId<ThingDef> {
        let mut best: Option<DefId<ThingDef>> = None;
        let mut score = -999_999.0f32;
        for (def, noise) in &self.noises {
            let v = noise.at(c.x, c.z);
            if v > score {
                best = Some(*def);
                score = v;
            }
        }
        best.or_else(|| defs.things.id("Sandstone"))
            .expect("Sandstone")
    }
}

/// `GenRadial.RadialPattern`, as `GridShapeMaker.IrregularLump` uses it.
fn irregular_lump(center: Cell, size: GridSize, n: usize, rng: &mut Rand) -> Vec<Cell> {
    let pattern = crate::clean::radial_pattern();
    // A set enumerated in insertion order (only removals follow).
    let mut cells: Vec<Cell> = pattern[..n * 2]
        .iter()
        .map(|&d| center + d)
        .filter(|&c| size.contains(c))
        .collect();
    let neighbours = |cells: &[Cell], c: Cell| {
        CARDINALS
            .iter()
            .filter(|&&d| cells.contains(&(c + d)))
            .count()
    };
    while cells.len() > n {
        let min = cells
            .iter()
            .map(|&c| neighbours(&cells, c))
            .min()
            .unwrap_or(0);
        let list: Vec<Cell> = cells
            .iter()
            .copied()
            .filter(|&c| neighbours(&cells, c) == min)
            .collect();
        let pick = list[rng.range(0, list.len() as i32) as usize];
        cells.retain(|&c| c != pick);
    }
    cells
}

/// [`irregular_lump`] for tests.
pub fn debug_irregular_lump(center: Cell, size: GridSize, n: usize, rng: &mut Rand) -> Vec<Cell> {
    irregular_lump(center, size, n, rng)
}

/// `Mathf.RoundToInt` (ties to even).
fn round_to_int(v: f32) -> i32 {
    v.round_ties_even() as i32
}

/// Runs the supported steps.
pub fn generate(defs: &GameDefs, input: &TileInput) -> Generated {
    let size = input.size;
    let n = (size.width * size.height) as usize;
    let seed = map_seed(&input.world_seed, input.tile);
    let mut rng = Rand::new(0);
    // The content stream: rock noises first.
    rng.push_state_seeded(seed);
    let rock_types = input
        .forced_rocks
        .clone()
        .unwrap_or_else(|| natural_rock_types(defs, input.biome, input.tile));
    let mut rock_noise_seeds = Vec::new();
    let noises = RockNoises {
        noises: rock_types
            .iter()
            .map(|&def| {
                let s = rng.range(0, i32::MAX);
                rock_noise_seeds.push(s);
                (
                    def,
                    Perlin::new(0.004_999_999_888_241_291, 2.0, 0.5, 6, s, Quality::Medium),
                )
            })
            .collect(),
    };
    let cells: Vec<Cell> = (0..size.height)
        .flat_map(|z| (0..size.width).map(move |x| Cell::new(x, z)))
        .collect();

    // GenStep_ElevationFertility.
    rng.push_state_seeded(hash_combine(seed, SEED_PART_ELEVATION));
    let elev = elevation_module(&mut rng, input.hilliness);
    let fert = ScaleBias {
        source: Perlin::new(
            0.020_999_999_716_877_937,
            2.0,
            0.5,
            6,
            rng.range(0, i32::MAX),
            Quality::High,
        ),
        scale: 0.5,
        bias: 0.5,
    };
    let elevation: Vec<f32> = cells
        .iter()
        .map(|c| elev.at(c.x, c.z).min(f32::MAX))
        .collect();
    let fertility: Vec<f32> = cells.iter().map(|c| fert.at(c.x, c.z)).collect();
    let s_elev = rng.state();
    rng.pop_state();

    // GenStep_RocksFromGrid.
    rng.push_state_seeded(hash_combine(seed, SEED_PART_ROCKS));
    let thick = ROCK_ELEVATION * 1.14;
    let thin = ROCK_ELEVATION * 1.04;
    let mut rock: Vec<Option<DefId<ThingDef>>> = vec![None; n];
    let mut roof: Vec<Option<NaturalRoof>> = vec![None; n];
    for (k, &c) in cells.iter().enumerate() {
        let e = elevation[k];
        if e <= ROCK_ELEVATION {
            continue;
        }
        rock[k] = Some(noises.rock_at(c, defs));
        if e > thick {
            roof[k] = Some(NaturalRoof::Thick);
        } else if e > thin {
            roof[k] = Some(NaturalRoof::Thin);
        }
    }
    remove_small_roof_groups(size, &mut roof);
    scatter_ores(defs, input, size, &mut rock, &mut rng);
    let s_rocks = rng.state();
    rng.pop_state();

    // GenStep_Terrain.
    rng.push_state_seeded(hash_combine(seed, SEED_PART_TERRAIN));
    let terrain = terrain_step(
        defs, input, &cells, &elevation, &fertility, &noises, &mut rock, &mut roof, &mut rng,
    );
    let s_terrain = rng.state();
    rng.pop_state();
    rng.pop_state();

    Generated {
        size,
        elevation,
        fertility,
        rock_types,
        rock_noise_seeds,
        rock,
        roof,
        terrain,
        step_streams: [s_elev, s_rocks, s_terrain],
    }
}

/// Natural roof groups (cardinal flood fill) of fewer than 20 cells lose
/// their roof; rocks stay.
fn remove_small_roof_groups(size: GridSize, roof: &mut [Option<NaturalRoof>]) {
    let idx = |c: Cell| (c.z * size.width + c.x) as usize;
    let mut visited = vec![false; roof.len()];
    for z in 0..size.height {
        for x in 0..size.width {
            let start = Cell::new(x, z);
            if visited[idx(start)] || roof[idx(start)].is_none() {
                continue;
            }
            let mut group = vec![start];
            visited[idx(start)] = true;
            let mut k = 0;
            while k < group.len() {
                let c = group[k];
                k += 1;
                for d in CARDINALS {
                    let m = c + d;
                    if size.contains(m) && !visited[idx(m)] && roof[idx(m)].is_some() {
                        visited[idx(m)] = true;
                        group.push(m);
                    }
                }
            }
            if group.len() < MIN_ROOFED_CELLS_PER_GROUP {
                for c in group {
                    roof[idx(c)] = None;
                }
            }
        }
    }
}

/// `GenStep_ScatterLumpsMineable` as the rock step runs it: count from
/// the per-10k rate (width²), centers on natural rock at least 5 apart
/// (1000 not-edge tries each), a weighted ore choice (list algorithm) and
/// an irregular lump of its size.
fn scatter_ores(
    defs: &GameDefs,
    input: &TileInput,
    size: GridSize,
    rock: &mut [Option<DefId<ThingDef>>],
    rng: &mut Rand,
) {
    let per_10k = input
        .ore_blotches_override
        .unwrap_or_else(|| input.hilliness.ore_blotches_per_10k());
    // COMPATIBILITY TODO: currently approximate — the difficulty's nomadic
    // resource factor (non-starting maps) is taken as 1.
    let _ = input.starting_map;
    if per_10k <= 0.0 {
        return;
    }
    let denom = round_to_int(10_000.0 / per_10k);
    let count = round_to_int((size.width * size.width) as f32 / denom as f32);
    let idx = |c: Cell| (c.z * size.width + c.x) as usize;
    let is_natural = |r: Option<DefId<ThingDef>>| {
        r.is_some_and(|d| {
            defs.things[d]
                .building
                .as_ref()
                .is_some_and(|b| b.is_natural_rock)
        })
    };
    // Ore candidates in database order with their commonality and sizes.
    let ores: Vec<(DefId<ThingDef>, f32, (i32, i32))> = defs
        .things
        .iter()
        .map(|(id, t)| {
            let raw = defs.raw.get("ThingDef", &t.def_name);
            let w = if t.building.is_some() {
                raw.and_then(|d| {
                    d.node
                        .path_text(&["building", "mineableScatterCommonality"])
                })
                .and_then(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(0.0)
            } else {
                0.0
            };
            let lump = defs
                .raw
                .get("ThingDef", &t.def_name)
                .and_then(|d| {
                    d.node
                        .path_text(&["building", "mineableScatterLumpSizeRange"])
                })
                .and_then(rimworld_defs::values::parse_float_range)
                .map_or((20, 40), |(a, b)| (a as i32, b as i32));
            (id, w, lump)
        })
        .collect();
    let mut used: Vec<Cell> = Vec::new();
    for _ in 0..count {
        // `TryFindRandomNotEdgeCellWith(5, CanScatterAt)`.
        let mut center = None;
        if 5 <= size.width / 2 && 5 <= size.height / 2 {
            for _ in 0..1000 {
                let x = rng.range(5, size.width - 5);
                let z = rng.range(5, size.height - 5);
                let c = Cell::new(x, z);
                let near = used.iter().any(|u| (u.distance_squared(c) as f32) <= 25.0);
                if !near && is_natural(rock[idx(c)]) {
                    center = Some(c);
                    break;
                }
            }
        }
        let Some(c) = center else {
            return;
        };
        // `RandomElementByWeightWithFallback` over the def list.
        let total: f32 = ores.iter().map(|o| o.1.max(0.0)).fold(0.0, |a, w| a + w);
        let chosen = if total == 0.0 {
            None
        } else {
            let mut r = total * rng.value();
            ores.iter()
                .find(|o| {
                    if o.1 <= 0.0 {
                        return false;
                    }
                    r -= o.1;
                    r <= 0.0
                })
                .copied()
        };
        if let Some((def, _, (lo, hi))) = chosen {
            let n = rng.range_inclusive(lo, hi).max(0) as usize;
            for cell in irregular_lump(c, size, n, rng) {
                rock[idx(cell)] = Some(def);
            }
        }
        used.push(c);
    }
}

/// `TerrainDef.supportsRock` (default true).
fn supports_rock(defs: &GameDefs, t: DefId<TerrainDef>) -> bool {
    raw_bool(
        defs,
        "TerrainDef",
        &defs.terrain[t].def_name,
        "supportsRock",
    )
    .unwrap_or(true)
}

/// `TerrainThreshold.TerrainAtValue`: the first range containing the value
/// (both ends inclusive).
fn threshold_terrain(
    defs: &GameDefs,
    list: &[(String, f32, f32)],
    v: f32,
) -> Option<DefId<TerrainDef>> {
    list.iter()
        .find(|(_, lo, hi)| *lo <= v && *hi >= v)
        .and_then(|(t, _, _)| defs.terrain.id(t))
}

/// A biome's terrain patch maker (`TerrainPatchMaker`).
struct PatchMaker {
    frequency: f32,
    lacunarity: f32,
    persistence: f32,
    octaves: i32,
    min_fertility: f32,
    max_fertility: f32,
    thresholds: Vec<(String, f32, f32)>,
    noise: Option<Perlin>,
}

fn patch_makers(defs: &GameDefs, biome: &BiomeDef) -> Vec<PatchMaker> {
    let Some(node) = defs
        .raw
        .get("BiomeDef", &biome.def_name)
        .and_then(|d| d.node.child("terrainPatchMakers"))
    else {
        return Vec::new();
    };
    node.children
        .iter()
        .map(|li| {
            let f = |k: &str, d: f32| {
                li.child_text(k)
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(d)
            };
            // COMPATIBILITY TODO: currently approximate — `minSize` (the
            // flood-filled minimum patch size) is not modelled; TemperateForest
            // uses 0.
            PatchMaker {
                frequency: f("perlinFrequency", 0.01),
                lacunarity: f("perlinLacunarity", 2.0),
                persistence: f("perlinPersistence", 0.5),
                octaves: f("perlinOctaves", 6.0) as i32,
                min_fertility: f("minFertility", -999.0),
                max_fertility: f("maxFertility", 999.0),
                thresholds: li
                    .child("thresholds")
                    .map(|t| {
                        t.children
                            .iter()
                            .map(|th| {
                                let g = |k: &str, d: f32| {
                                    th.child_text(k)
                                        .and_then(|v| v.trim().parse().ok())
                                        .unwrap_or(d)
                                };
                                (
                                    th.child_text("terrain")
                                        .unwrap_or_default()
                                        .trim()
                                        .to_owned(),
                                    g("min", -1000.0),
                                    g("max", 1000.0),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                noise: None,
            }
        })
        .collect()
}

/// `GenStep_Terrain`: each cell's natural terrain
/// (`MapGenUtility.TerrainFrom`); a wall on terrain that can't hold rock is
/// destroyed (its roof with it).
// COMPATIBILITY TODO: currently approximate — the bulk-collapse check after
// removed walls simply removes their roofs.
#[allow(clippy::too_many_arguments)]
fn terrain_step(
    defs: &GameDefs,
    input: &TileInput,
    cells: &[Cell],
    elevation: &[f32],
    fertility: &[f32],
    noises: &RockNoises,
    rock: &mut [Option<DefId<ThingDef>>],
    roof: &mut [Option<NaturalRoof>],
    rng: &mut Rand,
) -> Vec<DefId<TerrainDef>> {
    let biome = &defs.biomes[input.biome];
    let mut makers = patch_makers(defs, biome);
    let no_gravel = raw_bool(defs, "BiomeDef", &biome.def_name, "noGravel").unwrap_or(false);
    let gravel = raw_text(defs, "BiomeDef", &biome.def_name, "gravelTerrain")
        .and_then(|t| defs.terrain.id(t))
        .or_else(|| defs.terrain.id("Gravel"));
    let sand = defs.terrain.id("Sand").expect("Sand");
    let natural_terrain = |c: Cell| {
        defs.things[noises.rock_at(c, defs)]
            .building
            .as_ref()
            .and_then(|b| b.natural_terrain.as_deref())
            .and_then(|t| defs.terrain.id(t))
    };
    let mut out = Vec::with_capacity(cells.len());
    for (k, &c) in cells.iter().enumerate() {
        let full = rock[k].is_some();
        let (e, f) = (elevation[k], fertility[k]);
        let mut t: Option<DefId<TerrainDef>> = None;
        for m in &mut makers {
            if f < m.min_fertility || f > m.max_fertility {
                continue;
            }
            let noise = m.noise.get_or_insert_with(|| {
                Perlin::new(
                    m.frequency as f64,
                    m.lacunarity as f64,
                    m.persistence as f64,
                    m.octaves,
                    rng.range(0, i32::MAX),
                    Quality::Medium,
                )
            });
            let v = noise.at(c.x, c.z);
            t = threshold_terrain(defs, &m.thresholds, v);
            if t.is_some() {
                break;
            }
        }
        if t.is_none() {
            if e > 0.55 && e < 0.61 && !no_gravel {
                t = gravel;
            } else if e >= 0.61 {
                t = natural_terrain(c);
            }
        }
        let mut t = t
            .or_else(|| threshold_terrain(defs, &biome.terrains_by_fertility, f))
            .unwrap_or(sand);
        if full
            && supports_rock(defs, t)
            && let Some(r) = natural_terrain(c)
        {
            t = r;
        }
        if !supports_rock(defs, t) && rock[k].is_some() {
            rock[k] = None;
            roof[k] = None;
        }
        out.push(t);
    }
    out
}

/// Four-way connected components of the solid-rock mask, largest first.
pub fn rock_components(g: &Generated) -> Vec<usize> {
    let size = g.size;
    let idx = |c: Cell| (c.z * size.width + c.x) as usize;
    let mut seen = vec![false; g.rock.len()];
    let mut sizes = Vec::new();
    for z in 0..size.height {
        for x in 0..size.width {
            let s = Cell::new(x, z);
            if seen[idx(s)] || g.rock[idx(s)].is_none() {
                continue;
            }
            seen[idx(s)] = true;
            let mut stack = vec![s];
            let mut n = 0;
            while let Some(c) = stack.pop() {
                n += 1;
                for d in CARDINALS {
                    let m = c + d;
                    if size.contains(m) && !seen[idx(m)] && g.rock[idx(m)].is_some() {
                        seen[idx(m)] = true;
                        stack.push(m);
                    }
                }
            }
            sizes.push(n);
        }
    }
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    sizes
}
