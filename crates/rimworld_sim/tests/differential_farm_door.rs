//! Differential runtime tests for sowing, harvesting and walking through
//! a door: replays traces recorded in the original game by the local
//! diagnostic mod and compares our simulation tick by tick.
//!
//! Needs the game-derived traces (`local/research/traces/`, or
//! `FERROCOLONY_TRACES`) and a RimWorld installation for the real Defs;
//! skipped when either is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::needs::NeedKind;
use rimworld_sim::{Cell, Command, GridSize, Map, Sim};

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

struct Row {
    tick: i64,
    pos: Cell,
    moving: bool,
    next: Cell,
    cost_left: f32,
    cost_total: f32,
    job: String,
    cells: String,
    rice: u32,
}

struct Trace {
    header: HashMap<String, Vec<String>>,
    lists: HashMap<String, Vec<Vec<String>>>,
    rows: Vec<Row>,
}

impl Trace {
    fn get(&self, key: &str) -> &str {
        &self.header[key][0]
    }
    fn num<T: std::str::FromStr>(&self, key: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        self.get(key).parse().unwrap()
    }
    fn cell(&self, key: &str) -> Cell {
        let v = &self.header[key];
        Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap())
    }
    fn cells(&self, key: &str) -> Vec<Cell> {
        self.lists
            .get(key)
            .map(|l| {
                l.iter()
                    .map(|v| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn parse(path: &Path) -> Trace {
    let text = std::fs::read_to_string(path).unwrap();
    let (mut header, mut lists, mut rows) = (
        HashMap::new(),
        HashMap::<String, Vec<Vec<String>>>::new(),
        Vec::new(),
    );
    let cell = |x: &str, z: &str| Cell::new(x.parse().unwrap(), z.parse().unwrap());
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('#') {
            let p: Vec<String> = h.split(',').map(str::to_owned).collect();
            if matches!(p[0].as_str(), "field" | "wallAt") {
                lists.entry(p[0].clone()).or_default().push(p[1..].to_vec());
            } else {
                header.insert(p[0].clone(), p[1..].to_vec());
            }
        } else if !line.starts_with("tick") && !line.is_empty() {
            let f: Vec<&str> = line.split(',').collect();
            rows.push(Row {
                tick: f[0].parse().unwrap(),
                pos: cell(f[1], f[2]),
                moving: f[3] == "1",
                next: cell(f[4], f[5]),
                cost_left: f[6].parse().unwrap(),
                cost_total: f[7].parse().unwrap(),
                job: f[8].to_owned(),
                cells: f[14].to_owned(),
                rice: f[15].parse().unwrap(),
            });
        }
    }
    Trace {
        header,
        lists,
        rows,
    }
}

fn strip(defs: &Arc<GameDefs>, trace: &Trace) -> (Sim, rimworld_sim::PawnId) {
    let origin = trace.cell("origin");
    let area = trace.cell("area");
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let size = GridSize::new(origin.x + area.x + 2, origin.z + area.z + 2);
    let mut map = Map::new(size, water);
    for x in 0..area.x {
        for z in 0..area.z {
            map.terrain[Cell::new(origin.x + x, origin.z + z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    sim.outdoor_temperature = trace.num("temperature");
    let colonist = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(colonist, "probe", trace.cell("pawnStart"))
        .unwrap();
    sim.set_thing_id_number(pawn, trace.num("thingIDNumber"));
    sim.set_move_speed(pawn, trace.num("moveSpeed"));
    sim.set_update_rate(pawn, trace.num("updateRate"));
    sim.set_carrying_capacity(pawn, trace.num("carryingCapacity"));
    sim.set_stat_override(pawn, "PlantWorkSpeed", trace.num("plantWorkSpeed"));
    sim.set_stat_override(pawn, "PlantHarvestYield", trace.num("plantHarvestYield"));
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    (sim, pawn)
}

fn ours_cells(sim: &Sim, defs: &GameDefs, field: &[Cell]) -> Vec<(String, f32)> {
    field
        .iter()
        .map(|&c| match sim.map.plant_at(c) {
            Some(p) => (defs.things[p.def].def_name.clone(), p.growth),
            None => ("-".to_owned(), 0.0),
        })
        .collect()
}

/// The probe's cells: `def:growth:thingIDNumber` or `-`.
fn game_cells(s: &str) -> Vec<(String, f32, Option<i32>)> {
    s.split(';')
        .map(|c| {
            let p: Vec<&str> = c.split(':').collect();
            if p.len() >= 2 {
                (
                    p[0].to_owned(),
                    p[1].parse().unwrap(),
                    p.get(2).map(|v| v.parse().unwrap()),
                )
            } else {
                (c.to_owned(), 0.0, None)
            }
        })
        .collect()
}

fn job_name(sim: &Sim, defs: &GameDefs, pawn: rimworld_sim::PawnId) -> String {
    sim.pawn(pawn)
        .unwrap()
        .job
        .as_ref()
        .and_then(|j| j.def)
        .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone())
}

fn check_movement(sim: &Sim, pawn: rimworld_sim::PawnId, row: &Row, ctx: &str) {
    let p = sim.pawn(pawn).unwrap();
    assert_eq!(p.position, row.pos, "{ctx}: position");
    if row.moving && row.cost_total != 1.0 && row.cost_left > 0.0 {
        let step = p.step.unwrap_or_else(|| panic!("{ctx}: game is mid-step"));
        assert_eq!(step.to, row.next, "{ctx}: next cell");
        assert!(
            (step.cost_left - row.cost_left).abs() < 1e-3,
            "{ctx}: cost left {} vs {}",
            step.cost_left,
            row.cost_left
        );
    }
}

#[test]
fn sowing_and_harvesting_match_original_game_traces() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut checked = 0;
    for name in ["trace_farm_sow.csv", "trace_farm_harvest.csv"] {
        let Some(path) = trace_file("farm1", name) else {
            eprintln!("skipping: no trace {name}");
            continue;
        };
        let trace = parse(&path);
        assert!(
            !trace.header.contains_key("error"),
            "{name}: probe timed out"
        );
        let (mut sim, pawn) = strip(&defs, &trace);
        let field = trace.cells("field");
        let rice = defs.things.id("Plant_Rice").unwrap();
        sim.designate_growing_zone(rice, &field).unwrap();
        if name.contains("harvest") {
            for &c in &field {
                let id = sim.map.spawn_plant(rice, c, 1.0, 85.0);
                sim.map.plant_mut(id).unwrap().sown = true;
            }
        }
        sim.set_skill(pawn, "Plants", 8);
        for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
            sim.set_work_priority(pawn, &wt, if wt == "Growing" { 3 } else { 0 });
        }
        let undraft: u64 = trace.num("undraftTick");
        sim.latitude = trace.num("latitude");
        sim.start_day_of_year =
            trace.num::<i32>("dayOfYear") - ((undraft + 15_000) / 60_000) as i32;
        sim.debug_set_tick(undraft);
        sim.debug_find_and_start_job(pawn);
        let harvest_yield: f32 = trace.num("plantHarvestYield");
        for row in &trace.rows {
            while (sim.tick_count() as i64) < row.tick {
                sim.tick();
                // Plants' thing ids come from the game's global counter.
                let next = trace
                    .rows
                    .iter()
                    .find(|r| r.tick == sim.tick_count() as i64);
                if let Some(r) = next {
                    for (c, g) in field.iter().zip(game_cells(&r.cells)) {
                        if let Some(id) = g.2 {
                            sim.debug_set_plant_id_number(*c, id);
                        }
                    }
                }
            }
            let ctx = format!("{name} tick {}", row.tick);
            assert_eq!(job_name(&sim, &defs, pawn), row.job, "{ctx}: job");
            check_movement(&sim, pawn, row, &ctx);
            for (o, g) in ours_cells(&sim, &defs, &field)
                .iter()
                .zip(game_cells(&row.cells))
            {
                let g = (g.0, g.1);
                assert_eq!(o.0, g.0, "{ctx}: plant");
                assert!((o.1 - g.1).abs() < 5e-7, "{ctx}: growth {} vs {}", o.1, g.1);
            }
            let raw = defs.things.id("RawRice").unwrap();
            let ours: u32 = sim
                .map
                .items()
                .iter()
                .filter(|i| i.def == raw)
                .map(|i| i.stack_count)
                .sum();
            if harvest_yield >= 1.0 {
                assert_eq!(ours, row.rice, "{ctx}: rice on the ground");
            } else {
                // A harvest can fail (`Rand.Value > PlantHarvestYield`); the
                // random stream is not the game's, so only whole yields of 6
                // can differ.
                assert_eq!(ours % 6, row.rice % 6, "{ctx}: rice on the ground");
            }
            checked += 1;
        }
        eprintln!("{name}: {} ticks match", trace.rows.len());
    }
    if checked == 0 {
        eprintln!("skipping: no farming traces");
    }
}

#[test]
fn walking_through_a_door_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("door1", "trace_door_walk.csv") else {
        eprintln!("skipping: no door trace");
        return;
    };
    let trace = parse(&path);
    assert!(!trace.header.contains_key("error"), "probe timed out");
    let (mut sim, pawn) = strip(&defs, &trace);
    let wood = defs.things.id("WoodLog").unwrap();
    let wall = defs.things.id("Wall").unwrap();
    let door_def = defs.things.id("Door").unwrap();
    for c in trace.cells("wallAt") {
        sim.debug_spawn_building(wall, Some(wood), c);
    }
    let d = &trace.header["door"];
    let door_cell = Cell::new(d[0].parse().unwrap(), d[1].parse().unwrap());
    sim.debug_spawn_building(door_def, Some(wood), door_cell);
    assert_eq!(
        sim.map.door_at(door_cell).unwrap().ticks_to_open,
        d[2].parse::<i32>().unwrap()
    );
    sim.debug_set_tick(trace.num::<u64>("orderTick"));
    sim.apply(Command::MoveTo {
        pawn,
        target: trace.cell("goal"),
    })
    .unwrap();
    let mut checked = 0;
    for row in &trace.rows {
        while (sim.tick_count() as i64) < row.tick {
            sim.tick();
        }
        let ctx = format!("door tick {}", row.tick);
        if row.job == "Goto" {
            check_movement(&sim, pawn, row, &ctx);
        }
        // Door state: open, ticks till fully opened, ticks until close.
        let parts: Vec<&str> = row.cells.split(':').collect();
        let door = sim.map.door_at(door_cell).unwrap();
        assert_eq!(door.open, parts[0] == "O", "{ctx}: door open");
        assert_eq!(
            door.ticks_till_fully_opened(),
            parts[1].parse::<i32>().unwrap(),
            "{ctx}: opening"
        );
        assert_eq!(
            door.ticks_until_close,
            parts[2].parse::<i32>().unwrap(),
            "{ctx}: until close"
        );
        checked += 1;
    }
    eprintln!("door: {checked} ticks match");
}

#[test]
fn sun_glow_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut checked = 0;
    for name in [
        "trace_farm_sow.csv",
        "trace_farm_harvest.csv",
        "trace_farm_harvest_haul.csv",
        "trace_farm_cut.csv",
    ] {
        let Some(path) = trace_file("farm1", name) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let trace = parse(&path);
        if !trace.header.contains_key("latitude") {
            continue;
        }
        let (mut sim, _) = strip(&defs, &trace);
        let undraft: u64 = trace.num("undraftTick");
        sim.latitude = trace.num("latitude");
        sim.start_day_of_year =
            trace.num::<i32>("dayOfYear") - ((undraft + 15_000) / 60_000) as i32;
        let mut max_err = 0.0f32;
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        {
            let f: Vec<&str> = line.split(',').collect();
            let (tick, glow): (u64, f32) = (f[0].parse().unwrap(), f[16].parse().unwrap());
            sim.debug_set_tick(tick);
            let ours = sim.outdoor_glow();
            max_err = max_err.max((ours - glow).abs());
            assert!(
                (ours - glow).abs() < 1e-6,
                "{name} tick {tick}: glow {ours} vs {glow}"
            );
            checked += 1;
        }
        eprintln!("{name}: glow max error {max_err}");
    }
    if checked == 0 {
        eprintln!("skipping: no glow traces");
    }
}

#[test]
fn sleeping_in_a_bed_and_on_the_ground_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut checked = 0;
    for name in [
        "trace_bed_sleep.csv",
        "trace_ground_sleep.csv",
        "trace_bed_wake.csv",
    ] {
        let Some(path) = trace_file("bed1", name) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let h: HashMap<String, Vec<String>> = text
            .lines()
            .filter_map(|l| l.strip_prefix('#'))
            .map(|l| {
                let p: Vec<String> = l.split(',').map(str::to_owned).collect();
                (p[0].clone(), p[1..].to_vec())
            })
            .collect();
        let num = |k: &str| -> f32 { h[k][0].parse().unwrap() };
        let cell = |k: &str| Cell::new(h[k][0].parse().unwrap(), h[k][1].parse().unwrap());
        let origin = cell("origin");
        let area = cell("area");
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
            .spawn_pawn(colonist, "probe", cell("pawnStart"))
            .unwrap();
        sim.set_thing_id_number(pawn, num("thingIDNumber") as i32);
        sim.set_move_speed(pawn, num("moveSpeed"));
        sim.set_update_rate(pawn, num("updateRate") as u32);
        for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
            sim.set_work_priority(pawn, &wt, 0);
        }
        let bed = h.contains_key("bed").then(|| {
            let b = defs.things.id("Bed").unwrap();
            let wood = defs.things.id("WoodLog").unwrap();
            sim.debug_spawn_building_rotated(b, Some(wood), cell("bed"), rimworld_sim::Rot4::North)
        });
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.debug_set_need(pawn, NeedKind::Rest, num("restStart"));
        let undraft = num("undraftTick") as u64;
        sim.debug_set_tick(undraft);
        sim.debug_find_and_start_job(pawn);
        // The wake scenario tops rest up right after the tick it fell
        // asleep, then follows the pawn until it wanders off (random).
        let boost = h
            .get("restBoost")
            .map(|v| -> (u64, f32) { (v[0].parse().unwrap(), v[1].parse().unwrap()) });
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        {
            let f: Vec<&str> = line.split(',').collect();
            let tick: u64 = f[0].parse().unwrap();
            while sim.tick_count() < tick {
                if let Some((at, level)) = boost
                    && sim.tick_count() == at
                {
                    sim.debug_set_need(pawn, NeedKind::Rest, level);
                }
                sim.tick();
            }
            let ctx = format!("{name} tick {tick}");
            let p = sim.pawn(pawn).unwrap();
            assert_eq!(
                p.position,
                Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
                "{ctx}: position"
            );
            let ours = job_name(&sim, &defs, pawn);
            if f[8].contains("Wander") {
                // Walking off or waiting first depends on a random spot.
                assert!(ours.contains("Wander"), "{ctx}: job {ours} vs {}", f[8]);
            } else {
                assert_eq!(ours, f[8], "{ctx}: job");
            }
            assert_eq!(p.asleep, f[10] == "1", "{ctx}: asleep");
            let rest = p.needs.rest_level().unwrap();
            let game: f32 = f[11].parse().unwrap();
            assert!((rest - game).abs() < 1e-5, "{ctx}: rest {rest} vs {game}");
            if let Some(b) = bed {
                assert_eq!(p.owned_bed == Some(b), f[12] == "1", "{ctx}: owner");
            }
            checked += 1;
            // Getting up on the same tick is checked; where it wanders is
            // random.
            if f[8].contains("Wander") {
                break;
            }
        }
        eprintln!("{name}: matches");
    }
    if checked == 0 {
        eprintln!("skipping: no bed traces");
    }
}

#[test]
fn melee_approach_and_swing_timing_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("melee1", "trace_melee_duel.csv") else {
        eprintln!("skipping: no melee trace");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let h: HashMap<String, Vec<String>> = text
        .lines()
        .filter_map(|l| l.strip_prefix('#'))
        .map(|l| {
            let p: Vec<String> = l.split(',').map(str::to_owned).collect();
            (p[0].clone(), p[1..].to_vec())
        })
        .collect();
    let num = |k: &str| -> f32 { h[k][0].parse().unwrap() };
    let cell = |k: &str| Cell::new(h[k][0].parse().unwrap(), h[k][1].parse().unwrap());
    let (origin, area) = (cell("origin"), cell("area"));
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
    let a = sim
        .spawn_pawn(colonist, "attacker", cell("pawnStart"))
        .unwrap();
    let b = sim
        .spawn_pawn(colonist, "target", cell("targetAt"))
        .unwrap();
    sim.set_thing_id_number(a, num("thingIDNumber") as i32);
    sim.set_thing_id_number(b, num("targetIDNumber") as i32);
    sim.set_move_speed(a, num("moveSpeed"));
    sim.set_update_rate(a, num("updateRate") as u32);
    sim.set_skill(a, "Melee", 12);
    // The drafted target stands still.
    let wait = defs.jobs.id("Wait_Combat");
    sim.debug_start_job(
        b,
        rimworld_sim::Job {
            def: wait,
            kind: rimworld_sim::JobKind::Wait {
                expiry_interval: 1_000_000,
            },
            forced: true,
            urgency: rimworld_sim::path::LocomotionUrgency::Jog,
            start_tick: 0,
        },
    );
    let order = num("orderTick") as u64;
    sim.debug_set_tick(order);
    assert!(sim.order_melee_attack(a, b));
    // Game swing ticks: the first one after arriving, then every cooldown
    // (only hits change the target, but every swing restarts the cooldown).
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    let first_hit: u64 = rows
        .iter()
        .find_map(|f| f[9].parse::<i64>().ok().filter(|&v| v > 0))
        .expect("the game hit at least once") as u64;
    let downed_at: u64 = rows
        .iter()
        .find(|f| f[12] == "1")
        .map_or(u64::MAX, |f| f[0].parse().unwrap());
    let mut swings: Vec<u64> = Vec::new();
    for f in &rows {
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.tick();
            for _ in sim.take_swings() {
                swings.push(sim.tick_count());
            }
        }
        if tick < first_hit {
            let p = sim.pawn(a).unwrap();
            let ctx = format!("melee tick {tick}");
            assert_eq!(
                p.position,
                Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
                "{ctx}: position"
            );
            let (left, total): (f32, f32) = (f[6].parse().unwrap(), f[7].parse().unwrap());
            if f[3] == "1" && total != 1.0 && left > 0.0 {
                let step = p.step.unwrap_or_else(|| panic!("{ctx}: game is mid-step"));
                assert!(
                    (step.cost_left - left).abs() < 1e-3,
                    "{ctx}: cost left {} vs {left}",
                    step.cost_left
                );
            }
        }
    }
    // Every swing of ours before the target went down falls on the game's
    // cadence: the first hit's tick plus whole cooldowns.
    let cooldown = (2.0 * num("meleeCooldownFactor") * 60.0).round() as u64;
    let ours: Vec<u64> = swings.iter().copied().filter(|&t| t < downed_at).collect();
    eprintln!("game first hit {first_hit}, cooldown {cooldown}, our swings {ours:?}");
    assert_eq!(ours.first().copied(), Some(first_hit), "first swing tick");
    for (n, t) in ours.iter().enumerate() {
        assert_eq!(*t, first_hit + n as u64 * cooldown, "swing {n}");
    }
}

#[test]
fn plant_growth_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("farm1", "trace_farm_grow.csv") else {
        eprintln!("skipping: no growth trace");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let trace = parse(&path);
    let (mut sim, _pawn) = strip(&defs, &trace);
    let field = trace.cells("field");
    let rice = defs.things.id("Plant_Rice").unwrap();
    sim.designate_growing_zone(rice, &field).unwrap();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    // The plants as the game spawned them (growth 0.1, sown) with their ids.
    for (c, g) in field.iter().zip(game_cells(rows[0][14])) {
        let id = sim.map.spawn_plant(rice, *c, g.1, 85.0);
        sim.map.plant_mut(id).unwrap().sown = true;
        sim.debug_set_plant_id_number(*c, g.2.unwrap());
    }
    let undraft: u64 = trace.num("undraftTick");
    sim.latitude = trace.num("latitude");
    sim.start_day_of_year = trace.num::<i32>("dayOfYear") - ((undraft + 15_000) / 60_000) as i32;
    sim.debug_set_tick(rows[0][0].parse().unwrap());
    let mut steps = 0;
    let mut last: Vec<f32> = Vec::new();
    for f in &rows {
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            // The cell's temperature as the game had it that tick (the
            // field's room temperature lags the outdoor temperature; rooms'
            // temperatures are not modelled).
            let factors: Vec<&str> = f[19].split(';').collect();
            sim.outdoor_temperature = factors
                .get(3)
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| f[18].parse().unwrap());
            sim.tick();
        }
        let game = game_cells(f[14]);
        let ours = ours_cells(&sim, &defs, &field);
        for (o, g) in ours.iter().zip(&game) {
            let err = (o.1 - g.1).abs();
            assert!(err < 5e-7, "grow tick {tick}: growth {} vs {}", o.1, g.1);
        }
        let now: Vec<f32> = game.iter().map(|g| g.1).collect();
        if !last.is_empty() && now != last {
            steps += 1;
        }
        last = now;
    }
    eprintln!("growth: {} rows, {steps} growth steps match", rows.len());
    assert!(steps >= 4);
}

#[test]
fn cleaning_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("clean1", "trace_clean_dirt.csv") else {
        eprintln!("skipping: no cleaning trace");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let mut h: HashMap<String, Vec<String>> = HashMap::new();
    let mut filth: Vec<(Cell, u32)> = Vec::new();
    for l in text.lines().filter_map(|l| l.strip_prefix('#')) {
        let p: Vec<String> = l.split(',').map(str::to_owned).collect();
        if p[0] == "filth" {
            filth.push((
                Cell::new(p[1].parse().unwrap(), p[2].parse().unwrap()),
                p[3].parse().unwrap(),
            ));
        } else {
            h.insert(p[0].clone(), p[1..].to_vec());
        }
    }
    assert!(!h.contains_key("error"), "probe timed out");
    let num = |k: &str| -> f32 { h[k][0].parse().unwrap() };
    let cell = |k: &str| Cell::new(h[k][0].parse().unwrap(), h[k][1].parse().unwrap());
    let (origin, area) = (cell("origin"), cell("area"));
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let mut map = Map::new(
        GridSize::new(origin.x + area.x + 2, origin.z + area.z + 2),
        water,
    );
    for x in 0..area.x {
        for z in 0..area.z {
            let c = Cell::new(origin.x + x, origin.z + z);
            map.terrain[c] = soil;
            map.set_home(c, true);
        }
    }
    let dirt = defs.things.id("Filth_Dirt").unwrap();
    for &(c, th) in &filth {
        // The probe dated the filth 5000 ticks back (past the 600-tick rule).
        let id = map.spawn_filth(dirt, c, th, 0);
        map.set_filth_grow_tick(id, -5_000);
    }
    let mut sim = Sim::new(defs.clone(), map);
    let colonist = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(colonist, "probe", cell("pawnStart"))
        .unwrap();
    sim.set_thing_id_number(pawn, num("thingIDNumber") as i32);
    sim.set_move_speed(pawn, num("moveSpeed"));
    sim.set_update_rate(pawn, num("updateRate") as u32);
    sim.set_stat_override(pawn, "CleaningSpeed", num("cleaningSpeed"));
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Cleaning" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    sim.debug_set_tick(num("undraftTick") as u64);
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
        let ctx = format!("clean tick {tick}");
        let row = Row {
            tick: tick as i64,
            pos: Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
            moving: f[3] == "1",
            next: Cell::new(f[4].parse().unwrap(), f[5].parse().unwrap()),
            cost_left: f[6].parse().unwrap(),
            cost_total: f[7].parse().unwrap(),
            job: f[8].to_owned(),
            cells: String::new(),
            rice: 0,
        };
        assert_eq!(job_name(&sim, &defs, pawn), row.job, "{ctx}: job");
        check_movement(&sim, pawn, &row, &ctx);
        let game: Vec<u32> = f[10].split(';').map(|v| v.parse().unwrap()).collect();
        for ((c, _), g) in filth.iter().zip(&game) {
            let ours = sim
                .map
                .items_at(*c)
                .find(|i| i.is_filth())
                .map_or(0, |i| i.thickness);
            assert_eq!(ours, *g, "{ctx}: filth at {c:?}");
        }
        checked += 1;
    }
    eprintln!("cleaning: {checked} ticks match");
}

#[test]
fn harvesting_then_hauling_to_a_stockpile_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let name = "trace_farm_harvest_haul.csv";
    let Some(path) = trace_file("farm1", name) else {
        eprintln!("skipping: no trace {name}");
        return;
    };
    let trace = parse(&path);
    // The probe ran on into idle wandering (random); replay up to it.
    let rice_at: HashMap<i64, String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            (f[0].parse().unwrap(), f[f.len() - 1].to_owned())
        })
        .collect();
    let (mut sim, pawn) = strip(&defs, &trace);
    let field = trace.cells("field");
    let stockpile: Vec<Cell> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("#stockpile,"))
        .map(|v| {
            let (x, z) = v.split_once(',').unwrap();
            Cell::new(x.parse().unwrap(), z.parse().unwrap())
        })
        .collect();
    let rice = defs.things.id("Plant_Rice").unwrap();
    let raw = defs.things.id("RawRice").unwrap();
    sim.designate_growing_zone(rice, &field).unwrap();
    sim.designate_stockpile(&stockpile).unwrap();
    for &c in &field {
        let id = sim.map.spawn_plant(rice, c, 1.0, 85.0);
        sim.map.plant_mut(id).unwrap().sown = true;
    }
    sim.set_skill(pawn, "Plants", 8);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        let on = wt == "Growing" || wt == "Hauling";
        sim.set_work_priority(pawn, &wt, if on { 3 } else { 0 });
    }
    let undraft: u64 = trace.num("undraftTick");
    sim.latitude = trace.num("latitude");
    sim.start_day_of_year = trace.num::<i32>("dayOfYear") - ((undraft + 15_000) / 60_000) as i32;
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    for row in &trace.rows {
        if row.job.contains("Wander") {
            break;
        }
        while (sim.tick_count() as i64) < row.tick {
            sim.tick();
            let next = trace
                .rows
                .iter()
                .find(|r| r.tick == sim.tick_count() as i64);
            if let Some(r) = next {
                for (c, g) in field.iter().zip(game_cells(&r.cells)) {
                    if let Some(id) = g.2 {
                        sim.debug_set_plant_id_number(*c, id);
                    }
                }
            }
        }
        let ctx = format!("{name} tick {}", row.tick);
        assert_eq!(job_name(&sim, &defs, pawn), row.job, "{ctx}: job");
        check_movement(&sim, pawn, row, &ctx);
        for (o, g) in ours_cells(&sim, &defs, &field)
            .iter()
            .zip(game_cells(&row.cells))
        {
            assert_eq!(o.0, g.0, "{ctx}: plant");
            assert!((o.1 - g.1).abs() < 5e-7, "{ctx}: growth {} vs {}", o.1, g.1);
        }
        let mut ours: Vec<(i32, i32, u32)> = sim
            .map
            .items()
            .iter()
            .filter(|i| i.def == raw)
            .map(|i| (i.position.x, i.position.z, i.stack_count))
            .collect();
        ours.sort();
        let ours = ours
            .iter()
            .map(|(x, z, n)| format!("{x}:{z}:{n}"))
            .collect::<Vec<_>>()
            .join(";");
        assert_eq!(ours, rice_at[&row.tick], "{ctx}: rice stacks");
        checked += 1;
    }
    assert!(checked > 1400, "{checked}");
    eprintln!("{name}: {checked} ticks match");
}

#[test]
fn cutting_designated_plants_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let name = "trace_farm_cut.csv";
    let Some(path) = trace_file("farm1", name) else {
        eprintln!("skipping: no trace {name}");
        return;
    };
    let trace = parse(&path);
    assert!(
        !trace.header.contains_key("error"),
        "{name}: probe timed out"
    );
    let (mut sim, pawn) = strip(&defs, &trace);
    let field = trace.cells("field");
    // The wild plants as the game spawned them, then designated for cutting.
    for (c, g) in field.iter().zip(game_cells(&trace.rows[0].cells)) {
        let def = defs.things.id(&g.0).unwrap();
        let max_hp = defs.things[def].stat("MaxHitPoints").unwrap_or(100.0);
        sim.map.spawn_plant(def, *c, g.1, max_hp);
        sim.debug_set_plant_id_number(*c, g.2.unwrap());
    }
    assert_eq!(sim.designate_cut_plants(&field), field.len());
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        let on = wt == "PlantCutting";
        sim.set_work_priority(pawn, &wt, if on { 3 } else { 0 });
    }
    let undraft: u64 = trace.num("undraftTick");
    sim.latitude = trace.num("latitude");
    sim.start_day_of_year = trace.num::<i32>("dayOfYear") - ((undraft + 15_000) / 60_000) as i32;
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    for row in &trace.rows {
        while (sim.tick_count() as i64) < row.tick {
            sim.tick();
        }
        let ctx = format!("{name} tick {}", row.tick);
        assert_eq!(job_name(&sim, &defs, pawn), row.job, "{ctx}: job");
        check_movement(&sim, pawn, row, &ctx);
        for (o, g) in ours_cells(&sim, &defs, &field)
            .iter()
            .zip(game_cells(&row.cells))
        {
            assert_eq!(o.0, g.0, "{ctx}: plant");
            assert!((o.1 - g.1).abs() < 5e-7, "{ctx}: growth {} vs {}", o.1, g.1);
        }
    }
    eprintln!("{name}: {} ticks match", trace.rows.len());
}

#[test]
fn leafless_temperatures_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("plants1", "trace_leafless.csv") else {
        eprintln!("skipping: no leafless trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let mut checked = 0;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let props = defs.things.get(f[0]).unwrap().plant.as_ref().unwrap();
        assert_eq!(props.min_growth_temperature, f[2].parse::<f32>().unwrap());
        assert_eq!(props.die_if_leafless, f[3] == "True", "{}", f[0]);
        let ours = rimworld_sim::plant::leafless_temperature(props, f[1].parse().unwrap());
        let game: f32 = f[4].parse().unwrap();
        assert!((ours - game).abs() < 1e-5, "{}: {ours} vs {game}", f[0]);
        checked += 1;
    }
    assert_eq!(checked, 8);
}

/// The chopping probe: a colonist with only PlantCutting enabled chops two
/// grown oaks designated for harvest (46 wood each); each leaves a stump.
#[test]
fn chopping_designated_trees_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("chop1", "trace_chop.csv") else {
        eprintln!("skipping: no chopping trace");
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
    sim.debug_set_sky_glow(Some(1.0));
    let oak = defs.things.id("Plant_TreeOak").unwrap();
    let trees: Vec<Cell> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#tree,"))
        .map(|l| {
            let v: Vec<String> = l.split(',').map(str::to_owned).collect();
            let c = cell(&v);
            let id = sim.map.spawn_plant(oak, c, v[3].parse().unwrap(), 300.0);
            sim.debug_set_plant_id_number(c, v[2].parse().unwrap());
            let _ = id;
            c
        })
        .collect();
    sim.debug_refresh_path_grid();
    assert_eq!(sim.designate_harvest_plants(&trees), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    sim.set_stat_override(
        pawn,
        "PlantWorkSpeed",
        header("plantWorkSpeed")[0].parse().unwrap(),
    );
    sim.set_stat_override(
        pawn,
        "PlantHarvestYield",
        header("plantHarvestYield")[0].parse().unwrap(),
    );
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "PlantCutting" { 3 } else { 0 });
    }
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let wood = defs.things.id("WoodLog").unwrap();
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
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
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
        let ours: Vec<&str> = trees
            .iter()
            .map(|&c| {
                if sim.map.plant_at(c).is_some() {
                    "T"
                } else {
                    "-"
                }
            })
            .collect();
        assert_eq!(ours.join(";"), f[12], "{ctx}: trees");
        let ours_wood: u32 = sim
            .map
            .items()
            .iter()
            .filter(|i| i.def == wood)
            .map(|i| i.stack_count)
            .sum();
        assert_eq!(ours_wood.to_string(), f[13], "{ctx}: wood");
        checked += 1;
    }
    let stump = defs.things.id("ChoppedStump").unwrap();
    for &c in &trees {
        assert_eq!(
            sim.map.plant_at(c).map(|p| p.def),
            Some(stump),
            "stump at {c:?}"
        );
    }
    assert!(checked > 2_900, "{checked}");
    eprintln!("chopping: {checked} ticks match");
}
