//! Differential runtime test for hauling: replays the cases recorded in the
//! original game (rev591) by the hauling research diagnostic
//! (an unpublished research report; trace in `local/research/traces/haul1/`)
//! and compares our simulation with every recorded tick: position, toil,
//! job count, carried thing (identity and count), ground items (identity,
//! count and cell) and reservations. Needs the trace and a RimWorld
//! installation; skipped otherwise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{HaulStage, Job};
use rimworld_sim::reservation::Target;
use rimworld_sim::storage::StoragePriority;
use rimworld_sim::{Cell, GridSize, ItemId, JobKind, Map, PawnId, Sim};

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

fn trace_file() -> PathBuf {
    std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        })
        .join("haul1/hauling-trace.txt")
}

/// `(x, 0, z)` → cell.
fn cell(text: &str) -> Cell {
    let v: Vec<i32> = text
        .trim_matches(|c| c == '(' || c == ')')
        .split(',')
        .map(|p| p.trim().parse().unwrap())
        .collect();
    Cell::new(v[0], v[2])
}

/// Splits a list of `(x, 0, z)` cells separated by commas.
fn cells(text: &str) -> Vec<Cell> {
    text.split("),")
        .map(|p| cell(&format!("{})", p.trim_end_matches(')'))))
        .collect()
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let pat = format!("{key}=");
    let start = line
        .match_indices(&pat)
        .find(|(i, _)| *i == 0 || matches!(line.as_bytes()[i - 1], b' ' | b'|'))
        .map(|(i, _)| i + pat.len())
        .unwrap_or_else(|| panic!("no {key} in {line}"));
    let rest = &line[start..];
    let end = if rest.starts_with('(') {
        rest.find(')').map_or(rest.len(), |e| e + 1)
    } else {
        rest.find([' ', '|']).unwrap_or(rest.len())
    };
    &rest[..end]
}

/// A recorded snapshot row.
struct Row {
    tick: u64,
    pos: Cell,
    job: String,
    toil: i32,
    count: i32,
    carry: Option<(u32, String, u32)>,
    /// Ground items: (game id, def, count, cell).
    ground: Vec<(u32, String, u32, Cell)>,
    /// Reservation targets: cells and game thing ids.
    reserved: Vec<String>,
}

fn parse_row(line: &str) -> Row {
    let parts: HashMap<&str, &str> = line.split('|').filter_map(|p| p.split_once('=')).collect();
    let toil: i32 = parts["toil"].split(':').next().unwrap().parse().unwrap();
    let carry = (parts["carry"] != "null").then(|| {
        let v: Vec<&str> = parts["carry"].split(':').collect();
        (
            v[0].parse().unwrap(),
            v[1].to_owned(),
            v[2].parse().unwrap(),
        )
    });
    let ground = parts["items"]
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let v: Vec<&str> = s.splitn(4, ':').collect();
            v[3].starts_with('(').then(|| {
                (
                    v[0].parse().unwrap(),
                    v[1].to_owned(),
                    v[2].parse().unwrap(),
                    cell(v[3]),
                )
            })
        })
        .collect::<Vec<_>>();
    // Stacks inside the destination listing (split-off things are only
    // listed there).
    let mut ground = ground;
    for entry in parts["dest"].split('[').skip(1) {
        let inner = entry.split(']').next().unwrap();
        for s in inner.split(";").flat_map(|x| x.split("),")) {
            let s = s.trim().trim_start_matches(',');
            if s.is_empty() {
                continue;
            }
            let s = if s.ends_with(')') {
                s.to_owned()
            } else {
                format!("{s})")
            };
            let v: Vec<&str> = s.splitn(4, ':').collect();
            let id: u32 = v[0].parse().unwrap();
            if !ground.iter().any(|g: &(u32, String, u32, Cell)| g.0 == id) {
                ground.push((id, v[1].to_owned(), v[2].parse().unwrap(), cell(v[3])));
            }
        }
    }
    let reserved = parts["reserved"]
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let rest = s.splitn(3, ':').nth(2).unwrap();
            match rest.strip_prefix('T') {
                Some(t) => format!("T{}", t.split(':').next().unwrap()),
                None => {
                    let c = cell(rest.split(':').next().unwrap());
                    format!("({}, {})", c.x, c.z)
                }
            }
        })
        .collect();
    Row {
        tick: parts["tick"].parse().unwrap(),
        pos: cell(parts["pos"]),
        job: parts["job"].split(':').next().unwrap().to_owned(),
        toil,
        count: parts["count"].parse().unwrap(),
        carry,
        ground,
        reserved,
    }
}

/// One recorded case: its header lines and the snapshot rows.
struct Case {
    name: String,
    lines: Vec<String>,
}

fn cases(text: &str) -> Vec<Case> {
    let mut out: Vec<Case> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("CASE ") {
            out.push(Case {
                name: name.to_owned(),
                lines: Vec::new(),
            });
        } else if let Some(c) = out.last_mut() {
            // The meals run of case K starts with its own RESET.
            if line.starts_with("RESET") && c.lines.iter().any(|l| l.starts_with("RESET")) {
                let name = format!("{} (second run)", c.name);
                out.push(Case {
                    name,
                    lines: vec![line.to_owned()],
                });
            } else {
                c.lines.push(line.to_owned());
            }
        }
    }
    out
}

fn priority(name: &str) -> StoragePriority {
    match name {
        "Low" => StoragePriority::Low,
        "Normal" => StoragePriority::Normal,
        "Preferred" => StoragePriority::Preferred,
        "Important" => StoragePriority::Important,
        "Critical" => StoragePriority::Critical,
        other => panic!("priority {other}"),
    }
}

struct Replay {
    sim: Sim,
    defs: Arc<GameDefs>,
    a: PawnId,
    b: PawnId,
    /// Game thing id → ours.
    ids: HashMap<u32, ItemId>,
    jobs: HashMap<PawnId, Job>,
}

impl Replay {
    fn new(defs: Arc<GameDefs>, tick: u64) -> Self {
        let concrete = defs.terrain.id("Concrete").unwrap();
        let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(130, 130), concrete));
        // The report's runs were in daylight and log no time of day: keep
        // the light full so MoveSpeed's darkness factor stays out.
        sim.debug_set_sky_glow(Some(1.0));
        let kind = defs.pawn_kinds.id("Colonist").unwrap();
        let a = sim.spawn_pawn(kind, "A", Cell::new(100, 100)).unwrap();
        let b = sim.spawn_pawn(kind, "B", Cell::new(100, 102)).unwrap();
        sim.set_thing_id_number(a, 433);
        sim.set_thing_id_number(b, 436);
        sim.debug_set_tick(tick);
        Self {
            sim,
            defs,
            a,
            b,
            ids: HashMap::new(),
            jobs: HashMap::new(),
        }
    }

    /// One tick. Tracked dirt and trash are random outcomes of the shared
    /// stream (which we can't follow); the recorded runs left none, so any
    /// we drop is cleared.
    fn tick(&mut self) {
        self.sim.tick();
        let filth: Vec<_> = self
            .sim
            .map
            .items()
            .iter()
            .filter(|i| i.is_filth())
            .map(|i| (i.id, i.stack_count))
            .collect();
        for (id, n) in filth {
            self.sim.map.take_from_item(id, n);
        }
    }

    fn pawn_of(&self, game: &str) -> PawnId {
        if game == "433" { self.a } else { self.b }
    }

    fn item(&mut self, spec: &str) {
        let v: Vec<&str> = spec.splitn(4, ':').collect();
        let def = self.defs.things.id(v[1]).unwrap();
        let id = self.sim.spawn_item(def, cell(v[3]), v[2].parse().unwrap());
        self.ids.insert(v[0].parse().unwrap(), id);
    }

    fn our_id(&mut self, game: u32, fallback: Option<ItemId>) -> Option<ItemId> {
        if let Some(&id) = self.ids.get(&game) {
            return Some(id);
        }
        let id = fallback?;
        self.ids.insert(game, id);
        Some(id)
    }

    fn compare(&mut self, case: &str, pawn: PawnId, row: &Row) {
        let ctx = format!("{case} tick {}", row.tick);
        let p = self.sim.pawn(pawn).unwrap().clone();
        assert_eq!(p.position, row.pos, "{ctx}: position");
        let job = p.job.as_ref().expect("has a job");
        if row.job == "HaulToCell" {
            let JobKind::Haul { count, stage, .. } = job.kind else {
                panic!("{ctx}: not hauling: {:?}", job.kind)
            };
            let toil = match stage {
                HaulStage::GotoSource => 4,
                HaulStage::CarryToCell => 9,
                HaulStage::Delay => 10,
            };
            assert_eq!(toil, row.toil, "{ctx}: toil");
            assert_eq!(count, row.count, "{ctx}: job count");
        } else {
            assert_eq!(self.job_name(pawn), row.job, "{ctx}: job");
        }
        match (&row.carry, p.carried) {
            (None, None) => {}
            (Some((gid, def, n)), Some(c)) => {
                assert_eq!(&self.defs.things[c.def].def_name, def, "{ctx}: carried def");
                assert_eq!(c.count, *n, "{ctx}: carried count");
                let ours = self.our_id(*gid, Some(c.id));
                assert_eq!(ours, Some(c.id), "{ctx}: carried identity");
            }
            (game, ours) => panic!("{ctx}: carried {game:?} vs {ours:?}"),
        }
        // Ground items.
        let mut theirs: Vec<(Option<ItemId>, String, u32, Cell)> = Vec::new();
        for (gid, def, n, c) in &row.ground {
            let fallback = self
                .sim
                .map
                .items()
                .iter()
                .find(|i| i.position == *c && !self.ids.values().any(|v| *v == i.id))
                .map(|i| i.id);
            theirs.push((self.our_id(*gid, fallback), def.clone(), *n, *c));
        }
        let mut ours: Vec<(Option<ItemId>, String, u32, Cell)> = self
            .sim
            .map
            .items()
            .iter()
            .map(|i| {
                (
                    Some(i.id),
                    self.defs.things[i.def].def_name.clone(),
                    i.stack_count,
                    i.position,
                )
            })
            .collect();
        theirs.sort_by_key(|t| (t.3.x, t.3.z));
        ours.sort_by_key(|t| (t.3.x, t.3.z));
        assert_eq!(ours, theirs, "{ctx}: ground items");
        // Reservations of this pawn.
        let reverse: HashMap<ItemId, u32> = self.ids.iter().map(|(g, o)| (*o, *g)).collect();
        let mut ours: Vec<String> = self
            .sim
            .reservations
            .rows()
            .iter()
            .filter(|r| r.claimant.pawn == pawn)
            .map(|r| match r.target {
                Target::Cell(c) => format!("({}, {})", c.x, c.z),
                Target::Item(id) => format!("T{}", reverse.get(&id).copied().unwrap_or(0)),
                Target::Ceiling(c) => format!("ceiling ({}, {})", c.x, c.z),
                Target::Floor(c) => format!("floor ({}, {})", c.x, c.z),
                Target::Pawn(p) => format!("pawn {}", p.0),
            })
            .collect();
        let mut theirs = row.reserved.clone();
        ours.sort();
        theirs.sort();
        assert_eq!(ours, theirs, "{ctx}: reservations");
    }

    fn job_name(&self, pawn: PawnId) -> String {
        self.sim
            .pawn(pawn)
            .and_then(|p| p.job.as_ref())
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| self.defs.jobs[d].def_name.clone())
    }
}

fn replay(defs: &Arc<GameDefs>, case: &Case) -> usize {
    let mut r: Option<Replay> = None;
    let mut driven: Option<PawnId> = None;
    let mut compared = 0;
    let mut lines = case.lines.iter().peekable();
    while let Some(line) = lines.next() {
        let head = line.split([' ', '|']).next().unwrap();
        match head {
            "RESET" => {
                r = Some(Replay::new(
                    defs.clone(),
                    field(line, "tick").parse().unwrap(),
                ));
            }
            "ZONE" => {
                let rp = r.as_mut().unwrap();
                let (prio, list) = line["ZONE ".len()..].split_once(" cells=").unwrap();
                rp.sim
                    .map
                    .storage
                    .add_stockpile(priority(prio), &cells(list), {
                        rimworld_sim::storage::ThingFilter::preset(
                            &rp.sim.defs,
                            rimworld_sim::storage::StoragePreset::DefaultStockpile,
                        )
                    });
            }
            "ITEM" => {
                let rp = r.as_mut().unwrap();
                rp.item(&line["ITEM ".len()..]);
            }
            "FRAMEWORK" => {
                // The real work giver chooses the job.
                let rp = r.as_mut().unwrap();
                rp.sim.debug_find_and_start_job(rp.a);
                let job = rp.sim.pawn(rp.a).unwrap().job.clone().unwrap();
                let JobKind::Haul { dest, count, .. } = job.kind else {
                    panic!("{}: framework chose {:?}", case.name, job.kind)
                };
                assert_eq!(dest, cell(field(line, "B")), "{}: framework B", case.name);
                assert_eq!(count, field(line, "count").parse::<i32>().unwrap());
                driven = Some(rp.a);
            }
            "SELECT" => {
                let rp = r.as_mut().unwrap();
                let pawn = rp.pawn_of(field(line, "pawn"));
                let source = rp.ids[&field(line, "source").parse::<u32>().unwrap()];
                // The fixture's carrying capacity (case E lowers it to 40).
                rp.sim
                    .set_carrying_capacity(pawn, field(line, "cap").parse().unwrap());
                let job = rp.sim.debug_haul_job(pawn, source);
                if field(line, "job") == "null" {
                    assert!(
                        job.is_none(),
                        "{}: selection should find nothing",
                        case.name
                    );
                    continue;
                }
                let job = job.unwrap_or_else(|| panic!("{}: no job selected", case.name));
                let JobKind::Haul { dest, count, .. } = job.kind else {
                    unreachable!()
                };
                assert_eq!(dest, cell(field(line, "B")), "{}: selected B", case.name);
                assert_eq!(
                    count,
                    field(line, "count").parse::<i32>().unwrap(),
                    "{}: count",
                    case.name
                );
                rp.jobs.insert(pawn, job);
            }
            "OVERRIDE" => {
                // Case F: pawn B's preselected job gets another cell.
                let rp = r.as_mut().unwrap();
                let b = rp.b;
                if let Some(Job {
                    kind: JobKind::Haul { dest, .. },
                    ..
                }) = rp.jobs.get_mut(&b)
                {
                    *dest = Cell::new(111, 100);
                }
            }
            "START" => {
                let rp = r.as_mut().unwrap();
                let next = lines.peek().expect("started row");
                let pawn = rp.pawn_of(field(next, "pawn"));
                // Case A: the framework query already started its job.
                let started = match rp.jobs.remove(&pawn) {
                    Some(job) => rp.sim.debug_start_job(pawn, job),
                    None => true,
                };
                let row = parse_row(lines.next().unwrap());
                if row.job == "HaulToCell" {
                    assert!(started, "{}: start", case.name);
                    rp.compare(&case.name, pawn, &row);
                    compared += 1;
                    if driven.is_none() {
                        driven = Some(pawn);
                    }
                } else {
                    // A failed start (reservation conflict).
                    assert!(!started, "{}: start should fail", case.name);
                    let mine: usize = rp
                        .sim
                        .reservations
                        .rows()
                        .iter()
                        .filter(|r| r.claimant.pawn == pawn)
                        .count();
                    assert_eq!(mine, 0, "{}: no leaked reservations", case.name);
                }
            }
            "tick" => {
                let rp = r.as_mut().unwrap();
                let row = parse_row(line);
                while rp.sim.tick_count() < row.tick {
                    rp.tick();
                }
                rp.compare(&case.name, driven.unwrap(), &row);
                compared += 1;
            }
            "MUTATE" => {
                let rp = r.as_mut().unwrap();
                if line.contains("disallow steel") {
                    let steel = rp.defs.things.id("Steel").unwrap();
                    let zone = rp.sim.map.storage.zone_at(Cell::new(110, 100)).unwrap();
                    rp.sim.map.storage.set_allowed(zone, steel, false);
                } else if line.contains("ordered Wait") {
                    let pawn = driven.unwrap();
                    rp.sim.debug_interrupt_with_wait(pawn);
                    assert!(rp.sim.pawn(pawn).unwrap().carried.is_none());
                    let items = rp.sim.map.items_at(Cell::new(102, 100)).count();
                    assert_eq!(items, 1, "{}: dropped at the pawn", case.name);
                    assert!(rp.sim.reservations.rows().is_empty());
                }
            }
            "FINISH" => {
                if field(line, "condition") == "InterruptForced" {
                    continue; // the fixture's own clean-up between cases
                }
                let rp = r.as_mut().unwrap();
                let pawn = driven.unwrap();
                let tick: u64 = field(line, "tick").parse().unwrap();
                while rp.sim.tick_count() < tick {
                    rp.tick();
                }
                let condition = field(line, "condition");
                if condition == "InterruptForced" {
                    continue; // the fixture's own clean-up between cases
                }
                let still = matches!(
                    rp.sim.pawn(pawn).unwrap().job.as_ref().map(|j| &j.kind),
                    Some(JobKind::Haul { .. })
                ) && condition == "Succeeded";
                assert!(!still, "{}: job should have ended at {tick}", case.name);
            }
            "END" => {
                let rp = r.as_mut().unwrap();
                let pawn = driven.unwrap();
                let replacement = field(line, "replacement").split(':').next().unwrap();
                assert_eq!(
                    rp.job_name(pawn),
                    replacement,
                    "{}: replacement job",
                    case.name
                );
                // H and I: the new job hauls the same thing to the other cell.
                if replacement == "HaulToCell" {
                    let p = rp.sim.pawn(pawn).unwrap();
                    let Some(Job {
                        kind: JobKind::Haul { dest, .. },
                        ..
                    }) = p.job
                    else {
                        unreachable!()
                    };
                    assert_eq!(dest, Cell::new(110, 102), "{}: replacement cell", case.name);
                }
            }
            "PICKUP" | "started" | "finish" | "preInterrupt" | "postInterrupt" | "conflictA"
            | "conflictB" | "COMPLETE" => {}
            other => panic!("unknown trace line {other}"),
        }
    }
    compared
}

#[test]
fn hauling_matches_original_game_trace() {
    let Ok(text) = std::fs::read_to_string(trace_file()) else {
        eprintln!("skipping: no hauling trace at {}", trace_file().display());
        return;
    };
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    assert!(text.lines().any(|l| l == "COMPLETE"));
    let mut total = 0;
    for case in cases(&text) {
        // Case M destroys the source right after the job starts.
        let n = if case.name.starts_with("M ") {
            replay_m(&defs, &case)
        } else {
            replay(&defs, &case)
        };
        eprintln!("{}: {n} snapshots compared", case.name);
        total += n;
    }
    assert!(total > 500, "most ticks compared");
}

/// Case M: the source is destroyed straight after the job started; the
/// pending path fails on the next tick and the pawn waits 250 ticks.
fn replay_m(defs: &Arc<GameDefs>, case: &Case) -> usize {
    let mut r = Replay::new(defs.clone(), 56000);
    let filter = rimworld_sim::storage::ThingFilter::preset(
        &r.sim.defs,
        rimworld_sim::storage::StoragePreset::DefaultStockpile,
    );
    r.sim
        .map
        .storage
        .add_stockpile(StoragePriority::Normal, &[Cell::new(110, 100)], filter);
    r.item("27618:Steel:20:(105, 0, 100)");
    let source = r.ids[&27618];
    let job = r.sim.debug_haul_job(r.a, source).expect("job");
    assert!(r.sim.debug_start_job(r.a, job));
    r.sim.debug_destroy_item(source);
    r.tick();
    assert_eq!(r.sim.tick_count(), 56001);
    assert_eq!(
        r.job_name(r.a),
        "Wait",
        "{}: ErroredPather then Wait",
        case.name
    );
    let p = r.sim.pawn(r.a).unwrap();
    assert!(matches!(
        p.job.as_ref().unwrap().kind,
        JobKind::Wait {
            expiry_interval: 250
        }
    ));
    assert!(r.sim.reservations.rows().is_empty(), "no claims");
    1
}
