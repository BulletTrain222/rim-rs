//! Farming end to end with the real game data: a growing zone is sown,
//! grows, is harvested, the crop is hauled and the zone resown. Skipped
//! without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::plant::{LifeStage, sun_glow};
use rimworld_sim::storage::StoragePriority;
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
fn crop_defs_are_read() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rice = defs.things[defs.things.id("Plant_Rice").unwrap()]
        .plant
        .clone()
        .unwrap();
    assert_eq!(rice.grow_days, 3.0);
    assert_eq!(rice.harvest_yield, 6.0);
    assert_eq!(rice.harvested_thing_def.as_deref(), Some("RawRice"));
    assert!(rice.sowable() && rice.harvestable() && rice.harvest_destroys());
    assert_eq!(rice.sow_work, 170.0);
    assert_eq!(rice.harvest_work, 200.0);
    let potato = defs.things[defs.things.id("Plant_Potato").unwrap()]
        .plant
        .clone()
        .unwrap();
    assert_eq!(potato.fertility_sensitivity, 0.4);
    let corn = defs.things[defs.things.id("Plant_Corn").unwrap()]
        .plant
        .clone()
        .unwrap();
    assert_eq!(corn.fertility_min, 0.7);
    assert!(sun_glow(0.0, 0, 0.5) > 0.9);
}

#[test]
fn colonists_sow_grow_harvest_and_resow_rice() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(40, 40), soil), 12);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(10, 10)).unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(11, 10)).unwrap();
    for p in [a, b] {
        sim.set_skill(p, "Plants", 8);
        sim.initialize_work(p);
    }
    let rice = defs.things.id("Plant_Rice").unwrap();
    let raw_rice = defs.things.id("RawRice").unwrap();
    // Packaged survival meals, which do not rot.
    let meal = defs.things.id("MealSurvivalPack").unwrap();
    sim.spawn_item(meal, Cell::new(5, 5), 10);
    sim.spawn_item(meal, Cell::new(5, 6), 10);
    sim.spawn_item(meal, Cell::new(5, 7), 10);
    sim.spawn_item(meal, Cell::new(5, 8), 10);
    let field: Vec<Cell> = (15..18)
        .flat_map(|x| (15..18).map(move |z| Cell::new(x, z)))
        .collect();
    let zone = sim.designate_growing_zone(rice, &field).unwrap();
    assert_eq!(sim.map.growing_zone(zone).cells.len(), 9);
    let filter = rimworld_sim::storage::ThingFilter::preset(
        &sim.defs,
        rimworld_sim::storage::StoragePreset::DefaultStockpile,
    );
    sim.map.storage.add_stockpile(
        StoragePriority::Normal,
        &[Cell::new(25, 25), Cell::new(26, 25)],
        filter,
    );

    let mut sown_all_at = None;
    let mut harvested_at = None;
    let mut saw_sow = false;
    let mut saw_harvest = false;
    for _ in 0..900_000 {
        sim.tick();
        for p in sim.pawns() {
            match p.job.as_ref().map(|j| j.kind) {
                Some(JobKind::Sow { .. }) => saw_sow = true,
                Some(JobKind::Harvest { .. }) => saw_harvest = true,
                _ => {}
            }
        }
        let growing = field
            .iter()
            .filter(|&&c| {
                sim.map
                    .plant_at(c)
                    .is_some_and(|pl| pl.life_stage() != LifeStage::Sowing)
            })
            .count();
        if sown_all_at.is_none() && growing == 9 {
            sown_all_at = Some(sim.tick_count());
        }
        let rice_items: u32 = sim
            .map
            .items()
            .iter()
            .filter(|i| i.def == raw_rice)
            .map(|i| i.stack_count)
            .sum();
        if harvested_at.is_none() && rice_items > 0 && saw_harvest {
            harvested_at = Some(sim.tick_count());
        }
        // Resown after harvesting: the field has young plants again.
        if harvested_at.is_some()
            && sim.tick_count() > harvested_at.unwrap() + 60_000
            && field.iter().all(|&c| sim.map.plant_at(c).is_some())
        {
            break;
        }
    }
    eprintln!(
        "sown {sown_all_at:?}, harvested {harvested_at:?}, now {}",
        sim.tick_count()
    );
    assert!(saw_sow, "sowing happened");
    let sown = sown_all_at.expect("every cell sown");
    let harvested = harvested_at.expect("rice harvested");
    // Rice needs 3 growing days; plants rest 45% of the day and light ramps
    // with the sun, so it takes several days in all.
    assert!(harvested - sown > 3 * 60_000, "{sown} {harvested}");
    let rice_total: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == raw_rice)
        .map(|i| i.stack_count)
        .sum::<u32>()
        + sim
            .pawns()
            .iter()
            .filter_map(|p| p.carried)
            .filter(|c| c.def == raw_rice)
            .map(|c| c.count)
            .sum::<u32>();
    eprintln!("rice on the map: {rice_total}");
    assert!(rice_total > 0);
    assert!(
        field.iter().all(|&c| sim.map.plant_at(c).is_some()),
        "resown"
    );
}

#[test]
fn hard_frost_kills_crops_and_strips_wild_plants() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(10, 10), soil));
    sim.outdoor_temperature = -20.0;
    sim.debug_set_room_temperatures(-20.0);
    let rice = defs.things.id("Plant_Rice").unwrap();
    let grass = defs.things.id("Plant_Grass").unwrap();
    let r = sim.map.spawn_plant(rice, Cell::new(2, 2), 0.5, 85.0);
    let g = sim.map.spawn_plant(grass, Cell::new(4, 4), 0.5, 85.0);
    for _ in 0..2_000 {
        sim.tick();
    }
    // Rice dies when leafless (`dieIfLeafless`); grass just loses its
    // leaves (thresholds are -10 to -18 °C below 0 °C minimum growth).
    assert!(sim.map.plant(r).is_none(), "rice survived -20 °C");
    let g = sim.map.plant(g).expect("grass survives");
    assert!(g.leafless_now(sim.tick_count() as i64));
}
