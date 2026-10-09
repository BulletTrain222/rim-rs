//! Research (docs/research.md §56): the colony's projects
//! (`ResearchManager`), the research bench's speed (`ResearchSpeedFactor`
//! and its room parts) and the research job (`JobDriver_Research`).

use std::collections::BTreeMap;

use rimworld_defs::{DefId, ThingDef, food_type};
use serde::{Deserialize, Serialize};

use super::{JobEvent, Sim, tick_movement};
use crate::grid::Cell;
use crate::job::{Job, JobKind, ResearchStage};
use crate::map::{ItemId, Map};
use crate::path::PathGrid;
use crate::pawn::Pawn;
use crate::research::{
    RESEARCH_POINTS_PER_SPEED, RESEARCH_TOIL_TICKS, RESEARCH_XP_PER_TICK, ResearchBench,
    cost_factor,
};
use crate::stats::{def_stat, terrain_stat};

/// The colony's research (`ResearchManager`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchState {
    /// The selected project (`currentProj`).
    pub current: Option<String>,
    /// Points per project (`progress`).
    pub progress: BTreeMap<String, f32>,
    /// The player faction's tech level (`TechLevel` as an int).
    #[serde(default = "industrial")]
    pub tech_level: i32,
}

fn industrial() -> i32 {
    4
}

impl Default for ResearchState {
    fn default() -> Self {
        Self {
            current: None,
            progress: BTreeMap::new(),
            tech_level: industrial(),
        }
    }
}

/// `Room.Role`'s outcome: the role, as its `RoomRoleDef` name.
pub type RoomRole = &'static str;

/// `StatPart_WorkTableTemperature`: outside 9..35 °C work goes at 70%.
const WORK_TABLE_MIN_TEMP: f32 = 9.0;
const WORK_TABLE_MAX_TEMP: f32 = 35.0;
const WORK_TABLE_TEMP_FACTOR: f32 = 0.7;
/// `Room.MaxRegionsToAssignRoomRole`.
const MAX_REGIONS_FOR_ROLE: usize = 60;

/// What the room code needs about one room.
struct RoomFacts {
    cells: Vec<Cell>,
    regions: usize,
    touches_edge: bool,
    any_normal_region: bool,
    open_roof: usize,
}

impl RoomFacts {
    /// `ProperRoom` with at most 60 regions: rooms that get a role and stats.
    fn has_stats(&self) -> bool {
        !self.touches_edge && self.any_normal_region && self.regions <= MAX_REGIONS_FOR_ROLE
    }

    /// `PsychologicallyOutdoors`.
    fn psychologically_outdoors(&self) -> bool {
        self.open_roof >= 300
            || (self.touches_edge && self.open_roof as f32 / self.cells.len().max(1) as f32 >= 0.5)
    }

    /// `OutdoorsForWork`.
    fn outdoors_for_work(&self) -> bool {
        self.open_roof > 100 || self.open_roof as f32 > self.cells.len() as f32 * 0.25
    }
}

impl Sim {
    /// The research state (for display and saving).
    pub fn research(&self) -> &ResearchState {
        &self.research
    }

    /// `ResearchProjectDef.Cost`.
    pub fn research_cost(&self, project: &str) -> f32 {
        self.defs.research.get(project).map_or(0.0, |p| p.base_cost)
    }

    /// `ProgressReal`.
    pub fn research_progress(&self, project: &str) -> f32 {
        self.research.progress.get(project).copied().unwrap_or(0.0)
    }

    /// `IsFinished`.
    pub fn research_finished(&self, project: &str) -> bool {
        self.defs.research.get(project).is_some()
            && self.research_progress(project) >= self.research_cost(project)
    }

    /// `PrerequisitesCompleted`: visible and hidden prerequisites.
    pub fn research_prerequisites_completed(&self, project: &str) -> bool {
        self.defs.research.get(project).is_some_and(|p| {
            p.prerequisites
                .iter()
                .chain(&p.hidden_prerequisites)
                .all(|q| self.research_finished(q))
        })
    }

    /// `CanStartNow`: unfinished, prerequisites done, and a suitable bench
    /// if the project needs one.
    // COMPATIBILITY TODO: currently approximate — techprints, analysed
    // things and DLC visibility (`hideWhen`) are not modelled.
    pub fn can_start_research(&self, project: &str) -> bool {
        let Some(p) = self.defs.research.get(project) else {
            return false;
        };
        !self.research_finished(project)
            && self.research_prerequisites_completed(project)
            && (p.required_research_building.is_none()
                || self
                    .research_bench_ids()
                    .iter()
                    .any(|&b| self.can_be_researched_at(project, b, true)))
    }

    /// The selected project (`GetProject`).
    pub fn current_research(&self) -> Option<&str> {
        self.research.current.as_deref()
    }

    /// `SetCurrentProject`; `None` clears it. A project that cannot start
    /// now is refused.
    pub fn set_research_project(&mut self, project: Option<&str>) -> bool {
        match project {
            None => {
                self.research.current = None;
                true
            }
            Some(p) if self.can_start_research(p) => {
                self.research.current = Some(p.to_owned());
                true
            }
            Some(_) => false,
        }
    }

    /// `FinishProject`: its unfinished prerequisites first, then its
    /// progress set to its cost; it stops being the current project.
    pub fn finish_research(&mut self, project: &str) {
        let defs = self.defs.clone();
        let Some(p) = defs.research.get(project) else {
            return;
        };
        for q in &p.prerequisites {
            if !self.research_finished(q) {
                self.finish_research(q);
            }
        }
        if p.base_cost > 0.0 {
            self.research
                .progress
                .insert(project.to_owned(), p.base_cost);
        }
        if self.research.current.as_deref() == Some(project) {
            self.research.current = None;
        }
    }

    /// `ResearchUtility.ApplyPlayerStartingResearch` for the player
    /// faction `faction` (a `FactionDef`): every project carrying one of
    /// its `startingResearchTags` is finished; its tech level is recorded.
    pub fn apply_starting_research(&mut self, faction: &str) {
        let defs = self.defs.clone();
        let Some(def) = defs.raw.get("FactionDef", faction) else {
            return;
        };
        if let Some(level) = def.node.child_text("techLevel") {
            self.research.tech_level = rimworld_defs::tech_level(level);
        }
        for tag in def.node.child_list_texts("startingResearchTags") {
            for (_, p) in defs.research.iter() {
                if p.tags.contains(&tag) {
                    self.finish_research(&p.def_name);
                }
            }
        }
    }

    /// `BuildableDef.IsResearchFinished`: all its research prerequisites
    /// are done.
    pub fn research_unlocked(&self, def: DefId<ThingDef>) -> bool {
        self.defs.things[def]
            .research_prerequisites
            .iter()
            .all(|p| self.research_finished(p))
    }

    /// `ResearchPerformed`: `amount` of speed-ticks of work by a colonist.
    fn research_performed(&mut self, amount: f32) {
        let Some(project) = self.research.current.clone() else {
            return;
        };
        let Some(p) = self.defs.research.get(&project) else {
            return;
        };
        // COMPATIBILITY TODO: currently approximate — the difficulty's
        // research speed factor is taken as 1 (most presets).
        let points = amount * RESEARCH_POINTS_PER_SPEED
            / cost_factor(p.tech_level, self.research.tech_level);
        *self.research.progress.entry(project.clone()).or_insert(0.0) += points;
        if self.research_finished(&project) {
            self.finish_research(&project);
        }
    }

    /// Research benches on the map (`Building_ResearchBench`).
    fn research_bench_ids(&self) -> Vec<ItemId> {
        self.map
            .structures()
            .iter()
            .filter(|s| {
                self.defs.things[s.def].thing_class.as_deref() == Some("Building_ResearchBench")
            })
            .map(|s| s.id)
            .collect()
    }

    /// `CanBeResearchedAt`: the project's required bench, power (unless
    /// ignored) and linked facilities.
    // COMPATIBILITY TODO: currently approximate — facilities are not
    // modelled, so a project needing one can never be researched.
    fn can_be_researched_at(&self, project: &str, bench: ItemId, ignore_power: bool) -> bool {
        let (Some(p), Some(s)) = (self.defs.research.get(project), self.map.structure(bench))
        else {
            return false;
        };
        let def = &self.defs.things[s.def];
        if p.required_research_building
            .as_deref()
            .is_some_and(|b| b != def.def_name)
        {
            return false;
        }
        if !ignore_power
            && def
                .power
                .as_ref()
                .is_some_and(|pw| pw.comp_class == "CompPowerTrader")
            && !s.power.on
        {
            return false;
        }
        p.required_research_facilities.is_empty()
    }

    /// `Thing.InteractionCell`: the def's offset turned by the rotation.
    pub fn interaction_cell(&self, id: ItemId) -> Option<Cell> {
        let s = self.map.structure(id)?;
        let (x, z) = self.defs.things[s.def].interaction_cell_offset?;
        let (dx, dz) = crate::geom::rotate_offset((x, z), s.footprint.rot);
        Some(Cell::new(
            s.footprint.center.x + dx,
            s.footprint.center.z + dz,
        ))
    }

    /// The benches the research work giver may use (none without a
    /// project).
    pub(super) fn research_benches(&mut self) -> Vec<ResearchBench> {
        let Some(project) = self.research.current.clone() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for id in self.research_bench_ids() {
            let Some(cell) = self.interaction_cell(id) else {
                continue;
            };
            let usable = self.can_be_researched_at(&project, id, false);
            let speed_factor = self.research_speed_factor(id);
            out.push(ResearchBench {
                id,
                cell,
                usable,
                speed_factor,
            });
        }
        out
    }

    fn room_facts(&self, room: usize) -> RoomFacts {
        let regions = self
            .regions
            .rooms()
            .into_iter()
            .nth(room)
            .unwrap_or_default();
        let cells = crate::roof::room_cells(&self.regions, &regions);
        let size = self.map.size();
        let touches_edge = cells
            .iter()
            .any(|c| c.x == 0 || c.z == 0 || c.x == size.width - 1 || c.z == size.height - 1);
        let any_normal_region = regions
            .iter()
            .any(|&r| self.regions.region(r).kind == crate::region::RegionType::Normal);
        let open_roof = cells.iter().filter(|&&c| !self.map.roofed(c)).count();
        RoomFacts {
            regions: regions.len(),
            cells,
            touches_edge,
            any_normal_region,
            open_roof,
        }
    }

    /// Things in a room or next to it (`ContainedAndAdjacentThings`):
    /// buildings and items with a cell in the room or touching it.
    // COMPATIBILITY TODO: currently approximate — the game lists the things
    // registered in the room's regions (including those that only touch
    // them); here a footprint within one cell of a room cell counts.
    fn room_things(
        &self,
        cells: &[Cell],
    ) -> (Vec<&crate::map::Structure>, Vec<(DefId<ThingDef>, u32)>) {
        let set: std::collections::HashSet<Cell> = cells.iter().copied().collect();
        let near = |fp: &crate::geom::Footprint| {
            fp.cells()
                .chain(fp.adjacent_8_way())
                .any(|c| set.contains(&c))
        };
        let buildings = self
            .map
            .structures()
            .iter()
            .filter(|s| near(&s.footprint))
            .collect();
        let items = self
            .map
            .items()
            .iter()
            .filter(|i| near(&crate::geom::Footprint::single(i.position)))
            .map(|i| (i.def, i.stack_count))
            .collect();
        (buildings, items)
    }

    /// `Room.Role` (`RoomRoleDef` workers' highest score, the first on
    /// ties; `None` for rooms without stats).
    // COMPATIBILITY TODO: currently approximate — no prisoners, medical
    // beds or love relations (two colonists owning beds in one room make
    // barracks).
    /// `Room.PsychologicallyOutdoors`.
    pub(super) fn room_psychologically_outdoors(&self, room: usize) -> bool {
        self.room_facts(room).psychologically_outdoors()
    }

    /// The room stat `FoodPoisonChance`: a curve over cleanliness, or its
    /// roomless 0.02 for rooms without stats.
    pub(super) fn room_food_poison_chance(&self, room: usize) -> f32 {
        let facts = self.room_facts(room);
        if !facts.has_stats() {
            return 0.02;
        }
        crate::food::evaluate_curve(
            &[(-5.0, 0.05), (-3.5, 0.025), (-2.0, 0.0)],
            self.room_cleanliness(&facts),
        )
    }

    /// A room's cells.
    pub(super) fn room_cells(&self, room: usize) -> Vec<Cell> {
        self.room_facts(room).cells
    }

    pub(super) fn room_role(&self, room: usize) -> RoomRole {
        let facts = self.room_facts(room);
        if !facts.has_stats() {
            return "None";
        }
        let (buildings, _) = self.room_things(&facts.cells);
        let defs = &self.defs;
        let count = |f: &dyn Fn(&ThingDef) -> bool| {
            buildings.iter().filter(|s| f(&defs.things[s.def])).count() as f32
        };
        let role_count = |role: &str| {
            count(&|d| {
                d.building
                    .as_ref()
                    .and_then(|b| b.work_table_room_role.as_deref())
                    == Some(role)
            })
        };
        // Bedroom and barracks: humanlike beds counting for them
        // (`RoomRoleWorker_Bedroom.IsBedroom`).
        let beds: Vec<&crate::map::Structure> = buildings
            .iter()
            .filter(|s| {
                let d = &defs.things[s.def];
                d.is_bed()
                    && d.building
                        .as_ref()
                        .is_some_and(|b| b.bed_humanlike && b.bed_counts_for_bedroom_or_barracks)
            })
            .copied()
            .collect();
        let empty = beds
            .iter()
            .filter(|s| {
                s.owners.is_empty()
                    && defs.things[s.def]
                        .building
                        .as_ref()
                        .is_some_and(|b| b.bed_empty_counts_for_barracks)
            })
            .count();
        let owned = beds.iter().filter(|s| !s.owners.is_empty()).count();
        let first = beds.iter().find_map(|s| s.owners.first());
        let strangers = beds
            .iter()
            .filter(|s| s.owners.iter().any(|o| Some(o) != first))
            .count();
        let is_bedroom = if (empty == 1 && owned == 0) || (empty == 0 && owned == 1) {
            true
        } else if empty > 0 {
            false
        } else {
            first.is_none() || strangers == 0
        };
        let bedroom = if beds.is_empty() || !is_bedroom {
            0.0
        } else {
            100_000.0
        };
        let barracks = if is_bedroom {
            0.0
        } else {
            beds.len() as f32 * 100_100.0
        };
        let human_food = |d: &ThingDef| {
            d.ingestible
                .as_ref()
                .is_some_and(|i| i.food_type & food_type::OMNIVORE_HUMAN != 0)
                && def_stat(defs, d, None, "Nutrition") > 0.0
        };
        let kitchen = count(&|d| {
            d.designation_category.as_deref() == Some("Production")
                && self
                    .recipe_products(d)
                    .iter()
                    .any(|&p| human_food(&defs.things[p]))
        });
        let rec = count(&|d| self.counts_for_rec_room(d));
        let scores: [(RoomRole, f32); 15] = [
            ("None", -1.0),
            ("Room", 0.99),
            ("Bedroom", bedroom),
            ("PrisonCell", 0.0),
            (
                "DiningRoom",
                12.0 * count(&|d| {
                    d.category.as_deref() == Some("Building")
                        && d.surface_type.as_deref() == Some("Eat")
                }),
            ),
            ("RecRoom", 7.0 * rec),
            ("Hospital", 0.0),
            ("Laboratory", 60.0 * role_count("Laboratory")),
            ("Workshop", 27.0 * role_count("Workshop")),
            (
                "Storeroom",
                count(&|d| d.thing_class.as_deref() == Some("Building_Storage")),
            ),
            ("Barracks", barracks),
            ("PrisonBarracks", 0.0),
            ("Kitchen", 28.0 * kitchen),
            (
                "Tomb",
                50.0 * count(&|d| d.thing_class.as_deref() == Some("Building_Sarcophagus")),
            ),
            (
                "Barn",
                7.6 * count(&|d| {
                    d.is_bed() && d.building.as_ref().is_some_and(|b| !b.bed_humanlike)
                }),
            ),
        ];
        let mut best = scores[0];
        for s in scores {
            if s.1 > best.1 {
                best = s;
            }
        }
        best.0
    }

    /// `ThingDef.AllRecipes`' products: its own `recipes` and recipes
    /// naming it among their `recipeUsers`.
    fn recipe_products(&self, def: &ThingDef) -> Vec<DefId<ThingDef>> {
        let raw = &self.defs.raw;
        let mut recipes: Vec<String> = raw
            .get("ThingDef", &def.def_name)
            .map(|d| d.node.child_list_texts("recipes"))
            .unwrap_or_default();
        for r in raw.table("RecipeDef").into_iter().flat_map(|t| t.iter()) {
            if r.node
                .child_list_texts("recipeUsers")
                .iter()
                .any(|u| u == &def.def_name)
            {
                recipes.push(r.def_name.clone());
            }
        }
        recipes
            .iter()
            .filter_map(|r| raw.get("RecipeDef", r))
            .flat_map(|r| {
                r.node
                    .child("products")
                    .map(|p| {
                        p.children
                            .iter()
                            .filter_map(|c| self.defs.things.id(&c.name))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// A `JoyGiverDef` (`countsForRecRoom`, default true) lists the def.
    fn counts_for_rec_room(&self, def: &ThingDef) -> bool {
        def.category.as_deref() == Some("Building")
            && self
                .defs
                .raw
                .table("JoyGiverDef")
                .into_iter()
                .flat_map(|t| t.iter())
                .any(|j| {
                    j.node.child_text("countsForRecRoom").map(str::trim) != Some("false")
                        && j.node
                            .child_list_texts("thingDefs")
                            .iter()
                            .any(|t| t == &def.def_name)
                })
    }

    /// The room's `Cleanliness` (`RoomStatWorker_Cleanliness`): the
    /// cleanliness of its things and floors over its cell count.
    fn room_cleanliness(&self, facts: &RoomFacts) -> f32 {
        let defs = &self.defs;
        let (buildings, items) = self.room_things(&facts.cells);
        let mut sum: f32 = buildings
            .iter()
            .map(|s| def_stat(defs, &defs.things[s.def], None, "Cleanliness"))
            .sum();
        sum += items
            .iter()
            .map(|&(d, n)| n as f32 * def_stat(defs, &defs.things[d], None, "Cleanliness"))
            .sum::<f32>();
        sum += facts
            .cells
            .iter()
            .map(|&c| terrain_stat(defs, &defs.terrain[self.map.terrain[c]], "Cleanliness"))
            .sum::<f32>();
        sum / facts.cells.len().max(1) as f32
    }

    /// A building's `ResearchSpeedFactor`: its base value, then the stat's
    /// parts — outdoors, room role, temperature, room cleanliness and
    /// reading bonus — and the 0.25 floor.
    // COMPATIBILITY TODO: currently approximate — the bench's stuff and
    // quality are ignored; the reading bonus (books in bookcases) is 1.
    pub fn research_speed_factor(&mut self, bench: ItemId) -> f32 {
        let Some(s) = self.map.structure(bench) else {
            return 0.0;
        };
        let def = self.defs.things[s.def].clone();
        let center = s.footprint.center;
        let mut v = def_stat(&self.defs, &def, None, "ResearchSpeedFactor");
        let room = self.room_at(center);
        let facts = room.map(|r| self.room_facts(r));
        // `StatPart_Outdoors` (0.75 outdoors).
        if let Some(f) = &facts
            && (f.outdoors_for_work() || !self.map.roofed(center))
        {
            v *= 0.75;
        }
        // `StatPart_WorkTableRoomRole`.
        if let (Some(r), Some(f), Some(b)) = (room, &facts, def.building.as_ref())
            && let Some(want) = b.work_table_room_role.as_deref()
            && !f.psychologically_outdoors()
            && self.room_role(r) != want
        {
            v *= b.work_table_not_in_room_role_factor;
        }
        // `StatPart_WorkTableTemperature`.
        let t = self.cell_temperature(center);
        if !(WORK_TABLE_MIN_TEMP..=WORK_TABLE_MAX_TEMP).contains(&t) {
            v *= WORK_TABLE_TEMP_FACTOR;
        }
        // `StatPart_RoomStat` ResearchSpeedFactor: the room stat curve on
        // cleanliness, or its roomless 0.75.
        if let Some(f) = &facts {
            v *= if f.has_stats() {
                crate::food::evaluate_curve(
                    &[(-5.0, 0.75), (-2.5, 0.85), (0.0, 1.0), (1.0, 1.15)],
                    self.room_cleanliness(f),
                )
            } else {
                0.75
            };
        }
        v.max(0.25)
    }

    /// The research job starts: walk to the bench's interaction cell.
    pub(super) fn begin_research(&mut self, i: usize) -> bool {
        let Some(JobKind::Research { cell, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        // Already there: the toil starts after this tick's driver tick, so
        // its first tick is not paid.
        if self.pawns[i].position == cell {
            set_research_stage(
                &mut self.pawns[i],
                ResearchStage::Work {
                    ticks_left: RESEARCH_TOIL_TICKS,
                },
            );
            return true;
        }
        self.walk_to(i, cell, false)
    }

    /// At the bench: the 4,000-tick research toil, its first tick paid on
    /// arrival.
    pub(super) fn start_research_toil(&mut self, i: usize) {
        set_research_stage(
            &mut self.pawns[i],
            ResearchStage::Work {
                ticks_left: RESEARCH_TOIL_TICKS - 1,
            },
        );
    }

    /// The research toil's interval: ResearchSpeed × the bench's factor ×
    /// delta research, 0.1 × delta Intellectual experience; the job fails
    /// without a project or with the bench unusable.
    // COMPATIBILITY TODO: currently approximate — the bench's factor is
    // recomputed every interval (the game caches stats); comfort from a
    // chair is not gained.
    pub(super) fn research_interval(&mut self, i: usize, delta: i32) {
        let Some(Job {
            kind:
                JobKind::Research {
                    bench,
                    stage: ResearchStage::Work { .. },
                    ..
                },
            ..
        }) = self.pawns[i].job
        else {
            return;
        };
        let Some(project) = self.research.current.clone() else {
            self.end_job(i, false);
            return;
        };
        if !self.can_be_researched_at(&project, bench, false) {
            self.end_job(i, false);
            return;
        }
        let speed = self.pawn_stat_of(i, "ResearchSpeed") * self.research_speed_factor(bench);
        self.research_performed(speed * delta as f32);
        self.learn(i, "Intellectual", RESEARCH_XP_PER_TICK * delta as f32);
    }
}

fn set_research_stage(pawn: &mut Pawn, new: ResearchStage) {
    if let Some(Job {
        kind: JobKind::Research { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new;
    }
}

/// The per-tick part of the research job: walking, the toil's countdown,
/// then the two-tick wait.
pub(super) fn tick_research(
    pawn: &mut Pawn,
    map: &Map,
    grid: &PathGrid,
    t: u64,
) -> Option<JobEvent> {
    let JobKind::Research { bench, stage, .. } = pawn.job.as_ref()?.kind else {
        return None;
    };
    if map.structure(bench).is_none() {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        ResearchStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToResearch
            }
        }
        ResearchStage::Work { ticks_left } => {
            let left = ticks_left - 1;
            set_research_stage(
                pawn,
                if left <= 0 {
                    // `Wait(2)` starts without paying this tick.
                    ResearchStage::Wait { ticks_left: 2 }
                } else {
                    ResearchStage::Work { ticks_left: left }
                },
            );
            JobEvent::None
        }
        ResearchStage::Wait { ticks_left } => {
            let left = ticks_left - 1;
            set_research_stage(pawn, ResearchStage::Wait { ticks_left: left });
            if left <= 0 {
                JobEvent::Ended(true)
            } else {
                JobEvent::None
            }
        }
    })
}
