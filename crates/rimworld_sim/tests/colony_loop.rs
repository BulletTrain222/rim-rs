//! The colony loop with the real game data: colonists given a stockpile,
//! a growing zone and blueprints for a walled bedroom build it, farm,
//! haul, eat and sleep on their own, deterministically. Skipped without an
//! install.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{JobKind, Rot4};
use rimworld_sim::storage::StoragePriority;
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

/// What happened during a run.
#[derive(Debug, Default, PartialEq)]
struct Log {
    jobs: BTreeSet<String>,
    ate_rice: bool,
    slept_in_bed: BTreeSet<u32>,
    harvested: bool,
}

struct Colony {
    sim: Sim,
    walls: Vec<Cell>,
    door: Cell,
    beds: Vec<Cell>,
    field: Vec<Cell>,
}

fn colony(defs: Arc<GameDefs>, seed: u64) -> Colony {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(48, 48), soil), seed);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    for (n, name) in ["A", "B", "C"].iter().enumerate() {
        let p = sim
            .spawn_pawn(kind, *name, Cell::new(20 + n as i32, 20))
            .unwrap();
        sim.set_skill(p, "Construction", 8);
        sim.set_skill(p, "Plants", 8);
        sim.initialize_work(p);
    }
    let thing = |n: &str| defs.things.id(n).unwrap();
    // Starting things (scattered as a scenario would; packaged survival
    // meals, as Crashlanded gives, do not rot).
    sim.spawn_item(thing("Steel"), Cell::new(18, 24), 75);
    // Walls (5 each), the door (25) and beds (45 each) need ~370 wood.
    for x in 16..22 {
        sim.spawn_item(thing("WoodLog"), Cell::new(x, 25), 75);
    }
    for z in 0..5 {
        sim.spawn_item(thing("MealSurvivalPack"), Cell::new(23, 24 + z), 10);
    }
    // Player orders: a stockpile, a rice field and a 7x6 walled bedroom
    // with a door and three beds.
    let stockpile: Vec<Cell> = (30..36)
        .flat_map(|x| (20..24).map(move |z| Cell::new(x, z)))
        .collect();
    let filter = rimworld_sim::storage::ThingFilter::preset(
        &sim.defs,
        rimworld_sim::storage::StoragePreset::DefaultStockpile,
    );
    sim.map
        .storage
        .add_stockpile(StoragePriority::Normal, &stockpile, filter);
    let field: Vec<Cell> = (8..14)
        .flat_map(|x| (8..14).map(move |z| Cell::new(x, z)))
        .collect();
    sim.designate_growing_zone(thing("Plant_Rice"), &field)
        .unwrap();
    let (x0, x1, z0, z1) = (24, 30, 30, 35);
    let door = Cell::new(27, z0);
    let mut walls = Vec::new();
    for x in x0..=x1 {
        for z in z0..=z1 {
            let c = Cell::new(x, z);
            if (x == x0 || x == x1 || z == z0 || z == z1) && c != door {
                walls.push(c);
            }
        }
    }
    for &c in &walls {
        sim.place_blueprint(thing("Wall"), Some(thing("WoodLog")), c)
            .unwrap();
    }
    sim.place_blueprint(thing("Door"), Some(thing("WoodLog")), door)
        .unwrap();
    let beds = vec![Cell::new(25, 32), Cell::new(27, 32), Cell::new(29, 32)];
    for &c in &beds {
        sim.place_blueprint_rotated(thing("Bed"), Some(thing("WoodLog")), c, Rot4::North)
            .unwrap();
    }
    Colony {
        sim,
        walls,
        door,
        beds,
        field,
    }
}

fn run(c: &mut Colony, ticks: u32) -> Log {
    let mut log = Log::default();
    let defs = c.sim.defs.clone();
    let rice = defs.things.id("RawRice").unwrap();
    for _ in 0..ticks {
        c.sim.tick();
        for p in c.sim.pawns() {
            let Some(job) = p.job.as_ref() else { continue };
            if let Some(d) = job.def {
                log.jobs.insert(defs.jobs[d].def_name.clone());
            }
            match job.kind {
                JobKind::Ingest { .. } if p.carried.is_some_and(|x| x.def == rice) => {
                    log.ate_rice = true;
                }
                JobKind::LayDown { bed: Some(_), spot } if p.position == spot && p.asleep => {
                    log.slept_in_bed.insert(p.id.0);
                }
                JobKind::Harvest { .. } => log.harvested = true,
                _ => {}
            }
        }
    }
    log
}

#[test]
fn colonists_build_farm_eat_and_sleep() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut c = colony(defs.clone(), 21);
    let log = run(&mut c, 10 * 60_000);
    let sim = &c.sim;
    eprintln!("jobs seen: {:?}", log.jobs);
    let wall = defs.things.id("Wall").unwrap();
    let door = defs.things.id("Door").unwrap();
    let bed = defs.things.id("Bed").unwrap();
    // The bedroom was built: walls, door, beds; and it is its own room.
    for &w in &c.walls {
        assert_eq!(sim.map.buildings[w], Some(wall), "wall at {w:?}");
    }
    assert_eq!(sim.map.buildings[c.door], Some(door));
    assert!(sim.map.door_at(c.door).is_some());
    for &b in &c.beds {
        assert_eq!(sim.map.buildings[b], Some(bed), "bed at {b:?}");
    }
    let inside = Cell::new(27, 33);
    assert_ne!(
        sim.regions().room_at(inside),
        sim.regions().room_at(Cell::new(10, 30))
    );
    // Every colonist owns a different bed and has slept in one.
    let owned: BTreeSet<_> = sim.pawns().iter().filter_map(|p| p.owned_bed).collect();
    assert_eq!(owned.len(), 3, "three distinct beds owned");
    assert_eq!(log.slept_in_bed.len(), 3, "everyone slept in a bed");
    // The field was sown, grew and was harvested; rice was eaten.
    assert!(log.jobs.contains("Sow") && log.harvested);
    assert!(
        c.field
            .iter()
            .filter(|&&f| sim.map.plant_at(f).is_some())
            .count()
            > 20
    );
    assert!(log.ate_rice, "harvested rice was eaten");
    // Hauling and construction jobs ran.
    for j in [
        "HaulToCell",
        "HaulToContainer",
        "FinishFrame",
        "LayDown",
        "Ingest",
    ] {
        assert!(log.jobs.contains(j), "{j} never ran");
    }
    // Nobody went hungry for long: malnutrition no worse than its first
    // (trivial) stage. A colonist can still end the run hungry: colonists
    // only eat raw food once urgently hungry, so one who goes to bed just
    // before that sleeps on hungry (the game wakes sleepers only for
    // rest) and eats when it wakes.
    let malnutrition = defs.hediffs.id("Malnutrition");
    let trivial = malnutrition
        .and_then(|m| defs.hediffs[m].stages.get(1))
        .map_or(f32::MAX, |s| s.min_severity);
    for p in sim.pawns() {
        let food = p.needs.get(NeedKind::Food).unwrap();
        eprintln!(
            "{} food {:.2} at {:?}",
            p.name,
            food.level / food.max,
            p.position
        );
        assert!(!p.health.dead, "{} died", p.name);
        let mal = p
            .health
            .hediffs
            .iter()
            .find(|h| Some(h.def) == malnutrition)
            .map_or(0.0, |h| h.severity);
        assert!(mal < trivial, "{} malnourished ({mal})", p.name);
    }
}

#[test]
fn the_colony_loop_is_deterministic() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let snapshot = |c: &Colony| {
        let s = &c.sim;
        (
            s.pawns()
                .iter()
                .map(|p| (p.position, p.carried.map(|x| x.count), p.owned_bed))
                .collect::<Vec<_>>(),
            s.map
                .items()
                .iter()
                .map(|i| (i.id, i.position, i.stack_count))
                .collect::<Vec<_>>(),
            s.map
                .plants()
                .iter()
                .map(|p| (p.position, p.growth.to_bits()))
                .collect::<Vec<_>>(),
            s.map.constructibles().len(),
        )
    };
    let mut a = colony(defs.clone(), 5);
    let mut b = colony(defs.clone(), 5);
    let la = run(&mut a, 150_000);
    let lb = run(&mut b, 150_000);
    assert_eq!(la, lb);
    assert_eq!(snapshot(&a), snapshot(&b));
}

#[test]
fn a_loaded_game_continues_exactly_like_the_original() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut original = colony(defs.clone(), 33);
    run(&mut original, 120_000);
    let data = original.sim.save();
    // As in the game, loading resets what is not saved (mood observer and
    // caches, joy gain tick); the original forgets it too.
    original.sim.forget_unsaved_state();
    eprintln!("save size: {} bytes", data.len());
    let loaded = Sim::load(defs.clone(), &data).expect("loads");
    // Saving the loaded game gives the same file.
    assert_eq!(loaded.save(), data);
    let mut copy = Colony {
        sim: loaded,
        walls: original.walls.clone(),
        door: original.door,
        beds: original.beds.clone(),
        field: original.field.clone(),
    };
    let la = run(&mut original, 200_000);
    let lb = run(&mut copy, 200_000);
    assert_eq!(la, lb);
    assert_eq!(
        original.sim.save(),
        copy.sim.save(),
        "identical after continuing"
    );
}

#[test]
fn saves_are_checked_on_load() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let c = colony(defs.clone(), 1);
    let data = c.sim.save();
    assert!(matches!(
        Sim::load(defs.clone(), "not json"),
        Err(rimworld_sim::LoadError::Format(_))
    ));
    let bumped = data.replacen("\"version\":1", "\"version\":99", 1);
    assert!(matches!(
        Sim::load(defs.clone(), &bumped),
        Err(rimworld_sim::LoadError::Version(99))
    ));
    let tampered = {
        let v: serde_json_shim::Value = data.as_str().into();
        v.with_fingerprint(12345)
    };
    assert!(matches!(
        Sim::load(defs, &tampered),
        Err(rimworld_sim::LoadError::Defs)
    ));
}

/// Minimal helper to rewrite the fingerprint without a JSON dependency in
/// the test crate.
mod serde_json_shim {
    pub struct Value(String);
    impl From<&str> for Value {
        fn from(s: &str) -> Self {
            Value(s.to_owned())
        }
    }
    impl Value {
        pub fn with_fingerprint(&self, f: u64) -> String {
            let key = "\"defs_fingerprint\":";
            let start = self.0.find(key).unwrap() + key.len();
            let end = start + self.0[start..].find(',').unwrap();
            format!("{}{}{}", &self.0[..start], f, &self.0[end..])
        }
    }
}

/// A longer run into winter with every system on (climate,
/// room temperatures, apparel, illness, rescue, filth, weathering):
/// nothing panics, and the colonists come through it.
#[test]
fn a_colony_lives_through_a_cold_season() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut c = colony(defs.clone(), 33);
    c.sim.climate = Some(rimworld_sim::climate::Climate {
        tile_temperature: 8.0,
    });
    c.sim.latitude = 45.0;
    let colonists: Vec<_> = c.sim.pawns().iter().map(|p| p.id).collect();
    for &p in &colonists {
        c.sim.give_starting_apparel(p);
    }
    // Without hunting or foraging, these weeks need more than the field and
    // the starting meals: a store of survival meals (frozen ones keep).
    let meal = defs.things.id("MealSurvivalPack").unwrap();
    for k in 0..25 {
        c.sim
            .spawn_item(meal, Cell::new(36 + k % 5, 30 + k / 5), 10);
    }
    // Start late in fall and run into deep winter (the eleventh twelfth is
    // the coldest).
    c.sim.debug_set_tick(40 * 60_000);
    let log = run(&mut c, 25 * 60_000);
    eprintln!("jobs seen: {:?}", log.jobs);
    eprintln!("outdoors {:.1} °C", c.sim.outdoor_temperature);
    for p in c.sim.pawns() {
        let hediffs: Vec<String> = p
            .health
            .hediffs
            .iter()
            .map(|h| format!("{} {:.2}", defs.hediffs[h.def].def_name, h.severity))
            .collect();
        eprintln!(
            "{}: dead {} downed {} at {:?} hediffs {:?}",
            p.name, p.health.dead, p.health.downed, p.position, hediffs
        );
    }
    let alive = c.sim.pawns().iter().filter(|p| !p.health.dead).count();
    assert_eq!(alive, 3, "everyone survived");
}
