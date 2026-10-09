//! Differential runtime test for eating: replays Ingest traces recorded in
//! the original game by the local diagnostic mod (docs/research.md §16) and
//! compares our simulation tick by tick.
//!
//! Needs both the game-derived traces (`local/research/traces/`, or
//! `FERROCOLONY_TRACES`) and a RimWorld installation for the real Defs;
//! skipped when either is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::needs::NeedKind;
use rimworld_sim::{Cell, GridSize, IngestStage, JobKind, Map, Rot4, Sim};

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

fn traces_dir() -> PathBuf {
    std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        })
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for p in entries.filter_map(Result::ok).map(|e| e.path()) {
        if p.is_dir() {
            collect(&p, out);
        } else if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("trace_eat_"))
        {
            out.push(p);
        }
    }
}

/// One recorded tick (columns written by the eating probe).
struct Row {
    tick: i64,
    pos: Cell,
    next: Cell,
    cost_left: f32,
    cost_total: f32,
    job: String,
    toil: i32,
    ticks_left: i32,
    carried: String,
    carried_count: u32,
    target: Cell,
    dest: Cell,
    food: f32,
    job_count: i32,
}

struct Trace {
    name: String,
    header: HashMap<String, Vec<String>>,
    /// `#item` lines: def, x, z, count.
    items: Vec<(String, Cell, u32)>,
    /// `#building` lines: def, x, z, rotation (wooden).
    buildings: Vec<(String, Cell, i32)>,
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
}

fn parse(path: &Path) -> Trace {
    let text = std::fs::read_to_string(path).unwrap();
    let mut header = HashMap::new();
    let mut items = Vec::new();
    let mut buildings = Vec::new();
    let mut rows = Vec::new();
    let cell = |x: &str, z: &str| Cell::new(x.parse().unwrap(), z.parse().unwrap());
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('#') {
            let parts: Vec<String> = h.split(',').map(str::to_owned).collect();
            if parts[0] == "item" {
                items.push((
                    parts[1].clone(),
                    cell(&parts[2], &parts[3]),
                    parts[4].parse().unwrap(),
                ));
            } else if parts[0] == "building" {
                buildings.push((
                    parts[1].clone(),
                    cell(&parts[2], &parts[3]),
                    parts[4].parse().unwrap(),
                ));
            } else {
                header.insert(parts[0].clone(), parts[1..].to_vec());
            }
        } else if !line.starts_with("tick") && !line.is_empty() {
            let f: Vec<&str> = line.split(',').collect();
            rows.push(Row {
                tick: f[0].parse().unwrap(),
                pos: cell(f[1], f[2]),
                next: cell(f[4], f[5]),
                cost_left: f[6].parse().unwrap(),
                cost_total: f[7].parse().unwrap(),
                job: f[8].to_owned(),
                toil: f[9].parse().unwrap(),
                ticks_left: f[10].parse().unwrap(),
                carried: f[11].to_owned(),
                carried_count: f[12].parse().unwrap(),
                target: cell(f[13], f[14]),
                dest: cell(f[16], f[17]),
                food: f[18].parse().unwrap(),
                job_count: f[19].parse().unwrap(),
            });
        }
    }
    Trace {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        header,
        items,
        buildings,
        rows,
    }
}

/// The walled, roofed soil area of the probe, in game coordinates.
fn build(defs: &Arc<GameDefs>, trace: &Trace) -> (Sim, rimworld_sim::PawnId) {
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
    let colonist = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(colonist, "probe", trace.cell("pawnStart"))
        .unwrap();
    let wood = defs.things.id("WoodLog");
    for (def, at, rot) in &trace.buildings {
        let id = defs.things.id(def).unwrap();
        let stuff = if defs.things[id].made_from_stuff() {
            wood
        } else {
            None
        };
        let rot = [Rot4::North, Rot4::East, Rot4::South, Rot4::West][*rot as usize];
        sim.debug_spawn_building_rotated(id, stuff, *at, rot);
    }
    for (def, at, count) in &trace.items {
        sim.spawn_item(defs.things.id(def).unwrap(), *at, *count);
    }
    sim.set_thing_id_number(pawn, trace.num("thingIDNumber"));
    sim.set_move_speed(pawn, trace.num("moveSpeed"));
    sim.set_eating_speed(pawn, trace.num("eatingSpeed"));
    sim.debug_set_need(pawn, NeedKind::Food, trace.num("foodStart"));
    // The probe fills rest so that only food matters.
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    (sim, pawn)
}

/// The game's toil index for each of our Ingest stages (tool users).
fn toil_of(stage: IngestStage) -> i32 {
    match stage {
        IngestStage::GotoFood { .. } => 4,
        IngestStage::CarryToChewSpot { .. } => 6,
        IngestStage::Chew { .. } => 8,
    }
}

#[test]
fn eating_matches_original_game_traces() {
    let mut files = Vec::new();
    collect(&traces_dir(), &mut files);
    if files.is_empty() {
        eprintln!("skipping: no eating traces in {}", traces_dir().display());
        return;
    }
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    files.sort();
    let mut checked = 0;
    for trace in files.iter().map(|p| parse(p)) {
        let (mut sim, pawn) = build(&defs, &trace);
        let start: i64 = trace.num("undraftTick");
        sim.debug_set_tick(start as u64);
        // Without a chair the chew spot is a random choice; replay the
        // game's. With one, our own search must find it.
        if trace.buildings.is_empty() {
            let spot = trace
                .rows
                .iter()
                .find(|r| r.toil == 6)
                .expect("game carried food to a spot")
                .dest;
            sim.debug_force_next_chew_spot(pawn, spot);
        }
        sim.debug_find_and_start_job(pawn);

        let ingest: Vec<&Row> = trace
            .rows
            .iter()
            .take_while(|r| r.job == "Ingest")
            .collect();
        let end_tick = trace.rows[ingest.len()].tick;
        for row in &ingest {
            while (sim.tick_count() as i64) < row.tick {
                sim.tick();
            }
            let ctx = format!("{} tick {}", trace.name, row.tick);
            let p = sim.pawn(pawn).unwrap();
            let job = p.job.as_ref().expect("has a job");
            let JobKind::Ingest {
                food, count, stage, ..
            } = job.kind
            else {
                panic!("{ctx}: not eating: {:?}", job.kind);
            };
            assert_eq!(defs.jobs[job.def.unwrap()].def_name, "Ingest");
            assert_eq!(count as i32, row.job_count, "{ctx}: job count");
            assert_eq!(toil_of(stage), row.toil, "{ctx}: toil");
            assert_eq!(p.position, row.pos, "{ctx}: position");
            match p.step {
                Some(step) => {
                    assert_eq!(step.to, row.next, "{ctx}: next cell");
                    assert_eq!(step.cost_total, row.cost_total, "{ctx}: cell cost");
                    assert!(
                        (step.cost_left - row.cost_left).abs() < 1e-4,
                        "{ctx}: cost left {} vs {}",
                        step.cost_left,
                        row.cost_left
                    );
                }
                None => assert!(
                    row.cost_total == 1.0 && row.cost_left == 0.0,
                    "{ctx}: game is mid-step"
                ),
            }
            match stage {
                IngestStage::GotoFood { dest } => {
                    // Still on the map: the food the game chose.
                    let item = sim.map.item(food).expect("food still on the map");
                    assert_eq!(item.position, row.target, "{ctx}: chosen food");
                    assert_eq!(dest, row.dest, "{ctx}: walks onto the food");
                }
                IngestStage::CarryToChewSpot { spot } => {
                    assert_eq!(spot, row.dest, "{ctx}: chew spot");
                }
                IngestStage::Chew { ticks_left } => {
                    assert_eq!(ticks_left, row.ticks_left, "{ctx}: chew ticks left");
                }
            }
            let food_level = p.needs.get(NeedKind::Food).unwrap().level;
            assert!(
                (food_level - row.food).abs() < 1e-6,
                "{ctx}: food {food_level} vs {}",
                row.food
            );
            match p.carried {
                Some(c) => {
                    assert_eq!(defs.things[c.def].def_name, row.carried, "{ctx}: carried");
                    assert_eq!(c.count, row.carried_count, "{ctx}: carried count");
                }
                None => assert_eq!(row.carried, "-", "{ctx}: game carries food"),
            }
        }
        // The job ends on the same tick, with the same food level.
        while (sim.tick_count() as i64) < end_tick {
            sim.tick();
        }
        let p = sim.pawn(pawn).unwrap();
        assert!(
            !matches!(p.job.as_ref().map(|j| j.kind), Some(JobKind::Ingest { .. })),
            "{}: still eating at {end_tick}",
            trace.name
        );
        let game_food = trace.rows[ingest.len()].food;
        let ours = p.needs.get(NeedKind::Food).unwrap().level;
        assert!(
            (ours - game_food).abs() < 1e-6,
            "{}: food after eating",
            trace.name
        );
        eprintln!(
            "{}: {} ticks compared; food after eating {ours} (game {game_food})",
            trace.name,
            ingest.len()
        );
        // Afterwards the pawn holds its posture until its next interval
        // tick, then thinks again.
        let game_next = trace.rows[ingest.len()..]
            .iter()
            .find(|r| r.job != "Wait_MaintainPosture")
            .map(|r| r.tick);
        let job_name = |sim: &Sim| {
            sim.pawn(pawn)
                .and_then(|p| p.job.as_ref())
                .and_then(|j| j.def)
                .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone())
        };
        assert_eq!(job_name(&sim), "Wait_MaintainPosture", "{}", trace.name);
        if let Some(next) = game_next {
            while (sim.tick_count() as i64) < next - 1 {
                sim.tick();
                assert_eq!(
                    job_name(&sim),
                    "Wait_MaintainPosture",
                    "{} at {}",
                    trace.name,
                    sim.tick_count()
                );
            }
            sim.tick();
            assert_ne!(
                job_name(&sim),
                "Wait_MaintainPosture",
                "{}: posture ends at {next}",
                trace.name
            );
        }
        checked += 1;
    }
    assert!(checked >= 3, "expected the three eating scenarios");
}
