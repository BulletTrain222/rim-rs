//! Runs a colonist with the real `Humanlike` think tree, if an install is
//! configured. Skipped otherwise.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::PawnId;
use rimworld_sim::cell_finder::IngestionSpotOrder;
use rimworld_sim::job::JobDefs;
use rimworld_sim::rand::Rand;
use rimworld_sim::region::Regions;
use rimworld_sim::reservation::{Claimant, DestinationManager, ReservationManager};
use rimworld_sim::think::{PawnFacts, ThinkContext, support_stats, think};
use rimworld_sim::{Cell, GridSize, Map, MoveCosts, PathGrid, Sim, WanderParams};

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

#[test]
fn colonist_thinks_with_real_humanlike_tree() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let tree = defs.think_trees.get("Humanlike").unwrap();
    let (total, supported) = support_stats(tree);
    eprintln!("Humanlike: {supported}/{total} nodes supported");

    let soil = defs.terrain.id("Soil").unwrap();
    let map = Map::new(GridSize::new(40, 40), soil);
    let grid: PathGrid = map.build_path_grid(&defs);
    let regions = Regions::build(&map, &defs, &grid);
    let mut order = IngestionSpotOrder::default();
    let job_defs = JobDefs::resolve(&defs);
    let mut rng = Rand::new(1);
    let mut ctx = ThinkContext {
        defs: &defs,
        pawn: PawnFacts {
            kind_def_name: "Colonist",
            is_colonist: true,
            at: Cell::new(20, 20),
            next_idle_is_wait: false,
            move_costs: MoveCosts::from_move_speed(4.6),
            rest_level: Some(0.9),
            starving: false,
            humanlike: true,
            hour: 12,
            assignment: rimworld_sim::rest::time_assignment(12, true),
            food: None,
            race: None,
            ever_work: false,
            carrying_capacity: 75.0,
            incapable: &[],
            temperature: None,
            joy: None,
            mental_state: None,
            drafted: false,
            mental_state_class: None,
            manhunter_target: None,
            wrong_season: false,
            dangerous_temperature: false,
            outdoor: true,
            can_reach_map_edge: true,
            id_number: 0,
            downed: false,
        },
        map: &map,
        grid: &grid,
        job_defs: &job_defs,
        default_wander: WanderParams::default(),
        claimant: Claimant {
            pawn: PawnId(0),
            has_faction: true,
        },
        reservations: &ReservationManager::default(),
        destinations: &DestinationManager::default(),
        regions: &regions,
        colonists: &[],
        ingest_order: &mut order,
        work: None,
        tick: 0,
        job_queue_out: Vec::new(),
        rng: &mut rng,
        unsupported: BTreeSet::new(),
        next_idle_is_wait_out: None,
        construction: None,
        beds: None,
        room_temperatures: &[],
        abs_tick: 0,
        think_data: Default::default(),
    };
    let result = think(tree, &mut ctx).expect("a job");
    eprintln!("trail: {}", result.trail.join(" > "));
    eprintln!("unsupported visited: {}", ctx.unsupported.len());
    assert_eq!(result.trail.last().unwrap(), "JobGiver_WanderColony");
    assert_eq!(result.tag.as_deref(), Some("Idle"));
    assert_eq!(defs.jobs[result.job.def.unwrap()].def_name, "GotoWander");

    // A tired colonist: the needs PrioritySorter picks JobGiver_GetRest.
    let mut ctx = ThinkContext {
        defs: &defs,
        pawn: PawnFacts {
            kind_def_name: "Colonist",
            is_colonist: true,
            at: Cell::new(20, 20),
            next_idle_is_wait: false,
            move_costs: MoveCosts::from_move_speed(4.6),
            rest_level: Some(0.2),
            starving: false,
            humanlike: true,
            hour: 12,
            assignment: rimworld_sim::rest::time_assignment(12, true),
            food: None,
            race: None,
            ever_work: false,
            carrying_capacity: 75.0,
            incapable: &[],
            temperature: None,
            joy: None,
            mental_state: None,
            drafted: false,
            mental_state_class: None,
            manhunter_target: None,
            wrong_season: false,
            dangerous_temperature: false,
            outdoor: true,
            can_reach_map_edge: true,
            id_number: 0,
            downed: false,
        },
        map: &map,
        grid: &grid,
        job_defs: &job_defs,
        default_wander: WanderParams::default(),
        claimant: Claimant {
            pawn: PawnId(0),
            has_faction: true,
        },
        reservations: &ReservationManager::default(),
        destinations: &DestinationManager::default(),
        regions: &regions,
        colonists: &[],
        ingest_order: &mut order,
        work: None,
        tick: 0,
        job_queue_out: Vec::new(),
        rng: &mut rng,
        unsupported: BTreeSet::new(),
        next_idle_is_wait_out: None,
        construction: None,
        beds: None,
        room_temperatures: &[],
        abs_tick: 0,
        think_data: Default::default(),
    };
    let tired = think(tree, &mut ctx).expect("a job");
    eprintln!("tired trail: {}", tired.trail.join(" > "));
    assert_eq!(tired.trail.last().unwrap(), "JobGiver_GetRest");
    assert_eq!(defs.jobs[tired.job.def.unwrap()].def_name, "LayDown");

    // End to end through the Sim: the spawned colonist uses the tree.
    let mut sim = Sim::with_seed(defs.clone(), map.clone(), 3);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let id = sim.spawn_pawn(kind, "T", Cell::new(20, 20)).unwrap();
    // Off-screen pawns run interval logic (incl. thinking) every 15 ticks.
    sim.advance(15);
    let p = sim.pawn(id).unwrap();
    assert!(p.think_tree.is_some());
    assert_eq!(p.think_trail.first().map(String::as_str), Some("Humanlike"));
    assert_eq!(sim.job_report(p), "wandering.");
}
