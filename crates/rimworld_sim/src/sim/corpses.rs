//! Corpses (`Pawn.Kill`, `Corpse`, docs/research.md §50): a pawn that dies
//! leaves a `Corpse_<race>` item holding it, placed on its cell (or near
//! it); the corpse rots and weathers like any rottable item, and the dead
//! pawn goes with it.

use super::Sim;
use crate::grid::Cell;
use crate::map::ItemId;
use crate::pawn::{Carried, PawnId};

/// `CompRottable.Stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotStage {
    Fresh,
    Rotting,
    Dessicated,
}

impl Sim {
    /// `MakeCorpse` + `GenPlace.TryPlaceThing` (Direct, then Near) for pawn
    /// `i`, which just died: on its own cell even when other items lie
    /// there (as in the game), else nearby.
    // COMPATIBILITY TODO: currently approximate — the bed rotation is not
    // kept.
    pub(super) fn spawn_corpse_hunted(&mut self, i: usize, hunted: bool) {
        let race = &self.defs.things[self.pawns[i].race];
        let Some(def) = self.defs.things.id(&format!("Corpse_{}", race.def_name)) else {
            return;
        };
        let at = self.pawns[i].position;
        let mut thing = Carried {
            id: self.map.allocate_item_id(),
            def,
            count: 1,
            rot: 0.0,
            hit_points: None,
        };
        let id = thing.id;
        if self.map.size().contains(at) && self.path_grid.walkable(at) {
            self.map.spawn_carried(&thing, at);
            self.corpse_placed(id, i, hunted);
            return;
        }
        if !self.place_thing_near(&mut thing, at) {
            self.map.spawn_carried(&thing, at);
        }
        self.corpse_placed(id, i, hunted);
    }

    /// The corpse is down: link it to its pawn and forbid it outside the
    /// home area (`SetForbiddenIfOutsideHomeArea`) unless it was hunted.
    fn corpse_placed(&mut self, id: ItemId, i: usize, hunted: bool) {
        self.corpses.insert(id, self.pawns[i].id);
        if let Some(cell) = self.map.item(id).map(|it| it.position)
            && !hunted
            && !self.map.home[cell]
        {
            let defs = self.defs.clone();
            self.map.set_forbidden(&defs, id, true);
        }
        self.refresh_path_grid();
    }

    /// The corpse holding dead pawn `pawn`, if it still exists.
    pub fn corpse_of(&self, pawn: PawnId) -> Option<ItemId> {
        self.corpses
            .iter()
            .find(|&(_, &p)| p == pawn)
            .map(|(&id, _)| id)
    }

    /// The dead pawn inside corpse `item`.
    pub fn corpse_pawn(&self, item: ItemId) -> Option<PawnId> {
        self.corpses.get(&item).copied()
    }

    /// `CompRottable.Stage` of a rottable item: fresh, rotting from
    /// `daysToRotStart`, dessicated from `daysToDessicated`.
    pub fn rot_stage(&self, item: ItemId) -> Option<RotStage> {
        let it = self.map.item(item)?;
        let props = self.defs.things[it.def].rottable.as_ref()?;
        Some(if it.rot >= props.days_to_dessicated * 60_000.0 {
            RotStage::Dessicated
        } else if it.rot >= props.ticks_to_rot_start() as f32 {
            RotStage::Rotting
        } else {
            RotStage::Fresh
        })
    }

    /// Dead pawns follow their corpses (on the map or carried); a corpse
    /// that is gone takes its pawn with it.
    pub(super) fn sync_corpses(&mut self) {
        if self.corpses.is_empty() {
            return;
        }
        let mut gone = Vec::new();
        for (&item, &pawn) in &self.corpses {
            let at: Option<Cell> = self.map.item(item).map(|it| it.position).or_else(|| {
                self.pawns
                    .iter()
                    .find(|p| p.carried.is_some_and(|c| c.id == item))
                    .map(|p| p.position)
            });
            match at {
                Some(cell) => {
                    if let Some(k) = self.index_of(pawn) {
                        self.pawns[k].position = cell;
                    }
                }
                None => gone.push(item),
            }
        }
        for item in gone {
            self.corpses.remove(&item);
        }
    }
}
