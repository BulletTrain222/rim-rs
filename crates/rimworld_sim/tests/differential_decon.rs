//! Differential replay of the deconstruction probe trace (local,
//! gitignored): a colonist with only Construction enabled takes down two
//! wooden walls. Compares the colonist and the walls every tick; the
//! refund is a random rounding of half the cost, so only its range is
//! checked.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{DeconstructStage, JobKind};
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

fn trace_file(dir: &str, name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        });
    let p = base.join(dir).join(name);
    p.exists().then_some(p)
}

#[test]
fn deconstructing_walls_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("decon1", "trace_decon_walls.csv") else {
        eprintln!("skipping: no deconstruction trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> Vec<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect()
    };
    let cell = |v: &[String]| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap());
    assert!(!text.contains("#error"), "probe timed out");
    let origin = cell(&header("origin"));
    let area = cell(&header("area"));
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
    sim.debug_set_sky_glow(Some(1.0));
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let walls: Vec<(Cell, f32)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#wall,"))
        .map(|l| {
            let v: Vec<String> = l.split(',').map(str::to_owned).collect();
            (cell(&v), v[2].parse().unwrap())
        })
        .collect();
    for &(c, work) in &walls {
        sim.debug_spawn_building(wall, Some(wood), c);
        // Our WorkToBuild for the wooden wall is the game's.
        let ours = rimworld_sim::stats::def_stat(
            &defs,
            &defs.things[wall],
            Some(&defs.things[wood]),
            "WorkToBuild",
        );
        assert_eq!(ours, work);
    }
    let cells: Vec<Cell> = walls.iter().map(|w| w.0).collect();
    assert_eq!(sim.designate_deconstruct(&cells), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    sim.set_stat_override(
        pawn,
        "ConstructionSpeed",
        header("constructionSpeed")[0].parse().unwrap(),
    );
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    let mut game_wood = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let p = sim.pawn(pawn).unwrap();
        let job = p
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: job");
        if let Some(JobKind::Deconstruct { stage, .. }) = p.job.as_ref().map(|j| j.kind) {
            let toil = match stage {
                DeconstructStage::Goto => "0",
                DeconstructStage::Work { .. } => "1",
            };
            assert_eq!(toil, f[9], "{ctx}: toil");
        }
        let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        assert_eq!(p.position, pos, "{ctx}: position");
        let ours: Vec<&str> = cells
            .iter()
            .map(|&c| {
                if sim.map.buildings[c].is_some() {
                    "W"
                } else {
                    "-"
                }
            })
            .collect();
        assert_eq!(ours.join(";"), f[12], "{ctx}: walls");
        game_wood = f[13].parse().unwrap();
        checked += 1;
    }
    // Each wooden wall (5 wood) leaves RoundRandom(2.5): 2 or 3.
    let ours: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == wood)
        .map(|i| i.stack_count)
        .sum();
    assert!((4..=6).contains(&ours), "{ours}");
    assert!((4..=6).contains(&game_wood), "{game_wood}");
    assert!(checked > 300, "{checked}");
    eprintln!("deconstruction: {checked} ticks match; wood left {ours} (game {game_wood})");
}

/// The roof collapse probe: walls A and B hold a roof strip; taking A down
/// drops the one roof cell beyond 6.9 of B, taking B down drops the rest
/// (no holder left). Roofs, rubble and the colonist must match every tick.
#[test]
fn unsupported_roofs_collapse_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("collapse1", "trace_collapse.csv") else {
        eprintln!("skipping: no roof collapse trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> Vec<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect()
    };
    let cell = |v: &[String]| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap());
    assert!(!text.contains("#error"), "probe timed out");
    let origin = cell(&header("origin"));
    let area = cell(&header("area"));
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
    sim.debug_set_sky_glow(Some(1.0));
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let walls: Vec<Cell> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#wall,"))
        .map(|l| {
            let v: Vec<String> = l.split(',').map(str::to_owned).collect();
            cell(&v)
        })
        .collect();
    for &c in &walls {
        sim.debug_spawn_building(wall, Some(wood), c);
    }
    let strip: Vec<Cell> = (5..=12).map(|x| origin + Cell::new(x, 3)).collect();
    for &c in &strip {
        sim.map.set_roof(c, true);
    }
    assert_eq!(sim.designate_deconstruct(&walls), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    sim.set_stat_override(
        pawn,
        "ConstructionSpeed",
        header("constructionSpeed")[0].parse().unwrap(),
    );
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let rubble = defs.things.id("Filth_RubbleBuilding").unwrap();
    let mut checked = 0;
    let mut collapsed = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let p = sim.pawn(pawn).unwrap();
        let job = p
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: job");
        let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        assert_eq!(p.position, pos, "{ctx}: position");
        let ws: Vec<&str> = walls
            .iter()
            .map(|&c| {
                if sim.map.buildings[c].is_some() {
                    "W"
                } else {
                    "-"
                }
            })
            .collect();
        assert_eq!(ws.join(";"), f[12], "{ctx}: walls");
        let roofs: String = strip
            .iter()
            .map(|&c| if sim.map.roofed(c) { 'R' } else { '-' })
            .collect();
        assert_eq!(roofs, f[13], "{ctx}: roofs");
        let filth: String = strip
            .iter()
            .map(|&c| {
                sim.map
                    .items_at(c)
                    .find(|i| i.def == rubble)
                    .map_or('0', |i| char::from_digit(i.thickness, 10).unwrap())
            })
            .collect();
        assert_eq!(filth, f[14], "{ctx}: rubble");
        collapsed = f[13].matches('-').count();
        checked += 1;
    }
    assert_eq!(collapsed, 8);
    assert!(checked > 280, "{checked}");
    eprintln!("roof collapse: {checked} ticks match");
}

/// The skill probe: the deconstruction scenario with Construction at level
/// 9, 9985 experience and a major passion. Experience per work interval,
/// the level-up and the daily decay above level 9 must match every tick.
#[test]
fn construction_experience_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("skill1", "trace_skillxp.csv") else {
        eprintln!("skipping: no skill trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> Vec<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .split(',')
            .map(str::to_owned)
            .collect()
    };
    let cell = |v: &[String]| Cell::new(v[0].parse().unwrap(), v[1].parse().unwrap());
    let origin = cell(&header("origin"));
    let area = cell(&header("area"));
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
    sim.debug_set_sky_glow(Some(1.0));
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let walls: Vec<Cell> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#wall,"))
        .map(|l| {
            let v: Vec<String> = l.split(',').map(str::to_owned).collect();
            cell(&v)
        })
        .collect();
    for &c in &walls {
        sim.debug_spawn_building(wall, Some(wood), c);
    }
    assert_eq!(sim.designate_deconstruct(&walls), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    sim.set_stat_override(
        pawn,
        "ConstructionSpeed",
        header("constructionSpeed")[0].parse().unwrap(),
    );
    sim.set_stat_override(
        pawn,
        "GlobalLearningFactor",
        header("learningFactor")[0].parse().unwrap(),
    );
    let start = header("skillStart");
    sim.set_skill(pawn, "Construction", start[0].parse().unwrap());
    sim.debug_set_skill_xp(pawn, "Construction", start[1].parse().unwrap());
    sim.debug_set_passion(pawn, "Construction", rimworld_sim::stats::Passion::Major);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    let mut wandering = false;
    let mut decays = 0;
    let mut last_xp = f32::NAN;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let p = sim.pawn(pawn).unwrap();
        // Once idle, the wandering is random; only the skill is compared.
        wandering |= f[8].contains("Wander");
        if !wandering {
            let job = p
                .job
                .as_ref()
                .and_then(|j| j.def)
                .map_or("-".to_owned(), |d| defs.jobs[d].def_name.clone());
            assert_eq!(job, f[8], "{ctx}: job");
            let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
            assert_eq!(p.position, pos, "{ctx}: position");
        }
        let (level, xp): (i32, f32) = (f[14].parse().unwrap(), f[15].parse().unwrap());
        assert_eq!(p.skills.level("Construction"), level, "{ctx}: level");
        let ours = p.skills.xp("Construction");
        assert!((ours - xp).abs() < 1e-3, "{ctx}: xp {ours} vs {xp}");
        if wandering && xp < last_xp {
            decays += 1;
        }
        last_xp = xp;
        checked += 1;
    }
    assert!(decays >= 4, "{decays}");
    eprintln!("construction experience: {checked} ticks match ({decays} decays)");
}
