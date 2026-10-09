//! Differential runtime tests: replay movement traces recorded in the
//! original game (by the local diagnostic mod, docs/research.md §14) and
//! compare our simulation tick by tick.
//!
//! The traces are game-derived research artifacts and stay out of the
//! repository: they are read from `local/research/traces/` (or the directory
//! in `FERROCOLONY_TRACES`). Without them the test is skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_defs::xml::ActivePackages;
use rimworld_defs::{GameDefs, load_documents};
use rimworld_sim::path::{LocomotionUrgency, MoveCosts};
use rimworld_sim::{Cell, GridSize, Map, Sim};

struct Row {
    tick: i64,
    x: i32,
    z: i32,
    next_x: i32,
    next_z: i32,
    cost_left: f32,
    cost_total: f32,
    job: String,
    /// Per-tick MoveSpeed, when the trace records it (run 3 onwards).
    move_speed: Option<f32>,
}

struct Trace {
    name: String,
    header: HashMap<String, Vec<String>>,
    rows: Vec<Row>,
}

impl Trace {
    fn get(&self, key: &str) -> &str {
        &self.header[key][0]
    }
    fn cell(&self, key: &str) -> Cell {
        let v = &self.header[key];
        Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap())
    }
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
            .is_some_and(|n| n.to_string_lossy().starts_with("trace_"))
        {
            out.push(p);
        }
    }
}

fn load_traces() -> Vec<Trace> {
    let mut files = Vec::new();
    collect(&traces_dir(), &mut files);
    // Only traces of a moving pawn (others, e.g. health, log no position).
    files.retain(|p| {
        std::fs::read_to_string(p).is_ok_and(|t| t.lines().any(|l| l.starts_with("tick,x,z,")))
    });
    let mut traces: Vec<Trace> = files.iter().map(|p| parse(p)).collect();
    traces.sort_by(|a, b| a.name.cmp(&b.name));
    traces
}

fn parse(path: &Path) -> Trace {
    let text = std::fs::read_to_string(path).unwrap();
    let mut header = HashMap::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix('#') {
            let mut parts = h.split(',').map(str::to_owned);
            let key = parts.next().unwrap();
            header.insert(key, parts.collect());
        } else if !line.starts_with("tick") && !line.is_empty() {
            let f: Vec<&str> = line.split(',').collect();
            rows.push(Row {
                tick: f[0].parse().unwrap(),
                x: f[1].parse().unwrap(),
                z: f[2].parse().unwrap(),
                next_x: f[4].parse().unwrap(),
                next_z: f[5].parse().unwrap(),
                cost_left: f[6].parse().unwrap(),
                cost_total: f[7].parse().unwrap(),
                job: f[8].to_owned(),
                move_speed: f.get(9).and_then(|v| v.parse().ok()),
            });
        }
    }
    let run = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Trace {
        name: format!("{run}/{}", path.file_stem().unwrap().to_string_lossy()),
        header,
        rows,
    }
}

/// Builds a sim reproducing the scenario: a corridor (or square, for
/// diagonal runs) of one terrain with the recorded pathCost, walled by
/// impassable terrain, and one pawn with the recorded MoveSpeed.
fn build(trace: &Trace) -> (Sim, rimworld_sim::PawnId, Cell, Cell) {
    let path_cost: u32 = trace.get("pathCost").parse().unwrap();
    let defs_xml = format!(
        r#"<Defs>
          <TerrainDef><defName>Floor</defName><pathCost>{path_cost}</pathCost></TerrainDef>
          <TerrainDef><defName>Wall</defName><passability>Impassable</passability></TerrainDef>
          <ThingDef><defName>Walker</defName><category>Pawn</category>
            <statBases><MoveSpeed>1</MoveSpeed></statBases><race /></ThingDef>
          <PawnKindDef><defName>WalkerKind</defName><race>Walker</race></PawnKindDef>
          <JobDef><defName>Goto</defName></JobDef>
          <JobDef><defName>GotoWander</defName></JobDef>
        </Defs>"#
    );
    let (db, report) = load_documents("test", &[("d.xml", &defs_xml)], &ActivePackages::default());
    assert!(report.warnings.is_empty());
    let defs = Arc::new(GameDefs::from_database(db).0);
    let length: i32 = trace.get("length").parse().unwrap();
    let diagonal = trace.get("diagonal") == "True";
    let (w, h) = (length + 1, if diagonal { length + 1 } else { 1 });
    let floor = defs.terrain.id("Floor").unwrap();
    let wall = defs.terrain.id("Wall").unwrap();
    let mut map = Map::new(GridSize::new(w + 2, h + 2), wall);
    for x in 0..w {
        for z in 0..h {
            map.terrain[Cell::new(x + 1, z + 1)] = floor;
        }
    }
    // Game coordinates -> ours.
    let origin = trace.cell("start");
    let to_ours = |c: Cell| Cell::new(c.x - origin.x + 1, c.z - origin.z + 1);
    let start = to_ours(origin);
    let mut sim = Sim::new(defs.clone(), map);
    let kind = defs.pawn_kinds.id("WalkerKind").unwrap();
    let pawn = sim.spawn_pawn(kind, "probe", start).unwrap();
    let speed: f32 = trace.get("moveSpeed").parse().unwrap();
    sim.set_move_speed(pawn, speed);
    (sim, pawn, start, Cell::new(1 - origin.x, 1 - origin.z))
}

#[test]
fn movement_matches_original_game_traces() {
    let traces = load_traces();
    if traces.is_empty() {
        eprintln!("skipping: no traces in {}", traces_dir().display());
        return;
    }
    let mut checked = 0;
    // Movement scenarios (the eating traces are replayed separately).
    for trace in traces.iter().filter(|t| t.header.contains_key("length")) {
        let (mut sim, pawn, _start, shift) = build(trace);
        let to_ours = |x: i32, z: i32| Cell::new(x + shift.x, z + shift.z);

        // Ticks per move from the recorded MoveSpeed must equal the game's.
        let costs = MoveCosts::from_move_speed(trace.get("moveSpeed").parse().unwrap());
        let game_card: f32 = trace.get("ticksPerMoveCardinal").parse().unwrap();
        let game_diag: f32 = trace.get("ticksPerMoveDiagonal").parse().unwrap();
        assert_eq!(costs.cardinal, game_card, "{}: cardinal ticks", trace.name);
        assert_eq!(costs.diagonal, game_diag, "{}: diagonal ticks", trace.name);

        let urgency = match trace.get("urgency") {
            "Walk" => LocomotionUrgency::Walk,
            _ => LocomotionUrgency::Jog,
        };
        let job = trace.rows[0].job.clone();
        // Run on the game's absolute ticks with the recorded thing id, so
        // hash-staggered timing (e.g. the path re-check) lines up.
        let start_tick: i64 = trace.get("startTick").parse().unwrap();
        sim.set_thing_id_number(pawn, trace.get("thingIDNumber").parse().unwrap());
        sim.debug_set_tick(start_tick as u64);
        let target = to_ours(trace.cell("target").x, trace.cell("target").z);
        sim.debug_start_goto(pawn, target, urgency, urgency == LocomotionUrgency::Walk)
            .unwrap();
        let mut first_payment = None;
        let mut carries = 0;
        let mut prev_left: Option<f32> = None;
        for row in trace.rows.iter().take_while(|r| r.job == job) {
            let rel = row.tick - start_tick;
            // The game re-reads MoveSpeed when it sets up each cell; apply the
            // recorded value for the tick being simulated.
            if let Some(speed) = row.move_speed {
                sim.set_move_speed(pawn, speed);
            }
            while (sim.tick_count() as i64) < row.tick {
                sim.tick();
            }
            let p = sim.pawn(pawn).unwrap();
            let ctx = format!("{} tick +{rel}", trace.name);
            assert_eq!(p.position, to_ours(row.x, row.z), "{ctx}: position");
            let game_has_step = !(row.cost_total == 1.0 && row.cost_left == 0.0);
            match p.step {
                None => assert!(!game_has_step, "{ctx}: game is mid-step"),
                Some(step) => {
                    assert!(game_has_step, "{ctx}: we are mid-step, game is not");
                    assert_eq!(step.to, to_ours(row.next_x, row.next_z), "{ctx}: next cell");
                    assert_eq!(step.cost_total, row.cost_total, "{ctx}: cell cost");
                    assert!(
                        (step.cost_left - row.cost_left).abs() < 1e-4,
                        "{ctx}: cost left {} vs game {}",
                        step.cost_left,
                        row.cost_left
                    );
                    if first_payment.is_none() {
                        first_payment = Some(rel);
                    }
                    // Entering a new cell: remaining = max(total + overshoot, 1)
                    // with overshoot = previous remaining - 1 (<= 0). Check the
                    // game obeys this on every transition.
                    if let Some(pl) = prev_left
                        && row.cost_left > pl
                    {
                        let expected = (row.cost_total + (pl - 1.0).min(0.0)).max(1.0);
                        assert!(
                            (row.cost_left - expected).abs() < 1e-4,
                            "{ctx}: game carry {} vs rule {}",
                            row.cost_left,
                            expected
                        );
                        if pl < 1.0 - 1e-6 {
                            carries += 1; // a fractional overshoot was carried
                        }
                    }
                }
            }
            prev_left = game_has_step.then_some(row.cost_left);
        }
        // First-step delay: the first payment is two ticks after the start.
        let game_first_payment = trace
            .rows
            .iter()
            .find(|r| !(r.cost_total == 1.0 && r.cost_left == 0.0))
            .map(|r| r.tick - start_tick);
        assert_eq!(
            first_payment, game_first_payment,
            "{}: first payment tick",
            trace.name
        );
        let delay = first_payment.unwrap();
        assert!(
            delay == 2 || delay == 3,
            "{}: first-step delay {delay}",
            trace.name
        );
        // Arrival: the first recorded tick where the game pawn stands on the
        // target (the job may already have changed on that tick).
        let arrival = trace
            .rows
            .iter()
            .find(|r| to_ours(r.x, r.z) == target)
            .expect("game pawn arrived")
            .tick
            - start_tick;
        while (sim.tick_count() as i64) < start_tick + arrival - 1 {
            sim.tick();
        }
        assert_ne!(
            sim.pawn(pawn).unwrap().position,
            target,
            "{}: arrived early",
            trace.name
        );
        sim.tick();
        assert_eq!(
            sim.pawn(pawn).unwrap().position,
            target,
            "{}: arrival tick",
            trace.name
        );
        eprintln!(
            "{}: first step +{delay}, arrival +{arrival} matched, every tick compared, carry on {carries} cells",
            trace.name
        );
        checked += 1;
    }
    assert!(
        checked >= 21,
        "expected all recorded scenarios, got {checked}"
    );
}
