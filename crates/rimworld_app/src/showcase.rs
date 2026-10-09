//! `--showcase`: a staged demo colony for short videos. Presentation only:
//! it clears a patch of the test map and places a finished bedroom with a
//! research bench and a powered lamp, blueprints for a second room, loose
//! wood and steel to haul, a ripe potato field and trees marked for
//! chopping, then gives each colonist its own kind of work.

use rimworld_defs::GameDefs;
use rimworld_sim::{Cell, Rot4, Sim};

/// Half-size of the cleared area around the spawn point.
const CLEAR: (i32, i32) = (22, 16);

/// Clears the area and stages the colony around `s` (before colonists
/// spawn).
pub fn stage_map(sim: &mut Sim, defs: &GameDefs, s: Cell) {
    let soil = defs.terrain.id("Soil");
    let size = sim.map.size();
    let inside =
        |c: Cell| (c.x - s.x).abs() <= CLEAR.0 && (c.z - s.z).abs() <= CLEAR.1 && size.contains(c);
    // Rock, plants and loose things out; plain soil in.
    let structures: Vec<_> = sim
        .map
        .structures()
        .iter()
        .filter(|st| st.footprint.cells().any(inside))
        .map(|st| st.id)
        .collect();
    for id in structures {
        sim.debug_destroy_structure(id);
    }
    let plants: Vec<_> = sim
        .map
        .plants()
        .iter()
        .filter(|p| inside(p.position))
        .map(|p| p.id)
        .collect();
    for id in plants {
        sim.map.remove_plant(id);
    }
    let items: Vec<_> = sim
        .map
        .items()
        .iter()
        .filter(|i| inside(i.position))
        .map(|i| (i.id, i.stack_count))
        .collect();
    for (id, n) in items {
        sim.map.take_from_item(id, n);
    }
    if let Some(soil) = soil {
        for dz in -CLEAR.1..=CLEAR.1 {
            for dx in -CLEAR.0..=CLEAR.0 {
                let c = s + Cell::new(dx, dz);
                if size.contains(c) {
                    sim.map.set_terrain(c, soil);
                    sim.map.set_roof(c, false);
                }
            }
        }
    }
    let thing = |n: &str| defs.things.id(n);
    let wood = thing("WoodLog");
    // A finished, roofed bedroom north-east of the spawn: three beds, a
    // simple research bench and a standing lamp.
    if let Some(wall) = thing("Wall") {
        for x in 3..=11 {
            for z in 4..=10 {
                let c = s + Cell::new(x, z);
                let border = x == 3 || x == 11 || z == 4 || z == 10;
                if border && !(x == 7 && z == 4) {
                    sim.debug_spawn_building(wall, wood, c);
                } else if !border {
                    sim.map.set_roof(c, true);
                }
            }
        }
    }
    if let Some(door) = thing("Door") {
        sim.debug_spawn_building(door, wood, s + Cell::new(7, 4));
    }
    if let Some(bed) = thing("Bed") {
        for x in [4, 6, 8] {
            sim.debug_spawn_building_rotated(bed, wood, s + Cell::new(x, 9), Rot4::South);
        }
    }
    if let Some(bench) = thing("SimpleResearchBench") {
        sim.debug_spawn_building_rotated(bench, wood, s + Cell::new(9, 6), Rot4::North);
    }
    // Power: a wood-fired generator outside, conduit into the room, a lamp.
    if let (Some(generator), Some(conduit), Some(lamp)) = (
        thing("WoodFiredGenerator"),
        thing("PowerConduit"),
        thing("StandingLamp"),
    ) {
        sim.debug_spawn_building(generator, None, s + Cell::new(13, 9));
        sim.debug_set_fuel(s + Cell::new(13, 9), 75.0);
        for x in 5..=12 {
            sim.debug_spawn_building(conduit, None, s + Cell::new(x, 9));
        }
        sim.debug_spawn_building(lamp, thing("Steel"), s + Cell::new(5, 6));
        sim.debug_update_power_nets();
    }
    // A fueled stove by the stockpile, cooking simple meals to 10.
    if let Some(stove) = thing("FueledStove") {
        let id = sim.debug_spawn_building_rotated(stove, None, s + Cell::new(2, -3), Rot4::North);
        sim.debug_set_fuel(s + Cell::new(2, -3), 50.0);
        if let Some(recipe) = defs.recipes.id("CookMealSimple") {
            let bill = sim.add_bill(id, recipe);
            sim.edit_bill(id, bill, |b| {
                b.repeat_mode = rimworld_sim::bills::RepeatMode::TargetCount;
                b.target_count = 10;
            });
        }
    }
    // Blueprints for a second room to the west.
    if let (Some(wall), Some(door)) = (thing("Wall"), thing("Door")) {
        for x in -11..=-4 {
            for z in 3..=8 {
                let border = x == -11 || x == -4 || z == 3 || z == 8;
                if border && !(x == -7 && z == 3) {
                    sim.place_blueprint(wall, wood, s + Cell::new(x, z));
                }
            }
        }
        sim.place_blueprint(door, wood, s + Cell::new(-7, 3));
    }
    // Loose wood and steel to haul and build with.
    if let Some(w) = wood {
        for (dx, dz, n) in [(-3, -6, 75), (-1, -7, 75), (1, -6, 60), (-2, -9, 75)] {
            sim.spawn_item(w, s + Cell::new(dx, dz), n);
        }
    }
    if let Some(steel) = thing("Steel") {
        for (dx, dz, n) in [(4, -7, 50), (6, -9, 40)] {
            sim.spawn_item(steel, s + Cell::new(dx, dz), n);
        }
    }
    // A potato field, half ripe.
    if let Some(potato) = thing("Plant_Potato") {
        let cells: Vec<Cell> = (0..5)
            .flat_map(|dz| (0..7).map(move |dx| s + Cell::new(-15 + dx, -9 + dz)))
            .collect();
        sim.designate_growing_zone(potato, &cells);
        let hp = defs.things[potato].stat("MaxHitPoints").unwrap_or(85.0);
        for (k, &c) in cells.iter().enumerate() {
            if k % 7 < 4 {
                sim.map.spawn_plant(potato, c, 1.0, hp);
            }
        }
    }
    // Trees to chop.
    if let Some(oak) = thing("Plant_TreeOak") {
        let hp = defs.things[oak].stat("MaxHitPoints").unwrap_or(200.0);
        let cells: Vec<Cell> = [(-16, 10), (-14, 12), (-17, 13), (-13, 9)]
            .iter()
            .map(|&(dx, dz)| s + Cell::new(dx, dz))
            .collect();
        for &c in &cells {
            sim.map.spawn_plant(oak, c, 1.0, hp);
        }
        sim.designate_harvest_plants(&cells);
    }
    sim.debug_refresh_light();
}

/// After colonists spawn: research to do and one kind of work each
/// (builder/hauler, farmer, researcher).
pub fn stage_colonists(sim: &mut Sim, defs: &GameDefs, colonists: &[rimworld_sim::PawnId]) {
    sim.set_research_project(Some("Batteries"));
    sim.use_work_priorities = true;
    let jobs: [&[(&str, i32)]; 3] = [
        &[("Construction", 1), ("Cooking", 2), ("Hauling", 3)],
        &[
            ("Growing", 1),
            ("PlantCutting", 1),
            ("Cooking", 2),
            ("Hauling", 3),
        ],
        &[("Research", 1), ("Hauling", 2)],
    ];
    for (n, &id) in colonists.iter().enumerate() {
        let mine = jobs[n % jobs.len()];
        for (_, wt) in defs.work_types.iter() {
            let p = mine
                .iter()
                .find(|(name, _)| *name == wt.def_name)
                .map_or(0, |&(_, p)| p);
            sim.set_work_priority(id, &wt.def_name, p);
        }
        sim.set_skill(id, "Intellectual", 8);
        sim.set_skill(id, "Plants", 8);
        sim.set_skill(id, "Construction", 8);
        sim.set_skill(id, "Cooking", 6);
    }
}
