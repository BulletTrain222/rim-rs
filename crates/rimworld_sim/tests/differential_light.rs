//! Differential check of lamp light against the light probe trace (local,
//! gitignored): a torch lamp under a fully roofed strip with a wall and a
//! door; every cell's ground glow, and the torch's fuel over time.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
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

fn trace_file(dir: &str, name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        });
    let p = base.join(dir).join(name);
    p.exists().then_some(p)
}

#[test]
fn torch_light_and_fuel_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("light1", "trace_light_torch.csv") else {
        eprintln!("skipping: no light trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let origin = Cell::new(20, 20);
    let (w, h) = (24, 9);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(
        defs.clone(),
        Map::new(GridSize::new(origin.x + w + 10, origin.z + h + 10), soil),
    );
    // The probe roofed the strip and a ring around it; the game let some
    // unsupported roof collapse, so take the roofs it logged.
    for line in text.lines().filter_map(|l| l.strip_prefix("#roofRow,")) {
        let (z, row) = line.split_once(',').unwrap();
        let z: i32 = z.parse().unwrap();
        for (k, ch) in row.chars().enumerate() {
            sim.map
                .set_roof(origin + Cell::new(k as i32 - 1, z), ch == '1');
        }
    }
    let sky: f32 = text
        .lines()
        .find_map(|l| l.strip_prefix("#skyGlow,"))
        .unwrap()
        .parse()
        .unwrap();
    sim.debug_set_sky_glow(Some(sky));
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog");
    for z in 2..=6 {
        let def = if z == 4 { door } else { wall };
        sim.debug_spawn_building(def, wood, origin + Cell::new(8, z));
    }
    let torch = defs.things.id("TorchLamp").unwrap();
    let at = origin + Cell::new(4, 4);
    sim.debug_spawn_building(torch, None, at);
    sim.debug_refresh_light();
    let mut checked = 0;
    let mut worst = 0.0f32;
    for line in text.lines().filter_map(|l| l.strip_prefix("#glowRow,")) {
        let (z, values) = line.split_once(',').unwrap();
        let z: i32 = z.parse().unwrap();
        for (k, v) in values.split(';').enumerate() {
            let c = origin + Cell::new(k as i32 - 1, z);
            let game: f32 = v.parse().unwrap();
            let ours = sim.map.ground_glow(c);
            worst = worst.max((ours - game).abs());
            assert!(
                (ours - game).abs() < 1e-6,
                "glow at {:?}: {ours} vs {game}",
                Cell::new(c.x - origin.x, c.z - origin.z)
            );
            checked += 1;
        }
    }
    assert!(checked > 200, "{checked}");
    // Fuel: 2 a day, one f32 subtraction per tick (17 ULPs at about 20).
    // The probe logged it every 250 ticks.
    let rows: Vec<f32> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split_once(',').unwrap().1.parse().unwrap())
        .collect();
    let mut s = Sim::new(defs.clone(), Map::new(GridSize::new(10, 10), soil));
    let lamp = Cell::new(5, 5);
    s.debug_spawn_building(torch, None, lamp);
    assert_eq!(s.fuel_at(lamp), Some(20.0));
    // Burn down to the game's first logged level, then compare.
    let mut spent = 0;
    while s.fuel_at(lamp).unwrap() > rows[0] {
        s.tick();
        spent += 1;
        assert!(spent < 1_000, "never reached {}", rows[0]);
    }
    assert_eq!(s.fuel_at(lamp).unwrap(), rows[0]);
    for &game in &rows[1..] {
        for _ in 0..250 {
            s.tick();
        }
        assert_eq!(s.fuel_at(lamp).unwrap(), game);
    }
    eprintln!("light: {checked} cells match (worst {worst}); fuel matches");
}

#[test]
fn refuelling_a_torch_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("light1", "trace_refuel_torch.csv") else {
        eprintln!("skipping: no refuel trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> Vec<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect()
    };
    let cell = |v: &[String]| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap());
    assert!(!text.contains("#error"), "probe timed out");
    let origin = cell(&header("origin"));
    let area = cell(&header("area"));
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let mut map = Map::new(
        GridSize::new(origin.x + area.x + 2, origin.z + area.z + 2),
        water,
    );
    for x in 0..area.x {
        for z in 0..area.z {
            map.terrain[Cell::new(origin.x + x, origin.z + z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    // Daylight in the probe run.
    sim.debug_set_sky_glow(Some(1.0));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    sim.set_carrying_capacity(pawn, header("carryingCapacity")[0].parse().unwrap());
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Hauling" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, rimworld_sim::NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, rimworld_sim::NeedKind::Rest, 1.0);
    let t = header("torch");
    let torch_at = cell(&t);
    sim.debug_spawn_building(defs.things.id("TorchLamp").unwrap(), None, torch_at);
    sim.debug_set_fuel(torch_at, t[3].parse().unwrap());
    let w = header("wood");
    let wood = defs.things.id("WoodLog").unwrap();
    sim.spawn_item(wood, cell(&w), w[2].parse().unwrap());
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        if f[8].contains("Wander") {
            break;
        }
        while sim.tick_count() < tick {
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let p = sim.pawn(pawn).unwrap();
        let job = p
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: job");
        if let Some(rimworld_sim::job::JobKind::Refuel { stage, .. }) =
            p.job.as_ref().map(|j| j.kind)
        {
            let toil = match stage {
                rimworld_sim::job::RefuelStage::GotoFuel => 2,
                rimworld_sim::job::RefuelStage::GotoBuilding => 5,
                rimworld_sim::job::RefuelStage::Wait { .. } => 6,
            };
            assert_eq!(toil.to_string(), f[9], "{ctx}: toil");
        }
        let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        assert_eq!(p.position, pos, "{ctx}: position");
        let carried: u32 = f[11].parse().unwrap();
        assert_eq!(p.carried.map_or(0, |c| c.count), carried, "{ctx}: carried");
        let (moving, total): (bool, f32) = (f[3] == "1", f[7].parse().unwrap());
        if moving && total != 1.0 {
            let step = p.step.unwrap_or_else(|| panic!("{ctx}: mid-step"));
            let next = Cell::new(f[4].parse().unwrap(), f[5].parse().unwrap());
            assert_eq!(step.to, next, "{ctx}: next cell");
        }
        let fuel: f32 = f[12].parse().unwrap();
        let ours = sim.fuel_at(torch_at).unwrap();
        assert!((ours - fuel).abs() < 1e-5, "{ctx}: fuel {ours} vs {fuel}");
        checked += 1;
    }
    assert!(checked > 400, "{checked}");
    eprintln!("refuel: {checked} ticks match");
}
