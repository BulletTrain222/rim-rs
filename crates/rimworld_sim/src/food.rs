//! Food: choosing what to eat and the rules of the Ingest job
//! (docs/research.md §16).
//!
//! Implemented from the game's behaviour: the `JobGiver_GetFood` priority,
//! `FoodUtility`'s best-food scan and optimality score, the ingest count
//! rules and the chew duration.

use rimworld_defs::{FoodPreferability, GameDefs, RaceProperties, ThingDef};

use crate::cell_finder::{
    IngestionSpotOrder, MapView, StandSpotSearch, ingestion_place_spot, spot_to_stand_during_job,
};
use crate::grid::Cell;
use crate::map::{Item, ItemId, Map};
use crate::needs::HungerCategory;
use crate::path::{COLONIST_HEURISTIC_STRENGTH, MoveCosts, PathGrid, find_path};
use crate::rand::Rand;

/// `JobGiver_GetFood` priority when the pawn wants to eat.
pub const GET_FOOD_PRIORITY: f32 = 9.5;
/// Things whose cell costs more than this are eaten from an adjacent cell
/// (`PathEndMode.ClosestTouch` resolves to `Touch`); cheaper ones are
/// walked onto (`OnCell`).
const CLOSEST_TOUCH_MAX_PATH_COST: u32 = 30;
/// Optimality bonus/penalty per mood effect of the thoughts eating would
/// cause (`FoodOptimalityEffectFromMoodCurve`).
const MOOD_OPTIMALITY_CURVE: [(f32, f32); 6] = [
    (-100.0, -600.0),
    (-10.0, -100.0),
    (-5.0, -70.0),
    (-1.0, -50.0),
    (0.0, 0.0),
    (100.0, 800.0),
];

/// Linear interpolation over curve points, clamped at both ends (the
/// game's `SimpleCurve.Evaluate`).
pub fn evaluate_curve(points: &[(f32, f32)], x: f32) -> f32 {
    let (first, last) = (points[0], points[points.len() - 1]);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    let i = points.iter().position(|p| p.0 > x).expect("inside range");
    let (a, b) = (points[i - 1], points[i]);
    a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0)
}

/// The eater, as food selection sees it.
#[derive(Debug, Clone, Copy)]
pub struct Eater<'a> {
    pub race: &'a RaceProperties,
    pub humanlike: bool,
    pub at: Cell,
    pub move_costs: MoveCosts,
    pub hunger: HungerCategory,
    /// Temperature the food sits at (rot timing), if known.
    pub temperature: Option<f32>,
}

impl Eater<'_> {
    /// Whether the eater has a mood need, so food thoughts matter (the
    /// Mood need's `minIntelligence` is Humanlike).
    fn has_mood(&self) -> bool {
        self.humanlike
    }
}

/// Nutrition of one item of `def` (`Nutrition` stat).
pub fn unit_nutrition(defs: &GameDefs, def: &ThingDef) -> f32 {
    defs.base_stat(def, "Nutrition").unwrap_or(0.0)
}

/// `JobGiver_GetFood.GetPriority`.
// COMPATIBILITY TODO: currently approximate — pawns that should be fed by
// someone else (downed, prisoners) are not recognised.
pub fn get_food_priority(
    food_percent: f32,
    hunger: HungerCategory,
    min_category: HungerCategory,
    max_level_percentage: f32,
    want_eat: f32,
) -> f32 {
    if hunger < min_category || food_percent > max_level_percentage {
        0.0
    } else if food_percent < want_eat {
        GET_FOOD_PRIORITY
    } else {
        0.0
    }
}

/// Least preferred food a pawn will consider.
// COMPATIBILITY TODO: currently approximate — wild men count as humanlike.
pub fn min_preferability(eater: &Eater<'_>) -> FoodPreferability {
    if !eater.humanlike {
        FoodPreferability::NeverForNutrition
    } else if eater.hunger == HungerCategory::Starving {
        FoodPreferability::DesperateOnly
    } else if eater.hunger >= HungerCategory::UrgentlyHungry {
        FoodPreferability::RawBad
    } else {
        FoodPreferability::MealAwful
    }
}

/// How good eating `def` at Manhattan distance `dist` would be
/// (`FoodUtility.FoodOptimality`).
// COMPATIBILITY TODO: currently approximate — rot (fresh food about to rot
// gets +12, rotten food a thought), ingredient, trait and ideology thoughts
// are not modelled.
pub fn food_optimality(
    defs: &GameDefs,
    eater: &Eater<'_>,
    def: &ThingDef,
    dist: f32,
    rot: f32,
) -> f32 {
    let Some(ing) = &def.ingestible else {
        return 0.0;
    };
    let mut score = 300.0 - dist;
    match ing.preferability {
        FoodPreferability::NeverForNutrition => return -9_999_999.0,
        FoodPreferability::DesperateOnly => score -= 150.0,
        FoodPreferability::DesperateOnlyForHumanlikes if eater.humanlike => score -= 150.0,
        _ => {}
    }
    if eater.has_mood() {
        for thought in [&ing.taste_thought, &ing.special_thought_direct]
            .into_iter()
            .flatten()
        {
            if let Some(mood) = defs.thought_first_stage_mood(thought) {
                score += evaluate_curve(&MOOD_OPTIMALITY_CURVE, mood);
            }
        }
    }
    // Fresh food about to rot (within 30000 ticks at the rounded
    // temperature) is preferred.
    if let (Some(props), Some(temp)) = (&def.rottable, eater.temperature) {
        let fresh = rot < props.ticks_to_rot_start() as f32;
        if fresh && ticks_until_rot(props, rot, temp.round_ties_even()) < 30_000 {
            score += 12.0;
        }
    }
    if eater.humanlike {
        score += ing.optimality_offset_humanlikes;
    } else {
        score += ing.optimality_offset_feeding_animals;
    }
    score
}

/// `CompRottable.TicksUntilRotAtTemp`.
pub fn ticks_until_rot(props: &rimworld_defs::RottableProperties, rot: f32, temp: f32) -> i32 {
    let rate = crate::sim::rot_rate_at_temperature(temp);
    if rate <= 0.0 {
        return 72_000_000;
    }
    let left = props.ticks_to_rot_start() as f32 - rot;
    if left <= 0.0 {
        return 0;
    }
    (left / rate).round_ties_even() as i32
}

/// The cell a pawn walks to in order to touch a thing on `item_cell`
/// (`PathEndMode.ClosestTouch`): the cell itself when it can be stood on
/// cheaply, else where the Touch search ends (the first cell on or next to
/// it taken from the frontier; a diagonal touch needs a walkable side).
// COMPATIBILITY TODO: currently approximate — doors on the side cells of a
// diagonal touch are not excluded here.
pub fn touch_destination(
    grid: &PathGrid,
    item_cell: Cell,
    from: Cell,
    costs: MoveCosts,
) -> Option<Cell> {
    if grid
        .cost(item_cell)
        .is_some_and(|c| c <= CLOSEST_TOUCH_MAX_PATH_COST)
    {
        return find_path(grid, from, item_cell, costs, COLONIST_HEURISTIC_STRENGTH)
            .ok()
            .map(|_| item_cell);
    }
    let allowed = |c: Cell| {
        c.x == item_cell.x
            || c.z == item_cell.z
            || grid.walkable(Cell::new(c.x, item_cell.z))
            || grid.walkable(Cell::new(item_cell.x, c.z))
    };
    crate::path::find_path_touch(
        grid,
        from,
        item_cell,
        allowed,
        costs,
        COLONIST_HEURISTIC_STRENGTH,
    )
    .ok()
    .map(|p| p.cells.last().copied().unwrap_or(from))
}

// DIFFERENTIAL VERIFIED: food choice (preferability by hunger, optimality
// with the mood curve and meal offset) against 3 original-game eating traces.
/// The best food on the map for `eater` (`FoodUtility.BestFoodSourceOnMap`
/// for spawned items): the highest optimality among valid, reachable,
/// reservable items; a later item with an equal score wins.
// COMPATIBILITY TODO: currently approximate — animals scan like humans
// (the game searches nearby regions for them); social properness, food
// policies, freshness and plants/corpses/dispensers are not modelled.
pub fn best_food_source(
    defs: &GameDefs,
    map: &Map,
    grid: &PathGrid,
    eater: &Eater<'_>,
    can_reserve: impl Fn(&Item) -> bool,
) -> Option<ItemId> {
    let min_pref = min_preferability(eater);
    let mut best: Option<ItemId> = None;
    let mut best_score = f32::MIN;
    for item in map.items() {
        if item.forbidden {
            continue;
        }
        let def = &defs.things[item.def];
        // `allowCorpse`: only for animals and badly malnourished pawns.
        // COMPATIBILITY TODO: currently approximate — malnutrition above
        // 0.4 (which lets humans eat corpses) is not checked.
        if def.corpse_of.is_some() && eater.humanlike {
            continue;
        }
        let dist =
            ((eater.at.x - item.position.x).abs() + (eater.at.z - item.position.z).abs()) as f32;
        let score = food_optimality(defs, eater, def, dist, item.rot);
        if score < best_score {
            continue;
        }
        let Some(ing) = &def.ingestible else { continue };
        if ing.preferability < min_pref
            || !eater.race.can_ever_eat(def)
            || unit_nutrition(defs, def) <= 0.0
        {
            continue;
        }
        // `CanReserve(food, 10, 1)`.
        if !can_reserve(item) {
            continue;
        }
        if touch_destination(grid, item.position, eater.at, eater.move_costs).is_none() {
            continue;
        }
        best = Some(item.id);
        best_score = score;
    }
    best
}

// DIFFERENTIAL VERIFIED: job count, units eaten and chew duration against
// 3 original-game eating traces.
/// Units to take for `nutrition_wanted` (`FoodUtility.WillIngestStackCountOf`
/// for a pawn with a food need): rounded to nearest, ties to even, capped by
/// `maxNumToIngestAtOnce`, at least 1.
pub fn will_ingest_stack_count(nutrition_wanted: f32, unit: f32, max_at_once: i32) -> u32 {
    let mut n = if nutrition_wanted <= 0.0001 {
        0
    } else {
        ((nutrition_wanted / unit).round_ties_even() as i32).max(1)
    };
    if max_at_once > 0 {
        n = n.min(max_at_once);
    }
    n.max(1) as u32
}

/// Units actually eaten when finishing (`Thing.IngestedCalculateAmounts`):
/// rounded up, capped by the stack and `maxNumToIngestAtOnce`, at least 1.
pub fn ingested_count(nutrition_wanted: f32, unit: f32, stack: u32, max_at_once: i32) -> u32 {
    let mut n = (nutrition_wanted / unit).ceil() as i64;
    n = n.min(stack as i64);
    if max_at_once > 0 {
        n = n.min(max_at_once as i64);
    }
    n.max(1) as u32
}

/// Chew duration in ticks: `baseIngestTicks / EatingSpeed`, rounded to
/// nearest (ties to even) as the game does in single precision.
pub fn chew_ticks(base_ingest_ticks: i32, eating_speed: f32, use_eating_speed: bool) -> i32 {
    let multiplier = if use_eating_speed {
        1.0 / eating_speed
    } else {
        1.0
    };
    (base_ingest_ticks as f32 * multiplier).round_ties_even() as i32
}

/// A spot to stand and eat near `at` (`RCellFinder.SpotToChewStandingNear`,
/// no chairs yet): a `SpotToStandDuringJob` cell where the food could be
/// put down next to the pawn and the cell can be reserved.
/// `Toils_Ingest.TryFindChairOrSpot`, chair part: the nearest sittable
/// building within `radius` that has a reservable sitting cell with an
/// eating surface (a table) on a cardinal neighbour; returns that cell.
// COMPATIBILITY TODO: currently approximate — nearest by straight-line
// distance with connectivity instead of the region search; forbidding,
// fog, danger and social properness are not modelled.
pub fn chair_spot(
    view: &MapView<'_>,
    at: Cell,
    radius: f32,
    can_reserve_cell: &dyn Fn(Cell) -> bool,
    can_reserve_chair: &dyn Fn(crate::map::ItemId) -> bool,
) -> Option<Cell> {
    if radius <= 0.0 {
        return None;
    }
    let eat_surface = |c: Cell| {
        view.map.size().contains(c)
            && view.map.buildings[c]
                .is_some_and(|b| view.defs.things[b].surface_type.as_deref() == Some("Eat"))
    };
    view.map
        .structures()
        .iter()
        .filter(|s| {
            view.defs.things[s.def]
                .building
                .as_ref()
                .is_some_and(|b| b.is_sittable)
        })
        .filter(|s| {
            let c = s.footprint.center;
            ((c.x - at.x).pow(2) + (c.z - at.z).pow(2)) as f32 <= radius * radius
                && view.regions.connected(at, c)
                && can_reserve_chair(s.id)
        })
        .filter_map(|s| {
            // `TryFindFreeSittingSpotOnThing`: the first reservable cell.
            let spot = s.footprint.cells().find(|&c| can_reserve_cell(c))?;
            let d = s.footprint.center;
            Cell::NEIGHBORS_8[..4]
                .iter()
                .any(|&n| eat_surface(spot + n))
                .then_some((spot, (d.x - at.x).pow(2) + (d.z - at.z).pow(2)))
        })
        .min_by_key(|&(_, d)| d)
        .map(|(c, _)| c)
}

#[allow(clippy::too_many_arguments)]
pub fn spot_to_chew_standing_near(
    view: &MapView<'_>,
    order: &mut IngestionSpotOrder,
    at: Cell,
    food_def: rimworld_defs::DefId<ThingDef>,
    destination_free: &dyn Fn(Cell) -> bool,
    can_reserve_cell: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Option<Cell> {
    let same_def = |p: Cell| view.map.items_at(p).any(|i| i.def == food_def);
    // Filth does not block placing things (`GenPlace.HaulPlaceBlockerIn`).
    let blocked = |p: Cell| view.map.items_at(p).any(|i| !i.is_filth());
    let search = StandSpotSearch {
        position: at,
        destination_free,
        blocked: &blocked,
    };
    spot_to_stand_during_job(
        view,
        &search,
        |c, rng| {
            ingestion_place_spot(view, order, c, &same_def, rng).is_some()
                && view.grid.walkable(c)
                && can_reserve_cell(c)
        },
        rng,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mood_curve_matches_game_points() {
        let f = |x| evaluate_curve(&MOOD_OPTIMALITY_CURVE, x);
        assert_eq!(f(-7.0), -82.0); // AteRawFood
        assert_eq!(f(-200.0), -600.0);
        assert_eq!(f(0.0), 0.0);
        assert_eq!(f(5.0), 40.0);
        assert_eq!(f(500.0), 800.0);
    }

    #[test]
    fn ingest_counts_round_differently() {
        // Job count: nearest; finishing: up (verified in the eat traces).
        assert_eq!(will_ingest_stack_count(0.9013, 0.05, 0), 18);
        assert_eq!(ingested_count(0.904, 0.05, 18, 0), 18);
        assert_eq!(ingested_count(0.904, 0.05, 75, 0), 19);
        assert_eq!(will_ingest_stack_count(0.754, 0.9, 1), 1);
        assert_eq!(will_ingest_stack_count(0.0, 0.9, 0), 1);
        assert_eq!(ingested_count(0.766, 0.9, 1, 1), 1);
    }

    #[test]
    fn chew_duration_rounds_like_the_game() {
        assert_eq!(chew_ticks(500, 0.982, true), 509);
        assert_eq!(chew_ticks(500, 1.0, true), 500);
        assert_eq!(chew_ticks(500, 0.5, false), 500);
    }

    #[test]
    fn food_priority() {
        use HungerCategory::*;
        assert_eq!(get_food_priority(0.25, Fed, Fed, 1.0, 0.3), 9.5);
        assert_eq!(get_food_priority(0.31, Fed, Fed, 1.0, 0.3), 0.0);
        assert_eq!(
            get_food_priority(0.2, Hungry, UrgentlyHungry, 1.0, 0.3),
            0.0
        );
    }
}
