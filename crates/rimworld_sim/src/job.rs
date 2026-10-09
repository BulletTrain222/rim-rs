//! Jobs: what a pawn is currently doing.
//!
//! A job pairs a `JobDef` (identity + UI text from the game data) with a
//! driver implemented here. Today's drivers: `Goto` (walk a path) and `Wait`
//! (stand for N ticks). When a pawn has no job, [`think_idle`] picks one — a
//! tiny stand-in for the end of RimWorld's Humanlike think tree, where idle
//! colonists alternate between wandering to a nearby cell (`GotoWander`) and
//! waiting (`Wait_Wander`).

use rimworld_defs::{DefId, GameDefs, JobDef, ThingDef};

use crate::cell_finder::{MapView, Wanderer, colony_wander_root, random_wander_dest_for};
use crate::grid::Cell;
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::rand::Rand;

/// Facing, like RimWorld's `Rot4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Rot4 {
    North,
    East,
    #[default]
    South,
    West,
}

impl Rot4 {
    /// Facing for a step direction; diagonal steps face east/west.
    pub fn from_step(dir: Cell) -> Self {
        if dir.x > 0 {
            Rot4::East
        } else if dir.x < 0 {
            Rot4::West
        } else if dir.z > 0 {
            Rot4::North
        } else {
            Rot4::South
        }
    }

    /// `Rot4.FacingCell`: the offset of the cell in front.
    pub fn facing_offset(self) -> Cell {
        match self {
            Rot4::North => Cell::new(0, 1),
            Rot4::East => Cell::new(1, 0),
            Rot4::South => Cell::new(0, -1),
            Rot4::West => Cell::new(-1, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum JobKind {
    Goto {
        target: Cell,
    },
    /// Stand still until the job expires (hash-aligned, see `Sim`).
    Wait {
        expiry_interval: u32,
    },
    /// Walk to `spot` and lie down there (no beds yet). The job does not end
    /// by itself; an awake pawn re-thinks periodically and may switch.
    LayDown {
        spot: Cell,
        /// The bed lain in (`None`: on the ground).
        bed: Option<ItemId>,
    },
    /// Eat `count` units of the food stack `food` (`JobDriver_Ingest`).
    Ingest {
        food: ItemId,
        count: u32,
        /// Units of the stack reserved for this job (may be 0 when other
        /// pawns hold the rest, as in the game).
        reserved: u32,
        stage: IngestStage,
    },
    /// Clean the filth queued for the job (`JobDriver_CleanFilth`); `target`
    /// is the one being cleaned.
    Clean {
        target: Option<ItemId>,
        stage: CleanStage,
    },
    /// Haul `source` (targetA; the carried thing after pickup) to the
    /// storage cell `dest` (targetB) (`JobDriver_HaulToCell`).
    Haul {
        source: ItemId,
        dest: Cell,
        /// Remaining collection allowance (`job.count`).
        count: i32,
        stage: HaulStage,
        /// Tick the driver started (placement waits 30 ticks from it).
        start_tick: u64,
        /// Hauling aside to a cell that is not storage
        /// (`HaulMode.ToCellNonStorage`).
        aside: bool,
    },
    /// Carry `source` (targetA) into the blueprint or frame `container`
    /// (targetB) for `primary` (targetC) (`JobDriver_HaulToContainer`).
    /// Further resources and destinations are in the pawn's target queues.
    HaulToContainer {
        source: ItemId,
        container: ItemId,
        primary: Option<ItemId>,
        /// Remaining collection allowance (`job.count`).
        count: i32,
        stage: ContainerStage,
    },
    /// Build the frame (`JobDriver_ConstructFinishFrame`).
    FinishFrame {
        frame: ItemId,
        stage: BuildStage,
    },
    /// Sow `plant` on `cell` (`JobDriver_PlantSow`).
    Sow {
        cell: Cell,
        plant: DefId<ThingDef>,
        stage: SowStage,
    },
    /// Put a roof over `cell` and the build-roof cells around it
    /// (`JobDriver_BuildRoof`).
    BuildRoof {
        cell: Cell,
        stage: RoofStage,
    },
    /// Take down `building` (`JobDriver_Deconstruct`).
    Deconstruct {
        building: ItemId,
        stage: DeconstructStage,
    },
    /// Mine the rock on `cell` (`JobDriver_Mine`).
    Mine {
        cell: Cell,
        stage: MineStage,
    },
    /// Remove the floor on `cell` or smooth its stone
    /// (`JobDriver_RemoveFloor` / `JobDriver_SmoothFloor`).
    AffectFloor {
        cell: Cell,
        smooth: bool,
        stage: RoofStage,
    },
    /// Smooth the rock wall on `cell` (`JobDriver_SmoothWall`).
    SmoothWall {
        cell: Cell,
        stage: RoofStage,
    },
    /// Carry the downed `patient` to `bed` (`JobDriver_TakeToBed`).
    Rescue {
        patient: crate::pawn::PawnId,
        bed: ItemId,
        carrying: bool,
    },
    /// Bring `count` of `food` to the bedridden `patient` and feed them
    /// (`JobDriver_FoodFeedPatient`).
    FeedPatient {
        food: ItemId,
        patient: crate::pawn::PawnId,
        count: u32,
        stage: FeedStage,
    },
    /// Tend the bedridden `patient` (`JobDriver_TendPatient`, no medicine).
    TendPatient {
        patient: crate::pawn::PawnId,
        stage: TendStage,
    },
    /// Bring `component` to the broken-down `building` and fix it
    /// (`JobDriver_FixBrokenDownBuilding`).
    FixBrokenDown {
        building: ItemId,
        component: ItemId,
        stage: FixStage,
    },
    /// Research at `bench`, standing at its interaction `cell`
    /// (`JobDriver_Research`).
    Research {
        bench: ItemId,
        cell: crate::grid::Cell,
        stage: ResearchStage,
    },
    /// Do a bill at the work table `giver` (`JobDriver_DoBill`): gather
    /// the queued ingredients (`targetQueueB`/`countQueue` on the pawn),
    /// then work. `ingredient` and `count` are targetB and `job.count`.
    DoBill {
        giver: ItemId,
        bill: u32,
        ingredient: Option<ItemId>,
        count: i32,
        stage: DoBillStage,
    },
    /// Recreation (`JobDriver_GoForWalk`, `JobDriver_Skygaze`,
    /// `JobDriver_RelaxAlone`).
    Joy {
        activity: JoyActivity,
        stage: JoyStage,
    },
    /// Flick the switch of `target` (`JobDriver_Flick`).
    Flick {
        target: ItemId,
        stage: FlickStage,
    },
    /// Bring `fuel` to the refuelable `building` (`JobDriver_Refuel`);
    /// `count` is the fuel still wanted (`job.count`).
    Refuel {
        building: ItemId,
        fuel: ItemId,
        count: i32,
        stage: RefuelStage,
    },
    /// A drafted pawn standing ready (`Wait_Combat`, no expiry).
    WaitCombat,
    /// Hunt a marked animal (`JobDriver_Hunt`): position, shoot, execute
    /// it when downed.
    Hunt {
        victim: crate::pawn::PawnId,
        stage: HuntStage,
        /// `jobStartTick`.
        start_tick: u64,
    },
    /// Run to a cell away from a threat (`JobDriver_Flee`).
    Flee {
        dest: Cell,
        threat: Option<crate::pawn::PawnId>,
    },
    /// Walk to a weapon and take it as primary equipment
    /// (`JobDriver_Equip`).
    Equip {
        item: ItemId,
    },
    /// A predator's hunt (`JobDriver_PredatorHunt`): follow and attack the
    /// prey (downed or not) until it dies, then eat its corpse.
    PredatorHunt {
        prey: crate::pawn::PawnId,
        corpse: Option<ItemId>,
        stage: PredatorStage,
        /// The next attack is a surprise attack (`firstHit`).
        first_hit: bool,
        start_tick: u64,
    },
    /// Leave the map: walk to an edge cell and exit there (`Goto` with
    /// `exitMapOnArrival`).
    ExitMap {
        dest: Cell,
    },
    /// A non-tool-user eats an item or plant where it lies
    /// (`JobDriver_Ingest` without picking it up); `count` is the units
    /// wanted of an item.
    IngestInPlace {
        food: ItemId,
        count: u32,
        stage: IngestStage,
    },
    /// Turn a blueprint that costs nothing into its frame
    /// (`JobDriver_PlaceNoCostFrame`); `moving_off`: stepping off it first.
    PlaceNoCostFrame {
        blueprint: ItemId,
        moving_off: bool,
    },
    /// Shoot at `target` from where the pawn stands
    /// (`JobDriver_AttackStatic`).
    AttackStatic {
        target: crate::pawn::PawnId,
        /// `startedIncapacitated`.
        started_downed: bool,
        /// `numAttacksMade`.
        attacks: u32,
    },
    /// Fight `target` in melee until it is downed
    /// (`JobDriver_AttackMelee`).
    AttackMelee {
        target: crate::pawn::PawnId,
    },
    /// Harvest the queued plants (`JobDriver_PlantHarvest`); `target` is
    /// the one being worked on.
    Harvest {
        target: Option<ItemId>,
        stage: PlantWorkStage,
        /// Cutting (`JobDriver_PlantCut`: the plant is destroyed even if
        /// harvesting would leave it standing).
        cut: bool,
    },
}

/// Progress through the sow job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SowStage {
    /// Walking to touch the cell.
    Goto,
    /// Sowing the spawned plant.
    Sowing { plant: ItemId, work_done: f32 },
}

/// Progress through a deconstruction job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DeconstructStage {
    /// Walking to touch the building.
    Goto,
    /// Working: work left.
    Work { work_left: f32 },
}

/// Progress through a mining job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MineStage {
    /// Walking to touch the rock.
    Goto,
    /// Swinging the pick: ticks to the next hit (`ticksToPickHit`, -1000
    /// before the first).
    Mining { ticks_to_hit: i32 },
}

/// Progress through a refuel job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RefuelStage {
    /// Walking onto the fuel.
    GotoFuel,
    /// Carrying it to touch the building.
    GotoBuilding,
    /// `Toils_General.Wait(240)`.
    Wait { ticks_left: i32 },
}

/// Progress through a roof job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RoofStage {
    /// Walking to touch the cell.
    Goto,
    /// Working: roof work left (65 at the start).
    Work { work_left: f32 },
}

/// Progress through a feeding job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FeedStage {
    GotoFood,
    CarryToPatient,
    /// The patient chews; ends at zero (`ChewIngestible`, ×1.5 time).
    Feeding {
        ticks_left: i32,
    },
}

/// Progress through a repair job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FixStage {
    GotoComponent,
    GotoBuilding,
    Work { ticks_left: i32 },
}

/// Progress through a research job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ResearchStage {
    Goto,
    /// The research toil (`JobEndInterval` ticks).
    Work {
        ticks_left: i32,
    },
    /// The closing `Wait(2)`.
    Wait {
        ticks_left: i32,
    },
}

/// Progress through `JobDriver_DoBill`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DoBillStage {
    /// Not started yet.
    Start,
    /// Walking to the ingredient (`GotoThing` ClosestTouch).
    GotoIngredient,
    /// Carrying it to the table's interaction cell.
    CarryToTable,
    /// Walking to the interaction cell to work.
    GotoTable,
    /// `DoRecipeWork`: work left and ticks spent (`workLeft`,
    /// `ticksSpentDoingRecipeWork`).
    Work { work_left: f32, spent: i32 },
}

/// A recreation activity and where it happens.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum JoyActivity {
    /// Walking through the first `len` of `path`'s waypoints (targetA
    /// then `targetQueueA`); `next` is the one walked to.
    Walk {
        path: [crate::grid::Cell; crate::recreation::WALK_PATH_LEN],
        len: u8,
        next: u8,
    },
    /// Lying on an unroofed cell looking up.
    Skygaze { cell: crate::grid::Cell },
    /// Standing in one's room (praying, meditating).
    Relax { cell: crate::grid::Cell },
}

impl JoyActivity {
    /// Where the pawn is heading.
    pub fn cell(&self) -> crate::grid::Cell {
        match *self {
            Self::Walk { path, next, .. } => path[next as usize],
            Self::Skygaze { cell } | Self::Relax { cell } => cell,
        }
    }
}

/// Progress through a recreation job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum JoyStage {
    Goto,
    /// The activity's Delay toil (`joyDuration`).
    Active {
        ticks_left: i32,
    },
}

/// Progress through a flick job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FlickStage {
    Goto,
    Wait { ticks_left: i32 },
}

/// Progress through a tending job.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TendStage {
    /// Walking to the patient's interaction cell.
    Goto,
    /// Tending; ends at zero (600 / MedicalTendSpeed ticks).
    Wait { ticks_left: i32 },
}

/// Progress through a plant work job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlantWorkStage {
    /// Taking the next plant from the queue.
    Extract,
    /// Walking to touch it.
    Goto,
    /// Working on it.
    Cutting { work_done: f32 },
}

/// Progress through `JobDriver_HaulToContainer`'s toils.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContainerStage {
    /// Walking to the resource (`GotoThing` A, ClosestTouch).
    GotoSource,
    /// Carrying it to a spot touching the container (`GotoBuild` B).
    CarryToContainer,
}

/// Progress through `JobDriver_ConstructFinishFrame`'s toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BuildStage {
    /// Walking to a spot touching the frame (`GotoBuild`).
    Goto,
    /// Working on the frame; the toil ends after `ticks_left` ticks.
    Build { ticks_left: i32 },
}

/// Progress through the haul job's toils.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HaulStage {
    /// Walking to the source (toil 4).
    GotoSource,
    /// Carrying to the destination cell (toil 9).
    CarryToCell,
    /// At the cell, waiting for the minimum duration (toil 10).
    Delay,
}

/// Progress through the Hunt driver's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HuntStage {
    /// Not started yet.
    Start,
    /// Walking to a cast position (toil 2).
    GotoCast { cell: Cell },
    /// Shooting: waiting until no longer busy (toil 5).
    Cast,
    /// Walking to touch the downed prey (toil 9).
    GotoVictim,
    /// The execution wait (toil 10).
    ExecuteWait { ticks_left: i32 },
}

/// Progress through the cleaning job's toils.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CleanStage {
    /// Taking the next filth from the queue.
    Extract,
    /// Walking to touch the target.
    Goto,
    /// Cleaning; one thickness level per `cleaningWorkToReduceThickness`.
    Cleaning { work_done: f32 },
}

/// Progress through the Ingest job's toils (tool users, no chairs yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IngestStage {
    /// Walking to the food (`dest` is where touching it is possible).
    GotoFood { dest: Cell },
    /// Carrying the picked-up food to a spot to eat standing.
    CarryToChewSpot { spot: Cell },
    /// Chewing; the job finishes when the countdown reaches zero.
    Chew { ticks_left: i32 },
}

/// Where a predator's hunt is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PredatorStage {
    /// Following and attacking the prey.
    Follow,
    /// Walking to the corpse.
    GotoCorpse,
    /// Chewing a meal off the corpse.
    Chew { ticks_left: i32 },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Job {
    /// `None` only when the matching JobDef is missing from the data.
    pub def: Option<DefId<JobDef>>,
    pub kind: JobKind,
    /// Ordered by the player (not chosen by the pawn's own AI).
    pub forced: bool,
    /// Movement urgency (jobs default to Jog; wandering uses Walk).
    pub urgency: LocomotionUrgency,
    /// Tick the job started (set when started).
    pub start_tick: u64,
}

/// JobDefs the simulation needs, resolved once by defName.
#[derive(Debug, Clone, Default)]
pub struct JobDefs {
    pub goto: Option<DefId<JobDef>>,
    pub goto_wander: Option<DefId<JobDef>>,
    pub wait_wander: Option<DefId<JobDef>>,
    pub lay_down: Option<DefId<JobDef>>,
    pub wait_maintain_posture: Option<DefId<JobDef>>,
    pub ingest: Option<DefId<JobDef>>,
    pub clean: Option<DefId<JobDef>>,
    pub haul_to_cell: Option<DefId<JobDef>>,
    pub wait: Option<DefId<JobDef>>,
    pub haul_to_container: Option<DefId<JobDef>>,
    pub finish_frame: Option<DefId<JobDef>>,
    pub sow: Option<DefId<JobDef>>,
    pub harvest: Option<DefId<JobDef>>,
    pub cut_plant: Option<DefId<JobDef>>,
    pub build_roof: Option<DefId<JobDef>>,
    pub refuel: Option<DefId<JobDef>>,
    pub mine: Option<DefId<JobDef>>,
    pub deconstruct: Option<DefId<JobDef>>,
    pub cut_plant_designated: Option<DefId<JobDef>>,
    pub harvest_designated: Option<DefId<JobDef>>,
    pub remove_floor: Option<DefId<JobDef>>,
    pub smooth_floor: Option<DefId<JobDef>>,
    pub smooth_wall: Option<DefId<JobDef>>,
    pub rescue: Option<DefId<JobDef>>,
    pub feed_patient: Option<DefId<JobDef>>,
    pub tend_patient: Option<DefId<JobDef>>,
    pub wait_downed: Option<DefId<JobDef>>,
    pub flick: Option<DefId<JobDef>>,
    pub fix_broken_down: Option<DefId<JobDef>>,
    pub research: Option<DefId<JobDef>>,
    pub do_bill: Option<DefId<JobDef>>,
    pub wait_combat: Option<DefId<JobDef>>,
    pub attack_static: Option<DefId<JobDef>>,
    pub equip: Option<DefId<JobDef>>,
    pub place_no_cost_frame: Option<DefId<JobDef>>,
    pub hunt: Option<DefId<JobDef>>,
    pub flee: Option<DefId<JobDef>>,
}

impl JobDefs {
    pub fn resolve(defs: &GameDefs) -> Self {
        Self {
            goto: defs.jobs.id("Goto"),
            goto_wander: defs.jobs.id("GotoWander"),
            wait_wander: defs.jobs.id("Wait_Wander"),
            lay_down: defs.jobs.id("LayDown"),
            wait_maintain_posture: defs.jobs.id("Wait_MaintainPosture"),
            ingest: defs.jobs.id("Ingest"),
            clean: defs.jobs.id("Clean"),
            haul_to_cell: defs.jobs.id("HaulToCell"),
            wait: defs.jobs.id("Wait"),
            haul_to_container: defs.jobs.id("HaulToContainer"),
            finish_frame: defs.jobs.id("FinishFrame"),
            sow: defs.jobs.id("Sow"),
            harvest: defs.jobs.id("Harvest"),
            cut_plant: defs.jobs.id("CutPlant"),
            build_roof: defs.jobs.id("BuildRoof"),
            refuel: defs.jobs.id("Refuel"),
            mine: defs.jobs.id("Mine"),
            deconstruct: defs.jobs.id("Deconstruct"),
            cut_plant_designated: defs.jobs.id("CutPlantDesignated"),
            harvest_designated: defs.jobs.id("HarvestDesignated"),
            remove_floor: defs.jobs.id("RemoveFloor"),
            smooth_floor: defs.jobs.id("SmoothFloor"),
            smooth_wall: defs.jobs.id("SmoothWall"),
            rescue: defs.jobs.id("Rescue"),
            feed_patient: defs.jobs.id("FeedPatient"),
            tend_patient: defs.jobs.id("TendPatient"),
            wait_downed: defs.jobs.id("Wait_Downed"),
            flick: defs.jobs.id("Flick"),
            wait_combat: defs.jobs.id("Wait_Combat"),
            attack_static: defs.jobs.id("AttackStatic"),
            equip: defs.jobs.id("Equip"),
            place_no_cost_frame: defs.jobs.id("PlaceNoCostFrame"),
            hunt: defs.jobs.id("Hunt"),
            flee: defs.jobs.id("Flee"),
            fix_broken_down: defs.jobs.id("FixBrokenDownBuilding"),
            research: defs.jobs.id("Research"),
            do_bill: defs.jobs.id("DoBill"),
        }
    }
}

/// Wander parameters: the defaults of `JobGiver_WanderColony` /
/// `JobGiver_WanderAnywhere` (radius 7, wait 125–200 ticks); XML parameters
/// override them per think node.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct WanderParams {
    pub radius: i32,
    pub wait_ticks: (u32, u32),
}

impl Default for WanderParams {
    fn default() -> Self {
        Self {
            radius: 7,
            wait_ticks: (125, 200),
        }
    }
}

/// Where a wander job giver wanders around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WanderRoot {
    /// `JobGiver_WanderColony`: the colony wander root.
    Colony,
    /// `JobGiver_WanderAnywhere` / `WanderCurrentRoom`: the pawn's position.
    Own,
}

/// The wander job giver (`JobGiver_Wander`) for an idle pawn.
///
/// `next_is_wait` is the pawn's wait/walk alternation flag. Returns the job
/// and the new flag: the flag flips each call; if it was "wait" the pawn
/// waits; a destination equal to the pawn's own cell also means wait.
/// `colonists` are the free colonists' positions (for the colony root);
/// `available` is the destination-reservation check.
#[allow(clippy::too_many_arguments)]
pub fn think_idle(
    view: &MapView<'_>,
    pawn: &Wanderer,
    next_is_wait: bool,
    defs: &JobDefs,
    params: WanderParams,
    root: WanderRoot,
    colonists: &[Cell],
    available: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> (Option<Job>, bool) {
    let wait = |rng: &mut Rand| Job {
        def: defs.wait_wander,
        kind: JobKind::Wait {
            expiry_interval: rng
                .range_inclusive(params.wait_ticks.0 as i32, params.wait_ticks.1 as i32)
                as u32,
        },
        forced: false,
        urgency: LocomotionUrgency::Jog,
        start_tick: 0,
    };
    // The pawn is never already on a wander walk when thinking here.
    let flag = !next_is_wait;
    if next_is_wait {
        return (Some(wait(rng)), flag);
    }
    let root = match root {
        WanderRoot::Colony => colony_wander_root(view, pawn, colonists, rng),
        WanderRoot::Own => pawn.position,
    };
    let dest = random_wander_dest_for(view, pawn, root, params.radius as f32, available, rng);
    if dest == pawn.position {
        return (Some(wait(rng)), flag);
    }
    (
        Some(Job {
            def: defs.goto_wander,
            kind: JobKind::Goto { target: dest },
            forced: false,
            urgency: LocomotionUrgency::Walk,
            start_tick: 0,
        }),
        flag,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Grid, GridSize};

    #[test]
    fn rot_from_step() {
        assert_eq!(Rot4::from_step(Cell::new(1, 1)), Rot4::East);
        assert_eq!(Rot4::from_step(Cell::new(-1, 0)), Rot4::West);
        assert_eq!(Rot4::from_step(Cell::new(0, 1)), Rot4::North);
        assert_eq!(Rot4::from_step(Cell::new(0, -1)), Rot4::South);
    }

    struct World {
        map: crate::map::Map,
        defs: rimworld_defs::GameDefs,
        grid: crate::path::PathGrid,
        regions: crate::region::Regions,
    }

    fn world(costs: Grid<Option<u32>>) -> World {
        let (db, _) = rimworld_defs::load_documents(
            "t",
            &[(
                "t.xml",
                "<Defs><TerrainDef><defName>Soil</defName></TerrainDef></Defs>",
            )],
            &rimworld_defs::xml::ActivePackages::default(),
        );
        let defs = rimworld_defs::GameDefs::from_database(db).0;
        let map = crate::map::Map::new(costs.size(), defs.terrain.id("Soil").unwrap());
        let grid = crate::path::PathGrid::new(costs);
        let regions = crate::region::Regions::build(&map, &defs, &grid);
        World {
            map,
            defs,
            grid,
            regions,
        }
    }

    impl World {
        fn view(&self) -> MapView<'_> {
            MapView {
                map: &self.map,
                defs: &self.defs,
                grid: &self.grid,
                regions: &self.regions,
            }
        }
    }

    fn pawn(at: Cell) -> Wanderer {
        Wanderer {
            position: at,
            costs: crate::path::MoveCosts::from_move_speed(4.6),
        }
    }

    #[test]
    fn idle_alternates_and_targets_reachable_cells() {
        let w = world(Grid::new(GridSize::new(20, 20), Some(0)));
        let mut rng = Rand::new(1);
        let defs = JobDefs::default();
        let p = WanderParams::default();
        let at = Cell::new(10, 10);
        let me = pawn(at);
        let think = |wait: bool, rng: &mut Rand| {
            think_idle(
                &w.view(),
                &me,
                wait,
                &defs,
                p,
                WanderRoot::Own,
                &[at],
                &|_| true,
                rng,
            )
        };
        match think(true, &mut rng).0.unwrap().kind {
            JobKind::Wait { expiry_interval } => assert!((125..=200).contains(&expiry_interval)),
            other => panic!("{other:?}"),
        }
        let mut walked = false;
        for _ in 0..10 {
            if let JobKind::Goto { target } = think(false, &mut rng).0.unwrap().kind {
                let d = (target.x - at.x).pow(2) + (target.z - at.z).pow(2);
                assert!(d <= p.radius * p.radius && w.grid.walkable(target));
                walked = true;
            }
        }
        assert!(walked);
    }

    #[test]
    fn wander_respects_destination_reservations() {
        // 3x1 corridor: from the middle, the only targets are the two ends.
        let w = world(Grid::new(GridSize::new(3, 1), Some(0)));
        let mut rng = Rand::new(9);
        let me = pawn(Cell::new(1, 0));
        for _ in 0..20 {
            let (job, _) = think_idle(
                &w.view(),
                &me,
                false,
                &JobDefs::default(),
                WanderParams::default(),
                WanderRoot::Own,
                &[],
                &|c| c != Cell::new(0, 0),
                &mut rng,
            );
            if let Some(Job {
                kind: JobKind::Goto { target },
                ..
            }) = job
            {
                assert_eq!(target, Cell::new(2, 0));
            }
        }
    }

    #[test]
    fn no_destination_means_waiting_in_place() {
        let mut costs = Grid::new(GridSize::new(5, 5), None);
        costs[Cell::new(2, 2)] = Some(0);
        let w = world(costs);
        let (job, flag) = think_idle(
            &w.view(),
            &pawn(Cell::new(2, 2)),
            false,
            &JobDefs::default(),
            WanderParams::default(),
            WanderRoot::Own,
            &[],
            &|_| true,
            &mut Rand::new(3),
        );
        assert!(matches!(job.unwrap().kind, JobKind::Wait { .. }));
        assert!(flag);
    }
}
