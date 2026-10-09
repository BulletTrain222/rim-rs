//! Differential runtime test for construction: replays wall-building
//! traces recorded in the original game by the local diagnostic mod
//! (resource delivery through HaulToContainer, the frame, FinishFrame) and
//! compares our simulation tick by tick.
//!
//! Needs the game-derived traces (`local/research/traces/`, or
//! `FERROCOLONY_TRACES`) and a RimWorld installation for the real Defs;
//! skipped when either is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::map::ConstructStage;
use rimworld_sim::needs::NeedKind;
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
            .is_some_and(|n| n.to_string_lossy().starts_with("trace_build_"))
        {
            out.push(p);
        }
    }
}

struct Row {
    tick: i64,
    pos: Cell,
    moving: bool,
    next: Cell,
    cost_left: f32,
    cost_total: f32,
    job: String,
    carried_count: u32,
    sites: Vec<String>,
}

struct Trace {
    name: String,
    header: HashMap<String, Vec<String>>,
    items: Vec<(String, Cell, u32)>,
    walls: Vec<Cell>,
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
    let (mut header, mut items, mut walls, mut rows) =
        (HashMap::new(), Vec::new(), Vec::new(), Vec::new());
    let cell = |x: &str, z: &str| Cell::new(x.parse().unwrap(), z.parse().unwrap());
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('#') {
            let p: Vec<String> = h.split(',').map(str::to_owned).collect();
            match p[0].as_str() {
                "item" => items.push((p[1].clone(), cell(&p[2], &p[3]), p[4].parse().unwrap())),
                "wall" => walls.push(cell(&p[1], &p[2])),
                _ => {
                    header.insert(p[0].clone(), p[1..].to_vec());
                }
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
                carried_count: f[11].parse().unwrap(),
                sites: f[19].split(';').map(str::to_owned).collect(),
            });
        }
    }
    Trace {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        header,
        items,
        walls,
        rows,
    }
}

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
    for (def, at, count) in &trace.items {
        sim.spawn_item(defs.things.id(def).unwrap(), *at, *count);
    }
    let wall = defs.things.id("Wall").unwrap();
    let stuff = defs.things.id(trace.get("stuff")).unwrap();
    for &w in &trace.walls {
        sim.place_blueprint(wall, Some(stuff), w).unwrap();
    }
    sim.set_thing_id_number(pawn, trace.num("thingIDNumber"));
    sim.set_move_speed(pawn, trace.num("moveSpeed"));
    sim.set_update_rate(pawn, trace.num("updateRate"));
    sim.set_carrying_capacity(pawn, trace.num("carryingCapacity"));
    sim.set_stat_override(pawn, "ConstructionSpeed", trace.num("constructionSpeed"));
    sim.set_stat_override(pawn, "ConstructSuccessChance", trace.num("successChance"));
    sim.set_skill(pawn, "Construction", 20);
    // The probe leaves only Construction enabled and fills the needs.
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    (sim, pawn)
}

/// Our state of a wall site in the probe's notation.
fn site(sim: &Sim, defs: &GameDefs, c: Cell) -> String {
    if let Some(k) = sim.map.constructible_at(c) {
        return match k.stage {
            ConstructStage::Blueprint => "B".to_owned(),
            ConstructStage::Frame => {
                let n: u32 = k.resources.iter().map(|(_, n)| n).sum();
                format!("F{n}:{}", k.work_done)
            }
        };
    }
    if sim.map.buildings[c] == defs.things.id("Wall") {
        return "W".to_owned();
    }
    "-".to_owned()
}

/// Parses a frame site into (delivered, work done).
fn frame(s: &str) -> Option<(u32, f32)> {
    let (n, w) = s.strip_prefix('F')?.split_once(':')?;
    Some((n.parse().ok()?, w.parse().ok()?))
}

#[test]
fn construction_matches_original_game_traces() {
    let mut files = Vec::new();
    collect(&traces_dir(), &mut files);
    if files.is_empty() {
        eprintln!(
            "skipping: no construction traces in {}",
            traces_dir().display()
        );
        return;
    }
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    files.sort();
    let mut checked = 0;
    for trace in files.iter().map(|p| parse(p)) {
        assert!(
            !trace.header.contains_key("error"),
            "{}: probe timed out",
            trace.name
        );
        let (mut sim, pawn) = build(&defs, &trace);
        let start: i64 = trace.num("undraftTick");
        sim.debug_set_tick(start as u64);
        sim.debug_find_and_start_job(pawn);
        for row in &trace.rows {
            while (sim.tick_count() as i64) < row.tick {
                sim.tick();
            }
            let ctx = format!("{} tick {}", trace.name, row.tick);
            let p = sim.pawn(pawn).unwrap();
            let job = p
                .job
                .as_ref()
                .and_then(|j| j.def)
                .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
            assert_eq!(job, row.job, "{ctx}: job");
            assert_eq!(p.position, row.pos, "{ctx}: position");
            assert_eq!(
                p.carried.map_or(0, |c| c.count),
                row.carried_count,
                "{ctx}: carried"
            );
            if row.moving && row.cost_total != 1.0 {
                let step = p.step.unwrap_or_else(|| panic!("{ctx}: game is mid-step"));
                assert_eq!(step.to, row.next, "{ctx}: next cell");
                assert!(
                    (step.cost_left - row.cost_left).abs() < 1e-3,
                    "{ctx}: cost left {} vs {}",
                    step.cost_left,
                    row.cost_left
                );
            }
            for (w, expect) in trace.walls.iter().zip(&row.sites) {
                let ours = site(&sim, &defs, *w);
                match (frame(expect), frame(&ours)) {
                    (Some((gn, gw)), Some((on, ow))) => {
                        assert_eq!(on, gn, "{ctx}: delivered at {w:?}");
                        assert!((ow - gw).abs() < 1e-2, "{ctx}: work at {w:?}: {ow} vs {gw}");
                    }
                    _ => assert_eq!(&ours, expect, "{ctx}: site {w:?}"),
                }
            }
            checked += 1;
        }
        eprintln!("{}: {} ticks match", trace.name, trace.rows.len());
    }
    assert!(checked > 0);
}
