//! Cooking with the real game Defs (skipped without an install): a
//! colonist cooks simple meals at a fueled stove from ordered bills,
//! following the scenarios of the external cooking report (fixtures A, B,
//! C and D, re-run in our simulation).

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::bills::{RepeatMode, StoreMode};
use rimworld_sim::job::{DoBillStage, JobKind};
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

struct Kitchen {
    defs: Arc<GameDefs>,
    sim: Sim,
    stove: rimworld_sim::map::ItemId,
    pawn: rimworld_sim::pawn::PawnId,
}

/// A fueled stove facing north at (20, 20) on concrete, outdoors at 20 °C
/// (table factor 0.8), a Cooking 10 colonist who only cooks.
fn kitchen() -> Option<Kitchen> {
    let defs = real_defs()?;
    let concrete = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(40, 40), concrete));
    sim.set_default_update_rate(15);
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 20.0;
    let stove_def = defs.things.id("FueledStove").unwrap();
    let stove = sim.debug_spawn_building(stove_def, None, Cell::new(20, 20));
    sim.debug_set_fuel(Cell::new(20, 20), 50.0);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "Cook", Cell::new(20, 16)).unwrap();
    sim.set_update_rate(pawn, 15);
    sim.set_skill(pawn, "Cooking", 10);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Cooking" { 1 } else { 0 });
    }
    Some(Kitchen {
        defs,
        sim,
        stove,
        pawn,
    })
}

impl Kitchen {
    fn tick(&mut self) {
        self.sim.debug_set_need(self.pawn, NeedKind::Rest, 1.0);
        self.sim.debug_set_need(self.pawn, NeedKind::Food, 1.0);
        self.sim.debug_set_need(self.pawn, NeedKind::Joy, 1.0);
        self.sim.tick();
    }

    fn count(&self, def: &str) -> u32 {
        let d = self.defs.things.id(def).unwrap();
        self.sim
            .map
            .items()
            .iter()
            .filter(|i| i.def == d)
            .map(|i| i.stack_count)
            .sum()
    }

    fn job_name(&self) -> String {
        self.sim
            .pawn(self.pawn)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| self.defs.jobs[d].def_name.clone())
    }
}

/// Fixture A: one simple meal from a rice stack of 75; ten rice taken
/// (0.5 nutrition at 0.05), consumed only when the meal is made; work
/// 300 at 0.8 × CookSpeed 1, 12 per 15-tick interval; one meal dropped,
/// its ingredients RawRice; the bill's count goes to 0; fuel burnt only
/// while used.
#[test]
fn a_colonist_cooks_one_simple_meal() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rice = k.defs.things.id("RawRice").unwrap();
    k.sim.spawn_item(rice, Cell::new(24, 18), 75);
    let recipe = k.defs.recipes.id("CookMealSimple").unwrap();
    let bill = k.sim.add_bill(k.stove, recipe);
    k.sim
        .edit_bill(k.stove, bill, |b| b.store_mode = StoreMode::DropOnFloor);
    assert_eq!(k.sim.pawn_stat(k.pawn, "CookSpeed"), Some(1.0));
    let mut saw_queue = false;
    let mut works: Vec<f32> = Vec::new();
    let mut fuel_before_work = None;
    for _ in 0..3000 {
        k.tick();
        let p = k.sim.pawn(k.pawn).unwrap().clone();
        if let Some(JobKind::DoBill { stage, .. }) = p.job.as_ref().map(|j| j.kind) {
            if !saw_queue && !p.count_queue.is_empty() {
                saw_queue = true;
            }
            if let DoBillStage::Work { work_left, .. } = stage {
                fuel_before_work.get_or_insert(k.sim.fuel_at(Cell::new(20, 20)).unwrap());
                if works.last() != Some(&work_left) {
                    works.push(work_left);
                }
                // Ingredients stay on the table while cooking.
                assert_eq!(k.count("RawRice"), 75, "rice kept until done");
            }
        }
        if k.count("MealSimple") > 0 {
            break;
        }
    }
    assert!(saw_queue || works.len() > 1, "a bill job ran");
    assert_eq!(k.count("MealSimple"), 1);
    assert_eq!(k.count("RawRice"), 65, "ten rice used");
    assert_eq!(k.sim.bills(k.stove)[0].repeat_count, 0);
    // 300 work: 300, 288, 276, ... in steps of 12.
    assert_eq!(works[0], 300.0);
    for w in works.windows(2) {
        assert!((w[0] - w[1] - 12.0).abs() < 1e-3, "{works:?}");
    }
    let meal = k
        .sim
        .map
        .items()
        .iter()
        .find(|i| k.defs.things[i.def].def_name == "MealSimple")
        .unwrap()
        .id;
    let meta = k.sim.item_meta(meal).cloned().unwrap_or_default();
    assert_eq!(meta.ingredients, vec![rice]);
    let fuel = k.sim.fuel_at(Cell::new(20, 20)).unwrap();
    assert!(fuel < fuel_before_work.unwrap(), "fuel used while cooking");
    assert!(fuel > 48.0, "only while used: {fuel}");
    // Nothing more to do: the bill is done.
    for _ in 0..600 {
        k.tick();
    }
    assert_eq!(k.count("MealSimple"), 1);
    assert_ne!(k.job_name(), "DoBill");
}

/// A cook interrupted after putting the rice on the stove starts over and
/// still makes the meal (the placed things belong to the job).
#[test]
fn an_interrupted_bill_is_finished_later() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rice = k.defs.things.id("RawRice").unwrap();
    k.sim.spawn_item(rice, Cell::new(24, 18), 75);
    let recipe = k.defs.recipes.id("CookMealSimple").unwrap();
    let bill = k.sim.add_bill(k.stove, recipe);
    k.sim
        .edit_bill(k.stove, bill, |b| b.store_mode = StoreMode::DropOnFloor);
    let mut interrupted = false;
    for _ in 0..6000 {
        k.tick();
        let working = matches!(
            k.sim.pawn(k.pawn).unwrap().job.as_ref().map(|j| j.kind),
            Some(JobKind::DoBill {
                stage: DoBillStage::Work { .. },
                ..
            })
        );
        if working && !interrupted {
            k.sim.debug_find_and_start_job(k.pawn);
            interrupted = true;
        }
        if k.count("MealSimple") > 0 {
            break;
        }
    }
    assert!(interrupted);
    assert_eq!(k.count("MealSimple"), 1);
    assert_eq!(k.count("RawRice"), 65);
}

/// Fixture B: the ×4 bill takes 40 rice and makes one stack of 4.
#[test]
fn a_bulk_bill_makes_one_stack_of_four() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rice = k.defs.things.id("RawRice").unwrap();
    k.sim.spawn_item(rice, Cell::new(24, 18), 75);
    let recipe = k.defs.recipes.id("CookMealSimpleBulk").unwrap();
    let bill = k.sim.add_bill(k.stove, recipe);
    k.sim
        .edit_bill(k.stove, bill, |b| b.store_mode = StoreMode::DropOnFloor);
    let mut steps: Vec<(i32, f32)> = Vec::new();
    for _ in 0..6000 {
        k.tick();
        if let Some(JobKind::DoBill {
            stage: DoBillStage::Work { work_left, spent },
            ..
        }) = k.sim.pawn(k.pawn).unwrap().job.as_ref().map(|j| j.kind)
            && steps.last() != Some(&(spent, work_left))
        {
            steps.push((spent, work_left));
        }
        if k.count("MealSimple") > 0 {
            break;
        }
    }
    // Every interval subtracts 0.8 × delta at the game's precision; with
    // 15-tick intervals the last one before the end is the report's
    // (1485, 12.0) and the next finishes.
    let b = f32::from_bits(0x3F4C_CCCD);
    for w in steps.windows(2) {
        let delta = w[1].0 - w[0].0;
        assert_eq!(
            w[1].1.to_bits(),
            rimworld_sim::bills::subtract_work(w[0].1, b, delta).to_bits(),
            "{w:?}"
        );
    }
    if steps.iter().skip(1).all(|&(s, _)| s % 15 == 0) {
        assert_eq!(
            steps.last().map(|&(s, w)| (s, w.to_bits())),
            Some((1485, 0x4140_0000))
        );
    }
    let meals: Vec<u32> = k
        .sim
        .map
        .items()
        .iter()
        .filter(|i| k.defs.things[i.def].def_name == "MealSimple")
        .map(|i| i.stack_count)
        .collect();
    assert_eq!(meals, vec![4], "one thing, stack of 4");
    assert_eq!(k.count("RawRice"), 35);
}

/// Fixture C: with no ingredients in reach the bill waits 500–600 ticks
/// before searching again; ingredients arriving meanwhile are not used
/// until then.
#[test]
fn a_failed_search_waits_before_retrying() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let recipe = k.defs.recipes.id("CookMealSimple").unwrap();
    let bill = k.sim.add_bill(k.stove, recipe);
    k.sim
        .edit_bill(k.stove, bill, |b| b.store_mode = StoreMode::DropOnFloor);
    for _ in 0..30 {
        k.tick();
    }
    let next = k.sim.bills(k.stove)[0].next_search_tick;
    let now = k.sim.tick_count();
    assert!(
        next > now && next <= now + 600,
        "cooldown set: {next} at {now}"
    );
    let rice = k.defs.things.id("RawRice").unwrap();
    k.sim.spawn_item(rice, Cell::new(24, 18), 20);
    while k.sim.tick_count() < next {
        k.tick();
        assert_ne!(k.job_name(), "DoBill", "still waiting");
    }
    for _ in 0..200 {
        k.tick();
        if k.job_name() == "DoBill" {
            break;
        }
    }
    assert_eq!(k.job_name(), "DoBill", "searched again after the cooldown");
}

/// Fixture D in play: a target-count bill cooks until the counted stock
/// reaches its target. Stored things are counted by the resource counter
/// every 204 ticks (and when a target-count bill finishes, when the new
/// meal is still in hand), so a meal stored just before the next search
/// can be missed and one more is cooked — here, 3 for a target of 2 —
/// after which the bill stops.
#[test]
fn a_target_count_bill_stops_at_its_target() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let rice = k.defs.things.id("RawRice").unwrap();
    k.sim.spawn_item(rice, Cell::new(24, 18), 75);
    let cells: Vec<Cell> = (0..3)
        .flat_map(|dz| (0..3).map(move |dx| Cell::new(14 + dx, 14 + dz)))
        .collect();
    k.sim.designate_stockpile(&cells);
    let recipe = k.defs.recipes.id("CookMealSimple").unwrap();
    let bill = k.sim.add_bill(k.stove, recipe);
    k.sim.edit_bill(k.stove, bill, |b| {
        b.repeat_mode = RepeatMode::TargetCount;
        b.target_count = 2;
    });
    for _ in 0..12_000 {
        k.tick();
    }
    let stored: u32 = k
        .sim
        .map
        .items()
        .iter()
        .filter(|i| {
            k.defs.things[i.def].def_name == "MealSimple"
                && k.sim.map.storage.zone_at(i.position).is_some()
        })
        .map(|i| i.stack_count)
        .sum();
    assert_eq!(stored, 3, "the target plus one cooked on a stale count");
    assert_eq!(k.count("MealSimple"), 3);
    assert_eq!(k.count("RawRice"), 45);
}

/// The report's fixture E: candidates given farthest first — chicken 10,
/// corn 10, rice 5, each 0.05 nutrition — sort by nutrition then distance
/// and take rice 5 then corn 5 for 0.5 nutrition. (Meat defs are generated
/// from races, which we don't do yet; potatoes, also 0.05, stand in for
/// the chicken.)
#[test]
fn mixed_allocation_takes_the_nearest_first() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    use rimworld_sim::bills::{Candidate, allocate_mixing};
    use rimworld_sim::map::ItemId;
    let recipe = &defs.recipes[defs.recipes.id("CookMealSimple").unwrap()];
    let root = Cell::new(100, 99);
    let c = |id: u32, def: &str, x: i32, n: u32| Candidate {
        id: ItemId(id),
        def: defs.things.id(def).unwrap(),
        position: Cell::new(x, 99),
        stack_count: n,
        row_allows: [true, false, false, false],
        bill_allows: true,
    };
    let mut set = vec![
        c(1, "RawPotatoes", 106, 10),
        c(2, "RawCorn", 104, 10),
        c(3, "RawRice", 102, 5),
    ];
    let chosen = allocate_mixing(&defs, recipe, &[false], &mut set, root).unwrap();
    assert_eq!(chosen, vec![(ItemId(3), 5), (ItemId(2), 5)]);
}

/// A poisoned meal (poison 1.0) gives the eater food poisoning
/// (`CompFoodPoisonable.PostIngested`); its state survives the split when
/// one meal is taken from the stack.
#[test]
fn eating_a_poisoned_meal_gives_food_poisoning() {
    let Some(mut k) = kitchen() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let meal = k.defs.things.id("MealSimple").unwrap();
    let id = k.sim.spawn_item(meal, Cell::new(22, 14), 3);
    k.sim.map.item_meta.insert(
        id,
        rimworld_sim::map::ItemMeta {
            ingredients: vec![k.defs.things.id("RawRice").unwrap()],
            poison_pct: 1.0,
            poison_cause: rimworld_sim::map::PoisonCause::IncompetentCook,
        },
    );
    let poisoning = k.defs.hediffs.id("FoodPoisoning").unwrap();
    let mut poisoned = false;
    for _ in 0..3000 {
        k.sim.debug_set_need(k.pawn, NeedKind::Rest, 1.0);
        k.sim.debug_set_need(k.pawn, NeedKind::Joy, 1.0);
        if !poisoned && k.count("MealSimple") == 3 {
            k.sim.debug_set_need(k.pawn, NeedKind::Food, 0.2);
        }
        k.sim.tick();
        poisoned = k
            .sim
            .pawn(k.pawn)
            .unwrap()
            .health
            .hediffs
            .iter()
            .any(|h| h.def == poisoning);
        if poisoned {
            break;
        }
    }
    assert!(poisoned, "food poisoning after eating");
    assert_eq!(k.count("MealSimple"), 2, "one meal eaten");
}
