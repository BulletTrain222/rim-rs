//! Differential replay of the roof probe trace (local, gitignored): a
//! finished 5×5 wooden room with a door gets its auto-roof area, and a
//! colonist with only Construction enabled builds the roof. Compares the
//! colonist, the build roof area and the roofs every tick. Skipped without
//! an install or the trace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::{Cell, GridSize, Map, NeedKind, Sim};

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

/// The probe's grid notation: rows z = 0.. joined by '/', columns x 6..=14.
fn grid(origin: Cell, f: impl Fn(Cell) -> bool) -> String {
    (0..7)
        .map(|z| {
            (6..=14)
                .map(|x| {
                    if f(origin + Cell::new(x, z)) {
                        '1'
                    } else {
                        '0'
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn building_a_roof_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("roof1", "trace_roof_room.csv") else {
        eprintln!("skipping: no roof trace");
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
    let colonist = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(colonist, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    let speed: f32 = header("constructionSpeed")[0].parse().unwrap();
    sim.set_stat_override(pawn, "ConstructionSpeed", speed);
    sim.set_skill(pawn, "Construction", 20);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    // The finished room: wooden walls with a door in the west wall.
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 8..=12 {
        for z in 1..=5 {
            if x != 8 && x != 12 && z != 1 && z != 5 {
                continue;
            }
            let def = if (x, z) == (8, 3) { door } else { wall };
            sim.debug_spawn_building(def, wood, origin + Cell::new(x, z));
        }
    }
    // The game set the area when the room appeared, before the replay.
    sim.debug_resolve_queued_roofs();
    let area_now = grid(origin, |c| sim.map.build_roof(c));
    assert_eq!(area_now, header("buildRoofArea")[0], "auto-roof area");
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
        let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        assert_eq!(p.position, pos, "{ctx}: position");
        let (moving, total): (bool, f32) = (f[3] == "1", f[7].parse().unwrap());
        if moving && total != 1.0 {
            let step = p.step.unwrap_or_else(|| panic!("{ctx}: mid-step"));
            let next = Cell::new(f[4].parse().unwrap(), f[5].parse().unwrap());
            assert_eq!(step.to, next, "{ctx}: next cell");
            let left: f32 = f[6].parse().unwrap();
            assert!(
                (step.cost_left - left).abs() < 1e-3,
                "{ctx}: cost left {} vs {left}, total {} vs {total}",
                step.cost_left,
                step.cost_total
            );
        }
        if let Some(rimworld_sim::job::JobKind::BuildRoof { cell, .. }) =
            p.job.as_ref().map(|j| j.kind)
        {
            let target = Cell::new(f[10].parse().unwrap(), f[11].parse().unwrap());
            assert_eq!(cell, target, "{ctx}: roof target");
        }
        assert_eq!(grid(origin, |c| sim.map.roofed(c)), f[12], "{ctx}: roofs");
        assert_eq!(
            grid(origin, |c| sim.map.build_roof(c)),
            f[13],
            "{ctx}: area"
        );
        checked += 1;
    }
    eprintln!("roof: {checked} ticks match");
}

#[test]
fn a_roofed_room_follows_the_outdoors_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("roof1", "trace_room_temp.csv") else {
        eprintln!("skipping: no room temperature trace");
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
    // The same 16×7 strip walled by deep water as the roof probe.
    let origin = Cell::new(20, 20);
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let mut map = Map::new(GridSize::new(origin.x + 18, origin.z + 9), water);
    for x in 0..16 {
        for z in 0..7 {
            map.terrain[origin + Cell::new(x, z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 8..=12 {
        for z in 1..=5 {
            if x != 8 && x != 12 && z != 1 && z != 5 {
                continue;
            }
            let def = if (x, z) == (8, 3) { door } else { wall };
            sim.debug_spawn_building(def, wood, origin + Cell::new(x, z));
        }
    }
    for x in 8..=12 {
        for z in 1..=5 {
            sim.map.set_roof(origin + Cell::new(x, z), true);
        }
    }
    sim.debug_set_door_id_number(
        origin + Cell::new(8, 3),
        header("doorIDNumber")[0].parse().unwrap(),
    );
    // The wall-equalization cells are the game's.
    let mut game: Vec<Cell> = header("equalize")[0]
        .split(';')
        .map(|v| {
            let (x, z) = v.split_once(':').unwrap();
            origin + Cell::new(x.parse().unwrap(), z.parse().unwrap())
        })
        .collect();
    game.sort_by_key(|c| (c.x, c.z));
    assert_eq!(sim.wall_equalize_cells(origin + Cell::new(10, 3)), game);
    let rows: Vec<Vec<f32>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    sim.debug_set_tick(rows[0][0] as u64);
    sim.outdoor_temperature = rows[0][1];
    sim.debug_set_room_temperatures(rows[0][2]);
    let (inside, outside, door_cell) = (
        origin + Cell::new(10, 3),
        origin + Cell::new(2, 3),
        origin + Cell::new(8, 3),
    );
    let mut checked = 0;
    let mut last_door = rows[0][4];
    let mut first_door_change: Option<u64> = None;
    for r in &rows[1..] {
        let t = r[0] as u64;
        // Something (a colony animal) opened the door at some point in the
        // probe run; open doors mix air every 34 ticks. Stop at the first
        // door change off the closed-door schedule (every 375 ticks).
        if r[4] != last_door {
            last_door = r[4];
            let first = *first_door_change.get_or_insert(t);
            if !(t - first).is_multiple_of(375) {
                break;
            }
        }
        sim.outdoor_temperature = r[1];
        while sim.tick_count() < t {
            sim.tick();
        }
        for (name, cell, game) in [
            ("outside", outside, r[2]),
            ("room", inside, r[3]),
            ("door", door_cell, r[4]),
        ] {
            let ours = sim.cell_temperature(cell);
            assert!(
                (ours - game).abs() < 2e-5,
                "tick {t}: {name} {ours} vs {game}"
            );
        }
        checked += 1;
    }
    eprintln!("room temperature: {checked} ticks match");
}

#[test]
fn a_torch_heats_a_room_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("heat1", "trace_room_heat.csv") else {
        eprintln!("skipping: no room heat trace");
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
    // The room temperature probe's room, with a torch in the middle and no
    // pawns about.
    let origin = Cell::new(20, 20);
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let mut map = Map::new(GridSize::new(origin.x + 18, origin.z + 9), water);
    for x in 0..16 {
        for z in 0..7 {
            map.terrain[origin + Cell::new(x, z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 8..=12 {
        for z in 1..=5 {
            if x != 8 && x != 12 && z != 1 && z != 5 {
                continue;
            }
            let def = if (x, z) == (8, 3) { door } else { wall };
            sim.debug_spawn_building(def, wood, origin + Cell::new(x, z));
        }
    }
    for x in 8..=12 {
        for z in 1..=5 {
            sim.map.set_roof(origin + Cell::new(x, z), true);
        }
    }
    let inside = origin + Cell::new(10, 3);
    let torch = defs.things.id("TorchLamp").unwrap();
    sim.debug_spawn_building(torch, None, inside);
    sim.debug_set_building_id_number(inside, header("torchIDNumber")[0].parse().unwrap());
    sim.debug_set_door_id_number(
        origin + Cell::new(8, 3),
        header("doorIDNumber")[0].parse().unwrap(),
    );
    let mut game: Vec<Cell> = header("equalize")[0]
        .split(';')
        .map(|v| {
            let (x, z) = v.split_once(':').unwrap();
            origin + Cell::new(x.parse().unwrap(), z.parse().unwrap())
        })
        .collect();
    game.sort_by_key(|c| (c.x, c.z));
    assert_eq!(sim.wall_equalize_cells(inside), game);
    let rows: Vec<Vec<f32>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    sim.debug_set_tick(rows[0][0] as u64);
    sim.outdoor_temperature = rows[0][1];
    sim.debug_set_room_temperatures(rows[0][2]);
    sim.debug_set_fuel(inside, rows[0][5]);
    let mut checked = 0;
    let mut peak: f32 = 0.0;
    for r in &rows[1..] {
        let t = r[0] as u64;
        // Visitors and animals enter elsewhere on the map later; the
        // strip is walled by deep water, so none reach the door.
        sim.outdoor_temperature = r[1];
        while sim.tick_count() < t {
            sim.tick();
        }
        for (name, cell, game) in [
            ("outside", origin + Cell::new(2, 3), r[2]),
            ("room", inside, r[3]),
            ("door", origin + Cell::new(8, 3), r[4]),
        ] {
            let ours = sim.cell_temperature(cell);
            assert!(
                (ours - game).abs() < 2e-5,
                "tick {t}: {name} {ours} vs {game}"
            );
        }
        assert_eq!(sim.fuel_at(inside), Some(r[5]), "tick {t}: fuel");
        peak = peak.max(r[3]);
        checked += 1;
    }
    eprintln!("room heat: {checked} ticks match; room peaked at {peak}");
}
