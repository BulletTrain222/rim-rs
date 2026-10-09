//! Player work orders from the float menu
//! (`FloatMenuOptionProvider_WorkGivers`, docs/research.md §66): every
//! directly orderable work giver that considers the clicked target is
//! asked for its forced job there, and the player can take one as a
//! prioritized, player-forced job.

use rimworld_defs::{DefId, WorkGiverDef};

use super::Sim;
use crate::job::JobKind;
use crate::pawn::PawnId;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkJob, WorkTarget};

/// The reservation a work order's target is held by.
pub(super) fn reservation_target(t: WorkTarget) -> Target {
    match t {
        WorkTarget::Thing(id) => Target::Item(id),
        WorkTarget::Rock(c) | WorkTarget::Cell(c) => Target::Cell(c),
    }
}

/// Why a work order is offered but disabled, checked in the provider's
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkOrderBlock {
    /// `CannotMissingHealthActivities`: the giver needs this capacity.
    MissingCapacity(String),
    /// `CannotGenericAlreadyAm`: the pawn is doing this very job.
    AlreadyDoing,
    /// `CannotPrioritizeNotAssignedToWorkType`: the work type is off.
    NotAssigned,
    /// `CannotPrioritizeResearch`.
    Research,
    /// `CannotPrioritizeForbidden`.
    Forbidden,
    /// `NoPath`.
    NoPath,
}

/// One "Prioritize" float menu order for a pawn and a clicked target.
#[derive(Debug, Clone)]
pub struct WorkOrderOption {
    pub giver: DefId<WorkGiverDef>,
    pub target: WorkTarget,
    /// `PostProcessedGerund`: a keyed string replacing the giver's gerund
    /// (the designated plant cutter says "harvesting" or "cutting").
    pub gerund_key: Option<&'static str>,
    pub block: Option<WorkOrderBlock>,
    /// `DecoratePrioritizedTask`: another pawn holds the target; the order
    /// takes it from them.
    pub reserved_by: Option<PawnId>,
    job: WorkJob,
}

/// The target a job is about, for `Job.JobIsSameAs`.
fn job_target(kind: &JobKind, queue: &[crate::map::ItemId]) -> Option<WorkTarget> {
    Some(match *kind {
        JobKind::Mine { cell, .. } => WorkTarget::Rock(cell),
        JobKind::Haul { source, .. } => WorkTarget::Thing(source),
        JobKind::HaulToContainer { container, .. } => WorkTarget::Thing(container),
        JobKind::FinishFrame { frame, .. } => WorkTarget::Thing(frame),
        JobKind::Deconstruct { building, .. } => WorkTarget::Thing(building),
        JobKind::Flick { target, .. } => WorkTarget::Thing(target),
        JobKind::Refuel { building, .. } => WorkTarget::Thing(building),
        JobKind::FixBrokenDown { building, .. } => WorkTarget::Thing(building),
        JobKind::Research { bench, .. } => WorkTarget::Thing(bench),
        JobKind::DoBill { giver, .. } => WorkTarget::Thing(giver),
        JobKind::PlaceNoCostFrame { blueprint, .. } => WorkTarget::Thing(blueprint),
        JobKind::Clean { target, .. } => WorkTarget::Thing(target.or(queue.first().copied())?),
        JobKind::Harvest { target, .. } => WorkTarget::Thing(target.or(queue.first().copied())?),
        JobKind::Sow { cell, .. }
        | JobKind::BuildRoof { cell, .. }
        | JobKind::AffectFloor { cell, .. }
        | JobKind::SmoothWall { cell, .. } => WorkTarget::Cell(cell),
        _ => return None,
    })
}

impl Sim {
    /// The "Prioritize ..." orders a colonist's float menu offers for one
    /// clicked target, in the provider's order (work types in database
    /// order, givers by priority). Givers without a job there are left out
    /// (they would need a `JobFailReason`).
    ///
    /// The query asks the real work givers with the target's other
    /// reservations ignored (`forced`). It leaves the game's random stream
    /// untouched.
    // COMPATIBILITY TODO: currently approximate — `JobFailReason` texts
    // (e.g. missing materials), work tags and work types disabled by
    // backstories, pawn targets (hunt, rescue, tend) and cell targets'
    // forbidden areas are not modelled; querying here does not draw from
    // the random stream as the game's menu may.
    pub fn work_order_options(&mut self, pawn: PawnId, target: WorkTarget) -> Vec<WorkOrderOption> {
        let Some(i) = self.index_of(pawn) else {
            return Vec::new();
        };
        let p = &self.pawns[i];
        if !p.is_colonist || p.health.downed || p.health.dead || p.work.is_none() {
            return Vec::new();
        }
        let saved_rng = self.rng.clone();
        self.work_query = Some(target);
        let hour = self.hour_of_day();
        let _ = self.think_for(i, hour);
        self.work_query = None;
        self.rng = saved_rng;
        let found = std::mem::take(&mut self.work_query_out);
        let incapable = self.incapable_capacities(i);
        let claimant = self.claimant(i);
        let rt = reservation_target(target);
        let mut out = Vec::new();
        for (giver, job) in found {
            let Some(job) = job else {
                continue;
            };
            let def = &self.defs.work_givers[giver];
            let p = &self.pawns[i];
            let block = if let Some(cap) = def
                .required_capacities
                .iter()
                .find(|c| incapable.contains(c))
            {
                Some(WorkOrderBlock::MissingCapacity(cap.clone()))
            } else if p.job.as_ref().is_some_and(|cur| {
                cur.def == job.job.def && job_target(&cur.kind, &p.target_queue) == Some(target)
            }) {
                Some(WorkOrderBlock::AlreadyDoing)
            } else if self.work_type_priority(i, def.work_type.as_deref()) == 0 {
                Some(WorkOrderBlock::NotAssigned)
            } else if job.job.def.is_some() && job.job.def == self.job_defs.research {
                Some(WorkOrderBlock::Research)
            } else if self.work_target_forbidden(target) {
                Some(WorkOrderBlock::Forbidden)
            } else if !self.can_reach_work_target(i, target) {
                Some(WorkOrderBlock::NoPath)
            } else {
                None
            };
            let reserved_by = if block.is_none()
                && !self.reservations.can_reserve(claimant, rt, 1, 1, STACK_ALL)
            {
                self.reservations.reservers_of(rt, pawn).first().copied()
            } else {
                None
            };
            let gerund_key = match job.job.kind {
                JobKind::Harvest { cut, .. }
                    if def.giver_class.as_deref() == Some("WorkGiver_PlantsCut") =>
                {
                    Some(if cut { "CutGerund" } else { "HarvestGerund" })
                }
                _ => None,
            };
            out.push(WorkOrderOption {
                giver,
                target,
                gerund_key,
                block,
                reserved_by,
                job,
            });
        }
        out
    }

    /// Takes a work order (`TryTakeOrderedJobPrioritizedWork`): the job is
    /// player-forced, interrupts what the pawn was doing and takes the
    /// target from any pawn holding it (`ReservationManager.Reserve` with
    /// `playerForced`: their jobs end). Returns whether the job started.
    // COMPATIBILITY TODO: currently approximate — `prioritizeSustains`
    // (`PriorityWork`: keeping the pawn on this work nearby) and queueing
    // with Shift are not modelled.
    pub fn take_work_order(
        &mut self,
        pawn: PawnId,
        giver: DefId<WorkGiverDef>,
        target: WorkTarget,
    ) -> bool {
        let Some(option) = self
            .work_order_options(pawn, target)
            .into_iter()
            .find(|o| o.giver == giver && o.block.is_none())
        else {
            return false;
        };
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        let rt = reservation_target(target);
        let others = self.reservations.reservers_of(rt, pawn);
        self.reservations.release_others_on(rt, pawn);
        self.interrupt_for_order(i);
        self.drop_carried(i);
        let WorkJob { mut job, queue, .. } = option.job;
        job.forced = true;
        self.pawns[i].target_queue = queue;
        let started = self.start_job(i, job, false);
        for other in others {
            if let Some(j) = self.index_of(other) {
                self.end_job(j, false);
            }
        }
        started
    }

    /// A work type's effective priority for pawn `i` (0: off).
    fn work_type_priority(&self, i: usize, work_type: Option<&str>) -> i32 {
        let p = &self.pawns[i];
        let (Some(w), Some(t)) = (
            p.work.as_ref(),
            work_type.and_then(|n| self.defs.work_types.id(n)),
        ) else {
            return 0;
        };
        w.effective(t, self.use_work_priorities, true)
    }

    /// Whether a work order's target thing is forbidden.
    fn work_target_forbidden(&self, target: WorkTarget) -> bool {
        match target {
            WorkTarget::Thing(id) => self.map.item(id).is_some_and(|it| it.forbidden),
            _ => false,
        }
    }

    /// `CanReach` the target with Touch: the pawn stands on or next to it,
    /// or can walk to a walkable cell touching it.
    fn can_reach_work_target(&self, i: usize, target: WorkTarget) -> bool {
        let at = self.pawns[i].position;
        let cells: Vec<crate::grid::Cell> = match target {
            WorkTarget::Rock(c) | WorkTarget::Cell(c) => vec![c],
            WorkTarget::Thing(id) => match self.thing_footprint(id) {
                Some(fp) => fp.cells().collect(),
                None => return false,
            },
        };
        cells.iter().any(|&c| {
            (self.path_grid.walkable(c) && (c == at || self.regions.connected(at, c)))
                || crate::grid::Cell::NEIGHBORS_8.iter().any(|&d| {
                    let n = c + d;
                    self.map.size().contains(n)
                        && self.path_grid.walkable(n)
                        && (n == at || self.regions.connected(at, n))
                })
        })
    }

    /// `HostileTo(Faction.OfPlayer)`: humanlikes outside the colony and
    /// animals hunting people.
    // COMPATIBILITY TODO: currently approximate — there are no factions:
    // every non-colonist humanlike counts as hostile.
    pub fn hostile_to_colony(&self, pawn: PawnId) -> bool {
        let Some(p) = self.pawn(pawn) else {
            return false;
        };
        if p.is_colonist {
            return false;
        }
        if self.is_animal(pawn) {
            return self.mental_state_of(pawn) == Some("Manhunter");
        }
        true
    }

    /// `FloatMenuUtility.GetMeleeAttackAction`'s fail reasons (keyed):
    /// not drafted, no path, the pawn itself.
    pub fn melee_attack_check(&self, pawn: PawnId, target: PawnId) -> Result<(), &'static str> {
        let (Some(i), Some(t)) = (self.index_of(pawn), self.index_of(target)) else {
            return Err("NoPath");
        };
        if !self.pawns[i].drafted {
            return Err("IsNotDraftedLower");
        }
        if i == t {
            return Err("CannotAttackSelf");
        }
        let at = self.pawns[i].position;
        let tc = self.pawns[t].position;
        let touch = std::iter::once(tc)
            .chain(crate::grid::Cell::NEIGHBORS_8.iter().map(|&d| tc + d))
            .any(|c| {
                self.map.size().contains(c)
                    && self.path_grid.walkable(c)
                    && (c == at || self.regions.connected(at, c))
            });
        if !touch {
            return Err("NoPath");
        }
        Ok(())
    }

    /// `CanReach` a cell on the cell itself.
    pub fn can_reach_cell(&self, pawn: PawnId, c: crate::grid::Cell) -> bool {
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        let at = self.pawns[i].position;
        self.map.size().contains(c)
            && self.path_grid.walkable(c)
            && (c == at || self.regions.connected(at, c))
    }

    /// `CellFinder.StandableCellNear`: the nearest standable cell within
    /// `radius` of `c`.
    // COMPATIBILITY TODO: currently approximate — equally near cells are
    // tried in x-then-z order, not the game's radial pattern order.
    pub fn standable_cell_near(
        &self,
        c: crate::grid::Cell,
        radius: f32,
    ) -> Option<crate::grid::Cell> {
        let r = radius.ceil() as i32;
        let mut cells: Vec<crate::grid::Cell> = (-r..=r)
            .flat_map(|dx| (-r..=r).map(move |dz| crate::grid::Cell::new(dx, dz)))
            .filter(|d| ((d.x * d.x + d.z * d.z) as f32) <= radius * radius)
            .map(|d| c + d)
            .collect();
        cells.sort_by_key(|n| (n.x - c.x).pow(2) + (n.z - c.z).pow(2));
        cells
            .into_iter()
            .find(|&n| self.map.size().contains(n) && self.path_grid.walkable(n))
    }

    /// The equip float menu option's checks (`FloatMenuOptionProvider_Equip`):
    /// a reachable firearm (`NoPath`), a pawn able to manipulate
    /// (`Incapable`).
    pub fn equip_check(&self, pawn: PawnId, item: crate::map::ItemId) -> Result<(), &'static str> {
        let Some(i) = self.index_of(pawn) else {
            return Err("Incapable");
        };
        if !self.can_reach_work_target(i, WorkTarget::Thing(item)) {
            return Err("NoPath");
        }
        if self
            .incapable_capacities(i)
            .iter()
            .any(|c| c == "Manipulation")
        {
            return Err("Incapable");
        }
        Ok(())
    }

    /// Where a thing (item, building, blueprint, frame or plant) stands.
    pub fn thing_footprint(&self, id: crate::map::ItemId) -> Option<crate::geom::Footprint> {
        let single = |c| crate::geom::Footprint {
            center: c,
            rot: crate::job::Rot4::North,
            size: (1, 1),
        };
        if let Some(it) = self.map.item(id) {
            return Some(single(it.position));
        }
        if let Some(st) = self.map.structure(id) {
            return Some(st.footprint);
        }
        if let Some(k) = self.map.constructible(id) {
            return Some(k.footprint());
        }
        self.map
            .plants()
            .iter()
            .find(|p| p.id == id)
            .map(|p| single(p.position))
    }
}
