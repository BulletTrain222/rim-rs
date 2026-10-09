//! Simulation tests using small synthetic Defs (no game files needed).

use std::sync::Arc;

use rimworld_defs::xml::ActivePackages;
use rimworld_defs::{GameDefs, load_documents};

use crate::path::LocomotionUrgency;
use crate::*;

const DEFS: &str = r#"<Defs>
  <TerrainDef><defName>Soil</defName><pathCost>2</pathCost></TerrainDef>
  <TerrainDef><defName>Gravel</defName><pathCost>0</pathCost></TerrainDef>
  <TerrainDef><defName>WaterDeep</defName><passability>Impassable</passability></TerrainDef>
  <ThingDef><defName>Granite</defName><category>Building</category><passability>Impassable</passability></ThingDef>
  <ThingDef><defName>Human</defName><category>Pawn</category>
    <statBases><MoveSpeed>4.6</MoveSpeed></statBases><race><body>Human</body><intelligence>Humanlike</intelligence></race></ThingDef>
  <BodyDef><defName>Human</defName></BodyDef>
  <PawnKindDef><defName>Colonist</defName><race>Human</race></PawnKindDef>
  <PawnKindDef><defName>Rocky</defName><race>Granite</race></PawnKindDef>
</Defs>"#;

fn defs() -> Arc<GameDefs> {
    let (db, report) = load_documents("core", &[("t.xml", DEFS)], &ActivePackages::new(["core"]));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, _warnings) = GameDefs::from_database(db);
    Arc::new(defs)
}

/// 10x10 soil map with a granite wall at x=5 for z in 0..=8 and deep water at (2,2).
fn sim() -> Sim {
    let defs = defs();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut map = Map::new(GridSize::new(10, 10), soil);
    let granite = defs.things.id("Granite").unwrap();
    for z in 0..=8 {
        map.buildings[Cell::new(5, z)] = Some(granite);
    }
    map.terrain[Cell::new(2, 2)] = defs.terrain.id("WaterDeep").unwrap();
    let mut sim = Sim::new(defs, map);
    sim.set_default_update_rate(1);
    sim
}

#[test]
fn path_grid_comes_from_defs() {
    let s = sim();
    let g = s.path_grid();
    assert_eq!(g.cost(Cell::new(0, 0)), Some(2));
    assert!(!g.walkable(Cell::new(5, 3)), "granite is impassable");
    assert!(!g.walkable(Cell::new(2, 2)), "deep water is impassable");
}

#[test]
fn spawn_uses_race_move_speed() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "Test", Cell::new(1, 1)).unwrap();
    let p = s.pawn(id).unwrap();
    assert_eq!(s.defs.things[p.race].def_name, "Human");
    assert_eq!(p.move_costs, MoveCosts::from_move_speed(4.6));
    assert_eq!(s.pawn_at(Cell::new(1, 1)).unwrap().id, id);
}

#[test]
fn spawn_errors() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let rocky = s.defs.pawn_kinds.id("Rocky").unwrap();
    assert!(matches!(
        s.spawn_pawn(colonist, "x", Cell::new(5, 0)),
        Err(SpawnError::NotWalkable(_))
    ));
    s.spawn_pawn(colonist, "a", Cell::new(1, 0)).unwrap();
    assert!(matches!(
        s.spawn_pawn(colonist, "b", Cell::new(1, 0)),
        Err(SpawnError::Occupied(_))
    ));
    assert!(matches!(
        s.spawn_pawn(rocky, "x", Cell::new(0, 0)),
        Err(SpawnError::NoRace(_))
    ));
}

#[test]
fn pawn_walks_around_wall_to_target() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "Walker", Cell::new(1, 1)).unwrap();
    let target = Cell::new(8, 1);
    let planned = s.plan_path(id, target).unwrap();
    assert!(
        planned.cells.iter().any(|c| c.z == 9),
        "must go around the wall's top"
    );
    s.apply(Command::MoveTo { pawn: id, target }).unwrap();
    assert!(s.pawn(id).unwrap().is_moving());

    // Walking time follows the per-cell rule: each cell costs ticks per
    // move (float) + cell cost; 1 is paid per tick; overshoot carries over.
    let costs = s.pawn(id).unwrap().move_costs;
    let mut prev = Cell::new(1, 1);
    let (mut ticks, mut carry) = (0u32, 0.0f32);
    for &c in &planned.cells {
        let dir = Cell::new(c.x - prev.x, c.z - prev.z);
        let cost = costs.step_cost(dir, s.path_grid().cost(c).unwrap(), LocomotionUrgency::Jog);
        let mut left = (cost + carry.min(0.0)).max(1.0);
        while left > 0.0 {
            left -= 1.0;
            ticks += 1;
        }
        carry = left;
        prev = c;
    }
    // Plus the 2-tick path start latency (first payment on tick 2).
    let ticks = ticks + 1;
    s.advance(ticks - 1);
    assert_ne!(s.pawn(id).unwrap().position, target);
    s.tick();
    let p = s.pawn(id).unwrap();
    assert_eq!(p.position, target);
    assert!(!p.is_moving());
    assert_eq!(p.destination, None);
}

#[test]
fn visual_position_interpolates() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(0, 0)).unwrap();
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(1, 0),
    })
    .unwrap();
    s.tick();
    assert!(s.pawn(id).unwrap().step.is_none(), "path start latency");
    s.tick();
    let p = s.pawn(id).unwrap();
    // Jog: 60 / 4.6 ticks + Soil pathCost 2; one tick already paid.
    let step = p.step.unwrap();
    assert!((step.cost_total - (60.0 / 4.6 + 2.0)).abs() < 1e-4);
    assert!((step.cost_left - (step.cost_total - 1.0)).abs() < 1e-4);
    let (x, z) = p.visual_position();
    assert!(x > 0.0 && x < 1.0 && z == 0.0, "{x},{z}");
    assert_eq!(
        p.position,
        Cell::new(0, 0),
        "grid position is authoritative until the step ends"
    );
}

#[test]
fn redirect_mid_step_finishes_step_first() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(0, 0)).unwrap();
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(3, 0),
    })
    .unwrap();
    s.advance(3);
    let stepping_to = s.pawn(id).unwrap().step.unwrap().to;
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(0, 4),
    })
    .unwrap();
    assert_eq!(s.pawn(id).unwrap().step.unwrap().to, stepping_to);
    run_ordered_job(&mut s, id);
    assert_eq!(s.pawn(id).unwrap().position, Cell::new(0, 4));
}

/// Ticks until the pawn's player-ordered job has finished.
fn run_ordered_job(s: &mut Sim, id: PawnId) {
    for _ in 0..2000 {
        if !s.pawn(id).unwrap().job.as_ref().is_some_and(|j| j.forced) {
            return;
        }
        s.tick();
    }
    panic!("ordered job did not finish");
}

const JOB_DEFS: &str = r#"<Defs>
  <JobDef><defName>Goto</defName><reportString>moving.</reportString></JobDef>
  <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
  <PawnKindDef><defName>Settler</defName><race>Human</race>
    <defaultFactionDef>PlayerColony</defaultFactionDef></PawnKindDef>
</Defs>"#;

fn sim_with_jobs(seed: u64) -> Sim {
    let (db, report) = load_documents(
        "core",
        &[("t.xml", DEFS), ("j.xml", JOB_DEFS)],
        &ActivePackages::new(["core"]),
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let defs = Arc::new(GameDefs::from_database(db).0);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs, Map::new(GridSize::new(30, 30), soil), seed);
    sim.set_default_update_rate(1);
    sim
}

#[test]
fn idle_pawn_waits_then_wanders() {
    let mut s = sim_with_jobs(7);
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(15, 15)).unwrap();
    s.tick();
    let p = s.pawn(id).unwrap();
    assert!(matches!(p.job.as_ref().unwrap().kind, JobKind::Wait { .. }));
    assert_eq!(s.job_report(p), "wandering.");
    // Within the wait range plus a short walk, the pawn has moved.
    s.advance(240 + 5);
    assert!(s.pawn(id).unwrap().is_moving() || s.pawn(id).unwrap().position != Cell::new(15, 15));
    let start = Cell::new(15, 15);
    s.advance(2000);
    let p = s.pawn(id).unwrap();
    assert!(p.position.chebyshev(start) <= 30);
}

#[test]
fn order_interrupts_wandering_and_reports_moving() {
    let mut s = sim_with_jobs(3);
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(15, 15)).unwrap();
    s.advance(400); // somewhere in its wander cycle
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(2, 2),
    })
    .unwrap();
    let p = s.pawn(id).unwrap();
    assert!(p.job.as_ref().unwrap().forced);
    assert_eq!(s.job_report(p), "moving.");
    run_ordered_job(&mut s, id);
    assert_eq!(s.pawn(id).unwrap().position, Cell::new(2, 2));
    // Approaching (2, 2) from larger x/z, the last step faces west or south.
    let rot = s.pawn(id).unwrap().rotation;
    assert!(matches!(rot, Rot4::West | Rot4::South), "{rot:?}");
}

#[test]
fn same_seed_same_behaviour() {
    let run = |seed| {
        let mut s = sim_with_jobs(seed);
        let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
        let id = s.spawn_pawn(colonist, "W", Cell::new(15, 15)).unwrap();
        s.advance(3000);
        s.pawn(id).unwrap().position
    };
    assert_eq!(run(11), run(11));
}

#[test]
fn missing_job_defs_still_work() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(0, 0)).unwrap();
    s.tick();
    assert_eq!(s.job_report(s.pawn(id).unwrap()), "idle.");
}

#[test]
fn invalid_orders_are_rejected() {
    let mut s = sim();
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(0, 0)).unwrap();
    assert_eq!(
        s.apply(Command::MoveTo {
            pawn: id,
            target: Cell::new(5, 5)
        }),
        Err(CommandError::Path(PathError::GoalImpassable))
    );
    assert_eq!(
        s.apply(Command::MoveTo {
            pawn: PawnId(99),
            target: Cell::new(1, 1)
        }),
        Err(CommandError::NoSuchPawn(PawnId(99)))
    );
}

#[test]
fn test_map_is_deterministic_and_has_open_center() {
    let defs = defs();
    let palette = TestMapPalette::from_defs(&defs).unwrap();
    assert!(
        palette.missing.contains(&"Sand"),
        "falls back for absent defs"
    );
    let size = GridSize::new(100, 100);
    let a = generate_test_map(size, 42, &palette);
    let b = generate_test_map(size, 42, &palette);
    assert_eq!(a.terrain, b.terrain);
    assert_eq!(a.buildings, b.buildings);
    let grid = a.build_path_grid(&defs);
    let center = Cell::new(50, 50);
    let spawn = a.nearest_walkable(&grid, center).unwrap();
    assert!(spawn.chebyshev(center) < 5);
    let rocks = a.buildings.iter().filter(|(_, b)| b.is_some()).count();
    assert!(rocks > 0 && rocks < 5000, "{rocks}");
}

#[test]
fn group_order_spreads_destinations() {
    let mut s = sim_with_jobs(5);
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let ids: Vec<PawnId> = (0..4)
        .map(|i| {
            s.spawn_pawn(colonist, format!("P{i}"), Cell::new(2 + i, 2))
                .unwrap()
        })
        .collect();
    let target = Cell::new(20, 20);
    let dests: Vec<Cell> = ids
        .iter()
        .map(|&id| s.apply(Command::MoveTo { pawn: id, target }).unwrap())
        .collect();
    assert_eq!(dests[0], target);
    let unique: std::collections::HashSet<_> = dests.iter().collect();
    assert_eq!(unique.len(), 4, "{dests:?}");
    assert!(dests.iter().all(|d| d.chebyshev(target) <= 1));
    for &id in &ids {
        run_ordered_job(&mut s, id);
    }
    let positions: std::collections::HashSet<_> =
        ids.iter().map(|&id| s.pawn(id).unwrap().position).collect();
    assert_eq!(positions.len(), 4, "pawns ended on distinct cells");
}

#[test]
fn wanderers_never_share_a_destination() {
    // Colony pawns respect each other's destination reservations when
    // choosing where to wander (`RCellFinder.CanWanderToCell`).
    let mut s = sim_with_jobs(21);
    let settler = s.defs.pawn_kinds.id("Settler").unwrap();
    for i in 0..6 {
        s.spawn_pawn(settler, format!("P{i}"), Cell::new(10 + i, 10))
            .unwrap();
    }
    let mut wanders = 0;
    for _ in 0..5000 {
        s.tick();
        let current: Vec<Cell> = s
            .destinations
            .rows()
            .iter()
            .filter(|d| d.job.is_some() && !d.obsolete)
            .map(|d| d.cell)
            .collect();
        let unique: std::collections::HashSet<_> = current.iter().collect();
        assert_eq!(
            unique.len(),
            current.len(),
            "two current destinations on one cell at tick {}",
            s.tick_count()
        );
        wanders = wanders.max(current.len());
    }
    assert!(wanders >= 2, "several pawns wandered at once");
}

#[test]
fn reservations_follow_the_job_lifecycle() {
    let (mut sim, id) = food_sim(0.25);
    let meal = sim.defs.things.id("Meal").unwrap();
    sim.spawn_item(meal, Cell::new(13, 5), 3);
    sim.debug_find_and_start_job(id);
    // Starting the Ingest job reserved one meal of the stack (max 10 pawns).
    let rows = sim.reservations.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].max_pawns, rows[0].stack_count), (10, 1));
    // Picking up one of three splits the stack: the stack's row goes, and
    // the chew spot is reserved and becomes the destination.
    while sim.pawn(id).unwrap().carried.is_none() {
        sim.tick();
    }
    let rows = sim.reservations.rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        rows[0].target,
        crate::reservation::Target::Cell(_)
    ));
    let spot = sim.destinations.most_recent_for(id).unwrap().clone();
    assert!(spot.job.is_some());
    // Finishing releases the ordinary rows; the destination stays, jobless.
    while is_eating(&sim, id) {
        sim.tick();
    }
    assert!(sim.reservations.rows().is_empty());
    let after = sim.destinations.most_recent_for(id).unwrap();
    assert_eq!((after.cell, after.job), (spot.cell, None));
}

const NEEDS_DEFS: &str = r#"<Defs>
  <TerrainDef><defName>Soil</defName><pathCost>2</pathCost></TerrainDef>
  <ThingDef><defName>Human</defName><category>Pawn</category>
    <statBases><MoveSpeed>4.6</MoveSpeed></statBases>
    <race><thinkTreeMain>Main</thinkTreeMain><intelligence>Humanlike</intelligence></race></ThingDef>
  <PawnKindDef><defName>Colonist</defName><race>Human</race>
    <defaultFactionDef>PlayerColony</defaultFactionDef></PawnKindDef>
  <NeedDef><defName>Food</defName><needClass>Need_Food</needClass></NeedDef>
  <NeedDef><defName>Rest</defName><needClass>Need_Rest</needClass></NeedDef>
  <JobDef><defName>Goto</defName><reportString>moving.</reportString></JobDef>
  <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>LayDown</defName><reportString>lying down.</reportString></JobDef>
  <ThinkTreeDef><defName>Main</defName>
    <thinkRoot Class="ThinkNode_Priority"><subNodes>
      <li Class="ThinkNode_PrioritySorter"><subNodes>
        <li Class="JobGiver_GetFood" />
        <li Class="JobGiver_GetRest" />
      </subNodes></li>
      <li Class="JobGiver_WanderColony" />
    </subNodes></thinkRoot>
  </ThinkTreeDef>
</Defs>"#;

fn needs_sim() -> (Sim, PawnId) {
    let (db, report) = load_documents("core", &[("n.xml", NEEDS_DEFS)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, warnings) = GameDefs::from_database(db);
    assert!(warnings.is_empty(), "{warnings:?}");
    let defs = Arc::new(defs);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(30, 30), soil), 4);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let id = sim.spawn_pawn(kind, "N", Cell::new(15, 15)).unwrap();
    (sim, id)
}

fn set_rest(sim: &mut Sim, id: PawnId, level: f32) {
    let p = sim.pawn_mut_for_tests(id);
    p.needs
        .list
        .iter_mut()
        .find(|n| n.kind == NeedKind::Rest)
        .unwrap()
        .level = level;
}

#[test]
fn needs_fall_over_time() {
    let (mut s, id) = needs_sim();
    let before = s.pawn(id).unwrap().needs.clone();
    s.advance(2500); // one game hour
    let after = &s.pawn(id).unwrap().needs;
    for kind in [NeedKind::Food, NeedKind::Rest] {
        assert!(
            after.get(kind).unwrap().level < before.get(kind).unwrap().level,
            "{kind:?}"
        );
    }
}

#[test]
fn tired_pawn_lies_down_via_think_tree_and_wakes_rested() {
    let (mut s, id) = needs_sim();
    set_rest(&mut s, id, 0.2);
    // Tick 1: the think tree starts LayDown; tick 2: the lay-down driver
    // falls asleep.
    s.advance(2);
    let p = s.pawn(id).unwrap();
    assert!(p.is_sleeping(), "{:?}", p.think_trail);
    assert_eq!(s.job_report(p), "lying down.");
    assert_eq!(
        p.think_trail.last().map(String::as_str),
        Some("JobGiver_GetRest")
    );
    assert!(p.think_trail.contains(&"PrioritySorter".to_owned()));
    // From 20 % on the ground (0.8): (0.8 / 0.8) * 26 250 = 26 250 ticks.
    s.advance(27_000);
    let p = s.pawn(id).unwrap();
    assert!(!p.is_sleeping());
    assert!(p.needs.get(NeedKind::Rest).unwrap().percent() > 0.95);
}

#[test]
fn rested_pawn_wanders_instead() {
    let (mut s, id) = needs_sim();
    set_rest(&mut s, id, 0.9);
    s.tick();
    let p = s.pawn(id).unwrap();
    assert!(!p.is_sleeping());
    assert_eq!(
        p.think_trail.last().map(String::as_str),
        Some("JobGiver_WanderColony")
    );
}

#[test]
fn exhaustion_eventually_forces_sleep_during_orders() {
    // Rest at zero does not collapse a pawn at once: after > 1000 ticks at
    // zero, involuntary sleep is a random (MTB) event that interrupts even
    // player orders.
    let (mut s, id) = needs_sim();
    set_rest(&mut s, id, 0.0);
    let targets = [Cell::new(2, 2), Cell::new(27, 27)];
    let mut fell_asleep_at = None;
    for t in 0..40_000u32 {
        let p = s.pawn(id).unwrap();
        if p.is_sleeping() {
            fell_asleep_at = Some(t);
            break;
        }
        // Keep the pawn busy with forced orders so it never lies down
        // voluntarily.
        if !p.job.as_ref().is_some_and(|j| j.forced) {
            let target = targets[(t as usize / 7) % 2];
            s.apply(Command::MoveTo { pawn: id, target }).unwrap();
        }
        set_rest(&mut s, id, 0.0);
        s.tick();
    }
    let t = fell_asleep_at.expect("involuntary sleep within 40 000 ticks");
    assert!(
        t > 1000,
        "not before 1000 ticks at zero (fell asleep at {t})"
    );
    let p = s.pawn(id).unwrap();
    assert_eq!(p.think_trail, vec!["(involuntary sleep: exhausted)"]);
    assert_eq!(p.destination, None);
}

#[test]
fn night_sends_rested_but_not_full_pawns_to_bed() {
    let (mut s, id) = needs_sim();
    // Run to 22:00 (sleep hours) with rest kept at 0.7: below the 0.75
    // fall-asleep limit, so GetRest has priority in Sleep hours.
    let to_night = (22 - 6) * 2_500;
    for _ in 0..to_night {
        set_rest(&mut s, id, 0.7);
        s.tick();
    }
    assert_eq!(s.hour_of_day(), 22);
    set_rest(&mut s, id, 0.7);
    for _ in 0..300 {
        s.tick();
    }
    let p = s.pawn(id).unwrap();
    assert!(p.is_lying_down(), "{:?}", p.think_trail);
}

#[test]
fn lying_awake_pawn_gets_up_via_override_check() {
    let (mut s, id) = needs_sim();
    set_rest(&mut s, id, 0.2);
    s.advance(2);
    assert!(s.pawn(id).unwrap().is_sleeping());
    // Fully rested during the day: wakes, then within one 211-tick override
    // check switches to another job.
    set_rest(&mut s, id, 1.0);
    s.tick();
    assert!(!s.pawn(id).unwrap().is_sleeping());
    assert!(s.pawn(id).unwrap().is_lying_down(), "still lying awake");
    s.advance(212);
    assert!(!s.pawn(id).unwrap().is_lying_down());
}

#[test]
fn player_order_wakes_a_sleeping_pawn() {
    let (mut s, id) = needs_sim();
    set_rest(&mut s, id, 0.2);
    s.advance(2);
    assert!(s.pawn(id).unwrap().is_sleeping());
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(20, 15),
    })
    .unwrap();
    assert!(!s.pawn(id).unwrap().is_sleeping());
}

#[test]
fn walking_times_follow_locomotion_urgency() {
    // 10 cardinal cells of Soil (pathCost 2) at MoveSpeed 4.6.
    let mut s = sim_with_jobs(1);
    let colonist = s.defs.pawn_kinds.id("Colonist").unwrap();
    let id = s.spawn_pawn(colonist, "W", Cell::new(5, 20)).unwrap();
    s.apply(Command::MoveTo {
        pawn: id,
        target: Cell::new(15, 20),
    })
    .unwrap();
    let mut ticks = 0;
    while s.pawn(id).unwrap().position != Cell::new(15, 20) {
        s.tick();
        ticks += 1;
    }
    // Matches the original game: ordered at tick 2, arrived at tick 154 —
    // 2 ticks of path latency, then 151 payments of 1 (10 x 15.043478 with
    // overshoot carried).
    assert_eq!(ticks, 152);

    // A wander walk (urgency Walk) costs at least 50 ticks per cell.
    let walk =
        MoveCosts::from_move_speed(4.6).step_cost(Cell::new(1, 0), 2, LocomotionUrgency::Walk);
    assert_eq!(walk, 50.0);
}

/// Replays the scenario measured in the original game with the diagnostic
/// mod (docs/research.md §14): a colonist with thingIDNumber 1046, interval
/// update rate 3, MoveSpeed 4.6, ordered at tick 2 to walk 10 cells east
/// over Soil (pathCost 2).
#[test]
fn replays_the_movement_and_need_timing_observed_in_the_game() {
    let (db, _) = load_documents("core", &[("n.xml", NEEDS_DEFS)], &ActivePackages::default());
    let defs = Arc::new(GameDefs::from_database(db).0);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut s = Sim::with_seed(defs.clone(), Map::new(GridSize::new(40, 10), soil), 1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = s.spawn_pawn(kind, "Engie", Cell::new(5, 5)).unwrap();
    s.set_thing_id_number(pawn, 1046);
    s.set_update_rate(pawn, 3);
    s.advance(2);
    s.apply(Command::MoveTo {
        pawn,
        target: Cell::new(15, 5),
    })
    .unwrap();

    let mut rest_change_ticks = Vec::new();
    let mut last_rest = s.pawn(pawn).unwrap().needs.rest_level().unwrap();
    let mut arrived = None;
    let mut first_payment = None;
    while s.tick_count() < 600 {
        s.tick();
        let p = s.pawn(pawn).unwrap();
        if first_payment.is_none()
            && let Some(step) = p.step
        {
            first_payment = Some(s.tick_count());
            // Observed: 14.043478 left of 15.043478 after the first payment.
            assert!((step.cost_total - 15.043_478).abs() < 1e-4);
            assert!((step.cost_left - 14.043_478).abs() < 1e-4);
        }
        if arrived.is_none() && p.position == Cell::new(15, 5) {
            arrived = Some(s.tick_count());
        }
        let rest = p.needs.rest_level().unwrap();
        if rest != last_rest {
            rest_change_ticks.push(s.tick_count());
            last_rest = rest;
        }
    }
    assert_eq!(first_payment, Some(4), "observed first payment at tick 4");
    assert_eq!(arrived, Some(154), "observed arrival at tick 154");
    assert_eq!(
        &rest_change_ticks[..4],
        &[119, 269, 419, 569],
        "observed need ticks"
    );
}

const FOOD_DEFS: &str = r#"<Defs>
  <TerrainDef><defName>Soil</defName><pathCost>2</pathCost></TerrainDef>
  <ThingDef><defName>Human</defName><category>Pawn</category>
    <statBases><MoveSpeed>4.6</MoveSpeed></statBases>
    <race><thinkTreeMain>Main</thinkTreeMain><intelligence>Humanlike</intelligence>
      <foodType>OmnivoreHuman</foodType></race></ThingDef>
  <PawnKindDef><defName>Colonist</defName><race>Human</race>
    <defaultFactionDef>PlayerColony</defaultFactionDef></PawnKindDef>
  <ThingDef><defName>Meal</defName><category>Item</category><pathCost>14</pathCost>
    <statBases><Nutrition>0.9</Nutrition></statBases>
    <ingestible><foodType>Meal</foodType><preferability>MealSimple</preferability>
      <maxNumToIngestAtOnce>1</maxNumToIngestAtOnce><optimalityOffsetHumanlikes>16</optimalityOffsetHumanlikes>
    </ingestible></ThingDef>
  <ThingDef><defName>Spud</defName><category>Item</category><pathCost>14</pathCost>
    <statBases><Nutrition>0.05</Nutrition></statBases>
    <ingestible><foodType>VegetableOrFruit</foodType><preferability>RawBad</preferability>
      <tasteThought>AteRaw</tasteThought></ingestible></ThingDef>
  <ThoughtDef><defName>AteRaw</defName><stages><li><baseMoodEffect>-7</baseMoodEffect></li></stages></ThoughtDef>
  <NeedDef><defName>Food</defName><needClass>Need_Food</needClass></NeedDef>
  <JobDef><defName>Ingest</defName><reportString>consuming TargetA.</reportString></JobDef>
  <JobDef><defName>Wait_MaintainPosture</defName><reportString>standing.</reportString></JobDef>
  <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
  <ThinkTreeDef><defName>Main</defName>
    <thinkRoot Class="ThinkNode_Priority"><subNodes>
      <li Class="ThinkNode_PrioritySorter"><subNodes>
        <li Class="JobGiver_GetFood" />
      </subNodes></li>
      <li Class="JobGiver_WanderColony" />
    </subNodes></thinkRoot>
  </ThinkTreeDef>
</Defs>"#;

fn food_sim(food: f32) -> (Sim, PawnId) {
    let (db, report) = load_documents("core", &[("f.xml", FOOD_DEFS)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, warnings) = GameDefs::from_database(db);
    assert!(warnings.is_empty(), "{warnings:?}");
    let defs = Arc::new(defs);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(30, 30), soil), 7);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let id = sim.spawn_pawn(kind, "Eater", Cell::new(5, 5)).unwrap();
    sim.debug_set_need(id, NeedKind::Food, food);
    (sim, id)
}

fn is_eating(sim: &Sim, id: PawnId) -> bool {
    matches!(
        sim.pawn(id).unwrap().job.as_ref().map(|j| j.kind),
        Some(JobKind::Ingest { .. })
    )
}

fn food_level(sim: &Sim, id: PawnId) -> f32 {
    sim.pawn(id)
        .unwrap()
        .needs
        .get(NeedKind::Food)
        .unwrap()
        .level
}

#[test]
fn hungry_pawn_fetches_and_eats_a_meal() {
    let (mut sim, id) = food_sim(0.25);
    let meal = sim.defs.things.id("Meal").unwrap();
    let item = sim.spawn_item(meal, Cell::new(13, 5), 1);
    assert_eq!(
        sim.path_grid().cost(Cell::new(13, 5)),
        Some(14),
        "item path cost"
    );
    sim.debug_find_and_start_job(id);
    assert!(is_eating(&sim, id));
    assert_eq!(sim.job_report(sim.pawn(id).unwrap()), "consuming Meal.");
    let mut picked_up = None;
    for _ in 0..2000 {
        sim.tick();
        if picked_up.is_none() && sim.pawn(id).unwrap().carried.is_some() {
            picked_up = Some(sim.tick_count());
            assert!(sim.map.item(item).is_none(), "the meal left the map");
        }
        if !is_eating(&sim, id) {
            break;
        }
    }
    assert!(picked_up.is_some());
    assert!(!is_eating(&sim, id), "finished eating");
    assert_eq!(
        food_level(&sim, id),
        1.0,
        "0.25 + 0.9, capped at the maximum"
    );
    assert!(sim.pawn(id).unwrap().carried.is_none());
}

#[test]
fn raw_food_only_when_urgently_hungry() {
    // Merely hungry (below the want-eat level): raw food is not acceptable.
    let (mut sim, id) = food_sim(0.2);
    let spud = sim.defs.things.id("Spud").unwrap();
    let stack = sim.spawn_item(spud, Cell::new(8, 5), 75);
    sim.debug_find_and_start_job(id);
    assert!(!is_eating(&sim, id));

    // Urgently hungry: eats raw food, taking round(wanted / 0.05) units.
    let (mut sim, id) = food_sim(0.1);
    let stack2 = sim.spawn_item(spud, Cell::new(8, 5), 75);
    assert_eq!(stack, stack2);
    sim.debug_find_and_start_job(id);
    match sim.pawn(id).unwrap().job.as_ref().unwrap().kind {
        JobKind::Ingest { count, .. } => assert_eq!(count, 18),
        other => panic!("{other:?}"),
    }
    while is_eating(&sim, id) {
        sim.tick();
    }
    assert_eq!(sim.map.item(stack2).unwrap().stack_count, 75 - 18);
}

#[test]
fn meals_beat_closer_raw_food() {
    let (mut sim, id) = food_sim(0.1);
    let spud = sim.defs.things.id("Spud").unwrap();
    let meal = sim.defs.things.id("Meal").unwrap();
    sim.spawn_item(spud, Cell::new(8, 5), 75);
    let m = sim.spawn_item(meal, Cell::new(17, 5), 2);
    sim.debug_find_and_start_job(id);
    match sim.pawn(id).unwrap().job.as_ref().unwrap().kind {
        // 300 - 12 + 16 = 304 beats 300 - 3 - 82 = 215.
        JobKind::Ingest { food, count, .. } => assert_eq!((food, count), (m, 1)),
        other => panic!("{other:?}"),
    }
}

/// Reservation cases A and B of the external reservations report (verified
/// against the game's managed code): selecting reserves nothing, starting
/// reserves the job count, and a fully reserved stack is skipped.
#[test]
fn food_reservations_share_stacks_by_quantity() {
    let reserved = |sim: &Sim, id: PawnId| match sim.pawn(id).unwrap().job.as_ref().map(|j| j.kind)
    {
        Some(JobKind::Ingest {
            reserved, count, ..
        }) => Some((count, reserved)),
        _ => None,
    };
    // One meal: the second pawn finds nothing to eat.
    let (mut sim, a) = food_sim(0.2);
    let kind = sim.defs.pawn_kinds.id("Colonist").unwrap();
    let b = sim.spawn_pawn(kind, "Second", Cell::new(5, 7)).unwrap();
    sim.debug_set_need(b, NeedKind::Food, 0.2);
    let meal = sim.defs.things.id("Meal").unwrap();
    sim.spawn_item(meal, Cell::new(12, 6), 1);
    sim.debug_find_and_start_job(a);
    sim.debug_find_and_start_job(b);
    assert_eq!(reserved(&sim, a), Some((1, 1)));
    assert_eq!(reserved(&sim, b), None);

    // Five meals: both reserve one each.
    let (mut sim, a) = food_sim(0.2);
    let b = sim.spawn_pawn(kind, "Second", Cell::new(5, 7)).unwrap();
    sim.debug_set_need(b, NeedKind::Food, 0.2);
    sim.spawn_item(meal, Cell::new(12, 6), 5);
    sim.debug_find_and_start_job(a);
    sim.debug_find_and_start_job(b);
    assert_eq!(reserved(&sim, a), Some((1, 1)));
    assert_eq!(reserved(&sim, b), Some((1, 1)));
}

#[test]
fn scenario_starting_food_is_split_into_stacks() {
    let xml = r#"<Defs>
      <TerrainDef><defName>Soil</defName></TerrainDef>
      <ThingDef><defName>Pack</defName><category>Item</category><stackLimit>10</stackLimit>
        <statBases><Nutrition>0.9</Nutrition></statBases>
        <ingestible><foodType>Meal</foodType><preferability>MealSimple</preferability></ingestible></ThingDef>
      <ThingDef><defName>Silver</defName><category>Item</category><stackLimit>500</stackLimit></ThingDef>
      <ScenarioDef><defName>Crashlanded</defName><scenario><parts>
        <li Class="ScenPart_StartingThing_Defined"><thingDef>Silver</thingDef><count>800</count></li>
        <li Class="ScenPart_StartingThing_Defined"><thingDef>Pack</thingDef><count>25</count></li>
      </parts></scenario></ScenarioDef>
    </Defs>"#;
    let (db, report) = load_documents("core", &[("s.xml", xml)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let defs = Arc::new(GameDefs::from_database(db).0);
    let things = crate::scenario::starting_things(&defs, "Crashlanded");
    assert_eq!(things.len(), 2);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(20, 20), soil));
    let n = crate::scenario::spawn_starting_food(&mut sim, "Crashlanded", Cell::new(10, 10));
    assert_eq!(n, 25, "only food is placed");
    let stacks: Vec<u32> = sim.map.items().iter().map(|i| i.stack_count).collect();
    assert_eq!(stacks, vec![10, 10, 5]);
}

const CLEAN_DEFS: &str = r#"<Defs>
  <TerrainDef><defName>Soil</defName><pathCost>2</pathCost></TerrainDef>
  <ThingDef><defName>Human</defName><category>Pawn</category>
    <statBases><MoveSpeed>4.6</MoveSpeed></statBases>
    <race><thinkTreeMain>Main</thinkTreeMain><intelligence>Humanlike</intelligence></race></ThingDef>
  <PawnKindDef><defName>Colonist</defName><race>Human</race>
    <defaultFactionDef>PlayerColony</defaultFactionDef></PawnKindDef>
  <ThingDef><defName>Filth_Dirt</defName><category>Filth</category>
    <filth><cleaningWorkToReduceThickness>35</cleaningWorkToReduceThickness></filth></ThingDef>
  <WorkTypeDef><defName>Cleaning</defName><naturalPriority>200</naturalPriority></WorkTypeDef>
  <WorkGiverDef><defName>CleanFilth</defName><giverClass>WorkGiver_CleanFilth</giverClass>
    <workType>Cleaning</workType><priorityInType>5</priorityInType></WorkGiverDef>
  <JobDef><defName>Clean</defName><reportString>cleaning TargetA.</reportString></JobDef>
  <JobDef><defName>Wait_MaintainPosture</defName><reportString>standing.</reportString></JobDef>
  <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
  <ThinkTreeDef><defName>Main</defName>
    <thinkRoot Class="ThinkNode_Priority"><subNodes>
      <li Class="ThinkNode_PrioritySorter"><subNodes>
        <li Class="JobGiver_Work" />
      </subNodes></li>
      <li Class="JobGiver_WanderAnywhere" />
    </subNodes></thinkRoot>
  </ThinkTreeDef>
</Defs>"#;

fn clean_sim() -> Sim {
    let (db, report) = load_documents("core", &[("c.xml", CLEAN_DEFS)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, warnings) = GameDefs::from_database(db);
    assert!(warnings.is_empty(), "{warnings:?}");
    let defs = Arc::new(defs);
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs, Map::new(GridSize::new(24, 24), soil), 5);
    sim.set_default_update_rate(1);
    sim
}

fn is_cleaning(sim: &Sim, id: PawnId) -> bool {
    matches!(
        sim.pawn(id).unwrap().job.as_ref().map(|j| &j.kind),
        Some(JobKind::Clean { .. })
    )
}

#[test]
fn colonist_cleans_old_filth_in_the_home_area() {
    let mut sim = clean_sim();
    let dirt = sim.defs.things.id("Filth_Dirt").unwrap();
    let kind = sim.defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 5)).unwrap();
    for x in 0..24 {
        for z in 0..24 {
            sim.map.set_home(Cell::new(x, z), true);
        }
    }
    // Too fresh: filth must be 600 ticks old.
    let filth = sim.map.spawn_filth(dirt, Cell::new(10, 5), 2, 0);
    sim.debug_find_and_start_job(a);
    assert!(!is_cleaning(&sim, a));
    sim.debug_set_tick(600);
    sim.debug_find_and_start_job(a);
    assert!(is_cleaning(&sim, a), "the work giver chose the filth");
    assert_eq!(sim.job_report(sim.pawn(a).unwrap()), "cleaning Filth_Dirt.");
    // Starting the job reserved it; a second colonist now finds nothing.
    assert_eq!(sim.reservations.rows().len(), 1);
    let b = sim.spawn_pawn(kind, "B", Cell::new(15, 5)).unwrap();
    sim.debug_find_and_start_job(b);
    assert!(!is_cleaning(&sim, b));
    // Walk over, then two thickness levels of 35 work at speed 1.
    let mut cleaned_at = None;
    for _ in 0..400 {
        sim.tick();
        if sim.map.item(filth).is_none() {
            cleaned_at = Some(sim.tick_count());
            break;
        }
    }
    let cleaned_at = cleaned_at.expect("the filth was cleaned");
    assert!(sim.reservations.rows().is_empty(), "reservations released");
    assert!(sim.map.filth_in_home().is_empty());
    // The job ended successfully: the pawn holds its posture first.
    let job = sim.pawn(a).unwrap().job.as_ref().unwrap();
    assert_eq!(
        sim.defs.jobs[job.def.unwrap()].def_name,
        "Wait_MaintainPosture"
    );
    eprintln!("cleaned at tick {cleaned_at}");
}

#[test]
fn work_priority_follows_the_timetable_in_the_sorter() {
    use crate::rest::TimeAssignment;
    use crate::work::work_priority;
    // Sleep and recreation lower work's weight but do not forbid it.
    assert!(work_priority(TimeAssignment::Sleep, true) > 0.0);
    assert!(work_priority(TimeAssignment::Joy, true) > 0.0);
    // Food (9.5) outranks ordinary work even in work time (9).
    assert!(crate::food::GET_FOOD_PRIORITY > work_priority(TimeAssignment::Work, true));
    // Rest at 0.2 in "anything" time (8) outranks work there (5.5).
    assert!(crate::rest::GET_REST_PRIORITY > work_priority(TimeAssignment::Anything, true));
}

const HAUL_DEFS: &str = r#"<Defs>
  <TerrainDef><defName>Concrete</defName></TerrainDef>
  <ThingDef><defName>Human</defName><category>Pawn</category>
    <statBases><MoveSpeed>4.6</MoveSpeed></statBases>
    <race><thinkTreeMain>Main</thinkTreeMain><intelligence>Humanlike</intelligence></race></ThingDef>
  <PawnKindDef><defName>Colonist</defName><race>Human</race>
    <defaultFactionDef>PlayerColony</defaultFactionDef></PawnKindDef>
  <StatDef><defName>CarryingCapacity</defName><defaultBaseValue>75</defaultBaseValue></StatDef>
  <ThingDef><defName>Steel</defName><category>Item</category><stackLimit>75</stackLimit>
    <alwaysHaulable>true</alwaysHaulable><pathCost>14</pathCost>
    <thingCategories><li>Metals</li></thingCategories></ThingDef>
  <WorkTypeDef><defName>Hauling</defName><naturalPriority>300</naturalPriority></WorkTypeDef>
  <WorkGiverDef><defName>HaulGeneral</defName><giverClass>WorkGiver_HaulGeneral</giverClass>
    <workType>Hauling</workType><priorityInType>15</priorityInType></WorkGiverDef>
  <JobDef><defName>HaulToCell</defName><reportString>hauling TargetA.</reportString></JobDef>
  <JobDef><defName>Wait_MaintainPosture</defName><reportString>standing.</reportString></JobDef>
  <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
  <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
  <ThinkTreeDef><defName>Main</defName>
    <thinkRoot Class="ThinkNode_Priority"><subNodes>
      <li Class="ThinkNode_PrioritySorter"><subNodes><li Class="JobGiver_Work" /></subNodes></li>
      <li Class="JobGiver_WanderAnywhere" />
    </subNodes></thinkRoot>
  </ThinkTreeDef>
</Defs>"#;

fn haul_sim() -> Sim {
    let (db, report) = load_documents("core", &[("h.xml", HAUL_DEFS)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, warnings) = GameDefs::from_database(db);
    assert!(warnings.is_empty(), "{warnings:?}");
    let defs = Arc::new(defs);
    let floor = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::with_seed(defs, Map::new(GridSize::new(30, 30), floor), 3);
    sim.set_default_update_rate(1);
    sim
}

#[test]
fn storage_order_is_stable_by_priority() {
    use crate::storage::StoragePriority::*;
    let mut sim = haul_sim();
    let all = crate::storage::ThingFilter::everything(&sim.defs);
    let s = &mut sim.map.storage;
    let a = s.add_stockpile(Normal, &[Cell::new(1, 1)], all.clone());
    let b = s.add_stockpile(Low, &[Cell::new(2, 1)], all.clone());
    let c = s.add_stockpile(Normal, &[Cell::new(3, 1)], all);
    assert_eq!(s.in_priority_order(), &[a, c, b]);
    s.set_priority(b, Critical);
    assert_eq!(s.in_priority_order(), &[b, a, c]);
    // Raising and lowering again does not restore creation order.
    s.set_priority(a, Critical);
    s.set_priority(a, Normal);
    assert_eq!(s.in_priority_order(), &[b, a, c]);
}

#[test]
fn haul_count_overshoots_and_carry_limit_rounds() {
    let mut sim = haul_sim();
    let steel = sim.defs.things.id("Steel").unwrap();
    let all = crate::storage::ThingFilter::everything(&sim.defs);
    sim.map.storage.add_stockpile(
        crate::storage::StoragePriority::Normal,
        &[Cell::new(20, 5)],
        all,
    );
    let kind = sim.defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 5)).unwrap();
    let src = sim.spawn_item(steel, Cell::new(7, 5), 20);
    // One empty cell: count 75 even for a source of 20.
    let job = sim.debug_haul_job(a, src).unwrap();
    assert!(matches!(job.kind, JobKind::Haul { count: 75, .. }));
    assert_eq!(crate::haul::max_carry(&sim.defs, steel, 40.0), 40);
    assert_eq!(crate::haul::max_carry(&sim.defs, steel, 200.0), 75);
}

#[test]
fn two_colonists_haul_without_sharing_targets() {
    let mut sim = haul_sim();
    let steel = sim.defs.things.id("Steel").unwrap();
    let cells: Vec<Cell> = (0..4).map(|x| Cell::new(20 + x, 10)).collect();
    let all = crate::storage::ThingFilter::everything(&sim.defs);
    sim.map
        .storage
        .add_stockpile(crate::storage::StoragePriority::Normal, &cells, all);
    let kind = sim.defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 5)).unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(5, 7)).unwrap();
    for z in [4, 6, 8] {
        sim.spawn_item(steel, Cell::new(8, z), 30);
    }
    sim.debug_find_and_start_job(a);
    sim.debug_find_and_start_job(b);
    // Both haul, never the same source or cell.
    let rows = sim.reservations.rows();
    let targets: Vec<_> = rows.iter().map(|r| r.target).collect();
    let unique: std::collections::HashSet<_> = targets.iter().map(|t| format!("{t:?}")).collect();
    assert_eq!(unique.len(), targets.len(), "no shared reservation");
    assert_eq!(rows.len(), 4, "each reserved a cell and a source");
    for _ in 0..2000 {
        sim.tick();
    }
    let stored: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| cells.contains(&i.position))
        .map(|i| i.stack_count)
        .sum();
    assert_eq!(stored, 90, "all steel ended up in the stockpile");
}

#[test]
fn designating_and_removing_stockpile_cells() {
    let mut sim = haul_sim();
    let a = sim
        .designate_stockpile(&[Cell::new(1, 1), Cell::new(2, 1)])
        .unwrap();
    // Touching cells extend the same zone, appended in order.
    let b = sim.designate_stockpile(&[Cell::new(3, 1)]).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        sim.map.storage.zone(a).cells,
        vec![Cell::new(1, 1), Cell::new(2, 1), Cell::new(3, 1)]
    );
    // A separate drag far away makes a new zone.
    let c = sim.designate_stockpile(&[Cell::new(10, 10)]).unwrap();
    assert_ne!(a, c);
    sim.remove_zone_cells(&[Cell::new(2, 1)]);
    assert_eq!(
        sim.map.storage.zone(a).cells,
        vec![Cell::new(1, 1), Cell::new(3, 1)]
    );
    sim.remove_zone_cells(&[Cell::new(10, 10)]);
    assert_eq!(sim.map.storage.live_zones().collect::<Vec<_>>(), vec![a]);
}
