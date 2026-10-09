//! Work-selection fixtures reproducing the runtime cases A–N of the
//! external work-selection report (recorded in the original game, rev591),
//! with controlled providers as the report's diagnostic used: the real
//! settings, list building and dispatcher choose the job.

use std::cell::RefCell;
use std::collections::HashMap;

use rimworld_defs::xml::ActivePackages;
use rimworld_defs::{DefId, GameDefs, WorkGiverDef, load_documents};

use crate::grid::{Cell, Grid, GridSize};
use crate::job::{Job, JobKind};
use crate::map::{ItemId, Map};
use crate::path::{LocomotionUrgency, MoveCosts, PathGrid};
use crate::pawn::PawnId;
use crate::reservation::{Claimant, ReservationManager};
use crate::work::{GiverLists, WorkContext, WorkGiver, WorkSettings, giver_lists, try_issue_job};

const ROOT: Cell = Cell::new(10, 10);

fn defs(types: &[(&str, i32)], givers: &[(&str, &str, i32, bool)]) -> GameDefs {
    let mut xml = String::from("<Defs><TerrainDef><defName>Soil</defName></TerrainDef>");
    xml += "<ThingDef><defName>Probe</defName><category>Item</category></ThingDef>";
    for (name, natural) in types {
        xml += &format!(
            "<WorkTypeDef><defName>{name}</defName><naturalPriority>{natural}</naturalPriority></WorkTypeDef>"
        );
    }
    for (name, ty, prio, emergency) in givers {
        xml += &format!(
            "<WorkGiverDef><defName>{name}</defName><giverClass>Probe</giverClass>\
             <workType>{ty}</workType><priorityInType>{prio}</priorityInType>\
             <emergency>{emergency}</emergency><scanCells>true</scanCells></WorkGiverDef>"
        );
    }
    xml += "</Defs>";
    let (db, report) = load_documents("core", &[("w.xml", &xml)], &ActivePackages::default());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let (defs, w) = GameDefs::from_database(db);
    assert!(w.is_empty(), "{w:?}");
    defs
}

/// A controlled provider: fixed things and cells, optional scores.
#[derive(Default, Clone)]
struct Probe {
    things: Vec<ItemId>,
    cells: Vec<Cell>,
    prioritized: bool,
    thing_scores: HashMap<ItemId, f32>,
    cell_scores: HashMap<Cell, f32>,
    non_scan: Option<Cell>,
    skip: bool,
    /// HasJob accepts but JobOn yields nothing (an inconsistent provider).
    null_job: bool,
}

struct Running<'a> {
    probe: &'a Probe,
    name: String,
    log: &'a RefCell<Vec<String>>,
}

fn job_to(c: Cell) -> (Job, Vec<ItemId>) {
    (
        Job {
            def: None,
            kind: JobKind::Goto { target: c },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        },
        Vec::new(),
    )
}

impl WorkGiver for Running<'_> {
    fn should_skip(&self, _: &WorkContext<'_>) -> bool {
        self.probe.skip
    }
    fn non_scan_job(&self, _: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        self.probe.non_scan.map(job_to)
    }
    fn potential_things(&self, _: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(self.probe.things.clone())
    }
    fn prioritized(&self) -> bool {
        self.probe.prioritized
    }
    fn has_job_on_thing(&self, _: &WorkContext<'_>, t: ItemId) -> bool {
        self.log
            .borrow_mut()
            .push(format!("{} has {t:?}", self.name));
        true
    }
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        if self.probe.null_job {
            return None;
        }
        Some(job_to(ctx.map.item(t)?.position))
    }
    fn thing_priority(&self, _: &WorkContext<'_>, t: ItemId) -> f32 {
        self.probe.thing_scores.get(&t).copied().unwrap_or(0.0)
    }
    fn potential_cells(&self, _: &WorkContext<'_>) -> Vec<Cell> {
        self.probe.cells.clone()
    }
    fn has_job_on_cell(&self, _: &WorkContext<'_>, _: Cell) -> bool {
        true
    }
    fn job_on_cell(&self, _: &WorkContext<'_>, c: Cell) -> Option<(Job, Vec<ItemId>)> {
        (!self.probe.null_job).then(|| job_to(c))
    }
    fn cell_priority(&self, _: &WorkContext<'_>, c: Cell) -> f32 {
        self.probe.cell_scores.get(&c).copied().unwrap_or(0.0)
    }
}

struct World {
    defs: GameDefs,
    map: Map,
    grid: PathGrid,
    reservations: ReservationManager,
    probes: HashMap<String, Probe>,
    log: RefCell<Vec<String>>,
}

impl World {
    fn new(defs: GameDefs) -> Self {
        let soil = defs.terrain.id("Soil").unwrap();
        let size = GridSize::new(21, 21);
        Self {
            map: Map::new(size, soil),
            grid: PathGrid::new(Grid::new(size, Some(0))),
            defs,
            reservations: ReservationManager::default(),
            probes: HashMap::new(),
            log: RefCell::new(Vec::new()),
        }
    }

    fn thing(&mut self, offset: (i32, i32)) -> ItemId {
        let def = self.defs.things.id("Probe").unwrap();
        self.map
            .spawn_item(def, ROOT + Cell::new(offset.0, offset.1), 1)
    }

    fn settings(&self, priorities: &[(&str, i32)]) -> WorkSettings {
        let mut s = WorkSettings::initialize(&self.defs, &|_| false);
        for t in self.defs.work_type_ids() {
            s.set(t, 0, false);
        }
        for (name, p) in priorities {
            s.set(self.defs.work_types.id(name).unwrap(), *p, false);
        }
        s
    }

    fn lists(&self, priorities: &[(&str, i32)], manual: bool) -> GiverLists {
        giver_lists(&self.defs, &self.settings(priorities), manual, true)
    }

    /// Runs the dispatcher; returns (giver name, target cell).
    fn select(&self, list: &[DefId<WorkGiverDef>]) -> Option<(String, Cell)> {
        let regions = crate::region::Regions::build(&self.map, &self.defs, &self.grid);
        let mut rng = crate::rand::Rand::new(1);
        let ctx = WorkContext {
            defs: &self.defs,
            map: &self.map,
            grid: &self.grid,
            regions: &regions,
            rng: RefCell::new(&mut rng),
            reservations: &self.reservations,
            claimant: Claimant {
                pawn: PawnId(0),
                has_faction: true,
            },
            position: ROOT,
            incapable: &[],
            costs: MoveCosts::from_move_speed(4.6),
            tick: 0,
        };
        let providers = |g: DefId<WorkGiverDef>| -> Option<Box<dyn WorkGiver + '_>> {
            let name = self.defs.work_givers[g].def_name.clone();
            let probe = self.probes.get(&name)?;
            Some(Box::new(Running {
                probe,
                name,
                log: &self.log,
            }))
        };
        let work = try_issue_job(&ctx, list, &providers)?;
        let JobKind::Goto { target } = work.job.kind else {
            panic!("probe jobs are gotos")
        };
        Some((self.defs.work_givers[work.giver].def_name.clone(), target))
    }

    fn names(&self, list: &[DefId<WorkGiverDef>]) -> Vec<String> {
        list.iter()
            .map(|&g| self.defs.work_givers[g].def_name.clone())
            .collect()
    }
}

fn haul_clean() -> World {
    World::new(defs(
        &[("Hauling", 300), ("Cleaning", 200)],
        &[
            ("ProbeHaul", "Hauling", 0, false),
            ("ProbeClean", "Cleaning", 0, false),
        ],
    ))
}

fn one_thing_each(w: &mut World, haul: (i32, i32), clean: (i32, i32)) {
    let h = w.thing(haul);
    let c = w.thing(clean);
    w.probes.insert(
        "ProbeHaul".into(),
        Probe {
            things: vec![h],
            ..Default::default()
        },
    );
    w.probes.insert(
        "ProbeClean".into(),
        Probe {
            things: vec![c],
            ..Default::default()
        },
    );
}

#[test]
fn case_a_b_player_priority_beats_distance() {
    let mut w = haul_clean();
    one_thing_each(&mut w, (6, 0), (2, 0));
    // A: Haul 1, Clean 2: hauling first even though cleaning is nearer,
    // and cleaning is never asked.
    let l = w.lists(&[("Hauling", 1), ("Cleaning", 2)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "ProbeHaul");
    assert!(!w.log.borrow().iter().any(|l| l.starts_with("ProbeClean")));
    // B: swapped.
    let l = w.lists(&[("Hauling", 2), ("Cleaning", 1)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "ProbeClean");
}

#[test]
fn case_c_c2_natural_priority_then_work_type_database_order() {
    let mut w = haul_clean();
    one_thing_each(&mut w, (6, 0), (2, 0));
    let l = w.lists(&[("Hauling", 2), ("Cleaning", 2)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "ProbeHaul");
    // C2: equal natural priorities; the cleaning giver comes first in the
    // giver database, but work-type database order still puts hauling first.
    let mut w = World::new(defs(
        &[("Hauling", 300), ("Cleaning", 300)],
        &[
            ("ProbeClean", "Cleaning", 0, false),
            ("ProbeHaul", "Hauling", 0, false),
        ],
    ));
    one_thing_each(&mut w, (6, 0), (2, 0));
    let l = w.lists(&[("Hauling", 2), ("Cleaning", 2)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "ProbeHaul");
}

#[test]
fn case_d_e_e2_off_disabled_and_manual_mode() {
    let mut w = haul_clean();
    one_thing_each(&mut w, (6, 0), (2, 0));
    // D: priority 0 removes the type.
    let l = w.lists(&[("Hauling", 0), ("Cleaning", 1)], true);
    assert_eq!(w.names(&l.normal), vec!["ProbeClean"]);
    // E: a disabled type refuses a non-zero priority.
    let mut s = w.settings(&[("Hauling", 2)]);
    let clean = w.defs.work_types.id("Cleaning").unwrap();
    assert!(!s.set(clean, 1, true));
    assert_eq!(s.raw(clean), 0);
    // E2: manual priorities off: stored 2 and 1 both act as 3, so natural
    // priority decides; the stored numbers survive.
    let s = w.settings(&[("Hauling", 2), ("Cleaning", 1)]);
    let l = giver_lists(&w.defs, &s, false, true);
    assert_eq!(w.names(&l.normal), vec!["ProbeHaul", "ProbeClean"]);
    assert_eq!(s.raw(clean), 1);
    assert_eq!(s.effective(clean, false, true), 3);
}

fn same_type(givers: &[(&str, i32)]) -> World {
    let g: Vec<(&str, &str, i32, bool)> = givers
        .iter()
        .map(|&(n, p)| (n, "Hauling", p, false))
        .collect();
    World::new(defs(&[("Hauling", 300)], &g))
}

#[test]
fn case_f_f2_f3_giver_order_and_fallback() {
    // F: Low10 is first in the database, High20 sorts first and wins with
    // a farther target.
    let mut w = same_type(&[("Low", 10), ("High", 20)]);
    let near = w.thing((2, 0));
    let far = w.thing((6, 0));
    w.probes.insert(
        "Low".into(),
        Probe {
            things: vec![near],
            ..Default::default()
        },
    );
    w.probes.insert(
        "High".into(),
        Probe {
            things: vec![far],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(
        w.select(&l.normal),
        Some(("High".into(), ROOT + Cell::new(6, 0)))
    );
    // F2: an empty higher giver falls through.
    w.probes.get_mut("High").unwrap().things.clear();
    assert_eq!(w.select(&l.normal).unwrap().0, "Low");
    // F3: equal priorities: the first giver's farther target wins; the
    // second giver is never asked.
    let mut w = same_type(&[("FirstFar", 10), ("NextNear", 10)]);
    let far = w.thing((0, 7));
    let near = w.thing((2, 0));
    w.probes.insert(
        "FirstFar".into(),
        Probe {
            things: vec![far],
            ..Default::default()
        },
    );
    w.probes.insert(
        "NextNear".into(),
        Probe {
            things: vec![near],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "FirstFar");
    assert!(!w.log.borrow().iter().any(|l| l.starts_with("NextNear")));
}

#[test]
fn case_g_h_nearest_thing_and_ties() {
    let mut w = same_type(&[("One", 0)]);
    let far = w.thing((6, 0));
    let near = w.thing((2, 0));
    w.probes.insert(
        "One".into(),
        Probe {
            things: vec![far, near],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(2, 0));
    // H: equal distances keep the first; reversing the list reverses it.
    let a = w.thing((3, 0));
    let b = w.thing((-3, 0));
    w.probes.get_mut("One").unwrap().things = vec![a, b];
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(3, 0));
    w.probes.get_mut("One").unwrap().things = vec![b, a];
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(-3, 0));
}

#[test]
fn case_h2_h3_prioritized_things() {
    let mut w = same_type(&[("One", 0)]);
    let near = w.thing((2, 0));
    let far = w.thing((6, 0));
    let l = w.lists(&[("Hauling", 1)], true);
    let scored = |scores: &[(ItemId, f32)], order: Vec<ItemId>| Probe {
        things: order,
        prioritized: true,
        thing_scores: scores.iter().copied().collect(),
        ..Default::default()
    };
    // H2: a higher score beats distance; equal scores take the nearer.
    w.probes.insert(
        "One".into(),
        scored(&[(near, 1.0), (far, 2.0)], vec![near, far]),
    );
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(6, 0));
    w.probes.insert(
        "One".into(),
        scored(&[(near, 1.0), (far, 1.0)], vec![near, far]),
    );
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(2, 0));
    // H3: 1 and 1.00000048 count as equal only one way round.
    let s = [(near, 1.0), (far, 1.000_000_5)];
    w.probes.insert("One".into(), scored(&s, vec![near, far]));
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(2, 0));
    w.probes.insert("One".into(), scored(&s, vec![far, near]));
    assert_eq!(w.select(&l.normal).unwrap().1, ROOT + Cell::new(6, 0));
}

#[test]
fn case_k_k2_cells_overwrite_things_and_tie() {
    let mut w = same_type(&[("One", 0)]);
    let thing = w.thing((2, 0));
    let cell = ROOT + Cell::new(7, 0);
    w.probes.insert(
        "One".into(),
        Probe {
            things: vec![thing],
            cells: vec![cell],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(
        w.select(&l.normal).unwrap().1,
        cell,
        "K: the cell phase overwrites the thing"
    );
    let (a, b) = (ROOT + Cell::new(3, 0), ROOT + Cell::new(-3, 0));
    w.probes.insert(
        "One".into(),
        Probe {
            cells: vec![a, b],
            ..Default::default()
        },
    );
    assert_eq!(w.select(&l.normal).unwrap().1, a);
    w.probes.insert(
        "One".into(),
        Probe {
            cells: vec![b, a],
            ..Default::default()
        },
    );
    assert_eq!(w.select(&l.normal).unwrap().1, b);
    w.probes.insert(
        "One".into(),
        Probe {
            cells: vec![b, a],
            prioritized: true,
            cell_scores: [(a, 2.0)].into_iter().collect(),
            ..Default::default()
        },
    );
    assert_eq!(w.select(&l.normal).unwrap().1, a);
}

#[test]
fn case_l_l3_non_scan_first_and_should_skip() {
    let mut w = same_type(&[("One", 0)]);
    let thing = w.thing((2, 0));
    let ns = ROOT + Cell::new(0, 5);
    w.probes.insert(
        "One".into(),
        Probe {
            things: vec![thing],
            non_scan: Some(ns),
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(w.select(&l.normal).unwrap().1, ns);
    assert!(w.log.borrow().is_empty(), "L: no scan callbacks");
    // L3: a skipped giver yields to the next.
    let mut w = same_type(&[("Skip", 20), ("AfterSkip", 10)]);
    let t = w.thing((2, 0));
    w.probes.insert(
        "Skip".into(),
        Probe {
            things: vec![t],
            skip: true,
            ..Default::default()
        },
    );
    w.probes.insert(
        "AfterSkip".into(),
        Probe {
            things: vec![t],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(w.select(&l.normal).unwrap().0, "AfterSkip");
}

#[test]
fn case_m_m2_inconsistent_provider_guard() {
    // M: a target without a job blocks the next, lower giver: no job.
    let mut w = same_type(&[("NullHigh", 20), ("LowAfterNull", 10)]);
    let t = w.thing((2, 0));
    let u = w.thing((4, 0));
    w.probes.insert(
        "NullHigh".into(),
        Probe {
            things: vec![t],
            null_job: true,
            ..Default::default()
        },
    );
    w.probes.insert(
        "LowAfterNull".into(),
        Probe {
            things: vec![u],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(w.select(&l.normal), None);
    assert!(!w.log.borrow().iter().any(|l| l.starts_with("LowAfterNull")));
    // M2: a giver of the same priority may replace the target.
    let mut w = same_type(&[("NullFirst", 20), ("ValidSame", 20)]);
    let t = w.thing((2, 0));
    let u = w.thing((4, 0));
    w.probes.insert(
        "NullFirst".into(),
        Probe {
            things: vec![t],
            null_job: true,
            ..Default::default()
        },
    );
    w.probes.insert(
        "ValidSame".into(),
        Probe {
            things: vec![u],
            ..Default::default()
        },
    );
    let l = w.lists(&[("Hauling", 1)], true);
    assert_eq!(
        w.select(&l.normal),
        Some(("ValidSame".into(), ROOT + Cell::new(4, 0)))
    );
}

#[test]
fn case_n_emergency_partition() {
    let w = World::new(defs(
        &[("Hauling", 300), ("Cleaning", 200)],
        &[
            ("EmergencyHaul", "Hauling", 20, true),
            ("NormalClean", "Cleaning", 10, false),
        ],
    ));
    // Haul 2, Clean 1: the emergency giver is less urgent than the most
    // urgent normal work, so it is demoted into the normal list.
    let l = w.lists(&[("Hauling", 2), ("Cleaning", 1)], true);
    assert!(l.emergency.is_empty());
    assert_eq!(w.names(&l.normal), vec!["NormalClean", "EmergencyHaul"]);
    // Haul 1: it stays an emergency giver.
    let l = w.lists(&[("Hauling", 1), ("Cleaning", 1)], true);
    assert_eq!(w.names(&l.emergency), vec!["EmergencyHaul"]);
    assert_eq!(w.names(&l.normal), vec!["NormalClean"]);
}
