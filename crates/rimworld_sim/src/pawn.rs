//! Pawns as simulation entities. Rendering reads from these; it never owns
//! the authoritative position.

use std::collections::VecDeque;

use rimworld_defs::{DefId, PawnKindDef, ThingDef, ThinkTreeDef};

use crate::grid::Cell;
use crate::job::{Job, JobKind, Rot4};
use crate::map::ItemId;
use crate::needs::Needs;
use crate::path::MoveCosts;
use crate::work::WorkSettings;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct PawnId(pub u32);

/// Items a pawn is carrying (picked up from a stack).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Carried {
    /// Identity of the carried thing (kept on a whole pickup, new on a split).
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub count: u32,
    /// Rot progress (rottable things), kept while carried.
    #[serde(default)]
    pub rot: f32,
    /// Hit points when damaged (`None`: full), kept while carried.
    #[serde(default)]
    pub hit_points: Option<i32>,
}

/// Progress of the step currently being walked: the pawn pays ticks of
/// cost until `cost_left` reaches zero, then enters `to`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub to: Cell,
    pub cost_left: f32,
    pub cost_total: f32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pawn {
    pub id: PawnId,
    pub name: String,
    pub kind: DefId<PawnKindDef>,
    pub race: DefId<ThingDef>,
    /// Authoritative grid position (the cell the pawn is standing in, or
    /// leaving if mid-step).
    pub position: Cell,
    pub move_costs: MoveCosts,
    /// MoveSpeed before the light factor, when light affects it
    /// (`StatPart_Glow`; 0 when it doesn't).
    #[serde(default)]
    pub base_move_speed: f32,
    /// The light factor curve (glow → MoveSpeed factor); empty when light
    /// doesn't matter.
    #[serde(default)]
    pub move_glow_curve: Vec<(f32, f32)>,
    /// `MindState.nextMoveOrderIsCrawlBreak`: the next crawl order is a
    /// break (set on going down).
    #[serde(default)]
    pub crawl_break_next: bool,
    /// MoveSpeed's health factor (`capacityFactors`: Moving), refreshed
    /// whenever the pawn's health changes.
    #[serde(default = "full_factor")]
    pub move_capacity_factor: f32,
    pub step: Option<Step>,
    /// Remaining cells after the current step.
    pub path: VecDeque<Cell>,
    /// Final destination of the current move order, if any.
    pub destination: Option<Cell>,
    /// Facing (owned by the simulation, read by rendering).
    pub rotation: Rot4,
    /// Current job; `None` means the pawn will think on its next tick.
    pub job: Option<Job>,
    /// Idle behaviour alternates between waiting and wandering.
    pub next_idle_is_wait: bool,
    /// The race's main think tree (`race/thinkTreeMain`), if it exists.
    pub think_tree: Option<DefId<ThinkTreeDef>>,
    /// Member of the player's colony (PawnKindDef `defaultFactionDef`).
    pub is_colonist: bool,
    /// `Pawn_TimetableTracker.times`: the 24 hours' assignments (`None`:
    /// the default, sleep from 22 to 05).
    #[serde(default)]
    pub timetable: Option<Vec<crate::rest::TimeAssignment>>,
    /// Think-tree path that produced the current job (debug display).
    pub think_trail: Vec<String>,
    pub needs: Needs,
    /// Asleep (only while lying down).
    pub asleep: bool,
    /// Number used for per-pawn tick staggering (the game's thingIDNumber).
    pub id_number: i32,
    /// Interval-logic update rate in ticks (1-15). In the game it depends on
    /// the camera: 15 off-screen, zoom level + 1 when in view.
    pub update_rate: u32,
    /// Ticks accumulated since the last interval update.
    pub tick_delta: u32,
    /// First tick on which movement may progress (path start latency).
    pub move_ready_tick: u64,
    pub carried: Option<Carried>,
    /// `EatingSpeed` stat (chewing takes `baseIngestTicks / EatingSpeed`).
    pub eating_speed: f32,
    /// Identity of the current job (reservations are held per job).
    pub job_id: u64,
    /// Targets queued for the current job (`job.targetQueueA`).
    pub target_queue: Vec<ItemId>,
    /// Further destinations of the current job (`job.targetQueueB`).
    pub target_queue_b: Vec<ItemId>,
    /// Counts for `target_queue_b` (`job.countQueue`).
    #[serde(default)]
    pub count_queue: Vec<u32>,
    /// Things the job put down and how many of each it owns
    /// (`job.placedThings`).
    #[serde(default)]
    pub placed_things: Vec<(ItemId, u32)>,
    pub skills: crate::stats::Skills,
    /// Set when the pawn's path became blocked and no new one exists
    /// (`PatherFailed`); the job driver fails the job.
    pub path_failed: bool,
    /// Set by the path follower when the pawn steps into a new cell
    /// (`Notify_EnteredNewCell`).
    #[serde(default)]
    pub entered_cell: bool,
    /// Filth on the pawn's feet.
    #[serde(default)]
    pub filth: crate::sim::filth::PawnFilth,
    /// Worn apparel (`Pawn_ApparelTracker.WornApparel`), in wearing order.
    #[serde(default)]
    pub apparel: Vec<WornApparel>,
    /// A downed pawn this pawn carries (rescue).
    #[serde(default)]
    pub carried_pawn: Option<PawnId>,
    /// The pawn carrying this one, if any.
    #[serde(default)]
    pub carried_by: Option<PawnId>,
    /// Busy until this tick (a `Stance_Cooldown`, e.g. waiting for a door
    /// to open); the path follower does nothing meanwhile.
    pub stance_until: u64,
    /// Set by the path follower: the pawn stands before this closed door
    /// and opens it (handled by the simulation).
    pub door_request: Option<Cell>,
    /// Set by the path follower: the pawn just left this door cell.
    pub left_door: Option<Cell>,
    /// The bed this pawn owns (`ownership.OwnedBed`).
    pub owned_bed: Option<ItemId>,
    /// Hediffs and the downed/dead state.
    pub health: crate::health::Health,
    /// `Need_Mood` and its thoughts (humanlikes).
    #[serde(default)]
    pub mood: Option<crate::mood::MoodState>,
    /// Mental breaks and states.
    #[serde(default)]
    pub mind: crate::mood::MindState,
    /// `Pawn_DraftController.Drafted`.
    #[serde(default)]
    pub drafted: bool,
    /// The primary weapon and its verb.
    #[serde(default)]
    pub equipment: Option<crate::ranged::Equipped>,
    /// The busy stance (none: Mobile).
    #[serde(default)]
    pub stance: Option<crate::ranged::Stance>,
    /// Stat values fixed from outside (replays of recorded pawns whose
    /// traits, genes or health we do not model).
    #[serde(default)]
    pub stat_overrides: std::collections::BTreeMap<String, f32>,
    /// Work priorities; `None` for pawns that never work.
    pub work: Option<WorkSettings>,
    /// `CarryingCapacity` stat.
    // COMPATIBILITY TODO: currently approximate — the base stat; body size
    // and Manipulation factors are not applied.
    pub carrying_capacity: f32,
}

impl Pawn {
    pub fn is_moving(&self) -> bool {
        self.step.is_some() || !self.path.is_empty()
    }

    /// Continuous position for rendering: interpolated between `position` and
    /// the step target. Pure function of simulation state.
    pub fn visual_position(&self) -> (f32, f32) {
        let (x, z) = (self.position.x as f32, self.position.z as f32);
        match self.step {
            Some(step) if step.cost_total > 0.0 => {
                let t = (1.0 - step.cost_left / step.cost_total).clamp(0.0, 1.0);
                (
                    x + (step.to.x as f32 - x) * t,
                    z + (step.to.z as f32 - z) * t,
                )
            }
            _ => (x, z),
        }
    }

    pub fn is_lying_down(&self) -> bool {
        matches!(
            self.job.as_ref().map(|j| j.kind),
            Some(JobKind::LayDown { .. })
        )
    }

    pub fn is_sleeping(&self) -> bool {
        self.asleep
    }

    /// The cell this pawn lays claim to: where it is going, or where it
    /// stands. Other pawns will not choose it as a destination.
    pub fn claimed_cell(&self) -> Cell {
        self.destination.unwrap_or_else(|| self.next_stop())
    }

    /// The cell the pawn will occupy once the current step finishes.
    pub fn next_stop(&self) -> Cell {
        self.step.map_or(self.position, |s| s.to)
    }
}

/// A piece of worn apparel: its ThingDef and material.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WornApparel {
    pub def: DefId<ThingDef>,
    pub stuff: Option<DefId<ThingDef>>,
}

fn full_factor() -> f32 {
    1.0
}
