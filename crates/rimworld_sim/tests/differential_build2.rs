//! Differential replay of two colonists building three walls from one
//! steel pile (local, gitignored trace from the probe's construct2 mode).
//! The first takes the whole job; the second is blocked by its
//! reservations and wanders. When that wander ends is random (our random
//! stream is not the game's), and what the second pawn does next decides
//! the rest, so the replay stops at the second pawn's first job change.
//! Skipped without an install or the trace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::map::ConstructStage;
use rimworld_sim::needs::NeedKind;
use rimworld_sim::{Cell, GridSize, Map, PawnId, Sim};

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

fn trace_file(dir: &str, name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        });
    let p = base.join(dir).join(name);
    p.exists().then_some(p)
}

/// Our state of a wall site in the probe's notation.
fn site(sim: &Sim, defs: &GameDefs, c: Cell) -> String {
    if let Some(k) = sim.map.constructible_at(c) {
        return match k.stage {
            ConstructStage::Blueprint => "B".to_owned(),
            ConstructStage::Frame => {
                let n: u32 = k.resources.iter().map(|(_, n)| n).sum();
                format!("F{n}:{}", k.work_done)
            }
        };
    }
    if sim.map.buildings[c] == defs.things.id("Wall")
        || Some(sim.map.terrain[c]) == defs.terrain.id("WoodPlankFloor")
    {
        return "W".to_owned();
    }
    "-".to_owned()
}

/// Parses a frame site into (delivered, work done).
fn frame(s: &str) -> Option<(u32, f32)> {
    let (n, w) = s.strip_prefix('F')?.split_once(':')?;
    Some((n.parse().ok()?, w.parse().ok()?))
}

fn job_name(sim: &Sim, defs: &GameDefs, pawn: PawnId) -> String {
    sim.pawn(pawn)
        .unwrap()
        .job
        .as_ref()
        .and_then(|j| j.def)
        .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone())
}

#[test]
fn two_builders_sharing_a_pile_match_the_original_game() {
    replay("trace_build2_two_pawns.csv", 150);
}

#[test]
fn a_builder_laying_wood_floors_matches_the_original_game() {
    // Floor frames can be stood on: the builder delivers standing on them
    // and builds the one under it and both neighbours without moving.
    replay("trace_build2_two_pawns_floor.csv", 400);
}

#[test]
fn two_builders_working_apart_match_the_original_game() {
    replay("trace_build2_two_pawns_apart.csv", 170);
}

#[test]
fn a_builder_picks_the_blueprint_its_region_lists_first() {
    // The nearer blueprint lies across a region line but is listed in the
    // builder's region too, since it can be touched from there.
    replay("trace_build2_two_pawns_regions.csv", 470);
}

#[test]
fn a_builder_searches_its_own_region_before_a_nearer_one() {
    // The nearer blueprint is two cells into the next region; the game
    // builds the farther one in the builder's own region first, which a
    // nearest-overall search would get wrong.
    replay("trace_build2_two_pawns_regions2.csv", 420);
}

/// Replays a two-pawn trace until the randomness of idle wandering takes
/// over; `min_ticks` is how many ticks must have been compared.
fn replay(name: &str, min_ticks: usize) {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("build2", name) else {
        eprintln!("skipping: no trace {name}");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |key: &str| -> Vec<Vec<String>> {
        text.lines()
            .filter_map(|l| l.strip_prefix('#'))
            .map(|l| l.split(',').map(str::to_owned).collect::<Vec<_>>())
            .filter(|p| p[0] == key)
            .map(|p| p[1..].to_vec())
            .collect()
    };
    let cell = |v: &[String]| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap());
    assert!(header("error").is_empty(), "probe timed out");
    let origin = cell(&header("origin")[0]);
    let area = cell(&header("area")[0]);
    let soil = defs.terrain.id("Soil").unwrap();
    let water = defs.terrain.id("WaterDeep").unwrap();
    let mut map = Map::new(
        GridSize::new(origin.x + area.x + 2, origin.z + area.z + 2),
        water,
    );
    for x in 0..area.x {
        for z in 0..area.z {
            map.terrain[Cell::new(origin.x + x, origin.z + z)] = soil;
        }
    }
    let mut sim = Sim::new(defs.clone(), map);
    let colonist = defs.pawn_kinds.id("Colonist").unwrap();
    // Spawned in the game's tick order.
    let order: Vec<String> = header("tickOrder")[0][0]
        .split(';')
        .map(str::to_owned)
        .collect();
    let ids: Vec<String> = header("pawn").iter().map(|p| p[0].clone()).collect();
    assert_eq!(order, ids, "pawns listed in tick order");
    let mut pawns = Vec::new();
    for p in header("pawn") {
        let id = sim.spawn_pawn(colonist, "probe", cell(&p[1..3])).unwrap();
        sim.set_thing_id_number(id, p[0].parse().unwrap());
        sim.set_move_speed(id, p[3].parse().unwrap());
        sim.set_update_rate(id, p[4].parse().unwrap());
        sim.set_stat_override(id, "ConstructionSpeed", p[5].parse().unwrap());
        sim.set_stat_override(id, "ConstructSuccessChance", p[6].parse().unwrap());
        sim.set_carrying_capacity(id, p[7].parse().unwrap());
        sim.set_skill(id, "Construction", 20);
        for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
            sim.set_work_priority(id, &wt, if wt == "Construction" { 3 } else { 0 });
        }
        sim.debug_set_need(id, NeedKind::Food, 1.0);
        sim.debug_set_need(id, NeedKind::Rest, 1.0);
        pawns.push(id);
    }
    for it in header("item") {
        let def = defs.things.id(&it[0]).unwrap();
        sim.spawn_item(def, cell(&it[1..3]), it[3].parse().unwrap());
    }
    let wall = defs.things.id("Wall").unwrap();
    let steel = defs.things.id("Steel").unwrap();
    let walls: Vec<Cell> = header("wall").iter().map(|w| cell(w)).collect();
    let floor = header("floor")
        .first()
        .map(|f| defs.terrain.id(&f[0]).unwrap());
    for &w in &walls {
        match floor {
            Some(f) => sim.place_floor_blueprint(f, w).unwrap(),
            None => sim.place_blueprint(wall, Some(steel), w).unwrap(),
        };
    }
    let undraft: u64 = header("undraftTick")[0][0].parse().unwrap();
    sim.debug_set_tick(undraft);
    for &p in &pawns {
        sim.debug_find_and_start_job(p);
    }
    let mut checked = 0;
    let idle = |j: &str| j.contains("Wander") || j == "Wait_MaintainPosture";
    let job_of = |f: &[&str], k: usize| f[1 + k * 13 + 7].to_owned();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    // A pawn idle from the start found no work; while it stays in that
    // first idle job we only check that ours is idle too.
    let first: Vec<String> = (0..pawns.len()).map(|k| job_of(&rows[0], k)).collect();
    let blocked: Vec<bool> = first.iter().map(|j| idle(j)).collect();
    'rows: for f in &rows {
        let tick: u64 = f[0].parse().unwrap();
        for k in 0..pawns.len() {
            let job = job_of(f, k);
            if (blocked[k] && job != first[k]) || (!blocked[k] && job.contains("Wander")) {
                break 'rows;
            }
        }
        while sim.tick_count() < tick {
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        for (k, &pawn) in pawns.iter().enumerate() {
            let g = &f[1 + k * 13..1 + (k + 1) * 13];
            let ours = job_name(&sim, &defs, pawn);
            if blocked[k] {
                // The blocked pawn's wandering is random: only check that
                // it found no work.
                assert!(idle(&ours), "{ctx}: pawn {k} job {ours} vs {}", g[7]);
                continue;
            }
            assert_eq!(ours, g[7], "{ctx}: pawn {k} job");
            let p = sim.pawn(pawn).unwrap();
            let pos = Cell::new(g[0].parse().unwrap(), g[1].parse().unwrap());
            assert_eq!(p.position, pos, "{ctx}: pawn {k} position");
            let carried: u32 = g[10].parse().unwrap();
            assert_eq!(
                p.carried.map_or(0, |c| c.count),
                carried,
                "{ctx}: pawn {k} carried"
            );
            let (moving, total): (bool, f32) = (g[2] == "1", g[6].parse().unwrap());
            if moving && total != 1.0 {
                let step = p.step.unwrap_or_else(|| panic!("{ctx}: pawn {k} mid-step"));
                let next = Cell::new(g[3].parse().unwrap(), g[4].parse().unwrap());
                assert_eq!(step.to, next, "{ctx}: pawn {k} next cell");
                let left: f32 = g[5].parse().unwrap();
                assert!(
                    (step.cost_left - left).abs() < 1e-3,
                    "{ctx}: pawn {k} cost left"
                );
            }
        }
        let sites: Vec<&str> = f[f.len() - 1].split(';').collect();
        for (w, expect) in walls.iter().zip(sites) {
            let ours = site(&sim, &defs, *w);
            match (frame(expect), frame(&ours)) {
                (Some((gn, gw)), Some((on, ow))) => {
                    assert_eq!(on, gn, "{ctx}: delivered at {w:?}");
                    assert!((ow - gw).abs() < 1e-2, "{ctx}: work at {w:?}: {ow} vs {gw}");
                }
                _ => assert_eq!(ours, expect, "{ctx}: site {w:?}"),
            }
        }
        checked += 1;
    }
    assert!(checked > min_ticks, "{checked}");
    eprintln!("{name}: {checked} ticks match");
}
