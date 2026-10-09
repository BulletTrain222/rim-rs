//! Hauling to storage (docs/research.md §21): storage queries
//! (`StoreUtility`), haul job construction (`HaulAIUtility`) and the
//! `HaulGeneral` work giver.

use rimworld_defs::{DefId, GameDefs, ThingDef};

use crate::grid::Cell;
use crate::job::{HaulStage, Job, JobKind};
use crate::map::{ItemId, Map};
use crate::path::{LocomotionUrgency, PathGrid};
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{Claimant, ReservationManager, STACK_ALL, Target};
use crate::storage::{StoragePriority, ZoneId};
use crate::work::{WorkContext, WorkGiver};

/// Items an ordinary cell holds (`GetMaxItemsAllowedInCell` without
/// storage buildings).
pub const MAX_ITEMS_IN_CELL: usize = 1;
/// Job count when the destination has no stack of the item yet.
const UNLIMITED_COUNT: i32 = 99_999;
/// Placement waits until this long after the job started (`PossiblyDelay`).
pub const MIN_HAUL_TICKS: u64 = 30;
/// Range of the sampled prefix fraction in the accurate storage search.
const SAMPLE_FRACTION: (f32, f32) = (0.005, 0.018);

/// What a storage search needs about the map.
pub struct StoreView<'a> {
    pub defs: &'a GameDefs,
    pub map: &'a Map,
    pub grid: &'a PathGrid,
    pub regions: &'a Regions,
    pub reservations: &'a ReservationManager,
}

/// The thing being stored: its def and where it is (on the map, or carried
/// by `carrier` at the carrier's position).
#[derive(Debug, Clone, Copy)]
pub struct Storable {
    pub def: DefId<ThingDef>,
    /// Position of the thing if spawned, else of its carrier.
    pub position: Cell,
}

impl StoreView<'_> {
    /// `Thing.CanStackWith` for two items of these defs.
    // COMPATIBILITY TODO: currently approximate — stuff and comp
    // compatibility (quality, ingredients, rot) are not modelled; same def
    // stacks.
    fn can_stack(&self, a: DefId<ThingDef>, b: DefId<ThingDef>) -> bool {
        a == b && self.defs.things[a].category.as_deref() == Some("Item")
    }

    fn item_count(&self, c: Cell) -> usize {
        self.map.items_at(c).filter(|i| !i.is_filth()).count()
    }

    /// `StoreUtility.NoStorageBlockersIn`: a compatible stack with room
    /// accepts more; otherwise the cell needs a free item slot. Buildings
    /// that block standing block storage.
    pub fn no_storage_blockers_in(&self, c: Cell, def: DefId<ThingDef>) -> bool {
        if !self.map.standable_things(self.defs, c) {
            return false;
        }
        let partial = self.map.items_at(c).any(|i| {
            !i.is_filth()
                && self.defs.things[i.def].ever_storable()
                && self.can_stack(i.def, def)
                && (i.stack_count as i32) < self.defs.things[i.def].stack_limit
        });
        partial || self.item_count(c) < MAX_ITEMS_IN_CELL
    }

    /// `GetItemStackSpaceLeftFor`: room in same-def stacks plus free slots.
    pub fn stack_space_left(&self, c: Cell, def: DefId<ThingDef>) -> i32 {
        let limit = self.defs.things[def].stack_limit;
        let in_stacks: i32 = self
            .map
            .items_at(c)
            .filter(|i| i.def == def)
            .map(|i| (limit - i.stack_count as i32).max(0))
            .sum();
        let free = MAX_ITEMS_IN_CELL.saturating_sub(self.item_count(c)) as i32;
        in_stacks + free * limit
    }

    /// Reachability between two cells (same connected set of regions).
    // COMPATIBILITY TODO: currently approximate — path end modes and
    // danger are not modelled; every pawn can open every door, so
    // connected regions are mutually reachable.
    fn reachable(&self, a: Cell, b: Cell) -> bool {
        let room = |c: Cell| {
            self.regions.component_at(c).or_else(|| {
                Cell::NEIGHBORS_8
                    .iter()
                    .find_map(|&d| self.regions.component_at(c + d))
            })
        };
        room(a).is_some() && room(a) == room(b)
    }

    /// `StoreUtility.IsGoodStoreCell`. With a carrier the cell must be newly
    /// reservable by it and reachable from the thing; without one, no
    /// colonist may hold a reservation on it.
    // COMPATIBILITY TODO: currently approximate — forbidden cells, fire and
    // construction blockers do not exist yet.
    pub fn is_good_store_cell(&self, c: Cell, thing: Storable, carrier: Option<Claimant>) -> bool {
        if !self.no_storage_blockers_in(c, thing.def) {
            return false;
        }
        match carrier {
            Some(carrier) => {
                // `CanReserveNew`: not already reserved by this pawn.
                let own = self
                    .reservations
                    .rows()
                    .iter()
                    .any(|r| r.target == Target::Cell(c) && r.claimant.pawn == carrier.pawn);
                if own
                    || !self
                        .reservations
                        .can_reserve(carrier, Target::Cell(c), 1, 1, STACK_ALL)
                {
                    return false;
                }
                self.reachable(thing.position, c)
            }
            None => !self
                .reservations
                .rows()
                .iter()
                .any(|r| r.target == Target::Cell(c) && r.claimant.has_faction),
        }
    }

    /// `IsValidStorageFor`: no blockers and the cell's stockpile accepts it.
    pub fn is_valid_storage_for(&self, c: Cell, def: DefId<ThingDef>) -> bool {
        self.no_storage_blockers_in(c, def)
            && self
                .map
                .storage
                .zone_at(c)
                .is_some_and(|z| self.map.storage.zone(z).accepts(self.defs, def))
    }

    /// Storage priority a spawned item currently has (`CurrentStoragePriorityOf`):
    /// its stockpile's if that accepts it, else Unstored.
    pub fn current_priority(&self, item: ItemId) -> StoragePriority {
        let Some(i) = self.map.item(item) else {
            return StoragePriority::Unstored;
        };
        match self.map.storage.zone_at(i.position) {
            Some(z) if self.map.storage.zone(z).accepts(self.defs, i.def) => {
                self.map.storage.zone(z).priority
            }
            _ => StoragePriority::Unstored,
        }
    }

    /// `TryFindBestBetterStoreCellFor`. Zones are searched by descending
    /// priority and must be strictly better than `current`. In each
    /// accepting zone the cells are walked in list order: a good cell no
    /// farther than the best so far replaces it, and the walk stops at a
    /// success at index >= k, where k = floor(cells × U(0.005, 0.018)) in
    /// an accurate search (one random draw per accepting zone) and 0
    /// otherwise. Distance is measured from the thing.
    pub fn best_better_store_cell(
        &self,
        thing: Storable,
        carrier: Option<Claimant>,
        current: StoragePriority,
        accurate: bool,
        rng: &mut Rand,
    ) -> Option<Cell> {
        let storage = &self.map.storage;
        let mut found_priority = current;
        let mut closest = 2.147_483_6e9f32;
        let mut best: Option<Cell> = None;
        for &z in storage.in_priority_order() {
            let zone = storage.zone(z);
            if zone.priority < found_priority || zone.priority <= current {
                break;
            }
            if !zone.accepts(self.defs, thing.def) {
                continue;
            }
            let count = zone.cells.len();
            let k = if accurate {
                (count as f32 * rng.range_f32(SAMPLE_FRACTION.0, SAMPLE_FRACTION.1)).floor()
                    as usize
            } else {
                0
            };
            for (i, &c) in zone.cells.iter().enumerate() {
                let (dx, dz) = (thing.position.x - c.x, thing.position.z - c.z);
                let d = (dx * dx + dz * dz) as f32;
                if d > closest || !self.is_good_store_cell(c, thing, carrier) {
                    continue;
                }
                best = Some(c);
                closest = d;
                found_priority = zone.priority;
                if i >= k {
                    break;
                }
            }
        }
        best
    }

    /// `IsInValidBestStorage`: in an accepting stockpile with no strictly
    /// better place (inaccurate search, no carrier).
    pub fn is_in_valid_best_storage(&self, item: ItemId, rng: &mut Rand) -> bool {
        let Some(i) = self.map.item(item) else {
            return false;
        };
        let Some(z) = self.map.storage.zone_at(i.position) else {
            return false;
        };
        let zone = self.map.storage.zone(z);
        if !zone.accepts(self.defs, i.def) {
            return false;
        }
        let thing = Storable {
            def: i.def,
            position: i.position,
        };
        self.best_better_store_cell(thing, None, zone.priority, false, rng)
            .is_none()
    }

    /// Items needing hauling (`ListerHaulables`): always-haulable items not
    /// already in their valid best storage.
    // COMPATIBILITY TODO: currently approximate — the game keeps a HashSet
    // updated incrementally (with periodic re-checks), whose enumeration
    // order we replace by spawn order; forbidding and haul designations are
    // not modelled.
    pub fn haulables(&self, rng: &mut Rand) -> Vec<ItemId> {
        self.map
            .items()
            .iter()
            .filter(|i| !i.is_filth() && !i.forbidden && self.defs.things[i.def].always_haulable)
            .map(|i| i.id)
            .filter(|&id| !self.is_in_valid_best_storage(id, rng))
            .collect()
    }

    /// `HaulAIUtility.HaulToCellStorageJob` count: for a destination already
    /// holding the def, its stack limit, else unlimited; then the room of
    /// good cells of the zone summed in order until reaching that or the
    /// carrying capacity. Not clamped to the source or what can be carried.
    pub fn haul_count(
        &self,
        thing: Storable,
        carrier: Claimant,
        dest: Cell,
        carrying_capacity: f32,
    ) -> i32 {
        let limit = self.defs.things[thing.def].stack_limit;
        let wanted = if self.map.items_at(dest).any(|i| i.def == thing.def) {
            limit
        } else {
            UNLIMITED_COUNT
        };
        let Some(z) = self.map.storage.zone_at(dest) else {
            return wanted;
        };
        let mut room = 0;
        for &c in &self.map.storage.zone(z).cells {
            if self.is_good_store_cell(c, thing, Some(carrier)) {
                room += self.stack_space_left(c, thing.def);
                if room >= wanted || room as f32 >= carrying_capacity {
                    break;
                }
            }
        }
        wanted.min(room)
    }
}

/// Most units of `def` a pawn can carry (`MaxStackSpaceEver`).
pub fn max_carry(defs: &GameDefs, def: DefId<ThingDef>, carrying_capacity: f32) -> i32 {
    let d = &defs.things[def];
    (carrying_capacity / d.volume_per_unit())
        .round_ties_even()
        .min(d.stack_limit as f32) as i32
}

/// `WorkGiver_HaulGeneral`.
pub struct HaulGeneral<'a> {
    pub regions: &'a Regions,
    pub haul_job: Option<DefId<rimworld_defs::JobDef>>,
    pub carrying_capacity: f32,
}

impl HaulGeneral<'_> {
    fn view<'v>(&'v self, ctx: &'v WorkContext<'_>) -> StoreView<'v> {
        StoreView {
            defs: ctx.defs,
            map: ctx.map,
            grid: ctx.grid,
            regions: self.regions,
            reservations: ctx.reservations,
        }
    }
}

impl WorkGiver for HaulGeneral<'_> {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        self.view(ctx)
            .haulables(&mut ctx.rng.borrow_mut())
            .is_empty()
    }

    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(self.view(ctx).haulables(&mut ctx.rng.borrow_mut()))
    }

    /// `WorkGiver_Haul.JobOnThing` (base `HasJobOnThing` calls this too, so
    /// a successful selection searches storage twice).
    // COMPATIBILITY TODO: currently approximate — fog, burning, social
    // properness of food and unfinished-thing checks are not modelled.
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let view = self.view(ctx);
        let item = ctx.map.item(t)?;
        // `PawnCanAutomaticallyHaulFast`.
        if !ctx.reservations.can_reserve(
            ctx.claimant,
            Target::Item(t),
            item.stack_count as i32,
            1,
            STACK_ALL,
        ) || !ctx.can_reach(item.position)
        {
            return None;
        }
        let thing = Storable {
            def: item.def,
            position: item.position,
        };
        let current = view.current_priority(t);
        let dest = view.best_better_store_cell(
            thing,
            Some(ctx.claimant),
            current,
            true,
            &mut ctx.rng.borrow_mut(),
        )?;
        let count = view.haul_count(thing, ctx.claimant, dest, self.carrying_capacity);
        Some((
            Job {
                def: self.haul_job,
                kind: JobKind::Haul {
                    source: t,
                    dest,
                    count,
                    stage: HaulStage::GotoSource,
                    start_tick: 0,
                    aside: false,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}

/// The zone holding `c`, for callers outside this module.
pub fn zone_of(map: &Map, c: Cell) -> Option<ZoneId> {
    map.storage.zone_at(c)
}
