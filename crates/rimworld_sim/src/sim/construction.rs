//! Construction job drivers (docs/research.md §22): `HaulToContainer`
//! deliveries into blueprints and frames, and `FinishFrame`.

use rimworld_defs::{DefId, TerrainDef, ThingDef};

use super::{JobEvent, PATH_START_LATENCY_TICKS, Sim};
use crate::construct::{BuildView, is_completed, space_remaining_with_enroute};
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::haul::max_carry;
use crate::job::Rot4;
use crate::job::{BuildStage, ContainerStage, Job, JobKind};
use crate::map::{Buildable, ConstructStage, Constructible, ItemId, Map};
use crate::path::{COLONIST_HEURISTIC_STRENGTH, PathGrid, find_path};
use crate::pawn::{Carried, Pawn};
use crate::reservation::{JobId, STACK_ALL, Target};
use crate::stats::{def_stat, terrain_stat};

/// The build toil's duration: after this many ticks the job ends and the
/// pawn looks for work again (`JobDriver_ConstructFinishFrame`).
const BUILD_TOIL_TICKS: i32 = 5000;
/// Work per tick of `ConstructionSpeed` (the build toil's 1.7 factor).
const WORK_PER_SPEED_TICK: f32 = 1.7;
/// Pawns that may share a frame's reservation made on delivery.
const FRAME_DELIVERY_RESERVERS: i32 = 5;

/// `GenMath.RoundRandom`: the integer part plus one with probability of
/// the fraction (one random draw).
fn round_random(v: f32, rng: &mut crate::rand::Rand) -> u32 {
    let whole = v.trunc();
    whole as u32 + u32::from(rng.value() < v - whole)
}

impl Sim {
    /// Ids of current `HaulToContainer` jobs (their reservations do not
    /// stop other deliveries).
    pub(super) fn delivery_job_ids(&self) -> Vec<JobId> {
        self.pawns
            .iter()
            .filter(|p| {
                matches!(
                    p.job.as_ref().map(|j| &j.kind),
                    Some(JobKind::HaulToContainer { .. })
                )
            })
            .map(|p| p.job_id)
            .collect()
    }

    pub(super) fn pawn_cells(&self) -> Vec<(crate::pawn::PawnId, Cell)> {
        self.pawns
            .iter()
            .filter(|p| p.carried_by.is_none())
            .map(|p| (p.id, p.position))
            .collect()
    }

    /// `RCellFinder.TryFindGoodAdjacentSpotToTouch` for pawn `i`.
    pub(super) fn touch_spot(&mut self, i: usize, target: &Footprint) -> Option<Cell> {
        let pawn_cells = self.pawn_cells();
        let view = BuildView {
            defs: &self.defs,
            map: &self.map,
            grid: &self.path_grid,
            regions: &self.regions,
            pawn_cells: &pawn_cells,
            delivery_jobs: &[],
        };
        view.touch_spot(self.pawns[i].next_stop(), target, &mut self.rng)
            .0
    }

    fn blocked(&self, i: usize, k: &Constructible) -> bool {
        let pawn_cells = self.pawn_cells();
        BuildView {
            defs: &self.defs,
            map: &self.map,
            grid: &self.path_grid,
            regions: &self.regions,
            pawn_cells: &pawn_cells,
            delivery_jobs: &[],
        }
        .blocked(k, Some(self.pawns[i].id))
    }

    /// `JobDriver_HaulToContainer.TryMakePreToilReservations`: the
    /// resource, then what is on its way to the containers
    /// (`UpdateEnrouteTrackers`), then as many queued resources as
    /// possible. Containers that track deliveries are not reserved.
    pub(super) fn reserve_container_haul(
        &mut self,
        i: usize,
        job_id: JobId,
        kind: &JobKind,
    ) -> bool {
        let JobKind::HaulToContainer {
            source,
            container,
            primary,
            count,
            ..
        } = *kind
        else {
            return false;
        };
        // Queue B (destinations) arrived mixed into the target queue.
        let all = std::mem::take(&mut self.pawns[i].target_queue);
        let (queue_b, queue_a): (Vec<ItemId>, Vec<ItemId>) = all
            .into_iter()
            .partition(|&id| self.map.constructible(id).is_some());
        self.pawns[i].target_queue = queue_a.clone();
        self.pawns[i].target_queue_b = queue_b.clone();
        let claimant = self.claimant(i);
        let Some((def, stack)) = self.map.item(source).map(|it| (it.def, it.stack_count)) else {
            return false;
        };
        if !self.reservations.reserve(
            claimant,
            job_id,
            Target::Item(source),
            stack as i32,
            1,
            STACK_ALL,
        ) {
            return false;
        }
        let mut left = count;
        let mut targets = Vec::new();
        targets.extend(primary);
        if Some(container) != primary {
            targets.push(container);
        }
        targets.extend(queue_b.iter().copied().filter(|&b| Some(b) != primary));
        for t in targets {
            let Some(k) = self.map.constructible(t) else {
                continue;
            };
            let n = left.min(space_remaining_with_enroute(
                &self.defs,
                &self.enroute,
                k,
                def,
                None,
            ));
            if n > 0 {
                self.enroute.add(t, claimant.pawn, def, n as u32);
            }
            left -= n.max(0);
        }
        for q in queue_a {
            if let Some(n) = self.map.item(q).map(|it| it.stack_count as i32)
                && self
                    .reservations
                    .can_reserve(claimant, Target::Item(q), n, 1, STACK_ALL)
            {
                self.reservations
                    .reserve(claimant, job_id, Target::Item(q), n, 1, STACK_ALL);
            }
        }
        true
    }

    fn set_container_job(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut ItemId, &mut ItemId, &mut Option<ItemId>, &mut i32, &mut ContainerStage),
    ) {
        if let Some(Job {
            kind:
                JobKind::HaulToContainer {
                    source,
                    container,
                    primary,
                    count,
                    stage,
                },
            ..
        }) = &mut self.pawns[i].job
        {
            f(source, container, primary, count, stage);
        }
    }

    /// Starts a path for pawn `i` to `dest` (now if already there).
    /// Returns `false` if there is no path.
    pub(super) fn walk_to(&mut self, i: usize, dest: Cell, dest_is_thing: bool) -> bool {
        let tick = self.tick;
        let pawn = &mut self.pawns[i];
        pawn.path.clear();
        pawn.destination = None;
        let at = pawn.next_stop();
        if at == dest {
            return true;
        }
        match find_path(
            &self.path_grid,
            at,
            dest,
            pawn.move_costs,
            COLONIST_HEURISTIC_STRENGTH,
        ) {
            Ok(path) => {
                pawn.path = path.cells.into();
                pawn.destination = Some(dest);
                pawn.move_ready_tick = tick + PATH_START_LATENCY_TICKS;
                self.on_start_path(i, dest, dest_is_thing);
                true
            }
            Err(_) => false,
        }
    }

    /// `GotoBuild`: walk to a spot touching the constructible (its own cell
    /// if there is none). Arriving at once continues with `arrived`.
    fn goto_build(&mut self, i: usize, target: ItemId) -> bool {
        let Some(fp) = self.footprint_of(target) else {
            return false;
        };
        let dest = self.touch_spot(i, &fp).unwrap_or(fp.center);
        self.walk_to(i, dest, false)
    }

    /// Haul-to-container: at the resource; pick it up (`StartCarryThing`),
    /// then collect queued stacks of the same def
    /// (`JumpIfAlsoCollectingNextTargetInQueue`) or carry to the container.
    pub(super) fn container_pick_up(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::HaulToContainer { source, count, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some((def, stack)) = self.map.item(source).map(|it| (it.def, it.stack_count)) else {
            self.end_job(i, false);
            return;
        };
        let carried_now = self.pawns[i].carried;
        if carried_now.is_some_and(|c| c.def != def) {
            self.end_job(i, false);
            return;
        }
        let space = max_carry(&self.defs, def, self.carrying_capacity(i))
            - carried_now.map_or(0, |c| c.count as i32);
        let wanted = count.min(space).min(stack as i32);
        if wanted <= 0 {
            self.end_job(i, false);
            return;
        }
        let src_rot = self.map.item(source).map_or(0.0, |it| it.rot);
        let src_hp = self.map.item(source).and_then(|it| it.hit_points);
        let max_hp = self.max_hit_points(def);
        let taken = self.map.take_from_item(source, wanted as u32);
        let claimant = self.claimant(i);
        let job_id = self.pawns[i].job_id;
        let picked_id = if taken < stack {
            let id = self.map.allocate_item_id();
            self.reservations
                .release(Target::Item(source), claimant.pawn, job_id);
            id
        } else {
            source
        };
        let carried = match carried_now {
            // The picked-up part merges into what is carried.
            Some(mut c) => {
                c.rot = crate::map::blend_rot(c.rot, c.count, src_rot, taken);
                c.hit_points =
                    crate::map::blend_hit_points(c.hit_points, c.count, src_hp, taken, max_hp);
                c.count += taken;
                self.reservations
                    .release(Target::Item(picked_id), claimant.pawn, job_id);
                c
            }
            None => {
                if picked_id != source {
                    self.reservations.reserve(
                        claimant,
                        job_id,
                        Target::Item(picked_id),
                        1,
                        1,
                        STACK_ALL,
                    );
                }
                Carried {
                    id: picked_id,
                    def,
                    count: taken,
                    rot: src_rot,
                    hit_points: src_hp,
                }
            }
        };
        self.pawns[i].carried = Some(carried);
        self.refresh_path_grid();
        let mut left = 0;
        self.set_container_job(i, |src, _, _, cnt, _| {
            *src = carried.id;
            *cnt -= taken as i32;
            left = *cnt;
        });
        // Collect the next queued stack of the same def.
        let space = max_carry(&self.defs, def, self.carrying_capacity(i)) - carried.count as i32;
        if left > 0 && space > 0 && !self.pawns[i].target_queue.is_empty() {
            let queue = self.pawns[i].target_queue.clone();
            for (n, q) in queue.iter().enumerate() {
                let usable = self.map.item(*q).is_some_and(|it| {
                    self.reservations.can_reserve(
                        claimant,
                        Target::Item(*q),
                        it.stack_count as i32,
                        1,
                        STACK_ALL,
                    )
                });
                if !usable {
                    self.end_job(i, false);
                    return;
                }
                if self.map.item(*q).is_some_and(|it| it.def == def) {
                    self.pawns[i].target_queue.remove(n);
                    let q = *q;
                    self.set_container_job(i, |src, _, _, _, stage| {
                        *src = q;
                        *stage = ContainerStage::GotoSource;
                    });
                    let at = self.map.item(q).map(|it| it.position);
                    match at {
                        Some(c) if self.pawns[i].next_stop() == c => self.container_pick_up(i),
                        Some(c) if self.walk_to(i, c, true) => {}
                        _ => self.end_job(i, false),
                    }
                    return;
                }
            }
        }
        self.container_carry(i);
    }

    /// Carry to the container (`GotoBuild` B).
    fn container_carry(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::HaulToContainer { container, .. },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        self.set_container_job(i, |_, _, _, _, stage| {
            *stage = ContainerStage::CarryToContainer;
        });
        if !self.goto_build(i, container) {
            self.end_job(i, false);
            return;
        }
        if !self.pawns[i].is_moving() {
            self.container_arrived(i);
        }
    }

    /// Haul-to-container: next to the container. Move off a blueprint,
    /// turn a blueprint into a frame, deposit, then go on to the next
    /// queued container or finish.
    // COMPATIBILITY TODO: currently approximate — the zero-duration wait
    // toil and the deposit run on the arrival tick; not runtime-verified.
    pub(super) fn container_arrived(&mut self, i: usize) {
        let Some(Job {
            kind: JobKind::HaulToContainer {
                container, primary, ..
            },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(k) = self.map.constructible(container).cloned() else {
            self.end_job(i, false);
            return;
        };
        // `MoveOffTargetBlueprint`.
        let fp = k.footprint();
        if k.stage == ConstructStage::Blueprint && fp.contains(self.pawns[i].position) {
            match self.touch_spot(i, &fp) {
                Some(spot) if !fp.contains(spot) && self.walk_to(i, spot, false) => return,
                _ => {
                    self.end_job(i, false);
                    return;
                }
            }
        }
        // `MakeSolidThingFromBlueprintIfNecessary`.
        let k = if k.stage == ConstructStage::Blueprint {
            if self.blocked(i, &k) {
                self.end_job(i, false);
                return;
            }
            let frame = self.replace_with_frame(&k);
            let claimant = self.claimant(i);
            let job_id = self.pawns[i].job_id;
            self.reservations.reserve(
                claimant,
                job_id,
                Target::Item(frame.id),
                1,
                FRAME_DELIVERY_RESERVERS,
                1,
            );
            frame
        } else {
            k
        };
        // `DepositHauledThingInContainer`.
        let Some(mut carried) = self.pawns[i].carried else {
            self.end_job(i, false);
            return;
        };
        let pawn = Some(self.pawns[i].id);
        let mut num = (carried.count as i32).min(space_remaining_with_enroute(
            &self.defs,
            &self.enroute,
            &k,
            carried.def,
            pawn,
        ));
        let primary = match self.pawns[i].job.as_ref().map(|j| j.kind) {
            Some(JobKind::HaulToContainer { primary, .. }) => primary,
            _ => primary,
        };
        if let Some(c) = primary
            .filter(|&c| c != k.id)
            .and_then(|c| self.map.constructible(c))
        {
            let keep =
                space_remaining_with_enroute(&self.defs, &self.enroute, c, carried.def, pawn);
            num = num.min(carried.count as i32 - keep);
        }
        let num = num.max(0) as u32;
        if num > 0 {
            if let Some(f) = self.map.constructible_mut(k.id) {
                f.add_resource(carried.def, num);
            }
            self.enroute.release_for(k.id, self.pawns[i].id);
            carried.count -= num;
        }
        self.pawns[i].carried = (carried.count > 0).then_some(carried);
        // `JumpToCarryToNextContainerIfPossible`.
        if self.pawns[i].carried.is_some()
            && !self.pawns[i].target_queue_b.is_empty()
            && let Some(next) = self.next_destination(i, primary, carried.def)
        {
            self.pawns[i].target_queue_b.retain(|&b| b != next);
            self.set_container_job(i, |_, b, c, _, _| {
                *b = next;
                *c = Some(next);
            });
            self.container_carry(i);
            return;
        }
        self.end_job(i, true);
    }

    /// `Toils_Haul.TryGetNextDestinationFromQueue`: the nearest queued
    /// container still needing the def; containers other than the primary
    /// only if the pawn has more than the primary needs.
    fn next_destination(
        &self,
        i: usize,
        primary: Option<ItemId>,
        def: DefId<ThingDef>,
    ) -> Option<ItemId> {
        let carried = self.pawns[i].carried?;
        let pawn = Some(self.pawns[i].id);
        let spare = match primary.and_then(|p| self.map.constructible(p)) {
            Some(p) => {
                carried.count as i32
                    > space_remaining_with_enroute(&self.defs, &self.enroute, p, def, pawn)
            }
            None => carried.count > 0,
        };
        let at = self.pawns[i].position;
        self.pawns[i]
            .target_queue_b
            .iter()
            .filter_map(|&b| self.map.constructible(b))
            .filter(|k| {
                space_remaining_with_enroute(&self.defs, &self.enroute, k, def, pawn) > 0
                    && (Some(k.id) == primary || spare)
            })
            .min_by_key(|k| (k.position.x - at.x).pow(2) + (k.position.z - at.z).pow(2))
            .map(|k| k.id)
    }

    /// `Blueprint.TryReplaceWithSolidThing` for a building: the blueprint
    /// becomes a frame (new identity; deliveries on their way and other
    /// pawns' jobs follow it).
    fn replace_with_frame(&mut self, blueprint: &Constructible) -> Constructible {
        self.map.remove_constructible(blueprint.id);
        self.reservations
            .release_all_for_target(Target::Item(blueprint.id));
        let id = self.map.allocate_item_id();
        let frame = Constructible {
            id,
            stage: ConstructStage::Frame,
            resources: Vec::new(),
            work_done: 0.0,
            ..blueprint.clone()
        };
        self.map.spawn_constructible(frame.clone());
        // A frame is a building of the colony: the home area grows.
        self.mark_home_around(frame.footprint());
        self.enroute.transfer(blueprint.id, id);
        // `TryReplaceWithFrame` in other haulers' fail checks.
        let old = blueprint.id;
        for p in &mut self.pawns {
            if let Some(Job {
                kind:
                    JobKind::HaulToContainer {
                        container, primary, ..
                    },
                ..
            }) = &mut p.job
            {
                if *container == old {
                    *container = id;
                }
                if *primary == Some(old) {
                    *primary = Some(id);
                }
            }
            for b in &mut p.target_queue_b {
                if *b == old {
                    *b = id;
                }
            }
        }
        self.refresh_path_grid();
        frame
    }

    /// The build toil's interval work (`JobDriver_ConstructFinishFrame`).
    // COMPATIBILITY TODO: currently approximate — construction XP
    // (0.25 × delta) is not learned (skills do not progress yet).
    pub(super) fn build_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::FinishFrame {
                    frame,
                    stage: BuildStage::Build { .. },
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(k) = self.map.constructible(frame).cloned() else {
            return;
        };
        // `FailOn(!CanConstruct)`.
        // COMPATIBILITY TODO: currently approximate — only blocking things
        // are re-checked.
        if self.blocked(i, &k) {
            self.end_job(i, false);
            return;
        }
        // Construction experience while the frame holds materials.
        if !k.resources.is_empty() {
            self.learn(i, "Construction", 0.25 * delta as f32);
        }
        let defs = self.defs.clone();
        let mut work =
            self.pawn_stat_of(i, "ConstructionSpeed") * WORK_PER_SPEED_TICK * delta as f32;
        if let Some(stuff) = k.stuff {
            work *= def_stat(&defs, &defs.things[stuff], None, "ConstructionSpeedFactor");
        }
        let stuff = k.stuff.map(|s| &defs.things[s]);
        let work_to_build = match k.building {
            Buildable::Thing(b) => def_stat(&defs, &defs.things[b], stuff, "WorkToBuild"),
            Buildable::Floor(f) => terrain_stat(&defs, &defs.terrain[f], "WorkToBuild"),
        };
        if self.pawns[i].is_colonist {
            let chance = self.pawn_stat_of(i, "ConstructSuccessChance");
            if self.rng.value() < 1.0 - chance.powf(work / work_to_build) {
                self.fail_construction(&k);
                self.end_job(i, true);
                return;
            }
        }
        let done = {
            let Some(f) = self.map.constructible_mut(frame) else {
                return;
            };
            f.work_done += work;
            f.work_done >= work_to_build
        };
        if done {
            self.complete_construction(frame);
            self.end_job(i, true);
        }
    }

    /// `Frame.CompleteConstruction`: the frame and its materials go; the
    /// building appears.
    // COMPATIBILITY TODO: currently approximate — hit points, quality, art
    // and the building's comps are not modelled.
    fn complete_construction(&mut self, frame: ItemId) {
        let Some(k) = self.map.remove_constructible(frame) else {
            return;
        };
        self.enroute.remove_container(frame);
        self.reservations
            .release_all_for_target(Target::Item(frame));
        match k.building {
            Buildable::Thing(b) => {
                self.spawn_building(b, k.stuff, k.footprint());
                self.notify_colony_building(b, k.footprint());
            }
            Buildable::Floor(f) => self.lay_floor(f, k.position),
        }
    }

    /// A floor frame completes (`TerrainGrid.SetTerrain` and
    /// `DoTerrainChangedEffects`): the terrain changes, plants that cannot
    /// grow on it die, and the cell's filth goes.
    pub(super) fn lay_floor(&mut self, floor: DefId<TerrainDef>, c: Cell) {
        let defs = self.defs.clone();
        self.map.set_terrain_layered(&defs, c, floor);
        let fertility = self.defs.terrain[floor].fertility;
        if let Some(p) = self.map.plant_at(c).map(|p| (p.id, p.def))
            && self.defs.things[p.1]
                .plant
                .as_ref()
                .is_some_and(|pp| !pp.completely_ignore_fertility && fertility < pp.fertility_min)
        {
            self.map.remove_plant(p.0);
            self.reservations.release_all_for_target(Target::Item(p.0));
        }
        let filth: Vec<ItemId> = self
            .map
            .items_at(c)
            .filter(|i| i.is_filth())
            .map(|i| i.id)
            .collect();
        for f in filth {
            self.map.take_from_item(f, u32::MAX);
            self.reservations.release_all_for_target(Target::Item(f));
        }
        self.refresh_path_grid();
    }

    /// Footprint of a blueprint, frame or building.
    pub(super) fn footprint_of(&self, id: ItemId) -> Option<Footprint> {
        self.map
            .constructible(id)
            .map(|k| k.footprint())
            .or_else(|| self.map.structure(id).map(|s| s.footprint))
    }

    /// Spawns a finished building (registering door state) and refreshes
    /// paths, regions and pawns standing in its way.
    fn spawn_building(
        &mut self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        fp: Footprint,
    ) -> ItemId {
        let edifice = self.defs.things[building].is_edifice();
        let id = self.map.spawn_structure_with(building, stuff, fp, edifice);
        self.power_spawned(id);
        let cell = fp.center;
        let def = &self.defs.things[building];
        // `CompRefuelable.Initialize`: the starting fuel.
        if let Some(r) = &def.refuelable
            && let Some(s) = self.map.structure_mut(id)
        {
            s.fuel = r.capacity * r.initial_fuel_percent;
        }
        if def.is_door() {
            // `TicksToOpenNow` and `CloseDelayAdjusted` for an unpowered door.
            let stuff = stuff.map(|s| &self.defs.things[s]);
            let speed = def_stat(&self.defs, def, stuff, "DoorOpenSpeed");
            let factors = def.building.as_ref();
            let open_factor = factors.map_or(1.0, |b| b.unpowered_door_open_speed_factor);
            let close_factor = factors.map_or(1.0, |b| b.unpowered_door_close_speed_factor);
            let ticks_to_open = (45.0 / speed * open_factor).round_ties_even() as i32;
            let close_delay =
                (crate::map::DOOR_CLOSE_DELAY_TICKS as f32 * close_factor).floor() as i32;
            self.map.add_door(cell, ticks_to_open, close_delay);
        }
        self.refresh_path_grid();
        // A door changes region types without changing walkability.
        self.rebuild_regions();
        for c in fp.cells() {
            self.evict_pawns(c);
        }
        id
    }

    /// Debug/scenario tool: a finished building appears at once (as the
    /// game's god-mode placement does).
    pub fn debug_spawn_building(
        &mut self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        cell: Cell,
    ) -> ItemId {
        self.debug_spawn_building_rotated(building, stuff, cell, Rot4::North)
    }

    /// [`Sim::debug_spawn_building`] with a rotation.
    pub fn debug_spawn_building_rotated(
        &mut self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        cell: Cell,
        rot: Rot4,
    ) -> ItemId {
        let fp = Footprint {
            center: cell,
            rot,
            size: self.defs.things[building].size,
        };
        for c in fp.cells() {
            if let Some(id) = self.map.constructible_at(c).map(|k| k.id) {
                self.cancel_constructible(id);
            }
        }
        let id = self.spawn_building(building, stuff, fp);
        self.notify_colony_building(building, fp);
        id
    }

    /// `AutoHomeAreaMaker.Notify_BuildingSpawned` for a colony building:
    /// the home area grows around it unless its def says not to.
    fn notify_colony_building(&mut self, building: DefId<ThingDef>, fp: Footprint) {
        if self.defs.things[building]
            .building
            .as_ref()
            .is_none_or(|b| b.expand_home_area)
        {
            self.mark_home_around(fp);
        }
    }

    // STATIC VERIFIED, checked by the `home` probe: 9×9 around a wall
    // frame or wall, 9×10 around a north-facing bed at (80,80) (x 76–84,
    // z 75–84).
    /// `AutoHomeAreaMaker.MarkHomeAroundThing`: the rectangle from
    /// position − rotated size / 2 − 4, of rotated size + 8 each way.
    pub(super) fn mark_home_around(&mut self, fp: Footprint) {
        let (sx, sz) = match fp.rot {
            Rot4::East | Rot4::West => (fp.size.1, fp.size.0),
            _ => fp.size,
        };
        let (x0, z0) = (fp.center.x - sx / 2 - 4, fp.center.z - sz / 2 - 4);
        for x in x0..x0 + sx + 8 {
            for z in z0..z0 + sz + 8 {
                let c = Cell::new(x, z);
                if self.map.size().contains(c) {
                    self.map.set_home(c, true);
                }
            }
        }
    }

    /// `AutoHomeAreaMaker.Notify_ZoneCellAdded`: the 9×9 square centred on
    /// a new zone cell becomes home.
    pub(super) fn mark_home_around_zone_cell(&mut self, cell: Cell) {
        for dx in -4..=4 {
            for dz in -4..=4 {
                let c = Cell::new(cell.x + dx, cell.z + dz);
                if self.map.size().contains(c) {
                    self.map.set_home(c, true);
                }
            }
        }
    }

    /// The map's regions (rooms and connectivity).
    pub fn regions(&self) -> &crate::region::Regions {
        &self.regions
    }

    /// `Frame.FailConstruction`: the frame is destroyed, leaving about half
    /// its materials (`RoundRandom(count × 0.5)` per stack, at least one),
    /// and the blueprint comes back with the same stuff.
    // COMPATIBILITY TODO: currently approximate — leavings are placed with
    // our simplified "near" placement.
    fn fail_construction(&mut self, k: &Constructible) {
        self.map.remove_constructible(k.id);
        self.enroute.remove_container(k.id);
        self.reservations.release_all_for_target(Target::Item(k.id));
        for &(def, count) in &k.resources {
            let n = round_random(count as f32 * 0.5, &mut self.rng).max(1);
            self.place_near(def, n, k.position);
        }
        self.map.place_blueprint(k.building, k.stuff, k.footprint());
        self.refresh_path_grid();
    }

    /// Puts `count` of `def` on or near `at`.
    pub(super) fn place_near(&mut self, def: DefId<ThingDef>, count: u32, at: Cell) {
        let mut thing = Carried {
            id: self.map.allocate_item_id(),
            def,
            count,
            rot: 0.0,
            hit_points: None,
        };
        if !self.place_thing_near(&mut thing, at) {
            self.map.spawn_carried(&thing, at);
            self.refresh_path_grid();
        }
    }

    /// Pawns standing where an impassable building appeared step out to
    /// the nearest walkable cell.
    // COMPATIBILITY TODO: currently approximate — the game's recovery from
    // an unwalkable position is not reproduced.
    fn evict_pawns(&mut self, cell: Cell) {
        if self.path_grid.walkable(cell) {
            return;
        }
        for i in 0..self.pawns.len() {
            if self.pawns[i].position == cell
                && let Some(to) = self.map.nearest_walkable(&self.path_grid, cell)
            {
                let p = &mut self.pawns[i];
                p.position = to;
                p.step = None;
                p.path.clear();
                p.destination = None;
            }
        }
    }

    /// `GotoBuild`'s interval check: a pawn passing a spot from which it
    /// touches its target stops there (reserving its cell) and goes on.
    /// Returns whether it started the build toil.
    pub(super) fn goto_build_interval(&mut self, i: usize) -> bool {
        let (target, finish) = match self.pawns[i].job.as_ref().map(|j| j.kind) {
            Some(JobKind::HaulToContainer {
                container,
                stage: ContainerStage::CarryToContainer,
                ..
            }) => (container, false),
            Some(JobKind::FinishFrame {
                frame,
                stage: BuildStage::Goto,
            }) => (frame, true),
            _ => return false,
        };
        if !self.pawns[i].is_moving() {
            return false;
        }
        let Some(fp) = self.footprint_of(target) else {
            return false;
        };
        let at = self.pawns[i].position;
        let pawn_cells = self.pawn_cells();
        let touches = BuildView {
            defs: &self.defs,
            map: &self.map,
            grid: &self.path_grid,
            regions: &self.regions,
            pawn_cells: &pawn_cells,
            delivery_jobs: &[],
        }
        .touches(at, &fp);
        // Standing on a floor frame touches it too.
        let floor = self
            .map
            .constructible(target)
            .is_some_and(|k| k.building.floor().is_some());
        let touches = touches || (floor && fp.contains(at));
        let claimant = self.claimant(i);
        if (fp.contains(at) && !floor)
            || !touches
            || !self
                .reservations
                .can_reserve(claimant, Target::Cell(at), 1, 1, STACK_ALL)
        {
            return false;
        }
        let job_id = self.pawns[i].job_id;
        self.reservations
            .reserve(claimant, job_id, Target::Cell(at), 1, 1, STACK_ALL);
        let p = &mut self.pawns[i];
        p.step = None;
        p.path.clear();
        p.destination = None;
        if finish {
            self.start_building(i);
        } else {
            self.container_arrived(i);
        }
        finish
    }

    /// `JobDriver_PlaceNoCostFrame` starts: walk to touch the blueprint.
    pub(super) fn begin_place_no_cost_frame(&mut self, i: usize) -> bool {
        let Some(JobKind::PlaceNoCostFrame { blueprint, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(fp) = self.map.constructible(blueprint).map(|k| k.footprint()) else {
            return false;
        };
        match self.touch_spot(i, &fp) {
            Some(c) if c != self.pawns[i].next_stop() => self.walk_to(i, c, false),
            Some(_) => {
                self.place_frame_arrived(i);
                true
            }
            None => false,
        }
    }

    /// At the blueprint: step off it if standing on it
    /// (`MoveOffTargetBlueprint`), then it becomes its frame
    /// (`MakeSolidThingFromBlueprintIfNecessary`).
    pub(super) fn place_frame_arrived(&mut self, i: usize) {
        let Some(JobKind::PlaceNoCostFrame {
            blueprint,
            moving_off,
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(k) = self.map.constructible(blueprint).cloned() else {
            self.end_job(i, false);
            return;
        };
        let fp = k.footprint();
        if !moving_off && fp.contains(self.pawns[i].position) {
            let Some(c) = self.touch_spot(i, &fp).filter(|c| !fp.contains(*c)) else {
                self.end_job(i, false);
                return;
            };
            if let Some(Job {
                kind: JobKind::PlaceNoCostFrame { moving_off, .. },
                ..
            }) = &mut self.pawns[i].job
            {
                *moving_off = true;
            }
            if !self.walk_to(i, c, false) {
                self.end_job(i, false);
            }
            return;
        }
        self.replace_with_frame(&k);
        self.end_job(i, true);
    }

    /// `FinishFrame`: at the frame; the build toil starts.
    pub(super) fn start_building(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::FinishFrame { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = BuildStage::Build {
                ticks_left: BUILD_TOIL_TICKS,
            };
        }
    }

    /// Places a blueprint (the build designator); returns its id, or
    /// `None` if the cell cannot take it.
    // COMPATIBILITY TODO: currently approximate — the designator's
    // placement rules (`GenConstruct.CanPlaceBlueprintAt`: terrain support,
    // adjacency, interaction cells) are reduced to "free, walkable cell".
    pub fn place_blueprint(
        &mut self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        cell: Cell,
    ) -> Option<ItemId> {
        self.place_blueprint_rotated(building, stuff, cell, Rot4::North)
    }

    /// [`Sim::place_blueprint`] with a rotation (for buildings larger than
    /// one cell).
    pub fn place_blueprint_rotated(
        &mut self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        cell: Cell,
        rot: Rot4,
    ) -> Option<ItemId> {
        let def = &self.defs.things[building];
        let stuff_ok = match stuff {
            Some(s) => def.accepts_stuff(&self.defs.things[s]),
            None => !def.made_from_stuff(),
        };
        let fp = Footprint {
            center: cell,
            rot,
            size: def.size,
        };
        let free = fp.cells().all(|c| {
            self.map.size().contains(c)
                && self.path_grid.walkable(c)
                && self.map.buildings[c].is_none()
                && self.map.constructible_at(c).is_none()
        });
        if !stuff_ok || !free {
            return None;
        }
        // `Designator_Build.DesignateSingleCell`: a building with no work
        // to build (spots) appears finished at once.
        let stuff_def = stuff.map(|d| &self.defs.things[d]);
        if crate::stats::def_stat(&self.defs, def, stuff_def, "WorkToBuild") == 0.0 {
            return Some(self.debug_spawn_building_rotated(building, stuff, cell, rot));
        }
        Some(
            self.map
                .place_blueprint(Buildable::Thing(building), stuff, fp),
        )
    }

    /// Places a floor blueprint (`Designator_Build` for a floor TerrainDef):
    /// the ground must afford what the floor needs and not be that floor
    /// already.
    // COMPATIBILITY TODO: currently approximate — a cell holds one
    // blueprint or frame, so floors cannot be planned under buildings or
    // their blueprints; replacing an existing floor (removing it first) is
    // not modelled.
    pub fn place_floor_blueprint(
        &mut self,
        floor: DefId<TerrainDef>,
        cell: Cell,
    ) -> Option<ItemId> {
        if !self.map.size().contains(cell) {
            return None;
        }
        let here = self.map.terrain[cell];
        let def = &self.defs.terrain[floor];
        let affords = def
            .terrain_affordance_needed
            .as_ref()
            .is_none_or(|a| self.defs.terrain[here].affordances.contains(a));
        if here == floor
            || !affords
            || self.map.buildings[cell].is_some()
            || self.map.constructible_at(cell).is_some()
        {
            return None;
        }
        let fp = Footprint {
            center: cell,
            rot: Rot4::North,
            size: (1, 1),
        };
        Some(self.map.place_blueprint(Buildable::Floor(floor), None, fp))
    }

    /// Removes a blueprint or frame (cancelling it); a frame's materials are
    /// dropped on its cell.
    // COMPATIBILITY TODO: currently approximate — the game refunds a
    // cancelled frame's materials through its leavings rules.
    pub fn cancel_constructible(&mut self, id: ItemId) {
        let Some(k) = self.map.remove_constructible(id) else {
            return;
        };
        self.enroute.remove_container(id);
        self.reservations.release_all_for_target(Target::Item(id));
        for &(def, count) in &k.resources {
            self.place_near(def, count, k.position);
        }
        self.refresh_path_grid();
    }

    /// Re-runs the colonist's initial work assignment from its current
    /// skills (pawn generation assigns work after skills).
    pub fn initialize_work(&mut self, pawn: crate::pawn::PawnId) {
        if let Some(i) = self.index_of(pawn)
            && self.pawns[i].work.is_some()
        {
            let skills = self.pawns[i].skills.clone();
            self.pawns[i].work = Some(crate::work::WorkSettings::initialize_with_skills(
                &self.defs,
                &|_| false,
                &|k| skills.level(k),
            ));
        }
    }

    /// Sets a work priority by work type name (0 off, 1–4); returns
    /// whether it was set.
    pub fn set_work_priority(
        &mut self,
        pawn: crate::pawn::PawnId,
        work_type: &str,
        priority: i32,
    ) -> bool {
        let Some(t) = self.defs.work_types.id(work_type) else {
            return false;
        };
        match self
            .index_of(pawn)
            .and_then(|i| self.pawns[i].work.as_mut())
        {
            Some(w) => w.set(t, priority, false),
            None => false,
        }
    }

    /// A colonist's timetable (`Pawn_TimetableTracker.times`), hour by hour.
    pub fn timetable(&self, pawn: crate::pawn::PawnId) -> Vec<crate::rest::TimeAssignment> {
        self.pawn(pawn)
            .and_then(|p| p.timetable.clone())
            .unwrap_or_else(crate::rest::default_timetable)
    }

    /// `Pawn_TimetableTracker.SetAssignment`.
    pub fn set_time_assignment(
        &mut self,
        pawn: crate::pawn::PawnId,
        hour: usize,
        assignment: crate::rest::TimeAssignment,
    ) {
        let Some(i) = self.index_of(pawn) else { return };
        if hour >= 24 {
            return;
        }
        let p = &mut self.pawns[i];
        let t = p
            .timetable
            .get_or_insert_with(crate::rest::default_timetable);
        t[hour] = assignment;
    }

    /// Replaces a colonist's whole timetable (paste).
    pub fn set_timetable(
        &mut self,
        pawn: crate::pawn::PawnId,
        times: &[crate::rest::TimeAssignment],
    ) {
        if times.len() != 24 {
            return;
        }
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i].timetable = Some(times.to_vec());
        }
    }

    /// Fixes a pawn's stat value (replaying a recorded pawn).
    pub fn set_stat_override(&mut self, pawn: crate::pawn::PawnId, stat: &str, value: f32) {
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i].stat_overrides.insert(stat.to_owned(), value);
        }
    }

    /// A pawn's stat where it stands (the light on its cell and its health
    /// count), or a replay's recorded value.
    pub(super) fn pawn_stat_of(&self, i: usize, stat: &str) -> f32 {
        let p = &self.pawns[i];
        p.stat_overrides.get(stat).copied().unwrap_or_else(|| {
            let view = if p.health.hediffs.is_empty() {
                None
            } else {
                self.health_view(p.id)
            };
            let v = crate::stats::pawn_stat_lit(
                &self.defs,
                &self.defs.things[p.race],
                &p.skills,
                stat,
                &|c| view.as_ref().map_or(1.0, |v| v.capacity(c)),
                Some(self.map.ground_glow(p.position)),
            );
            v + crate::stats::gear_offset(&self.defs, &p.apparel, stat)
        })
    }

    /// `MaxHitPoints` of an item def (no stuff).
    pub fn max_hit_points(&self, def: DefId<ThingDef>) -> i32 {
        crate::stats::def_stat(&self.defs, &self.defs.things[def], None, "MaxHitPoints")
            .round_ties_even() as i32
    }

    /// A pawn's current value of `stat` (`GetStatValue`).
    pub fn pawn_stat(&self, pawn: crate::pawn::PawnId, stat: &str) -> Option<f32> {
        self.index_of(pawn).map(|i| self.pawn_stat_of(i, stat))
    }

    /// Puts on a piece of apparel (`Pawn_ApparelTracker.Wear`).
    // COMPATIBILITY TODO: currently approximate — layers and body part
    // groups are not checked, so nothing worn is replaced.
    pub fn wear_apparel(
        &mut self,
        pawn: crate::pawn::PawnId,
        def: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
    ) {
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i]
                .apparel
                .push(crate::pawn::WornApparel { def, stuff });
        }
    }

    /// `SkillRecord.Learn` for pawn `i` (scaled by GlobalLearningFactor).
    pub(super) fn learn(&mut self, i: usize, skill: &str, xp: f32) {
        if self.defs.things[self.pawns[i].race]
            .race
            .as_ref()
            .and_then(|r| r.intelligence.as_deref())
            != Some("Humanlike")
        {
            return;
        }
        let factor = self.pawn_stat_of(i, "GlobalLearningFactor");
        self.pawns[i].skills.learn(skill, xp, factor);
    }

    /// Sets a pawn's skill level (scenario setup).
    pub fn set_skill(&mut self, pawn: crate::pawn::PawnId, skill: &str, level: i32) {
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i].skills.set(skill, level);
        }
    }

    /// Debug tool: sets a pawn's experience toward the next level.
    pub fn debug_set_skill_xp(&mut self, pawn: crate::pawn::PawnId, skill: &str, xp: f32) {
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i].skills.set_xp(skill, xp);
        }
    }

    /// Debug tool: sets a pawn's passion for a skill.
    pub fn debug_set_passion(
        &mut self,
        pawn: crate::pawn::PawnId,
        skill: &str,
        passion: crate::stats::Passion,
    ) {
        if let Some(i) = self.index_of(pawn) {
            self.pawns[i].skills.set_passion(skill, passion);
        }
    }

    pub fn enroute(&self) -> &crate::construct::EnrouteManager {
        &self.enroute
    }
}

/// The per-tick part of the construction drivers.
pub(super) fn tick_construct(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let kind = pawn.job.as_ref()?.kind;
    Some(match kind {
        JobKind::HaulToContainer {
            source,
            container,
            stage,
            ..
        } => {
            if map.constructible(container).is_none() {
                return Some(JobEvent::Failed);
            }
            match stage {
                ContainerStage::GotoSource => {
                    if map.item(source).is_none() {
                        return Some(JobEvent::PatherError);
                    }
                    super::tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtContainerSource
                    }
                }
                ContainerStage::CarryToContainer => {
                    if pawn.carried.is_none() {
                        return Some(JobEvent::Failed);
                    }
                    super::tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtContainer
                    }
                }
            }
        }
        JobKind::FinishFrame { frame, stage } => {
            let Some(k) = map.constructible(frame) else {
                return Some(JobEvent::Failed);
            };
            match stage {
                BuildStage::Goto => {
                    super::tick_movement(pawn, grid, map, t);
                    if pawn.is_moving() {
                        JobEvent::None
                    } else {
                        JobEvent::ArrivedAtFrame
                    }
                }
                BuildStage::Build { ticks_left } => {
                    debug_assert!(k.stage == ConstructStage::Frame);
                    let left = ticks_left - 1;
                    if let Some(Job {
                        kind:
                            JobKind::FinishFrame {
                                stage: BuildStage::Build { ticks_left },
                                ..
                            },
                        ..
                    }) = &mut pawn.job
                    {
                        *ticks_left = left;
                    }
                    if left <= 0 {
                        JobEvent::Ended(true)
                    } else {
                        JobEvent::None
                    }
                }
            }
        }
        _ => return None,
    })
}

/// Whether a frame is ready for building (for UI and tests).
pub fn frame_ready(sim: &Sim, frame: ItemId) -> bool {
    sim.map
        .constructible(frame)
        .is_some_and(|k| k.stage == ConstructStage::Frame && is_completed(&sim.defs, k))
}
