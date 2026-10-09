//! Butchering with the real game Defs (skipped without an install): a
//! cook butchers animal corpses at a butcher spot or table from a bill;
//! meat and leather amounts follow the stats (`MeatAmount`,
//! `LeatherAmount`) × the butcher's efficiency × the table's.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::bills::{RepeatMode, StoreMode};
use rimworld_sim::map::ItemId;
use rimworld_sim::pawn::PawnId;
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

struct Shop {
    defs: Arc<GameDefs>,
    sim: Sim,
    table: ItemId,
    cook: PawnId,
}

/// A butcher spot (or table) at (20, 20) on concrete, outdoors at 20 °C,
/// a Cooking 10 colonist (butchery efficiency 1) who only cooks.
fn shop(table: &str) -> Option<Shop> {
    let defs = real_defs()?;
    let concrete = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(40, 40), concrete));
    sim.set_default_update_rate(15);
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 20.0;
    let def = defs.things.id(table).unwrap();
    let stuff = defs.things[def]
        .stuff_categories
        .first()
        .map(|_| defs.things.id("WoodLog").unwrap());
    let table = sim.debug_spawn_building(def, stuff, Cell::new(20, 20));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let cook = sim.spawn_pawn(kind, "Cook", Cell::new(20, 16)).unwrap();
    sim.set_update_rate(cook, 15);
    sim.set_skill(cook, "Cooking", 10);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(cook, &wt, if wt == "Cooking" { 1 } else { 0 });
    }
    Some(Shop {
        defs,
        sim,
        table,
        cook,
    })
}

impl Shop {
    fn tick(&mut self) {
        self.sim.debug_set_need(self.cook, NeedKind::Rest, 1.0);
        self.sim.debug_set_need(self.cook, NeedKind::Food, 1.0);
        self.sim.debug_set_need(self.cook, NeedKind::Joy, 1.0);
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

    /// A dead animal of `kind` at `at`, optionally shot first.
    fn carcass(&mut self, kind: &str, at: Cell, shot: bool) -> PawnId {
        let p = self.sim.spawn_animal(kind, at).unwrap();
        if shot {
            self.sim.debug_add_hediff(p, "Gunshot", 2.0);
        }
        self.sim.debug_kill(p);
        // Outside a home area a corpse starts forbidden; allow it.
        let corpse = self.sim.corpse_of(p).unwrap();
        let defs = self.defs.clone();
        self.sim.map.set_forbidden(&defs, corpse, false);
        p
    }

    fn bill(&mut self, mode: RepeatMode) -> u32 {
        let recipe = self.defs.recipes.id("ButcherCorpseFlesh").unwrap();
        let bill = self.sim.add_bill(self.table, recipe);
        self.sim.edit_bill(self.table, bill, |b| {
            b.store_mode = StoreMode::DropOnFloor;
            b.repeat_mode = mode;
        });
        bill
    }

    fn run_until(&mut self, ticks: u32, done: impl Fn(&Shop) -> bool) -> bool {
        for _ in 0..ticks {
            self.tick();
            if done(self) {
                return true;
            }
        }
        false
    }
}

/// The stats: a clean adult Hare (body size 0.2) has meat 140 × 0.2 = 28
/// and leather 40 × 0.2 = 8, both through the post-process curve (5→14,
/// 40→40): 31.09 and 16.23; a fresh wound makes both × 0.66 before the
/// curve (24.01 and 14.21).
#[test]
fn hare_yields_follow_the_stats() {
    let Some(mut s) = shop("ButcherSpot") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let clean = s.carcass("Hare", Cell::new(10, 10), false);
    let shot = s.carcass("Hare", Cell::new(12, 10), true);
    let (meat, leather) = s.sim.butcher_yield(clean).unwrap();
    let curve = |x: f32| 14.0 + 26.0 * (x - 5.0) / 35.0;
    assert!((meat - curve(28.0)).abs() < 1e-3, "{meat}");
    assert!((leather - curve(8.0)).abs() < 1e-3, "{leather}");
    let (meat, leather) = s.sim.butcher_yield(shot).unwrap();
    assert!((meat - curve(28.0 * 0.66)).abs() < 1e-3, "{meat}");
    assert!(
        (leather - (14.0 + 26.0 * 0.28 / 35.0)).abs() < 1e-3,
        "{leather}"
    );
    assert_eq!(
        s.sim.pawn_stat(s.cook, "ButcheryFleshEfficiency"),
        Some(1.0)
    );
}

/// At a butcher spot (70% efficiency) the cook fetches the corpse, works
/// 450 at ButcheryFleshSpeed and leaves 21–22 hare meat, 11–12 light
/// leather and blood; the corpse is gone and the bill done.
#[test]
fn a_hare_is_butchered_at_a_butcher_spot() {
    let Some(mut s) = shop("ButcherSpot") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let hare = s.carcass("Hare", Cell::new(26, 18), false);
    s.bill(RepeatMode::RepeatCount);
    assert!(s.run_until(5000, |s| s.count("Meat_Hare") > 0), "no meat");
    let meat = s.count("Meat_Hare");
    let leather = s.count("Leather_Light");
    assert!((21..=22).contains(&meat), "meat {meat}");
    assert!((11..=12).contains(&leather), "leather {leather}");
    assert!(s.sim.corpse_of(hare).is_none(), "corpse used up");
    let blood = s.defs.things.id("Filth_Blood").unwrap();
    assert!(s.sim.map.items().iter().any(|i| i.def == blood), "blood");
    assert_eq!(s.sim.bills(s.table)[0].repeat_count, 0);
    eprintln!("butcher spot: {meat} meat, {leather} leather");
}

/// At a butcher table (100%), shot Hares; stored meat counts toward an
/// "until 10" target bill, so a corpse arriving later is left alone. (The
/// count is the resource counter's, refreshed every 204 ticks: a second
/// corpse waiting at once would be butchered too, as in the game.)
#[test]
fn a_target_count_bill_stops_at_enough_meat() {
    let Some(mut s) = shop("TableButcher") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let first = s.carcass("Hare", Cell::new(26, 18), true);
    let cells: Vec<Cell> = (5..9)
        .flat_map(|x| (5..9).map(move |z| Cell::new(x, z)))
        .collect();
    s.sim.designate_stockpile(&cells).unwrap();
    let bill = s.bill(RepeatMode::TargetCount);
    s.sim.edit_bill(s.table, bill, |b| {
        b.store_mode = StoreMode::BestStockpile;
        b.target_count = 10;
    });
    let stored = |s: &Shop| {
        s.sim.corpse_of(first).is_none()
            && s.sim.pawn(s.cook).unwrap().carried.is_none()
            && s.count("Meat_Hare") > 0
    };
    assert!(s.run_until(6000, stored), "nothing butchered");
    let meat = s.count("Meat_Hare");
    // 24.01 at efficiency 1.
    assert!((24..=25).contains(&meat), "meat {meat}");
    for _ in 0..300 {
        s.tick();
    }
    let second = s.carcass("Hare", Cell::new(28, 18), true);
    for _ in 0..3000 {
        s.tick();
    }
    assert!(s.sim.corpse_of(second).is_some(), "enough meat: left alone");
    assert_eq!(s.count("Meat_Hare"), meat);
}

/// The default bill takes animal corpses only: a dead colonist is left.
#[test]
fn human_corpses_are_not_butchered_by_default() {
    let Some(mut s) = shop("ButcherSpot") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let kind = s.defs.pawn_kinds.id("Colonist").unwrap();
    let body = s.sim.spawn_pawn(kind, "Body", Cell::new(26, 18)).unwrap();
    s.sim.debug_kill(body);
    let corpse = s.sim.corpse_of(body).unwrap();
    let defs = s.defs.clone();
    s.sim.map.set_forbidden(&defs, corpse, false);
    s.bill(RepeatMode::Forever);
    for _ in 0..3000 {
        s.tick();
    }
    assert!(s.sim.corpse_of(body).is_some());
    assert_eq!(s.count("Meat_Human"), 0);
}

/// Meat cooks: a simple meal from hare meat at a stove.
#[test]
fn hare_meat_cooks_into_a_simple_meal() {
    let Some(mut s) = shop("FueledStove") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    s.sim.debug_set_fuel(Cell::new(20, 20), 50.0);
    let meat = s.defs.things.id("Meat_Hare").unwrap();
    s.sim.spawn_item(meat, Cell::new(24, 18), 20);
    let recipe = s.defs.recipes.id("CookMealSimple").unwrap();
    let bill = s.sim.add_bill(s.table, recipe);
    s.sim
        .edit_bill(s.table, bill, |b| b.store_mode = StoreMode::DropOnFloor);
    assert!(s.run_until(4000, |s| s.count("MealSimple") > 0));
    assert_eq!(s.count("Meat_Hare"), 10);
}

/// A save in the middle of butchering continues exactly like the running
/// game.
#[test]
fn butchering_survives_save_and_load() {
    let Some(mut s) = shop("ButcherSpot") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    s.carcass("Hare", Cell::new(26, 18), false);
    s.bill(RepeatMode::RepeatCount);
    let working = |s: &Shop| {
        matches!(
            s.sim.pawn(s.cook).unwrap().job.as_ref().map(|j| j.kind),
            Some(rimworld_sim::job::JobKind::DoBill {
                stage: rimworld_sim::job::DoBillStage::Work { .. },
                ..
            })
        )
    };
    assert!(s.run_until(3000, working));
    for _ in 0..60 {
        s.tick();
    }
    let data = s.sim.save();
    let mut copy = Sim::load(s.defs.clone(), &data).expect("loads");
    copy.debug_set_sky_glow(Some(1.0));
    copy.outdoor_temperature = 20.0;
    s.sim.forget_unsaved_state();
    for _ in 0..1500 {
        for sim in [&mut s.sim, &mut copy] {
            sim.debug_set_need(s.cook, NeedKind::Rest, 1.0);
            sim.debug_set_need(s.cook, NeedKind::Food, 1.0);
            sim.debug_set_need(s.cook, NeedKind::Joy, 1.0);
            sim.tick();
        }
    }
    assert!(s.count("Meat_Hare") > 0);
    assert!(s.sim.save() == copy.save(), "diverged after reload");
}

/// A butcher spot needs no work to build: placing it makes the spot at
/// once (`Designator_Build`), with no blueprint.
#[test]
fn a_butcher_spot_is_placed_finished() {
    let Some(mut s) = shop("FueledStove") else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let spot = s.defs.things.id("ButcherSpot").unwrap();
    let placed =
        s.sim
            .place_blueprint_rotated(spot, None, Cell::new(10, 10), rimworld_sim::Rot4::North);
    assert!(placed.is_some());
    assert!(s.sim.map.structures().iter().any(|b| b.def == spot));
    assert!(s.sim.map.constructibles().is_empty());
}
