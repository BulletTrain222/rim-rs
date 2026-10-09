//! Rotting with the real game data: which foods rot, how fast at a
//! temperature, on the item's rare tick, and blending when stacks merge.
//! Skipped without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::map::blend_rot;
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

fn empty_sim(defs: &Arc<GameDefs>) -> Sim {
    let soil = defs.terrain.id("Soil").unwrap();
    Sim::new(defs.clone(), Map::new(GridSize::new(10, 10), soil))
}

/// Rare ticks of a thing with `id_number` from tick 1 to `t`.
fn rare_ticks(t: u64, id_number: u64) -> u64 {
    let bucket = id_number % 250;
    let first = if bucket == 0 { 250 } else { bucket };
    if t < first { 0 } else { (t - first) / 250 + 1 }
}

#[test]
fn foods_rot_as_their_defs_say() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rot = |n: &str| defs.things.get(n).unwrap().rottable.clone();
    let meal = rot("MealSimple").unwrap();
    assert_eq!(meal.ticks_to_rot_start(), 4 * 60_000);
    assert!(meal.rot_destroys);
    assert!(rot("RawRice").is_some_and(|r| r.rot_destroys && r.days_to_rot_start > 4.0));
    assert!(rot("MealSurvivalPack").is_none());
    assert!(rot("Steel").is_none());
}

#[test]
fn a_meal_rots_away_after_four_warm_days_on_its_rare_tick() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = empty_sim(&defs);
    sim.outdoor_temperature = 21.0;
    let meal = defs.things.id("MealSimple").unwrap();
    let id = sim.spawn_item(meal, Cell::new(5, 5), 3);
    sim.map.set_item_id_number(id, 1003);
    // In a closed, roofed room it does not deteriorate (on this tiny map
    // every cell's weathering comes round every 100 ticks).
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    for d in Cell::NEIGHBORS_8 {
        sim.debug_spawn_building(wall, wood, Cell::new(5 + d.x, 5 + d.z));
    }
    sim.map.set_roof(Cell::new(5, 5), true);
    let gone = loop {
        sim.tick();
        let t = sim.tick_count();
        match sim.map.item(id) {
            None => break t,
            Some(item) => {
                // Rot moves only on the item's rare tick (1003 % 250 = 3).
                assert_eq!(item.rot, (rare_ticks(t, 1003) * 250) as f32, "tick {t}");
            }
        }
        assert!(t < 5 * 60_000, "never rotted");
    };
    // The 960th rare tick reaches 240000 ticks of rot.
    assert_eq!(gone, 3 + 959 * 250);
}

#[test]
fn cold_slows_and_freezing_stops_rot() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let meal = defs.things.id("MealSimple").unwrap();
    for (temp, per_rare_tick) in [(-5.0, 0.0), (0.0, 0.0), (5.0, 125.0), (10.0, 250.0)] {
        let mut sim = empty_sim(&defs);
        sim.outdoor_temperature = temp;
        sim.debug_set_room_temperatures(temp);
        let id = sim.spawn_item(meal, Cell::new(5, 5), 1);
        sim.map.set_item_id_number(id, 7);
        for _ in 0..2_500 {
            sim.tick();
        }
        let rot = sim.map.item(id).unwrap().rot;
        assert_eq!(
            rot,
            per_rare_tick * rare_ticks(2_500, 7) as f32,
            "{temp} °C"
        );
    }
}

#[test]
fn merged_stacks_blend_their_rot_by_count() {
    // 10 units at 1000 absorbing 30 units at 3000: Lerp(1000, 3000, 30/40).
    assert_eq!(blend_rot(1000.0, 10, 3000.0, 30), 2500.0);
    assert_eq!(blend_rot(500.0, 5, 500.0, 5), 500.0);
    assert_eq!(blend_rot(0.0, 0, 800.0, 4), 800.0);
}

#[test]
fn a_climate_moves_the_outdoor_temperature_by_day_and_season() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = empty_sim(&defs);
    sim.climate = Some(rimworld_sim::climate::Climate {
        tile_temperature: 15.0,
    });
    sim.latitude = 35.0;
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let mut changes = 0;
    let mut last = sim.outdoor_temperature;
    for _ in 0..60_000 {
        sim.tick();
        let t = sim.outdoor_temperature;
        lo = lo.min(t);
        hi = hi.max(t);
        if t != last {
            changes += 1;
            last = t;
        }
    }
    // The sun swings ±7 °C over a day; the cache refreshes every 60 ticks.
    assert!(hi - lo > 13.0 && hi - lo < 14.5, "{lo}..{hi}");
    assert!((990..=1000).contains(&changes), "{changes}");
    // Mid-winter (the eleventh twelfth) is colder than mid-summer.
    let at_day = |day: i32| {
        let mut s = empty_sim(&defs);
        s.climate = sim.climate;
        s.latitude = 35.0;
        s.start_day_of_year = day;
        s.tick();
        s.outdoor_temperature
    };
    assert!(
        at_day(52) < at_day(22) - 20.0,
        "{} {}",
        at_day(52),
        at_day(22)
    );
}
