//! Hunting against the research run's recorded fixtures (an unpublished
//! research report, traces kept locally): the
//! hunting work picks up a marked Hare, shoots from where it stands
//! (warmup 18), chooses the recorded cast position from 60 cells, executes
//! downed prey after the 180-tick wait, fails when the mark is removed, and
//! a Muffalo can turn manhunter and attack. Skipped without an install.

use std::path::Path;
use std::sync::Arc;

use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};
use rimworld_defs::{GameDefs, PackSource, load_packs};
use rimworld_sim::job::{HuntStage, JobKind};
use rimworld_sim::pawn::PawnId;
use rimworld_sim::storage::StoragePreset;
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

/// The research fixture: a flat concrete field, a hunter at (85,100) with
/// a revolver and only Hunting work, prey to the east.
struct Field {
    sim: Sim,
    hunter: PawnId,
    prey: PawnId,
}

fn field(prey_kind: &str, prey_at: Cell, tick: u64) -> Option<Field> {
    let defs = real_defs()?;
    let concrete = defs.terrain.id("Concrete").unwrap();
    let mut sim = Sim::new(defs.clone(), Map::new(GridSize::new(250, 250), concrete));
    sim.debug_set_sky_glow(Some(1.0));
    sim.outdoor_temperature = 21.0;
    sim.debug_set_tick(tick - 1);
    let kind = defs.pawn_kinds.id("Colonist").unwrap();
    let hunter = sim.spawn_pawn(kind, "Hunter", Cell::new(85, 100)).unwrap();
    sim.set_update_rate(hunter, 15);
    sim.set_skill(hunter, "Shooting", 10);
    for wt in defs.work_types.iter().map(|(_, w)| w.def_name.clone()) {
        sim.set_work_priority(hunter, &wt, if wt == "Hunting" { 1 } else { 0 });
    }
    assert!(sim.give_weapon(hunter, "Gun_Revolver"));
    let prey = sim.spawn_animal(prey_kind, prey_at).unwrap();
    Some(Field { sim, hunter, prey })
}

impl Field {
    fn tick(&mut self) {
        for p in [self.hunter, self.prey] {
            self.sim.debug_set_need(p, NeedKind::Food, 1.0);
            self.sim.debug_set_need(p, NeedKind::Rest, 1.0);
            self.sim.debug_set_need(p, NeedKind::Joy, 1.0);
        }
        self.sim.tick();
    }

    fn job(&self, p: PawnId) -> Option<JobKind> {
        self.sim.pawn(p)?.job.as_ref().map(|j| j.kind)
    }

    fn mark(&mut self) {
        let at = self.sim.pawn(self.prey).unwrap().position;
        assert_eq!(self.sim.designate_hunt(&[at]), 1);
    }
}

/// Fixtures D/E: 23 cells away the hunter's own cell is the cast position;
/// the Hunt job reserves the Hare, the first warmup is 18 ticks and the
/// bullet leaves without the hunter moving; the hunt ends with the Hare
/// dead, its corpse unforbidden and the mark gone.
#[test]
fn a_marked_hare_is_hunted_from_where_the_hunter_stands() {
    let Some(mut f) = field("Hare", Cell::new(108, 100), 160_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    f.mark();
    // The hunter thinks on its next interval.
    let mut started = None;
    let mut first_shot = None;
    let mut fled = false;
    for _ in 0..20_000 {
        let before = f.sim.projectiles().len();
        f.tick();
        let t = f.sim.tick_count();
        if started.is_none() && matches!(f.job(f.hunter), Some(JobKind::Hunt { .. })) {
            started = Some(t);
        }
        if first_shot.is_none() && f.sim.projectiles().len() > before {
            first_shot = Some(t);
            assert_eq!(f.sim.pawn(f.hunter).unwrap().position, Cell::new(85, 100));
        }
        if matches!(f.job(f.prey), Some(JobKind::Flee { .. })) {
            fled = true;
        }
        if f.sim.pawn(f.prey).unwrap().health.dead {
            break;
        }
    }
    let (s, shot) = (started.unwrap(), first_shot.unwrap());
    assert_eq!(shot - s, 18, "warmup from the job start");
    assert!(f.sim.pawn(f.prey).unwrap().health.dead, "the hare died");
    assert!(!f.sim.hunt_designated(f.prey), "mark removed on death");
    let corpse = f.sim.corpse_of(f.prey).expect("corpse");
    assert!(
        !f.sim.map.item(corpse).unwrap().forbidden,
        "hunted corpse not forbidden"
    );
    // Fixture E: the cast toil waits out the cooldown (last shot + 96),
    // then collection finishes at once without storage.
    for _ in 0..120 {
        f.tick();
    }
    assert!(!matches!(f.job(f.hunter), Some(JobKind::Hunt { .. })));
    eprintln!(
        "hunt: started {s}, first shot {shot}, hare fled: {fled}, died {}",
        f.sim.tick_count()
    );
}

/// Fixture H: 60 cells away the hunter walks to (122,100) first.
#[test]
fn a_far_hare_is_approached_to_the_recorded_cell() {
    let Some(mut f) = field("Hare", Cell::new(145, 100), 200_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    f.mark();
    for _ in 0..40 {
        f.tick();
        if let Some(JobKind::Hunt { stage, .. }) = f.job(f.hunter) {
            assert_eq!(
                stage,
                HuntStage::GotoCast {
                    cell: Cell::new(122, 100)
                }
            );
            return;
        }
    }
    panic!("no hunt started");
}

/// Fixture L: downed prey is not a finished hunt: the hunter walks over,
/// waits 180 ticks and executes it; the corpse is left (no storage).
#[test]
fn downed_prey_is_executed_after_the_wait() {
    let Some(mut f) = field("WildBoar", Cell::new(100, 100), 250_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    f.mark();
    for _ in 0..40 {
        f.tick();
    }
    assert!(matches!(f.job(f.hunter), Some(JobKind::Hunt { .. })));
    f.sim.debug_add_hediff(f.prey, "Anesthetic", 1.0);
    let mut wait_started = None;
    let mut executed = None;
    for _ in 0..3000 {
        f.tick();
        if wait_started.is_none()
            && matches!(
                f.job(f.hunter),
                Some(JobKind::Hunt {
                    stage: HuntStage::ExecuteWait { .. },
                    ..
                })
            )
        {
            wait_started = Some(f.sim.tick_count());
        }
        if f.sim.pawn(f.prey).unwrap().health.dead {
            executed = Some(f.sim.tick_count());
            break;
        }
    }
    assert!(f.sim.hunt_designated(f.prey) || executed.is_some());
    let (w, e) = (
        wait_started.expect("execution wait"),
        executed.expect("executed"),
    );
    assert_eq!(e - w, 179, "the 180-tick wait (entry to execution)");
    let hp = f.sim.pawn(f.hunter).unwrap().position;
    let bp = f.sim.pawn(f.prey).unwrap().position;
    assert!(
        (hp.x - bp.x).abs() <= 1 && (hp.z - bp.z).abs() <= 1,
        "touching"
    );
}

/// Fixture O: removing the mark ends the hunt.
#[test]
fn removing_the_mark_ends_the_hunt() {
    let Some(mut f) = field("Hare", Cell::new(108, 100), 300_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    f.mark();
    for _ in 0..40 {
        f.tick();
    }
    assert!(matches!(f.job(f.hunter), Some(JobKind::Hunt { .. })));
    let at = f.sim.pawn(f.prey).unwrap().position;
    f.sim.cancel_hunt(&[at]);
    while f.sim.stance_of(f.hunter).is_some() {
        f.tick();
    }
    f.tick();
    assert!(!matches!(f.job(f.hunter), Some(JobKind::Hunt { .. })));
}

/// A Muffalo (revenge chance 0.1, ×3, no stealth: 0.3 per hit) hunted by
/// a hunter without Animals skill sooner or later turns manhunter and
/// attacks in melee.
#[test]
fn a_hunted_muffalo_can_turn_manhunter() {
    let Some(mut f) = field("Muffalo", Cell::new(105, 100), 130_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    f.mark();
    let mut manhunter = false;
    for _ in 0..20_000 {
        f.tick();
        if f.sim.mental_state_of(f.prey) == Some("Manhunter") {
            manhunter = true;
        }
        if manhunter
            && matches!(f.job(f.prey), Some(JobKind::AttackMelee { target }) if target == f.hunter)
        {
            return;
        }
        if f.sim.pawn(f.prey).unwrap().health.dead || f.sim.pawn(f.hunter).unwrap().health.downed {
            break;
        }
    }
    assert!(manhunter, "never turned manhunter");
}

/// With a dumping stockpile (ordinary stockpiles refuse corpses), the hunter carries the corpse there.
#[test]
fn a_hunted_corpse_is_carried_to_storage() {
    let Some(mut f) = field("Hare", Cell::new(108, 100), 160_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let cells: Vec<Cell> = (80..83)
        .flat_map(|x| (95..98).map(move |z| Cell::new(x, z)))
        .collect();
    f.sim
        .designate_stockpile_with(&cells, StoragePreset::DumpingStockpile)
        .unwrap();
    f.mark();
    for _ in 0..20_000 {
        f.tick();
        if let Some(c) = f.sim.corpse_of(f.prey)
            && let Some(item) = f.sim.map.item(c)
            && cells.contains(&item.position)
        {
            return;
        }
    }
    panic!("corpse never stored");
}

/// Reloads while walking to the cast cell, while the prey flees, during
/// the execution wait and while carrying the corpse: each continues
/// exactly like the running game.
#[test]
fn hunting_state_survives_save_and_load() {
    let Some(mut f) = field("WildBoar", Cell::new(145, 100), 400_000) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };
    let cells: Vec<Cell> = (80..83)
        .flat_map(|x| (95..98).map(move |z| Cell::new(x, z)))
        .collect();
    f.sim
        .designate_stockpile_with(&cells, StoragePreset::DumpingStockpile)
        .unwrap();
    let defs = f.sim.defs.clone();
    f.mark();
    let mut checked: Vec<&str> = Vec::new();
    let mut downed = false;
    while checked.len() < 4 && f.sim.tick_count() < 420_000 {
        f.tick();
        let label = match (f.job(f.hunter), f.job(f.prey)) {
            (
                Some(JobKind::Hunt {
                    stage: HuntStage::GotoCast { .. },
                    ..
                }),
                _,
            ) => "walking to the cast cell",
            (_, Some(JobKind::Flee { .. })) => "fleeing",
            (
                Some(JobKind::Hunt {
                    stage:
                        HuntStage::ExecuteWait {
                            ticks_left: 50..=150,
                        },
                    ..
                }),
                _,
            ) => "execution wait",
            (Some(JobKind::Haul { .. }), _) if f.sim.pawn(f.hunter).unwrap().carried.is_some() => {
                "carrying the corpse"
            }
            _ => {
                // Down the boar once it has been shot at a few times.
                if !downed && checked.len() >= 2 {
                    f.sim.debug_add_hediff(f.prey, "Anesthetic", 1.0);
                    downed = true;
                }
                continue;
            }
        };
        if checked.contains(&label) {
            continue;
        }
        let data = f.sim.save();
        let mut copy = Sim::load(defs.clone(), &data).expect("loads");
        assert_eq!(copy.save(), data, "{label}: re-save");
        f.sim.forget_unsaved_state();
        // The fixture's overrides are not part of a save.
        copy.debug_set_sky_glow(Some(1.0));
        copy.outdoor_temperature = 21.0;
        for _ in 0..60 {
            for s in [&mut f.sim, &mut copy] {
                for p in [f.hunter, f.prey] {
                    if s.pawn(p).is_some_and(|x| !x.health.dead) {
                        s.debug_set_need(p, NeedKind::Food, 1.0);
                        s.debug_set_need(p, NeedKind::Rest, 1.0);
                        s.debug_set_need(p, NeedKind::Joy, 1.0);
                    }
                }
                s.tick();
            }
        }
        let (a, b) = (f.sim.save(), copy.save());
        if a != b {
            let at = a
                .bytes()
                .zip(b.bytes())
                .position(|(x, y)| x != y)
                .unwrap_or(0);
            let from = at.saturating_sub(300);
            panic!(
                "{label}: diverged after reload
 running: {}
 reloaded: {}",
                &a[from..(at + 200).min(a.len())],
                &b[from..(at + 200).min(b.len())]
            );
        }
        checked.push(label);
    }
    assert_eq!(checked.len(), 4, "{checked:?}");
}
