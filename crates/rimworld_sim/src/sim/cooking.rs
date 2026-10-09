//! Bills in the simulation (docs/research.md §58): the work tables' bill
//! stacks, the resource counts target-count bills read, and the bill job
//! (`JobDriver_DoBill`, `Toils_Recipe`): gather the ingredients onto the
//! table, work the recipe, then make the products, consume the
//! ingredients and store or drop the products.

use std::collections::BTreeMap;

use rimworld_defs::{DefId, RecipeDef, ThingDef};

use super::{JobEvent, Sim, tick_movement};
use crate::bills::{Bill, RecipeFilters, StoreMode};
use crate::cook::TableInfo;
use crate::grid::Cell;
use crate::haul::{Storable, StoreView, max_carry};
use crate::job::{DoBillStage, HaulStage, Job, JobKind};
use crate::map::{ItemId, ItemMeta, Map};
use crate::path::{LocomotionUrgency, PathGrid};
use crate::pawn::{Carried, Pawn};
use crate::reservation::{STACK_ALL, Target};
use crate::storage::StoragePriority;

/// `ResourceCounter` refreshes on ticks divisible by 204.
const RESOURCE_COUNT_INTERVAL: u64 = 204;
/// `JumpToCollectNextIntoHandsForBill`: stacks within 8 cells.
const COLLECT_NEXT_MAX_DIST_SQ: i32 = 64;

impl Sim {
    /// The bills of work table `table`, in order.
    pub fn bills(&self, table: ItemId) -> Vec<Bill> {
        self.bill_stacks
            .lock()
            .expect("bill stacks")
            .get(&table)
            .cloned()
            .unwrap_or_default()
    }

    /// `BillStack.AddBill`: a new bill for `recipe` at the end of the
    /// table's stack. Returns its id.
    pub fn add_bill(&mut self, table: ItemId, recipe: DefId<RecipeDef>) -> u32 {
        let id = self.map.next_bill_id;
        self.map.next_bill_id += 1;
        let bill = Bill::new(&self.defs, recipe, id);
        self.bill_stacks
            .lock()
            .expect("bill stacks")
            .entry(table)
            .or_default()
            .push(bill);
        id
    }

    /// `RecipeDef.AvailableNow` (research part): its research is done.
    pub fn recipe_available(&self, recipe: DefId<RecipeDef>) -> bool {
        let r = &self.defs.recipes[recipe];
        r.research_prerequisite
            .iter()
            .chain(&r.research_prerequisites)
            .all(|p| self.research_finished(p))
    }

    /// Changes a bill (`f` gets it mutably).
    pub fn edit_bill(&mut self, table: ItemId, id: u32, f: impl FnOnce(&mut Bill)) {
        if let Some(b) = self
            .bill_stacks
            .lock()
            .expect("bill stacks")
            .get_mut(&table)
            .and_then(|s| s.iter_mut().find(|b| b.id == id))
        {
            f(b);
        }
    }

    /// `BillStack.Delete`.
    pub fn delete_bill(&mut self, table: ItemId, id: u32) {
        if let Some(s) = self
            .bill_stacks
            .lock()
            .expect("bill stacks")
            .get_mut(&table)
        {
            s.retain(|b| b.id != id);
        }
    }

    /// Work tables doing bills (any building with recipes).
    pub(super) fn table_infos(&self) -> Vec<TableInfo> {
        let defs = &self.defs;
        self.map
            .structures()
            .iter()
            .filter(|s| !crate::bills::all_recipes(defs, s.def).is_empty())
            .filter_map(|s| {
                let d = &defs.things[s.def];
                let interaction_cell = self.interaction_cell(s.id)?;
                // `UsableForBillsAfterFueling`: power if it needs it, not
                // broken down.
                // COMPATIBILITY TODO: currently approximate —
                // `unpoweredWorkTableWorkSpeedFactor` tables are treated as
                // needing power.
                let needs_power = d
                    .power
                    .as_ref()
                    .is_some_and(|p| p.comp_class == "CompPowerTrader");
                Some(TableInfo {
                    id: s.id,
                    def: s.def,
                    position: s.footprint.center,
                    interaction_cell,
                    usable_after_fueling: (!needs_power || s.power.on) && !s.power.broken_down,
                    out_of_fuel: d.refuelable.is_some() && s.fuel <= 0.0,
                })
            })
            .collect()
    }

    /// Resolved recipe filters for every recipe with a bill.
    pub(super) fn bill_recipe_filters(&mut self) -> BTreeMap<DefId<RecipeDef>, RecipeFilters> {
        let recipes: Vec<DefId<RecipeDef>> = self
            .bill_stacks
            .lock()
            .expect("bill stacks")
            .values()
            .flatten()
            .map(|b| b.recipe)
            .collect();
        for r in recipes {
            if !self.recipe_filters.contains_key(&r) {
                let f = RecipeFilters::new(&self.defs, &self.defs.recipes[r]);
                self.recipe_filters.insert(r, f);
            }
        }
        self.recipe_filters.clone()
    }

    /// `ResourceCounter.UpdateResourceCounts`: things in stockpiles that
    /// are not rotting, by def; plus what colonists carry
    /// (`RecipeWorkerCounter.GetCarriedCount`).
    // COMPATIBILITY TODO: currently approximate — every stored item def is
    // counted (the game counts `CountAsResource` defs), minified things,
    // fog and the slow counting paths of non-default bill options are not
    // modelled.
    pub(super) fn update_resource_counts(&mut self) {
        let mut counts: BTreeMap<DefId<ThingDef>, i32> = BTreeMap::new();
        for it in self.map.items() {
            if it.is_filth()
                || self.map.storage.zone_at(it.position).is_none()
                || self
                    .rot_stage(it.id)
                    .is_some_and(|s| s != crate::sim::RotStage::Fresh)
            {
                continue;
            }
            *counts.entry(it.def).or_default() += it.stack_count as i32;
        }
        self.stored_counts = counts;
    }

    /// The counts target-count bills see: stored plus carried by
    /// colonists.
    pub(super) fn product_counts(&self) -> BTreeMap<DefId<ThingDef>, i32> {
        let mut c = self.stored_counts.clone();
        for p in &self.pawns {
            if p.is_colonist
                && let Some(carried) = p.carried
            {
                *c.entry(carried.def).or_default() += carried.count as i32;
            }
        }
        c
    }

    /// The resource counter's periodic refresh.
    pub(super) fn tick_resource_counter(&mut self) {
        if self.tick.is_multiple_of(RESOURCE_COUNT_INTERVAL) {
            self.update_resource_counts();
        }
    }

    /// The table's `WorkTableWorkSpeedFactor`: base value, then unpowered,
    /// room role, temperature and outdoors (×0.8 psychologically outdoors)
    /// parts, at least 0.1.
    pub fn work_table_speed_factor(&mut self, table: ItemId) -> f32 {
        let Some(s) = self.map.structure(table) else {
            return 1.0;
        };
        let def = self.defs.things[s.def].clone();
        let center = s.footprint.center;
        let mut v = crate::stats::def_stat(&self.defs, &def, None, "WorkTableWorkSpeedFactor");
        let room = self.room_at(center);
        let outdoors = room.is_some_and(|r| self.room_psychologically_outdoors(r));
        if let (Some(r), Some(b)) = (room, def.building.as_ref())
            && let Some(want) = b.work_table_room_role.as_deref()
            && !outdoors
            && self.room_role(r) != want
        {
            v *= b.work_table_not_in_room_role_factor;
        }
        let t = self.cell_temperature(center);
        if !(9.0..=35.0).contains(&t) {
            v *= 0.7;
        }
        if outdoors {
            v *= 0.8;
        }
        v.max(0.1)
    }

    /// `Building_WorkTable.UsedThisTick` (and `_HeatPush`): fuel burns
    /// while used (`consumeFuelOnlyWhenUsed`); every 30 ticks (hashed) the
    /// table pushes `heatPerTickWhileWorking` × 30.
    pub(super) fn table_used_this_tick(&mut self, table: ItemId) {
        let defs = self.defs.clone();
        let t = self.tick;
        let Some(s) = self.map.structure_mut(table) else {
            return;
        };
        let d = &defs.things[s.def];
        if let Some(r) = &d.refuelable
            && r.consume_fuel_only_when_used
        {
            s.fuel -= r.consumption_rate / 60_000.0;
            if s.fuel <= 0.0 {
                s.fuel = 0.0;
            }
        }
        let center = s.footprint.center;
        let id_number = s.id_number;
        if d.thing_class.as_deref() == Some("Building_WorkTable_HeatPush")
            && crate::hash::is_hash_interval_tick(t, id_number, 30)
        {
            self.push_heat(center, d.heat_per_tick_while_working * 30.0);
        }
    }

    fn set_do_bill(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut Option<ItemId>, &mut i32, &mut DoBillStage),
    ) {
        if let Some(Job {
            kind:
                JobKind::DoBill {
                    ingredient,
                    count,
                    stage,
                    ..
                },
            ..
        }) = &mut self.pawns[i].job
        {
            f(ingredient, count, stage);
        }
    }

    fn do_bill_parts(&self, i: usize) -> Option<(ItemId, u32, Option<ItemId>, i32, DoBillStage)> {
        match self.pawns[i].job.as_ref()?.kind {
            JobKind::DoBill {
                giver,
                bill,
                ingredient,
                count,
                stage,
            } => Some((giver, bill, ingredient, count, stage)),
            _ => None,
        }
    }

    /// The bill of the pawn's job, if it still exists.
    fn job_bill(&self, i: usize) -> Option<Bill> {
        let (giver, bill, ..) = self.do_bill_parts(i)?;
        self.bill_stacks
            .lock()
            .expect("bill stacks")
            .get(&giver)?
            .iter()
            .find(|b| b.id == bill)
            .cloned()
    }

    /// The driver-wide fail conditions: the table gone, the bill deleted,
    /// the table not currently usable (power, breakdown, fuel).
    fn do_bill_failed(&self, i: usize) -> bool {
        let Some((giver, ..)) = self.do_bill_parts(i) else {
            return true;
        };
        if self.job_bill(i).is_none() {
            return true;
        }
        let Some(t) = self.table_infos().into_iter().find(|t| t.id == giver) else {
            return true;
        };
        !t.usable_after_fueling || t.out_of_fuel
    }

    /// The job starts (`Notify_DoBillStarted`): with nothing queued it goes
    /// straight to the table, else to the first ingredient.
    pub(super) fn begin_do_bill(&mut self, i: usize) -> bool {
        if self.do_bill_failed(i) {
            return false;
        }
        if self.pawns[i].target_queue_b.is_empty() {
            return self.goto_bill_table(i);
        }
        self.extract_next_ingredient(i)
    }

    /// `ExtractNextTargetFromQueue` and the goto (ClosestTouch: onto the
    /// item's cell).
    fn extract_next_ingredient(&mut self, i: usize) -> bool {
        if self.pawns[i].target_queue_b.is_empty() {
            return self.goto_bill_table(i);
        }
        let item = self.pawns[i].target_queue_b.remove(0);
        let count = if self.pawns[i].count_queue.is_empty() {
            0
        } else {
            self.pawns[i].count_queue.remove(0) as i32
        };
        self.set_do_bill(i, |ing, c, stage| {
            *ing = Some(item);
            *c = count;
            *stage = DoBillStage::GotoIngredient;
        });
        self.goto_ingredient(i, item)
    }

    fn goto_ingredient(&mut self, i: usize, item: ItemId) -> bool {
        let Some(at) = self
            .map
            .item(item)
            .filter(|it| !it.forbidden)
            .map(|it| it.position)
        else {
            return false;
        };
        if self.pawns[i].next_stop() == at {
            self.bill_pick_up(i);
            return true;
        }
        self.walk_to(i, at, true)
    }

    /// `StartCarryThing` (remainder back into the queue, failing when the
    /// stack is smaller than the job count), then
    /// `JumpToCollectNextIntoHandsForBill`.
    pub(super) fn bill_pick_up(&mut self, i: usize) {
        let Some((_, _, Some(item), count, _)) = self.do_bill_parts(i) else {
            return;
        };
        let Some((def, stack)) = self.map.item(item).map(|it| (it.def, it.stack_count)) else {
            self.end_job(i, false);
            return;
        };
        if (stack as i32) < count {
            self.end_job(i, false);
            return;
        }
        let carried_now = self.pawns[i].carried;
        if carried_now.is_some_and(|c| c.def != def) {
            self.end_job(i, false);
            return;
        }
        let space = max_carry(&self.defs, def, self.carrying_capacity(i))
            - carried_now.map_or(0, |c| c.count as i32);
        let taken = count.min(space).min(stack as i32).max(0) as u32;
        if taken == 0 {
            self.end_job(i, false);
            return;
        }
        // `putRemainderInQueue`.
        if (taken as i32) < count {
            self.pawns[i].target_queue_b.insert(0, item);
            self.pawns[i]
                .count_queue
                .insert(0, (count - taken as i32) as u32);
        }
        let src_rot = self.map.item(item).map_or(0.0, |it| it.rot);
        let src_hp = self.map.item(item).and_then(|it| it.hit_points);
        let max_hp = self.max_hit_points(def);
        self.map.take_from_item(item, taken);
        let picked = if taken < stack {
            self.map.allocate_item_id()
        } else {
            item
        };
        let carried = match carried_now {
            Some(mut c) => {
                c.rot = crate::map::blend_rot(c.rot, c.count, src_rot, taken);
                c.hit_points =
                    crate::map::blend_hit_points(c.hit_points, c.count, src_hp, taken, max_hp);
                c.count += taken;
                c
            }
            None => Carried {
                id: picked,
                def,
                count: taken,
                rot: src_rot,
                hit_points: src_hp,
            },
        };
        self.pawns[i].carried = Some(carried);
        self.refresh_path_grid();
        // Collect a queued stack of the same thing within 8 cells.
        let room = max_carry(&self.defs, def, self.carrying_capacity(i)) - carried.count as i32;
        if room > 0 {
            let pos = self.pawns[i].position;
            let limit = self.defs.things[def].stack_limit;
            for n in 0..self.pawns[i].target_queue_b.len() {
                let q = self.pawns[i].target_queue_b[n];
                let Some(it) = self.map.item(q) else {
                    continue;
                };
                let (dx, dz) = (it.position.x - pos.x, it.position.z - pos.z);
                if it.def != def || it.forbidden || dx * dx + dz * dz > COLLECT_NEXT_MAX_DIST_SQ {
                    continue;
                }
                let want = self.pawns[i].count_queue.get(n).copied().unwrap_or(0) as i32;
                let a = want.min(limit - carried.count as i32).min(room);
                if a > 0 {
                    self.pawns[i].count_queue[n] -= a as u32;
                    if self.pawns[i].count_queue[n] == 0 {
                        self.pawns[i].count_queue.remove(n);
                        self.pawns[i].target_queue_b.remove(n);
                    }
                    self.set_do_bill(i, |ing, c, stage| {
                        *ing = Some(q);
                        *c = a;
                        *stage = DoBillStage::GotoIngredient;
                    });
                    if !self.goto_ingredient(i, q) {
                        self.end_job(i, false);
                    }
                    return;
                }
            }
        }
        // Carry to the table.
        self.set_do_bill(i, |_, _, stage| *stage = DoBillStage::CarryToTable);
        let Some((giver, ..)) = self.do_bill_parts(i) else {
            return;
        };
        let Some(cell) = self.interaction_cell(giver) else {
            self.end_job(i, false);
            return;
        };
        if self.pawns[i].next_stop() == cell {
            self.bill_deliver(i);
        } else if !self.walk_to(i, cell, false) {
            self.end_job(i, false);
        }
    }

    /// At the table with an ingredient: `SetTargetToIngredientPlaceCell`
    /// (the table's cells nearest the interaction cell first, then radial
    /// cells around it), put it down there (not as storage) and record it
    /// as placed; then the next ingredient or the work.
    // COMPATIBILITY TODO: currently approximate — the physical-interaction
    // reservation is not modelled; a thing that only partly fits is
    // dropped near the table.
    pub(super) fn bill_deliver(&mut self, i: usize) {
        let Some((giver, ..)) = self.do_bill_parts(i) else {
            return;
        };
        let Some(mut carried) = self.pawns[i].carried.take() else {
            self.end_job(i, false);
            return;
        };
        let Some(cell) = self.ingredient_place_cell(giver, &carried) else {
            self.pawns[i].carried = Some(carried);
            self.end_job(i, false);
            return;
        };
        let mut placed = Vec::new();
        let complete = self.place_direct_recording(&mut carried, cell, &mut placed);
        if !complete {
            let at = self.pawns[i].position;
            if !self.place_thing_near(&mut carried, at) {
                self.map.spawn_carried(&carried, at);
            }
        }
        self.refresh_path_grid();
        // `HaulAIUtility.UpdateJobWithPlacedThings`.
        for (id, n) in placed {
            match self.pawns[i]
                .placed_things
                .iter_mut()
                .find(|(p, _)| *p == id)
            {
                Some(e) => e.1 += n,
                None => self.pawns[i].placed_things.push((id, n)),
            }
        }
        if self.pawns[i].target_queue_b.is_empty() {
            self.start_bill_work(i);
        } else if !self.extract_next_ingredient(i) {
            self.end_job(i, false);
        }
    }

    fn ingredient_place_cell(&self, giver: ItemId, thing: &Carried) -> Option<Cell> {
        let s = self.map.structure(giver)?;
        let interact = self.interaction_cell(giver)?;
        let dist = |c: Cell| {
            let (dx, dz) = (c.x - interact.x, c.z - interact.z);
            dx * dx + dz * dz
        };
        // `OrderBy` is stable.
        let mut cells: Vec<Cell> = s.footprint.cells().collect();
        cells.sort_by_key(|&c| dist(c));
        let radial = crate::clean::radial_pattern();
        for &r in radial.iter().take(200) {
            let c = interact + r;
            if cells.contains(&c) {
                continue;
            }
            let ok = self.map.structure_at(c).is_none_or(|e| {
                let d = &self.defs.things[e.def];
                d.passability != rimworld_defs::Passability::Impassable || d.surface_type.is_some()
            });
            if ok {
                cells.push(c);
            }
        }
        let limit = self.defs.things[thing.def].stack_limit as u32;
        let mut first = None;
        for c in cells {
            if !self.map.size().contains(c) || !self.can_spawn_item_at(c) {
                continue;
            }
            first.get_or_insert(c);
            let blocked = self
                .map
                .items_at(c)
                .filter(|it| !it.is_filth())
                .any(|it| it.def != thing.def || it.stack_count >= limit);
            if !blocked {
                return Some(c);
            }
        }
        first
    }

    /// `GenSpawn.CanSpawnAt` for an item: in bounds, not inside an
    /// impassable building.
    fn can_spawn_item_at(&self, c: Cell) -> bool {
        self.map.structure_at(c).is_none_or(|s| {
            self.defs.things[s.def].passability != rimworld_defs::Passability::Impassable
        })
    }

    /// Go to the interaction cell to work.
    fn goto_bill_table(&mut self, i: usize) -> bool {
        let Some((giver, ..)) = self.do_bill_parts(i) else {
            return false;
        };
        let Some(cell) = self.interaction_cell(giver) else {
            return false;
        };
        self.set_do_bill(i, |_, _, stage| *stage = DoBillStage::GotoTable);
        if self.pawns[i].next_stop() == cell {
            self.start_bill_work(i);
            return true;
        }
        self.walk_to(i, cell, false)
    }

    /// `DoRecipeWork` starts: work left = the bill's work amount.
    pub(super) fn start_bill_work(&mut self, i: usize) {
        let Some(bill) = self.job_bill(i) else {
            self.end_job(i, false);
            return;
        };
        let work = crate::bills::work_amount(&self.defs, &self.defs.recipes[bill.recipe]);
        self.set_do_bill(i, |_, _, stage| {
            *stage = DoBillStage::Work {
                work_left: work,
                spent: 0,
            }
        });
    }

    /// The work toil's interval: ticks spent += delta; work left −=
    /// (work speed stat × table speed factor) × delta; done at ≤ 0. Fails
    /// on a suspended bill, an unusable table or placed ingredients gone or
    /// forbidden.
    // COMPATIBILITY TODO: currently approximate — chair comfort, effects
    // and sounds are not modelled.
    pub(super) fn do_bill_interval(&mut self, i: usize, delta: i32) {
        let Some((giver, _, _, _, DoBillStage::Work { work_left, spent })) = self.do_bill_parts(i)
        else {
            return;
        };
        let Some(bill) = self.job_bill(i) else {
            self.end_job(i, false);
            return;
        };
        if bill.suspended || self.do_bill_failed(i) {
            self.end_job(i, false);
            return;
        }
        let placed_ok = self.pawns[i].placed_things.iter().all(|(id, n)| {
            self.map
                .item(*id)
                .is_some_and(|it| !it.forbidden && it.stack_count >= *n)
        });
        if !placed_ok {
            self.end_job(i, false);
            return;
        }
        let recipe = self.defs.recipes[bill.recipe].clone();
        let mut rate = recipe
            .work_speed_stat
            .as_deref()
            .map_or(1.0, |s| self.pawn_stat_of(i, s));
        if recipe.work_table_speed_stat.as_deref() == Some("WorkTableWorkSpeedFactor") {
            rate *= self.work_table_speed_factor(giver);
        }
        let left = crate::bills::subtract_work(work_left, rate, delta);
        let spent = spent + delta;
        self.set_do_bill(i, |_, _, stage| {
            *stage = DoBillStage::Work {
                work_left: left,
                spent,
            }
        });
        if left <= 0.0 {
            self.finish_recipe(i);
        }
    }

    /// `FinishRecipeAndStartStoringProduct`.
    // COMPATIBILITY TODO: currently approximate — quality/art/style
    // products, smelted and stone-block special products are not made,
    // and the bill-done records are not kept.
    fn finish_recipe(&mut self, i: usize) {
        let Some((giver, _, _, _, DoBillStage::Work { spent, .. })) = self.do_bill_parts(i) else {
            return;
        };
        let Some(bill) = self.job_bill(i) else {
            self.end_job(i, false);
            return;
        };
        let recipe = self.defs.recipes[bill.recipe].clone();
        // No unfinished thing: the skill learns now.
        if let Some(skill) = &recipe.work_skill {
            self.learn(
                i,
                skill,
                spent as f32 * 0.1 * recipe.work_skill_learn_factor,
            );
        }
        // `CalculateIngredients`: the placed counts (split off if part of a
        // stack).
        let placed = std::mem::take(&mut self.pawns[i].placed_things);
        let mut ingredients: Vec<(ItemId, DefId<ThingDef>, u32)> = Vec::new();
        for (id, n) in placed {
            let Some((def, stack)) = self.map.item(id).map(|it| (it.def, it.stack_count)) else {
                continue;
            };
            let n = n.min(stack);
            if n == 0 {
                continue;
            }
            ingredients.push((id, def, n));
        }
        // `CalculateDominantIngredient`: a draw weighted by stack count
        // (none for a single ingredient).
        if !ingredients.is_empty() {
            let weights: Vec<f32> = ingredients.iter().map(|&(_, _, n)| n as f32).collect();
            let _ = crate::region::random_element_by_weight(&weights, &mut self.rng);
        }
        // `GenRecipe.MakeRecipeProducts`: efficiency = the worker's
        // efficiency stat (else 1) × the table's efficiency stat.
        let mut efficiency = recipe
            .efficiency_stat
            .as_deref()
            .map_or(1.0, |s| self.pawn_stat_of(i, s));
        if let (Some(stat), Some(table)) = (
            recipe.work_table_efficiency_stat.as_deref(),
            self.map.structure(giver),
        ) {
            efficiency *=
                crate::stats::def_stat(&self.defs, &self.defs.things[table.def], None, stat);
        }
        // Products: ceil(count × efficiency), ingredients registered, then
        // the food-poisoning rolls.
        let at = self.pawns[i].position;
        let mut products: Vec<Carried> = Vec::new();
        let mut metas: Vec<ItemMeta> = Vec::new();
        for (name, count) in &recipe.products {
            let Some(def) = self.defs.things.id(name) else {
                continue;
            };
            let mut meta = ItemMeta::default();
            for &(_, d, _) in &ingredients {
                if !meta.ingredients.contains(&d) {
                    meta.ingredients.push(d);
                }
            }
            if self.defs.things[def].ingestible.is_some()
                && self.has_comp(def, "CompProperties_FoodPoisonable")
            {
                let room = self.food_poison_room_chance(at);
                let cook = self.pawn_stat_of(i, "FoodPoisonChance");
                let (pct, cause) = crate::bills::roll_food_poison(&mut self.rng, room, cook);
                meta.poison_pct = pct;
                meta.poison_cause = cause;
            }
            products.push(Carried {
                id: self.map.allocate_item_id(),
                def,
                count: (*count as f32 * efficiency).ceil().max(0.0) as u32,
                rot: 0.0,
                hit_points: None,
            });
            metas.push(meta);
        }
        // Special products, per ingredient: butchery.
        for special in &recipe.special_products {
            if special != "Butchery" {
                continue;
            }
            for &(id, _, _) in &ingredients {
                let Some(t) = self.corpse_pawn(id).and_then(|p| self.index_of(p)) else {
                    continue;
                };
                for p in self.butcher_products(t, i, efficiency) {
                    products.push(p);
                    metas.push(ItemMeta::default());
                }
            }
        }
        // `ConsumeIngredients`: the counted part of each placed thing.
        for &(id, _, n) in &ingredients {
            self.map.take_from_item(id, n);
            self.map.item_meta.remove(&id);
        }
        // `Notify_IterationCompleted`.
        let bill_id = bill.id;
        self.edit_bill(giver, bill_id, |b| b.iteration_completed());
        if bill.repeat_mode == crate::bills::RepeatMode::TargetCount {
            self.update_resource_counts();
        }
        for (p, m) in products.iter().zip(&metas) {
            if m != &ItemMeta::default() {
                self.map.item_meta.insert(p.id, m.clone());
            }
        }
        if products.is_empty() {
            self.end_job(i, true);
            return;
        }
        if bill.store_mode == StoreMode::DropOnFloor {
            for mut p in products {
                if !self.place_thing_near(&mut p, at) {
                    self.map.spawn_carried(&p, at);
                }
            }
            self.refresh_path_grid();
            self.end_job(i, true);
            return;
        }
        // Store the first product (the rest are dropped).
        let mut first = products.remove(0);
        for mut p in products {
            if !self.place_thing_near(&mut p, at) {
                self.map.spawn_carried(&p, at);
            }
        }
        let claimant = self.claimant(i);
        let cell = {
            let view = StoreView {
                defs: &self.defs,
                map: &self.map,
                grid: &self.path_grid,
                regions: &self.regions,
                reservations: &self.reservations,
            };
            view.best_better_store_cell(
                Storable {
                    def: first.def,
                    position: at,
                },
                Some(claimant),
                StoragePriority::Unstored,
                true,
                &mut self.rng,
            )
        };
        match cell {
            Some(cell) => {
                let space =
                    max_carry(&self.defs, first.def, self.carrying_capacity(i)).max(0) as u32;
                if space < first.count {
                    let mut extra = Carried {
                        id: self.map.allocate_item_id(),
                        count: first.count - space,
                        ..first
                    };
                    first.count = space;
                    if !self.place_thing_near(&mut extra, at) {
                        self.map.spawn_carried(&extra, at);
                    }
                }
                if space == 0 {
                    self.refresh_path_grid();
                    self.end_job(i, true);
                    return;
                }
                self.start_carried_store_haul(i, first, cell);
            }
            None => {
                if !self.place_thing_near(&mut first, at) {
                    self.map.spawn_carried(&first, at);
                }
                self.refresh_path_grid();
                self.end_job(i, true);
            }
        }
    }

    /// `HaulAIUtility.HaulToCellStorageJob` for a product just made, kept in
    /// hand (`keepCarryingThingOverride`).
    fn start_carried_store_haul(&mut self, i: usize, product: Carried, cell: Cell) {
        self.cleanup_job(i);
        let id = self.next_job_id;
        self.next_job_id += 1;
        let claimant = self.claimant(i);
        self.reservations
            .reserve(claimant, id, Target::Cell(cell), 1, 1, STACK_ALL);
        let t = self.tick;
        self.pawns[i].carried = Some(product);
        self.pawns[i].target_queue_b.clear();
        self.pawns[i].count_queue.clear();
        self.pawns[i].job_id = id;
        self.pawns[i].job = Some(Job {
            def: self.job_defs.haul_to_cell,
            kind: JobKind::Haul {
                source: product.id,
                dest: cell,
                count: product.count as i32,
                stage: HaulStage::CarryToCell,
                start_tick: t,
                aside: false,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: t,
        });
        self.refresh_path_grid();
        self.haul_start_carry(i);
    }

    /// The room's `FoodPoisonChance` (a curve over its cleanliness), or
    /// the stat's roomless 0.02 for rooms without stats.
    fn food_poison_room_chance(&self, at: Cell) -> f32 {
        match self.room_at(at) {
            Some(r) => self.room_food_poison_chance(r),
            None => 0.02,
        }
    }

    /// Whether `def` has a comp of `class`.
    pub(super) fn has_comp(&self, def: DefId<ThingDef>, class: &str) -> bool {
        self.defs
            .raw
            .get("ThingDef", &self.defs.things[def].def_name)
            .and_then(|d| d.node.child("comps"))
            .is_some_and(|c| c.children.iter().any(|li| li.attr("Class") == Some(class)))
    }

    /// Cooked metadata of an item (ingredients, poisoning).
    pub fn item_meta(&self, item: ItemId) -> Option<&ItemMeta> {
        self.map.item_meta.get(&item)
    }
}

fn set_stage(pawn: &mut Pawn, new: DoBillStage) {
    if let Some(Job {
        kind: JobKind::DoBill { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new;
    }
}

/// The per-tick part of the bill job: walking, and using the table while
/// working (`UsedThisTick` is reported to the simulation).
pub(super) fn tick_do_bill(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::DoBill { giver, stage, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    if map.structure(giver).is_none() {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        DoBillStage::Start => JobEvent::None,
        DoBillStage::GotoIngredient | DoBillStage::CarryToTable | DoBillStage::GotoTable => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                match stage {
                    DoBillStage::GotoIngredient => JobEvent::ArrivedAtIngredient,
                    DoBillStage::CarryToTable => JobEvent::ArrivedAtTableWithIngredient,
                    _ => {
                        set_stage(pawn, DoBillStage::Start);
                        JobEvent::ArrivedToWorkBill
                    }
                }
            }
        }
        DoBillStage::Work { .. } => JobEvent::BillTableUsed(giver),
    })
}
