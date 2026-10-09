//! Diagnostic: summarises a Ferrocolony save named by `FERROCOLONY_SAVE`
//! (items, work tables and bills, plants, pawns). Does nothing without it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::Sim;

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

#[test]
fn inspect_save() {
    let Some(path) = std::env::var_os("FERROCOLONY_SAVE") else {
        return;
    };
    let defs = real_defs().expect("install");
    let data = std::fs::read_to_string(path).unwrap();
    let mut sim = Sim::load(defs.clone(), &data).unwrap();
    println!("tick {}", sim.tick_count());
    let mut items: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for i in sim.map.items() {
        if i.is_filth() {
            continue;
        }
        let e = items
            .entry(defs.things[i.def].def_name.clone())
            .or_default();
        e.0 += i.stack_count;
        if i.forbidden {
            e.1 += i.stack_count;
        }
    }
    for (name, (n, forbidden)) in &items {
        println!("item {name} x{n} (forbidden {forbidden})");
    }
    for s in sim.map.structures() {
        let bills = sim.bills(s.id);
        if !bills.is_empty() {
            println!(
                "table {} at {:?}: {:?}",
                defs.things[s.def].def_name, s.footprint.center, bills
            );
            println!("  fuel {:?}", sim.fuel_at(s.footprint.center));
        }
    }
    let mut built: BTreeMap<String, usize> = BTreeMap::new();
    for st in sim.map.structures() {
        *built
            .entry(defs.things[st.def].def_name.clone())
            .or_default() += 1;
    }
    println!(
        "STRUCTURES {built:?} constructibles {}",
        sim.map.constructibles().len()
    );
    let mut plants: BTreeMap<String, (usize, f32)> = BTreeMap::new();
    for p in sim.map.plants() {
        if p.sown {
            let e = plants
                .entry(defs.things[p.def].def_name.clone())
                .or_default();
            e.0 += 1;
            e.1 = e.1.max(p.growth);
        }
    }
    println!("sown plants {plants:?}");
    let ids: Vec<_> = sim.pawns().iter().map(|p| p.id).collect();
    for id in ids {
        let p = sim.pawn(id).unwrap();
        let work: Vec<String> = p
            .work
            .as_ref()
            .map(|w| {
                defs.work_types
                    .iter()
                    .map(|(t, d)| format!("{}={}", d.def_name, w.raw(t)))
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "pawn {} {} skills {:?} work {work:?}",
            p.name,
            sim.job_report(p),
            p.skills
        );
        let (mood, thoughts) = sim.mood_view(id);
        println!(
            "  mood {mood:?} {thoughts:?} state {:?}",
            sim.mental_state_of(id)
        );
    }
    let ticks: u64 = std::env::var("FERROCOLONY_TICKS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(6000);
    let meal = defs.things.id("MealSimple");
    let mut ate = BTreeMap::new();
    let mut most_meals = 0;
    let mut last: BTreeMap<String, String> = BTreeMap::new();
    for _ in 0..ticks {
        sim.tick();
        if std::env::var_os("FERROCOLONY_TRACE").is_some() {
            for p in sim.pawns() {
                let desc = format!(
                    "{:?} carried {:?} at {:?}",
                    p.job.as_ref().map(|j| j.kind),
                    p.carried
                        .map(|c| (defs.things[c.def].def_name.clone(), c.count)),
                    p.position
                );
                if last.get(&p.name) != Some(&desc) {
                    println!("t{} {} {}", sim.tick_count(), p.name, desc);
                    last.insert(p.name.clone(), desc);
                }
            }
        }
        for p in sim.pawns() {
            if let Some(j) = &p.job
                && let Some(d) = j.def
                && defs.jobs[d].def_name == "Ingest"
                && let Some(c) = p.carried
            {
                ate.insert(
                    (p.name.clone(), defs.things[c.def].def_name.clone()),
                    sim.tick_count(),
                );
            }
        }
        let meals: u32 = sim
            .map
            .items()
            .iter()
            .filter(|i| Some(i.def) == meal)
            .map(|i| i.stack_count)
            .sum();
        most_meals = most_meals.max(meals);
    }
    println!(
        "after {ticks} ticks: most simple meals in stock {most_meals}; eaten (last tick) {ate:?}"
    );
    let cur = sim.current_research().map(str::to_owned);
    println!(
        "RESEARCH {cur:?} {:?}",
        cur.as_deref().map(|p| sim.research_progress(p))
    );
}
