//! Differential replay of the research probe trace (local, gitignored):
//! the starting research, research benches' speed factors in several rooms
//! and a colonist researching tick by tick. Skipped without an install or
//! the trace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::Rot4;
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

fn rot(i: &str) -> Rot4 {
    match i {
        "1" => Rot4::East,
        "2" => Rot4::South,
        "3" => Rot4::West,
        _ => Rot4::North,
    }
}

#[test]
fn starting_research_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(10, 10), soil));
    sim.apply_starting_research("PlayerColony");
    let mut ours: Vec<&str> = defs
        .research
        .iter()
        .map(|(_, p)| p.def_name.as_str())
        .filter(|p| sim.research_finished(p))
        .collect();
    ours.sort();
    // RUNTIME VERIFIED (research probe): a Crashlanded colony starts with
    // these seven finished.
    let mut expected = vec![
        "ComplexFurniture",
        "PassiveCooler",
        "Stonecutting",
        "ComplexClothing",
        "Electricity",
        "NutrientPaste",
        "AirConditioning",
    ];
    expected.sort();
    assert_eq!(ours, expected);
    assert_eq!(sim.research().tech_level, 4);
    if let Some(path) = trace_file("research1", "trace_research.csv") {
        let text = std::fs::read_to_string(path).unwrap();
        let mut theirs: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("#finished,"))
            .collect();
        theirs.sort();
        assert_eq!(ours, theirs);
    }
    // Gating: batteries need Batteries; the hi-tech bench needs
    // Microelectronics.
    let battery = defs.things.id("Battery").unwrap();
    assert!(!sim.research_unlocked(battery));
    assert!(sim.research_unlocked(defs.things.id("Wall").unwrap()));
    assert!(sim.set_research_project(Some("Batteries")));
    assert!(!sim.can_start_research("Electricity"), "already finished");
    assert!(!sim.can_start_research("MultiAnalyzer"), "prerequisites");
    sim.finish_research("Batteries");
    assert!(sim.research_unlocked(battery));
    assert_eq!(sim.current_research(), None, "finished project unselected");
}

#[test]
fn research_benches_and_a_researcher_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("research1", "trace_research.csv") else {
        eprintln!("skipping: no research trace");
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
    let o = Cell::new(30, 30);
    let at = |x: &str, z: &str| {
        Cell::new(
            o.x + x.parse::<i32>().unwrap(),
            o.z + z.parse::<i32>().unwrap(),
        )
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut map = Map::new(GridSize::new(90, 70), soil);
    // Rooms: roofs and floors first (the walls come with the spawns).
    for line in text.lines().filter_map(|l| l.strip_prefix("#room,")) {
        let f: Vec<&str> = line.split(',').collect();
        let (x0, z0): (i32, i32) = (f[0].parse().unwrap(), f[1].parse().unwrap());
        for x in x0 + 1..=x0 + 5 {
            for z in z0 + 1..=z0 + 5 {
                let c = Cell::new(o.x + x, o.z + z);
                if f[2] == "1" {
                    map.set_roof(c, true);
                }
                if f[3] != "-" {
                    map.terrain[c] = defs.terrain.id(f[3]).unwrap();
                }
            }
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    sim.apply_starting_research("PlayerColony");
    let mut ids = std::collections::HashMap::new();
    for line in text.lines().filter_map(|l| l.strip_prefix("#spawn,")) {
        let f: Vec<&str> = line.split(',').collect();
        let def = defs.things.id(f[0]).unwrap();
        let cell = at(f[1], f[2]);
        if let Some(old) = ids.remove(&cell) {
            sim.debug_destroy_structure(old);
        }
        let stuff = (f[4] != "-").then(|| defs.things.id(f[4]).unwrap());
        let id = sim.debug_spawn_building_rotated(def, stuff, cell, rot(f[3]));
        ids.insert(cell, id);
    }
    let dirt = defs.things.id("Filth_Dirt").unwrap();
    let mut seeded = Vec::new();
    for line in text.lines().filter_map(|l| l.strip_prefix("#filth,")) {
        let (x, z) = line.split_once(',').unwrap();
        seeded.push(sim.map.spawn_filth(dirt, at(x, z), 1, 0));
    }
    // Every bench's factor (role, cleanliness, outdoors) and interaction cell.
    for line in text.lines().filter_map(|l| l.strip_prefix("#bench,")) {
        let f: Vec<&str> = line.split(',').collect();
        let bench = ids[&at(f[1], f[2])];
        let theirs: f32 = f[3].parse().unwrap();
        let ours = sim.research_speed_factor(bench);
        assert!(
            (ours - theirs).abs() < 1e-5,
            "bench at {},{} ({} room): {ours} vs {theirs}",
            f[1],
            f[2],
            f[4]
        );
        assert_eq!(sim.interaction_cell(bench), Some(at(f[11], f[12])));
    }
    // The researcher.
    let p = header("pawn");
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "researcher", at(&p[1], &p[2]))
        .unwrap();
    sim.set_thing_id_number(pawn, p[0].parse().unwrap());
    sim.set_update_rate(pawn, p[4].parse().unwrap());
    sim.set_move_speed(pawn, p[3].parse().unwrap());
    sim.set_skill(pawn, "Intellectual", p[5].parse().unwrap());
    assert_eq!(p[7], "0", "no passion");
    sim.debug_set_passion(pawn, "Intellectual", rimworld_sim::stats::Passion::None);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Research" { 3 } else { 0 });
    }
    assert!(sim.set_research_project(Some("Batteries")));
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|l| l.split(',').collect())
        .collect();
    let start: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(start);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    for f in &rows {
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.tick();
            // Tracked dirt is a random outcome; the recorded run left none
            // (its bench factor never changed).
            let tracked: Vec<_> = sim
                .map
                .items()
                .iter()
                .filter(|i| i.is_filth() && !seeded.contains(&i.id))
                .map(|i| (i.id, i.stack_count))
                .collect();
            for (id, n) in tracked {
                sim.map.take_from_item(id, n);
            }
        }
        let ctx = format!("tick {tick}");
        let pp = sim.pawn(pawn).unwrap();
        let job = pp
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |j| defs.jobs[j].def_name.clone());
        assert_eq!(job, f[4], "{ctx}: job");
        assert_eq!(pp.position, at(f[1], f[2]), "{ctx}: position");
        let speed: f32 = f[9].parse().unwrap();
        assert!(
            (sim.pawn_stat(pawn, "ResearchSpeed").unwrap() - speed).abs() < 1e-5,
            "{ctx}: research speed"
        );
        let progress: f32 = f[6].parse().unwrap();
        let ours = sim.research_progress("Batteries");
        assert!(
            (ours - progress).abs() < 1e-4,
            "{ctx}: progress {ours} vs {progress}"
        );
        checked += 1;
    }
    eprintln!("research: {checked} ticks match");
}
