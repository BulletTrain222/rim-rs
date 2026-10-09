//! Growing work givers (docs/research.md §24): `WorkGiver_GrowerHarvest`
//! and `WorkGiver_GrowerSow` over growing zones.

use rimworld_defs::{DefId, JobDef};

use crate::clean::radial_pattern;
use crate::grid::Cell;
use crate::job::{Job, JobKind, PlantWorkStage, SowStage};
use crate::map::ItemId;
use crate::path::LocomotionUrgency;
use crate::plant::{LifeStage, growth_season_now};
use crate::region::Regions;
use crate::reservation::{STACK_ALL, Target};
use crate::work::{WorkContext, WorkGiver};

/// Cells around a harvested plant considered for the same job.
const HARVEST_QUEUE_CELLS: usize = 40;
/// Harvest work after which no more plants are queued.
const HARVEST_QUEUE_WORK: f32 = 2400.0;

/// `WorkGiver_Grower.PotentialWorkCellsGlobal` for growing zones: the cells
/// of every zone passing `extra` whose first cell the pawn can reach.
// COMPATIBILITY TODO: currently approximate — plant growers (hydroponics)
// are not modelled; reachability is region connectivity; the game's
// static `wantedPlantDef` caching across zones is replaced by each cell's
// own zone.
fn zone_cells(
    ctx: &WorkContext<'_>,
    regions: &Regions,
    extra: impl Fn(usize) -> bool,
) -> Vec<Cell> {
    let mut out = Vec::new();
    for z in ctx.map.growing_zones() {
        let zone = ctx.map.growing_zone(z);
        if extra(z) && regions.connected(ctx.position, zone.cells[0]) {
            out.extend(zone.cells.iter().copied());
        }
    }
    out
}

/// `WorkGiver_GrowerHarvest`.
pub struct GrowerHarvest<'a> {
    pub regions: &'a Regions,
    pub job: Option<DefId<JobDef>>,
}

impl WorkGiver for GrowerHarvest<'_> {
    fn allow_unreachable(&self) -> bool {
        true
    }

    fn potential_cells(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        zone_cells(ctx, self.regions, |_| true)
    }

    // COMPATIBILITY TODO: currently approximate — blight, cutting
    // prevention and lords are not modelled.
    fn has_job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> bool {
        let Some(plant) = ctx.map.plant_at(c) else {
            return false;
        };
        let Some(props) = ctx.defs.things[plant.def].plant.as_ref() else {
            return false;
        };
        if !plant.harvestable_now(props)
            || plant.life_stage() != LifeStage::Mature
            || !props.auto_harvestable
        {
            return false;
        }
        if let Some(z) = ctx.map.growing_zone_at(c) {
            let zone = ctx.map.growing_zone(z);
            if !zone.allow_cut && plant.def != zone.plant {
                return false;
            }
        }
        ctx.reservations
            .can_reserve(ctx.claimant, Target::Item(plant.id), 1, 1, STACK_ALL)
    }

    /// Queues the harvestable plants among the 40 nearest cells in the same
    /// room until the queued work passes 2400.
    fn job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> Option<(Job, Vec<ItemId>)> {
        let room = self.regions.room_at(c);
        let mut queue: Vec<ItemId> = Vec::new();
        let mut work = 0.0;
        for &d in &radial_pattern()[..HARVEST_QUEUE_CELLS] {
            let cell = c + d;
            if !ctx.map.size().contains(cell)
                || self.regions.room_at(cell) != room
                || !self.has_job_on_cell(ctx, cell)
            {
                continue;
            }
            let plant = ctx.map.plant_at(cell).expect("checked");
            let wanted = ctx
                .map
                .growing_zone_at(cell)
                .map(|z| ctx.map.growing_zone(z).plant);
            if cell == c || Some(plant.def) == wanted {
                work += ctx.defs.things[plant.def]
                    .plant
                    .as_ref()
                    .map_or(0.0, |p| p.harvest_work);
                if cell != c && work > HARVEST_QUEUE_WORK {
                    break;
                }
                queue.push(plant.id);
            }
        }
        if queue.len() >= 3 {
            // `SortBy` (.NET `List.Sort`, unstable).
            let at = ctx.position;
            let key = |id: &ItemId| {
                ctx.map.plant(*id).map_or(i32::MAX, |p| {
                    (p.position.x - at.x).pow(2) + (p.position.z - at.z).pow(2)
                })
            };
            crate::netsort::sort(&mut queue, |a, b| key(a).cmp(&key(b)));
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Harvest {
                    target: None,
                    stage: PlantWorkStage::Extract,
                    cut: false,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            queue,
        ))
    }
}

/// A `CutPlant` job on one plant.
pub fn cut_job(def: Option<DefId<JobDef>>, plant: ItemId) -> (Job, Vec<ItemId>) {
    (
        Job {
            def,
            kind: JobKind::Harvest {
                target: None,
                stage: PlantWorkStage::Extract,
                cut: true,
            },
            forced: false,
            urgency: LocomotionUrgency::Jog,
            start_tick: 0,
        },
        vec![plant],
    )
}

/// `WorkGiver_PlantsCut`: plants designated for cutting, then those
/// designated for harvest.
// COMPATIBILITY TODO: currently approximate — ideology tree rules and
// cutting prevention are not modelled.
pub struct PlantsCut {
    pub job: Option<DefId<JobDef>>,
    /// `HarvestDesignated`.
    pub harvest_job: Option<DefId<JobDef>>,
}

impl WorkGiver for PlantsCut {
    fn should_skip(&self, ctx: &WorkContext<'_>) -> bool {
        ctx.map.cut_designations().is_empty() && ctx.map.harvest_designations().is_empty()
    }

    fn potential_things(&self, ctx: &WorkContext<'_>) -> Option<Vec<ItemId>> {
        let mut v = ctx.map.cut_designations().to_vec();
        v.extend_from_slice(ctx.map.harvest_designations());
        Some(v)
    }

    fn job_on_thing(&self, ctx: &WorkContext<'_>, t: ItemId) -> Option<(Job, Vec<ItemId>)> {
        let plant = ctx.map.plant(t)?;
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Item(t), 1, 1, STACK_ALL)
        {
            return None;
        }
        // A harvest designation comes first (`AllDesignationsOn` order):
        // only a plant harvestable now gives a job.
        if ctx.map.harvest_designations().contains(&t) {
            let props = ctx.defs.things[plant.def].plant.as_ref()?;
            if plant.growth < props.harvest_min_growth {
                return None;
            }
            let (mut job, queue) = cut_job(self.harvest_job, t);
            if let JobKind::Harvest { cut, .. } = &mut job.kind {
                *cut = false;
            }
            return Some((job, queue));
        }
        Some(cut_job(self.job, t))
    }
}

/// `WorkGiver_GrowerSow`.
pub struct GrowerSow<'a> {
    pub regions: &'a Regions,
    pub job: Option<DefId<JobDef>>,
    /// `CutPlant`, for plants in the way.
    pub cut_job: Option<DefId<JobDef>>,
    /// Outdoor temperature, for cells outside any room.
    pub temperature: f32,
    /// Each room's temperature (`GrowthSeasonNow` uses the cell's).
    pub room_temperatures: &'a [f32],
}

impl WorkGiver for GrowerSow<'_> {
    fn allow_unreachable(&self) -> bool {
        true
    }

    /// Zones that allow sowing (`ExtraRequirements`).
    fn potential_cells(&self, ctx: &WorkContext<'_>) -> Vec<Cell> {
        zone_cells(ctx, self.regions, |z| ctx.map.growing_zone(z).allow_sow)
    }

    // COMPATIBILITY TODO: currently approximate — cutting other plants,
    // adjacent sow blockers (trees), cave plants, roofs, vacuum, minimum
    // sowing skill and hauling blocking items aside are not modelled.
    fn job_on_cell(&self, ctx: &WorkContext<'_>, c: Cell) -> Option<(Job, Vec<ItemId>)> {
        let wanted = ctx.map.growing_zone(ctx.map.growing_zone_at(c)?).plant;
        let props = ctx.defs.things[wanted].plant.as_ref()?;
        let temperature = self
            .regions
            .room_at(c)
            .and_then(|r| self.room_temperatures.get(r).copied())
            .unwrap_or(self.temperature);
        if !growth_season_now(props, temperature) {
            return None;
        }
        if ctx.map.constructible_at(c).is_some() || ctx.map.buildings[c].is_some() {
            return None;
        }
        // A plant of the crop: nothing to do; any other plant is cut first
        // (`BlocksPlanting` → `CutPlant`), if the zone allows cutting.
        if let Some(plant) = ctx.map.plant_at(c) {
            let zone = ctx.map.growing_zone(ctx.map.growing_zone_at(c)?);
            if plant.def == wanted
                || !zone.allow_cut
                || !ctx.reservations.can_reserve(
                    ctx.claimant,
                    Target::Item(plant.id),
                    1,
                    1,
                    STACK_ALL,
                )
            {
                return None;
            }
            return Some(cut_job(self.cut_job, plant.id));
        }
        // `CanNowPlantAt`: fertile enough.
        let fertility = ctx.defs.terrain[ctx.map.terrain[c]].fertility;
        if !props.completely_ignore_fertility && fertility < props.fertility_min {
            return None;
        }
        if !ctx
            .reservations
            .can_reserve(ctx.claimant, Target::Cell(c), 1, 1, STACK_ALL)
        {
            return None;
        }
        Some((
            Job {
                def: self.job,
                kind: JobKind::Sow {
                    cell: c,
                    plant: wanted,
                    stage: SowStage::Goto,
                },
                forced: false,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            Vec::new(),
        ))
    }
}
