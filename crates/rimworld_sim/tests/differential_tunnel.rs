//! Mining against the research's native fixtures (`mountains_mining.md`,
//! rev591): the damage path's random draws for granite and steel, the
//! yield helper, the four-cell tunnel (`tunnel2`: a buried chain mined from
//! the reachable end, roofs kept, walkable afterwards), steel, cancelling,
//! the reload ordering of mine designations, and save/load mid-tunnel.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::native_mapgen::NaturalRoof;
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

/// `tunnel2`'s arena: deep water everywhere but a concrete corridor
/// x70..110 at z100 (no way around), a granite block x90..93 z99..101 on
/// soil under thick roof; `extra` adds rocks elsewhere. A Mining 8
/// colonist at (85,100) with MiningSpeed and MiningYield 1, only mining.
struct Arena {
    defs: Arc<GameDefs>,
    sim: Sim,
    miner: PawnId,
}

fn arena(block: bool, extra: &[(Cell, &str)]) -> Option<Arena> {
    let defs = real_defs()?;
    let water = defs.terrain.id("WaterDeep").unwrap();
    let concrete = defs.terrain.id("Concrete").unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let granite = defs.things.id("Granite").unwrap();
    let mut map = Map::new(GridSize::new(130, 130), water);
    for x in 70..=110 {
        map.terrain[Cell::new(x, 100)] = concrete;
    }
    if block {
        for x in 90..=93 {
            for z in 99..=101 {
                let c = Cell::new(x, z);
                map.terrain[c] = soil;
                map.buildings[c] = Some(granite);
                map.set_natural_roof(c, Some(NaturalRoof::Thick));
            }
        }
    }
    for &(c, def) in extra {
        map.buildings[c] = Some(defs.things.id(def).unwrap());
    }
    let mut sim = Sim::new(defs.clone(), map);
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 20.0;
    sim.debug_set_tick(110_000 - 1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let miner = sim.spawn_pawn(kind, "Miner", Cell::new(85, 100)).unwrap();
    sim.set_update_rate(miner, 15);
    sim.set_skill(miner, "Mining", 8);
    sim.set_stat_override(miner, "MiningSpeed", 1.0);
    sim.set_stat_override(miner, "MiningYield", 1.0);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(miner, &wt, if wt == "Mining" { 1 } else { 0 });
    }
    Some(Arena { defs, sim, miner })
}

impl Arena {
    fn tick(&mut self) {
        for k in [NeedKind::Food, NeedKind::Rest, NeedKind::Joy] {
            self.sim.debug_set_need(self.miner, k, 1.0);
        }
        self.sim.tick();
    }

    fn rock(&self, x: i32, z: i32) -> bool {
        self.sim.map.buildings[Cell::new(x, z)].is_some()
    }

    fn mine_target(&self) -> Option<Cell> {
        match self.sim.pawn(self.miner)?.job.as_ref()?.kind {
            JobKind::Mine { cell, .. } => Some(cell),
            _ => None,
        }
    }

    fn reachable(&self, from: Cell, to: Cell) -> bool {
        self.sim.regions().connected(from, to)
    }
}

/// Nonfinal hits through the native damage path from seed 123: granite
/// draws 2 (angle, hit points) to 820 HP with no yield; steel draws 3
/// (angle, yield, hit points) to 1420 HP and yield 80/1500.
#[test]
fn nonfinal_hits_draw_like_the_game() {
    let g = Cell::new(95, 100);
    let s = Cell::new(96, 100);
    let Some(mut a) = arena(false, &[(g, "Granite"), (s, "MineableSteel")]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    a.sim.debug_set_rng(123, 0);
    assert!(!a.sim.debug_mining_hit(a.miner, g));
    assert_eq!(a.sim.rng_state(), (123, 2));
    assert_eq!(a.sim.rock_hit_points(g), Some(820));
    assert_eq!(a.sim.rock_yield_fraction(g), 0.0);
    a.sim.debug_set_rng(123, 0);
    assert!(!a.sim.debug_mining_hit(a.miner, s));
    assert_eq!(a.sim.rng_state(), (123, 3));
    assert_eq!(a.sim.rock_hit_points(s), Some(1420));
    assert!((a.sim.rock_yield_fraction(s) - 0.053_333_33).abs() < 1e-7);
}

/// The yield helper alone: granite from seed 123 rolls above its 0.25
/// chance (no chunk, one draw); from seed 13 a chunk; steel with full
/// attribution gives 40 (two draws), half gives 20.
#[test]
fn yield_helper_matches_the_game() {
    let Some(mut a) = arena(false, &[]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let granite = a.defs.things.id("Granite").unwrap();
    let steel_rock = a.defs.things.id("MineableSteel").unwrap();
    let chunk = a.defs.things.id("ChunkGranite").unwrap();
    let steel = a.defs.things.id("Steel").unwrap();
    let count = |a: &Arena, d| -> u32 {
        a.sim
            .map
            .items()
            .iter()
            .filter(|i| i.def == d)
            .map(|i| i.stack_count)
            .sum()
    };
    a.sim.debug_set_rng(123, 0);
    a.sim
        .debug_try_spawn_yield(granite, Cell::new(80, 100), 0.0);
    assert_eq!(a.sim.rng_state(), (123, 1));
    assert_eq!(count(&a, chunk), 0);
    a.sim.debug_set_rng(13, 0);
    a.sim
        .debug_try_spawn_yield(granite, Cell::new(80, 100), 0.0);
    assert_eq!(count(&a, chunk), 1);
    a.sim.debug_set_rng(123, 0);
    a.sim
        .debug_try_spawn_yield(steel_rock, Cell::new(100, 100), 1.0);
    assert_eq!(a.sim.rng_state(), (123, 2));
    assert_eq!(count(&a, steel), 40);
    let forbidden = a
        .sim
        .map
        .items()
        .iter()
        .any(|i| i.def == steel && i.forbidden);
    assert!(!forbidden, "a player's output stays allowed");
    a.sim
        .debug_try_spawn_yield(steel_rock, Cell::new(105, 100), 0.5);
    assert_eq!(count(&a, steel), 60);
}

/// `tunnel2`: designated in reverse (93, 92, 91, 90), only A (90) is
/// reachable and is chosen first, then B, C, D in turn; the exit (95,100)
/// opens only after D; each wall takes 12 hits 105 ticks apart after the
/// first; the cells become rough-hewn granite, walkable, the four thick
/// roofs stay; the miner can then walk through.
#[test]
fn a_buried_tunnel_is_mined_from_its_open_end() {
    let Some(mut a) = arena(true, &[]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let cells: Vec<Cell> = [93, 92, 91, 90]
        .iter()
        .map(|&x| Cell::new(x, 100))
        .collect();
    assert_eq!(a.sim.designate_mine(&cells), 4);
    let exit = Cell::new(95, 100);
    assert!(!a.reachable(Cell::new(85, 100), exit));
    let mut order = Vec::new();
    let mut hits: Vec<(Cell, u64, i32)> = Vec::new();
    let mut last_hp = std::collections::HashMap::new();
    for _ in 0..12_000 {
        a.tick();
        if let Some(t) = a.mine_target()
            && order.last() != Some(&t)
        {
            order.push(t);
        }
        for &c in &cells {
            let hp = a.sim.rock_hit_points(c).unwrap_or(0);
            if last_hp.get(&c).is_some_and(|&h| h != hp) {
                hits.push((c, a.sim.tick_count(), hp));
            }
            last_hp.insert(c, hp);
        }
        if cells.iter().all(|c| !a.rock(c.x, c.z)) {
            break;
        }
    }
    let xs: Vec<i32> = order.iter().map(|c| c.x).collect();
    assert_eq!(xs, vec![90, 91, 92, 93], "A, B, C, D");
    for &c in &cells {
        let t: Vec<u64> = hits.iter().filter(|h| h.0 == c).map(|h| h.1).collect();
        assert_eq!(t.len(), 12, "{c:?}: 12 hits");
        for w in t.windows(2) {
            assert_eq!(w[1] - w[0], 105, "{c:?}: 105 ticks between hits");
        }
    }
    let hewn = a.defs.terrain.id("Granite_RoughHewn").unwrap();
    for &c in &cells {
        assert!(a.sim.path_grid().walkable(c), "{c:?} walkable");
        assert_eq!(a.sim.map.terrain[c], hewn, "{c:?} rough-hewn");
        assert_eq!(
            a.sim.map.natural_roof(c),
            Some(NaturalRoof::Thick),
            "{c:?} roof kept"
        );
        assert!(!a.sim.map.mine_designations.contains(&c));
    }
    assert!(a.reachable(Cell::new(85, 100), exit));
    // Ten more ticks: roofs still stand (the side walls hold them).
    for _ in 0..10 {
        a.tick();
    }
    assert!(cells.iter().all(|&c| a.sim.map.roofed(c)));
    a.sim
        .apply(rimworld_sim::Command::MoveTo {
            pawn: a.miner,
            target: exit,
        })
        .unwrap();
    for _ in 0..600 {
        a.tick();
        if a.sim.pawn(a.miner).unwrap().position == exit {
            return;
        }
    }
    panic!("never walked through the tunnel");
}

/// Save/load in the middle of B continues exactly like the running game
/// (wall hit points, yield fraction, countdown, roofs, products), then
/// finishes the tunnel.
#[test]
fn tunnel_survives_save_and_load() {
    let Some(mut a) = arena(true, &[]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let cells: Vec<Cell> = [93, 92, 91, 90]
        .iter()
        .map(|&x| Cell::new(x, 100))
        .collect();
    a.sim.designate_mine(&cells);
    while a.rock(90, 100)
        || a.sim
            .rock_hit_points(Cell::new(91, 100))
            .is_none_or(|hp| hp > 500)
    {
        a.tick();
        assert!(a.sim.tick_count() < 120_000);
    }
    let data = a.sim.save();
    let mut copy = Sim::load(a.defs.clone(), &data).expect("loads");
    copy.debug_set_sky_glow(Some(1.0));
    copy.outdoor_temperature = 20.0;
    assert_eq!(
        copy.rock_hit_points(Cell::new(91, 100)),
        a.sim.rock_hit_points(Cell::new(91, 100))
    );
    a.sim.forget_unsaved_state();
    for _ in 0..6000 {
        for sim in [&mut a.sim, &mut copy] {
            for k in [NeedKind::Food, NeedKind::Rest, NeedKind::Joy] {
                sim.debug_set_need(a.miner, k, 1.0);
            }
            sim.tick();
        }
    }
    assert!(
        cells.iter().all(|c| copy.map.buildings[*c].is_none()),
        "finished after load"
    );
    assert!(a.sim.save() == copy.save(), "diverged after reload");
}

/// Steel: 19 hits (the last one the remaining 60), 40 steel, allowed.
#[test]
fn mineable_steel_gives_forty_steel() {
    let rock = Cell::new(90, 100);
    let Some(mut a) = arena(false, &[(rock, "MineableSteel")]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    a.sim.designate_mine(&[rock]);
    let mut hits = 0;
    let mut hp = a.sim.rock_hit_points(rock).unwrap();
    while a.rock(90, 100) {
        a.tick();
        let now = a.sim.rock_hit_points(rock).unwrap_or(0);
        if now != hp {
            hits += 1;
            hp = now;
        }
        assert!(a.sim.tick_count() < 115_000);
    }
    assert_eq!(hits, 19);
    let steel = a.defs.things.id("Steel").unwrap();
    let out: Vec<_> = a
        .sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == steel)
        .collect();
    assert_eq!(out.iter().map(|i| i.stack_count).sum::<u32>(), 40);
    assert!(out.iter().all(|i| !i.forbidden));
}

/// Cancelling keeps the damage done; the job ends.
#[test]
fn cancelling_keeps_the_damage() {
    let rock = Cell::new(90, 100);
    let Some(mut a) = arena(false, &[(rock, "Granite")]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    a.sim.designate_mine(&[rock]);
    while a.sim.rock_hit_points(rock).is_some_and(|hp| hp > 740) {
        a.tick();
        assert!(a.sim.tick_count() < 112_000);
    }
    assert_eq!(a.sim.cancel_mine(&[rock]), 1);
    for _ in 0..30 {
        a.tick();
    }
    assert_eq!(a.sim.rock_hit_points(rock), Some(740));
    assert!(a.mine_target().is_none());
}

/// `extra1`: equidistant rocks designated (90,101) then (90,99): the
/// first is chosen; after a reload the list is reversed and the second is.
#[test]
fn reload_reverses_mine_designation_order() {
    let (r1, r2) = (Cell::new(90, 101), Cell::new(90, 99));
    let Some(mut a) = arena(false, &[(r1, "Granite"), (r2, "Granite")]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    // Make both rocks touchable from the corridor's neighbours.
    a.sim.designate_mine(&[r1, r2]);
    let data = a.sim.save();
    let mut copy = Sim::load(a.defs.clone(), &data).expect("loads");
    assert_eq!(copy.map.mine_designations, vec![r2, r1]);
    for _ in 0..60 {
        a.tick();
        copy.tick();
    }
    assert_eq!(a.mine_target(), Some(r1));
    let copy_target = match copy.pawn(a.miner).unwrap().job.as_ref().unwrap().kind {
        JobKind::Mine { cell, .. } => cell,
        ref k => panic!("{k:?}"),
    };
    assert_eq!(copy_target, r2);
}

/// With every wall dropping a chunk, the one-wide tunnel would stand
/// behind its own chunks: the miner hauls each one aside (the game's
/// `JobOnThing` haul-aside) and still finishes.
#[test]
fn chunks_in_a_narrow_tunnel_are_hauled_aside() {
    let Some(mut a) = arena(true, &[]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    a.sim.debug_set_mine_drops(Some(true));
    let cells: Vec<Cell> = [93, 92, 91, 90]
        .iter()
        .map(|&x| Cell::new(x, 100))
        .collect();
    a.sim.designate_mine(&cells);
    let mut hauled_aside = false;
    for _ in 0..15_000 {
        a.tick();
        if matches!(
            a.sim.pawn(a.miner).unwrap().job.as_ref().map(|j| j.kind),
            Some(JobKind::Haul { aside: true, .. })
        ) {
            hauled_aside = true;
        }
        if cells.iter().all(|c| !a.rock(c.x, c.z)) {
            break;
        }
    }
    assert!(cells.iter().all(|c| !a.rock(c.x, c.z)), "tunnel finished");
    assert!(hauled_aside, "a chunk was hauled aside");
    let chunk = a.defs.things.id("ChunkGranite").unwrap();
    assert_eq!(
        a.sim.map.items().iter().filter(|i| i.def == chunk).count(),
        4
    );
}

/// `cases1`: a lone steel wall under thin roof — the roof is still there
/// when the wall breaks and falls on the following ticks (no holder within
/// reach).
#[test]
fn an_unsupported_roof_falls_after_mining() {
    let rock = Cell::new(90, 100);
    let Some(mut a) = arena(false, &[(rock, "MineableSteel")]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    a.sim.map.set_natural_roof(rock, Some(NaturalRoof::Thin));
    a.sim.designate_mine(&[rock]);
    while a.rock(90, 100) {
        a.tick();
        assert!(a.sim.tick_count() < 115_000);
    }
    assert!(a.sim.map.roofed(rock), "roof present at destruction");
    for _ in 0..5 {
        a.tick();
    }
    assert!(!a.sim.map.roofed(rock), "unsupported roof gone");
}
