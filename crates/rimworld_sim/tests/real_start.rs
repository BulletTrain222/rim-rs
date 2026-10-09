//! The start experience with the real game data: a biome-dressed map with
//! wild plants, plants cut to make way for crops and buildings, and trees
//! chopped for wood. Skipped without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::mapgen::{TestMapPalette, apply_biome, generate_test_map};
use rimworld_sim::{Cell, GridSize, Map, Sim};

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

#[test]
fn a_temperate_forest_map_has_wild_plants() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let palette = TestMapPalette::from_defs(&defs).unwrap();
    let mut map = generate_test_map(GridSize::new(80, 80), 4, &palette);
    let biome = defs.biomes.get("TemperateForest").unwrap();
    assert!((biome.plant_density - 0.65).abs() < 1e-5);
    apply_biome(&mut map, &defs, biome, 4);
    let count = |name: &str| {
        let id = defs.things.id(name).unwrap();
        map.plants().iter().filter(|p| p.def == id).count()
    };
    let (grass, trees) = (
        count("Plant_Grass"),
        count("Plant_TreeOak") + count("Plant_TreePoplar"),
    );
    eprintln!("plants {} grass {grass} trees {trees}", map.plants().len());
    assert!(map.plants().len() > 1000);
    assert!(grass > trees && trees > 0);
    let rich = defs.terrain.id("SoilRich").unwrap();
    assert!(map.size().cells().any(|c| map.terrain[c] == rich));
    // The same seed gives the same map.
    let mut again = generate_test_map(GridSize::new(80, 80), 4, &palette);
    apply_biome(&mut again, &defs, biome, 4);
    assert_eq!(map.plants(), again.plants());
}

fn sim_on(defs: &Arc<GameDefs>, seed: u64) -> (Sim, rimworld_sim::PawnId) {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(30, 30), soil), seed);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 5)).unwrap();
    for s in ["Plants", "Construction"] {
        sim.set_skill(a, s, 10);
    }
    sim.initialize_work(a);
    sim.set_work_priority(a, "PlantCutting", 3);
    (sim, a)
}

#[test]
fn grass_in_a_field_is_cut_before_sowing() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, _a) = sim_on(&defs, 2);
    let grass = defs.things.id("Plant_Grass").unwrap();
    let rice = defs.things.id("Plant_Rice").unwrap();
    let field: Vec<Cell> = (10..13).map(|x| Cell::new(x, 10)).collect();
    for &c in &field {
        sim.map.spawn_plant(grass, c, 0.8, 85.0);
    }
    sim.designate_growing_zone(rice, &field).unwrap();
    let mut saw_cut = false;
    for _ in 0..20_000 {
        sim.tick();
        if sim.pawns().iter().any(|p| {
            matches!(
                p.job.as_ref().map(|j| j.kind),
                Some(JobKind::Harvest { cut: true, .. })
            )
        }) {
            saw_cut = true;
        }
        if field
            .iter()
            .all(|&c| sim.map.plant_at(c).is_some_and(|p| p.def == rice))
        {
            break;
        }
    }
    assert!(saw_cut, "grass was cut");
    assert!(
        field
            .iter()
            .all(|&c| sim.map.plant_at(c).is_some_and(|p| p.def == rice))
    );
}

#[test]
fn a_tree_in_the_way_is_cut_and_designated_trees_give_wood() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, _a) = sim_on(&defs, 3);
    let oak = defs.things.id("Plant_TreeOak").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let wall = defs.things.id("Wall").unwrap();
    // A grown oak where a wall should go.
    let site = Cell::new(12, 12);
    sim.map.spawn_plant(oak, site, 1.0, 200.0);
    sim.spawn_item(wood, Cell::new(6, 6), 30);
    sim.place_blueprint(wall, Some(wood), site).unwrap();
    // Another oak designated for chopping.
    let other = Cell::new(20, 20);
    let tree = sim.map.spawn_plant(oak, other, 1.0, 200.0);
    assert_eq!(sim.designate_cut_plants(&[other]), 1);
    assert_eq!(sim.map.cut_designations(), &[tree]);
    for _ in 0..60_000 {
        sim.tick();
        if sim.map.buildings[site] == Some(wall) && sim.map.plant_at(other).is_none() {
            break;
        }
    }
    let p = sim.pawns()[0].clone();
    eprintln!(
        "job {:?} trail {:?} work {:?}",
        p.job.as_ref().map(|j| j.kind),
        p.think_trail,
        p.work
    );
    assert!(sim.map.plant_at(site).is_none(), "blocking tree cut");
    assert_eq!(sim.map.buildings[site], Some(wall), "wall built");
    assert!(sim.map.plant_at(other).is_none(), "designated tree cut");
    assert!(sim.map.cut_designations().is_empty());
    let logs: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == wood)
        .map(|i| i.stack_count)
        .sum::<u32>()
        + sim
            .pawns()
            .iter()
            .filter_map(|p| p.carried)
            .filter(|c| c.def == wood)
            .map(|c| c.count)
            .sum::<u32>();
    eprintln!("wood now {logs}");
    // 30 − 5 for the wall + two grown oaks' yield (25 each, randomly
    // rounded and scaled by the cutter's PlantHarvestYield).
    assert!(logs > 30, "{logs}");
}

#[test]
fn wild_plants_regrow_over_time() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(40, 40), soil), 5);
    let biome = defs.biomes.id("TemperateForest").unwrap();
    sim.set_biome(Some(biome), 77);
    assert!(sim.map.plants().is_empty());
    for _ in 0..5 * 60_000 {
        sim.tick();
    }
    let n = sim.map.plants().len();
    eprintln!("regrown plants after 5 days: {n}");
    assert!(n > 10);
}
