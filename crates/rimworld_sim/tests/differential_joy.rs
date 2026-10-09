//! Differential replays of the recreation traces recorded by the external
//! research run (local, gitignored, `local/research/traces/joy2/`): walk
//! waypoints (`WalkPathFinder`), the walk and skygaze givers' choices and
//! random draws from fixed seeds, and 95-tick walk and skygaze runs with
//! joy and tolerance bits. Skipped without an install or the traces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JoyActivity;
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

fn trace(mode: &str) -> Option<String> {
    let base = std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        });
    std::fs::read_to_string(base.join("joy2").join(mode).join("trace.txt")).ok()
}

/// `(x, 0, z)` cells, `;`-separated.
fn cells(s: &str) -> Vec<Cell> {
    s.split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let v: Vec<i32> = p
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .split(',')
                .map(|n| n.trim().parse().unwrap())
                .collect();
            Cell::new(v[0], v[2])
        })
        .collect()
}

/// The value after `key=` up to the next space (or a parenthesised cell).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!(" {key}="))? + key.len() + 2;
    let rest = &line[at..];
    if rest.starts_with('(') {
        let end = rest.find(')')? + 1;
        return Some(&rest[..end]);
    }
    Some(rest.split(' ').next().unwrap_or(rest))
}

fn bits(v: &str) -> u32 {
    u32::from_str_radix(v.split('@').nth(1).unwrap(), 16).unwrap()
}

/// The research runs' map: 250×250, cleared, unroofed; a colonist with the
/// recorded id, update rate and move speed. The runs jumped the clock to
/// tick 120000 after one setup tick: 120000 itself is not simulated and
/// the pawn's interval accumulator holds 1.
fn setup(defs: &Arc<GameDefs>, text: &str, at: Cell) -> (Sim, rimworld_sim::pawn::PawnId) {
    let concrete = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(250, 250), concrete));
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 21.0;
    let line = text.lines().find(|l| l.contains("SETUP_COMPLETE")).unwrap();
    let id: i32 = field(line, "id").unwrap().parse().unwrap();
    let update: u32 = field(line, "update").unwrap().parse().unwrap();
    let speed = f32::from_bits(bits(field(line, "speed").unwrap()));
    sim.debug_set_expectation(field(line, "expectation"));
    sim.debug_set_tick(120_000);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "Joy", at).unwrap();
    sim.set_thing_id_number(pawn, id);
    sim.set_update_rate(pawn, update);
    sim.debug_set_tick_delta(pawn, 1);
    sim.set_move_speed(pawn, speed);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, 0);
    }
    (sim, pawn)
}

/// Fixture C: six waypoint routes (five open, one past a wall), no draws.
#[test]
fn walk_paths_match_the_original_game() {
    let (Some(defs), Some(text)) = (real_defs(), trace("paths")) else {
        eprintln!("skipping: no install or paths trace");
        return;
    };
    let (mut sim, _) = setup(&defs, &text, Cell::new(110, 110));
    let mut checked = 0;
    for line in text.lines().filter(|l| l.contains(" PATH root=")) {
        let root = cells(field(line, "root").unwrap())[0];
        let theirs = cells(line.split("cells=").nth(1).unwrap());
        assert_eq!(
            sim.debug_walk_path(root).map(|p| p.to_vec()),
            Some(theirs),
            "{root:?}"
        );
        checked += 1;
    }
    // The obstacle route: a wooden wall at x 115, z 104..116.
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    for z in 104..=116 {
        sim.debug_spawn_building(wall, wood, Cell::new(115, z));
    }
    let line = text.lines().find(|l| l.contains("OBSTACLE_PATH")).unwrap();
    let theirs = cells(line.split("cells=").nth(1).unwrap());
    assert_eq!(
        sim.debug_walk_path(Cell::new(110, 110)).map(|p| p.to_vec()),
        Some(theirs),
        "obstacle"
    );
    assert_eq!(checked, 5);
}

/// Fixtures D/G: the walk and skygaze givers from fixed seeds — the
/// chosen cells and the random stream's counter afterwards.
#[test]
fn joy_givers_match_the_original_game() {
    let (Some(defs), Some(text)) = (real_defs(), trace("generate")) else {
        eprintln!("skipping: no install or generate trace");
        return;
    };
    let (mut sim, pawn) = setup(&defs, &text, Cell::new(110, 110));
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    let lines: Vec<&str> = text.lines().collect();
    let mut checked = 0;
    for (k, line) in lines.iter().enumerate() {
        let Some(rest) = line.split("RNG_BEFORE ").nth(1) else {
            continue;
        };
        let seed: i32 = rest.split(':').next().unwrap().parse().unwrap();
        let giver = field(lines[k + 1], "giver").unwrap();
        let theirs = cells(
            lines[k + 2]
                .split("path=")
                .nth(1)
                .unwrap()
                .split(" urgency")
                .next()
                .unwrap(),
        );
        let counter: u32 = lines[k + 3]
            .split("RNG_AFTER ")
            .nth(1)
            .unwrap()
            .split(':')
            .nth(1)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let (ours, our_counter) = sim.debug_joy_giver(pawn, giver, seed);
        assert_eq!(ours, Some(theirs), "{giver} seed {seed}");
        assert_eq!(our_counter, counter, "{giver} seed {seed}: draws");
        checked += 1;
    }
    assert_eq!(checked, 4);
}

/// Replays a 95-tick run: the job starts at tick 120000, then position,
/// job, joy and tolerance bits after every tick.
fn replay(mode: &str, def: &str, activity: impl Fn(Cell) -> JoyActivity) -> usize {
    let (Some(defs), Some(text)) = (real_defs(), trace(mode)) else {
        eprintln!("skipping: no install or {mode} trace");
        return 0;
    };
    let start = text.lines().find(|l| l.contains(" STARTED ")).unwrap();
    let at = cells(field(start, "pos").unwrap())[0];
    let (mut sim, pawn) = setup(&defs, &text, at);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(
        pawn,
        NeedKind::Joy,
        f32::from_bits(bits(field(start, "joy").unwrap())),
    );
    sim.debug_start_joy(pawn, def, activity(at));
    let mut checked = 0;
    for line in text.lines().filter(|l| l.contains(" TICK ")) {
        let tick: u64 = field(line, "tick").unwrap().parse().unwrap();
        while sim.tick_count() < tick {
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.tick();
        }
        let ctx = format!("{mode} tick {tick}");
        let p = sim.pawn(pawn).unwrap();
        assert_eq!(
            p.position,
            cells(field(line, "pos").unwrap())[0],
            "{ctx}: position"
        );
        let job = p
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
        assert_eq!(job, field(line, "job").unwrap(), "{ctx}: job");
        let joy = p.needs.get(NeedKind::Joy).unwrap().level;
        assert_eq!(
            joy.to_bits(),
            bits(field(line, "joy").unwrap()),
            "{ctx}: joy {joy}"
        );
        let tol = p.needs.joy.tolerance("Meditative");
        assert_eq!(
            tol.to_bits(),
            bits(field(line, "tol").unwrap()),
            "{ctx}: tolerance {tol}"
        );
        checked += 1;
    }
    checked
}

/// Fixture E: a walk to (114,110), then (114,114), then back.
#[test]
fn a_walk_matches_the_original_game() {
    let n = replay("walk", "GoForWalk", |_| {
        let mut path = [Cell::new(0, 0); 10];
        path[0] = Cell::new(114, 110);
        path[1] = Cell::new(114, 114);
        path[2] = Cell::new(110, 110);
        JoyActivity::Walk {
            path,
            len: 3,
            next: 0,
        }
    });
    eprintln!("walk: {n} ticks match");
}

/// Fixture H: skygazing on the pawn's own cell.
#[test]
fn skygazing_matches_the_original_game() {
    let n = replay("sky", "Skygaze", |at| JoyActivity::Skygaze { cell: at });
    eprintln!("skygaze: {n} ticks match");
}
