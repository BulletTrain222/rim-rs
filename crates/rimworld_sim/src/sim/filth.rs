//! Filth (docs/research.md §38): where filth may lie (`FilthMaker`) and
//! what pawns carry on their feet (`Pawn_FilthTracker`): picking up dirt
//! outdoors, dropping it on floors, leaving trash and tracked dirt behind.

use rimworld_defs::filth_flags as ff;
use rimworld_defs::{DefId, ThingDef};

use super::Sim;
use crate::grid::Cell;
use crate::map::ItemId;
use crate::reservation::Target;

/// `Pawn_FilthTracker` chances per cell entered.
const DROP_CHANCE: f32 = 0.05;
const PICKUP_CHANCE: f32 = 0.1;
const FILTH_RATE_FACTOR: f32 = 0.005;
/// `Rand.Chance(0.66f)`: tracked terrain filth rather than trash.
const TERRAIN_FILTH_CHANCE: f32 = 0.66;
/// `Filth.CanBeThickened`.
const THICKEN_LIMIT: u32 = 5;
/// `Filth.CanFilthAttachNow`: ticks since it last thickened.
const ATTACH_AFTER_TICKS: i64 = 400;

/// Filth on a pawn's feet (`Pawn_FilthTracker.carriedFilth`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CarriedFilth {
    pub def: DefId<ThingDef>,
    pub thickness: u32,
}

/// `Pawn_FilthTracker` state.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PawnFilth {
    pub carried: Vec<CarriedFilth>,
    /// `lastTerrainFilthDef`.
    pub last_terrain: Option<DefId<ThingDef>>,
}

impl Sim {
    fn filth_placement(&self, def: DefId<ThingDef>) -> (u8, bool, u32) {
        self.defs.things[def]
            .filth
            .as_ref()
            .map_or((ff::UNNATURAL, false, 100), |f| {
                (
                    f.placement_mask,
                    f.ignore_filth_multiplier_stat,
                    f.max_thickness,
                )
            })
    }

    fn terrain_sourced(&self, def: DefId<ThingDef>) -> bool {
        self.filth_placement(def).0 & ff::TERRAIN != 0
    }

    /// `FilthMaker.CanMakeFilth` for one cell.
    pub(super) fn can_make_filth(&mut self, c: Cell, def: DefId<ThingDef>, additional: u8) -> bool {
        if !self.map.size().contains(c) {
            return false;
        }
        let (placement, ignore_multiplier, _) = self.filth_placement(def);
        let terrain = &self.defs.terrain[self.map.terrain[c]];
        let mask = terrain.filth_acceptance_mask;
        if !ignore_multiplier && placement & ff::NATURAL == 0 {
            let multiplier = terrain
                .stat_bases
                .get("FilthMultiplier")
                .copied()
                .unwrap_or(1.0);
            if self.rng.value() > multiplier {
                return false;
            }
        }
        let flags = placement | additional;
        // Pawns' filth lands anywhere indoors.
        if mask != 0 && flags & ff::PAWN != 0 {
            if self.map.roofed(c) {
                return true;
            }
            if self.room_at(c).is_some_and(|r| !self.room_uses_outdoor(r)) {
                return true;
            }
        }
        mask != 0 && mask & flags == flags
    }

    /// `FilthMaker.TryMakeFilth`: thickens the same filth on the cell or
    /// makes new filth where allowed; on an unwalkable cell or filth that
    /// can't thicken, tries the eight neighbours in random order (once).
    pub(super) fn try_make_filth(
        &mut self,
        c: Cell,
        def: DefId<ThingDef>,
        additional: u8,
        propagate: bool,
    ) -> bool {
        let existing = self
            .map
            .items()
            .iter()
            .find(|i| i.position == c && i.def == def)
            .map(|i| (i.id, i.thickness));
        if !self.path_grid.walkable(c) || existing.is_some_and(|(_, t)| t >= THICKEN_LIMIT) {
            if propagate {
                let mut around: Vec<Cell> = Cell::NEIGHBORS_8.iter().map(|&d| c + d).collect();
                self.rng.shuffle(&mut around);
                for n in around {
                    if self.map.size().contains(n) && self.try_make_filth(n, def, 0, false) {
                        return true;
                    }
                }
            }
            return false;
        }
        let now = self.tick;
        if let Some((id, _)) = existing {
            let (_, _, max) = self.filth_placement(def);
            if let Some(i) = self.map.items_mut().iter_mut().find(|i| i.id == id) {
                i.grow_tick = now as i64;
                if i.thickness < max {
                    i.thickness += 1;
                }
            }
        } else {
            if !self.can_make_filth(c, def, additional) {
                return false;
            }
            let id = self.map.spawn_filth(def, c, 1, now);
            // `Filth.SpawnSetup`: (int)(disappearsInDays.RandomInRange × 60000).
            let days = self.defs.things[def]
                .filth
                .as_ref()
                .map_or((0.0, 0.0), |f| f.disappears_in_days);
            if days != (0.0, 0.0) {
                let after = (self.rng.range_f32(days.0, days.1) * 60_000.0) as i64;
                if let Some(i) = self.map.items_mut().iter_mut().find(|i| i.id == id) {
                    i.disappear_after = after;
                }
            }
        }
        true
    }

    /// `Pawn_FilthTracker.AdditionalFilthSourceFlags`.
    fn pawn_filth_flags(&self, i: usize) -> u8 {
        let p = &self.pawns[i];
        let animal = self.defs.things[p.race]
            .race
            .as_ref()
            .is_some_and(|r| r.intelligence.as_deref() == Some("Animal"));
        if p.is_colonist || !animal {
            ff::UNNATURAL
        } else {
            ff::NATURAL
        }
    }

    /// `Pawn_FilthTracker.Notify_EnteredNewCell`.
    // COMPATIBILITY TODO: currently approximate — filth sources (labels),
    // game conditions spreading filth and flying pawns are not modelled;
    // only colonists and other non-animals carry the Unnatural flag.
    pub(super) fn pawn_entered_cell(&mut self, i: usize) {
        if self.rng.value() < DROP_CHANCE {
            self.drop_carried_filth(i);
        }
        if self.rng.value() < PICKUP_CHANCE {
            self.pick_up_filth(i);
        }
        let rate = self.pawn_stat_of(i, "FilthRate");
        if self.rng.value() >= rate * FILTH_RATE_FACTOR {
            return;
        }
        let race = self.defs.things[self.pawns[i].race].race.clone();
        let Some(race) = race else {
            return;
        };
        let flags = self.pawn_filth_flags(i) | ff::PAWN;
        let at = self.pawns[i].position;
        let def = if race.intelligence.as_deref() == Some("Humanlike") {
            let last = self.pawns[i].filth.last_terrain;
            match last {
                Some(l) if self.rng.chance(TERRAIN_FILTH_CHANCE) => Some(l),
                _ => self.defs.things.id("Filth_Trash"),
            }
        } else if race.flesh_type.as_deref() == Some("Insectoid") {
            self.defs.things.id("Filth_Slime")
        } else if race.intelligence.as_deref() == Some("Animal") {
            self.defs.things.id("Filth_AnimalFilth")
        } else {
            None
        };
        if let Some(def) = def {
            self.try_make_filth(at, def, flags, true);
        }
    }

    /// `TryDropFilth`: each carried filth that may lie here is dropped.
    fn drop_carried_filth(&mut self, i: usize) {
        let at = self.pawns[i].position;
        let flags = self.pawn_filth_flags(i);
        for k in (0..self.pawns[i].filth.carried.len()).rev() {
            let def = self.pawns[i].filth.carried[k].def;
            if self.can_make_filth(at, def, 0) && self.try_make_filth(at, def, flags, true) {
                self.thin_carried(i, k);
            }
        }
    }

    /// `TryPickupFilth`: the terrain's own filth (other terrain filth on the
    /// feet wears off), then attachable filth lying on the cell.
    fn pick_up_filth(&mut self, i: usize) {
        let at = self.pawns[i].position;
        let generated = self.defs.terrain[self.map.terrain[at]]
            .generated_filth
            .as_deref()
            .and_then(|g| self.defs.things.id(g));
        if let Some(g) = generated {
            for k in (0..self.pawns[i].filth.carried.len()).rev() {
                let def = self.pawns[i].filth.carried[k].def;
                if self.terrain_sourced(def) && def != g {
                    self.thin_carried(i, k);
                }
            }
            if !self.pawns[i].filth.carried.iter().any(|f| f.def == g) {
                self.gain_filth(i, g);
            }
        }
        let now = self.tick as i64;
        let lying: Vec<(ItemId, DefId<ThingDef>)> = self
            .map
            .items_at(at)
            .filter(|it| {
                it.is_filth()
                    && it.thickness > 1
                    && now - it.grow_tick > ATTACH_AFTER_TICKS
                    && self.defs.things[it.def]
                        .filth
                        .as_ref()
                        .is_some_and(|f| f.can_filth_attach)
            })
            .map(|it| (it.id, it.def))
            .collect();
        for (id, def) in lying.into_iter().rev() {
            self.gain_filth(i, def);
            if self.map.thin_filth(id) {
                self.reservations.release_all_for_target(Target::Item(id));
            }
        }
    }

    /// `GainFilth`.
    fn gain_filth(&mut self, i: usize, def: DefId<ThingDef>) {
        if self.terrain_sourced(def) {
            self.pawns[i].filth.last_terrain = Some(def);
        }
        let (_, _, max) = self.filth_placement(def);
        let carried = &mut self.pawns[i].filth.carried;
        match carried.iter_mut().find(|f| f.def == def) {
            Some(f) => {
                if f.thickness < THICKEN_LIMIT && f.thickness < max {
                    f.thickness += 1;
                }
            }
            None => carried.push(CarriedFilth { def, thickness: 1 }),
        }
    }

    fn thin_carried(&mut self, i: usize, k: usize) {
        let carried = &mut self.pawns[i].filth.carried;
        carried[k].thickness = carried[k].thickness.saturating_sub(1);
        if carried[k].thickness == 0 {
            carried.remove(k);
        }
    }
}
