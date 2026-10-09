//! The rules behind the native-style UI, with the real game data:
//! "Prioritize" work orders (`FloatMenuOptionProvider_WorkGivers`), the
//! zone designators (`Designator_ZoneAdd`/`ZoneDelete`), the order
//! designators' acceptance and blueprint placement checks
//! (docs/research.md §66). Skipped without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::sim::{OrderDesignator, ThingRef, WorkOrderBlock, ZoneKind, ZoneLabel, ZoneRef};
use rimworld_sim::work::WorkTarget;
use rimworld_sim::{Cell, GridSize, Map, Rot4, Sim};

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

/// 60×60 soil with a granite block at x 40..45, z 20..25.
fn rock_sim(defs: &Arc<GameDefs>) -> Sim {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut map = Map::new(GridSize::new(60, 60), soil);
    let granite = defs.things.id("Granite").unwrap();
    for x in 40..45 {
        for z in 20..25 {
            map.buildings[Cell::new(x, z)] = Some(granite);
        }
    }
    let mut sim = Sim::with_seed(defs.clone(), map, 7);
    sim.set_default_update_rate(1);
    sim
}

#[test]
fn prioritize_mining_is_offered_and_taken_as_a_forced_job() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(30, 22)).unwrap();
    sim.set_work_priority(a, "Mining", 3);
    let rock = Cell::new(40, 22);
    // Not designated: the miner doesn't consider it at all.
    assert!(sim.work_order_options(a, WorkTarget::Rock(rock)).is_empty());
    assert_eq!(sim.designate_cells(OrderDesignator::Mine, &[rock]), 1);
    let rng_before = format!("{:?}", sim.rng_state());
    let opts = sim.work_order_options(a, WorkTarget::Rock(rock));
    assert_eq!(
        format!("{:?}", sim.rng_state()),
        rng_before,
        "the query draws nothing"
    );
    assert_eq!(opts.len(), 1, "{opts:?}");
    let o = &opts[0];
    let giver = &defs.work_givers[o.giver];
    assert_eq!(giver.def_name, "Mine");
    assert_eq!(
        (giver.verb.as_str(), giver.gerund.as_str()),
        ("mine", "mining")
    );
    assert_eq!(o.block, None);
    assert_eq!(o.reserved_by, None);
    // Taking it: a forced Mine job on that rock.
    assert!(sim.take_work_order(a, o.giver, WorkTarget::Rock(rock)));
    let job = sim.pawn(a).unwrap().job.clone().unwrap();
    assert!(job.forced);
    assert!(matches!(job.kind, JobKind::Mine { cell, .. } if cell == rock));
    // Now it is the pawn's job: "Already mining".
    let again = sim.work_order_options(a, WorkTarget::Rock(rock));
    assert_eq!(again[0].block, Some(WorkOrderBlock::AlreadyDoing));
}

#[test]
fn prioritize_takes_the_target_from_another_pawn() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(30, 22)).unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(30, 30)).unwrap();
    let rock = Cell::new(40, 22);
    sim.designate_cells(OrderDesignator::Mine, &[rock]);
    sim.set_work_priority(a, "Mining", 3);
    sim.set_work_priority(b, "Mining", 3);
    // B takes the rock first.
    let ob = sim.work_order_options(b, WorkTarget::Rock(rock));
    assert!(sim.take_work_order(b, ob[0].giver, WorkTarget::Rock(rock)));
    let oa = sim.work_order_options(a, WorkTarget::Rock(rock));
    assert_eq!(oa.len(), 1);
    assert_eq!(oa[0].block, None);
    assert_eq!(oa[0].reserved_by, Some(b), "Reserved by B");
    assert!(sim.take_work_order(a, oa[0].giver, WorkTarget::Rock(rock)));
    assert!(
        matches!(sim.pawn(a).unwrap().job.as_ref().unwrap().kind, JobKind::Mine { cell, .. } if cell == rock)
    );
    assert!(
        !matches!(sim.pawn(b).unwrap().job.as_ref().map(|j| j.kind), Some(JobKind::Mine { cell, .. }) if cell == rock),
        "B lost the rock"
    );
}

#[test]
fn prioritize_reports_work_not_assigned() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(30, 22)).unwrap();
    sim.set_work_priority(a, "Mining", 0);
    let rock = Cell::new(40, 22);
    sim.designate_cells(OrderDesignator::Mine, &[rock]);
    let opts = sim.work_order_options(a, WorkTarget::Rock(rock));
    assert_eq!(opts.len(), 1);
    assert_eq!(opts[0].block, Some(WorkOrderBlock::NotAssigned));
    assert!(!sim.take_work_order(a, opts[0].giver, WorkTarget::Rock(rock)));
}

#[test]
fn zone_designators_follow_the_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let rect = |x0: i32, z0: i32, x1: i32, z1: i32| -> Vec<Cell> {
        (z0..=z1)
            .flat_map(|z| (x0..=x1).map(move |x| Cell::new(x, z)))
            .collect()
    };
    // The 5-cell edge rule and rock.
    assert!(
        sim.can_zone_cell(ZoneKind::Stockpile, Cell::new(4, 20))
            .is_err()
    );
    assert!(
        sim.can_zone_cell(ZoneKind::Stockpile, Cell::new(5, 20))
            .is_ok()
    );
    assert!(
        sim.can_zone_cell(ZoneKind::Stockpile, Cell::new(41, 21))
            .is_err()
    );
    // A first drag: a new zone, "Stockpile zone 1" in the palette's first
    // colour (red lerped halfway to grey, 9% opacity).
    let z1 = sim
        .zone_add(ZoneKind::Stockpile, None, &rect(10, 10, 12, 12))
        .unwrap();
    assert_eq!(sim.zone_label(z1), (ZoneLabel::Stockpile, 1));
    assert_eq!(sim.zone_color(z1), Some([0.75, 0.25, 0.25, 0.09]));
    assert_eq!(sim.zone_cells(z1).len(), 9);
    // A separate drag with nothing selected: a second zone.
    let z2 = sim
        .zone_add(ZoneKind::Stockpile, None, &rect(20, 10, 21, 11))
        .unwrap();
    assert_ne!(z1, z2);
    assert_eq!(sim.zone_label(z2), (ZoneLabel::Stockpile, 2));
    assert_eq!(sim.zone_color(z2), Some([0.75, 0.25, 0.75, 0.09]));
    // With zone 1 selected, a touching drag grows it.
    let z = sim
        .zone_add(ZoneKind::Stockpile, Some(z1), &rect(13, 10, 14, 10))
        .unwrap();
    assert_eq!(z, z1);
    assert_eq!(sim.zone_cells(z1).len(), 11);
    // A single click on a zone of this type only selects it.
    assert_eq!(
        sim.zone_add(ZoneKind::Stockpile, None, &[Cell::new(20, 10)]),
        Some(z2)
    );
    // The dumping stockpile is its own zone, with its own name.
    let d = sim
        .zone_add(ZoneKind::DumpingStockpile, None, &rect(30, 10, 31, 11))
        .unwrap();
    assert_eq!(sim.zone_label(d), (ZoneLabel::DumpingStockpile, 1));
    let ZoneRef::Stockpile(did) = d else { panic!() };
    let corpse_ok = sim.map.storage.zone(did).filter.count();
    assert!(corpse_ok > 0);
    // A growing zone can't overlap a stockpile.
    assert!(
        sim.can_zone_cell(ZoneKind::Growing, Cell::new(10, 10))
            .is_err()
    );
    let g = sim
        .zone_add(ZoneKind::Growing, None, &rect(10, 30, 12, 31))
        .unwrap();
    assert_eq!(sim.zone_label(g), (ZoneLabel::Growing, 1));
    // Deleting the middle column splits zone 1: the part away from its
    // first cell leaves.
    sim.zone_delete_cells(&rect(11, 10, 11, 12));
    let left = sim.zone_cells(z1).to_vec();
    assert!(left.iter().all(|c| c.x == 10), "{left:?}");
    assert_eq!(sim.zone_at(Cell::new(12, 11)), None);
    // Deleting the rest removes the zone and frees its name.
    sim.zone_delete_cells(&rect(10, 10, 10, 12));
    let z3 = sim
        .zone_add(ZoneKind::Stockpile, None, &rect(10, 40, 10, 40))
        .unwrap();
    assert_eq!(sim.zone_label(z3), (ZoneLabel::Stockpile, 1));
}

#[test]
fn order_designators_accept_what_the_game_does() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let rock = Cell::new(40, 20);
    let open = Cell::new(20, 20);
    assert_eq!(
        sim.can_designate_cell(OrderDesignator::Mine, open),
        Err(Some("MessageMustDesignateMineable"))
    );
    assert!(sim.can_designate_cell(OrderDesignator::Mine, rock).is_ok());
    assert!(
        sim.can_designate_thing(OrderDesignator::Mine, ThingRef::Rock(rock))
            .is_ok()
    );
    sim.designate_cells(OrderDesignator::Mine, &[rock]);
    assert_eq!(
        sim.can_designate_cell(OrderDesignator::Mine, rock),
        Err(None)
    );
    assert!(
        sim.can_designate_cell(OrderDesignator::Cancel, rock)
            .is_ok()
    );
    sim.designate_cells(OrderDesignator::Cancel, &[rock]);
    assert!(sim.map.mine_designations.is_empty());
    // Trees are chopped for wood, crops harvested; both can be cut.
    let oak = defs.things.id("Plant_TreeOak").unwrap();
    let rice = defs.things.id("Plant_Rice").unwrap();
    let t = Cell::new(15, 15);
    let c = Cell::new(16, 15);
    sim.map.spawn_plant(oak, t, 1.0, 100.0);
    sim.map.spawn_plant(rice, c, 1.0, 100.0);
    assert!(
        sim.can_designate_cell(OrderDesignator::HarvestWood, t)
            .is_ok()
    );
    assert_eq!(
        sim.can_designate_cell(OrderDesignator::Harvest, t),
        Err(Some("MessageMustDesignateHarvestable"))
    );
    assert!(sim.can_designate_cell(OrderDesignator::Harvest, c).is_ok());
    assert_eq!(
        sim.can_designate_cell(OrderDesignator::HarvestWood, c),
        Err(Some("MessageMustDesignateHarvestableWood"))
    );
    assert_eq!(
        sim.can_designate_cell(OrderDesignator::CutPlants, open),
        Err(Some("MessageMustDesignatePlants"))
    );
    // Cutting replaces a harvest mark.
    sim.designate_cells(OrderDesignator::Harvest, &[c]);
    let rice_id = sim.map.plant_at(c).unwrap().id;
    assert!(sim.map.harvest_designations().contains(&rice_id));
    sim.designate_cells(OrderDesignator::CutPlants, &[c]);
    assert!(!sim.map.harvest_designations().contains(&rice_id));
    assert!(sim.map.cut_designations().contains(&rice_id));
}

#[test]
fn blueprint_placement_checks() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = rock_sim(&defs);
    let wall = defs.things.id("Wall").unwrap();
    let blocks = defs.things.id("BlocksGranite").unwrap();
    let steel = defs.things.id("Steel").unwrap();
    let n = Rot4::North;
    assert_eq!(
        sim.can_place_blueprint(wall, Some(blocks), Cell::new(9, 20), n),
        Err("TooCloseToMapEdge")
    );
    assert_eq!(
        sim.can_place_blueprint(wall, Some(blocks), Cell::new(10, 20), n),
        Ok(())
    );
    assert_eq!(
        sim.can_place_blueprint(wall, None, Cell::new(10, 20), n),
        Err("UnchosenStuff")
    );
    assert_eq!(
        sim.can_place_blueprint(wall, Some(blocks), Cell::new(40, 20), n),
        Err("SpaceAlreadyOccupied")
    );
    sim.place_blueprint_rotated(wall, Some(blocks), Cell::new(12, 20), n)
        .unwrap();
    assert_eq!(
        sim.can_place_blueprint(wall, Some(steel), Cell::new(12, 20), n),
        Err("IdenticalBlueprintExists")
    );
}
