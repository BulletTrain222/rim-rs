//! Loads the Defs from a real RimWorld installation, if one is configured
//! (`RIMWORLD_PATH` or the workspace `config.toml`). Skipped otherwise, so CI
//! and contributors without the game can still run `cargo test`.

use std::path::Path;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, RimWorldInstall, resolve_install};
use rimworld_defs::{GameDefs, PackSource, Passability, load_packs};

fn find_install() -> Option<RimWorldInstall> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = AppConfig::load(&root.join("config.toml")).ok().flatten();
    let env = std::env::var_os(ENV_RIMWORLD_PATH).map(Into::into);
    resolve_install(None, env, config.as_ref(), &[]).ok()
}

#[test]
fn loads_real_base_game_defs() {
    let Some(install) = find_install() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let packs: Vec<PackSource> = install
        .content_packs()
        .into_iter()
        .map(|p| PackSource {
            defs_dir: p.defs_dir(),
            package_id: p.package_id,
        })
        .collect();
    let (db, report) = load_packs(&packs).expect("load defs");
    eprintln!(
        "{} files, {} defs ({} abstract), {} warnings, {:?}",
        report.files_loaded,
        db.total(),
        report.abstract_defs,
        report.warnings.len(),
        report.elapsed
    );
    for w in report.warnings.iter().take(40) {
        eprintln!("  warn: {w}");
    }
    assert_eq!(report.files_failed, 0, "{:?}", report.warnings);
    assert!(db.count("ThingDef") > 500);
    assert!(db.count("TerrainDef") > 30);

    let (defs, warnings) = GameDefs::from_database(db);
    // Work types in the order (and with the natural priorities) the running
    // game's Def database holds them (work-selection report, rev591).
    let order: Vec<(&str, i32)> = defs
        .work_types
        .iter()
        .map(|(_, w)| (w.def_name.as_str(), w.natural_priority))
        .collect();
    assert_eq!(
        order,
        vec![
            ("Firefighter", 1400),
            ("Patient", 1350),
            ("Doctor", 1300),
            ("PatientBedRest", 1200),
            ("BasicWorker", 1150),
            ("Warden", 1100),
            ("Handling", 1050),
            ("Cooking", 1000),
            ("Hunting", 950),
            ("Construction", 900),
            ("Growing", 700),
            ("Mining", 600),
            ("PlantCutting", 500),
            ("Smithing", 470),
            ("Tailoring", 450),
            ("Art", 430),
            ("Crafting", 400),
            ("Hauling", 300),
            ("Cleaning", 200),
            ("Research", 100),
        ]
    );
    let givers = |t: &str| -> Vec<(String, i32)> {
        let wt = defs.work_types.get(t).unwrap();
        wt.givers_by_priority
            .iter()
            .map(|&g| {
                let g = &defs.work_givers[g];
                (g.def_name.clone(), g.priority_in_type)
            })
            .collect()
    };
    assert_eq!(
        givers("Cleaning"),
        vec![
            ("CleanClearSnow".to_owned(), 10),
            ("CleanFilth".to_owned(), 5)
        ]
    );
    let hauling = givers("Hauling");
    eprintln!("Hauling givers: {hauling:?}");
    let pos = |n: &str| hauling.iter().position(|(g, _)| g == n).unwrap();
    assert!(pos("RearmTurrets") < pos("Refuel") && pos("HaulGeneral") < pos("HaulMerge"));
    for w in warnings.iter().take(20) {
        eprintln!("  typed warn: {w}");
    }

    let soil = defs.terrain.get("Soil").expect("Soil");
    assert_eq!(soil.path_cost, 2);
    assert!(
        soil.affordances.iter().any(|a| a == "Walkable"),
        "inherited from NaturalTerrainBase"
    );
    assert_eq!(
        defs.terrain.get("WaterDeep").unwrap().passability,
        Passability::Impassable
    );

    let human = defs.things.get("Human").expect("Human");
    assert!(human.is_pawn());
    assert_eq!(human.stat("MoveSpeed"), Some(4.6));
    assert_eq!(
        human.category.as_deref(),
        Some("Pawn"),
        "inherited from BasePawn"
    );

    let granite = defs.things.get("Granite").expect("Granite");
    assert_eq!(
        granite.passability,
        Passability::Impassable,
        "inherited from RockBase"
    );
    assert!(granite.graphic.as_ref().unwrap().tex_path.is_some());

    let colonist = defs.pawn_kinds.get("Colonist").expect("Colonist");
    let race = defs.race_of(colonist).expect("race resolves");
    assert_eq!(defs.things[race].def_name, "Human");

    let goto = defs.jobs.get("Goto").expect("Goto JobDef");
    assert_eq!(goto.report_string, "moving.");
    assert_eq!(
        defs.jobs.get("GotoWander").unwrap().report_string,
        "wandering."
    );
    assert!(defs.jobs.get("Wait_Wander").is_some());

    let race_tree = human.race.as_ref().unwrap().think_tree_main.as_deref();
    assert_eq!(race_tree, Some("Humanlike"));
    let tree = defs
        .think_trees
        .get("Humanlike")
        .expect("Humanlike think tree");
    let nodes = tree.root.as_ref().unwrap().count();
    eprintln!("Humanlike think tree: {nodes} nodes");
    assert!(nodes > 100);
    assert_eq!(colonist.default_faction.as_deref(), Some("PlayerColony"));

    assert_eq!(
        defs.needs.get("Food").unwrap().need_class.as_deref(),
        Some("Need_Food")
    );
    assert_eq!(defs.needs.get("Rest").unwrap().label, "sleep");
    assert_eq!(defs.base_stat(human, "RestFallRateFactor"), Some(1.0));
    assert_eq!(defs.base_stat(human, "RestRateMultiplier"), Some(1.0));
    assert_eq!(
        defs.jobs.get("LayDown").unwrap().report_string,
        "lying down."
    );

    assert_eq!(
        defs.stat_value_if_missing("BedRestEffectiveness"),
        Some(0.8)
    );
    assert!(!defs.terrain.get("Soil").unwrap().avoid_wander);
    assert!(
        defs.terrain.get("WaterShallow").unwrap().avoid_wander,
        "inherited from WaterBase"
    );

    // Base game only: DLC-gated nodes must be gone.
    let raw_human = defs.raw.get("ThingDef", "Human").unwrap();
    let anomaly = raw_human.node.path(&["race", "knowledgeCategory"]);
    assert!(
        anomaly.is_none(),
        "MayRequire=Anomaly field should be dropped"
    );
}
