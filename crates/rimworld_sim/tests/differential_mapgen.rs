//! Map generation against the research's native fixtures (the reports
//! `map_generation_phase1/2.md` and `mountains_mining.md`; rev591 traces):
//! world `ferro-mapgen-phase1`, tile 100, 80×80, TemperateForest, forced
//! Granite,Limestone. Grid hashes serialize binary32 little-endian
//! (z-major, x-minor); the solid mask is one byte per cell.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::native_mapgen::{
    Generated, Hilliness, NaturalRoof, TileInput, generate, map_seed, natural_rock_types,
    rock_components,
};
use rimworld_sim::rand::Rand;
use rimworld_sim::sha256;
use rimworld_sim::{Cell, GridSize};

fn real_defs() -> Option<Arc<GameDefs>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = AppConfig::load(&root.join("config.toml")).ok().flatten();
    let env = std::env::var_os(ENV_RIMWORLD_PATH).map(Into::into);
    let install = resolve_install(None, env, config.as_ref(), &[]).ok()?;
    let packs: Vec<PackSource> = install
        .content_packs()
        .into_iter()
        .map(|p| PackSource {
            defs_dir: p.defs_dir(),
            package_id: p.package_id,
        })
        .collect();
    let (db, _) = load_packs(&packs).ok()?;
    Some(Arc::new(GameDefs::from_database(db).0))
}

fn input(defs: &GameDefs, hilliness: Hilliness, ores: Option<f32>) -> TileInput {
    TileInput {
        world_seed: "ferro-mapgen-phase1".to_owned(),
        tile: 100,
        size: GridSize::new(80, 80),
        hilliness,
        biome: defs.biomes.id("TemperateForest").unwrap(),
        starting_map: false,
        forced_rocks: Some(vec![
            defs.things.id("Granite").unwrap(),
            defs.things.id("Limestone").unwrap(),
        ]),
        ore_blotches_override: ores,
    }
}

fn float_hash(v: &[f32]) -> String {
    let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
    sha256::hex(&bytes)
}

fn mask_hash(g: &Generated) -> String {
    let bytes: Vec<u8> = g.rock.iter().map(|r| u8::from(r.is_some())).collect();
    sha256::hex(&bytes)
}

/// Seeds: the map seed's step streams, and the rock noise seeds drawn on
/// the content stream.
#[test]
fn seeds_match_the_native_streams() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let g = generate(&defs, &input(&defs, Hilliness::SmallHills, Some(0.0)));
    assert_eq!(g.rock_noise_seeds, vec![1_415_372_179, 1_456_029_765]);
    assert_eq!(
        g.step_streams[0],
        (2_109_206_802, 15),
        "elevation: 15 draws"
    );
    assert_eq!(
        g.step_streams[1],
        (1_786_169_530, 0),
        "rocks without ore: 0"
    );
    assert_eq!(
        g.step_streams[2],
        (1_394_239_934, 1),
        "terrain: the pond noise"
    );
    let _ = map_seed("ferro-mapgen-phase1", 100);
}

/// `geometry2`/`geometry3`: SmallHills, no ores — the elevation and
/// fertility grids, the 107-cell mask, its cardinal components 75/27/4/1
/// (largest x54..72 z53..62), 41 thin roofs, and the edge cells.
#[test]
fn small_hills_geometry_matches_native() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let g = generate(&defs, &input(&defs, Hilliness::SmallHills, Some(0.0)));
    assert_eq!(
        float_hash(&g.elevation),
        "DE398E70742526B089D2E10A6469F00402C803D03699527E4FA8FD5B167365AA"
    );
    assert_eq!(
        float_hash(&g.fertility),
        "A8D65D05984765196D300C2BAFC045B720ACCF0009A06EF3B8199EDB7171FFE4"
    );
    assert_eq!(g.rock.iter().filter(|r| r.is_some()).count(), 107);
    assert_eq!(
        mask_hash(&g),
        "71FACFB59BD7C2246A3B720ED91833B6FE3BB82F90F13C5D6CE3CF44A7FF8E74"
    );
    assert_eq!(rock_components(&g), vec![75, 27, 4, 1]);
    let thin = g
        .roof
        .iter()
        .filter(|r| **r == Some(NaturalRoof::Thin))
        .count();
    let thick = g
        .roof
        .iter()
        .filter(|r| **r == Some(NaturalRoof::Thick))
        .count();
    assert_eq!((thin, thick), (41, 0));
    let lime = defs.things.id("Limestone").unwrap();
    let lime_rough = defs.terrain.id("Limestone_Rough").unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    for (x, z, bits, wall, terrain, roof) in [
        (0, 0, 0x3EE6_6666u32, None, soil, None),
        (60, 58, 0x3F2D_D198, None, lime_rough, None),
        (61, 58, 0x3F35_8BB0, Some(lime), lime_rough, None),
        (
            62,
            58,
            0x3F41_566E,
            Some(lime),
            lime_rough,
            Some(NaturalRoof::Thin),
        ),
        (
            63,
            58,
            0x3F49_A3F1,
            Some(lime),
            lime_rough,
            Some(NaturalRoof::Thin),
        ),
        (
            64,
            58,
            0x3F49_B0C9,
            Some(lime),
            lime_rough,
            Some(NaturalRoof::Thin),
        ),
        (66, 58, 0x3F35_9754, Some(lime), lime_rough, None),
    ] {
        let k = g.index(Cell::new(x, z));
        assert_eq!(g.elevation[k].to_bits(), bits, "elevation ({x},{z})");
        assert_eq!(g.rock[k], wall, "wall ({x},{z})");
        assert_eq!(g.terrain[k], terrain, "terrain ({x},{z})");
        assert_eq!(g.roof[k], roof, "roof ({x},{z})");
    }
}

/// `geometry1`: Flat with the ordinary ore rate — 3 threshold cells, then
/// a 30-cell MineableSteel lump; the post-ore mask.
#[test]
fn flat_control_matches_native() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let g = generate(&defs, &input(&defs, Hilliness::Flat, None));
    assert_eq!(
        float_hash(&g.elevation),
        "227762715036DEBD26631B2D8EEFFD06ADE1C0DD65AF2012B8AB8DC07B5A5995"
    );
    assert_eq!(
        mask_hash(&g),
        "6E787A029BA9CDA6C63CACA4A7BB79CF78ACF7829A5D4919DAB84D3B85D62459"
    );
    let steel = defs.things.id("MineableSteel").unwrap();
    assert_eq!(g.rock.iter().filter(|r| **r == Some(steel)).count(), 30);
    assert!(g.roof.iter().all(|r| r.is_none()));
}

/// Unforced TemperateForest tile 100: Marble, Limestone.
#[test]
fn tile_rock_types_match_native() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let b = defs.biomes.id("TemperateForest").unwrap();
    let names: Vec<String> = natural_rock_types(&defs, b, 100)
        .iter()
        .map(|&d| defs.things[d].def_name.clone())
        .collect();
    assert_eq!(names, vec!["Marble", "Limestone"]);
}

/// The irregular lump fixture: center (20,20), size 10, seed 123, 10
/// draws, the native cell order.
#[test]
fn irregular_lump_matches_native() {
    let mut rng = Rand::new(0);
    rng.push_state_seeded(123);
    let cells = rimworld_sim::native_mapgen::debug_irregular_lump(
        Cell::new(20, 20),
        GridSize::new(80, 80),
        10,
        &mut rng,
    );
    assert_eq!(
        cells,
        [
            (20, 20),
            (21, 20),
            (20, 21),
            (20, 19),
            (19, 20),
            (21, 21),
            (19, 21),
            (21, 19),
            (22, 20),
            (22, 19)
        ]
        .map(|(x, z)| Cell::new(x, z))
        .to_vec()
    );
    assert_eq!(rng.state(), (123, 10));
}

/// Lengths of maximal straight runs of rock/open boundary edges (along
/// rows and columns) inside the map (the map border clips rock and is not
/// a boundary), longest first.
fn boundary_runs(g: &Generated) -> Vec<usize> {
    let (w, h) = (g.size.width, g.size.height);
    let solid = |x: i32, z: i32| {
        x >= 0 && z >= 0 && x < w && z < h && g.rock[g.index(Cell::new(x, z))].is_some()
    };
    let mut runs = Vec::new();
    // Horizontal edges between rows z and z+1.
    for z in 0..h - 1 {
        let mut run = 0;
        for x in 0..=w {
            let edge = x < w && solid(x, z) != solid(x, z + 1);
            if edge {
                run += 1;
            } else if run > 0 {
                runs.push(run);
                run = 0;
            }
        }
    }
    // Vertical edges between columns x and x+1.
    for x in 0..w - 1 {
        let mut run = 0;
        for z in 0..=h {
            let edge = z < h && solid(x, z) != solid(x + 1, z);
            if edge {
                run += 1;
            } else if run > 0 {
                runs.push(run);
                run = 0;
            }
        }
    }
    runs.sort_unstable_by(|a, b| b.cmp(a));
    runs
}

/// The app's profile: world "ferrocolony-1", tile 100, 100×100, small
/// hills, the tile's own rocks and ores.
fn app_map(defs: &GameDefs) -> Generated {
    generate(
        defs,
        &TileInput {
            world_seed: "ferrocolony-1".to_owned(),
            tile: 100,
            size: GridSize::new(100, 100),
            hilliness: Hilliness::SmallHills,
            biome: defs.biomes.id("TemperateForest").unwrap(),
            starting_map: true,
            forced_rocks: None,
            ore_blotches_override: None,
        },
    )
}

/// Elevation is sampled per cell from a continuous field: no two
/// neighbouring cells, and no 2×2 block, share a value (a coarse-block or
/// integer-division bug would copy one sample over many cells).
#[test]
fn elevation_varies_cell_by_cell() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    for g in [
        generate(&defs, &input(&defs, Hilliness::SmallHills, Some(0.0))),
        app_map(&defs),
    ] {
        let (w, h) = (g.size.width, g.size.height);
        let e = |x: i32, z: i32| g.elevation[g.index(Cell::new(x, z))].to_bits();
        let mut same = 0;
        let mut blocks = 0;
        for z in 0..h - 1 {
            for x in 0..w - 1 {
                same += usize::from(e(x, z) == e(x + 1, z)) + usize::from(e(x, z) == e(x, z + 1));
                let v = e(x, z);
                blocks += usize::from(e(x + 1, z) == v && e(x, z + 1) == v && e(x + 1, z + 1) == v);
            }
        }
        assert_eq!(same, 0, "identical neighbouring elevations");
        assert_eq!(blocks, 0, "identical 2x2 blocks");
    }
}

/// The report's native window of the SmallHills fixture (x53..73,
/// z52..63, `#` solid), cell for cell.
#[test]
fn native_mask_window_matches() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let g = generate(&defs, &input(&defs, Hilliness::SmallHills, Some(0.0)));
    let native = [
        ".....................",
        "...................#.",
        "...............#####.",
        ".............####.##.",
        "..........#######...#",
        ".........#####......#",
        "..##....######.......",
        "..###########........",
        ".############........",
        ".#.##########........",
        "....#########........",
        ".....................",
    ];
    for (k, row) in native.iter().enumerate() {
        let z = 52 + k as i32;
        let ours: String = (53..=73)
            .map(|x| {
                if g.rock[g.index(Cell::new(x, z))].is_some() {
                    '#'
                } else {
                    '.'
                }
            })
            .collect();
        assert_eq!(&ours, row, "z{z}");
    }
}

/// Rock boundaries are organic: on the native fixture and the app map,
/// most straight boundary runs are a cell or two and none is long (a
/// block-grid artifact would give many long, equal runs).
#[test]
fn rock_boundaries_are_organic() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    for (name, g) in [
        (
            "native fixture",
            generate(&defs, &input(&defs, Hilliness::SmallHills, Some(0.0))),
        ),
        ("app map", app_map(&defs)),
    ] {
        let runs = boundary_runs(&g);
        let n = runs.len() as f32;
        let mean = runs.iter().sum::<usize>() as f32 / n;
        let short = runs.iter().filter(|&&r| r <= 2).count() as f32 / n;
        eprintln!(
            "{name}: {} runs, longest {:?}, mean {mean:.2}, ≤2 cells {:.0}%",
            runs.len(),
            &runs[..runs.len().min(8)],
            short * 100.0
        );
        assert!(mean < 2.5, "{name}: mean run {mean}");
        assert!(runs[0] <= 12, "{name}: longest run {}", runs[0]);
        assert!(short > 0.7, "{name}: short runs {short}");
    }
}
