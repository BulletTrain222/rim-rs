//! Wild animals with the real game Defs (skipped without an install): a
//! temperate forest map fills with its biome's animals up to the
//! ecosystem's weight, the spawner brings new ones in from the edge when
//! there is room, and herbivores graze to stay fed.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
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

/// A soil map of `size` dressed as temperate forest at 15 °C.
fn forest(size: i32, seed: u64) -> Option<Sim> {
    let defs = real_defs()?;
    let soil = defs.terrain.id("Soil").unwrap();
    let mut map = Map::new(GridSize::new(size, size), soil);
    let b = defs.biomes.id("TemperateForest").unwrap();
    rimworld_sim::mapgen::apply_biome(&mut map, &defs, &defs.biomes[b], seed);
    let mut sim = Sim::new(defs.clone(), map);
    sim.set_biome(Some(b), seed as u32);
    sim.outdoor_temperature = 15.0;
    Some(sim)
}

fn animals(sim: &Sim) -> Vec<String> {
    sim.pawns()
        .iter()
        .filter(|p| !p.health.dead && sim.is_animal(p.id))
        .map(|p| sim.defs.pawn_kinds[p.kind].def_name.clone())
        .collect()
}

/// `GenStep_Animals`: the map fills until the eco weight reaches area /
/// (10000 / density) (250² at 3.7: 23.1); only the biome's animals.
#[test]
fn map_generation_fills_the_ecosystem() {
    let Some(mut sim) = forest(250, 1) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let density = sim.desired_animal_density();
    assert!(density > 0.0 && density <= 3.7, "{density}");
    assert!(!sim.animal_ecosystem_full());
    let n = sim.generate_wild_animals();
    assert!(n > 0);
    assert!(sim.animal_ecosystem_full());
    let b = sim.defs.biomes.id("TemperateForest").unwrap();
    let list: Vec<String> = sim.defs.biomes[b]
        .wild_animals
        .iter()
        .map(|(k, _)| k.clone())
        .collect();
    let kinds = animals(&sim);
    assert!(kinds.iter().all(|k| list.contains(k)), "{kinds:?}");
    eprintln!("{n} groups, {} animals: {kinds:?}", kinds.len());
}

/// In deep cold most animals' comfort ranges exclude the season: the
/// desired density falls.
#[test]
fn cold_seasons_thin_the_ecosystem() {
    let Some(mut sim) = forest(100, 2) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let mild = sim.desired_animal_density();
    sim.outdoor_temperature = -35.0;
    let cold = sim.desired_animal_density();
    assert!(cold < mild, "{cold} < {mild}");
}

/// With room in the ecosystem, the spawner (every 1213 ticks, chance
/// 0.027 × density) brings animals in at the map edge.
#[test]
fn the_spawner_brings_animals_in_from_the_edge() {
    let Some(mut sim) = forest(150, 3) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    assert!(animals(&sim).is_empty());
    let mut first = None;
    for _ in 0..(1213 * 200) {
        sim.tick();
        if first.is_none() && !animals(&sim).is_empty() {
            first = Some(sim.tick_count());
            let p = sim
                .pawns()
                .iter()
                .find(|p| sim.is_animal(p.id))
                .unwrap()
                .position;
            let near_edge = p.x <= 3 || p.z <= 3 || p.x >= 146 || p.z >= 146;
            assert!(near_edge, "entered at {p:?}");
            assert_eq!(sim.tick_count() % 1213, 0);
            break;
        }
    }
    assert!(first.is_some(), "no animal came");
}

/// A starving Hare grazes (eating grass where it grows) and stays fed
/// (unless a predator the spawner brought in kills it first).
#[test]
fn a_hare_grazes_to_stay_fed() {
    let Some(mut sim) = forest(60, 4) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let hare = sim.spawn_animal("Hare", Cell::new(30, 30)).unwrap();
    sim.debug_set_need(hare, NeedKind::Food, 0.02);
    let mut ate = false;
    for _ in 0..240_000 {
        sim.tick();
        let p = sim.pawn(hare).unwrap();
        if matches!(
            p.job.as_ref().map(|j| j.kind),
            Some(JobKind::IngestInPlace { .. })
        ) {
            ate = true;
        }
    }
    let p = sim.pawn(hare).unwrap();
    assert!(ate, "never grazed");
    // The spawner may bring a predator in that kills it; it never starves.
    assert!(p.health.dead || !p.health.downed);
    let food = p
        .needs
        .list
        .iter()
        .find(|n| n.kind == NeedKind::Food)
        .unwrap();
    assert!(food.level > 0.0, "{}", food.level);
}

/// A save while an animal is chewing continues exactly like the running
/// game.
#[test]
fn grazing_survives_save_and_load() {
    let Some(mut sim) = forest(60, 5) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let defs = sim.defs.clone();
    let hare = sim.spawn_animal("Hare", Cell::new(30, 30)).unwrap();
    sim.debug_set_need(hare, NeedKind::Food, 0.02);
    let chewing = |s: &Sim| {
        matches!(
            s.pawn(hare).unwrap().job.as_ref().map(|j| j.kind),
            Some(JobKind::IngestInPlace {
                stage: rimworld_sim::job::IngestStage::Chew { .. },
                ..
            })
        )
    };
    for _ in 0..20_000 {
        sim.tick();
        if chewing(&sim) {
            break;
        }
    }
    assert!(chewing(&sim));
    let data = sim.save();
    let mut copy = Sim::load(defs, &data).expect("loads");
    copy.outdoor_temperature = 15.0;
    sim.forget_unsaved_state();
    for _ in 0..3000 {
        sim.tick();
        copy.tick();
    }
    assert!(sim.save() == copy.save(), "diverged after reload");
}
