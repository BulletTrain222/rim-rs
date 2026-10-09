//! What the Work and Schedule tabs change, with the real game data
//! (docs/research.md §67): stored vs effective work priorities with
//! manual priorities off and on, the skills and capacities a work box
//! shows, and a colonist's timetable deciding when it sleeps. Skipped
//! without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::rest::TimeAssignment;
use rimworld_sim::stats::Passion;
use rimworld_sim::{Cell, GridSize, Map, NeedKind, PawnId, Sim};

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

fn colony(defs: &Arc<GameDefs>) -> (Sim, PawnId) {
    let soil = defs.terrain.id("Soil").unwrap();
    let map = Map::new(GridSize::new(40, 40), soil);
    let mut sim = Sim::with_seed(defs.clone(), map, 11);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let id = sim.spawn_pawn(kind, "A", Cell::new(20, 20)).unwrap();
    (sim, id)
}

#[test]
fn manual_priorities_show_the_stored_numbers() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = colony(&defs);
    let mining = defs.work_types.id("Mining").unwrap();
    assert!(sim.ever_works(a));
    // Checkbox mode: any enabled type reads 3; the stored 1 is kept.
    assert!(sim.set_work_type_priority(a, mining, 1));
    assert_eq!(sim.work_priority(a, mining), Some(3));
    sim.set_use_work_priorities(true);
    assert_eq!(sim.work_priority(a, mining), Some(1));
    sim.set_use_work_priorities(false);
    assert!(sim.set_work_type_priority(a, mining, 0));
    assert_eq!(sim.work_priority(a, mining), Some(0));
    // Saved with the game.
    sim.set_use_work_priorities(true);
    assert!(sim.set_work_type_priority(a, mining, 2));
    let loaded = Sim::load(defs.clone(), &sim.save()).unwrap();
    assert_eq!(loaded.work_priority(a, mining), Some(2));
    assert!(loaded.use_work_priorities);
}

#[test]
fn work_boxes_read_skills_passions_and_capacities() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = colony(&defs);
    let mining = defs.work_types.id("Mining").unwrap();
    let doctor = defs.work_types.id("Doctor").unwrap();
    // A type without relevant skills averages 3.
    let firefighter = defs.work_types.id("Firefighter").unwrap();
    assert!(defs.work_types[firefighter].relevant_skills.is_empty());
    assert_eq!(sim.average_relevant_skill(a, firefighter), 3.0);
    sim.set_skill(a, "Mining", 9);
    assert_eq!(sim.average_relevant_skill(a, mining), 9.0);
    // Doctor's relevant skill is Medicine alone.
    sim.set_skill(a, "Medicine", 4);
    assert_eq!(sim.average_relevant_skill(a, doctor), 4.0);
    assert_eq!(sim.max_relevant_passion(a, mining), Passion::None);
    sim.debug_set_passion(a, "Mining", Passion::Major);
    assert_eq!(sim.max_relevant_passion(a, mining), Passion::Major);
    // A healthy colonist can do every type with givers.
    assert!(!sim.incapable_of_work_type(a, mining));
}

/// Whether the colonist lies down within a few hundred ticks.
fn lies_down(sim: &mut Sim, a: PawnId) -> bool {
    for _ in 0..300 {
        sim.tick();
        let p = sim.pawn(a).unwrap();
        if p.job
            .as_ref()
            .and_then(|j| j.def)
            .is_some_and(|d| sim.defs.jobs[d].def_name == "LayDown")
        {
            return true;
        }
    }
    false
}

#[test]
fn the_timetable_decides_when_a_colonist_sleeps() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    // Half rested: sleeps only in Sleep hours.
    let (mut sim, a) = colony(&defs);
    sim.set_timetable(a, &[TimeAssignment::Sleep; 24]);
    sim.debug_set_need(a, NeedKind::Rest, 0.5);
    assert!(lies_down(&mut sim, a));

    let (mut sim, a) = colony(&defs);
    sim.set_timetable(a, &[TimeAssignment::Anything; 24]);
    sim.debug_set_need(a, NeedKind::Rest, 0.5);
    assert!(!lies_down(&mut sim, a));

    // Tired, but a Work hour: no sleep.
    let (mut sim, a) = colony(&defs);
    sim.set_timetable(a, &[TimeAssignment::Work; 24]);
    sim.debug_set_need(a, NeedKind::Rest, 0.2);
    assert!(!lies_down(&mut sim, a));

    // Painting one hour, and the timetable is saved.
    let (mut sim, a) = colony(&defs);
    let hour = sim.hour_of_day() as usize;
    sim.set_time_assignment(a, hour, TimeAssignment::Joy);
    let loaded = Sim::load(defs.clone(), &sim.save()).unwrap();
    let t = loaded.timetable(a);
    assert_eq!(t[hour], TimeAssignment::Joy);
    assert_eq!(
        t[(hour + 12) % 24],
        rimworld_sim::rest::default_timetable()[(hour + 12) % 24]
    );
}
