//! Production bills (docs/research.md §58): a work table's ordered bills
//! (`BillStack`, `Bill_Production`), recipe data helpers (`RecipeDef`
//! work amounts, ingredient values) and the ingredient allocators
//! (`WorkGiver_DoBill.TryFindBestBillIngredientsInSet_AllowMix` and
//! `_NoMix`).

use rimworld_defs::{DefId, GameDefs, RecipeDef, ThingDef};
use serde::{Deserialize, Serialize};

use crate::grid::Cell;
use crate::map::ItemId;
use crate::storage::ThingFilter;

/// `BillRepeatModeDef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    RepeatCount,
    Forever,
    TargetCount,
}

/// `BillStoreModeDef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreMode {
    BestStockpile,
    DropOnFloor,
}

/// A production bill (`Bill_Production`) with the constructor's
/// defaults.
// COMPATIBILITY TODO: currently approximate — pawn restrictions, specific
// stockpiles, the hit point/quality/tainted/equipped/stuff options of
// target counts, styles and DLC fields are not modelled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bill {
    /// `loadID`.
    pub id: u32,
    pub recipe: DefId<RecipeDef>,
    pub suspended: bool,
    /// `ingredientSearchRadius` (default 999).
    pub search_radius: f32,
    /// `allowedSkillRange` (inclusive, default 0~20).
    pub allowed_skill_range: (i32, i32),
    pub ingredient_filter: ThingFilter,
    /// `nextTickToSearchForIngredients` (not in the game's save data).
    #[serde(skip)]
    pub next_search_tick: u64,
    pub repeat_mode: RepeatMode,
    /// Iterations left (`repeatCount`, default 1).
    pub repeat_count: i32,
    /// `targetCount` (default 10).
    pub target_count: i32,
    pub store_mode: StoreMode,
    pub pause_when_satisfied: bool,
    /// `unpauseWhenYouHave` (default 5).
    pub unpause_when_you_have: i32,
    pub paused: bool,
}

impl Bill {
    /// `new Bill_Production(recipe)`: the recipe's default ingredient
    /// filter copied.
    pub fn new(defs: &GameDefs, recipe: DefId<RecipeDef>, id: u32) -> Self {
        let r = &defs.recipes[recipe];
        let ingredient_filter = r
            .default_ingredient_filter
            .as_ref()
            .map(|s| ThingFilter::from_spec(defs, s))
            .unwrap_or_default();
        Self {
            id,
            recipe,
            suspended: false,
            search_radius: 999.0,
            allowed_skill_range: (0, 20),
            ingredient_filter,
            next_search_tick: 0,
            repeat_mode: RepeatMode::RepeatCount,
            repeat_count: 1,
            target_count: 10,
            store_mode: StoreMode::BestStockpile,
            pause_when_satisfied: false,
            unpause_when_you_have: 5,
            paused: false,
        }
    }

    /// `Bill_Production.ShouldDoNow` (it can change `paused`); `products`
    /// counts the recipe's products for target counts.
    pub fn should_do_now(&mut self, products: impl FnOnce() -> i32) -> bool {
        if self.repeat_mode != RepeatMode::TargetCount {
            self.paused = false;
        }
        if self.suspended {
            return false;
        }
        match self.repeat_mode {
            RepeatMode::Forever => true,
            RepeatMode::RepeatCount => self.repeat_count > 0,
            RepeatMode::TargetCount => {
                let n = products();
                if self.pause_when_satisfied && n >= self.target_count {
                    self.paused = true;
                }
                if n <= self.unpause_when_you_have || !self.pause_when_satisfied {
                    self.paused = false;
                }
                !self.paused && n < self.target_count
            }
        }
    }

    /// `Bill.PawnAllowedToStartAnew` without pawn restrictions: the work
    /// skill within the allowed range.
    pub fn pawn_allowed(&self, skill_level: Option<i32>) -> bool {
        skill_level
            .is_none_or(|l| l >= self.allowed_skill_range.0 && l <= self.allowed_skill_range.1)
    }

    /// `Bill_Production.Notify_IterationCompleted`.
    pub fn iteration_completed(&mut self) {
        if self.repeat_mode == RepeatMode::RepeatCount && self.repeat_count > 0 {
            self.repeat_count -= 1;
        }
    }
}

/// A recipe's filters resolved once: the recipe-level hard filter and one
/// per ingredient row, plus whether each row is fixed (one def).
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeFilters {
    pub fixed: ThingFilter,
    pub rows: Vec<(ThingFilter, Option<DefId<ThingDef>>)>,
}

impl RecipeFilters {
    pub fn new(defs: &GameDefs, recipe: &RecipeDef) -> Self {
        Self {
            fixed: ThingFilter::from_spec(defs, &recipe.fixed_ingredient_filter),
            rows: recipe
                .ingredients
                .iter()
                .map(|i| {
                    (
                        ThingFilter::from_spec(defs, &i.filter),
                        i.filter.single_def().and_then(|d| defs.things.id(d)),
                    )
                })
                .collect(),
        }
    }
}

/// `ThingDef.AllRecipes`: its own `recipes`, then recipes naming it among
/// their `recipeUsers`, in database order.
pub fn all_recipes(defs: &GameDefs, building: DefId<ThingDef>) -> Vec<DefId<RecipeDef>> {
    let d = &defs.things[building];
    let mut out: Vec<DefId<RecipeDef>> = d
        .recipes
        .iter()
        .filter_map(|r| defs.recipes.id(r))
        .collect();
    for (id, r) in defs.recipes.iter() {
        if r.recipe_users.iter().any(|u| u == &d.def_name) && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// `RecipeDef.WorkAmountForStuff(null)`: `workAmount`, else the first
/// product's `WorkToMake`.
pub fn work_amount(defs: &GameDefs, recipe: &RecipeDef) -> f32 {
    if recipe.work_amount >= 0.0 {
        return recipe.work_amount;
    }
    recipe
        .products
        .first()
        .and_then(|(p, _)| defs.things.get(p))
        .map_or(0.0, |p| crate::stats::def_stat(defs, p, None, "WorkToMake"))
}

/// `IngredientValueGetter.ValuePerUnitOf`: nutrition per unit for
/// nutrition recipes (0 for non-food), else the def's volume per unit
/// for stuff and 1 otherwise (`IngredientValueGetter_Volume`).
pub fn value_per_unit(defs: &GameDefs, recipe: &RecipeDef, def: DefId<ThingDef>) -> f32 {
    let d = &defs.things[def];
    if recipe.ingredient_value_getter_class.as_deref() == Some("IngredientValueGetter_Nutrition") {
        let n = crate::food::unit_nutrition(defs, d);
        if d.ingestible.is_some() && n > 0.0 {
            n
        } else {
            0.0
        }
    } else if d.stuff_props.is_some() {
        d.volume_per_unit()
    } else {
        1.0
    }
}

/// `workLeft -= rate * delta`: as the game's runtime evaluates it, the
/// product is not rounded to binary32 before the subtraction (only the
/// result is). This is what leaves the bulk meal's recorded −1.788e-7.
pub fn subtract_work(work_left: f32, rate: f32, delta: i32) -> f32 {
    (work_left as f64 - rate as f64 * delta as f64) as f32
}

/// `CompFoodPoisonable.Notify_RecipeProduced`: the room's chance first
/// (a success skips the cook roll), then the cook's `FoodPoisonChance`.
/// Returns the poison percent and cause (0 / Unknown when clean).
pub fn roll_food_poison(
    rng: &mut crate::rand::Rand,
    room_chance: f32,
    cook_chance: f32,
) -> (f32, crate::map::PoisonCause) {
    use crate::map::PoisonCause;
    if rng.chance(room_chance) {
        (1.0, PoisonCause::FilthyKitchen)
    } else if rng.chance(cook_chance) {
        (1.0, PoisonCause::IncompetentCook)
    } else {
        (0.0, PoisonCause::Unknown)
    }
}

/// A candidate ingredient for allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub position: Cell,
    pub stack_count: u32,
    /// Whether each ingredient row's filter (with special filters) and the
    /// bill's filter allow it.
    pub row_allows: [bool; 4],
    pub bill_allows: bool,
}

/// `TryFindBestBillIngredientsInSet_AllowMix`: candidates sorted by value
/// per unit, then squared distance to `root` (the .NET sort); each row in
/// order takes min(ceil(remaining / value), stack) from each allowed
/// candidate until at most 0.0001 remains. A thing taken by an earlier row
/// is not reduced for later rows (as in the game). `None` when a row can't
/// be met.
pub fn allocate_mixing(
    defs: &GameDefs,
    recipe: &RecipeDef,
    fixed_rows: &[bool],
    candidates: &mut [Candidate],
    root: Cell,
) -> Option<Vec<(ItemId, u32)>> {
    let value = |c: &Candidate| value_per_unit(defs, recipe, c.def);
    let dist = |c: &Candidate| {
        let (dx, dz) = (c.position.x - root.x, c.position.z - root.z);
        dx * dx + dz * dz
    };
    crate::netsort::sort(candidates, |a, b| {
        let (va, vb) = (value(a), value(b));
        if va != vb {
            va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            dist(a).cmp(&dist(b))
        }
    });
    let mut chosen: Vec<(ItemId, u32)> = Vec::new();
    for (row, ing) in recipe.ingredients.iter().enumerate() {
        let mut remaining = ing.count;
        for c in candidates.iter() {
            let fixed = fixed_rows.get(row).copied().unwrap_or(false);
            if !c.row_allows.get(row).copied().unwrap_or(false) || !(fixed || c.bill_allows) {
                continue;
            }
            let v = value(c);
            let take = ((remaining / v).ceil() as i64).min(c.stack_count as i64) as u32;
            // `ThingCountUtility.AddToList`: counts for the same thing add up.
            match chosen.iter_mut().find(|(id, _)| *id == c.id) {
                Some(e) => e.1 += take,
                None => chosen.push((c.id, take)),
            }
            remaining -= take as f32 * v;
            if remaining <= 0.0001 {
                break;
            }
        }
        if remaining > 0.0001 {
            return None;
        }
    }
    Some(chosen)
}

/// `TryFindBestIngredientsInSet_NoMixHelper`: candidates sorted by
/// squared distance to `root` (the .NET sort), their counts summed per def
/// in first-seen order; each row in order looks through the defs: one
/// whose total covers ceil(count / value per unit), which the row's
/// filter allows and (unless the row is fixed) the bill's filter allows,
/// is taken from its things in order (floor of what is still needed,
/// within what is left of each) until less than 0.001 remains. `None`
/// when a row can't be met.
pub fn allocate_no_mix(
    defs: &GameDefs,
    recipe: &RecipeDef,
    filters: &RecipeFilters,
    bill_filter: &ThingFilter,
    candidates: &mut [Candidate],
    root: Cell,
) -> Option<Vec<(ItemId, u32)>> {
    let dist = |c: &Candidate| {
        let (dx, dz) = (c.position.x - root.x, c.position.z - root.z);
        (dx * dx + dz * dz) as f32
    };
    crate::netsort::sort(candidates, |a, b| {
        dist(a)
            .partial_cmp(&dist(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // `DefCountList.GenerateFrom`.
    let mut counts: Vec<(DefId<ThingDef>, f32)> = Vec::new();
    for c in candidates.iter() {
        match counts.iter_mut().find(|(d, _)| *d == c.def) {
            Some(e) => e.1 += c.stack_count as f32,
            None => counts.push((c.def, c.stack_count as f32)),
        }
    }
    let mut chosen: Vec<(ItemId, u32)> = Vec::new();
    for (row, ing) in recipe.ingredients.iter().enumerate() {
        let (row_filter, single) = &filters.rows[row];
        let mut found = false;
        for k in 0..counts.len() {
            let (def, total) = counts[k];
            let mut need = (ing.count / value_per_unit(defs, recipe, def)).ceil();
            if (!recipe.ignore_ingredient_count_take_entire_stacks && need > total)
                || !row_filter.allows(def)
                || (single.is_none() && !bill_filter.allows(def))
            {
                continue;
            }
            for c in candidates.iter().filter(|c| c.def == def) {
                let already = chosen
                    .iter()
                    .find(|(id, _)| *id == c.id)
                    .map_or(0, |(_, n)| *n);
                let left = c.stack_count.saturating_sub(already);
                if left == 0 {
                    continue;
                }
                let take = if recipe.ignore_ingredient_count_take_entire_stacks {
                    left
                } else {
                    (need.floor() as u32).min(left)
                };
                match chosen.iter_mut().find(|(id, _)| *id == c.id) {
                    Some(e) => e.1 += take,
                    None => chosen.push((c.id, take)),
                }
                if recipe.ignore_ingredient_count_take_entire_stacks {
                    return Some(chosen);
                }
                need -= take as f32;
                if need < 0.001 {
                    found = true;
                    // As the game does, the leftover (not the amount taken)
                    // is subtracted from the def's count.
                    counts[k].1 -= need;
                    break;
                }
            }
            if found {
                if counts[k].1 == 0.0 {
                    counts.remove(k);
                }
                break;
            }
        }
        if !found {
            return None;
        }
    }
    Some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill(mode: RepeatMode) -> Bill {
        Bill {
            id: 1,
            recipe: DefId::from_index(0),
            suspended: false,
            search_radius: 999.0,
            allowed_skill_range: (0, 20),
            ingredient_filter: ThingFilter::default(),
            next_search_tick: 0,
            repeat_mode: mode,
            repeat_count: 1,
            target_count: 10,
            store_mode: StoreMode::BestStockpile,
            pause_when_satisfied: false,
            unpause_when_you_have: 5,
            paused: false,
        }
    }

    /// The report's fixture D: target 10, pause when satisfied, unpause at
    /// 5 (counts queried in this order).
    #[test]
    fn target_count_hysteresis() {
        let mut b = bill(RepeatMode::TargetCount);
        b.pause_when_satisfied = true;
        for (n, should, paused) in [
            (10, false, true),
            (9, false, true),
            (6, false, true),
            (5, true, false),
            (4, true, false),
            (10, false, true),
        ] {
            assert_eq!(b.should_do_now(|| n), should, "count {n}");
            assert_eq!(b.paused, paused, "count {n}");
        }
    }

    /// The external report's recorded work values (binary32 bits):
    /// fixture A at table factor 0.56 (bits 3F0F5C29) and fixture B at
    /// 0.8, 15-tick intervals.
    #[test]
    fn work_subtraction_matches_recorded_bits() {
        let a = f32::from_bits(0x3F0F_5C29);
        let mut left = 300.0f32;
        left = subtract_work(left, a, 15);
        assert_eq!(left.to_bits(), 0x4391_CCCD, "291.6");
        left = subtract_work(left, a, 15);
        assert_eq!(left.to_bits(), 0x438D_999A, "283.2");
        let b = f32::from_bits(0x3F4C_CCCD);
        let mut left = 1200.0f32;
        for _ in 0..99 {
            left = subtract_work(left, b, 15);
        }
        assert_eq!(left.to_bits(), 0x4140_0000, "12 left at 1485 ticks");
        left = subtract_work(left, b, 15);
        assert_eq!(left.to_bits(), 0xB440_0000, "-1.78813934e-7 at 1500 ticks");
    }

    /// The report's fixture F: room 0.02 (bits 3CA3D70A), cook 0.001
    /// (3A83126F); seeds 1 and 7 fail both rolls (two draws), seed 664's
    /// first draw passes the kitchen roll (one draw).
    #[test]
    fn food_poison_rolls_match_the_recorded_seeds() {
        use crate::map::PoisonCause;
        let room = f32::from_bits(0x3CA3_D70A);
        let cook = f32::from_bits(0x3A83_126F);
        for (seed, draws, pct, cause) in [
            (1, 2, 0.0, PoisonCause::Unknown),
            (7, 2, 0.0, PoisonCause::Unknown),
            (664, 1, 1.0, PoisonCause::FilthyKitchen),
        ] {
            let mut rng = crate::rand::Rand::new(0);
            rng.push_state_seeded(seed);
            let got = roll_food_poison(&mut rng, room, cook);
            assert_eq!(got, (pct, cause), "seed {seed}");
            assert_eq!(rng.state().1, draws, "seed {seed} draws");
        }
    }

    /// The report's fixture C: from seed 7 at counter 0 the failed-search
    /// wait is 562 ticks (one draw).
    #[test]
    fn failed_search_wait_matches_the_recorded_seed() {
        let mut rng = crate::rand::Rand::new(0);
        rng.push_state_seeded(7);
        let (lo, hi) = crate::cook::RECHECK_FAILED_BILL_TICKS;
        assert_eq!(rng.range_inclusive(lo, hi), 562);
        assert_eq!(rng.state().1, 1);
    }

    #[test]
    fn repeat_count_and_suspension() {
        let mut b = bill(RepeatMode::RepeatCount);
        assert!(b.should_do_now(|| 0));
        b.iteration_completed();
        assert_eq!(b.repeat_count, 0);
        assert!(!b.should_do_now(|| 0));
        b.iteration_completed();
        assert_eq!(b.repeat_count, 0, "never negative");
        let mut f = bill(RepeatMode::Forever);
        f.paused = true;
        assert!(f.should_do_now(|| 0));
        assert!(!f.paused, "non-target modes clear paused");
        f.suspended = true;
        assert!(!f.should_do_now(|| 0));
    }
}
