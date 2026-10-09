//! Health with the real game data: the Human body, injuries on parts,
//! part destruction, pain shock, bleeding out and healing. Skipped
//! without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
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

fn one_colonist(defs: Arc<GameDefs>, seed: u64) -> (Sim, rimworld_sim::PawnId) {
    let soil = defs.terrain.id("Soil").unwrap();
    let mut sim = Sim::with_seed(defs.clone(), Map::new(GridSize::new(20, 20), soil), seed);
    sim.set_default_update_rate(1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let a = sim.spawn_pawn(kind, "A", Cell::new(10, 10)).unwrap();
    (sim, a)
}

fn part_named(defs: &GameDefs, body: &str, def: &str) -> usize {
    defs.bodies
        .get(body)
        .unwrap()
        .parts
        .iter()
        .position(|p| p.def == def)
        .unwrap_or_else(|| panic!("no {def}"))
}

#[test]
fn the_human_body_is_read_with_coverage() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let body = defs.bodies.get("Human").unwrap();
    assert_eq!(body.parts[0].def, "Torso");
    assert!(body.parts.len() > 40, "{}", body.parts.len());
    // Coverage of every part's own share adds up to the whole body.
    let total: f32 = body.parts.iter().map(|p| p.coverage_abs).sum();
    assert!((total - 1.0).abs() < 1e-3, "{total}");
    let torso = defs.body_parts.get("Torso").unwrap();
    assert_eq!(torso.hit_points, 40);
    let cut = defs.hediffs.get("Cut").unwrap();
    assert_eq!(cut.injury.as_ref().unwrap().bleed_rate, 0.06);
    assert!(defs.capacities.get("Consciousness").unwrap().lethal_flesh);
    assert_eq!(defs.capacities.get("Moving").unwrap().min_for_capable, 0.15);
}

#[test]
fn injuries_hurt_and_destroy_parts() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 1);
    {
        let v = sim.health_view(a).unwrap();
        assert_eq!(v.pain_total(), 0.0);
        assert_eq!(v.capacity("Consciousness"), 1.0);
        assert_eq!(v.capacity("Moving"), 1.0);
    }
    // A cut on the left leg.
    let leg = part_named(&defs, "Human", "Leg");
    let r = sim.damage_pawn(a, "Cut", 10.0, Some(leg)).unwrap();
    assert_eq!(r.parts_hit, vec![leg]);
    {
        let v = sim.health_view(a).unwrap();
        // 10 × 0.0125 pain per severity.
        assert!((v.pain_total() - 0.125).abs() < 1e-5);
        assert!(v.part_health(leg) < v.max_health(leg));
        assert!(v.bleed_rate_total() > 0.0);
        assert!(v.capacity("Moving") < 1.0);
    }
    // Destroying the leg: walking fails (one of two legs left: 50%).
    sim.damage_pawn(a, "Cut", 100.0, Some(leg));
    // The overkill roll may have spared it; hit it until it is gone.
    for _ in 0..20 {
        if sim.health_view(a).unwrap().missing(leg) {
            break;
        }
        sim.damage_pawn(a, "Cut", 100.0, Some(leg));
    }
    let v = sim.health_view(a).unwrap();
    assert!(v.missing(leg), "leg destroyed");
    let foot = part_named(&defs, "Human", "Foot");
    assert!(v.missing(foot) || defs.bodies.get("Human").unwrap().parts[foot].parent != Some(leg));
    eprintln!(
        "pain {:.3} moving {:.2} consciousness {:.2}",
        v.pain_total(),
        v.capacity("Moving"),
        v.capacity("Consciousness")
    );
}

#[test]
fn heavy_damage_downs_and_bleeding_can_kill() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 2);
    // Many cuts on the limbs: pain shock downs the pawn. (Five such cuts
    // on the torso would destroy it, which kills.)
    let body = defs.bodies.get("Human").unwrap();
    let limbs: Vec<usize> = (0..body.parts.len())
        .filter(|&i| ["Leg", "Arm", "Shoulder"].contains(&body.parts[i].def.as_str()))
        .collect();
    for round in 0..4 {
        for &l in &limbs {
            if !sim.pawn(a).unwrap().health.downed {
                sim.damage_pawn(a, "Cut", 6.0 + round as f32, Some(l));
            }
        }
    }
    let p = sim.pawn(a).unwrap();
    let v = sim.health_view(a).unwrap();
    eprintln!(
        "pain {:.2} bleed {:.2}",
        v.pain_total(),
        v.bleed_rate_total()
    );
    for h in &p.health.hediffs {
        eprintln!(
            "  {} part {:?} sev {}",
            defs.hediffs[h.def].def_name, h.part, h.severity
        );
    }
    assert!(p.health.downed, "downed by pain shock");
    assert!(!p.health.dead);
    assert!(p.job.is_none());
    // Untended, the bleeding builds blood loss until the pawn dies.
    let mut died_at = None;
    for _ in 0..60_000 {
        sim.tick();
        if sim.pawn(a).unwrap().health.dead {
            died_at = Some(sim.tick_count());
            break;
        }
    }
    let died = died_at.expect("bled to death");
    eprintln!("died at tick {died}");
    // Blood loss ×0.001 per bleed per 60 ticks: well under a day here.
    assert!(died > 1000);
}

#[test]
fn small_wounds_heal_over_time() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 3);
    // Food for the three days, or it would starve.
    let meal = defs.things.id("MealSurvivalPack").unwrap();
    sim.spawn_item(meal, Cell::new(12, 10), 10);
    let arm = part_named(&defs, "Human", "Shoulder");
    sim.damage_pawn(a, "Blunt", 3.0, Some(arm));
    assert!(!sim.pawn(a).unwrap().health.hediffs.is_empty());
    for _ in 0..3 * 60_000 {
        sim.tick();
    }
    let p = sim.pawn(a).unwrap();
    assert!(!p.health.dead && !p.health.downed);
    assert!(
        p.health
            .hediffs
            .iter()
            .all(|h| defs.hediffs[h.def].def_name == "BloodLoss" || h.severity < 3.0),
        "{:?}",
        p.health.hediffs
    );
    // 8 per day × 1% × 1 health scale, every 600 ticks: 3 days heal ~2.4.
    let injury: f32 = p
        .health
        .hediffs
        .iter()
        .filter(|h| defs.hediffs[h.def].is_injury())
        .map(|h| h.severity)
        .sum();
    assert!(injury < 1.0, "{injury}");
}

#[test]
fn melee_stats_follow_the_curves() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    use rimworld_sim::stats::{Skills, pawn_stat};
    let human = &defs.things[defs.things.id("Human").unwrap()];
    let mut s = Skills::default();
    // MeleeHitChance: skill level offset, then the post-process curve.
    assert!((pawn_stat(&defs, human, &s, "MeleeHitChance") - 0.5).abs() < 1e-5);
    s.set("Melee", 10);
    assert!((pawn_stat(&defs, human, &s, "MeleeHitChance") - 0.8).abs() < 1e-5);
    s.set("Melee", 20);
    assert!((pawn_stat(&defs, human, &s, "MeleeHitChance") - 0.9).abs() < 1e-5);
    // MeleeDodgeChance: 0 until level 5, 0.3 at 20.
    assert!((pawn_stat(&defs, human, &s, "MeleeDodgeChance") - 0.3).abs() < 1e-5);
    let verbs = rimworld_sim::combat::melee_verbs(&defs, human);
    assert!(
        verbs
            .iter()
            .any(|v| v.damage == "Blunt" && (v.power - 8.2).abs() < 1e-5)
    );
    assert!(verbs.iter().any(|v| v.damage == "Bite"));
}

#[test]
fn a_melee_fight_ends_with_the_target_downed() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 7);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let b = sim.spawn_pawn(kind, "B", Cell::new(14, 10)).unwrap();
    sim.set_skill(a, "Melee", 12);
    assert!(sim.order_melee_attack(a, b));
    let (mut hits, mut misses, mut dodges) = (0, 0, 0);
    let mut kinds = std::collections::BTreeSet::new();
    for _ in 0..20_000 {
        sim.tick();
        for (_, swing) in sim.take_swings() {
            match swing {
                rimworld_sim::sim::Swing::Hit { damage, .. } => {
                    hits += 1;
                    kinds.insert(damage);
                }
                rimworld_sim::sim::Swing::Miss => misses += 1,
                rimworld_sim::sim::Swing::Dodged => dodges += 1,
            }
        }
        if sim.pawn(b).unwrap().health.downed || sim.pawn(b).unwrap().health.dead {
            break;
        }
    }
    eprintln!(
        "hits {hits} misses {misses} dodges {dodges} kinds {kinds:?} at {}",
        sim.tick_count()
    );
    let target = sim.pawn(b).unwrap().clone();
    assert!(target.health.downed || target.health.dead);
    // ~82% hit chance at Melee 12: misses are possible but not certain.
    assert!(hits > 3, "hits {hits} misses {misses}");
    assert!(kinds.contains("Blunt"));
    // The attacker stops once the target is down.
    for _ in 0..300 {
        sim.tick();
    }
    assert!(!matches!(
        sim.pawn(a).unwrap().job.as_ref().map(|j| j.kind),
        Some(rimworld_sim::JobKind::AttackMelee { .. })
    ));
    // Every injury is on an outside part.
    let body = defs.bodies.get("Human").unwrap();
    for h in &target.health.hediffs {
        if let Some(p) = h.part
            && defs.hediffs[h.def].is_injury()
        {
            assert_eq!(
                body.parts[p].depth,
                rimworld_defs::health::PartDepth::Outside,
                "{}",
                body.parts[p].def
            );
        }
    }
}

#[test]
fn bleeding_pawns_drop_blood() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 5);
    let leg = part_named(&defs, "Human", "Leg");
    sim.damage_pawn(a, "Cut", 12.0, Some(leg));
    let blood = defs.things.id("Filth_Blood").unwrap();
    for _ in 0..5000 {
        sim.tick();
    }
    // Bleed rate ~0.7 standing: ~0.3% a tick, so a dozen drops or so.
    let drops: u32 = sim
        .map
        .items()
        .iter()
        .filter(|i| i.def == blood)
        .map(|i| i.thickness)
        .sum();
    assert!((3..60).contains(&drops), "{drops}");
}

#[test]
fn starving_brings_malnutrition_and_finally_death() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, a) = one_colonist(defs.clone(), 9);
    sim.debug_set_need(a, NeedKind::Food, 0.0);
    let malnutrition = defs.hediffs.id("Malnutrition").unwrap();
    let severity = |sim: &Sim| {
        sim.pawn(a)
            .unwrap()
            .health
            .hediffs
            .iter()
            .find(|h| h.def == malnutrition)
            .map_or(0.0, |h| h.severity)
    };
    // 0.453 a day (×0.8–1.2 per pawn) while starving.
    for _ in 0..60_000 {
        sim.tick();
    }
    let day1 = severity(&sim);
    assert!((0.36..=0.55).contains(&day1), "{day1}");
    // It lowers consciousness.
    let consciousness = sim.health_view(a).unwrap().capacity("Consciousness");
    assert!(consciousness < 1.0, "{consciousness}");
    // Severity 1 kills, within about three days of starving.
    let mut died = None;
    for t in 0..3 * 60_000 {
        sim.tick();
        if sim.pawn(a).unwrap().health.dead {
            died = Some(t);
            break;
        }
    }
    assert!(died.is_some(), "still alive at severity {}", severity(&sim));
}

#[test]
fn freezing_brings_hypothermia_frostbite_and_death() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    // Frostbite is a 1-2.5% roll per minute past severity 0.37; this seed
    // gets some.
    let (mut sim, pawn) = one_colonist(defs.clone(), 11);
    let hypo = defs.hediffs.id("Hypothermia").unwrap();
    let frostbite = defs.hediffs.id("Frostbite").unwrap();
    sim.outdoor_temperature = -40.0;
    sim.debug_set_room_temperatures(-40.0);
    let mut frostbitten = false;
    let mut downed_at = None;
    for _ in 0..60_000 {
        sim.outdoor_temperature = -40.0;
        sim.debug_set_need(pawn, NeedKind::Food, 1.0);
        sim.debug_set_need(pawn, NeedKind::Rest, 1.0);
        sim.tick();
        let p = sim.pawn(pawn).unwrap();
        frostbitten |= p.health.hediffs.iter().any(|h| h.def == frostbite);
        if p.health.downed && downed_at.is_none() {
            downed_at = Some(sim.tick_count());
            // Downed at the 0.62 stage: consciousness capped at 0.1.
            let s = p
                .health
                .hediffs
                .iter()
                .find(|h| h.def == hypo)
                .unwrap()
                .severity;
            assert!(s >= 0.62, "{s}");
        }
        if p.health.dead {
            break;
        }
    }
    assert!(downed_at.is_some());
    assert!(frostbitten, "no frostbite in a long freeze");
    assert!(
        sim.pawn(pawn).unwrap().health.dead,
        "hypothermia is lethal at 1"
    );
}

#[test]
fn a_warm_room_cures_hypothermia() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, pawn) = one_colonist(defs.clone(), 3);
    let hypo = defs.hediffs.id("Hypothermia").unwrap();
    let sev = |sim: &Sim| {
        sim.pawn(pawn)
            .unwrap()
            .health
            .hediffs
            .iter()
            .find(|h| h.def == hypo)
            .map_or(0.0, |h| h.severity)
    };
    for _ in 0..3_000 {
        sim.outdoor_temperature = -20.0;
        sim.tick();
    }
    let cold = sev(&sim);
    assert!(cold > 0.05, "{cold}");
    for _ in 0..6_000 {
        sim.outdoor_temperature = 20.0;
        sim.tick();
    }
    assert_eq!(sev(&sim), 0.0, "recovered and removed");
}

#[test]
fn a_seriously_hypothermic_colonist_seeks_a_warm_room() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, pawn) = one_colonist(defs.clone(), 5);
    // A closed, roofed room east of the colonist, kept at 20 °C.
    let wall = defs.things.id("Wall").unwrap();
    let wood = defs.things.id("WoodLog");
    for x in 13..=17 {
        for z in 8..=12 {
            if (x == 13 || x == 17 || z == 8 || z == 12) && (x, z) != (13, 10) {
                sim.debug_spawn_building(wall, wood, Cell::new(x, z));
            }
            sim.map.set_roof(Cell::new(x, z), true);
        }
    }
    let door = defs.things.id("Door").unwrap();
    sim.debug_spawn_building(door, wood, Cell::new(13, 10));
    sim.debug_add_hediff(pawn, "Hypothermia", 0.4);
    let inside = Cell::new(15, 10);
    let job = |sim: &Sim| {
        sim.pawn(pawn)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.clone())
    };
    let warm = |sim: &mut Sim| {
        sim.outdoor_temperature = -20.0;
        sim.debug_set_room_temperatures(-20.0);
        let room = sim.room_at(inside).unwrap();
        sim.debug_set_room_temperature(room, 20.0);
    };
    warm(&mut sim);
    sim.debug_find_and_start_job(pawn);
    assert_eq!(job(&sim).as_deref(), Some("GotoSafeTemperature"));
    for _ in 0..600 {
        warm(&mut sim);
        sim.tick();
        if job(&sim).as_deref() == Some("Wait_SafeTemperature") {
            break;
        }
    }
    let p = sim.pawn(pawn).unwrap();
    assert_eq!(job(&sim).as_deref(), Some("Wait_SafeTemperature"));
    assert_eq!(sim.room_at(p.position), sim.room_at(inside));
}

#[test]
fn starting_colonists_dress_for_the_season() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let names = |sim: &Sim, p| -> Vec<String> {
        sim.pawn(p)
            .unwrap()
            .apparel
            .iter()
            .map(|a| defs.things[a.def].def_name.clone())
            .collect()
    };
    // A temperate tile in spring: shirt and pants only.
    let (mut sim, pawn) = one_colonist(defs.clone(), 1);
    sim.climate = Some(rimworld_sim::climate::Climate {
        tile_temperature: 20.0,
    });
    sim.latitude = 35.0;
    sim.give_starting_apparel(pawn);
    assert_eq!(names(&sim, pawn), ["Apparel_BasicShirt", "Apparel_Pants"]);
    let min = sim.pawn_stat(pawn, "ComfyTemperatureMin").unwrap();
    assert!((min - 8.44).abs() < 1e-3, "{min}");
    // A frozen tile: a free parka and hat on top.
    let (mut sim, pawn) = one_colonist(defs.clone(), 1);
    sim.climate = Some(rimworld_sim::climate::Climate {
        tile_temperature: -25.0,
    });
    sim.latitude = 70.0;
    sim.give_starting_apparel(pawn);
    assert_eq!(
        names(&sim, pawn),
        [
            "Apparel_BasicShirt",
            "Apparel_Pants",
            "Apparel_Parka",
            "Apparel_Tuque"
        ]
    );
}

#[test]
fn a_downed_colonist_is_carried_to_a_bed() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, patient) = one_colonist(defs.clone(), 2);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let doctor = sim.spawn_pawn(kind, "Doc", Cell::new(3, 3)).unwrap();
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 1 } else { 0 });
    }
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    let bed_id = sim.debug_spawn_building(bed, wood, Cell::new(16, 4));
    sim.debug_add_hediff(patient, "Anesthetic", 1.0);
    assert!(sim.pawn(patient).unwrap().health.downed);
    let job = |sim: &Sim, p| {
        sim.pawn(p)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.clone())
    };
    sim.debug_find_and_start_job(doctor);
    assert_eq!(job(&sim, doctor).as_deref(), Some("Rescue"));
    let mut carried = false;
    for _ in 0..3_000 {
        sim.debug_set_need(doctor, NeedKind::Rest, 1.0);
        sim.tick();
        carried |= sim.pawn(patient).unwrap().carried_by == Some(doctor);
        if job(&sim, patient).as_deref() == Some("LayDown") {
            break;
        }
    }
    assert!(carried, "the doctor carried the patient");
    let p = sim.pawn(patient).unwrap();
    assert_eq!(job(&sim, patient).as_deref(), Some("LayDown"));
    let fp = sim.map.structure(bed_id).unwrap().footprint;
    assert_eq!(p.position, fp.sleeping_slot(0));
    assert_eq!(p.owned_bed, Some(bed_id));
    assert!(p.carried_by.is_none());
    assert!(sim.pawn(doctor).unwrap().carried_pawn.is_none());
}

#[test]
fn a_doctor_feeds_a_bedridden_colonist() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, patient) = one_colonist(defs.clone(), 3);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let doctor = sim.spawn_pawn(kind, "Doc", Cell::new(3, 3)).unwrap();
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 1 } else { 0 });
    }
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    sim.debug_spawn_building(bed, wood, Cell::new(16, 4));
    let meal = defs.things.id("MealSurvivalPack").unwrap();
    sim.spawn_item(meal, Cell::new(5, 15), 4);
    sim.debug_add_hediff(patient, "Anesthetic", 1.0);
    let food_level = |sim: &Sim| {
        sim.pawn(patient)
            .unwrap()
            .needs
            .get(NeedKind::Food)
            .unwrap()
            .percent()
    };
    let job = |sim: &Sim, p| {
        sim.pawn(p)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.clone())
    };
    let mut fed = false;
    for _ in 0..6_000 {
        // Keep the patient hungry until the doctor has them in bed.
        if !fed && job(&sim, patient).as_deref() != Some("LayDown") {
            sim.debug_set_need(patient, NeedKind::Food, 0.1);
        }
        sim.debug_set_need(doctor, NeedKind::Food, 1.0);
        sim.debug_set_need(doctor, NeedKind::Rest, 1.0);
        sim.tick();
        fed |= job(&sim, doctor).as_deref() == Some("FeedPatient");
        if fed && food_level(&sim) > 0.5 {
            break;
        }
    }
    assert!(fed, "the doctor took a feeding job");
    assert!(food_level(&sim) > 0.5, "{}", food_level(&sim));
}

#[test]
fn armor_stops_or_halves_some_hits() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let torso = |defs: &GameDefs| part_named(defs, "Human", "Torso");
    let mut taken = [0.0f32; 2];
    let mut deflected = [0u32; 2];
    for (k, vest) in [false, true].into_iter().enumerate() {
        for seed in 0..200 {
            let (mut sim, pawn) = one_colonist(defs.clone(), seed);
            if vest {
                sim.wear_apparel(pawn, defs.things.id("Apparel_FlakVest").unwrap(), None);
            }
            let r = sim
                .damage_pawn_full(pawn, "Cut", 6.0, 0.0, Some(torso(&defs)), None)
                .unwrap();
            taken[k] += r.total_damage;
            deflected[k] += u32::from(r.deflected);
        }
    }
    eprintln!("taken {taken:?}, deflected {deflected:?}");
    assert_eq!(deflected[0], 0, "no armor, nothing deflected");
    assert!(deflected[1] > 20, "a flak vest deflects some cuts");
    assert!(taken[1] < taken[0] * 0.8, "and lowers the damage taken");
}

#[test]
fn an_injured_colonist_goes_to_bed_and_is_tended() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, patient) = one_colonist(defs.clone(), 4);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let doctor = sim.spawn_pawn(kind, "Doc", Cell::new(3, 3)).unwrap();
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(doctor, &wt, if wt == "Doctor" { 1 } else { 0 });
        let patient_work = wt == "Patient" || wt == "PatientBedRest";
        sim.set_work_priority(patient, &wt, if patient_work { 1 } else { 0 });
    }
    let bed = defs.things.id("Bed").unwrap();
    let wood = defs.things.id("WoodLog");
    let bed_id = sim.debug_spawn_building(bed, wood, Cell::new(16, 4));
    sim.debug_add_injury(patient, "Cut", "Torso", 6.0);
    sim.debug_add_injury(patient, "Cut", "Leg", 3.0);
    let bleeding = |sim: &Sim| sim.health_view(patient).unwrap().bleed_rate_total();
    assert!(bleeding(&sim) > 0.5);
    let job = |sim: &Sim, p| {
        sim.pawn(p)
            .unwrap()
            .job
            .as_ref()
            .and_then(|j| j.def)
            .map(|d| defs.jobs[d].def_name.clone())
    };
    sim.debug_find_and_start_job(patient);
    assert_eq!(job(&sim, patient).as_deref(), Some("LayDown"));
    let mut tended_by_doctor = false;
    for _ in 0..8_000 {
        for p in [doctor, patient] {
            sim.debug_set_need(p, NeedKind::Rest, 1.0);
            sim.debug_set_need(p, NeedKind::Food, 1.0);
        }
        sim.tick();
        tended_by_doctor |= job(&sim, doctor).as_deref() == Some("TendPatient");
        if bleeding(&sim) == 0.0 {
            break;
        }
    }
    assert!(tended_by_doctor);
    assert_eq!(bleeding(&sim), 0.0, "both cuts are bandaged");
    let p = sim.pawn(patient).unwrap();
    let fp = sim.map.structure(bed_id).unwrap().footprint;
    assert_eq!(p.position, fp.sleeping_slot(0));
    // Tended and still healing: the patient stays in bed to recuperate.
    for _ in 0..1_000 {
        sim.debug_set_need(patient, NeedKind::Rest, 1.0);
        sim.debug_set_need(patient, NeedKind::Food, 1.0);
        sim.tick();
    }
    assert_eq!(job(&sim, patient).as_deref(), Some("LayDown"));
    assert_eq!(sim.pawn(patient).unwrap().position, fp.sleeping_slot(0));
}

#[test]
fn the_dead_leave_corpses_that_rot_and_are_hauled_to_a_dump() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, dead) = one_colonist(defs.clone(), 5);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let hauler = sim.spawn_pawn(kind, "Hauler", Cell::new(3, 3)).unwrap();
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(hauler, &wt, if wt == "Hauling" { 1 } else { 0 });
    }
    sim.debug_add_hediff(dead, "BloodLoss", 1.0);
    let corpse = sim.corpse_of(dead).expect("a corpse");
    assert_eq!(sim.map.item(corpse).unwrap().position, Cell::new(10, 10));
    assert_eq!(
        sim.rot_stage(corpse),
        Some(rimworld_sim::sim::RotStage::Fresh)
    );
    // A default stockpile does not take it.
    use rimworld_sim::storage::{StoragePreset, ThingFilter};
    let pile = sim.map.storage.add_stockpile(
        Default::default(),
        &[Cell::new(16, 16)],
        ThingFilter::preset(&defs, StoragePreset::DefaultStockpile),
    );
    for _ in 0..600 {
        sim.debug_set_need(hauler, NeedKind::Rest, 1.0);
        sim.debug_set_need(hauler, NeedKind::Food, 1.0);
        sim.tick();
    }
    assert_eq!(sim.map.item(corpse).unwrap().position, Cell::new(10, 10));
    // Dead outside the home area: the corpse is forbidden, so even a
    // dumping stockpile waits until the player allows it.
    assert!(sim.map.item(corpse).unwrap().forbidden);
    sim.map.storage.set_filter(
        pile,
        ThingFilter::preset(&defs, StoragePreset::DumpingStockpile),
    );
    for _ in 0..300 {
        sim.tick();
    }
    assert_eq!(sim.map.item(corpse).unwrap().position, Cell::new(10, 10));
    let defs2 = defs.clone();
    sim.map.set_forbidden(&defs2, corpse, false);
    // Allowed, it is hauled, and the dead pawn goes with its corpse.
    let mut carried = false;
    for _ in 0..3_000 {
        sim.debug_set_need(hauler, NeedKind::Rest, 1.0);
        sim.debug_set_need(hauler, NeedKind::Food, 1.0);
        sim.tick();
        carried |= sim
            .pawn(hauler)
            .unwrap()
            .carried
            .is_some_and(|c| c.id == corpse);
        if sim
            .map
            .item(corpse)
            .is_some_and(|i| i.position == Cell::new(16, 16))
        {
            break;
        }
    }
    assert!(carried, "the hauler carried the corpse");
    assert_eq!(sim.map.item(corpse).unwrap().position, Cell::new(16, 16));
    assert_eq!(sim.pawn(dead).unwrap().position, Cell::new(16, 16));
    assert_eq!(sim.corpse_pawn(corpse), Some(dead));
}

#[test]
fn rotting_corpses_lose_hit_points_each_rot_day() {
    let Some(defs) = real_defs() else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let (mut sim, dead) = one_colonist(defs.clone(), 6);
    sim.debug_add_hediff(dead, "BloodLoss", 1.0);
    let corpse = sim.corpse_of(dead).unwrap();
    let set_rot = |sim: &mut Sim, rot: f32| {
        let item = sim
            .map
            .items_mut()
            .iter_mut()
            .find(|i| i.id == corpse)
            .unwrap();
        item.rot = rot;
    };
    let hp = |sim: &Sim| sim.map.item(corpse).unwrap().hit_points.unwrap_or(100);
    // Crossing into rot day 3 while rotting: 2 damage.
    set_rot(&mut sim, 3.0 * 60_000.0 - 200.0);
    for _ in 0..500 {
        sim.tick();
    }
    assert!(
        sim.map.item(corpse).unwrap().rot >= 180_000.0,
        "warm enough to rot"
    );
    assert_eq!(hp(&sim), 98);
    assert_eq!(
        sim.rot_stage(corpse),
        Some(rimworld_sim::sim::RotStage::Rotting)
    );
    // Crossing into day 5, now dessicated: RoundRandom(0.7).
    set_rot(&mut sim, 5.0 * 60_000.0 - 200.0);
    for _ in 0..500 {
        sim.tick();
    }
    assert_eq!(
        sim.rot_stage(corpse),
        Some(rimworld_sim::sim::RotStage::Dessicated)
    );
    assert!((97..=98).contains(&hp(&sim)), "{}", hp(&sim));
}
