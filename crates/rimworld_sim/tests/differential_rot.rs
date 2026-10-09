//! Differential replay of the rot probe trace (local, gitignored): food
//! lying outdoors, rot progress of each stack every tick, and a meal that
//! rots away. The game's cell temperature is fed in (rooms are not
//! modelled). Skipped without an install or the trace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
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
fn food_rots_outdoors_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("rot1", "trace_rot_outdoors.csv") else {
        eprintln!("skipping: no rot trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(40, 40), soil));
    let mut items = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.trim_start_matches('#').split(',').collect();
        if line.starts_with("#thing,") {
            let def = defs.things.id(f[1]).unwrap();
            let cell = Cell::new(f[2].parse().unwrap(), f[3].parse().unwrap());
            let id = sim.spawn_item(def, cell, f[4].parse().unwrap());
            sim.map.set_item_id_number(id, f[5].parse().unwrap());
            sim.map.set_item_rot(id, f[6].parse().unwrap());
            items.push(id);
        } else if line.starts_with("#startTick,") {
            sim.debug_set_tick(f[1].parse::<u64>().unwrap() - 1);
        } else if !line.starts_with('#') && !line.starts_with("tick") && !line.is_empty() {
            rows.push(f.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        }
    }
    let mut destroyed = 0;
    // The outdoor room starts at the game's first cell temperature; from
    // then on our room model follows the outdoor temperature fed in.
    sim.debug_set_room_temperatures(rows[0][1].parse().unwrap());
    for row in &rows {
        let t: u64 = row[0].parse().unwrap();
        sim.outdoor_temperature = row[2].parse().unwrap();
        while sim.tick_count() < t {
            sim.tick();
        }
        let cell_temp: f32 = row[1].parse().unwrap();
        let ours = sim.cell_temperature(Cell::new(21, 21));
        assert!(
            (ours - cell_temp).abs() < 1e-6,
            "tick {t}: cell temperature {ours} vs {cell_temp}"
        );
        for (k, (&id, game)) in items.iter().zip(row[3].split(';')).enumerate() {
            match (sim.map.item(id), game) {
                (None, "-") => {}
                (Some(item), g) if g != "-" => {
                    let g: f32 = g.parse().unwrap();
                    assert!(
                        (item.rot - g).abs() <= g.abs() * 1e-6 + 1e-4,
                        "tick {t}: item {k} rot {} vs {g}",
                        item.rot
                    );
                }
                (ours, g) => panic!("tick {t}: item {k} present {} vs game {g}", ours.is_some()),
            }
        }
        destroyed = items
            .iter()
            .filter(|&&id| sim.map.item(id).is_none())
            .count();
    }
    assert_eq!(destroyed, 1, "the near-rotten meal rots away");
    eprintln!("rot: {} ticks match", rows.len());
}
