//! Playable milestone 1: the basic colony loop driven only through the
//! orders the app gives (stockpile, growing zone, blueprints, bills,
//! research), with save/reload at several points. Three colonists with
//! starting wood, steel and a little food must build a roofed bedroom with
//! beds, a fuelled stove and a research bench, grow and harvest rice, cook
//! simple meals from it through a target-count bill, eat them, sleep in
//! their beds and research — and every reload must continue exactly like
//! the running game. Skipped without an install.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::bills::RepeatMode;
use rimworld_sim::job::{JobKind, Rot4};
use rimworld_sim::storage::{StoragePreset, StoragePriority, ThingFilter};
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

#[derive(Debug, Default)]
struct Seen {
    jobs: BTreeSet<String>,
    ate: BTreeSet<String>,
    slept_in_bed: BTreeSet<u32>,
    meals_cooked: bool,
    rice_harvested: bool,
    bill_added: bool,
    research_started: bool,
    downed: BTreeSet<String>,
}

const ROOM: (i32, i32, i32, i32) = (24, 32, 30, 36);

fn start(defs: &Arc<GameDefs>) -> Sim {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(56, 56), soil), 7);
    sim.set_default_update_rate(1);
    sim.apply_starting_research("PlayerColony");
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    for (n, name) in ["Ada", "Brin", "Cole"].iter().enumerate() {
        let p = sim
            .spawn_pawn(kind, *name, Cell::new(20 + n as i32, 20))
            .unwrap();
        for skill in ["Construction", "Plants", "Cooking", "Intellectual"] {
            sim.set_skill(p, skill, 6);
        }
        sim.initialize_work(p);
        sim.give_starting_apparel(p);
    }
    sim.give_colonists_memory("NewColonyOptimism");
    let thing = |n: &str| defs.things.id(n).unwrap();
    for x in 14..24 {
        sim.spawn_item(thing("WoodLog"), Cell::new(x, 26), 75);
    }
    sim.spawn_item(thing("Steel"), Cell::new(14, 27), 75);
    sim.spawn_item(thing("Steel"), Cell::new(15, 27), 75);
    // Crashlanded's 50 packaged survival meals.
    for z in 0..5 {
        sim.spawn_item(thing("MealSurvivalPack"), Cell::new(26, 22 + z), 10);
    }
    sim.spawn_item(thing("RawRice"), Cell::new(27, 24), 40);
    // Orders as the app gives them.
    let stockpile: Vec<Cell> = (34..42)
        .flat_map(|x| (18..24).map(move |z| Cell::new(x, z)))
        .collect();
    let filter = ThingFilter::preset(&sim.defs, StoragePreset::DefaultStockpile);
    sim.map
        .storage
        .add_stockpile(StoragePriority::Normal, &stockpile, filter);
    let field: Vec<Cell> = (6..14)
        .flat_map(|x| (6..14).map(move |z| Cell::new(x, z)))
        .collect();
    sim.designate_growing_zone(thing("Plant_Rice"), &field)
        .unwrap();
    let (x0, z0, x1, z1) = ROOM;
    let door = Cell::new(27, z0);
    for x in x0..=x1 {
        for z in z0..=z1 {
            let c = Cell::new(x, z);
            if (x == x0 || x == x1 || z == z0 || z == z1) && c != door {
                sim.place_blueprint(thing("Wall"), Some(thing("WoodLog")), c)
                    .unwrap();
            }
        }
    }
    sim.place_blueprint(thing("Door"), Some(thing("WoodLog")), door)
        .unwrap();
    for x in [25, 27, 29] {
        sim.place_blueprint_rotated(
            thing("Bed"),
            Some(thing("WoodLog")),
            Cell::new(x, 35),
            Rot4::South,
        )
        .unwrap();
    }
    sim.place_blueprint_rotated(thing("FueledStove"), None, Cell::new(30, 26), Rot4::South)
        .unwrap();
    sim.place_blueprint_rotated(
        thing("SimpleResearchBench"),
        Some(thing("WoodLog")),
        Cell::new(20, 30),
        Rot4::South,
    )
    .unwrap();
    sim
}

/// Player follow-up orders once the buildings exist: a target-count simple
/// meal bill on the stove and a research project.
fn follow_up(sim: &mut Sim, seen: &mut Seen) {
    let defs = sim.defs.clone();
    if !seen.bill_added {
        let stove = defs.things.id("FueledStove").unwrap();
        if let Some(id) = sim
            .map
            .structures()
            .iter()
            .find(|s| s.def == stove)
            .map(|s| s.id)
        {
            let recipe = defs.recipes.id("CookMealSimple").unwrap();
            let bill = sim.add_bill(id, recipe);
            sim.edit_bill(id, bill, |b| {
                b.repeat_mode = RepeatMode::TargetCount;
                b.target_count = 10;
            });
            seen.bill_added = true;
        }
    }
    if !seen.research_started {
        let bench = defs.things.id("SimpleResearchBench").unwrap();
        if sim.map.structures().iter().any(|s| s.def == bench) {
            let project = defs
                .research
                .iter()
                .map(|(_, p)| p.def_name.clone())
                .find(|p| sim.can_start_research(p))
                .expect("a startable project");
            assert!(sim.set_research_project(Some(&project)));
            seen.research_started = true;
        }
    }
}

fn run(sim: &mut Sim, ticks: u64, seen: &mut Seen) {
    let defs = sim.defs.clone();
    let meal = defs.things.id("MealSimple").unwrap();
    let rice_plant = defs.things.id("Plant_Rice").unwrap();
    for _ in 0..ticks {
        sim.tick();
        follow_up(sim, seen);
        for p in sim.pawns() {
            if let Some(job) = &p.job {
                if let Some(d) = job.def {
                    seen.jobs.insert(defs.jobs[d].def_name.clone());
                }
                if let JobKind::LayDown { bed: Some(bed), .. } = job.kind
                    && p.asleep
                {
                    seen.slept_in_bed.insert(bed.0);
                }
                if matches!(job.kind, JobKind::Ingest { .. })
                    && let Some(c) = p.carried
                {
                    seen.ate.insert(defs.things[c.def].def_name.clone());
                }
            }
        }
        for p in sim.pawns() {
            if p.health.downed && !seen.downed.contains(&p.name) {
                seen.downed.insert(p.name.clone());
                let hs: Vec<String> = p
                    .health
                    .hediffs
                    .iter()
                    .map(|h| format!("{}:{:.3}", defs.hediffs[h.def].def_name, h.severity))
                    .collect();
                eprintln!(
                    "tick {} {} downed: {hs:?} food {:?} rest {:?}",
                    sim.tick_count(),
                    p.name,
                    p.needs.get(rimworld_sim::NeedKind::Food).map(|n| n.level),
                    p.needs.rest_level()
                );
            }
        }
        if !seen.meals_cooked && sim.map.items().iter().any(|i| i.def == meal) {
            seen.meals_cooked = true;
        }
        if !seen.rice_harvested && seen.jobs.contains("Harvest") {
            seen.rice_harvested = !sim
                .map
                .plants()
                .iter()
                .all(|p| p.def != rice_plant || p.growth < 1.0)
                || seen.jobs.contains("Harvest");
        }
    }
}

/// Saves, reloads and checks the reload continues exactly like the
/// running game; returns the reloaded game.
fn reload(sim: &mut Sim, defs: &Arc<GameDefs>, seen: &mut Seen, label: &str) -> Sim {
    let data = sim.save();
    let loaded = Sim::load(defs.clone(), &data).unwrap_or_else(|e| panic!("{label}: load: {e:?}"));
    assert_eq!(
        loaded.save(),
        data,
        "{label}: save of the loaded game differs"
    );
    sim.forget_unsaved_state();
    let mut copy = loaded;
    let mut a = Seen::default();
    let mut b = Seen::default();
    a.bill_added = seen.bill_added;
    b.bill_added = seen.bill_added;
    a.research_started = seen.research_started;
    b.research_started = seen.research_started;
    for t in 0..2_000 {
        run(sim, 1, &mut a);
        run(&mut copy, 1, &mut b);
        if std::env::var_os("FERROCOLONY_BISECT").is_some() && sim.save() != copy.save() {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local");
            let _ = std::fs::write(dir.join("diverged_a.json"), sim.save());
            let _ = std::fs::write(dir.join("diverged_b.json"), copy.save());
            panic!("{label}: first divergence {t} ticks after reload");
        }
    }
    let (sa, sb) = (sim.save(), copy.save());
    if sa != sb {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local");
        let _ = std::fs::write(dir.join("diverged_a.json"), &sa);
        let _ = std::fs::write(dir.join("diverged_b.json"), &sb);
        panic!("{label}: reload diverged (saves in local/diverged_*.json)");
    }
    seen.jobs.extend(b.jobs);
    seen.ate.extend(b.ate);
    seen.slept_in_bed.extend(b.slept_in_bed);
    copy
}

#[test]
fn the_basic_colony_loop_can_be_played_and_resumed() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = start(&defs);
    let mut seen = Seen::default();
    // Construction in progress.
    run(&mut sim, 8_000, &mut seen);
    sim = reload(&mut sim, &defs, &mut seen, "building");
    // Up to day 3: buildings done, field sown, cooking and research going.
    run(&mut sim, 140_000, &mut seen);
    sim = reload(&mut sim, &defs, &mut seen, "day 3");
    run(&mut sim, 150_000, &mut seen);
    sim = reload(&mut sim, &defs, &mut seen, "day 5");
    run(&mut sim, 300_000, &mut seen);
    eprintln!("{seen:#?}");
    let defs2 = sim.defs.clone();
    let thing = |n: &str| defs2.things.id(n).unwrap();
    let built = |n: &str| {
        sim.map
            .structures()
            .iter()
            .filter(|s| s.def == thing(n))
            .count()
    };
    assert_eq!(built("Wall"), 19, "walls");
    assert_eq!(built("Door"), 1, "door");
    assert_eq!(built("Bed"), 3, "beds");
    assert_eq!(built("FueledStove"), 1, "stove");
    assert_eq!(built("SimpleResearchBench"), 1, "bench");
    let (x0, z0, x1, z1) = ROOM;
    let inside = Cell::new((x0 + x1) / 2, (z0 + z1) / 2);
    assert!(sim.map.roofed(inside), "the bedroom is roofed");
    assert!(seen.bill_added && seen.research_started);
    for job in [
        "Sow",
        "Harvest",
        "HaulToCell",
        "DoBill",
        "Research",
        "Ingest",
        "LayDown",
    ] {
        assert!(seen.jobs.contains(job), "no {job} job: {:?}", seen.jobs);
    }
    assert!(seen.meals_cooked, "no simple meal was cooked");
    assert!(
        seen.ate.contains("MealSimple"),
        "nobody ate a simple meal: {:?}",
        seen.ate
    );
    assert!(!seen.slept_in_bed.is_empty(), "nobody slept in a bed");
    let project = sim.current_research().map(str::to_owned);
    let progressed = project.is_none_or(|p| sim.research_progress(&p) > 0.0)
        || defs
            .research
            .iter()
            .any(|(_, p)| sim.research_finished(&p.def_name));
    assert!(progressed, "no research progress");
    for p in sim.pawns() {
        assert!(!p.health.dead, "{} died", p.name);
    }
}
