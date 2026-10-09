//! Zones as the zone designators make them (`Designator_ZoneAdd`,
//! `Designator_ZoneDelete`, `Zone`, docs/research.md §66): a drag adds
//! cells to the selected zone of its type, cells touching it join it, the
//! rest open new zones, and a zone keeps only the part connected to its
//! first cell.

use super::Sim;
use crate::grid::Cell;
use crate::storage::{StoragePreset, ZoneId};

/// `GenGrid.NoZoneEdgeWidth`: no zones this close to the map edge.
pub const NO_ZONE_EDGE_WIDTH: i32 = 5;

/// What a zone designator places (`zoneTypeToPlace` and its preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneKind {
    Stockpile,
    DumpingStockpile,
    Growing,
}

impl ZoneKind {
    fn is_stockpile(self) -> bool {
        matches!(self, ZoneKind::Stockpile | ZoneKind::DumpingStockpile)
    }
}

/// A zone on the map (`Zone_Stockpile` or `Zone_Growing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneRef {
    Stockpile(ZoneId),
    Growing(usize),
}

impl ZoneRef {
    fn same_type(self, kind: ZoneKind) -> bool {
        matches!(self, ZoneRef::Stockpile(_)) == kind.is_stockpile()
    }
}

/// `ZoneColorUtility`: each palette colour is lerped halfway to grey and
/// drawn at 9% opacity.
const ZONE_OPACITY: f32 = 0.09;
const STORAGE_COLORS: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 0.5],
    [0.0, 0.5, 1.0],
    [0.5, 0.0, 1.0],
];
const GROWING_COLORS: [[f32; 3]; 5] = [
    [0.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.5, 1.0, 0.0],
    [1.0, 1.0, 0.5],
    [0.5, 1.0, 0.5],
];

fn zone_color(c: [f32; 3]) -> [f32; 4] {
    let g = |v: f32| v + (0.5 - v) * 0.5;
    [g(c[0]), g(c[1]), g(c[2]), ZONE_OPACITY]
}

/// Which label a zone's name is built from (`ZonePresetNames` and the
/// growing zone's): the front end translates it and adds the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneLabel {
    Stockpile,
    DumpingStockpile,
    Growing,
}

impl Sim {
    /// The zone on a cell.
    pub fn zone_at(&self, c: Cell) -> Option<ZoneRef> {
        if !self.map.size().contains(c) {
            return None;
        }
        self.map
            .storage
            .zone_at(c)
            .map(ZoneRef::Stockpile)
            .or_else(|| self.map.growing_zone_at(c).map(ZoneRef::Growing))
    }

    /// A zone's cells, in the order they were added.
    pub fn zone_cells(&self, z: ZoneRef) -> &[Cell] {
        match z {
            ZoneRef::Stockpile(id) => &self.map.storage.zone(id).cells,
            ZoneRef::Growing(g) => &self.map.growing_zone(g).cells,
        }
    }

    /// A zone's name: its label and number ("Stockpile zone" 1).
    pub fn zone_label(&self, z: ZoneRef) -> (ZoneLabel, u32) {
        match z {
            ZoneRef::Stockpile(id) => {
                let s = self.map.storage.zone(id);
                let label = if s.dumping {
                    ZoneLabel::DumpingStockpile
                } else {
                    ZoneLabel::Stockpile
                };
                (label, s.label_number)
            }
            ZoneRef::Growing(g) => (ZoneLabel::Growing, self.map.growing_zone(g).label_number),
        }
    }

    /// A zone's colour (RGBA).
    pub fn zone_color(&self, z: ZoneRef) -> Option<[f32; 4]> {
        match z {
            ZoneRef::Stockpile(id) => self.map.storage.zone(id).color,
            ZoneRef::Growing(g) => self.map.growing_zone(g).color,
        }
    }

    /// Every zone with cells.
    pub fn zones(&self) -> Vec<ZoneRef> {
        let mut v: Vec<ZoneRef> = self
            .map
            .storage
            .live_zones()
            .map(ZoneRef::Stockpile)
            .collect();
        v.extend(self.map.growing_zones().map(ZoneRef::Growing));
        v
    }

    /// `ZoneManager.NewZoneName`: the lowest number from 1 no other zone
    /// of the same label uses.
    fn new_zone_number(&self, label: ZoneLabel) -> u32 {
        let used: Vec<u32> = self
            .zones()
            .into_iter()
            .filter(|&z| self.zone_label(z).0 == label)
            .map(|z| self.zone_label(z).1)
            .collect();
        (1..=1000).find(|n| !used.contains(n)).unwrap_or(0)
    }

    /// `Zone`'s constructor: name and the next colour of its palette
    /// (`NextStorageZoneColor` / `NextGrowingZoneColor`).
    // COMPATIBILITY TODO: currently approximate — the game's palette
    // position is static and restarts each session; here it restarts with
    // each loaded simulation.
    pub(super) fn name_new_zone(&mut self, z: ZoneRef, dumping: bool) {
        match z {
            ZoneRef::Stockpile(id) => {
                let label = if dumping {
                    ZoneLabel::DumpingStockpile
                } else {
                    ZoneLabel::Stockpile
                };
                let n = self.new_zone_number(label);
                let color = zone_color(STORAGE_COLORS[self.zone_colors[0] % STORAGE_COLORS.len()]);
                self.zone_colors[0] += 1;
                self.map.storage.set_meta(id, dumping, n, color);
            }
            ZoneRef::Growing(g) => {
                let n = self.new_zone_number(ZoneLabel::Growing);
                let color = zone_color(GROWING_COLORS[self.zone_colors[1] % GROWING_COLORS.len()]);
                self.zone_colors[1] += 1;
                let zone = self.map.growing_zone_mut(g);
                zone.label_number = n;
                zone.color = Some(color);
            }
        }
    }

    /// `Designator_ZoneAdd.CanDesignateCell` (with the stockpile's and the
    /// growing zone's own checks): in bounds, not in another type's zone,
    /// at least 5 cells from the edge, nothing that can't share a cell with
    /// a zone; stockpiles need passable terrain, growing zones the potato's
    /// minimum fertility.
    // COMPATIBILITY TODO: currently approximate — `CanOverlapZones` is
    // reduced to "no building, blueprint or frame of an impassable or
    // plant-supporting building"; fog is not modelled.
    pub fn can_zone_cell(&self, kind: ZoneKind, c: Cell) -> Result<(), ZoneReject> {
        let size = self.map.size();
        if !size.contains(c) {
            return Err(ZoneReject::Silent);
        }
        if self.zone_at(c).is_some_and(|z| !z.same_type(kind)) {
            return Err(ZoneReject::Silent);
        }
        let e = NO_ZONE_EDGE_WIDTH;
        if c.x < e || c.z < e || c.x >= size.width - e || c.z >= size.height - e {
            return Err(ZoneReject::TooCloseToMapEdge);
        }
        if !self.zone_overlappable(c) {
            return Err(ZoneReject::Silent);
        }
        match kind {
            ZoneKind::Stockpile | ZoneKind::DumpingStockpile => {
                if self.defs.terrain[self.map.terrain[c]].passability
                    == rimworld_defs::Passability::Impassable
                {
                    return Err(ZoneReject::Silent);
                }
            }
            ZoneKind::Growing => {
                let min = self
                    .defs
                    .things
                    .id("Plant_Potato")
                    .and_then(|p| self.defs.things[p].plant.as_ref())
                    .map_or(0.0, |p| p.fertility_min);
                if self.defs.terrain[self.map.terrain[c]].fertility < min {
                    return Err(ZoneReject::Silent);
                }
            }
        }
        Ok(())
    }

    /// `ThingDef.CanOverlapZones` for everything on the cell.
    fn zone_overlappable(&self, c: Cell) -> bool {
        let blocks = |d: rimworld_defs::DefId<rimworld_defs::ThingDef>| {
            let def = &self.defs.things[d];
            def.passability == rimworld_defs::Passability::Impassable
                || def.building.as_ref().is_some_and(|b| b.supports_plants)
        };
        if self.map.buildings[c].is_some_and(blocks) {
            return false;
        }
        if self
            .map
            .structures()
            .iter()
            .any(|s| s.footprint.contains(c) && blocks(s.def))
        {
            return false;
        }
        !self
            .map
            .constructible_at(c)
            .is_some_and(|k| match k.building {
                crate::map::Buildable::Thing(b) => blocks(b),
                crate::map::Buildable::Floor(_) => false,
            })
    }

    /// Makes an empty zone of a kind and adds its first cell.
    fn open_zone(&mut self, kind: ZoneKind, first: Cell) -> ZoneRef {
        let z = match kind {
            ZoneKind::Stockpile | ZoneKind::DumpingStockpile => {
                let preset = if kind == ZoneKind::DumpingStockpile {
                    StoragePreset::DumpingStockpile
                } else {
                    StoragePreset::DefaultStockpile
                };
                let filter = crate::storage::ThingFilter::preset(&self.defs, preset);
                ZoneRef::Stockpile(self.map.storage.add_stockpile(
                    crate::storage::StoragePriority::Normal,
                    &[],
                    filter,
                ))
            }
            ZoneKind::Growing => {
                let potato = self
                    .defs
                    .things
                    .id("Plant_Potato")
                    .expect("Plant_Potato is a Core ThingDef");
                ZoneRef::Growing(self.map.add_growing_zone(potato, &[]))
            }
        };
        self.name_new_zone(z, kind == ZoneKind::DumpingStockpile);
        self.add_zone_cell(z, first);
        z
    }

    /// `Zone.AddCell` (the home area grows around it).
    fn add_zone_cell(&mut self, z: ZoneRef, c: Cell) {
        match z {
            ZoneRef::Stockpile(id) => self.map.storage.add_cells(id, &[c]),
            ZoneRef::Growing(g) => self.map.add_growing_cells(g, &[c]),
        }
        self.mark_home_around_zone_cell(c);
    }

    fn remove_zone_cell(&mut self, z: ZoneRef, c: Cell) {
        match z {
            ZoneRef::Stockpile(_) => self.map.storage.remove_cell(c),
            ZoneRef::Growing(_) => self.map.remove_growing_cell(c),
        }
    }

    /// `Designator_ZoneAdd.DesignateMultiCell`: `cells` are the drag's
    /// accepted cells, `selected` the selected zone. A single cell already
    /// in a zone of this type selects it. Otherwise, with no selection, the
    /// one zone of this type the drag touches (if exactly one) is used;
    /// free cells join the zone when they touch it (cardinally), the
    /// others open new zones; the last zone keeps only its connected part.
    /// Returns the zone selected afterwards.
    pub fn zone_add(
        &mut self,
        kind: ZoneKind,
        selected: Option<ZoneRef>,
        cells: &[Cell],
    ) -> Option<ZoneRef> {
        let mut selected =
            selected.filter(|z| z.same_type(kind) && !self.zone_cells(*z).is_empty());
        let mut unset: Vec<Cell> = cells.to_vec();
        if let [c] = unset[..]
            && let Some(z) = self.zone_at(c)
        {
            return if z.same_type(kind) { Some(z) } else { selected };
        }
        if selected.is_none() {
            let mut found = None;
            for &c in cells {
                if let Some(z) = self.zone_at(c).filter(|z| z.same_type(kind)) {
                    match found {
                        None => found = Some(z),
                        Some(f) if f != z => {
                            found = None;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            selected = found;
        }
        unset.retain(|&c| self.zone_at(c).is_none());
        if unset.is_empty() {
            return selected;
        }
        let mut zone = match selected {
            Some(z) => z,
            None => {
                let first = unset.remove(0);
                self.open_zone(kind, first)
            }
        };
        loop {
            let count = unset.len();
            for k in (0..unset.len()).rev() {
                let c = unset[k];
                let touches = Cell::NEIGHBORS_4
                    .iter()
                    .any(|&d| self.zone_at(c + d) == Some(zone));
                if touches {
                    self.add_zone_cell(zone, c);
                    unset.remove(k);
                }
            }
            if unset.is_empty() {
                break;
            }
            if unset.len() == count {
                let first = unset.remove(0);
                zone = self.open_zone(kind, first);
            }
        }
        self.check_zone_contiguous(zone);
        self.refresh_path_grid();
        Some(zone)
    }

    /// `Designator_ZoneDelete.CanDesignateCell`: a zone is there.
    pub fn can_delete_zone_cell(&self, c: Cell) -> bool {
        self.zone_at(c).is_some()
    }

    /// `Designator_ZoneDelete`: the cells leave their zones; each zone
    /// touched keeps only its connected part.
    pub fn zone_delete_cells(&mut self, cells: &[Cell]) {
        let mut touched: Vec<ZoneRef> = Vec::new();
        for &c in cells {
            if let Some(z) = self.zone_at(c) {
                self.remove_zone_cell(z, c);
                if !touched.contains(&z) {
                    touched.push(z);
                }
            }
        }
        for z in touched {
            self.check_zone_contiguous(z);
        }
    }

    /// `Zone.Delete`: every cell goes, last first.
    pub fn delete_zone(&mut self, z: ZoneRef) {
        while let Some(&c) = self.zone_cells(z).last() {
            self.remove_zone_cell(z, c);
        }
    }

    /// `Zone.CheckContiguous`: cells not connected (cardinally) to the
    /// zone's first cell leave it.
    fn check_zone_contiguous(&mut self, z: ZoneRef) {
        let cells = self.zone_cells(z).to_vec();
        let Some(&start) = cells.first() else {
            return;
        };
        let mut found = vec![start];
        let mut k = 0;
        while k < found.len() {
            let c = found[k];
            k += 1;
            for &d in &Cell::NEIGHBORS_4 {
                let n = c + d;
                if cells.contains(&n) && !found.contains(&n) {
                    found.push(n);
                }
            }
        }
        if found.len() >= cells.len() {
            return;
        }
        let size = self.map.size();
        for z_ in 0..size.height {
            for x in 0..size.width {
                let c = Cell::new(x, z_);
                if cells.contains(&c) && !found.contains(&c) {
                    self.remove_zone_cell(z, c);
                }
            }
        }
    }
}

/// Why a cell can't take a zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneReject {
    /// Rejected without a message.
    Silent,
    /// `TooCloseToMapEdge`.
    TooCloseToMapEdge,
}
