//! Construction (docs/research.md §22): costs and enroute tracking of
//! blueprints and frames, the touch-spot search used by builders, and the
//! `ConstructDeliverResourcesToBlueprints` / `...ToFrames` and
//! `ConstructFinishFrames` work givers.
//!
//! The delivery job is the game's `HaulToContainer` (driven in `sim.rs`):
//! a resource stack is carried to the nearest needer, other stacks within 5
//! cells are queued for collection and other needers within 8 cells are
//! queued as further destinations.

use rimworld_defs::{DefId, GameDefs, JobDef, Passability, ThingDef};

use crate::clean::radial_pattern;
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::haul::max_carry;
use crate::job::{BuildStage, ContainerStage, Job, JobKind};
use crate::map::{Buildable, ConstructStage, Constructible, ItemId, Map};
use crate::path::LocomotionUrgency;
use crate::pawn::PawnId;
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{STACK_ALL, Target};
use crate::stats::{cost_list, terrain_cost_list};
use crate::work::{WorkContext, WorkGiver};

/// Cells around the first resource searched for more of it.
const NEARBY_RESOURCE_RADIUS: f32 = 5.0;
/// Cells around the constructible searched for other needers.
const NEARBY_NEEDER_RADIUS: f32 = 8.0;

/// Materials pawns are bringing to one container (`ThingCountTracker`
/// rows of the `EnrouteManager`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Enroute {
    pub container: ItemId,
    pub pawn: PawnId,
    pub def: DefId<ThingDef>,
    pub count: u32,
}

/// `EnrouteManager`: what is on its way to each blueprint or frame, so
/// several haulers do not bring more than is needed.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EnrouteManager {
    rows: Vec<Enroute>,
}

impl EnrouteManager {
    /// `AddEnroute` (a pawn's repeated entries for one container add up).
    pub fn add(&mut self, container: ItemId, pawn: PawnId, def: DefId<ThingDef>, count: u32) {
        match self
            .rows
            .iter_mut()
            .find(|r| r.container == container && r.pawn == pawn && r.def == def)
        {
            Some(r) => r.count += count,
            None => self.rows.push(Enroute {
                container,
                pawn,
                def,
                count,
            }),
        }
    }

    /// `GetEnroute`: the amount of `def` on its way, optionally not
    /// counting one pawn's.
    pub fn get(&self, container: ItemId, def: DefId<ThingDef>, exclude: Option<PawnId>) -> u32 {
        self.rows
            .iter()
            .filter(|r| r.container == container && r.def == def && Some(r.pawn) != exclude)
            .map(|r| r.count)
            .sum()
    }

    /// `ReleaseFor`.
    pub fn release_for(&mut self, container: ItemId, pawn: PawnId) {
        self.rows
            .retain(|r| !(r.container == container && r.pawn == pawn));
    }

    /// `ReleaseAllClaimedBy` (job cleanup).
    pub fn release_all_claimed_by(&mut self, pawn: PawnId) {
        self.rows.retain(|r| r.pawn != pawn);
    }

    /// `SendReservations` followed by the old container despawning: a
    /// blueprint replaced by its frame passes its rows on.
    pub fn transfer(&mut self, from: ItemId, to: ItemId) {
        for r in &mut self.rows {
            if r.container == from {
                r.container = to;
            }
        }
    }

    /// `Notify_ContainerDespawned`.
    pub fn remove_container(&mut self, container: ItemId) {
        self.rows.retain(|r| r.container != container);
    }

    pub fn rows(&self) -> &[Enroute] {
        &self.rows
    }
}

/// `TotalMaterialCost` (`CostListAdjusted` of the building and stuff).
pub fn total_cost(defs: &GameDefs, k: &Constructible) -> Vec<(DefId<ThingDef>, u32)> {
    match k.building {
        Buildable::Thing(b) => cost_list(defs, &defs.things[b], k.stuff),
        Buildable::Floor(f) => terrain_cost_list(defs, &defs.terrain[f]),
    }
}

/// `constructionSkillPrerequisite` of what is built.
pub fn skill_prerequisite(defs: &GameDefs, building: Buildable) -> i32 {
    match building {
        Buildable::Thing(b) => defs.things[b].construction_skill_prerequisite,
        Buildable::Floor(f) => defs.terrain[f].construction_skill_prerequisite,
    }
}

/// `ThingCountNeeded`: a blueprint needs its whole cost; a frame what has
/// not been delivered yet.
pub fn count_needed(defs: &GameDefs, k: &Constructible, def: DefId<ThingDef>) -> i32 {
    total_cost(defs, k)
        .into_iter()
        .find(|(d, _)| *d == def)
        .map_or(0, |(_, n)| n as i32 - k.delivered(def) as i32)
}

/// `GetSpaceRemainingWithEnroute`: what is still needed once other pawns'
/// deliveries arrive.
pub fn space_remaining_with_enroute(
    defs: &GameDefs,
    enroute: &EnrouteManager,
    k: &Constructible,
    def: DefId<ThingDef>,
    exclude: Option<PawnId>,
) -> i32 {
    let needed = count_needed(defs, k, def);
    (needed - enroute.get(k.id, def, exclude) as i32).clamp(0, needed.max(0))
}

/// `Frame.IsCompleted`: every material delivered.
pub fn is_completed(defs: &GameDefs, k: &Constructible) -> bool {
    total_cost(defs, k)
        .iter()
        .all(|&(d, n)| k.delivered(d) >= n)
}

/// `ItemAvailability.ThingsAvailableAnywhere`: the map's stacks of `def`
/// add up to at least `count`.
pub fn things_available_anywhere(map: &Map, def: DefId<ThingDef>, count: i32) -> bool {
    let total: i64 = map
        .items()
        .iter()
        .filter(|i| i.def == def && !i.is_filth() && !i.forbidden)
        .map(|i| i.stack_count as i64)
        .sum();
    total >= count as i64
}

/// What a builder's search needs about the world.
pub struct BuildView<'a> {
    pub defs: &'a GameDefs,
    pub map: &'a Map,
    pub grid: &'a crate::path::PathGrid,
    pub regions: &'a Regions,
    /// Every spawned pawn's position (pawns block construction).
    pub pawn_cells: &'a [(PawnId, Cell)],
    /// Ids of the jobs that are `HaulToContainer` deliveries.
    pub delivery_jobs: &'a [crate::reservation::JobId],
}

impl BuildView<'_> {
    pub fn standable(&self, c: Cell) -> bool {
        self.map.size().contains(c)
            && self.grid.walkable(c)
            && self.map.standable_things(self.defs, c)
    }

    /// Reachability by region connectivity.
    // COMPATIBILITY TODO: currently approximate — danger is not modelled
    // and every pawn opens every door, so connected regions are reachable.
    pub fn reachable(&self, from: Cell, to: Cell) -> bool {
        from == to || self.regions.connected(from, to)
    }

    /// `ReachabilityImmediate.CanReachImmediate(cell, target, Touch)`: next
    /// to the target's footprint (not inside it).
    // COMPATIBILITY TODO: currently approximate — the corner rule for
    // diagonal touching is simplified to "not both side cells blocked".
    pub fn touches(&self, from: Cell, target: &Footprint) -> bool {
        if target.distance(from) != 1 {
            return false;
        }
        let near = target.nearest_cell(from);
        let (dx, dz) = (near.x - from.x, near.z - from.z);
        if dx == 0 || dz == 0 {
            return true;
        }
        let a = Cell::new(from.x + dx, from.z);
        let b = Cell::new(from.x, from.z + dz);
        self.grid.walkable(a) || self.grid.walkable(b)
    }

    /// `RCellFinder.TryFindGoodAdjacentSpotToTouch`: the adjacent cell
    /// nearest the pawn (its own cell wins) from which it can stand and
    /// touch the target; otherwise a random reachable walkable adjacent
    /// cell (`ok = false` when there is none).
    pub fn touch_spot(
        &self,
        pawn_at: Cell,
        target: &Footprint,
        rng: &mut Rand,
    ) -> (Option<Cell>, bool) {
        let mut best: Option<Cell> = None;
        let mut best_d = i32::MAX;
        let ring = target.adjacent_8_way();
        for &c in &ring {
            if !self.standable(c) || !self.reachable(pawn_at, c) || !self.touches(c, target) {
                continue;
            }
            if c == pawn_at {
                return (Some(c), true);
            }
            let d = (c.x - pawn_at.x).pow(2) + (c.z - pawn_at.z).pow(2);
            // COMPATIBILITY TODO: currently approximate — the preference
            // for cells without `avoidWander` terrain or traps is not
            // modelled.
            if d < best_d {
                best_d = d;
                best = Some(c);
            }
        }
        if best.is_some() {
            return (best, true);
        }
        let mut cells = ring;
        rng.shuffle(&mut cells);
        let fallback = cells.into_iter().find(|&c| {
            self.map.size().contains(c) && self.grid.walkable(c) && self.reachable(pawn_at, c)
        });
        (fallback, fallback.is_some())
    }

    /// Items block a building that cannot be stood on, or one that wants
    /// its site cleared (`forceMoveItemsBeforeConstruction`).
    // COMPATIBILITY TODO: currently approximate — floors'
    // `forceMoveItemsBeforeConstruction` is not read (no Core floor sets
    // it).
    fn items_block(&self, k: &Constructible) -> bool {
        let Buildable::Thing(b) = k.building else {
            return false;
        };
        let building = &self.defs.things[b];
        building.passability != Passability::Standable
            || building.force_move_items_before_construction
    }

    /// `GenConstruct.FirstBlockingThing` (with `BlocksConstruction`) over
    /// the footprint: items (see `items_block`), pawns other than the worker,
    /// and existing buildings block.
    // COMPATIBILITY TODO: currently approximate — plants, edifice
    // replacement rules and `clearBuildingArea` details are not modelled.
    pub fn blocked(&self, k: &Constructible, worker: Option<PawnId>) -> bool {
        let items_block = self.items_block(k);
        if let Buildable::Floor(_) = k.building {
            // A floor's blueprint does not clear its area
            // (`clearBuildingArea` false): only large plants block it.
            return k
                .footprint()
                .cells()
                .any(|c| !self.map.size().contains(c) || self.blocking_plant_at(c).is_some());
        }
        k.footprint().cells().any(|c| {
            !self.map.size().contains(c)
                || self.map.buildings[c].is_some()
                || self.blocking_plant_at(c).is_some()
                || (items_block && self.map.items_at(c).any(|i| !i.is_filth()))
                || self
                    .pawn_cells
                    .iter()
                    .any(|&(p, at)| at == c && Some(p) != worker)
        })
    }
}

impl BuildView<'_> {
    /// A plant that blocks construction on `c` (`BlocksConstruction`:
    /// plants with more harvest work than a dandelion).
    pub fn blocking_plant_at(&self, c: Cell) -> Option<ItemId> {
        let dandelion = self
            .defs
            .things
            .get("Plant_Dandelion")
            .and_then(|d| d.plant.as_ref())
            .map_or(10.0, |p| p.harvest_work);
        self.map
            .plant_at(c)
            .filter(|p| {
                self.defs.things[p.def]
                    .plant
                    .as_ref()
                    .is_some_and(|pp| pp.harvest_work > dandelion)
            })
            .map(|p| p.id)
    }

    /// The item blocking a constructible, if the blocker is an item
    /// (`FirstBlockingThing` restricted to what `HandleBlockingThingJob`
    /// can deal with here).
    fn blocking_item(&self, k: &Constructible) -> Option<ItemId> {
        if !self.items_block(k) {
            return None;
        }
        k.footprint()
            .cells()
            .find_map(|c| self.map.items_at(c).find(|i| !i.is_filth()).map(|i| i.id))
    }

    /// `HaulPlaceBlockerIn(thing, c, checkBlueprintsAndFrames: true)` is
    /// empty: no blueprint or frame, and items only if `def` stacks onto
    /// them with room for `count`.
    fn haul_place_free(&self, c: Cell, def: DefId<ThingDef>, count: u32) -> bool {
        if self.map.constructible_at(c).is_some() {
            return false;
        }
        self.map.items_at(c).filter(|i| !i.is_filth()).all(|i| {
            i.def == def
                && self.defs.things[def].category.as_deref() == Some("Item")
                && self.defs.things[def].stack_limit - (i.stack_count as i32) >= count as i32
        })
    }

    /// `HaulAIUtility.HaulAsideJobFor`: carry the item to the closest
    /// acceptable cell, searching region by region from its position
    /// (`TryFindSpotToPlaceHaulableCloseTo`).
    // COMPATIBILITY TODO: currently approximate — the game sorts each
    // region's cells with an unstable sort (ties may differ); fire, growing
    // zones, mining designations and traps are not modelled.
    pub fn haul_aside_job(
        &self,
        ctx: &WorkContext<'_>,
        haul_job: Option<DefId<JobDef>>,
        t: ItemId,
    ) -> Option<(Job, Vec<ItemId>)> {
        let item = ctx.map.item(t)?;
        if !self.defs.things[item.def].ever_haulable()
            || !ctx.reservations.can_reserve(
                ctx.claimant,
                Target::Item(t),
                item.stack_count as i32,
                1,
                STACK_ALL,
            )
            || !self.reachable(ctx.position, item.position)
        {
            return None;
        }
        let center = item.position;
        let root = self.regions.region_at(center)?;
        let mut found = None;
        self.regions.traverse(
            root,
            |_, _| true,
            |r| {
                let mut cells: Vec<Cell> = self.regions.cells(r).collect();
                cells.sort_by_key(|c| (c.x - center.x).pow(2) + (c.z - center.z).pow(2));
                found = cells.into_iter().find(|&c| {
                    c != center
                        && ctx.reservations.can_reserve(
                            ctx.claimant,
                            Target::Cell(c),
                            1,
                            1,
                            STACK_ALL,
                        )
                        && self.reachable(ctx.position, c)
                        && self.haul_place_free(c, item.def, item.stack_count)
                        && self.standable(c)
                });
                found.is_some()
            },
            100,
        );
        let dest = found?;
        Some((
            Job {
                def: haul_job,
                kind: JobKind::Haul {
                    source: t,
                    dest,
                    count: 99_999,
                    stage: crate::job::HaulStage::GotoSource,
                    start_tick: 0,
                    aside: true,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }

    /// `GenConstruct.HandleBlockingThingJob` for an item blocker.
    // COMPATIBILITY TODO: currently approximate — plants (cutting) and
    // buildings (deconstruction) are not handled.
    fn handle_blocking(
        &self,
        ctx: &WorkContext<'_>,
        haul_job: Option<DefId<JobDef>>,
        k: &Constructible,
    ) -> Option<(Job, Vec<ItemId>)> {
        // A plant in the way is cut (`CutPlant`).
        if let Some(p) = k
            .footprint()
            .cells()
            .find_map(|c| self.blocking_plant_at(c))
        {
            let pos = self.map.plant(p)?.position;
            if !ctx
                .reservations
                .can_reserve(ctx.claimant, Target::Item(p), 1, 1, STACK_ALL)
                || !self.reachable(ctx.position, pos)
            {
                return None;
            }
            let cut = self.defs.jobs.id("CutPlant");
            return Some(crate::farm::cut_job(cut, p));
        }
        let t = self.blocking_item(k)?;
        self.haul_aside_job(ctx, haul_job, t)
    }
}

/// The pawn doing construction work.
#[derive(Debug, Clone, Copy)]
pub struct Builder {
    pub pawn: PawnId,
    /// Construction skill level.
    pub construction: i32,
    pub carrying_capacity: f32,
}

/// `GenConstruct.CanConstruct`. `delivery` stands for a
/// `jobForReservation` of `HaulToContainer`: then every reservation of the
/// constructible must belong to a delivery job
/// (`OnlyReservationsForJobDef`); otherwise the pawn must be able to
/// reserve it.
pub fn can_construct(
    ctx: &WorkContext<'_>,
    view: &BuildView<'_>,
    builder: Builder,
    k: &Constructible,
    check_skills: bool,
    delivery: bool,
) -> bool {
    if view.blocked(k, Some(builder.pawn)) {
        return false;
    }
    if !view
        .touch_spot(ctx.position, &k.footprint(), &mut ctx.rng.borrow_mut())
        .1
    {
        return false;
    }
    let target = Target::Item(k.id);
    let reservable = if delivery {
        ctx.reservations
            .rows()
            .iter()
            .filter(|r| r.target == target)
            .all(|r| view.delivery_jobs.contains(&r.job))
    } else {
        ctx.reservations
            .can_reserve(ctx.claimant, target, 1, 1, STACK_ALL)
    };
    if !reservable || !can_touch_from_reach(view, ctx.position, &k.footprint()) {
        return false;
    }
    // COMPATIBILITY TODO: currently approximate — the artistic skill
    // prerequisite and ideology/mech checks are not modelled.
    !check_skills || builder.construction >= skill_prerequisite(view.defs, k.building)
}

/// `CanReach(t, Touch)` for a thing whose own cell may not be standable.
fn can_touch_from_reach(view: &BuildView<'_>, from: Cell, target: &Footprint) -> bool {
    target
        .adjacent_8_way()
        .into_iter()
        .any(|c| view.grid.walkable(c) && view.reachable(from, c) && view.touches(c, target))
}

/// Things in the radial pattern around `center` (`RadialDistinctThingsAround`).
fn radial_cells(center: Cell, radius: f32, use_center: bool) -> impl Iterator<Item = Cell> {
    let r2 = radius * radius;
    radial_pattern()
        .iter()
        .take_while(move |d| ((d.x * d.x + d.z * d.z) as f32) <= r2)
        .skip(usize::from(!use_center))
        .map(move |&d| center + d)
}

/// Delivery work giver (`WorkGiver_ConstructDeliverResourcesToBlueprints`
/// and `...ToFrames`; used by both the Construction and the Hauling work
/// types).
pub struct DeliverResources<'a> {
    pub view: BuildView<'a>,
    pub enroute: &'a EnrouteManager,
    pub builder: Builder,
    pub frames: bool,
    /// The giver belongs to the Construction work type (skill checks).
    pub construction_work: bool,
    pub job: Option<DefId<JobDef>>,
    /// `HaulToCell`, for hauling blockers aside.
    pub haul_job: Option<DefId<JobDef>>,
    /// `PlaceNoCostFrame`, for blueprints that cost nothing.
    pub no_cost_job: Option<DefId<JobDef>>,
}

impl DeliverResources<'_> {
    fn stage(&self) -> ConstructStage {
        if self.frames {
            ConstructStage::Frame
        } else {
            ConstructStage::Blueprint
        }
    }

    /// `CanUseItemForWork`: reservable and reachable.
    fn can_use_item(&self, ctx: &WorkContext<'_>, id: ItemId) -> bool {
        ctx.map.item(id).is_some_and(|i| {
            ctx.reservations.can_reserve(
                ctx.claimant,
                Target::Item(id),
                i.stack_count as i32,
                1,
                STACK_ALL,
            ) && self.view.reachable(ctx.position, i.position)
        })
    }

    /// `ResourceDeliverJobFor`.
    fn deliver_job(&self, ctx: &WorkContext<'_>, k: &Constructible) -> Option<(Job, Vec<ItemId>)> {
        let defs = ctx.defs;
        let pawn = Some(self.builder.pawn);
        for (def, _) in total_cost(defs, k) {
            let needed = space_remaining_with_enroute(defs, self.enroute, k, def, pawn);
            if needed <= 0 {
                continue;
            }
            if !things_available_anywhere(ctx.map, def, needed) {
                return None;
            }
            // COMPATIBILITY TODO: currently approximate — a carried stack of
            // the resource (`CanUseCarriedResource`) is not used (pawns put
            // carried things down when a job ends), and the game searches
            // region by region (`ClosestThingReachable`).
            let found = ctx
                .map
                .items()
                .iter()
                .filter(|i| {
                    i.def == def
                        && !i.is_filth()
                        && !i.forbidden
                        && ctx.reservations.can_reserve(
                            ctx.claimant,
                            Target::Item(i.id),
                            i.stack_count as i32,
                            1,
                            STACK_ALL,
                        )
                })
                .filter(|i| self.view.reachable(ctx.position, i.position))
                .min_by_key(|i| {
                    (i.position.x - ctx.position.x).pow(2) + (i.position.z - ctx.position.z).pow(2)
                })?;
            // `FindAvailableNearbyResources`.
            let max = max_carry(defs, def, self.builder.carrying_capacity);
            let mut resources = vec![(found.id, found.stack_count as i32)];
            let mut res_total = found.stack_count as i32;
            if res_total < max {
                for c in radial_cells(found.position, NEARBY_RESOURCE_RADIUS, false) {
                    if !ctx.map.size().contains(c) {
                        continue;
                    }
                    let mut full = false;
                    for item in ctx.map.items_at(c) {
                        if res_total >= max {
                            res_total = max;
                            full = true;
                            break;
                        }
                        if item.def == def && self.can_use_item(ctx, item.id) {
                            resources.push((item.id, item.stack_count as i32));
                            res_total += item.stack_count as i32;
                        }
                    }
                    if full {
                        break;
                    }
                }
            }
            res_total = res_total.min(max);
            // `FindNearbyNeeders`.
            let mut needers: Vec<(ItemId, Cell)> = Vec::new();
            let mut needed_total = needed;
            'cells: for c in radial_cells(k.position, NEARBY_NEEDER_RADIUS, true) {
                for other in ctx
                    .map
                    .constructibles()
                    .iter()
                    .filter(|o| o.footprint().contains(c))
                {
                    if needed_total >= res_total {
                        break 'cells;
                    }
                    if other.id == k.id
                        || needers.iter().any(|(id, _)| *id == other.id)
                        || !can_construct(ctx, &self.view, self.builder, other, false, true)
                    {
                        continue;
                    }
                    let n = space_remaining_with_enroute(defs, self.enroute, other, def, pawn);
                    if n > 0 {
                        needers.push((other.id, other.position));
                        needed_total += n;
                    }
                }
            }
            needers.push((k.id, k.position));
            let manhattan =
                |c: Cell| (c.x - found.position.x).abs() + (c.z - found.position.z).abs();
            let first_min = needers
                .iter()
                .enumerate()
                .min_by_key(|(i, (_, c))| (manhattan(*c), *i))
                .map(|(i, _)| i)?;
            let (dest, _) = needers.remove(first_min);
            // The job count: stacks are added until the need or the
            // carryable amount is covered; the stacks after that are not
            // queued.
            let mut count = 0;
            let mut used = 0;
            loop {
                count += resources[used].1;
                count = count.min(res_total.min(needed_total));
                used += 1;
                if !(count < needed_total && count < res_total && used < resources.len()) {
                    break;
                }
            }
            resources.truncate(used);
            let mut queue: Vec<ItemId> = resources
                .iter()
                .map(|(id, _)| *id)
                .filter(|&id| id != found.id)
                .collect();
            // Queue B (further destinations) travels in the same list; the
            // driver tells them apart (they are constructibles).
            queue.extend(needers.iter().map(|(id, _)| *id));
            return Some((
                Job {
                    def: self.job,
                    kind: JobKind::HaulToContainer {
                        source: found.id,
                        container: dest,
                        primary: Some(k.id),
                        count,
                        stage: ContainerStage::GotoSource,
                    },
                    forced: false,
                    urgency: LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                queue,
            ));
        }
        None
    }
}

impl WorkGiver for DeliverResources<'_> {
    /// Blueprints, frames and filth are stored in regions.
    fn region_request(&self) -> bool {
        true
    }

    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(
            ctx.map
                .constructibles()
                .iter()
                .filter(|k| k.stage == self.stage())
                .map(|k| k.id)
                .collect(),
        )
    }

    // COMPATIBILITY TODO: currently approximate — floor removal under the
    // blueprint is not modelled.
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let k = ctx.map.constructible(t)?;
        if k.stage != self.stage() {
            return None;
        }
        if !self
            .view
            .touch_spot(ctx.position, &k.footprint(), &mut ctx.rng.borrow_mut())
            .1
        {
            return None;
        }
        if self.view.blocked(k, Some(self.builder.pawn)) {
            return self.view.handle_blocking(ctx, self.haul_job, k);
        }
        if !can_construct(
            ctx,
            &self.view,
            self.builder,
            k,
            self.construction_work,
            true,
        ) {
            return None;
        }
        if let Some(job) = self.deliver_job(ctx, k) {
            return Some(job);
        }
        // `NoCostFrameMakeJobFor`: a blueprint with no material cost is
        // placed as its frame (not by haulers).
        if !self.frames && self.construction_work && total_cost(ctx.defs, k).is_empty() {
            return Some((
                Job {
                    def: self.no_cost_job,
                    kind: JobKind::PlaceNoCostFrame {
                        blueprint: k.id,
                        moving_off: false,
                    },
                    forced: false,
                    urgency: LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                Vec::new(),
            ));
        }
        None
    }
}

/// `WorkGiver_ConstructFinishFrames`.
pub struct FinishFrames<'a> {
    pub view: BuildView<'a>,
    pub builder: Builder,
    pub job: Option<DefId<JobDef>>,
    /// `HaulToCell`, for hauling blockers aside.
    pub haul_job: Option<DefId<JobDef>>,
}

impl WorkGiver for FinishFrames<'_> {
    /// Blueprints, frames and filth are stored in regions.
    fn region_request(&self) -> bool {
        true
    }

    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(
            ctx.map
                .constructibles()
                .iter()
                .filter(|k| k.stage == ConstructStage::Frame)
                .map(|k| k.id)
                .collect(),
        )
    }

    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let k = ctx.map.constructible(t)?;
        if k.stage != ConstructStage::Frame
            || !is_completed(ctx.defs, k)
            || !self
                .view
                .touch_spot(ctx.position, &k.footprint(), &mut ctx.rng.borrow_mut())
                .1
        {
            return None;
        }
        if self.view.blocked(k, Some(self.builder.pawn)) {
            return self.view.handle_blocking(ctx, self.haul_job, k);
        }
        if !can_construct(ctx, &self.view, self.builder, k, true, false) {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::FinishFrame {
                    frame: t,
                    stage: BuildStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_cells_respect_radius_and_centre() {
        let with: Vec<Cell> = radial_cells(Cell::new(0, 0), 1.5, true).collect();
        assert_eq!(with.len(), 9);
        assert_eq!(with[0], Cell::new(0, 0));
        let without: Vec<Cell> = radial_cells(Cell::new(0, 0), 1.5, false).collect();
        assert_eq!(without.len(), 8);
    }

    #[test]
    fn enroute_adds_and_releases() {
        let mut m = EnrouteManager::default();
        let def = DefId::from_index(0);
        m.add(ItemId(1), PawnId(0), def, 3);
        m.add(ItemId(1), PawnId(0), def, 2);
        m.add(ItemId(1), PawnId(1), def, 4);
        assert_eq!(m.get(ItemId(1), def, None), 9);
        assert_eq!(m.get(ItemId(1), def, Some(PawnId(0))), 4);
        m.transfer(ItemId(1), ItemId(2));
        assert_eq!(m.get(ItemId(2), def, None), 9);
        m.release_for(ItemId(2), PawnId(1));
        assert_eq!(m.get(ItemId(2), def, None), 5);
        m.release_all_claimed_by(PawnId(0));
        assert!(m.rows().is_empty());
    }
}
