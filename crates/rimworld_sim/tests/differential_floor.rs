//! Differential replay of the floor work probe trace (local, gitignored):
//! a colonist with only Construction enabled removes two wood floors and
//! smooths two rough granite cells. Compares the colonist and each cell's
//! terrain every tick.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{JobKind, RoofStage};
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
fn removing_and_smoothing_floors_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("floor1", "trace_floorwork.csv") else {
        eprintln!("skipping: no floor work trace");
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
    let rows_of = |k: &str| -> Vec<Vec<String>> {
        text.lines()
            .filter_map(|l| l.strip_prefix(&format!("#{k},")))
            .map(|l| l.split(',').map(str::to_owned).collect())
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
    let rough = defs.terrain.id("Granite_Rough").unwrap();
    let wood = defs.terrain.id("WoodPlankFloor").unwrap();
    let smooth: Vec<Cell> = rows_of("smooth").iter().map(|v| cell(v)).collect();
    let remove: Vec<Cell> = rows_of("remove").iter().map(|v| cell(v)).collect();
    for &c in &smooth {
        sim.map.set_terrain_layered(&defs, c, rough);
    }
    for (c, v) in remove.iter().zip(rows_of("remove")) {
        sim.map.set_terrain_layered(&defs, *c, wood);
        assert_eq!(
            defs.terrain[sim.map.under_terrain(*c).unwrap()].def_name,
            v[2]
        );
    }
    sim.debug_refresh_path_grid();
    assert_eq!(sim.designate_smooth_floor(&smooth), 2);
    assert_eq!(sim.designate_remove_floor(&remove), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", cell(&header("pawnStart")))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    sim.set_move_speed(pawn, header("moveSpeed")[0].parse().unwrap());
    sim.set_update_rate(pawn, header("updateRate")[0].parse().unwrap());
    for (stat, key) in [
        ("ConstructionSpeed", "constructionSpeed"),
        ("SmoothingSpeed", "smoothingSpeed"),
    ] {
        sim.set_stat_override(pawn, stat, header(key)[0].parse().unwrap());
    }
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 3 } else { 0 });
    }
    sim.debug_set_need(pawn, NeedKind::Food, 1.0);
    sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(pawn);
    let cells: Vec<Cell> = smooth.iter().chain(&remove).copied().collect();
    let mut checked = 0;
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
        if let Some(JobKind::AffectFloor {
            cell: target,
            stage,
            ..
        }) = p.job.as_ref().map(|j| j.kind)
        {
            let toil = match stage {
                RoofStage::Goto => "0",
                RoofStage::Work { .. } => "1",
            };
            assert_eq!(toil, f[9], "{ctx}: toil");
            let game = Cell::new(f[10].parse().unwrap(), f[11].parse().unwrap());
            assert_eq!(target, game, "{ctx}: target");
        }
        let pos = Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap());
        assert_eq!(p.position, pos, "{ctx}: position");
        let ours: Vec<&str> = cells
            .iter()
            .map(|&c| defs.terrain[sim.map.terrain[c]].def_name.as_str())
            .collect();
        assert_eq!(ours.join(";"), f[12], "{ctx}: terrains");
        checked += 1;
    }
    assert!(checked > 3_000, "{checked}");
    eprintln!("floor work: {checked} ticks match");
}
