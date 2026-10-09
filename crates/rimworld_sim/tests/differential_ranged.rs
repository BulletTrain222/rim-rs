//! Drafting and ranged combat replayed against the research run's native
//! traces (local, gitignored: `local/research/traces/ranged/run-04`):
//! the ordered revolver shot's timing (B), an isolated seeded hit's
//! flight and damage (C), undrafting/moving during warmup and cooldown
//! (K) and the job ending when the target goes down (J). The method-level
//! fixtures (C/D/E/F/H bits and draws) are unit tests in `ranged.rs`.
//! Skipped without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::JobKind;
use rimworld_sim::pawn::PawnId;
use rimworld_sim::{Cell, Command, GridSize, Map, NeedKind, Sim};

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

/// The research run's lane: shooter 648 at (85,100) with Shooting 10 and a
/// normal revolver, target 651 at (105,100), both drafted (held in place),
/// update rate 15 (offscreen), no roofs.
struct Range {
    sim: Sim,
    shooter: PawnId,
    target: PawnId,
}

fn range(tick: u64) -> Option<Range> {
    let defs = real_defs()?;
    let concrete = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(250, 250), concrete));
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 21.0;
    sim.debug_set_tick(tick - 1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let shooter = sim.spawn_pawn(kind, "Shooter", Cell::new(85, 100)).unwrap();
    let target = sim.spawn_pawn(kind, "Target", Cell::new(105, 100)).unwrap();
    sim.set_thing_id_number(shooter, 648);
    sim.set_thing_id_number(target, 651);
    for p in [shooter, target] {
        sim.set_update_rate(p, 15);
        sim.set_skill(p, "Shooting", 10);
        assert!(sim.set_drafted(p, true));
    }
    assert!(sim.give_weapon(shooter, "Gun_Revolver"));
    sim.tick();
    Some(Range {
        sim,
        shooter,
        target,
    })
}

impl Range {
    fn tick(&mut self) {
        for p in [self.shooter, self.target] {
            self.sim.debug_set_need(p, NeedKind::Food, 1.0);
            self.sim.debug_set_need(p, NeedKind::Rest, 1.0);
            self.sim.debug_set_need(p, NeedKind::Joy, 1.0);
        }
        self.sim.tick();
    }

    fn until(&mut self, tick: u64) {
        while self.sim.tick_count() < tick {
            self.tick();
        }
    }

    fn job(&self, p: PawnId) -> Option<JobKind> {
        self.sim.pawn(p)?.job.as_ref().map(|j| j.kind)
    }
}

/// Case B: ordered at 130000; the attack starts on the shooter's interval
/// at 130007 (warmup 18), the bullet leaves at 130025, cooldown 96, and
/// the next warmup starts on the interval at 130127 (shot at 130145).
#[test]
fn an_ordered_revolver_shot_has_the_recorded_timing() {
    let Some(mut r) = range(130_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    assert_eq!(r.sim.tick_count(), 130_000);
    r.sim.order_ranged_attack(r.shooter, r.target).unwrap();
    assert!(matches!(
        r.job(r.shooter),
        Some(JobKind::AttackStatic { .. })
    ));
    let mut launches = Vec::new();
    let mut first_warmup = None;
    while r.sim.tick_count() < 130_150 {
        let before = r.sim.projectiles().len();
        r.tick();
        if r.sim.projectiles().len() > before {
            launches.push(r.sim.tick_count());
            if launches.len() == 1 {
                assert_eq!(r.sim.stance_of(r.shooter), Some((false, 96)), "cooldown");
            }
        }
        if first_warmup.is_none()
            && let Some((true, left)) = r.sim.stance_of(r.shooter)
        {
            first_warmup = Some((r.sim.tick_count(), left));
        }
    }
    assert_eq!(first_warmup, Some((130_007, 18)));
    assert_eq!(launches, vec![130_025, 130_145]);
}

/// Case C: the isolated seeded hit (seed 3, projectile id 37122) flies on
/// intervals at 140013/140028/140043 with the recorded positions and hits
/// the target for 12 at 140043.
#[test]
fn a_seeded_bullet_flies_and_hits_as_recorded() {
    let Some(mut r) = range(140_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    r.sim.debug_set_next_thing_id(37122);
    assert!(r.sim.debug_fire_seeded(r.shooter, r.target, 3));
    let p = r.sim.projectiles()[0].clone();
    assert_eq!((p.ticks_to_impact, p.flags), (40, 3));
    let mut seen = Vec::new();
    let mut last = p.ticks_to_impact;
    while !r.sim.projectiles().is_empty() {
        r.tick();
        if let Some(p) = r.sim.projectiles().first()
            && p.ticks_to_impact != last
        {
            last = p.ticks_to_impact;
            let pos = r.sim.projectile_position(p);
            seen.push((
                r.sim.tick_count(),
                p.ticks_to_impact,
                pos.x.to_bits(),
                pos.z.to_bits(),
            ));
        }
    }
    assert_eq!(
        seen,
        vec![
            (140_013, 27, 0x42B7_FFCD, 0x42C8_FA86),
            (140_028, 12, 0x42C7_3B7B, 0x42C8_F41C),
        ]
    );
    assert_eq!(r.sim.tick_count(), 140_043, "impact tick");
    let pain = r.sim.health_view(r.target).unwrap().pain_total();
    assert!(pain > 0.0, "the target was hit");
}

/// Case K: undrafting during warmup removes the warmup; undrafting during
/// cooldown keeps it, and a move order then waits for it to finish before
/// the pawn walks.
#[test]
fn undraft_and_move_keep_the_cooldown() {
    let Some(mut r) = range(240_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    r.sim.order_ranged_attack(r.shooter, r.target).unwrap();
    r.until(240_008);
    assert!(
        matches!(r.sim.stance_of(r.shooter), Some((true, _))),
        "aiming"
    );
    r.sim.set_drafted(r.shooter, false);
    assert_eq!(r.sim.stance_of(r.shooter), None, "warmup cancelled");
    r.sim.set_drafted(r.shooter, true);
    r.sim.order_ranged_attack(r.shooter, r.target).unwrap();
    // Shoot, then undraft during the cooldown.
    while !matches!(r.sim.stance_of(r.shooter), Some((false, _))) {
        r.tick();
    }
    for _ in 0..10 {
        r.tick();
    }
    let (_, left) = r.sim.stance_of(r.shooter).unwrap();
    r.sim.set_drafted(r.shooter, false);
    assert_eq!(
        r.sim.stance_of(r.shooter),
        Some((false, left)),
        "cooldown kept"
    );
    r.sim.set_drafted(r.shooter, true);
    r.sim
        .apply(Command::MoveTo {
            pawn: r.shooter,
            target: Cell::new(85, 96),
        })
        .unwrap();
    assert!(matches!(r.job(r.shooter), Some(JobKind::Goto { .. })));
    for _ in 0..(left - 2) {
        r.tick();
        assert_eq!(r.sim.pawn(r.shooter).unwrap().position, Cell::new(85, 100));
    }
    r.until(r.sim.tick_count() + 200);
    assert_eq!(r.sim.pawn(r.shooter).unwrap().position, Cell::new(85, 96));
}

/// Case J: when the target goes down, the shooter's attack job ends on
/// its next interval and the cooldown stays.
#[test]
fn the_attack_ends_when_the_target_goes_down() {
    let Some(mut r) = range(270_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    r.sim.order_ranged_attack(r.shooter, r.target).unwrap();
    while !matches!(r.sim.stance_of(r.shooter), Some((false, _))) {
        r.tick();
    }
    // Down the target (a large anesthetic dose).
    r.sim.debug_add_hediff(r.target, "Anesthetic", 1.0);
    for _ in 0..16 {
        r.tick();
    }
    assert!(r.sim.pawn(r.target).unwrap().health.downed);
    assert!(!r.sim.is_drafted(r.target), "downed pawns are undrafted");
    assert!(
        !matches!(r.job(r.shooter), Some(JobKind::AttackStatic { .. })),
        "attack ended"
    );
    assert!(
        matches!(r.sim.stance_of(r.shooter), Some((false, _))),
        "cooldown kept"
    );
}

/// The order validator: range, too close, line of sight, not drafted.
#[test]
fn attack_orders_are_validated() {
    let Some(mut r) = range(100_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let defs = r.sim.defs.clone();
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let far = r.sim.spawn_pawn(kind, "Far", Cell::new(112, 100)).unwrap();
    assert_eq!(
        r.sim.order_ranged_attack(r.shooter, far),
        Err("out of range")
    );
    let wall = defs.things.id("Wall").unwrap();
    r.sim
        .debug_spawn_building(wall, defs.things.id("WoodLog"), Cell::new(95, 100));
    assert_eq!(
        r.sim.order_ranged_attack(r.shooter, r.target),
        Err("no line of sight")
    );
    r.sim.set_drafted(r.shooter, false);
    assert_eq!(
        r.sim.order_ranged_attack(r.shooter, r.target),
        Err("not drafted")
    );
}

/// Drafting stops work: a drafted colonist stands ready; undrafted it
/// returns to its normal jobs.
#[test]
fn drafted_pawns_stand_ready_and_return_to_work() {
    let Some(mut r) = range(100_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    assert!(matches!(r.job(r.shooter), Some(JobKind::WaitCombat)));
    for _ in 0..300 {
        r.tick();
    }
    assert!(matches!(r.job(r.shooter), Some(JobKind::WaitCombat)));
    assert_eq!(r.sim.pawn(r.shooter).unwrap().position, Cell::new(85, 100));
    r.sim.set_drafted(r.shooter, false);
    assert!(!matches!(r.job(r.shooter), Some(JobKind::WaitCombat)));
}

/// Save/load in combat: drafted, aiming, cooling down and with a bullet in
/// flight — each reload continues exactly like the running game.
#[test]
fn combat_state_survives_save_and_load() {
    let Some(mut r) = range(130_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let defs = r.sim.defs.clone();
    r.sim.order_ranged_attack(r.shooter, r.target).unwrap();
    let mut checked = Vec::new();
    while checked.len() < 3 && r.sim.tick_count() < 131_000 {
        r.tick();
        let label = match (r.sim.stance_of(r.shooter), r.sim.projectiles().is_empty()) {
            (Some((true, 5..=12)), _) => "warmup",
            (Some((false, 61..=95)), false) => "cooldown with a bullet in flight",
            (Some((false, 1..=40)), true) => "cooldown",
            _ => continue,
        };
        if checked.contains(&label) {
            continue;
        }
        let data = r.sim.save();
        let mut copy = Sim::load(defs.clone(), &data).expect("loads");
        assert_eq!(copy.save(), data, "{label}: re-save");
        r.sim.forget_unsaved_state();
        for _ in 0..30 {
            for s in [&mut r.sim, &mut copy] {
                for p in [r.shooter, r.target] {
                    s.debug_set_need(p, NeedKind::Food, 1.0);
                    s.debug_set_need(p, NeedKind::Rest, 1.0);
                    s.debug_set_need(p, NeedKind::Joy, 1.0);
                }
                s.tick();
            }
        }
        assert_eq!(r.sim.save(), copy.save(), "{label}: diverged after reload");
        assert!(copy.is_drafted(r.shooter));
        checked.push(label);
    }
    assert_eq!(checked.len(), 3, "{checked:?}");
}

/// A whole fight with ordinary gunfire: the shooter keeps shooting a
/// hostile target until it goes down, then stands ready again.
#[test]
fn a_fight_ends_with_the_target_down() {
    let Some(mut r) = range(100_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let defs = r.sim.defs.clone();
    let drifter = defs.pawn_kinds.id("Drifter").unwrap();
    let enemy = r
        .sim
        .spawn_pawn(drifter, "Enemy", Cell::new(97, 104))
        .unwrap();
    r.sim.order_ranged_attack(r.shooter, enemy).unwrap();
    let mut shots = 0;
    for _ in 0..30_000 {
        let before = r.sim.projectiles().len();
        r.tick();
        shots += r.sim.projectiles().len().saturating_sub(before);
        let e = r.sim.pawn(enemy).unwrap();
        if e.health.downed || e.health.dead {
            break;
        }
        // The enemy wanders; reorder when the job ended out of range.
        if !matches!(r.job(r.shooter), Some(JobKind::AttackStatic { .. })) {
            let _ = r.sim.order_ranged_attack(r.shooter, enemy);
        }
    }
    let e = r.sim.pawn(enemy).unwrap();
    assert!(e.health.downed || e.health.dead, "after {shots} shots");
    for _ in 0..40 {
        r.tick();
    }
    assert!(matches!(r.job(r.shooter), Some(JobKind::WaitCombat)));
    eprintln!("downed after {shots} shots");
}

/// Equipping: a colonist walks to a gun and takes it, dropping the one it
/// held; a downed pawn drops its weapon.
#[test]
fn colonists_equip_guns_and_drop_them_when_downed() {
    let Some(mut r) = range(100_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let defs = r.sim.defs.clone();
    let rifle = defs.things.id("Gun_BoltActionRifle").unwrap();
    let revolver = defs.things.id("Gun_Revolver").unwrap();
    r.sim.set_drafted(r.shooter, false);
    let item = r.sim.spawn_item(rifle, Cell::new(90, 100), 1);
    r.sim.order_equip(r.shooter, item).unwrap();
    for _ in 0..600 {
        r.tick();
    }
    let p = r.sim.pawn(r.shooter).unwrap();
    assert_eq!(p.equipment.as_ref().map(|e| e.def), Some(rifle));
    assert!(
        r.sim.map.items().iter().any(|i| i.def == revolver),
        "old gun dropped"
    );
    assert!(
        !r.sim.map.items().iter().any(|i| i.def == rifle),
        "rifle taken"
    );
    r.sim.debug_add_hediff(r.shooter, "Anesthetic", 1.0);
    for _ in 0..40 {
        r.tick();
    }
    assert!(r.sim.pawn(r.shooter).unwrap().equipment.is_none());
    assert!(
        r.sim.map.items().iter().any(|i| i.def == rifle),
        "dropped when downed"
    );
}
