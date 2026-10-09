//! Interpreter for the game's `ThinkTreeDef` behaviour trees.
//!
//! Semantics (from the XML structure and community documentation):
//! - `ThinkNode_Priority`: try children in order; the first job found wins.
//! - `ThinkNode_Tagger`: like Priority, and tags the resulting job
//!   (`tagToGive`, e.g. `Idle`).
//! - `ThinkNode_Subtree`: evaluate the `treeDef` tree's root.
//! - `ThinkNode_SubtreesByTag`: evaluate every tree whose `insertTag` matches,
//!   highest `insertPriority` first.
//! - `ThinkNode_Conditional*`: if the condition holds (negated by
//!   `<invert>true</invert>`), behave like Priority over the children.
//! - `JobGiver_*`: leaves that produce a job or nothing.
//!
//! Anything we do not implement yet yields no job, so evaluation falls
//! through to later siblings exactly as when a real job giver finds nothing
//! to do. `ThinkNode_PrioritySorter` shuffles its children, then runs them
//! by descending priority (docs/research.md §12).

use std::collections::BTreeSet;

use rimworld_defs::{GameDefs, RaceProperties, ThinkNodeSpec, ThinkTreeDef};

use crate::cell_finder::{IngestionSpotOrder, MapView, Wanderer};
use crate::clean::CleanFilth;
use crate::food::{
    Eater, best_food_source, get_food_priority, spot_to_chew_standing_near, touch_destination,
    unit_nutrition, will_ingest_stack_count,
};
use crate::grid::Cell;
use crate::haul::HaulGeneral;
use crate::job::{IngestStage, Job, JobDefs, JobKind, WanderParams, WanderRoot, think_idle};
use crate::map::ItemId;
use crate::map::{Item, Map};
use crate::needs::HungerCategory;
use crate::path::{LocomotionUrgency, MoveCosts, PathGrid};
use crate::rand::Rand;
use crate::region::Regions;
use crate::reservation::{Claimant, DestinationManager, ReservationManager, STACK_ALL, Target};
use crate::rest::{find_ground_sleep_spot, get_rest_priority};
use crate::work::{GiverLists, WorkContext, WorkGiver, WorkJob, try_issue_job, work_priority};

const MAX_DEPTH: usize = 64;
/// How long `JobGiver_IdleError` makes a pawn stand (assumption).
// COMPATIBILITY TODO: currently approximate — wait duration of JobGiver_IdleError not verified.
const IDLE_ERROR_WAIT_TICKS: u32 = 100;

/// Facts about the thinking pawn that conditions can read.
#[derive(Debug, Clone)]
pub struct PawnFacts<'a> {
    pub kind_def_name: &'a str,
    pub is_colonist: bool,
    pub at: Cell,
    pub next_idle_is_wait: bool,
    pub move_costs: MoveCosts,
    /// Rest need level, if the pawn has one.
    pub rest_level: Option<f32>,
    pub starving: bool,
    pub humanlike: bool,
    /// Local hour of day (0-23).
    pub hour: u32,
    /// The current time assignment (`timetable.CurrentAssignment`).
    pub assignment: crate::rest::TimeAssignment,
    /// The pawn's food need, if it has one.
    pub food: Option<FoodFacts>,
    pub race: Option<&'a RaceProperties>,
    /// Has work settings (`EverWork`).
    pub ever_work: bool,
    /// `CarryingCapacity` stat.
    pub carrying_capacity: f32,
    /// Capacities the pawn is incapable of (work givers need some).
    pub incapable: &'a [String],
    /// What the safe-temperature giver needs, if known.
    pub temperature: Option<TemperatureFacts>,
    /// The joy need and what recreation needs, if the pawn has one.
    pub joy: Option<crate::recreation::JoyFacts<'a>>,
    /// The active mental state's Def name.
    pub mental_state: Option<&'a str>,
    /// `Drafted`.
    pub drafted: bool,
    /// The active mental state's `stateClass`.
    pub mental_state_class: Option<&'a str>,
    /// For a manhunter: the pawn it would attack.
    pub manhunter_target: Option<crate::pawn::PawnId>,
    /// An animal whose season is outside its comfortable range
    /// (`ThinkNode_ConditionalAnimalWrongSeason`).
    pub wrong_season: bool,
    /// The ambient temperature is outside the safe range (comfortable
    /// ± 10, inclusive) (`ThinkNode_ConditionalDangerousTemperature`).
    pub dangerous_temperature: bool,
    /// Standing where the outdoor temperature applies.
    pub outdoor: bool,
    /// Some map edge cell is reachable (`CanReachMapEdge`).
    pub can_reach_map_edge: bool,
    /// `thingIDNumber` (hour-seeded chances).
    pub id_number: i32,
    pub downed: bool,
}

/// The thinking pawn's temperature situation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemperatureFacts {
    pub ambient: f32,
    /// `ComfortableTemperatureRange`.
    pub comfy: (f32, f32),
    /// `HasTemperatureInjury(Serious)`: hypothermia or heatstroke at stage
    /// index 3 or higher.
    pub serious_injury: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodFacts {
    pub level: f32,
    pub max: f32,
    pub want_eat: f32,
    pub hunger: HungerCategory,
}

impl FoodFacts {
    pub fn percent(&self) -> f32 {
        if self.max > 0.0 {
            self.level / self.max
        } else {
            0.0
        }
    }
}

/// What construction work givers need beyond the map.
pub struct ConstructionEnv<'a> {
    pub enroute: &'a crate::construct::EnrouteManager,
    /// Every spawned pawn's position.
    pub pawn_cells: &'a [(crate::pawn::PawnId, Cell)],
    /// Ids of current `HaulToContainer` jobs.
    pub delivery_jobs: &'a [crate::reservation::JobId],
    /// The thinking pawn's Construction skill level.
    pub construction_level: i32,
    /// Downed colonists to rescue, with their beds.
    pub rescue: &'a [crate::rescue::RescueCandidate],
    /// Bedridden colonists to feed, with their food.
    pub feed: &'a [crate::rescue::FeedCandidate],
    /// Colonists in bed to tend.
    pub tend: &'a [crate::tend::TendCandidate],
    /// The thinking pawn as a patient.
    pub patient: crate::tend::PatientFacts,
    /// Outdoor temperature (growing season for the sow giver).
    pub outdoor_temperature: f32,
    /// Each room's temperature (cells' temperatures).
    pub room_temperatures: &'a [f32],
    /// Research benches (none while no project is chosen).
    pub research: &'a [crate::research::ResearchBench],
    /// Work tables and their bills.
    pub bills: Option<&'a crate::cook::BillEnv<'a>>,
    /// The Hunt job the hunting work giver would give (the closest
    /// eligible marked animal), if any.
    pub hunt: Option<Job>,
    /// For a wild animal: the food job `JobGiver_GetFood` would give.
    pub animal_food: Option<Job>,
    /// The same with `forceScanWholeMap` (computed when starving).
    pub animal_food_whole_map: Option<Job>,
}

pub struct ThinkContext<'a> {
    pub defs: &'a GameDefs,
    pub pawn: PawnFacts<'a>,
    pub map: &'a Map,
    pub grid: &'a PathGrid,
    pub job_defs: &'a JobDefs,
    pub default_wander: WanderParams,
    /// Cells other pawns are heading to or standing on.
    /// The thinking pawn, as reservations see it.
    pub claimant: Claimant,
    pub reservations: &'a ReservationManager,
    pub destinations: &'a DestinationManager,
    pub regions: &'a Regions,
    /// Free colonists' positions, in spawn order (colony wander root).
    pub colonists: &'a [Cell],
    /// The game's persistent ingestion-spot shuffle lists.
    pub ingest_order: &'a mut IngestionSpotOrder,
    /// The pawn's ordered work-giver lists, if it works.
    pub work: Option<&'a GiverLists>,
    pub tick: u64,
    /// Set by job givers that queue targets for their job.
    pub job_queue_out: Vec<ItemId>,
    pub rng: &'a mut Rand,
    /// Node classes visited that we do not implement (diagnostics).
    pub unsupported: BTreeSet<String>,
    /// Set by wander job givers: the pawn's new wait/walk flag.
    pub next_idle_is_wait_out: Option<bool>,
    /// Construction state (`None`: construction givers find nothing).
    pub construction: Option<ConstructionEnv<'a>>,
    /// Bed state (`None`: pawns sleep on the ground).
    pub beds: Option<BedEnv<'a>>,
    /// Each room's temperature, by room index.
    pub room_temperatures: &'a [f32],
    /// `TicksAbs` (hour-seeded chances).
    pub abs_tick: i64,
    /// The pawn's `thinkData`, written back after thinking.
    pub think_data: std::collections::BTreeMap<i32, i64>,
}

/// What the bed search needs about the thinking pawn and the beds.
pub struct BedEnv<'a> {
    pub owned: Option<crate::map::ItemId>,
    pub occupancy: crate::rest::Occupancy<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThinkResult {
    pub job: Job,
    /// Tag from the nearest enclosing `ThinkNode_Tagger`.
    pub tag: Option<String>,
    /// Node labels from the root to the job giver, for debugging.
    pub trail: Vec<String>,
    /// New value of the pawn's wait/walk alternation flag, if a wander job
    /// giver ran (it changes even when it gives no job).
    pub next_idle_is_wait: Option<bool>,
    /// Targets queued for the job.
    pub queue: Vec<ItemId>,
}

/// Evaluates `tree` for the pawn described by `ctx`.
pub fn think(tree: &ThinkTreeDef, ctx: &mut ThinkContext<'_>) -> Option<ThinkResult> {
    let root = tree.root.as_ref()?;
    let mut trail = vec![tree.def_name.clone()];
    let (job, tag) = eval(root, ctx, 0, &mut trail)?;
    Some(ThinkResult {
        job,
        tag,
        trail,
        next_idle_is_wait: ctx.next_idle_is_wait_out,
        queue: std::mem::take(&mut ctx.job_queue_out),
    })
}

fn label(node: &ThinkNodeSpec) -> String {
    let short = node
        .class
        .strip_prefix("ThinkNode_")
        .unwrap_or(&node.class)
        .to_owned();
    match node.class.as_str() {
        "ThinkNode_Subtree" => format!("{short}({})", node.param("treeDef").unwrap_or("?")),
        "ThinkNode_Tagger" => format!("{short}({})", node.param("tagToGive").unwrap_or("?")),
        "ThinkNode_SubtreesByTag" => {
            format!("{short}({})", node.param("insertTag").unwrap_or("?"))
        }
        _ => short,
    }
}

fn eval(
    node: &ThinkNodeSpec,
    ctx: &mut ThinkContext<'_>,
    depth: usize,
    trail: &mut Vec<String>,
) -> Option<(Job, Option<String>)> {
    if depth > MAX_DEPTH {
        return None;
    }
    trail.push(label(node));
    let result = eval_inner(node, ctx, depth, trail);
    if result.is_none() {
        trail.pop();
    }
    result
}

fn eval_children(
    node: &ThinkNodeSpec,
    ctx: &mut ThinkContext<'_>,
    depth: usize,
    trail: &mut Vec<String>,
) -> Option<(Job, Option<String>)> {
    node.sub_nodes
        .iter()
        .find_map(|c| eval(c, ctx, depth + 1, trail))
}

fn eval_inner(
    node: &ThinkNodeSpec,
    ctx: &mut ThinkContext<'_>,
    depth: usize,
    trail: &mut Vec<String>,
) -> Option<(Job, Option<String>)> {
    let class = node.class.as_str();
    match class {
        "ThinkNode_Priority" => eval_children(node, ctx, depth, trail),
        "ThinkNode_PrioritySorter" => {
            // Shuffle by inserting each child at a random index, then
            // repeatedly run the highest-priority child (> 0, >= minPriority).
            let min_priority: f32 = node
                .param("minPriority")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            let mut working: Vec<&ThinkNodeSpec> = Vec::with_capacity(node.sub_nodes.len());
            for child in &node.sub_nodes {
                let at = ctx.rng.range(0, working.len() as i32 - 1) as usize;
                working.insert(at.min(working.len()), child);
            }
            loop {
                let mut best: Option<(usize, f32)> = None;
                for (i, c) in working.iter().enumerate() {
                    let p = priority(c, &ctx.pawn);
                    if p > 0.0 && p >= min_priority && best.is_none_or(|(_, b)| p > b) {
                        best = Some((i, p));
                    }
                }
                let (i, _) = best?;
                if let Some(result) = eval(working[i], ctx, depth + 1, trail) {
                    return Some(result);
                }
                working.remove(i);
            }
        }
        "ThinkNode_Tagger" => {
            let (job, tag) = eval_children(node, ctx, depth, trail)?;
            Some((
                job,
                tag.or_else(|| node.param("tagToGive").map(str::to_owned)),
            ))
        }
        "ThinkNode_Subtree" => {
            let defs = ctx.defs;
            let tree = defs.think_trees.get(node.param("treeDef")?)?;
            eval(tree.root.as_ref()?, ctx, depth + 1, trail)
        }
        "ThinkNode_SubtreesByTag" => {
            let tag = node.param("insertTag")?;
            let defs = ctx.defs;
            let mut trees: Vec<&ThinkTreeDef> = defs
                .think_trees
                .iter()
                .map(|(_, t)| t)
                .filter(|t| t.insert_tag.as_deref() == Some(tag))
                .collect();
            trees.sort_by(|a, b| b.insert_priority.total_cmp(&a.insert_priority));
            trees
                .into_iter()
                .find_map(|t| eval(t.root.as_ref()?, ctx, depth + 1, trail))
        }
        _ if class.starts_with("ThinkNode_Conditional") => {
            let Some(holds) = condition(class, node, &ctx.pawn) else {
                ctx.unsupported.insert(class.to_owned());
                return None;
            };
            let invert = node
                .param("invert")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"));
            if holds != invert {
                eval_children(node, ctx, depth, trail)
            } else {
                None
            }
        }
        // `ThinkNode_ChancePerHour`: at most one try per 2500 ticks (the
        // last try kept per node); an MTB roll on an hour-seeded stream that
        // leaves the main stream untouched.
        "ThinkNode_ChancePerHour_Constant" => {
            let key = node.save_key;
            let now = ctx.tick as i64;
            let last = ctx.think_data.get(&key).copied().unwrap_or(-99_999);
            if now < last + 2500 {
                return None;
            }
            ctx.think_data.insert(key, now);
            let days: f32 = node
                .param("mtbDays")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(-1.0);
            let hours: f32 = node
                .param("mtbHours")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(-1.0);
            let mtb = if days > 0.0 { days * 24.0 } else { hours };
            if mtb <= 0.0 {
                return None;
            }
            let salt = crate::native_mapgen::hash_combine(key, 26_504_059);
            let offset = crate::native_mapgen::hash_combine(ctx.pawn.id_number, 169_495_093);
            let seed = crate::native_mapgen::hash_combine(
                crate::native_mapgen::hash_combine(offset, (ctx.abs_tick / 2500) as i32),
                salt,
            );
            ctx.rng.push_state_seeded(seed);
            let happens = ctx.rng.mtb_event_occurs(mtb, 2500.0, 2500.0);
            ctx.rng.pop_state();
            if happens {
                eval_children(node, ctx, depth, trail)
            } else {
                None
            }
        }
        "JobGiver_ExitMapRandom" => exit_map_random(node, ctx).map(|j| (j, None)),
        "JobGiver_WanderColony" | "JobGiver_WanderAnywhere" | "JobGiver_WanderCurrentRoom" => {
            let params = wander_params(node, ctx.default_wander);
            let p = &ctx.pawn;
            let current = ctx.next_idle_is_wait_out.unwrap_or(p.next_idle_is_wait);
            let root = if class == "JobGiver_WanderColony" {
                WanderRoot::Colony
            } else {
                WanderRoot::Own
            };
            let view = MapView {
                map: ctx.map,
                defs: ctx.defs,
                grid: ctx.grid,
                regions: ctx.regions,
            };
            let wanderer = Wanderer {
                position: p.at,
                costs: p.move_costs,
            };
            let (destinations, claimant) = (ctx.destinations, ctx.claimant);
            let (job, flag) = think_idle(
                &view,
                &wanderer,
                current,
                ctx.job_defs,
                params,
                root,
                ctx.colonists,
                &|c| destinations.can_reserve(c, claimant, false),
                ctx.rng,
            );
            ctx.next_idle_is_wait_out = Some(flag);
            job.map(|j| (j, None))
        }
        // `JobGiver_Manhunter`: attack the target in melee.
        "JobGiver_Manhunter" => ctx.pawn.manhunter_target.map(|target| {
            (
                Job {
                    def: ctx.defs.jobs.id("AttackMelee"),
                    kind: JobKind::AttackMelee { target },
                    forced: false,
                    urgency: crate::path::LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                None,
            )
        }),
        // `JobGiver_Orders`: a drafted pawn stands ready where it is.
        "JobGiver_Orders" => ctx.pawn.drafted.then_some({
            (
                Job {
                    def: ctx.job_defs.wait_combat,
                    kind: JobKind::WaitCombat,
                    forced: false,
                    urgency: crate::path::LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                None,
            )
        }),
        "JobGiver_GetRest" => {
            // A bed if one is usable, else the ground; not above
            // `maxLevelPercentage` (default 1).
            if ctx.pawn.rest_level? > rest_max_level(node) {
                return None;
            }
            let p = &ctx.pawn;
            if let Some(beds) = &ctx.beds {
                let regions = ctx.regions;
                let at = p.at;
                if let Some(bed) = crate::rest::find_bed_for(
                    ctx.defs,
                    ctx.map,
                    &|c| regions.connected(at, c),
                    ctx.reservations,
                    ctx.claimant,
                    at,
                    beds.owned,
                    beds.occupancy,
                ) {
                    let spot = ctx
                        .map
                        .structure(bed)
                        .map(|s| s.footprint.sleeping_slot(0))?;
                    return Some((
                        Job {
                            def: ctx.job_defs.lay_down,
                            kind: JobKind::LayDown {
                                spot,
                                bed: Some(bed),
                            },
                            forced: false,
                            urgency: LocomotionUrgency::Jog,
                            start_tick: 0,
                        },
                        None,
                    ));
                }
            }
            let view = MapView {
                map: ctx.map,
                defs: ctx.defs,
                grid: ctx.grid,
                regions: ctx.regions,
            };
            let (reservations, claimant) = (ctx.reservations, ctx.claimant);
            let spot = find_ground_sleep_spot(
                &view,
                p.at,
                &|c| reservations.can_reserve(claimant, Target::Cell(c), 1, 1, STACK_ALL),
                ctx.rng,
            )?;
            Some((
                Job {
                    def: ctx.job_defs.lay_down,
                    kind: JobKind::LayDown { spot, bed: None },
                    forced: false,
                    urgency: LocomotionUrgency::Jog,
                    start_tick: 0,
                },
                None,
            ))
        }
        "JobGiver_GetFood" => get_food(node, ctx).map(|j| (j, None)),
        "ThinkNode_Priority_GetJoy" => eval_children(node, ctx, depth, trail),
        // `JobGiver_GetJoyInBed` needs the pawn lying awake in bed, which
        // thinking here never sees.
        // COMPATIBILITY TODO: currently approximate — in-bed joy is not
        // offered.
        "JobGiver_GetJoyInBed" => None,
        "JobGiver_GetJoy" | "JobGiver_IdleJoy" => {
            let joy = ctx.pawn.joy.as_ref()?;
            if class == "JobGiver_IdleJoy"
                && (joy.tick < crate::recreation::GAME_START_NO_IDLE_JOY_TICKS
                    || !crate::recreation::allows_joy(joy.assignment))
            {
                return None;
            }
            let view = MapView {
                map: ctx.map,
                defs: ctx.defs,
                grid: ctx.grid,
                regions: ctx.regions,
            };
            let (reservations, claimant) = (ctx.reservations, ctx.claimant);
            let reservable =
                |c: Cell| reservations.can_reserve(claimant, Target::Cell(c), 1, 1, STACK_ALL);
            let env = crate::recreation::JoyEnv {
                view: &view,
                at: ctx.pawn.at,
                job_defs: ctx.job_defs,
                reservable: &reservable,
            };
            crate::recreation::get_joy(ctx.defs, joy, &env, ctx.rng).map(|j| (j, None))
        }
        "JobGiver_SeekSafeTemperature" => seek_safe_temperature(ctx).map(|j| (j, None)),
        "JobGiver_Work" => {
            let emergency = node
                .param("emergency")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"));
            let work = try_work(ctx, emergency)?;
            ctx.job_queue_out = work.queue;
            Some((work.job, None))
        }
        "JobGiver_IdleError" => Some((
            Job {
                def: ctx.defs.jobs.id("Wait"),
                kind: JobKind::Wait {
                    expiry_interval: IDLE_ERROR_WAIT_TICKS,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            None,
        )),
        _ => {
            ctx.unsupported.insert(class.to_owned());
            None
        }
    }
}

/// `Wait_SafeTemperature` length (ticks).
const SAFE_TEMPERATURE_WAIT_TICKS: u32 = 500;

/// `JobGiver_SeekSafeTemperature` (defaults: injury required, waiting in
/// safe air): a pawn with serious hypothermia or heatstroke waits where the
/// air is comfortable, or goes to the first region (breadth first, doors
/// passed but not stopped in) whose room is comfortable.
// COMPATIBILITY TODO: currently approximate — allowed areas are not
// modelled and a region's random cell is drawn from its cell list, not the
// game's extents-based `Region.RandomCell`.
fn seek_safe_temperature(ctx: &mut ThinkContext<'_>) -> Option<Job> {
    let t = ctx.pawn.temperature?;
    if !t.serious_injury {
        return None;
    }
    let comfy = |v: f32| v >= t.comfy.0 && v <= t.comfy.1;
    if comfy(t.ambient) {
        return Some(Job {
            def: ctx.defs.jobs.id("Wait_SafeTemperature"),
            kind: JobKind::Wait {
                expiry_interval: SAFE_TEMPERATURE_WAIT_TICKS,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        });
    }
    let regions = ctx.regions;
    let root = regions.region_at(ctx.pawn.at)?;
    let temps = ctx.room_temperatures;
    let rng = &mut *ctx.rng;
    let mut found = None;
    regions.traverse(
        root,
        |_, _| true,
        |r| {
            if regions.region(r).kind == crate::region::RegionType::Portal {
                return false;
            }
            // `TryGetAllowedCellInRegion` runs first for every region.
            let cells: Vec<Cell> = regions.cells(r).collect();
            if cells.is_empty() {
                return false;
            }
            let cell = cells[rng.range(0, cells.len() as i32) as usize];
            let temp = regions
                .room_of_region(r)
                .and_then(|room| temps.get(room).copied());
            if temp.is_some_and(comfy) {
                found = Some(cell);
                return true;
            }
            false
        },
        9999,
    );
    Some(Job {
        def: ctx.defs.jobs.id("GotoSafeTemperature"),
        kind: JobKind::Goto { target: found? },
        forced: false,
        urgency: LocomotionUrgency::Jog,
        start_tick: 0,
    })
}

/// A child's priority inside `ThinkNode_PrioritySorter`.
// COMPATIBILITY TODO: currently approximate — only JobGiver_GetRest reports a
// priority; every other child (food, joy, work, ...) is treated as 0.
fn priority(node: &ThinkNodeSpec, pawn: &PawnFacts<'_>) -> f32 {
    match node.class.as_str() {
        "JobGiver_GetRest" => {
            let max = rest_max_level(node);
            if pawn.rest_level.is_some_and(|r| r > max) {
                return 0.0;
            }
            get_rest_priority(pawn.rest_level, pawn.starving, pawn.assignment)
        }
        "JobGiver_Work" => work_priority(pawn.assignment, pawn.ever_work),
        "JobGiver_GetFood" => pawn.food.map_or(0.0, |f| {
            let (min_category, max_level) = get_food_params(node);
            get_food_priority(f.percent(), f.hunger, min_category, max_level, f.want_eat)
        }),
        "ThinkNode_Priority_GetJoy" => crate::recreation::get_joy_priority(pawn.joy.as_ref()),
        _ => 0.0,
    }
}

/// Conditions we can evaluate today; `None` = not implemented.
// COMPATIBILITY TODO: currently approximate — faction checks use "is colonist" for
// all faction-related conditions.
fn condition(class: &str, node: &ThinkNodeSpec, pawn: &PawnFacts<'_>) -> Option<bool> {
    Some(match class {
        "ThinkNode_ConditionalColonist"
        | "ThinkNode_ConditionalOfPlayerFaction"
        | "ThinkNode_ConditionalPlayerControlledColonist" => pawn.is_colonist,
        "ThinkNode_ConditionalHasFaction" => pawn.is_colonist,
        "ThinkNode_ConditionalPawnKind" => node.param("pawnKind") == Some(pawn.kind_def_name),
        "ThinkNode_ConditionalStarving" => pawn.starving,
        "ThinkNode_ConditionalAnimalWrongSeason" => pawn.wrong_season,
        "ThinkNode_ConditionalDangerousTemperature" => pawn.dangerous_temperature,
        "ThinkNode_ConditionalOutdoorTemperature" => pawn.outdoor,
        "ThinkNode_ConditionalCanReachMapEdge" => pawn.can_reach_map_edge,
        // No explicit exit deadline is ever set (`exitMapAfterTick` −99999).
        "ThinkNode_ConditionalExitTimedOut" => false,
        "ThinkNode_ConditionalMentalState" => {
            pawn.mental_state.is_some() && pawn.mental_state == node.param("state")
        }
        "ThinkNode_ConditionalMentalStateClass" => {
            pawn.mental_state_class.is_some() && pawn.mental_state_class == node.param("stateClass")
        }
        "ThinkNode_ConditionalMentalStates" => pawn.mental_state.is_some_and(|s| {
            node.params
                .child_list_texts("states")
                .iter()
                .any(|t| t == s)
        }),
        // `ThinkNode_ConditionalNeedPercentageAbove` for the joy need (a
        // pawn without the need fails it).
        "ThinkNode_ConditionalNeedPercentageAbove" if node.param("need") == Some("Joy") => {
            let threshold: f32 = node.param("threshold")?.parse().ok()?;
            pawn.joy.as_ref().is_some_and(|j| j.level > threshold)
        }
        _ => return None,
    })
}

/// `JobGiver_Work`: the first job of the pawn's normal or emergency giver
/// list (see [`crate::work::try_issue_job`]).
fn try_work(ctx: &mut ThinkContext<'_>, emergency: bool) -> Option<WorkJob> {
    let lists = ctx.work?;
    let list = if emergency {
        &lists.emergency
    } else {
        &lists.normal
    };
    let wctx = WorkContext {
        defs: ctx.defs,
        map: ctx.map,
        grid: ctx.grid,
        regions: ctx.regions,
        rng: std::cell::RefCell::new(&mut *ctx.rng),
        reservations: ctx.reservations,
        claimant: ctx.claimant,
        position: ctx.pawn.at,
        costs: ctx.pawn.move_costs,
        tick: ctx.tick,
        incapable: ctx.pawn.incapable,
    };
    let env = GiverEnv {
        defs: ctx.defs,
        map: ctx.map,
        grid: ctx.grid,
        regions: ctx.regions,
        job_defs: ctx.job_defs,
        carrying_capacity: ctx.pawn.carrying_capacity,
        pawn: ctx.claimant.pawn,
        construction: ctx.construction.as_ref(),
    };
    let providers = |g| work_giver_for(&env, g);
    try_issue_job(&wctx, list, &providers)
}

/// `FloatMenuOptionProvider_WorkGivers.GetWorkGiversOptionsFor`: every
/// work type in database order, its givers by priority; directly
/// orderable ones (drafted pawns: only those allowed while drafted) that
/// consider the target, each with its forced job there (`None`: no job).
pub(crate) fn prioritized_work(
    ctx: &mut ThinkContext<'_>,
    target: crate::work::WorkTarget,
) -> Vec<(
    rimworld_defs::DefId<rimworld_defs::WorkGiverDef>,
    Option<WorkJob>,
)> {
    let wctx = WorkContext {
        defs: ctx.defs,
        map: ctx.map,
        grid: ctx.grid,
        regions: ctx.regions,
        rng: std::cell::RefCell::new(&mut *ctx.rng),
        reservations: ctx.reservations,
        claimant: ctx.claimant,
        position: ctx.pawn.at,
        costs: ctx.pawn.move_costs,
        tick: ctx.tick,
        incapable: ctx.pawn.incapable,
    };
    let env = GiverEnv {
        defs: ctx.defs,
        map: ctx.map,
        grid: ctx.grid,
        regions: ctx.regions,
        job_defs: ctx.job_defs,
        carrying_capacity: ctx.pawn.carrying_capacity,
        pawn: ctx.claimant.pawn,
        construction: ctx.construction.as_ref(),
    };
    let defs = ctx.defs;
    let mut out = Vec::new();
    for (_, work_type) in defs.work_types.iter() {
        for &g in &work_type.givers_by_priority {
            let def = &defs.work_givers[g];
            if (ctx.pawn.drafted && !def.can_be_done_while_drafted) || !def.direct_orderable {
                continue;
            }
            let Some(giver) = work_giver_for(&env, g) else {
                continue;
            };
            if giver.should_skip(&wctx) {
                continue;
            }
            if let Some(made) = giver.job_on_target(&wctx, target) {
                out.push((
                    g,
                    made.map(|(job, queue)| WorkJob {
                        job,
                        queue,
                        giver: g,
                    }),
                ));
            }
        }
    }
    out
}

/// `WorkGiver_HunterHunt`: the simulation chose the target (closest
/// reachable marked animal with a cast position, for a pawn with a
/// hunting weapon); none means skip.
struct HunterHunt {
    job: Option<Job>,
}

impl WorkGiver for HunterHunt {
    fn should_skip(&self, _ctx: &WorkContext<'_>) -> bool {
        self.job.is_none()
    }

    fn non_scan_job(&self, _ctx: &WorkContext<'_>) -> Option<(Job, Vec<ItemId>)> {
        self.job.clone().map(|j| (j, Vec::new()))
    }
}

/// What the implemented work givers are built from.
struct GiverEnv<'a, 'c> {
    defs: &'a GameDefs,
    map: &'a Map,
    grid: &'a PathGrid,
    regions: &'a Regions,
    job_defs: &'a JobDefs,
    carrying_capacity: f32,
    pawn: crate::pawn::PawnId,
    construction: Option<&'c ConstructionEnv<'a>>,
}

/// The work givers we implement, by the game's worker class.
fn work_giver_for<'a>(
    env: &GiverEnv<'a, '_>,
    giver: rimworld_defs::DefId<rimworld_defs::WorkGiverDef>,
) -> Option<Box<dyn WorkGiver + 'a>> {
    let (defs, regions, job_defs) = (env.defs, env.regions, env.job_defs);
    let carrying_capacity = env.carrying_capacity;
    let def = &defs.work_givers[giver];
    let build = || {
        let c = env.construction?;
        Some((
            crate::construct::BuildView {
                defs: env.defs,
                map: env.map,
                grid: env.grid,
                regions: env.regions,
                pawn_cells: c.pawn_cells,
                delivery_jobs: c.delivery_jobs,
            },
            crate::construct::Builder {
                pawn: env.pawn,
                construction: c.construction_level,
                carrying_capacity,
            },
            c.enroute,
        ))
    };
    match def.giver_class.as_deref()? {
        class @ ("WorkGiver_ConstructDeliverResourcesToBlueprints"
        | "WorkGiver_ConstructDeliverResourcesToFrames") => {
            let (view, builder, enroute) = build()?;
            Some(Box::new(crate::construct::DeliverResources {
                view,
                enroute,
                builder,
                frames: class.ends_with("Frames"),
                construction_work: def.work_type.as_deref() == Some("Construction"),
                job: job_defs.haul_to_container,
                haul_job: job_defs.haul_to_cell,
                no_cost_job: job_defs.place_no_cost_frame,
            }))
        }
        "WorkGiver_GrowerHarvest" => Some(Box::new(crate::farm::GrowerHarvest {
            regions,
            job: job_defs.harvest,
        })),
        "WorkGiver_GrowerSow" => {
            let c = env.construction.as_ref()?;
            Some(Box::new(crate::farm::GrowerSow {
                regions,
                job: job_defs.sow,
                cut_job: job_defs.cut_plant,
                temperature: c.outdoor_temperature,
                room_temperatures: c.room_temperatures,
            }))
        }
        "WorkGiver_HunterHunt" => Some(Box::new(HunterHunt {
            job: env.construction.and_then(|c| c.hunt.clone()),
        })),
        "WorkGiver_FixBrokenDownBuilding" => Some(Box::new(crate::repair::FixGiver {
            regions,
            component: defs.things.id("ComponentIndustrial"),
            job: job_defs.fix_broken_down,
        })),
        "WorkGiver_DoBill" => Some(Box::new(crate::cook::DoBillGiver {
            env: env.construction?.bills?,
            fixed_givers: def
                .fixed_bill_giver_defs
                .iter()
                .filter_map(|d| defs.things.id(d))
                .collect(),
            work_type: def.work_type.clone(),
            job: job_defs.do_bill,
        })),
        "WorkGiver_Researcher" => Some(Box::new(crate::research::ResearchGiver {
            benches: env.construction?.research,
            job: job_defs.research,
        })),
        "WorkGiver_Flick" => Some(Box::new(crate::flick::FlickGiver {
            job: job_defs.flick,
        })),
        "WorkGiver_Deconstruct" => Some(Box::new(crate::deconstruct::DeconstructGiver {
            job: job_defs.deconstruct,
        })),
        "WorkGiver_Miner" => {
            let (view, _, _) = build()?;
            Some(Box::new(crate::mine::MineGiver {
                view,
                job: job_defs.mine,
                haul_job: job_defs.haul_to_cell,
            }))
        }
        "WorkGiver_Refuel" => Some(Box::new(crate::refuel::RefuelGiver {
            job: job_defs.refuel,
        })),
        class @ ("WorkGiver_ConstructRemoveFloor" | "WorkGiver_ConstructSmoothFloor") => {
            env.construction?;
            let smooth = class.ends_with("SmoothFloor");
            Some(Box::new(crate::floorwork::AffectFloorGiver {
                regions,
                smooth,
                job: if smooth {
                    job_defs.smooth_floor
                } else {
                    job_defs.remove_floor
                },
            }))
        }
        "WorkGiver_FeedPatient" => {
            let c = env.construction.as_ref()?;
            Some(Box::new(crate::rescue::FeedGiver {
                candidates: c.feed,
                job: job_defs.feed_patient,
            }))
        }
        class @ ("WorkGiver_TendOther_Humanlike" | "WorkGiver_TendOtherUrgent") => {
            let c = env.construction.as_ref()?;
            Some(Box::new(crate::tend::TendGiver {
                candidates: c.tend,
                regions,
                urgent_only: class.ends_with("Urgent"),
                job: job_defs.tend_patient,
            }))
        }
        class @ ("WorkGiver_PatientGoToBedEmergencyTreatment"
        | "WorkGiver_PatientGoToBedTreatment"
        | "WorkGiver_PatientGoToBedRecuperate") => {
            use crate::tend::PatientGiverKind as K;
            let c = env.construction.as_ref()?;
            Some(Box::new(crate::tend::PatientGiver {
                facts: c.patient,
                kind: match class {
                    "WorkGiver_PatientGoToBedEmergencyTreatment" => K::EmergencyTreatment,
                    "WorkGiver_PatientGoToBedTreatment" => K::Treatment,
                    _ => K::Recuperate,
                },
                job: job_defs.lay_down,
            }))
        }
        "WorkGiver_RescueDowned" => {
            let c = env.construction.as_ref()?;
            Some(Box::new(crate::rescue::RescueGiver {
                candidates: c.rescue,
                regions,
                job: job_defs.rescue,
            }))
        }
        "WorkGiver_ConstructSmoothWall" => {
            env.construction?;
            Some(Box::new(crate::floorwork::SmoothWallGiver {
                regions,
                job: job_defs.smooth_wall,
            }))
        }
        "WorkGiver_BuildRoof" => {
            env.construction?;
            Some(Box::new(crate::roof::BuildRoofGiver {
                regions,
                job: job_defs.build_roof,
                cut_job: job_defs.cut_plant,
            }))
        }
        "WorkGiver_PlantsCut" => {
            env.construction?;
            Some(Box::new(crate::farm::PlantsCut {
                job: job_defs.cut_plant_designated,
                harvest_job: job_defs.harvest_designated,
            }))
        }
        "WorkGiver_ConstructFinishFrames" => {
            let (view, builder, _) = build()?;
            Some(Box::new(crate::construct::FinishFrames {
                view,
                builder,
                job: job_defs.finish_frame,
                haul_job: job_defs.haul_to_cell,
            }))
        }
        "WorkGiver_HaulGeneral" => Some(Box::new(HaulGeneral {
            regions,
            haul_job: job_defs.haul_to_cell,
            carrying_capacity,
        })),
        "WorkGiver_CleanFilth" => Some(Box::new(CleanFilth {
            regions,
            clean_job: job_defs.clean,
        })),
        _ => None,
    }
}

/// `JobGiver_GetFood` parameters: `minCategory` (default Fed) and
/// `maxLevelPercentage` (default 1).
fn get_food_params(node: &ThinkNodeSpec) -> (HungerCategory, f32) {
    let min_category = match node.param("minCategory") {
        Some("Hungry") => HungerCategory::Hungry,
        Some("UrgentlyHungry") => HungerCategory::UrgentlyHungry,
        Some("Starving") => HungerCategory::Starving,
        _ => HungerCategory::Fed,
    };
    let max_level = node
        .param("maxLevelPercentage")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    (min_category, max_level)
}

/// `JobGiver_GetFood.TryGiveJob` for food lying on the map.
// COMPATIBILITY TODO: currently approximate — malnourished/animal corpse
// eating, hunting, plants, dispensers and inventory food are not modelled.
fn get_food(node: &ThinkNodeSpec, ctx: &mut ThinkContext<'_>) -> Option<Job> {
    let food = ctx.pawn.food?;
    let (min_category, max_level) = get_food_params(node);
    if food.hunger < min_category || food.percent() > max_level {
        return None;
    }
    // Animals eat where the food lies (the simulation searched for it).
    if !ctx.pawn.humanlike {
        let whole_map = node
            .param("forceScanWholeMap")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        return ctx.construction.as_ref().and_then(|c| {
            if whole_map {
                c.animal_food_whole_map.clone()
            } else {
                c.animal_food.clone()
            }
        });
    }
    let p = &ctx.pawn;
    let eater = Eater {
        race: p.race?,
        humanlike: p.humanlike,
        at: p.at,
        move_costs: p.move_costs,
        hunger: food.hunger,
        temperature: ctx.construction.as_ref().map(|c| c.outdoor_temperature),
    };
    let (reservations, destinations, claimant) = (ctx.reservations, ctx.destinations, ctx.claimant);
    let can_reserve = |item: &Item| {
        reservations.can_reserve(
            claimant,
            Target::Item(item.id),
            item.stack_count as i32,
            10,
            1,
        )
    };
    let item_id = best_food_source(ctx.defs, ctx.map, ctx.grid, &eater, can_reserve)?;
    let item = ctx.map.item(item_id)?;
    // The game only gives the job if a place to eat exists (the spot is
    // searched again when the food has been picked up).
    let view = MapView {
        map: ctx.map,
        defs: ctx.defs,
        grid: ctx.grid,
        regions: ctx.regions,
    };
    spot_to_chew_standing_near(
        &view,
        ctx.ingest_order,
        p.at,
        item.def,
        &|c| destinations.can_reserve(c, claimant, false),
        &|c| reservations.can_reserve(claimant, Target::Cell(c), 1, 1, STACK_ALL),
        ctx.rng,
    )?;
    let def = &ctx.defs.things[item.def];
    let max_at_once = def.ingestible.as_ref()?.max_num_to_ingest_at_once;
    let count = will_ingest_stack_count(
        food.max - food.level,
        unit_nutrition(ctx.defs, def),
        max_at_once,
    );
    let dest = touch_destination(ctx.grid, item.position, p.at, p.move_costs)?;
    Some(Job {
        def: ctx.job_defs.ingest,
        kind: JobKind::Ingest {
            food: item_id,
            count,
            reserved: 0, // set when the job starts
            stage: IngestStage::GotoFood { dest },
        },
        forced: false,
        urgency: LocomotionUrgency::Jog,
        start_tick: 0,
    })
}

/// `JobGiver_ExitMapRandom`: not while downed; up to 40 tries of a random
/// cell (x then z) moved to a random side (0..3: x=0, x=max, z=0, z=max)
/// that is standable and reachable on its cell; a `Goto` there that exits
/// the map on arrival, at the node's `defaultLocomotion`.
// COMPATIBILITY TODO: currently approximate — danger relaxation after 15
// tries, known dangers, forbidden cells, dangerous terrain, doors that
// animals can't open and flying exits (`canLeaveMapFlying`) are not modelled;
// reachability is region connectivity.
fn exit_map_random(node: &ThinkNodeSpec, ctx: &mut ThinkContext<'_>) -> Option<Job> {
    if ctx.pawn.downed {
        return None;
    }
    let view = MapView {
        map: ctx.map,
        defs: ctx.defs,
        grid: ctx.grid,
        regions: ctx.regions,
    };
    let c = random_exit_spot(&view, ctx.pawn.at, ctx.rng)?;
    let urgency = match node.param("defaultLocomotion").map(str::trim) {
        Some("Walk") => LocomotionUrgency::Walk,
        Some("Amble") => LocomotionUrgency::Amble,
        Some("Sprint") => LocomotionUrgency::Sprint,
        _ => LocomotionUrgency::Jog,
    };
    Some(Job {
        def: ctx.job_defs.goto,
        kind: JobKind::ExitMap { dest: c },
        forced: false,
        urgency,
        start_tick: 0,
    })
}

/// `RCellFinder.TryFindRandomExitSpot` (ByPawn): up to 40 tries of a
/// random cell (x then z) pushed to a random side (0..3: x=0, x=max, z=0,
/// z=max), standable and reachable on its cell.
pub fn random_exit_spot(view: &MapView<'_>, at: Cell, rng: &mut Rand) -> Option<Cell> {
    let size = view.map.size();
    for _ in 0..40 {
        let mut c = Cell::new(rng.range(0, size.width), rng.range(0, size.height));
        match rng.range_inclusive(0, 3) {
            0 => c.x = 0,
            1 => c.x = size.width - 1,
            2 => c.z = 0,
            _ => c.z = size.height - 1,
        }
        if view.standable(c) && (c == at || view.regions.connected(at, c)) {
            return Some(c);
        }
    }
    None
}

/// `JobGiver_GetRest.maxLevelPercentage` (default 1).
fn rest_max_level(node: &ThinkNodeSpec) -> f32 {
    node.param("maxLevelPercentage")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1.0)
}

fn wander_params(node: &ThinkNodeSpec, default: WanderParams) -> WanderParams {
    let mut p = default;
    if let Some((lo, hi)) = node
        .param("ticksBetweenWandersRange")
        .and_then(rimworld_defs::values::parse_float_range)
    {
        p.wait_ticks = (lo.max(0.0) as u32, hi.max(lo).max(0.0) as u32);
    }
    if let Some(r) = node
        .param("wanderRadius")
        .and_then(|r| r.parse::<f32>().ok())
    {
        p.radius = r.round().max(1.0) as i32;
    }
    p
}

/// Whether the interpreter implements a node class.
pub fn is_supported(class: &str) -> bool {
    matches!(
        class,
        "ThinkNode_Priority"
            | "ThinkNode_PrioritySorter"
            | "ThinkNode_Tagger"
            | "ThinkNode_Subtree"
            | "ThinkNode_SubtreesByTag"
            | "ThinkNode_ConditionalColonist"
            | "ThinkNode_ConditionalOfPlayerFaction"
            | "ThinkNode_ConditionalPlayerControlledColonist"
            | "ThinkNode_ConditionalHasFaction"
            | "ThinkNode_ConditionalPawnKind"
            | "JobGiver_WanderColony"
            | "JobGiver_WanderAnywhere"
            | "JobGiver_WanderCurrentRoom"
            | "JobGiver_IdleError"
            | "JobGiver_GetRest"
            | "JobGiver_GetFood"
            | "JobGiver_Work"
            | "ThinkNode_ConditionalStarving"
            | "ThinkNode_ConditionalAnimalWrongSeason"
            | "ThinkNode_ConditionalDangerousTemperature"
            | "ThinkNode_ConditionalOutdoorTemperature"
            | "ThinkNode_ConditionalCanReachMapEdge"
            | "ThinkNode_ConditionalExitTimedOut"
            | "ThinkNode_ChancePerHour_Constant"
            | "JobGiver_ExitMapRandom"
            | "ThinkNode_ConditionalMentalState"
            | "ThinkNode_ConditionalMentalStates"
            | "ThinkNode_ConditionalMentalStateClass"
            | "ThinkNode_Priority_GetJoy"
            | "JobGiver_GetJoy"
            | "JobGiver_IdleJoy"
    )
}

/// `(total nodes, supported nodes)` in a tree, not following subtrees.
pub fn support_stats(tree: &ThinkTreeDef) -> (usize, usize) {
    let (mut total, mut supported) = (0, 0);
    if let Some(root) = &tree.root {
        root.walk(&mut |n| {
            total += 1;
            if is_supported(&n.class) {
                supported += 1;
            }
        });
    }
    (total, supported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Grid, GridSize};
    use crate::pawn::PawnId;
    use rimworld_defs::load_documents;
    use rimworld_defs::xml::ActivePackages;

    fn test_map(defs: &GameDefs, size: GridSize) -> Map {
        Map::new(size, defs.terrain.id("Soil").expect("Soil in test defs"))
    }

    const TREES: &str = r#"<Defs>
      <TerrainDef><defName>Soil</defName></TerrainDef>
      <JobDef><defName>GotoWander</defName><reportString>wandering.</reportString></JobDef>
      <JobDef><defName>Wait_Wander</defName><reportString>wandering.</reportString></JobDef>
      <JobDef><defName>Wait</defName><reportString>standing.</reportString></JobDef>
      <ThinkTreeDef>
        <defName>Main</defName>
        <thinkRoot Class="ThinkNode_Priority">
          <subNodes>
            <li Class="JobGiver_GetJoy" />
            <li Class="ThinkNode_ConditionalPrisoner">
              <subNodes><li Class="JobGiver_WanderAnywhere" /></subNodes>
            </li>
            <li Class="ThinkNode_SubtreesByTag"><insertTag>Hook</insertTag></li>
            <li Class="ThinkNode_ConditionalPawnKind">
              <pawnKind>WildMan</pawnKind>
              <subNodes><li Class="JobGiver_IdleError" /></subNodes>
            </li>
            <li Class="ThinkNode_ConditionalColonist">
              <subNodes>
                <li Class="ThinkNode_Subtree"><treeDef>Core</treeDef></li>
                <li Class="ThinkNode_Tagger">
                  <tagToGive>Idle</tagToGive>
                  <subNodes>
                    <li Class="JobGiver_WanderColony">
                      <ticksBetweenWandersRange>10~20</ticksBetweenWandersRange>
                    </li>
                  </subNodes>
                </li>
              </subNodes>
            </li>
            <li Class="JobGiver_IdleError" />
          </subNodes>
        </thinkRoot>
      </ThinkTreeDef>
      <ThinkTreeDef>
        <defName>Core</defName>
        <thinkRoot Class="ThinkNode_PrioritySorter">
          <subNodes><li Class="JobGiver_Work" /></subNodes>
        </thinkRoot>
      </ThinkTreeDef>
    </Defs>"#;

    fn defs(extra: &str) -> GameDefs {
        let (db, r) = load_documents(
            "core",
            &[("t.xml", TREES), ("e.xml", extra)],
            &ActivePackages::default(),
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let (d, w) = GameDefs::from_database(db);
        assert!(w.is_empty(), "{w:?}");
        d
    }

    fn run(
        defs: &GameDefs,
        kind: &str,
        colonist: bool,
        wait: bool,
    ) -> (Option<ThinkResult>, BTreeSet<String>) {
        let grid = PathGrid::new(Grid::new(GridSize::new(20, 20), Some(0)));
        let map = test_map(defs, GridSize::new(20, 20));
        let regions = Regions::build(&map, defs, &grid);
        let mut order = IngestionSpotOrder::default();
        let job_defs = JobDefs::resolve(defs);
        let mut rng = Rand::new(1);
        let mut ctx = ThinkContext {
            defs,
            pawn: PawnFacts {
                kind_def_name: kind,
                is_colonist: colonist,
                at: Cell::new(10, 10),
                next_idle_is_wait: wait,
                move_costs: MoveCosts::from_move_speed(4.6),
                rest_level: None,
                starving: false,
                humanlike: true,
                hour: 12,
                assignment: crate::rest::time_assignment(12, true),
                food: None,
                race: None,
                ever_work: false,
                carrying_capacity: 75.0,
                incapable: &[],
                temperature: None,
                joy: None,
                mental_state: None,
                drafted: false,
                mental_state_class: None,
                manhunter_target: None,
                wrong_season: false,
                dangerous_temperature: false,
                outdoor: true,
                can_reach_map_edge: true,
                id_number: 0,
                downed: false,
            },
            map: &map,
            grid: &grid,
            job_defs: &job_defs,
            default_wander: WanderParams::default(),
            claimant: Claimant {
                pawn: PawnId(0),
                has_faction: true,
            },
            reservations: &ReservationManager::default(),
            destinations: &DestinationManager::default(),
            regions: &regions,
            colonists: &[],
            ingest_order: &mut order,
            work: None,
            tick: 0,
            job_queue_out: Vec::new(),
            rng: &mut rng,
            unsupported: BTreeSet::new(),
            next_idle_is_wait_out: None,
            construction: None,
            beds: None,
            room_temperatures: &[],
            abs_tick: 0,
            think_data: Default::default(),
        };
        let r = think(defs.think_trees.get("Main").unwrap(), &mut ctx);
        (r, ctx.unsupported)
    }

    #[test]
    fn colonist_falls_through_to_tagged_wander() {
        let d = defs("<Defs/>");
        let (r, unsupported) = run(&d, "Colonist", true, true);
        let r = r.unwrap();
        assert_eq!(r.tag.as_deref(), Some("Idle"));
        match r.job.kind {
            JobKind::Wait {
                expiry_interval: ticks_left,
            } => {
                assert!((10..=20).contains(&ticks_left), "XML range used")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(d.jobs[r.job.def.unwrap()].def_name, "Wait_Wander");
        assert_eq!(
            r.trail,
            vec![
                "Main",
                "Priority",
                "ConditionalColonist",
                "Tagger(Idle)",
                "JobGiver_WanderColony"
            ]
        );
        // Unimplemented nodes were skipped, not fatal.
        assert!(unsupported.contains("ThinkNode_ConditionalPrisoner"));
        // The PrioritySorter in the Core subtree skips zero-priority children.
        assert!(!unsupported.contains("JobGiver_Work"));
    }

    #[test]
    fn pawn_kind_condition_and_fallback() {
        let d = defs("<Defs/>");
        let (r, _) = run(&d, "WildMan", false, true);
        let r = r.unwrap();
        assert_eq!(r.trail.last().unwrap(), "JobGiver_IdleError");
        assert!(r.trail.contains(&"ConditionalPawnKind".to_owned()));
        // Non-colonist, other kind: reaches the final IdleError directly.
        let (r, _) = run(&d, "Visitor", false, true);
        assert_eq!(
            r.unwrap().trail,
            vec!["Main", "Priority", "JobGiver_IdleError"]
        );
    }

    #[test]
    fn inserted_trees_run_by_priority() {
        let d = defs(
            r#"<Defs>
              <ThinkTreeDef><defName>Low</defName><insertTag>Hook</insertTag><insertPriority>1</insertPriority>
                <thinkRoot Class="JobGiver_WanderAnywhere" /></ThinkTreeDef>
              <ThinkTreeDef><defName>High</defName><insertTag>Hook</insertTag><insertPriority>9</insertPriority>
                <thinkRoot Class="JobGiver_IdleError" /></ThinkTreeDef>
            </Defs>"#,
        );
        let (r, _) = run(&d, "Colonist", true, true);
        assert_eq!(r.unwrap().trail.last().unwrap(), "JobGiver_IdleError");
    }

    #[test]
    fn invert_negates_conditions() {
        let d = defs(
            r#"<Defs><ThinkTreeDef><defName>Inv</defName>
              <thinkRoot Class="ThinkNode_ConditionalColonist"><invert>true</invert>
                <subNodes><li Class="JobGiver_IdleError" /></subNodes></thinkRoot>
            </ThinkTreeDef></Defs>"#,
        );
        let tree = d.think_trees.get("Inv").unwrap();
        let grid = PathGrid::new(Grid::new(GridSize::new(5, 5), Some(0)));
        let map = test_map(&d, GridSize::new(5, 5));
        let regions = Regions::build(&map, &d, &grid);
        let jd = JobDefs::resolve(&d);
        for (colonist, expect_job) in [(true, false), (false, true)] {
            let mut rng = Rand::new(1);
            let mut order = IngestionSpotOrder::default();
            let mut ctx = ThinkContext {
                defs: &d,
                pawn: PawnFacts {
                    kind_def_name: "X",
                    is_colonist: colonist,
                    at: Cell::new(2, 2),
                    next_idle_is_wait: true,
                    move_costs: MoveCosts::from_move_speed(4.6),
                    rest_level: None,
                    starving: false,
                    humanlike: true,
                    hour: 12,
                    assignment: crate::rest::time_assignment(12, true),
                    food: None,
                    race: None,
                    ever_work: false,
                    carrying_capacity: 75.0,
                    incapable: &[],
                    temperature: None,
                    joy: None,
                    mental_state: None,
                    drafted: false,
                    mental_state_class: None,
                    manhunter_target: None,
                    wrong_season: false,
                    dangerous_temperature: false,
                    outdoor: true,
                    can_reach_map_edge: true,
                    id_number: 0,
                    downed: false,
                },
                map: &map,
                grid: &grid,
                job_defs: &jd,
                default_wander: WanderParams::default(),
                claimant: Claimant {
                    pawn: PawnId(0),
                    has_faction: true,
                },
                reservations: &ReservationManager::default(),
                destinations: &DestinationManager::default(),
                regions: &regions,
                colonists: &[],
                ingest_order: &mut order,
                work: None,
                tick: 0,
                job_queue_out: Vec::new(),
                rng: &mut rng,
                unsupported: BTreeSet::new(),
                next_idle_is_wait_out: None,
                construction: None,
                beds: None,
                room_temperatures: &[],
                abs_tick: 0,
                think_data: Default::default(),
            };
            assert_eq!(think(tree, &mut ctx).is_some(), expect_job);
        }
    }

    #[test]
    fn support_stats_counts() {
        let d = defs("<Defs/>");
        let (total, supported) = support_stats(d.think_trees.get("Main").unwrap());
        assert_eq!(total, 12);
        assert_eq!(supported, 11);
    }
}
