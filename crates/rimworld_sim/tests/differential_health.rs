//! Differential replay of the health probe trace (local, gitignored):
//! untended injuries on a standing colonist — pain, bleeding, blood loss,
//! natural healing and capacities, tick by tick. Skipped without an
//! install or the trace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::hash::is_hash_interval_tick_delta;
use rimworld_sim::health::{Health, HealthView, Hediff, health_interval};
use rimworld_sim::rand::Rand;
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

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn bleeding_and_natural_healing_match_the_original_game() {
    replay("trace_health_bleed.csv");
}

#[test]
fn bleeding_out_downs_then_kills_as_in_the_original_game() {
    replay("trace_health_bleedout.csv");
}

/// Replays a health trace tick by tick; returns without a trace or install.
fn replay(name: &str) {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("health1", name) else {
        eprintln!("skipping: no trace {name}");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let human = defs.things.get("Human").unwrap();
    let race = human.race.as_ref().unwrap();
    let body = defs.bodies.get(race.body.as_deref().unwrap()).unwrap();
    let blood_loss = defs.hediffs.id("BloodLoss").unwrap();

    let (mut id, mut scale, mut start) = (0, 1.0f32, 0u64);
    let mut health = Health::default();
    let mut injuries = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.trim_start_matches('#').split(',').collect();
        if line.starts_with('#') {
            match f[0] {
                "thingIDNumber" => id = f[1].parse().unwrap(),
                "healthScale" => scale = f[1].parse().unwrap(),
                "startTick" => start = f[1].parse().unwrap(),
                "injury" => {
                    let part: usize = f[3].parse().unwrap();
                    assert_eq!(body.parts[part].def, f[2], "part order differs");
                    injuries.push(health.hediffs.len());
                    health.hediffs.push(Hediff {
                        def: defs.hediffs.id(f[1]).unwrap(),
                        part: Some(part),
                        severity: f[4].parse().unwrap(),
                        age_ticks: 0,
                        comps: Default::default(),
                    });
                }
                _ => {}
            }
        } else if !line.starts_with("tick") && !line.is_empty() {
            rows.push(f.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        }
    }
    assert!(rows.len() > 7000);

    let mut rng = Rand::new(1);
    let (mut heals, mut bleeds, mut downed) = (0, 0, false);
    let mut died = None;
    for row in &rows {
        let t: u64 = row[0].parse().unwrap();
        if t > start {
            let bleed_tick = is_hash_interval_tick_delta(t, id, 60, 1);
            let heal_tick = is_hash_interval_tick_delta(t, id, 600, 1);
            bleeds += bleed_tick as u32;
            let before: f32 = injuries.iter().map(|&i| health.hediffs[i].severity).sum();
            health_interval(
                &mut health,
                &defs,
                body,
                scale,
                race.bleed_rate_factor,
                1,
                bleed_tick,
                heal_tick,
                is_hash_interval_tick_delta(t, id, 200, 1),
                downed,
                None,
                false,
                Some(1.0),
                &mut rng,
            );
            let after: f32 = injuries.iter().map(|&i| health.hediffs[i].severity).sum();
            if row[9] == "1" {
                let view = HealthView {
                    defs: &defs,
                    body,
                    hediffs: &health.hediffs,
                    health_scale: scale,
                    bleed_rate_factor: race.bleed_rate_factor,
                };
                assert!(view.should_be_dead(), "tick {t}: the game's pawn died");
                died = Some(t);
                break;
            }
            let game: Vec<f32> = row[4].split(';').map(|s| s.parse().unwrap()).collect();
            if heal_tick {
                heals += 1;
                // Same amount healed; which injury heals is a random pick,
                // so take the game's choice.
                let game_before: f32 = injuries
                    .iter()
                    .map(|&i| health.hediffs[i].severity)
                    .sum::<f32>()
                    + (before - after);
                assert!(
                    close(before - after, game_before - game.iter().sum::<f32>(), 1e-4),
                    "tick {t}: healed {} vs game",
                    before - after
                );
                for (k, &i) in injuries.iter().enumerate() {
                    health.hediffs[i].severity = game[k];
                }
            }
            for (k, &i) in injuries.iter().enumerate() {
                assert!(
                    close(health.hediffs[i].severity, game[k], 1e-4),
                    "tick {t}: injury {k} {} vs {}",
                    health.hediffs[i].severity,
                    game[k]
                );
            }
        }
        let view = HealthView {
            defs: &defs,
            body,
            hediffs: &health.hediffs,
            health_scale: scale,
            bleed_rate_factor: race.bleed_rate_factor,
        };
        let num = |k: usize| row[k].parse::<f32>().unwrap();
        let ours_loss = health
            .hediffs
            .iter()
            .find(|h| h.def == blood_loss)
            .map_or(0.0, |h| h.severity);
        let checks = [
            ("pain", view.pain_total(), num(1), 1e-5),
            ("bleed", view.bleed_rate_total(), num(2), 1e-5),
            ("blood loss", ours_loss, num(3), 1e-5),
            (
                "consciousness",
                view.capacity("Consciousness"),
                num(5),
                1e-5,
            ),
            ("moving", view.capacity("Moving"), num(6), 1e-5),
            ("manipulation", view.capacity("Manipulation"), num(7), 1e-5),
        ];
        for (name, ours, game, tol) in checks {
            assert!(
                close(ours, game, tol),
                "tick {t}: {name} {ours} vs {game}",
                t = row[0]
            );
        }
        assert!(!view.should_be_dead(), "tick {}: alive in the game", row[0]);
        downed = view.should_be_downed(0.8);
        assert_eq!(downed, row[8] == "1", "tick {}: downed", row[0]);
    }
    assert!(
        heals >= 13 && bleeds >= 130,
        "{heals} heals, {bleeds} bleeds"
    );
    eprintln!("{name}: {} ticks match; died at {died:?}", rows.len());
}

#[test]
fn starving_builds_malnutrition_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("health1", "trace_health_starve.csv") else {
        eprintln!("skipping: no trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> i64 {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .parse()
            .unwrap()
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = rimworld_sim::Sim::new(
        defs.clone(),
        rimworld_sim::Map::new(rimworld_sim::GridSize::new(20, 20), soil),
    );
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim
        .spawn_pawn(kind, "probe", rimworld_sim::Cell::new(10, 10))
        .unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber") as i32);
    sim.set_update_rate(pawn, header("updateRate") as u32);
    sim.debug_set_tick(header("startTick") as u64);
    sim.debug_set_need(pawn, rimworld_sim::NeedKind::Food, 0.0);
    sim.debug_set_need(pawn, rimworld_sim::NeedKind::Rest, 1.0);
    let malnutrition = defs.hediffs.id("Malnutrition").unwrap();
    let mut checked = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let t: u64 = f[0].parse().unwrap();
        while sim.tick_count() < t {
            sim.tick();
        }
        let p = sim.pawn(pawn).unwrap();
        let ours = p
            .health
            .hediffs
            .iter()
            .find(|h| h.def == malnutrition)
            .map_or(0.0, |h| h.severity);
        let game: f32 = f[11].parse().unwrap();
        assert!(
            (ours - game).abs() <= game * 1e-5 + 1e-7,
            "tick {t}: malnutrition {ours} vs {game}"
        );
        let view = sim.health_view(pawn).unwrap();
        let consciousness: f32 = f[5].parse().unwrap();
        assert!((view.capacity("Consciousness") - consciousness).abs() < 1e-5);
        let hunger: f32 = f[12].parse().unwrap();
        assert!(
            (view.hunger_rate_factor() - hunger).abs() < 1e-6,
            "tick {t}"
        );
        checked += 1;
    }
    eprintln!("starving: {checked} ticks match");
}

/// The temperature-illness probe: a drafted colonist without apparel in a
/// 3×3 roofed room the probe holds at -30, 25, 60 then 22 °C. Hypothermia
/// and heatstroke severities must match every tick, including the
/// colonist's own body heat warming the room every 250 ticks (frostbite
/// rolls are random and none struck in the recorded run).
#[test]
fn hypothermia_and_heatstroke_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("hypo1", "trace_hypo.csv") else {
        eprintln!("skipping: no temperature illness trace");
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
    let comfy: Vec<f32> = header("comfy").iter().map(|v| v.parse().unwrap()).collect();
    // The race's own range: the stripped colonist has no other modifiers.
    let human = defs.things.get("Human").unwrap();
    assert_eq!(human.stat("ComfyTemperatureMin"), Some(comfy[0]));
    assert_eq!(human.stat("ComfyTemperatureMax"), Some(comfy[1]));
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(9, 9), soil));
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 2..=6 {
        for z in 2..=6 {
            if x == 2 || x == 6 || z == 2 || z == 6 {
                sim.debug_spawn_building(wall, wood, Cell::new(x, z));
            }
            sim.map.set_roof(Cell::new(x, z), true);
        }
    }
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "probe", Cell::new(4, 4)).unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber")[0].parse().unwrap());
    let hypo = defs.hediffs.id("Hypothermia").unwrap();
    let heat = defs.hediffs.id("Heatstroke").unwrap();
    let sev = |sim: &Sim, d| {
        sim.pawn(pawn)
            .unwrap()
            .health
            .hediffs
            .iter()
            .find(|x| x.def == d)
            .map_or(0.0, |x| x.severity)
    };
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    sim.debug_set_tick(rows[0][0].parse().unwrap());
    let mut set: f32 = rows[0][1].parse().unwrap();
    let mut checked = 0;
    let (mut peak_hypo, mut peak_heat) = (0.0f32, 0.0f32);
    for f in &rows[1..] {
        let t: u64 = f[0].parse().unwrap();
        assert!(f[5].is_empty(), "tick {t}: frostbite struck in the game");
        // The probe sets the room after the pawns tick: they see the
        // previous tick's setting.
        sim.debug_set_room_temperatures(set);
        sim.outdoor_temperature = set;
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
        sim.tick();
        assert_eq!(sim.tick_count(), t);
        let (gh, gs): (f32, f32) = (f[3].parse().unwrap(), f[4].parse().unwrap());
        let (oh, os) = (sev(&sim, hypo), sev(&sim, heat));
        assert!((oh - gh).abs() < 1e-6, "tick {t}: hypothermia {oh} vs {gh}");
        assert!((os - gs).abs() < 1e-6, "tick {t}: heatstroke {os} vs {gs}");
        assert_eq!(
            sim.pawn(pawn).unwrap().health.downed,
            f[6] == "1",
            "tick {t}: downed"
        );
        peak_hypo = peak_hypo.max(gh);
        peak_heat = peak_heat.max(gs);
        set = f[1].parse().unwrap();
        checked += 1;
    }
    assert!(checked > 25_000, "{checked}");
    assert!(
        peak_hypo > 0.37 && peak_heat > 0.1,
        "{peak_hypo} {peak_heat}"
    );
    eprintln!(
        "temperature illness: {checked} ticks match (hypothermia peaked at {peak_hypo}, heatstroke at {peak_heat})"
    );
}

/// The apparel probe: a stripped colonist puts on sets of apparel piece
/// by piece; the comfortable temperature range must match after each.
#[test]
fn apparel_insulation_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("apparel1", "trace_apparel.csv") else {
        eprintln!("skipping: no apparel trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(9, 9), soil));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let mut pawn = None;
    let mut checked = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("set"))
    {
        let f: Vec<&str> = line.split(',').collect();
        if f[1] == "-" {
            // A new set: a fresh, naked colonist.
            pawn = Some(
                sim.spawn_pawn(kind, "probe", Cell::new(1 + checked % 7, 4))
                    .unwrap(),
            );
        } else {
            let (d, s) = f[1].split_once(':').unwrap();
            let (d, s) = (defs.things.id(d).unwrap(), defs.things.id(s));
            sim.wear_apparel(pawn.unwrap(), d, s);
            // The piece's armor ratings (`StatPart_Stuff` with the stuff's
            // armor power).
            for (k, stat) in ["ArmorRating_Sharp", "ArmorRating_Blunt", "ArmorRating_Heat"]
                .iter()
                .enumerate()
            {
                let ours = rimworld_sim::stats::def_stat(
                    &defs,
                    &defs.things[d],
                    s.map(|s| &defs.things[s]),
                    stat,
                );
                let game: f32 = f[4 + k].parse().unwrap();
                assert!(
                    (ours - game).abs() < 1e-4,
                    "{line}: {stat} {ours} vs {game}"
                );
            }
        }
        let p = pawn.unwrap();
        let (gmin, gmax): (f32, f32) = (f[2].parse().unwrap(), f[3].parse().unwrap());
        let omin = sim.pawn_stat(p, "ComfyTemperatureMin").unwrap();
        let omax = sim.pawn_stat(p, "ComfyTemperatureMax").unwrap();
        assert!((omin - gmin).abs() < 1e-3, "{line}: min {omin} vs {gmin}");
        assert!((omax - gmax).abs() < 1e-3, "{line}: max {omax} vs {gmax}");
        checked += 1;
    }
    assert_eq!(checked, 12);
    eprintln!("apparel insulation and armor: {checked} rows match");
}

/// The rescue probe: a doctor carries a colonist knocked out by
/// anesthetic to a wooden bed. The doctor's walk (slower while carrying),
/// the pick-up, the tuck-in and the bed claim must match every tick.
#[test]
fn rescuing_a_downed_colonist_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("rescue1", "trace_rescue.csv") else {
        eprintln!("skipping: no rescue trace");
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
    assert!(!text.contains("#error"), "probe timed out");
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
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    let b = header("bed");
    let bed_id = sim.debug_spawn_building(bed, wood, cell(&b));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let d = header("doctor");
    let p = header("patient");
    let doctor = sim.spawn_pawn(kind, "doctor", cell(&d[1..3])).unwrap();
    let patient = sim.spawn_pawn(kind, "patient", cell(&p[1..3])).unwrap();
    sim.set_thing_id_number(doctor, d[0].parse().unwrap());
    sim.set_thing_id_number(patient, p[0].parse().unwrap());
    sim.set_move_speed(doctor, d[3].parse().unwrap());
    sim.set_update_rate(doctor, d[4].parse().unwrap());
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 3 } else { 0 });
        sim.set_work_priority(patient, &wt, 0);
    }
    sim.debug_add_hediff(patient, "Anesthetic", 1.0);
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(doctor);
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    let mut checked = 0;
    for f in &rows {
        let tick: u64 = f[0].parse().unwrap();
        // Idle wandering afterwards is random.
        if f[8].contains("Wander") {
            break;
        }
        while sim.tick_count() < tick {
            for pw in [doctor, patient] {
                sim.debug_set_need(pw, NeedKind::Food, 1.0);
                sim.debug_set_need(pw, NeedKind::Rest, 1.0);
            }
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let dp = sim.pawn(doctor).unwrap();
        let job = dp
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |j| defs.jobs[j].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: doctor job");
        assert_eq!(
            dp.position,
            Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
            "{ctx}: doctor"
        );
        let (moving, total): (bool, f32) = (f[3] == "1", f[7].parse().unwrap());
        if moving && total != 1.0 {
            let step = dp.step.unwrap_or_else(|| panic!("{ctx}: doctor mid-step"));
            let left: f32 = f[6].parse().unwrap();
            assert!(
                (step.cost_left - left).abs() < 1e-3,
                "{ctx}: cost left {} vs {left}",
                step.cost_left
            );
        }
        let pp = sim.pawn(patient).unwrap();
        assert_eq!(
            pp.position,
            Cell::new(f[10].parse().unwrap(), f[11].parse().unwrap()),
            "{ctx}: patient"
        );
        assert_eq!(pp.carried_by.is_some(), f[19] == "1", "{ctx}: carrying");
        let in_bed = matches!(
            pp.job.as_ref().map(|j| j.kind),
            Some(rimworld_sim::job::JobKind::LayDown { bed: Some(_), spot }) if spot == pp.position
        );
        assert_eq!(in_bed, f[20] == "1", "{ctx}: in bed");
        assert_eq!(pp.owned_bed == Some(bed_id), f[21] == "1", "{ctx}: owner");
        checked += 1;
    }
    assert!(checked > 200, "{checked}");
    eprintln!("rescue: {checked} ticks match");
}

/// The anesthetic probe: severity falls 0.8 a day in 200-tick steps
/// (`HediffComp_SeverityPerDay`); below 0.8 the pawn is no longer downed.
/// The random disappearance came after the recorded 30000 ticks.
#[test]
fn anesthetic_wears_off_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("anesthetic1", "trace_anesthetic.csv") else {
        eprintln!("skipping: no anesthetic trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> String {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .to_owned()
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(9, 9), soil));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "probe", Cell::new(4, 4)).unwrap();
    sim.set_thing_id_number(pawn, header("thingIDNumber").parse().unwrap());
    // The probe pawn's update rate was 3 (the 200-tick steps land on
    // multiples of 3; the probe did not log it).
    sim.set_update_rate(pawn, 3);
    let add: u64 = header("addTick").parse().unwrap();
    sim.debug_set_tick(add - 1);
    sim.debug_add_hediff(pawn, "Anesthetic", 1.0);
    let anesthetic = defs.hediffs.id("Anesthetic").unwrap();
    let mut checked = 0;
    let mut seen_rows = std::collections::HashSet::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let t: u64 = f[0].parse().unwrap();
        if !seen_rows.insert(t) {
            continue;
        }
        while sim.tick_count() < t {
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.tick();
        }
        let p = sim.pawn(pawn).unwrap();
        let h = p
            .health
            .hediffs
            .iter()
            .find(|h| h.def == anesthetic)
            .unwrap();
        let game: f32 = f[1].parse().unwrap();
        assert!(
            (h.severity - game).abs() < 1e-5,
            "tick {t}: {} vs {game}",
            h.severity
        );
        assert_eq!(p.health.downed, f[3] == "1", "tick {t}: downed");
        checked += 1;
    }
    assert!(checked > 29_000, "{checked}");
    eprintln!("anesthetic: {checked} ticks match");
}

/// The feeding probe: a doctor brings a survival meal to an anesthetized
/// colonist lying in bed and feeds them (chewing 1.5 × the meal's time).
#[test]
fn feeding_a_bedridden_colonist_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("feed1", "trace_feed.csv") else {
        eprintln!("skipping: no feeding trace");
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
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    let bed_id = sim.debug_spawn_building(bed, wood, cell(&header("bed")));
    let m = header("meal");
    let meal = sim.spawn_item(
        defs.things.id("MealSurvivalPack").unwrap(),
        cell(&m),
        m[2].parse().unwrap(),
    );
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let d = header("doctor");
    let p = header("patient");
    let doctor = sim.spawn_pawn(kind, "doctor", cell(&d[1..3])).unwrap();
    let patient = sim
        .spawn_pawn(kind, "patient", Cell::new(origin.x + 1, origin.z))
        .unwrap();
    sim.set_thing_id_number(doctor, d[0].parse().unwrap());
    sim.set_thing_id_number(patient, p[0].parse().unwrap());
    sim.set_move_speed(doctor, d[3].parse().unwrap());
    sim.set_update_rate(doctor, d[4].parse().unwrap());
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 3 } else { 0 });
        sim.set_work_priority(patient, &wt, 0);
    }
    sim.debug_add_hediff(patient, "Anesthetic", 1.0);
    sim.debug_tuck_into_bed(patient, bed_id);
    let food: Vec<f32> = header("patientFood")
        .iter()
        .map(|v| v.parse().unwrap())
        .collect();
    sim.debug_set_need(patient, NeedKind::Food, food[0] / food[1]);
    let undraft: u64 = header("undraftTick")[0].parse().unwrap();
    sim.debug_set_tick(undraft);
    sim.debug_find_and_start_job(doctor);
    let mut checked = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
    {
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        if f[8].contains("Wander") {
            break;
        }
        while sim.tick_count() < tick {
            sim.debug_set_need(doctor, NeedKind::Food, 1.0);
            sim.debug_set_need(doctor, NeedKind::Rest, 1.0);
            sim.debug_set_need(patient, NeedKind::Rest, 1.0);
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let dp = sim.pawn(doctor).unwrap();
        let job = dp
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |j| defs.jobs[j].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: doctor job");
        assert_eq!(
            dp.position,
            Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
            "{ctx}: doctor"
        );
        let carried: u32 = f[19].parse().unwrap();
        assert_eq!(dp.carried.map_or(0, |c| c.count), carried, "{ctx}: carried");
        let level = sim
            .pawn(patient)
            .unwrap()
            .needs
            .get(NeedKind::Food)
            .unwrap()
            .level;
        let game: f32 = f[20].parse().unwrap();
        assert!(
            (level - game).abs() < 1e-4,
            "{ctx}: patient food {level} vs {game}"
        );
        let left: u32 = f[21].parse().unwrap();
        assert_eq!(
            sim.map.item(meal).map_or(0, |i| i.stack_count),
            left,
            "{ctx}: meal left"
        );
        checked += 1;
    }
    assert!(checked > 900, "{checked}");
    eprintln!("feeding: {checked} ticks match");
}

#[test]
fn tending_a_colonist_in_bed_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("tend1", "trace_tend.csv") else {
        eprintln!("skipping: no tending trace");
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
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
    // Start where the doctor takes the job (the patient already lies in
    // bed; the wandering before is random).
    let start = rows
        .iter()
        .position(|r| r[8] == "TendPatient")
        .expect("doctor tends");
    let before = &rows[start - 1];
    let hediffs = |r: &[&str]| -> Vec<(String, f32, bool)> {
        r[22..]
            .join(",")
            .split(';')
            .filter_map(|h| {
                let f: Vec<&str> = h.split(':').collect();
                f[0].starts_with("Cut@")
                    .then(|| (f[0][4..].to_owned(), f[1].parse().unwrap(), f[2] == "1"))
            })
            .collect()
    };

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
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    let bed_id = sim.debug_spawn_building(bed, wood, cell(&header("bed")));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let d = header("doctor");
    let p = header("patient");
    let doctor = sim
        .spawn_pawn(
            kind,
            "doctor",
            Cell::new(before[1].parse().unwrap(), before[2].parse().unwrap()),
        )
        .unwrap();
    let patient = sim
        .spawn_pawn(kind, "patient", Cell::new(origin.x + 1, origin.z))
        .unwrap();
    sim.set_thing_id_number(doctor, d[0].parse().unwrap());
    sim.set_thing_id_number(patient, p[0].parse().unwrap());
    sim.set_move_speed(doctor, d[3].parse().unwrap());
    sim.set_update_rate(doctor, d[4].parse().unwrap());
    sim.set_update_rate(patient, p[4].parse().unwrap());
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 3 } else { 0 });
        let patient_work = wt == "Patient" || wt == "PatientBedRest";
        sim.set_work_priority(patient, &wt, if patient_work { 3 } else { 0 });
    }
    let med = header("doctorMedicine");
    sim.set_skill(doctor, "Medicine", med[0].parse().unwrap());
    sim.debug_set_skill_xp(doctor, "Medicine", med[1].parse().unwrap());
    sim.debug_set_passion(doctor, "Medicine", rimworld_sim::stats::Passion::None);
    let tend_speed: f32 = med[2].parse().unwrap();
    let tend_quality: f32 = med[3].parse().unwrap();
    assert!(close(
        sim.pawn_stat(doctor, "MedicalTendSpeed").unwrap(),
        tend_speed,
        1e-5
    ));
    assert!(close(
        sim.pawn_stat(doctor, "MedicalTendQuality").unwrap(),
        tend_quality,
        1e-5
    ));
    for (part, severity, _) in hediffs(before) {
        sim.debug_add_injury(patient, "Cut", &part, severity);
    }
    sim.debug_tuck_into_bed(patient, bed_id);
    let tick: u64 = rows[start][0].parse().unwrap();
    sim.debug_set_tick(tick);
    sim.debug_find_and_start_job(doctor);

    let xp0 = sim.pawn(doctor).unwrap().skills.xp("Medicine");
    let mut checked = 0;
    for f in &rows[start..] {
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            for pawn in [doctor, patient] {
                sim.debug_set_need(pawn, NeedKind::Food, 1.0);
                sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            }
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let dp = sim.pawn(doctor).unwrap();
        let job = dp
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |j| defs.jobs[j].def_name.clone());
        if f[8] != "TendPatient" {
            assert_ne!(job, "TendPatient", "{ctx}: the tending ends");
            break;
        }
        assert_eq!(job, f[8], "{ctx}: doctor job");
        assert_eq!(
            dp.position,
            Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
            "{ctx}: doctor"
        );
        // Which injuries are tended, tick by tick (the order follows the
        // tend priority: bleed rate, then severity).
        let pp = sim.pawn(patient).unwrap();
        let ours: Vec<(String, bool)> = pp
            .health
            .hediffs
            .iter()
            .filter(|h| defs.hediffs[h.def].def_name == "Cut")
            .map(|h| {
                let body = defs
                    .bodies
                    .get(
                        defs.things[pp.race]
                            .race
                            .as_ref()
                            .unwrap()
                            .body
                            .as_deref()
                            .unwrap(),
                    )
                    .unwrap();
                (
                    body.parts[h.part.unwrap()].def.clone(),
                    h.comps.tend_ticks_left > 0,
                )
            })
            .collect();
        for (part, _, tended) in hediffs(f) {
            let mine = ours.iter().find(|(p, _)| *p == part).map(|o| o.1);
            assert_eq!(mine, Some(tended), "{ctx}: {part} tended");
        }
        let bed_flag = f[21] == "1";
        assert_eq!(
            pp.position == cell(&header("bed")),
            bed_flag,
            "{ctx}: in bed"
        );
        checked += 1;
    }
    let tends = sim
        .pawn(patient)
        .unwrap()
        .health
        .hediffs
        .iter()
        .filter(|h| h.comps.tend_ticks_left > 0)
        .count();
    assert_eq!(tends, 3);
    // 250 xp per tend, ×0.35 without passion.
    let xp = sim.pawn(doctor).unwrap().skills.xp("Medicine");
    let game_xp: f32 =
        rows.last().unwrap()[20].parse::<f32>().unwrap() - 1000.0 * med[0].parse::<f32>().unwrap();
    assert!(
        close(xp - xp0, game_xp - med[1].parse::<f32>().unwrap(), 1e-3),
        "xp {xp}"
    );
    // Every tend's quality lies within ±0.25 of the base quality (0.3 ×
    // MedicalTendQuality), clamped to 0..0.7.
    let base = tend_quality * 0.3;
    for h in &sim.pawn(patient).unwrap().health.hediffs {
        if h.comps.tend_ticks_left > 0 {
            let q = h.comps.tend_quality;
            assert!(q >= (base - 0.25).max(0.0) - 1e-6 && q <= (base + 0.25).min(0.7) + 1e-6);
        }
    }
    assert!(checked > 3900, "{checked}");
    eprintln!("tending: {checked} ticks match");

    // Healing in the trace: each 600-tick step takes 16/100 from one
    // injury (8 + 4 lying + the bed's 4) and, once any is tended, another
    // 8 × lerp(0.5, 1.5, quality) / 100 from a tended one.
    let quality = |r: &[&str], part: &str| -> f32 {
        r[22..]
            .join(",")
            .split(';')
            .find(|h| h.starts_with(&format!("Cut@{part}:")))
            .map(|h| h.split(':').nth(3).unwrap().parse().unwrap())
            .unwrap()
    };
    let mut steps = 0;
    for w in rows[start..].windows(2) {
        let (a, b) = (hediffs(&w[0]), hediffs(&w[1]));
        let drop: f32 = a.iter().map(|h| h.1).sum::<f32>() - b.iter().map(|h| h.1).sum::<f32>();
        if drop.abs() < 1e-6 {
            continue;
        }
        let tended: Vec<f32> = a
            .iter()
            .filter(|h| h.2)
            .map(|h| 8.0 * (0.5 + quality(&w[0], &h.0).clamp(0.0, 1.0)) * 0.01)
            .collect();
        let ok = if tended.is_empty() {
            close(drop, 0.16, 1e-4)
        } else {
            tended.iter().any(|t| close(drop, 0.16 + t, 1e-4))
        };
        assert!(
            ok,
            "tick {}: healed {drop}, tended steps {tended:?}",
            w[1][0]
        );
        steps += 1;
    }
    assert!(steps >= 10, "{steps}");
}

#[test]
fn capacities_scale_work_and_move_stats_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("statcap1", "trace_statcap.csv") else {
        eprintln!("skipping: no stat capacity trace");
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
    let columns: Vec<&str> = text
        .lines()
        .find(|l| l.starts_with("step,"))
        .unwrap()
        .split(',')
        .collect();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(9, 9), soil));
    sim.debug_set_sky_glow(Some(header("glow")[0].parse().unwrap()));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "pawn", Cell::new(4, 4)).unwrap();
    for s in header("skills") {
        let (skill, level) = s.split_once('=').unwrap();
        sim.set_skill(pawn, skill, level.parse().unwrap());
    }
    let mut checked = 0;
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("step"))
    {
        let f: Vec<&str> = line.split(',').collect();
        if f[0] != "healthy" {
            let (hediff, rest) = f[0].split_once('@').unwrap();
            let (part, severity) = rest.split_once(':').unwrap();
            sim.debug_add_injury(pawn, hediff, part, severity.parse().unwrap());
        }
        for (k, name) in columns.iter().enumerate().skip(1) {
            let game: f32 = f[k].parse().unwrap();
            let ours = if *name == "pain" {
                sim.health_view(pawn).unwrap().pain_total()
            } else if defs.capacities.get(name).is_some() {
                sim.health_view(pawn).unwrap().capacity(name)
            } else {
                sim.pawn_stat(pawn, name).unwrap()
            };
            assert!(
                (ours - game).abs() <= 1e-4 * game.abs().max(1.0),
                "{}: {name} ours {ours} game {game}",
                f[0]
            );
            checked += 1;
        }
        // The speed the pawn actually walks at uses the same factor.
        let p = sim.pawn(pawn).unwrap();
        let game: f32 = f[columns.iter().position(|c| *c == "MoveSpeed").unwrap()]
            .parse()
            .unwrap();
        let walk = p.base_move_speed * p.move_capacity_factor;
        assert!(
            (walk - game).abs() < 1e-4,
            "{}: walking speed {walk} vs {game}",
            f[0]
        );
    }
    assert!(checked > 150, "{checked}");
    eprintln!("stat capacities: {checked} values match");
}

#[test]
fn corpses_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("corpse1", "trace_corpse.csv") else {
        eprintln!("skipping: no corpse trace");
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
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(40, 40), soil));
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let wood = defs.things.id("WoodLog").unwrap();
    let mut corpses = Vec::new();
    for (k, name) in ["a", "b"].into_iter().enumerate() {
        let h = header(name);
        let at = Cell::new(h[1].parse().unwrap(), h[2].parse().unwrap());
        if k == 1 {
            // The second colonist died standing on a wood pile.
            sim.spawn_item(wood, at, 20);
        }
        let pawn = sim.spawn_pawn(kind, name, at).unwrap();
        sim.debug_add_hediff(pawn, "BloodLoss", 1.0);
        assert!(sim.pawn(pawn).unwrap().health.dead);
        let corpse = sim.corpse_of(pawn).expect("a corpse");
        assert_eq!(sim.corpse_pawn(corpse), Some(pawn));
        let item = sim.map.item(corpse).unwrap();
        let def = &defs.things[item.def];
        assert_eq!(def.def_name, h[0]);
        assert_eq!(
            item.position,
            Cell::new(h[3].parse().unwrap(), h[4].parse().unwrap()),
            "{name}: placed where it died"
        );
        assert_eq!(def.stat_bases["MaxHitPoints"], h[6].parse::<f32>().unwrap());
        assert_eq!(def.stat_bases["Beauty"], h[8].parse::<f32>().unwrap());
        assert_eq!(
            def.stat_bases["DeteriorationRate"],
            h[9].parse::<f32>().unwrap()
        );
        assert_eq!(def.path_cost, h[11].parse::<i32>().unwrap());
        assert_eq!(def.thing_categories.join(";"), h[12]);
        corpses.push(corpse);
    }
    // The default stockpile refuses corpses; a dumping stockpile takes them.
    let accepts = header("accepts");
    let corpse_def = sim.map.item(corpses[0]).unwrap().def;
    use rimworld_sim::storage::{StoragePreset, ThingFilter};
    let zone = sim.map.storage.add_stockpile(
        Default::default(),
        &[Cell::new(1, 1)],
        ThingFilter::preset(&defs, StoragePreset::DefaultStockpile),
    );
    assert_eq!(
        sim.map.storage.zone(zone).accepts(&defs, corpse_def),
        accepts[0] == "default=True"
    );
    sim.map.storage.set_filter(
        zone,
        ThingFilter::preset(&defs, StoragePreset::DumpingStockpile),
    );
    assert_eq!(
        sim.map.storage.zone(zone).accepts(&defs, corpse_def),
        accepts[1] == "dumping=True"
    );
    // Rot: every step of the trace is the rot rate at the cell's
    // temperature over the 250-tick rare interval; corpses don't vanish.
    let rows: Vec<Vec<f32>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| {
            l.split(',')
                .enumerate()
                .filter(|(i, _)| *i != 3)
                .map(|(_, v)| v.parse().unwrap())
                .collect()
        })
        .collect();
    let mut steps = 0;
    for w in rows.windows(2) {
        let gained = w[1][2] - w[0][2];
        if gained != 0.0 {
            let expected = rimworld_sim::sim::rot_rate_at_temperature(w[0][5]) * 250.0;
            assert!(
                (gained - expected).abs() < 0.05,
                "tick {}: rot {gained} vs {expected}",
                w[1][0]
            );
            steps += 1;
        }
    }
    assert!(steps > 40, "{steps}");
    eprintln!("corpses: placement, stats, storage and {steps} rot steps match");
}

#[test]
fn the_home_area_grows_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("home1", "trace_home.csv") else {
        eprintln!("skipping: no home area trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let row = |k: &str| -> Vec<i32> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k},")))
            .unwrap()
            .split(',')
            .map(|v| v.parse().unwrap())
            .collect()
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let wood = defs.things.id("WoodLog");
    let home_box = |sim: &Sim| -> Vec<i32> {
        let size = sim.map.size();
        let cells: Vec<Cell> = (0..size.width)
            .flat_map(|x| (0..size.height).map(move |z| Cell::new(x, z)))
            .filter(|&c| sim.map.home[c])
            .collect();
        if cells.is_empty() {
            return vec![0];
        }
        vec![
            cells.len() as i32,
            cells.iter().map(|c| c.x).min().unwrap(),
            cells.iter().map(|c| c.z).min().unwrap(),
            cells.iter().map(|c| c.x).max().unwrap(),
            cells.iter().map(|c| c.z).max().unwrap(),
        ]
    };
    let fresh = || Sim::new(defs.clone(), Map::new(GridSize::new(100, 100), soil));
    // A new map has no home area; a blueprint adds none.
    let mut sim = fresh();
    assert_eq!(home_box(&sim), row("start"));
    let wall = defs.things.id("Wall").unwrap();
    sim.place_blueprint(wall, wood, Cell::new(30, 30)).unwrap();
    assert_eq!(home_box(&sim), row("blueprint"));
    // A finished wall (as a frame does).
    let mut sim = fresh();
    sim.debug_spawn_building(wall, wood, Cell::new(30, 30));
    assert_eq!(home_box(&sim), row("wall"));
    // A stockpile cell.
    let mut sim = fresh();
    sim.designate_stockpile(&[Cell::new(60, 60)]);
    assert_eq!(home_box(&sim), row("stockpile"));
    // A two-cell bed facing north.
    let mut sim = fresh();
    sim.debug_spawn_building(defs.things.id("Bed").unwrap(), wood, Cell::new(80, 80));
    assert_eq!(home_box(&sim), row("bed"));
}

#[test]
fn the_radial_pattern_matches_the_original_game() {
    let Some(path) = trace_file("radial1", "trace_radial.csv") else {
        eprintln!("skipping: no radial pattern trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let game: Vec<Cell> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let (x, z) = l.split_once(',').unwrap();
            Cell::new(x.parse().unwrap(), z.parse().unwrap())
        })
        .collect();
    let ours = rimworld_sim::clean::radial_pattern();
    assert_eq!(game.len(), ours.len());
    let first_diff = game.iter().zip(ours).position(|(a, b)| a != b);
    assert_eq!(first_diff, None, "radial pattern differs");
}

#[test]
fn placing_things_near_a_cell_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("place1", "trace_place.csv") else {
        eprintln!("skipping: no placement trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(41, 41), soil));
    let o = Cell::new(20, 20);
    // The setup: walls with a door, stacks and grass.
    for line in text.lines().filter_map(|l| l.strip_prefix("#spawn,")) {
        let f: Vec<&str> = line.split(',').collect();
        let def = defs.things.id(f[0]).unwrap();
        let at = Cell::new(
            o.x + f[1].parse::<i32>().unwrap(),
            o.z + f[2].parse::<i32>().unwrap(),
        );
        let stuff = defs.things.id(f[4]);
        if defs.things[def].plant.is_some() {
            sim.map.spawn_plant(def, at, 1.0, 85.0);
        } else if defs.things[def].category.as_deref() == Some("Building") {
            sim.debug_spawn_building(def, stuff, at);
        } else {
            sim.spawn_item(def, at, f[3].parse().unwrap());
        }
    }
    // Each placement, then every item on the square.
    let mut checked = 0;
    for block in text.split("#place,").skip(1) {
        let mut lines = block.lines();
        let f: Vec<&str> = lines.next().unwrap().split(',').collect();
        let def = defs.things.id(f[0]).unwrap();
        let at = Cell::new(
            o.x + f[2].parse::<i32>().unwrap(),
            o.z + f[3].parse::<i32>().unwrap(),
        );
        assert!(sim.debug_place_near(def, f[1].parse().unwrap(), at));
        let mut game: Vec<(String, u32, i32, i32)> = lines
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let f: Vec<&str> = l.split(',').collect();
                (
                    f[1].to_owned(),
                    f[2].parse().unwrap(),
                    f[3].parse().unwrap(),
                    f[4].parse().unwrap(),
                )
            })
            .collect();
        let mut ours: Vec<(String, u32, i32, i32)> = sim
            .map
            .items()
            .iter()
            .filter(|i| !i.is_filth())
            .map(|i| {
                (
                    defs.things[i.def].def_name.clone(),
                    i.stack_count,
                    i.position.x - o.x,
                    i.position.z - o.z,
                )
            })
            .collect();
        game.sort();
        ours.sort();
        assert_eq!(ours, game, "after placing {} {}", f[1], f[0]);
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn stockpile_presets_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("filter1", "trace_filter.csv") else {
        eprintln!("skipping: no storage filter trace");
        return;
    };
    use rimworld_sim::storage::{StoragePreset, ThingFilter};
    let default = ThingFilter::preset(&defs, StoragePreset::DefaultStockpile);
    let dumping = ThingFilter::preset(&defs, StoragePreset::DumpingStockpile);
    let text = std::fs::read_to_string(path).unwrap();
    let (mut checked, mut wrong) = (0, Vec::new());
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("def,"))
    {
        let f: Vec<&str> = line.split(',').collect();
        // DLC defs from the probe's install are not ours.
        let Some(id) = defs.things.id(f[0]) else {
            continue;
        };
        let storable = defs.things[id].ever_storable();
        if storable != (f[1] == "1") {
            wrong.push(format!("{} storable", f[0]));
        }
        if default.allows(id) != (f[2] == "1") {
            wrong.push(format!("{} default", f[0]));
        }
        if dumping.allows(id) != (f[3] == "1") {
            wrong.push(format!("{} dumping", f[0]));
        }
        checked += 1;
    }
    assert!(
        wrong.is_empty(),
        "{} differ: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(20)]
    );
    assert!(checked > 700, "{checked}");
    eprintln!("stockpile presets: {checked} defs match");
}

#[test]
fn a_downed_colonist_crawls_to_bed_as_in_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("crawl1", "trace_crawl.csv") else {
        eprintln!("skipping: no crawl trace");
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
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').collect())
        .collect();
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
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    sim.debug_spawn_building(bed, wood, cell(&header("bed")));
    let p = header("pawn");
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "crawler", cell(&p[1..3])).unwrap();
    sim.set_thing_id_number(pawn, p[0].parse().unwrap());
    sim.set_update_rate(pawn, p[3].parse().unwrap());
    // Downed by pain, still awake (the probe's bruises).
    sim.debug_add_injury_nth(pawn, "Bruise", "Torso", 0, 30.0);
    sim.debug_add_injury_nth(pawn, "Bruise", "Leg", 0, 20.0);
    sim.debug_add_injury_nth(pawn, "Bruise", "Head", 0, 15.0);
    sim.debug_add_injury_nth(pawn, "Bruise", "Leg", 1, 20.0);
    assert!(sim.pawn(pawn).unwrap().health.downed);
    let crawl: f32 = p[6].parse().unwrap();
    assert!(
        close(sim.pawn_stat(pawn, "CrawlSpeed").unwrap(), crawl, 1e-5),
        "crawl speed"
    );
    // From the tick the crawl starts (the break before it is random).
    let start = rows.iter().position(|r| r[8] == "LayDown").unwrap();
    let tick: u64 = rows[start][0].parse().unwrap();
    sim.debug_set_tick(tick);
    sim.debug_set_crawl_break(pawn, false);
    sim.debug_find_and_start_job(pawn);
    let mut checked = 0;
    for f in &rows[start..] {
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
            sim.debug_set_need(pawn, NeedKind::Food, 1.0);
            sim.tick();
        }
        let ctx = format!("tick {tick}");
        let pp = sim.pawn(pawn).unwrap();
        let job = pp
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map_or("-".to_owned(), |j| defs.jobs[j].def_name.clone());
        assert_eq!(job, f[8], "{ctx}: job");
        assert_eq!(
            pp.position,
            Cell::new(f[1].parse().unwrap(), f[2].parse().unwrap()),
            "{ctx}"
        );
        if f[3] == "1" && f[6] != "0" {
            let left: f32 = f[6].parse().unwrap();
            let ours = pp.step.map_or(0.0, |s| s.cost_left);
            assert!(
                (ours - left).abs() < 0.01,
                "{ctx}: cost left {ours} vs {left}"
            );
        }
        checked += 1;
    }
    assert!(checked > 5000, "{checked}");
    eprintln!("crawling: {checked} ticks match");
}

#[test]
fn a_power_net_matches_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("power1", "trace_power.csv") else {
        eprintln!("skipping: no power trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(70, 70), soil));
    let o = Cell::new(30, 30);
    let mut ids = std::collections::HashMap::new();
    let steel = defs.things.id("Steel");
    for line in text.lines().filter_map(|l| l.strip_prefix("#spawn,")) {
        let f: Vec<&str> = line.split(',').collect();
        let def = defs.things.id(f[0]).unwrap();
        let at = Cell::new(
            o.x + f[1].parse::<i32>().unwrap(),
            o.z + f[2].parse::<i32>().unwrap(),
        );
        let stuff = if defs.things[def].stuff_categories.is_empty() {
            None
        } else {
            steel
        };
        let id = sim.debug_spawn_building(def, stuff, at);
        ids.insert(at, id);
    }
    sim.debug_set_fuel(o, 75.0);
    let start: u64 = text
        .lines()
        .find(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    sim.debug_set_tick(start - 1);
    sim.debug_update_power_nets();
    let generator = ids[&o];
    let battery = ids[&Cell::new(o.x + 5, o.z + 1)];
    let lamps: Vec<_> = [(4, 3), (8, -6), (10, -7), (14, 2), (2, -2)]
        .iter()
        .map(|&(x, z)| ids[&Cell::new(o.x + x, o.z + z)])
        .collect();
    let pos_of = |sim: &Sim, id| {
        let c = sim.map.structure(id).unwrap().footprint.center;
        format!("{}:{}", c.x, c.z)
    };
    // The probe acts after a tick's work (in its post-tick component) and
    // logs that tick's row afterwards; the nets update after the row.
    let mut cut = None;
    let mut flick = false;
    let mut checked = 0;
    for line in text.lines() {
        if let Some(c) = line.strip_prefix("#cut,") {
            let (x, z) = c.split_once(':').unwrap();
            cut = Some(ids[&Cell::new(x.parse().unwrap(), z.parse().unwrap())]);
            continue;
        }
        if line.starts_with("#flickOff") {
            flick = true;
            continue;
        }
        if !line.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        let tick: u64 = f[0].parse().unwrap();
        while sim.tick_count() < tick {
            sim.tick();
        }
        let mut update = false;
        if let Some(c) = cut.take() {
            sim.debug_destroy_structure(c);
            update = true;
        }
        if std::mem::take(&mut flick) {
            sim.set_power_switch(generator, false);
        }
        let ctx = format!("tick {tick}");
        let g = &sim.map.structure(generator).unwrap();
        assert_eq!(g.power.on, f[1] == "1", "{ctx}: generator on");
        assert!(
            (g.power.output - f[2].parse::<f32>().unwrap()).abs() < 1e-3,
            "{ctx}: output"
        );
        assert!(
            (g.fuel - f[3].parse::<f32>().unwrap()).abs() < 1e-3,
            "{ctx}: fuel {} vs {}",
            g.fuel,
            f[3]
        );
        let stored = sim.map.structure(battery).unwrap().power.stored;
        let game: f32 = f[4].parse().unwrap();
        assert!(
            (stored - game).abs() < 1e-3,
            "{ctx}: battery {stored} vs {game}"
        );
        let mut on_game = 0;
        let mut on_ours = 0;
        for (k, &lamp) in lamps.iter().enumerate() {
            let parts: Vec<&str> = f[6 + k].split('|').collect();
            let s = sim.map.structure(lamp).unwrap();
            let parent = s
                .power
                .connect_parent
                .map_or("-".to_owned(), |p| pos_of(&sim, p));
            assert_eq!(parent, parts[0], "{ctx}: lamp {k} wired to");
            on_game += (parts[1] == "1") as u32;
            on_ours += s.power.on as u32;
        }
        assert_eq!(on_ours, on_game, "{ctx}: lamps on");
        assert_eq!(sim.power_nets().len().to_string(), f[11], "{ctx}: nets");
        if update {
            sim.debug_update_power_nets();
        }
        checked += 1;
    }
    assert!(checked > 4000, "{checked}");
    eprintln!("power: {checked} ticks match");
}

#[test]
fn heaters_coolers_and_solar_power_match_the_original_game() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let Some(path) = trace_file("power2", "trace_power2.csv") else {
        eprintln!("skipping: no second power trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(70, 70), soil));
    let o = Cell::new(30, 30);
    let mut ids = std::collections::HashMap::new();
    let steel = defs.things.id("Steel");
    for line in text.lines().filter_map(|l| l.strip_prefix("#spawn,")) {
        let f: Vec<&str> = line.split(',').collect();
        let def = defs.things.id(f[0]).unwrap();
        let at = Cell::new(
            o.x + f[1].parse::<i32>().unwrap(),
            o.z + f[2].parse::<i32>().unwrap(),
        );
        let rot = match f[3] {
            "1" => rimworld_sim::job::Rot4::East,
            "2" => rimworld_sim::job::Rot4::South,
            "3" => rimworld_sim::job::Rot4::West,
            _ => rimworld_sim::job::Rot4::North,
        };
        let stuff = match f[4] {
            "-" => defs.things[def].stuff_categories.first().and(steel),
            s => defs.things.id(s),
        };
        let id = sim.debug_spawn_building_rotated(def, stuff, at, rot);
        sim.debug_set_building_id_number(at, f[5].parse().unwrap());
        ids.insert(at, id);
    }
    for x in (9..=13).chain(17..=21) {
        for z in 1..=5 {
            sim.map.set_roof(o + Cell::new(x, z), true);
        }
    }
    let rows: Vec<Vec<f32>> = text
        .lines()
        .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|l| l.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    let battery = ids[&(o + Cell::new(4, 4))];
    let solar = ids[&(o + Cell::new(1, 1))];
    let heater = ids[&(o + Cell::new(11, 3))];
    let cooler = ids[&(o + Cell::new(19, 0))];
    // From tick 200, when both are on (which starts first is random).
    let start = rows.iter().position(|r| r[0] == 200.0).unwrap();
    let r0 = &rows[start];
    sim.debug_set_tick(r0[0] as u64);
    sim.debug_update_power_nets();
    sim.outdoor_temperature = r0[10];
    sim.debug_set_room_temperatures(r0[10]);
    let room_a = o + Cell::new(11, 3);
    let room_b = o + Cell::new(19, 3);
    sim.debug_set_cell_room_temperature(room_a, r0[6]);
    sim.debug_set_cell_room_temperature(room_b, r0[9]);
    sim.debug_set_stored_energy(battery, r0[3]);
    sim.debug_set_power(heater, true, r0[5]);
    sim.debug_set_power(cooler, true, r0[8]);
    let mut checked = 0;
    let mut worst: f32 = 0.0;
    for r in &rows[start + 1..] {
        let t = r[0] as u64;
        sim.debug_set_sky_glow(Some(r[1]));
        sim.outdoor_temperature = r[10];
        while sim.tick_count() < t {
            sim.tick();
        }
        let ctx = format!("tick {t}");
        let s = sim.map.structure(solar).unwrap();
        assert!(
            (s.power.output - r[2]).abs() < 0.01,
            "{ctx}: solar {} vs {}",
            s.power.output,
            r[2]
        );
        let b = sim.map.structure(battery).unwrap().power.stored;
        assert!((b - r[3]).abs() < 1e-3, "{ctx}: battery {b} vs {}", r[3]);
        let h = sim.map.structure(heater).unwrap();
        assert_eq!(h.power.on, r[4] == 1.0, "{ctx}: heater on");
        assert!(
            (h.power.output - r[5]).abs() < 1e-3,
            "{ctx}: heater draw {} vs {}",
            h.power.output,
            r[5]
        );
        let c = sim.map.structure(cooler).unwrap();
        assert!((c.power.output - r[8]).abs() < 1e-3, "{ctx}: cooler draw");
        // The game updated the outdoor room after these rooms here, so they
        // equalize with its value from the interval before; we update it
        // first. The difference stays within a few hundredths of a degree.
        let ta = sim.cell_temperature(room_a);
        let tb = sim.cell_temperature(room_b);
        worst = worst.max((ta - r[6]).abs()).max((tb - r[9]).abs());
        assert!((ta - r[6]).abs() < 0.05, "{ctx}: room A {ta} vs {}", r[6]);
        assert!((tb - r[9]).abs() < 0.05, "{ctx}: room B {tb} vs {}", r[9]);
        checked += 1;
    }
    assert!(checked > 5000, "{checked}");
    eprintln!("heater, cooler and solar: {checked} ticks match; rooms within {worst}");
}

#[test]
fn a_power_switch_splits_its_net() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(30, 30), soil));
    let steel = defs.things.id("Steel");
    let thing = |n: &str| defs.things.id(n).unwrap();
    let generator = sim.debug_spawn_building(thing("WoodFiredGenerator"), None, Cell::new(5, 5));
    sim.debug_set_fuel(Cell::new(5, 5), 75.0);
    for x in 6..=12 {
        if x == 9 {
            continue;
        }
        sim.debug_spawn_building(thing("PowerConduit"), None, Cell::new(x, 5));
    }
    let switch = sim.debug_spawn_building(thing("PowerSwitch"), steel, Cell::new(9, 5));
    // Only the conduits beyond the switch (x 11, 12) reach it.
    let lamp = sim.debug_spawn_building(thing("StandingLamp"), steel, Cell::new(17, 5));
    sim.debug_update_power_nets();
    assert_eq!(sim.power_nets().len(), 1);
    // The lamp is wired to the far side of the switch and starts up.
    for _ in 0..400 {
        sim.tick();
    }
    assert!(sim.map.structure(lamp).unwrap().power.on, "lamp on");
    sim.set_power_switch(switch, false);
    sim.debug_update_power_nets();
    assert_eq!(sim.power_nets().len(), 2, "split at the switch");
    for _ in 0..40 {
        sim.tick();
    }
    assert!(!sim.map.structure(lamp).unwrap().power.on, "lamp shut down");
    assert!(sim.map.structure(generator).unwrap().power.on);
    sim.set_power_switch(switch, true);
    for _ in 0..400 {
        sim.tick();
    }
    assert_eq!(sim.power_nets().len(), 1);
    assert!(sim.map.structure(lamp).unwrap().power.on, "lamp back on");
}

#[test]
fn a_colonist_flicks_a_designated_switch() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(30, 30), soil));
    sim.set_default_update_rate(1);
    let steel = defs.things.id("Steel");
    let thing = |n: &str| defs.things.id(n).unwrap();
    sim.debug_spawn_building(thing("WoodFiredGenerator"), None, Cell::new(5, 5));
    sim.debug_set_fuel(Cell::new(5, 5), 75.0);
    let lamp = sim.debug_spawn_building(thing("StandingLamp"), steel, Cell::new(8, 5));
    sim.debug_update_power_nets();
    for _ in 0..300 {
        sim.tick();
    }
    assert!(sim.map.structure(lamp).unwrap().power.on);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "A", Cell::new(15, 15)).unwrap();
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "BasicWorker" { 1 } else { 0 });
    }
    sim.toggle_switch(lamp);
    assert_eq!(sim.map.flick_designations, vec![lamp]);
    let mut flicking = false;
    for _ in 0..1500 {
        sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.tick();
        flicking |= sim
            .pawn(pawn)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.as_str())
            == Some("Flick");
        if sim.map.flick_designations.is_empty() {
            break;
        }
    }
    assert!(flicking, "a colonist took the flick job");
    let s = sim.map.structure(lamp).unwrap();
    assert!(!s.power.switch_on && !s.power.on, "switched off");
    // Flicking back on: the lamp starts up again with the net.
    sim.toggle_switch(lamp);
    for _ in 0..1500 {
        sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.tick();
    }
    let s = sim.map.structure(lamp).unwrap();
    assert!(s.power.switch_on && s.power.on, "back on");
}

#[test]
fn a_broken_down_generator_is_fixed_with_a_component() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(30, 30), soil));
    sim.set_default_update_rate(1);
    let steel = defs.things.id("Steel");
    let thing = |n: &str| defs.things.id(n).unwrap();
    let generator = sim.debug_spawn_building(thing("WoodFiredGenerator"), None, Cell::new(5, 5));
    sim.debug_set_fuel(Cell::new(5, 5), 75.0);
    let lamp = sim.debug_spawn_building(thing("StandingLamp"), steel, Cell::new(8, 5));
    sim.debug_update_power_nets();
    for _ in 0..300 {
        sim.tick();
    }
    assert!(sim.map.structure(lamp).unwrap().power.on);
    sim.break_down(generator);
    for _ in 0..60 {
        sim.tick();
    }
    let g = sim.map.structure(generator).unwrap();
    assert!(g.power.broken_down && !g.power.on && g.power.output == 0.0);
    assert!(
        !sim.map.structure(lamp).unwrap().power.on,
        "no power, lamp off"
    );
    // A builder with a component fixes it (Construction 10: always works).
    let component = sim.spawn_item(thing("ComponentIndustrial"), Cell::new(15, 15), 3);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let pawn = sim.spawn_pawn(kind, "A", Cell::new(20, 20)).unwrap();
    sim.set_skill(pawn, "Construction", 10);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(pawn, &wt, if wt == "Construction" { 1 } else { 0 });
    }
    let mut fixing = false;
    for _ in 0..3000 {
        sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.tick();
        fixing |= sim
            .pawn(pawn)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.as_str())
            == Some("FixBrokenDownBuilding");
        if !sim.map.structure(generator).unwrap().power.broken_down {
            break;
        }
    }
    assert!(fixing);
    assert!(
        !sim.map.structure(generator).unwrap().power.broken_down,
        "repaired"
    );
    assert_eq!(
        sim.map.item(component).map(|i| i.stack_count),
        Some(2),
        "one component used"
    );
    for _ in 0..600 {
        sim.tick();
    }
    assert!(
        sim.map.structure(generator).unwrap().power.on,
        "running again"
    );
    assert!(sim.map.structure(lamp).unwrap().power.on, "lamp back on");
}

#[test]
fn a_powered_autodoor_opens_faster() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(30, 30), soil));
    let steel = defs.things.id("Steel");
    let thing = |n: &str| defs.things.id(n).unwrap();
    sim.debug_spawn_building(thing("WoodFiredGenerator"), None, Cell::new(5, 5));
    sim.debug_set_fuel(Cell::new(5, 5), 75.0);
    let door_cell = Cell::new(9, 5);
    sim.debug_spawn_building(thing("Autodoor"), steel, door_cell);
    let unpowered = sim.map.door_at(door_cell).unwrap().ticks_to_open;
    sim.debug_update_power_nets();
    for _ in 0..400 {
        sim.tick();
    }
    let d = sim.map.door_at(door_cell).unwrap();
    assert!(d.powered, "the autodoor took power");
    assert!(
        d.ticks_to_open < unpowered,
        "{} vs {unpowered}",
        d.ticks_to_open
    );
    assert_eq!(d.slows_pawns(), d.ticks_to_open > 20);
}
