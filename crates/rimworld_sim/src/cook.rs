//! The bill work giver (docs/research.md §58): `WorkGiver_DoBill` for
//! work tables — the table checks, the bills in order, the ingredient
//! search (`TryFindBestIngredientsHelper`) and the failed-search cooldown.

use std::cell::RefCell;
use std::collections::BTreeMap;

use rimworld_defs::{DefId, GameDefs, JobDef, RecipeDef, ThingDef};

use crate::bills::{Bill, Candidate, RecipeFilters, allocate_mixing, allocate_no_mix};
use crate::grid::Cell;
use crate::job::{DoBillStage, Job, JobKind};
use crate::map::{Item, ItemId};
use crate::path::LocomotionUrgency;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// `WorkGiver_DoBill.ReCheckFailedBillTicksRange`.
pub const RECHECK_FAILED_BILL_TICKS: (i32, i32) = (500, 600);

/// What the bill work giver knows about one work table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableInfo {
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub position: Cell,
    pub interaction_cell: Cell,
    /// `UsableForBillsAfterFueling`.
    pub usable_after_fueling: bool,
    /// Refuelable and empty.
    pub out_of_fuel: bool,
}

/// The colony's bill stacks, by work table (`Building_WorkTable.billStack`).
pub type BillStacks = BTreeMap<ItemId, Vec<Bill>>;

/// What the bill giver shares with the simulation: the bill stacks (their
/// `paused` and cooldown fields change while searching) and the chosen
/// ingredients of the job it gives.
pub struct BillEnv<'a> {
    pub tables: &'a [TableInfo],
    pub stacks: &'a std::sync::Mutex<BillStacks>,
    pub filters: &'a BTreeMap<DefId<RecipeDef>, RecipeFilters>,
    /// Stored product counts (`ResourceCounter`) plus carried ones.
    pub product_counts: &'a BTreeMap<DefId<ThingDef>, i32>,
    /// Stored counts only (`ResourceCounter`).
    pub stored_counts: &'a BTreeMap<DefId<ThingDef>, i32>,
    /// The thinking pawn's skill levels, by skill name.
    pub skill: &'a dyn Fn(&str) -> i32,
    /// An item's rot stage (for the rotten/fresh special filters).
    pub rot_stage: &'a dyn Fn(&Item) -> Option<crate::sim::RotStage>,
    /// Set by the giver: the ingredients (`targetQueueB`, `countQueue`).
    pub chosen_out: &'a RefCell<Vec<(ItemId, u32)>>,
}

/// `WorkGiver_DoBill` for one work giver Def.
pub struct DoBillGiver<'a> {
    pub env: &'a BillEnv<'a>,
    /// `fixedBillGiverDefs`.
    pub fixed_givers: Vec<DefId<ThingDef>>,
    pub work_type: Option<String>,
    pub job: Option<DefId<JobDef>>,
}

impl DoBillGiver<'_> {
    fn table(&self, t: ItemId) -> Option<&TableInfo> {
        self.env.tables.iter().find(|x| x.id == t)
    }
}

/// `ThingFilter.Allows(Thing)`: the def, then the disallowed special
/// filters that match the thing within their category.
// COMPATIBILITY TODO: currently approximate — only the plant-food, rotten
// and fresh special filters have workers; hit points and quality are not
// checked.
pub fn filter_allows_item(
    defs: &GameDefs,
    f: &crate::storage::ThingFilter,
    item: &Item,
    rot: Option<crate::sim::RotStage>,
) -> bool {
    use crate::sim::RotStage;
    if !f.allows(item.def) {
        return false;
    }
    let d = &defs.things[item.def];
    for name in f.disallowed_special() {
        let Some(s) = defs.special_filters.get(name) else {
            continue;
        };
        let matches = match s.worker_class.as_deref() {
            Some("SpecialThingFilterWorker_PlantFood") => d
                .ingestible
                .as_ref()
                .is_some_and(|i| i.food_type & rimworld_defs::food_type::PLANT != 0),
            Some("SpecialThingFilterWorker_Rotten") => d
                .rottable
                .as_ref()
                .is_some_and(|r| !r.rot_destroys && rot.is_some_and(|s| s != RotStage::Fresh)),
            Some("SpecialThingFilterWorker_Fresh") => match d.rottable {
                None => d.ingestible.is_some(),
                Some(_) => rot == Some(RotStage::Fresh),
            },
            _ => false,
        };
        let within = s.parent_category.as_deref().is_some_and(|p| {
            d.thing_categories
                .iter()
                .any(|c| defs.category_within(c, p))
        });
        if matches && within {
            return false;
        }
    }
    true
}

impl WorkGiver for DoBillGiver<'_> {
    /// `ShouldSkip`: no table of this giver with a bill to do now.
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        let stacks = self.env.stacks.lock().expect("bill stacks");
        !self.env.tables.iter().any(|t| {
            self.fixed_givers.contains(&t.def) && stacks.get(&t.id).is_some_and(|b| !b.is_empty())
        })
    }

    /// `PotentialWorkThingRequest`: the giver's fixed work tables.
    fn potential_things(&self, _ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        Some(
            self.env
                .tables
                .iter()
                .filter(|t| self.fixed_givers.contains(&t.def))
                .map(|t| t.id)
                .collect(),
        )
    }

    /// `JobOnThing` / `StartOrResumeBillJob`.
    // COMPATIBILITY TODO: currently approximate — an empty refuelable table
    // gives no job here (the game offers the refuel job); burning tables,
    // `RemoveIncompletableBills`, unfinished things, medical and autonomous
    // bills, pawn restrictions and the haul-off of things blocking the
    // table are not modelled.
    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let table = self.table(t)?.clone();
        if !self.fixed_givers.contains(&table.def) || !table.usable_after_fueling {
            return None;
        }
        {
            let mut stacks = self.env.stacks.lock().expect("bill stacks");
            let bills = stacks.get_mut(&t)?;
            // `BillStack.AnyShouldDoNow`.
            if !bills.iter_mut().any(|b| {
                let r = b.recipe;
                b.should_do_now(|| self.count_products(ctx.defs, r))
            }) {
                return None;
            }
        }
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
            || !ctx.reservations.can_reserve(
                ctx.claimant,
                Target::Cell(table.interaction_cell),
                1,
                1,
                STACK_ALL,
            )
        {
            return None;
        }
        if table.out_of_fuel {
            return None;
        }
        let n = self
            .env
            .stacks
            .lock()
            .expect("bill stacks")
            .get(&t)
            .map_or(0, |b| b.len());
        for index in 0..n {
            let (recipe, id) = {
                let mut stacks = self.env.stacks.lock().expect("bill stacks");
                let bill = &mut stacks.get_mut(&t)?[index];
                let recipe_id = bill.recipe;
                let r = &ctx.defs.recipes[recipe_id];
                if r.required_giver_work_type
                    .as_deref()
                    .is_some_and(|w| Some(w) != self.work_type.as_deref())
                    || ctx.tick <= bill.next_search_tick
                    || !bill.should_do_now(|| self.count_products(ctx.defs, recipe_id))
                    || !bill.pawn_allowed(r.work_skill.as_deref().map(|s| (self.env.skill)(s)))
                    || r.skill_requirements
                        .iter()
                        .any(|(s, min)| (self.env.skill)(s) < *min)
                {
                    continue;
                }
                (bill.recipe, bill.id)
            };
            match self.find_ingredients(ctx, &table, t, index, recipe) {
                Some(chosen) => {
                    *self.env.chosen_out.borrow_mut() = chosen.clone();
                    return Some((
                        Job {
                            def: self.job,
                            kind: JobKind::DoBill {
                                giver: t,
                                bill: id,
                                ingredient: None,
                                count: 0,
                                stage: DoBillStage::Start,
                            },
                            forced: false,
                            urgency: LocomotionUrgency::Jog,
                            start_tick: 0,
                        },
                        Vec::new(),
                    ));
                }
                None => {
                    // The bill waits 500–600 ticks before searching again.
                    let wait = ctx
                        .rng
                        .borrow_mut()
                        .range_inclusive(RECHECK_FAILED_BILL_TICKS.0, RECHECK_FAILED_BILL_TICKS.1);
                    if let Some(b) = self
                        .env
                        .stacks
                        .lock()
                        .expect("bill stacks")
                        .get_mut(&t)
                        .and_then(|s| s.get_mut(index))
                    {
                        b.next_search_tick = ctx.tick + wait as u64;
                    }
                }
            }
        }
        None
    }
}

impl DoBillGiver<'_> {
    /// `RecipeWorkerCounter.CountProducts` (default options): the stored
    /// and carried count of the single product; butchering counts every
    /// stored raw meat, not what colonists carry
    /// (`RecipeWorkerCounter_ButcherAnimals`).
    fn count_products(&self, defs: &GameDefs, recipe: DefId<RecipeDef>) -> i32 {
        let r = &defs.recipes[recipe];
        if r.worker_counter_class.as_deref() == Some("RecipeWorkerCounter_ButcherAnimals") {
            return defs
                .things
                .iter()
                .filter(|(_, t)| t.thing_categories.iter().any(|c| c == "MeatRaw"))
                .map(|(id, _)| self.env.stored_counts.get(&id).copied().unwrap_or(0))
                .sum();
        }
        match r.products.as_slice() {
            [(p, _)] => defs
                .things
                .id(p)
                .and_then(|d| self.env.product_counts.get(&d).copied())
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// `TryFindBestBillIngredients`: breadth-first over regions from the
    /// table's interaction cell; usable things (allowed by the bill and a
    /// recipe row, within the search radius of the table, unforbidden,
    /// reservable) are collected per region and, once more regions than
    /// the root's neighbours have been processed, offered to the
    /// allocator; the search stops when it succeeds.
    // COMPATIBILITY TODO: currently approximate — a region's things are
    // taken in spawn order, reachability within a region is assumed, and
    // haul-source containers are not searched.
    fn find_ingredients(
        &self,
        ctx: &WorkContext<'_>,
        table: &TableInfo,
        t: ItemId,
        index: usize,
        recipe: DefId<RecipeDef>,
    ) -> Option<Vec<(ItemId, u32)>> {
        let defs = ctx.defs;
        let r = &defs.recipes[recipe];
        if r.ingredients.is_empty() {
            return Some(Vec::new());
        }
        if r.ingredients.len() > 4 {
            return None;
        }
        let filters = self.env.filters.get(&recipe)?;
        let stacks = self.env.stacks.lock().expect("bill stacks");
        let bill = stacks.get(&t)?.get(index)?;
        let root_region = ctx.regions.region_at(table.interaction_cell)?;
        let radius = bill.search_radius;
        let radius_sq = radius * radius;
        let regions = ctx.regions;
        let usable = |it: &Item| -> Option<Candidate> {
            let rot = (self.env.rot_stage)(it);
            // `IsFixedOrAllowedIngredient` then a row allowing it.
            let fixed_ok = filters
                .rows
                .iter()
                .any(|(f, single)| single.is_some() && filter_allows_item(defs, f, it, rot));
            let bill_ok = filter_allows_item(defs, &bill.ingredient_filter, it, rot);
            let allowed =
                fixed_ok || (filter_allows_item(defs, &filters.fixed, it, rot) && bill_ok);
            let mut row_allows = [false; 4];
            for (k, (f, _)) in filters.rows.iter().enumerate() {
                row_allows[k] = filter_allows_item(defs, f, it, rot);
            }
            if !allowed || !row_allows.iter().any(|&a| a) {
                return None;
            }
            let (dx, dz) = (
                it.position.x - table.position.x,
                it.position.z - table.position.z,
            );
            if ((dx * dx + dz * dz) as f32) >= radius_sq
                || it.forbidden
                || !ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Item(it.id),
                    it.stack_count as i32,
                    1,
                    STACK_ALL,
                )
            {
                return None;
            }
            Some(Candidate {
                id: it.id,
                def: it.def,
                position: it.position,
                stack_count: it.stack_count,
                row_allows,
                bill_allows: bill_ok,
            })
        };
        let entry = |_: usize, rg: usize| {
            if (999.0 - radius).abs() < 1.0 {
                return true;
            }
            let e = regions.region(rg).extents;
            let p = table.position;
            let nx = (p.x - p.x.clamp(e.min_x, e.max_x)).abs();
            let nz = (p.z - p.z.clamp(e.min_z, e.max_z)).abs();
            (nx as f32) <= radius
                && (nz as f32) <= radius
                && ((nx * nx + nz * nz) as f32) <= radius_sq
        };
        let adjacent = regions
            .neighbors(root_region)
            .filter(|&n| regions.region(n).kind.passable() && entry(root_region, n))
            .count();
        let fixed_rows: Vec<bool> = filters.rows.iter().map(|(_, s)| s.is_some()).collect();
        let mut relevant: Vec<Candidate> = Vec::new();
        let mut fresh: Vec<Candidate> = Vec::new();
        let mut processed = 0usize;
        let mut chosen = None;
        regions.traverse(
            root_region,
            entry,
            |rg| {
                for it in ctx.map.items() {
                    if it.is_filth()
                        || regions.region_at(it.position) != Some(rg)
                        || relevant.iter().chain(&fresh).any(|c| c.id == it.id)
                    {
                        continue;
                    }
                    if let Some(c) = usable(it) {
                        fresh.push(c);
                    }
                }
                processed += 1;
                if !fresh.is_empty() && processed > adjacent {
                    relevant.append(&mut fresh);
                    // The allocator sorts the list in place, as the game's
                    // does (its unstable sort sees the previous order).
                    let found = if r.allow_mixing_ingredients {
                        allocate_mixing(defs, r, &fixed_rows, &mut relevant, table.interaction_cell)
                    } else {
                        allocate_no_mix(
                            defs,
                            r,
                            filters,
                            &bill.ingredient_filter,
                            &mut relevant,
                            table.interaction_cell,
                        )
                    };
                    if let Some(c) = found {
                        chosen = Some(c);
                        return true;
                    }
                }
                false
            },
            99_999,
        );
        chosen
    }
}
