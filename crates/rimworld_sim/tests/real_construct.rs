//! Builds walls end to end with the real game data (blueprint → delivery
//! through `HaulToContainer` → frame → `FinishFrame` → wall), if an
//! install is configured. Skipped otherwise.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::map::ConstructStage;
use rimworld_sim::stats::{Skills, cost_list, def_stat, pawn_stat};
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

fn steel_count(sim: &Sim) -> u32 {
    let steel = sim.defs.things.id("Steel").unwrap();
    sim.map
        .items()
        .iter()
        .filter(|i| i.def == steel)
        .map(|i| i.stack_count)
        .sum::<u32>()
        + sim
            .pawns()
            .iter()
            .filter_map(|p| p.carried)
            .filter(|c| c.def == steel)
            .map(|c| c.count)
            .sum::<u32>()
}

#[test]
fn wall_stats_follow_the_stat_pipeline() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let wall = &defs.things[defs.things.id("Wall").unwrap()];
    let steel_id = defs.things.id("Steel").unwrap();
    let steel = &defs.things[steel_id];
    let wood_id = defs.things.id("WoodLog").unwrap();
    // costStuffCount 5 / volume 1.
    assert_eq!(cost_list(&defs, wall, Some(steel_id)), vec![(steel_id, 5)]);
    assert_eq!(cost_list(&defs, wall, Some(wood_id)), vec![(wood_id, 5)]);
    assert!(wall.accepts_stuff(steel));
    let work = def_stat(&defs, wall, Some(steel), "WorkToBuild");
    eprintln!("Wall (steel) WorkToBuild = {work}");
    assert!(work > 0.0);
    let human = &defs.things[defs.things.id("Human").unwrap()];
    let mut skills = Skills::default();
    let speed0 = pawn_stat(&defs, human, &skills, "ConstructionSpeed");
    let chance0 = pawn_stat(&defs, human, &skills, "ConstructSuccessChance");
    skills.set("Construction", 10);
    let speed10 = pawn_stat(&defs, human, &skills, "ConstructionSpeed");
    let chance10 = pawn_stat(&defs, human, &skills, "ConstructSuccessChance");
    eprintln!("speed {speed0} -> {speed10}, success {chance0} -> {chance10}");
    // SkillNeed_BaseBonus 0.3 + 0.0875/level (research §22).
    assert!((speed0 - 0.3).abs() < 1e-4, "{speed0}");
    assert!((speed10 - 1.175).abs() < 1e-4, "{speed10}");
    assert!(chance10 > chance0);
}

fn wall_sim(defs: Arc<GameDefs>, seed: u64) -> Sim {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs, Map::new(GridSize::new(40, 40), soil), seed);
    sim.set_default_update_rate(1);
    sim
}

#[test]
fn colonist_builds_a_steel_wall() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 5);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(10, 10)).unwrap();
    sim.set_skill(a, "Construction", 20);
    let steel = defs.things.id("Steel").unwrap();
    let wall = defs.things.id("Wall").unwrap();
    sim.spawn_item(steel, Cell::new(14, 10), 30);
    let cell = Cell::new(10, 16);
    let bp = sim.place_blueprint(wall, Some(steel), cell).unwrap();
    assert_eq!(
        sim.map.constructible(bp).unwrap().stage,
        ConstructStage::Blueprint
    );
    let mut saw_delivery = false;
    let mut saw_frame = false;
    let mut saw_building = false;
    for _ in 0..20_000 {
        sim.tick();
        let p = sim.pawn(a).unwrap();
        match p.job.as_ref().map(|j| j.kind) {
            Some(JobKind::HaulToContainer { .. }) => saw_delivery = true,
            Some(JobKind::FinishFrame { .. }) => saw_building = true,
            _ => {}
        }
        if sim
            .map
            .constructible_at(cell)
            .is_some_and(|k| k.stage == ConstructStage::Frame)
        {
            saw_frame = true;
        }
        if sim.map.buildings[cell] == Some(wall) {
            break;
        }
    }
    assert!(saw_delivery && saw_frame && saw_building);
    assert_eq!(sim.map.buildings[cell], Some(wall));
    assert_eq!(sim.map.building_stuff[cell], Some(steel));
    assert!(sim.map.constructible_at(cell).is_none());
    assert!(!sim.path_grid().walkable(cell));
    // Skill 20: the success chance is 1, so exactly 5 steel were used.
    assert_eq!(steel_count(&sim), 25);
    assert!(sim.enroute().rows().is_empty());
}

#[test]
fn two_colonists_build_a_wall_line_without_overdelivering() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 9);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 5)).unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(6, 5)).unwrap();
    sim.set_skill(a, "Construction", 20);
    sim.set_skill(b, "Construction", 20);
    let steel = defs.things.id("Steel").unwrap();
    let wall = defs.things.id("Wall").unwrap();
    sim.spawn_item(steel, Cell::new(8, 8), 40);
    sim.spawn_item(steel, Cell::new(9, 8), 20);
    let cells: Vec<Cell> = (12..18).map(|x| Cell::new(x, 14)).collect();
    for &c in &cells {
        sim.place_blueprint(wall, Some(steel), c).unwrap();
    }
    for _ in 0..60_000 {
        sim.tick();
        // Never more material in a frame than it needs.
        for k in sim.map.constructibles() {
            assert!(k.delivered(steel) <= 5);
        }
        if cells.iter().all(|&c| sim.map.buildings[c] == Some(wall)) {
            break;
        }
    }
    for &c in &cells {
        assert_eq!(sim.map.buildings[c], Some(wall), "{c:?}");
    }
    assert_eq!(steel_count(&sim), 60 - 5 * cells.len() as u32);
    // Nobody ended up inside a wall.
    for p in sim.pawns() {
        assert!(sim.path_grid().walkable(p.position));
    }
}

#[test]
fn low_skill_builders_sometimes_fail_and_retry() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 11);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(10, 10)).unwrap();
    sim.set_skill(a, "Construction", 0);
    let steel = defs.things.id("Steel").unwrap();
    let wall = defs.things.id("Wall").unwrap();
    sim.spawn_item(steel, Cell::new(12, 10), 75);
    let cells: Vec<Cell> = (8..16).map(|x| Cell::new(x, 18)).collect();
    for &c in &cells {
        sim.place_blueprint(wall, Some(steel), c).unwrap();
    }
    for _ in 0..200_000 {
        sim.tick();
        if cells.iter().all(|&c| sim.map.buildings[c] == Some(wall)) {
            break;
        }
    }
    let built = cells
        .iter()
        .filter(|&&c| sim.map.buildings[c] == Some(wall))
        .count();
    let used = 75 - steel_count(&sim);
    eprintln!("built {built} walls using {used} steel");
    for k in sim.map.constructibles() {
        eprintln!("left: {:?} {:?} {:?}", k.position, k.stage, k.resources);
    }
    for i in sim.map.items() {
        eprintln!("item {:?} x{}", i.position, i.stack_count);
    }
    let p = sim.pawn(a).unwrap();
    eprintln!(
        "pawn at {:?} job {:?} trail {:?}",
        p.position,
        p.job.as_ref().map(|j| j.kind),
        p.think_trail
    );
    assert!(built > 0);
    // Failures waste material: at least 5 per wall, never less.
    assert!(used >= 5 * built as u32);
}

#[test]
fn doors_split_rooms_and_make_pawns_wait() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 3);
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    // A 5x5 room (walls on the border of x 10..=14, z 10..=14) with a door
    // in the south wall.
    let door_cell = Cell::new(12, 10);
    for x in 10..=14 {
        for z in 10..=14 {
            let c = Cell::new(x, z);
            let border = x == 10 || x == 14 || z == 10 || z == 14;
            if border && c != door_cell {
                sim.debug_spawn_building(wall, Some(wood), c);
            }
        }
    }
    sim.debug_spawn_building(door, Some(wood), door_cell);
    let inside = Cell::new(12, 12);
    let outside = Cell::new(12, 5);
    let r = sim.regions();
    assert_ne!(r.room_at(inside), r.room_at(outside));
    assert_ne!(r.room_at(door_cell), r.room_at(inside));
    assert!(r.connected(inside, outside));
    let d = *sim.map.door_at(door_cell).unwrap();
    // DoorOpenSpeed with wood: 45 / (1 × wood's factor), rounded.
    let wood_factor = defs.things[wood]
        .stuff_props
        .as_ref()
        .and_then(|p| p.stat_factors.get("DoorOpenSpeed").copied())
        .unwrap_or(1.0);
    assert_eq!(d.ticks_to_open, (45.0f32 / wood_factor).round() as i32);
    assert!(!d.open);

    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", outside).unwrap();
    sim.apply(rimworld_sim::Command::MoveTo {
        pawn: a,
        target: inside,
    })
    .unwrap();
    let mut opened_at = None;
    let mut entered_door_at = None;
    let mut closed_at = None;
    for _ in 0..3000 {
        sim.tick();
        let t = sim.tick_count();
        let door = sim.map.door_at(door_cell).unwrap();
        if door.open && opened_at.is_none() {
            opened_at = Some(t);
            // The pawn stands next to the door, waiting.
            assert_eq!(sim.pawn(a).unwrap().position, Cell::new(12, 9));
        }
        if sim.pawn(a).unwrap().position == door_cell && entered_door_at.is_none() {
            entered_door_at = Some(t);
        }
        if opened_at.is_some() && !door.open && closed_at.is_none() {
            closed_at = Some(t);
        }
        if closed_at.is_some() && sim.pawn(a).unwrap().position == inside {
            break;
        }
    }
    let (o, e, c) = (
        opened_at.unwrap(),
        entered_door_at.unwrap(),
        closed_at.unwrap(),
    );
    eprintln!("door opened {o}, entered {e}, closed {c}");
    // The pawn waits until the door is fully open.
    assert!(e - o >= d.ticks_to_open as u64);
    // Closes after the close delay once the pawn left the doorway.
    assert!(c > e + 100);
    assert_eq!(sim.pawn(a).unwrap().position, inside);
}

#[test]
fn colonists_claim_and_sleep_in_beds() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    use rimworld_sim::NeedKind;
    use rimworld_sim::job::Rot4;
    let mut sim = wall_sim(defs.clone(), 4);
    let bed_def = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let bed = sim.debug_spawn_building_rotated(bed_def, Some(wood), Cell::new(20, 20), Rot4::North);
    let fp = sim.map.structure(bed).unwrap().footprint;
    assert_eq!(
        fp.cells().collect::<Vec<_>>(),
        vec![Cell::new(20, 20), Cell::new(20, 21)]
    );
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(15, 15)).unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(16, 15)).unwrap();
    // Night (22:00): both tired.
    sim.debug_set_tick(16 * 2500);
    sim.debug_set_need(a, NeedKind::Rest, 0.2);
    sim.debug_set_need(b, NeedKind::Rest, 0.2);
    for _ in 0..1500 {
        sim.tick();
    }
    let in_bed: Vec<_> = sim
        .pawns()
        .iter()
        .filter(|p| matches!(p.job.as_ref().map(|j| j.kind), Some(JobKind::LayDown { bed: Some(x), .. }) if x == bed))
        .map(|p| p.id)
        .collect();
    assert_eq!(in_bed.len(), 1, "exactly one sleeper takes the single bed");
    let sleeper = in_bed[0];
    let other = if sleeper == a { b } else { a };
    assert_eq!(sim.pawn(sleeper).unwrap().position, Cell::new(20, 20));
    assert_eq!(sim.pawn(sleeper).unwrap().owned_bed, Some(bed));
    assert_eq!(sim.map.structure(bed).unwrap().owners, vec![sleeper]);
    // The other one sleeps on the ground.
    assert!(matches!(
        sim.pawn(other).unwrap().job.as_ref().map(|j| j.kind),
        Some(JobKind::LayDown { bed: None, .. })
    ));
}

#[test]
fn colonist_builds_a_bed() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    use rimworld_sim::job::Rot4;
    let mut sim = wall_sim(defs.clone(), 6);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(10, 10)).unwrap();
    sim.set_skill(a, "Construction", 20);
    let bed_def = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    sim.spawn_item(wood, Cell::new(12, 10), 75);
    let at = Cell::new(15, 15);
    sim.place_blueprint_rotated(bed_def, Some(wood), at, Rot4::East)
        .unwrap();
    // The footprint is reserved for the blueprint.
    assert!(
        sim.place_blueprint(
            defs.things.id("Wall").unwrap(),
            Some(wood),
            Cell::new(16, 15)
        )
        .is_none()
    );
    for _ in 0..40_000 {
        sim.tick();
        if sim.map.structure_at(at).is_some() {
            break;
        }
    }
    let s = sim.map.structure_at(at).expect("bed built");
    assert_eq!(s.def, bed_def);
    assert!(s.footprint.contains(Cell::new(16, 15)));
    assert_eq!(sim.map.buildings[Cell::new(16, 15)], Some(bed_def));
}

#[test]
fn hungry_colonist_eats_at_a_table() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    use rimworld_sim::job::Rot4;
    use rimworld_sim::{IngestStage, NeedKind};
    let mut sim = wall_sim(defs.clone(), 8);
    let wood = defs.things.id("WoodLog").unwrap();
    let table = defs.things.id("Table1x2c").unwrap();
    let chair = defs.things.id("DiningChair").unwrap();
    let meal = defs.things.id("MealSimple").unwrap();
    // Table on (20,20)-(20,21); a chair west of it.
    sim.debug_spawn_building_rotated(table, Some(wood), Cell::new(20, 20), Rot4::North);
    sim.debug_spawn_building_rotated(chair, Some(wood), Cell::new(19, 20), Rot4::East);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(12, 12)).unwrap();
    sim.spawn_item(meal, Cell::new(13, 12), 1);
    sim.debug_set_need(a, NeedKind::Food, 0.2);
    let mut chewed_at = None;
    for _ in 0..3000 {
        sim.tick();
        let p = sim.pawn(a).unwrap();
        if let Some(JobKind::Ingest {
            stage: IngestStage::Chew { .. },
            ..
        }) = p.job.as_ref().map(|j| j.kind)
        {
            chewed_at = Some(p.position);
            break;
        }
    }
    assert_eq!(
        chewed_at,
        Some(Cell::new(19, 20)),
        "eats sitting on the chair"
    );
}

#[test]
fn work_and_movement_slow_in_the_dark() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let human = defs.things.get("Human").unwrap();
    let skills = rimworld_sim::stats::Skills::default();
    let at = |stat: &str, glow: f32| {
        rimworld_sim::stats::pawn_stat_lit(&defs, human, &skills, stat, &|_| 1.0, Some(glow))
    };
    // `StatPart_Glow` on WorkSpeedGlobal: ×0.8 in the dark, ×1 from 0.3.
    let lit = at("ConstructionSpeed", 1.0);
    assert!((at("ConstructionSpeed", 0.0) - lit * 0.8).abs() < 1e-5);
    assert!((at("ConstructionSpeed", 0.15) - lit * 0.9).abs() < 1e-5);
    assert_eq!(at("ConstructionSpeed", 0.3), lit);
    assert!((at("WorkSpeedGlobal", 0.0) - 0.8).abs() < 1e-6);
}

#[test]
fn a_built_torch_lamp_starts_full_and_lights_the_dark() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 3);
    // Night: no sun.
    sim.debug_set_sky_glow(Some(0.0));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(4, 4)).unwrap();
    sim.set_skill(a, "Construction", 20);
    sim.initialize_work(a);
    let wood = defs.things.id("WoodLog").unwrap();
    sim.spawn_item(wood, Cell::new(6, 6), 75);
    let torch = defs.things.id("TorchLamp").unwrap();
    let at = Cell::new(12, 12);
    sim.place_blueprint(torch, None, at).unwrap();
    assert_eq!(sim.map.ground_glow(Cell::new(13, 12)), 0.0);
    for _ in 0..6_000 {
        sim.tick();
        if sim.fuel_at(at).is_some() {
            break;
        }
    }
    let fuel = sim.fuel_at(at).expect("the torch was built");
    assert!(fuel > 19.9, "{fuel}");
    sim.tick();
    // Lamps give at most 0.5 glow; a cell next to the torch gets that.
    assert_eq!(sim.map.ground_glow(Cell::new(13, 12)), 0.5);
    assert_eq!(sim.map.ground_glow(Cell::new(1, 1)), 0.0);
}

#[test]
fn a_campfire_heats_its_room_only_while_it_burns() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(12, 12), soil));
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 2..=8 {
        for z in 2..=8 {
            if x == 2 || x == 8 || z == 2 || z == 8 {
                sim.debug_spawn_building(wall, wood, Cell::new(x, z));
            }
            sim.map.set_roof(Cell::new(x, z), true);
        }
    }
    let fire = defs.things.id("Campfire").unwrap();
    let at = Cell::new(5, 5);
    sim.debug_spawn_building(fire, None, at);
    sim.debug_set_fuel(at, 0.0);
    sim.outdoor_temperature = -10.0;
    sim.debug_set_room_temperatures(-10.0);
    for _ in 0..600 {
        sim.tick();
    }
    let cold = sim.cell_temperature(at);
    assert!(cold < -9.0, "{cold}");
    sim.debug_set_fuel(at, 20.0);
    for _ in 0..600 {
        sim.tick();
    }
    // 21 heat a second over the 25-cell room, ten pushes, less the loss
    // through the walls.
    let warm = sim.cell_temperature(at);
    assert!(warm > cold + 5.0, "{cold} -> {warm}");
    // Outside, nothing changes.
    assert_eq!(sim.cell_temperature(Cell::new(0, 0)), -10.0);
}

#[test]
fn a_laid_wood_floor_replaces_the_soil_and_its_grass() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 3);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(4, 4)).unwrap();
    sim.set_skill(a, "Construction", 20);
    sim.initialize_work(a);
    let wood = defs.things.id("WoodLog").unwrap();
    sim.spawn_item(wood, Cell::new(6, 6), 20);
    let grass = defs.things.id("Plant_Grass").unwrap();
    let at = Cell::new(10, 10);
    sim.map.spawn_plant(grass, at, 0.5, 85.0);
    let floor = defs.terrain.id("WoodPlankFloor").unwrap();
    // Wood floors need heavy support: not on deep water.
    let water = defs.terrain.id("WaterDeep").unwrap();
    sim.map.set_terrain(Cell::new(11, 10), water);
    assert!(
        sim.place_floor_blueprint(floor, Cell::new(11, 10))
            .is_none()
    );
    sim.place_floor_blueprint(floor, at).unwrap();
    assert!(sim.place_floor_blueprint(floor, at).is_none());
    for _ in 0..6_000 {
        sim.tick();
        if sim.map.terrain[at] == floor {
            break;
        }
    }
    assert_eq!(sim.map.terrain[at], floor);
    // Grass cannot grow without fertility.
    assert!(sim.map.plant_at(at).is_none());
    let left: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == wood)
        .map(|i| i.stack_count)
        .sum();
    assert_eq!(left, 17);
}

#[test]
fn colonists_track_dirt_onto_floors_but_not_onto_soil_outdoors() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = wall_sim(defs.clone(), 9);
    // A roofed room with a wood floor and a door on its west side.
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let wood = defs.things.id("WoodLog");
    let floor = defs.terrain.id("WoodPlankFloor").unwrap();
    for x in 20..=26 {
        for z in 10..=16 {
            let edge = x == 20 || x == 26 || z == 10 || z == 16;
            if edge && (x, z) != (20, 13) {
                sim.debug_spawn_building(wall, wood, Cell::new(x, z));
            } else if !edge {
                sim.map.set_terrain(Cell::new(x, z), floor);
            }
            sim.map.set_roof(Cell::new(x, z), true);
        }
    }
    sim.debug_spawn_building(door, wood, Cell::new(20, 13));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(5, 13)).unwrap();
    for trip in 0..40 {
        let target = if trip % 2 == 0 {
            Cell::new(25, 13)
        } else {
            Cell::new(5, 13)
        };
        sim.apply(rimworld_sim::Command::MoveTo { pawn: a, target })
            .unwrap();
        for _ in 0..1_500 {
            sim.debug_set_need(a, rimworld_sim::NeedKind::Food, 1.0);
            sim.debug_set_need(a, rimworld_sim::NeedKind::Rest, 1.0);
            sim.tick();
            if sim.pawn(a).unwrap().position == target {
                break;
            }
        }
    }
    let dirt = defs.things.id("Filth_Dirt").unwrap();
    let inside = |c: Cell| (21..=25).contains(&c.x) && (11..=15).contains(&c.z);
    let filth: Vec<_> = sim.map.items().iter().filter(|i| i.is_filth()).collect();
    // Dirt from the soil lands on the floor inside...
    assert!(
        filth.iter().any(|i| i.def == dirt && inside(i.position)),
        "{:?}",
        filth
            .iter()
            .map(|i| (i.def, i.position))
            .collect::<Vec<_>>()
    );
    // ...but no pawn filth stays on open soil (it does not accept it).
    assert!(
        filth
            .iter()
            .all(|i| inside(i.position) || sim.map.roofed(i.position))
    );
}

#[test]
fn things_left_outside_deteriorate_but_not_in_a_closed_room() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    // A 40×40 map visits each cell about every 1,600 ticks, close to a
    // full-size map's 1,645.
    let mut sim = wall_sim(defs.clone(), 4);
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    let inside = Cell::new(20, 20);
    for d in Cell::NEIGHBORS_8 {
        sim.debug_spawn_building(wall, wood, inside + d);
    }
    sim.map.set_roof(inside, true);
    // Frozen, so the meals don't rot: only weather wears them down
    // (simple meals: 10 hit points a day outdoors, 50 in all).
    sim.outdoor_temperature = -20.0;
    let meal = defs.things.id("MealSimple").unwrap();
    let out = sim.spawn_item(meal, Cell::new(5, 5), 5);
    let kept = sim.spawn_item(meal, inside, 5);
    sim.debug_set_room_temperatures(-20.0);
    let mut out_gone = None;
    for _ in 0..9 * 60_000 {
        sim.outdoor_temperature = -20.0;
        sim.tick();
        if out_gone.is_none() && sim.map.item(out).is_none() {
            out_gone = Some(sim.tick_count());
        }
    }
    let days = out_gone.expect("the outdoor meals deteriorated away") as f32 / 60_000.0;
    assert!((3.5..7.0).contains(&days), "{days}");
    assert_eq!(sim.map.item(kept).unwrap().hit_points, None);
}
