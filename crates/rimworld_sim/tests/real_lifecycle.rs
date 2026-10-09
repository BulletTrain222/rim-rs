//! The wild animal lifecycle against the research's native fixtures
//! (`predator_hunting.md`, `animal_leaving_map.md`; rev591): a Red Fox
//! choosing, hunting and eating a Hare, and wild animals leaving the map.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{JobKind, PredatorStage};
use rimworld_sim::pawn::PawnId;
use rimworld_sim::rand::Rand;
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

/// An isolated area like the research's: soil x75..105, z90..110 inside
/// deep water (the fixtures' cells all on soil), 20 °C, no plants, no
/// spawner (no biome).
fn pen(defs: &Arc<GameDefs>) -> Sim {
    let water = defs.terrain.id("WaterDeep").unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut map = Map::new(GridSize::new(150, 150), water);
    for x in 75..=105 {
        for z in 90..=110 {
            map.terrain[Cell::new(x, z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 20.0;
    sim.debug_set_tick(100_000 - 1);
    sim
}

fn food_of(sim: &Sim, p: PawnId) -> f32 {
    sim.pawn(p)
        .unwrap()
        .needs
        .list
        .iter()
        .find(|n| n.kind == NeedKind::Food)
        .unwrap()
        .level
}

fn job_of(sim: &Sim, p: PawnId) -> Option<JobKind> {
    sim.pawn(p)?.job.as_ref().map(|j| j.kind)
}

/// Multi-prey goldens: Fox at (90,100); healthy Hares at (92,100) and
/// (96,100) score −43.0666656 / −47.0666656 (nearer wins); a farther Hare
/// at 0.546667 health scores −18.27256 and wins; an exact tie keeps the
/// first. Scoring draws nothing.
#[test]
fn prey_selection_matches_the_goldens() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(90, 100)).unwrap();
    let a = sim.spawn_animal("Hare", Cell::new(92, 100)).unwrap();
    let b = sim.spawn_animal("Hare", Cell::new(96, 100)).unwrap();
    assert!(sim.debug_acceptable_prey(fox, a));
    let before = sim.rng_state();
    let (who, score) = sim.debug_best_prey(fox).unwrap();
    assert_eq!(sim.rng_state(), before, "scoring draws nothing");
    assert_eq!(who, a);
    assert_eq!(score, -43.066_666);
    // B weakened to 0.546667 (Hare health scale 0.4: injury severity 13.6).
    sim.debug_add_hediff(b, "Cut", 13.6);
    let (who, score) = sim.debug_best_prey(fox).unwrap();
    assert_eq!(who, b);
    assert!((score - -18.27256).abs() < 1e-4, "{score}");

    // Exact tie: (92,100) and (88,100).
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(90, 100)).unwrap();
    let a = sim.spawn_animal("Hare", Cell::new(92, 100)).unwrap();
    let _b = sim.spawn_animal("Hare", Cell::new(88, 100)).unwrap();
    let (who, score) = sim.debug_best_prey(fox).unwrap();
    assert_eq!(who, a);
    assert_eq!(score, -43.066_666);
}

/// Acceptance: a Muffalo (body size above the Fox's 0.8) is not prey; a
/// Fox below 25% health only takes downed prey.
#[test]
fn prey_acceptance_rules() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(80, 100)).unwrap();
    let muffalo = sim.spawn_animal("Muffalo", Cell::new(84, 100)).unwrap();
    assert!(!sim.debug_acceptable_prey(fox, muffalo));
    assert!(sim.debug_best_prey(fox).is_none());
    let hare = sim.spawn_animal("Hare", Cell::new(86, 100)).unwrap();
    assert_eq!(sim.debug_best_prey(fox).map(|p| p.0), Some(hare));
    // Fox at ~20% health (health scale 0.7: severity 42 → 0.2).
    sim.debug_add_hediff(fox, "Cut", 42.0);
    assert!(sim.debug_best_prey(fox).is_none(), "standing prey refused");
}

/// The food decision: at 30% food (the carnivore threshold) the Fox does
/// nothing; just below it hunts; a fresh corpse lying about is eaten
/// instead of hunting.
#[test]
fn hunger_and_corpses_come_before_hunting() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(85, 100)).unwrap();
    let hare = sim.spawn_animal("Hare", Cell::new(93, 100)).unwrap();
    sim.debug_set_need(fox, NeedKind::Food, 0.3);
    assert!(
        sim.debug_animal_food_job(fox).is_none(),
        "not hungry at 0.3"
    );
    sim.debug_set_need(fox, NeedKind::Food, 0.299);
    assert!(matches!(
        sim.debug_animal_food_job(fox),
        Some(JobKind::PredatorHunt { prey, .. }) if prey == hare
    ));
    let other = sim.spawn_animal("Hare", Cell::new(80, 95)).unwrap();
    sim.debug_kill(other);
    let corpse = sim.corpse_of(other).unwrap();
    assert!(sim.debug_corpse_nutrition(corpse) > 1.0);
    assert!(matches!(
        sim.debug_animal_food_job(fox),
        Some(JobKind::IngestInPlace { food, .. }) if food == corpse
    ));
}

/// `basic3`: a 10%-fed Fox hunts the Hare, keeps attacking it when
/// downed, switches to its (forbidden) corpse when it dies, eats meals
/// 500 ticks apart until at least 90% fed, ends the hunt and leaves the
/// rest of the corpse.
#[test]
fn a_fox_hunts_and_eats_a_hare() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(85, 100)).unwrap();
    let hare = sim.spawn_animal("Hare", Cell::new(93, 100)).unwrap();
    sim.debug_set_need(fox, NeedKind::Food, 0.055);
    let mut hunted = false;
    let mut downed_while_hunting = false;
    let mut meals: Vec<(u64, f32)> = Vec::new();
    let mut last_food = food_of(&sim, fox);
    let mut done = false;
    for _ in 0..8000 {
        sim.debug_set_need(hare, NeedKind::Food, 1.0);
        sim.tick();
        let job = job_of(&sim, fox);
        if matches!(job, Some(JobKind::PredatorHunt { .. })) {
            hunted = true;
        }
        if sim.pawn(hare).unwrap().health.downed
            && !sim.pawn(hare).unwrap().health.dead
            && matches!(job, Some(JobKind::PredatorHunt { .. }))
        {
            downed_while_hunting = true;
        }
        let f = food_of(&sim, fox);
        if f > last_food + 0.01 {
            meals.push((sim.tick_count(), f));
        }
        last_food = f;
        if hunted && !meals.is_empty() && !matches!(job, Some(JobKind::PredatorHunt { .. })) {
            done = true;
            break;
        }
    }
    assert!(hunted && done, "hunt never finished");
    assert!(downed_while_hunting, "a downed hare is attacked on");
    assert!(sim.pawn(hare).unwrap().health.dead);
    for w in meals.windows(2) {
        assert_eq!(w[1].0 - w[0].0, 500, "{meals:?}");
    }
    let pct = food_of(&sim, fox) / 0.55;
    assert!(pct >= 0.9, "{pct}");
    let corpse = sim.corpse_of(hare).expect("the rest of the corpse is left");
    assert!(
        sim.map.item(corpse).unwrap().forbidden,
        "a wild predator's kill"
    );
    eprintln!("meals {meals:?}");
}

/// Save/load in the chase and while chewing continues exactly like the
/// running game (the target, stage, start tick and first-hit flag kept).
#[test]
fn predator_hunt_survives_save_and_load() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let fox = sim.spawn_animal("Fox_Red", Cell::new(85, 100)).unwrap();
    let hare = sim.spawn_animal("Hare", Cell::new(93, 100)).unwrap();
    sim.debug_set_need(fox, NeedKind::Food, 0.055);
    let mut checked = Vec::new();
    for _ in 0..8000 {
        sim.debug_set_need(hare, NeedKind::Food, 1.0);
        sim.tick();
        let label = match job_of(&sim, fox) {
            Some(JobKind::PredatorHunt {
                stage: PredatorStage::Follow,
                ..
            }) => "chase",
            Some(JobKind::PredatorHunt {
                stage:
                    PredatorStage::Chew {
                        ticks_left: 100..=400,
                    },
                ..
            }) => "chewing",
            _ => continue,
        };
        if checked.contains(&label) {
            continue;
        }
        let data = sim.save();
        let mut copy = Sim::load(defs.clone(), &data).expect("loads");
        copy.debug_set_sky_glow(Some(1.0));
        copy.outdoor_temperature = 20.0;
        assert_eq!(job_of(&copy, fox), job_of(&sim, fox), "{label}");
        sim.forget_unsaved_state();
        for _ in 0..300 {
            for s in [&mut sim, &mut copy] {
                if s.pawn(hare).is_some_and(|p| !p.health.dead) {
                    s.debug_set_need(hare, NeedKind::Food, 1.0);
                }
                s.tick();
            }
        }
        assert!(sim.save() == copy.save(), "{label}: diverged after reload");
        checked.push(label);
    }
    assert_eq!(checked, vec!["chase", "chewing"]);
}

/// Two hungry Foxes and one fresh corpse: both eat from it at once (no
/// exclusive claim).
#[test]
fn two_foxes_share_a_corpse() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mut sim = pen(&defs);
    let a = sim.spawn_animal("Fox_Red", Cell::new(85, 100)).unwrap();
    let b = sim.spawn_animal("Fox_Red", Cell::new(87, 100)).unwrap();
    let hare = sim.spawn_animal("Hare", Cell::new(93, 100)).unwrap();
    sim.debug_kill(hare);
    sim.debug_set_need(a, NeedKind::Food, 0.055);
    sim.debug_set_need(b, NeedKind::Food, 0.055);
    let chewing = |s: &Sim, p| {
        matches!(
            job_of(s, p),
            Some(JobKind::IngestInPlace {
                stage: rimworld_sim::job::IngestStage::Chew { .. },
                ..
            })
        )
    };
    let mut together = false;
    for _ in 0..1500 {
        sim.tick();
        if chewing(&sim, a) && chewing(&sim, b) {
            together = true;
            break;
        }
    }
    assert!(together, "both chew at once");
}

/// `TryFindRandomExitSpot` on a clear 250×250 map from seed 123: (0,59)
/// in one try, three draws.
#[test]
fn exit_spot_matches_native() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let sim = Sim::new(defs.clone(), Map::new(GridSize::new(250, 250), soil));
    let view = rimworld_sim::cell_finder::MapView {
        map: &sim.map,
        defs: &defs,
        grid: sim.path_grid(),
        regions: sim.regions(),
    };
    let mut rng = Rand::new(0);
    rng.push_state_seeded(123);
    let spot = rimworld_sim::think::random_exit_spot(&view, Cell::new(20, 100), &mut rng);
    assert_eq!(spot, Some(Cell::new(0, 59)));
    assert_eq!(rng.state(), (123, 3));
}

/// `run2`: a starving Hare with no food anywhere takes a walking exit job
/// to a map edge, keeps it through save/load, leaves there (no corpse) and
/// takes its ecosystem weight with it.
#[test]
fn a_starving_hare_leaves_the_map() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(250, 250), soil));
    sim.outdoor_temperature = 20.0;
    sim.debug_set_tick(100_000 - 1);
    let hare = sim.spawn_animal("Hare", Cell::new(20, 100)).unwrap();
    sim.debug_set_need(hare, NeedKind::Food, 0.0);
    assert!((sim.wild_animal_weight() - 0.25).abs() < 1e-6);
    let mut dest = None;
    for _ in 0..200 {
        sim.tick();
        if let Some(JobKind::ExitMap { dest: d }) = job_of(&sim, hare) {
            dest = Some(d);
            assert_eq!(
                sim.pawn(hare).unwrap().job.as_ref().unwrap().urgency,
                rimworld_sim::path::LocomotionUrgency::Walk
            );
            break;
        }
    }
    let dest = dest.expect("an exit job");
    assert!(dest.x == 0 || dest.z == 0 || dest.x == 249 || dest.z == 249);
    for _ in 0..30 {
        sim.tick();
    }
    let data = sim.save();
    let mut copy = Sim::load(defs.clone(), &data).expect("loads");
    assert_eq!(job_of(&copy, hare), job_of(&sim, hare));
    copy.outdoor_temperature = 20.0;
    sim.forget_unsaved_state();
    // Walking (50 ticks a cell) to a random edge.
    for _ in 0..20_000 {
        sim.tick();
        copy.tick();
        if sim.pawn(hare).is_none() {
            break;
        }
    }
    assert!(sim.pawn(hare).is_none(), "left the map");
    assert!(sim.corpse_of(hare).is_none());
    assert_eq!(sim.wild_animal_weight(), 0.0);
    for _ in 0..5 {
        copy.tick();
    }
    assert!(copy.pawn(hare).is_none(), "left after reload too");
}

/// Wrong season: outside the Hare's comfortable range it walks off.
#[test]
fn wrong_season_sends_animals_away() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(100, 100), soil));
    sim.outdoor_temperature = 60.0;
    let hare = sim.spawn_animal("Hare", Cell::new(50, 50)).unwrap();
    let mut left = false;
    for _ in 0..300 {
        sim.tick();
        if let Some(JobKind::ExitMap { .. }) = job_of(&sim, hare) {
            left = true;
            break;
        }
    }
    assert!(left, "no exit job in the wrong season");
}

/// The random-departure node keeps its last try in the pawn's think data
/// and tries at most once per 2500 ticks.
#[test]
fn random_departure_is_tried_hourly() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(100, 100), soil));
    sim.outdoor_temperature = 20.0;
    let hare = sim.spawn_animal("Hare", Cell::new(50, 50)).unwrap();
    let mut seen: Vec<i64> = Vec::new();
    for _ in 0..12_000 {
        sim.debug_set_need(hare, NeedKind::Food, 1.0);
        sim.tick();
        let Some(p) = sim.pawn(hare) else { break };
        for &v in p.mind.think_data.values() {
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
    }
    assert!(seen.len() >= 2, "{seen:?}");
    for w in seen.windows(2) {
        assert!(w[1] - w[0] >= 2500, "{seen:?}");
    }
}
