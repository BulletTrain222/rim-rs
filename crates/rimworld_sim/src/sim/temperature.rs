//! Room temperatures (docs/research.md §31): each room's temperature
//! drifts toward the outdoors through its roof and walls every 120 ticks
//! (`RoomTempTracker.EqualizeTemperature`), doors mix the rooms around them
//! (`GenTemperature.EqualizeTemperaturesThroughBuilding`), and things take
//! the temperature of their cell's room.

use serde::{Deserialize, Serialize};

use super::Sim;
use crate::grid::Cell;
use crate::region::RegionType;

/// Rooms equalize when `TicksGame % 120 == 7` (`MapTemperatureTick`).
const EQUALIZE_INTERVAL: u64 = 120;
const EQUALIZE_OFFSET: u64 = 7;
/// `RoomTempTracker` rates.
const WALL_EQUALIZE_FACTOR: f32 = 0.00017;
const FRACTION_WALL_EQUALIZE_CELLS: f32 = 0.2;
const THIN_ROOF_EQUALIZE_RATE: f32 = 5e-5;
const NO_ROOF_EQUALIZE_RATE: f32 = 0.0007;
/// `BuildingProperties` door defaults.
const DOOR_EQUALIZE_INTERVAL_CLOSED: i32 = 375;
const DOOR_EQUALIZE_INTERVAL_OPEN: i32 = 34;
const DOOR_EQUALIZE_RATE: f32 = 1.0;

/// Per-room temperature state, by the current rooms' indices.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoomTemps {
    pub temps: Vec<f32>,
    /// `cycleIndex`: which wall-equalization cells were sampled last.
    cycle: Vec<usize>,
    /// Derived per room: rebuilt when rooms or roofs change (saved, since
    /// rebuilding shuffles with the random stream).
    data: Vec<RoomData>,
    roof_revision: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RoomData {
    cells: usize,
    doorway: bool,
    uses_outdoor: bool,
    no_roof: f32,
    thin_roof: f32,
    /// Cells two steps outside the walls (`equalizeCells`), shuffled.
    equalize: Vec<Cell>,
}

/// `TempDiffFromOutdoorsAdjusted`.
fn diff_adjusted(outdoor: f32, t: f32) -> f32 {
    let d = outdoor - t;
    if d.abs() < 100.0 {
        d
    } else {
        d.signum() * 100.0 + 5.0 * (d - d.signum() * 100.0)
    }
}

impl RoomTemps {
    pub(super) fn reset_cycles(&mut self, rooms: usize) {
        self.cycle = vec![0; rooms];
        self.data.clear();
    }
}

impl Sim {
    /// The temperature of the room `c` is in, or the outdoor temperature
    /// (`GenTemperature.GetTemperatureForCell`).
    // COMPATIBILITY TODO: currently approximate — a cell inside an
    // impassable building takes the outdoor temperature, not the average of
    // the rooms around it.
    pub fn cell_temperature(&self, c: Cell) -> f32 {
        self.regions
            .room_at(c)
            .and_then(|r| self.room_temps.temps.get(r).copied())
            .unwrap_or(self.outdoor_temperature)
    }

    /// Debug tool: sets a door's `thingIDNumber` (its equalization ticks).
    pub fn debug_set_door_id_number(&mut self, cell: Cell, id_number: i32) {
        if let Some(d) = self.map.door_at_mut(cell) {
            d.id_number = id_number;
        }
    }

    /// The cells whose rooms pull a room's temperature through its walls,
    /// sorted (for comparing with the game's set).
    pub fn wall_equalize_cells(&mut self, c: Cell) -> Vec<Cell> {
        if self.room_temps.data.len() != self.room_temps.temps.len()
            || self.room_temps.roof_revision != Some(self.map.roof_revision)
        {
            self.regenerate_room_data();
        }
        let Some(r) = self.regions.room_at(c) else {
            return Vec::new();
        };
        let mut v = self.room_temps.data[r].equalize.clone();
        v.sort_by_key(|c| (c.x, c.z));
        v
    }

    /// `GenTemperature.PushHeat`: raises the temperature of the room at `c`
    /// by energy / cells; a cell without a room shares it among the rooms
    /// of its eight neighbours. Rooms using the outdoor temperature take
    /// nothing. Returns whether any room took heat.
    pub fn push_heat(&mut self, c: Cell, energy: f32) -> bool {
        if self.room_temps.data.len() != self.room_temps.temps.len()
            || self.room_temps.roof_revision != Some(self.map.roof_revision)
        {
            self.regenerate_room_data();
        }
        let rooms: Vec<usize> = match self.regions.room_at(c) {
            Some(r) => vec![r],
            // Listed per neighbour: a room next to several takes more.
            None => Cell::NEIGHBORS_8
                .iter()
                .map(|&d| Cell::new(c.x + d.x, c.z + d.z))
                .filter(|&n| self.map.size().contains(n))
                .filter_map(|n| self.regions.room_at(n))
                .collect(),
        };
        if rooms.is_empty() {
            return false;
        }
        let share = energy / rooms.len() as f32;
        for r in rooms {
            let d = &self.room_temps.data[r];
            if d.uses_outdoor {
                continue;
            }
            let t = &mut self.room_temps.temps[r];
            *t = (*t + share / d.cells as f32).clamp(-273.15, 1000.0);
        }
        true
    }

    /// The room `c` is in, if any.
    pub fn room_at(&self, c: Cell) -> Option<usize> {
        self.regions.room_at(c)
    }

    /// `Room.UsesOutdoorTemperature` (touching the map edge or a quarter
    /// open to the sky).
    pub(super) fn room_uses_outdoor(&mut self, room: usize) -> bool {
        if self.room_temps.data.len() != self.room_temps.temps.len()
            || self.room_temps.roof_revision != Some(self.map.roof_revision)
        {
            self.regenerate_room_data();
        }
        self.room_temps
            .data
            .get(room)
            .is_none_or(|d| d.uses_outdoor)
    }

    /// A room's cell count (`Room.CellCount`).
    pub(super) fn room_cell_count(&mut self, room: usize) -> usize {
        self.room_uses_outdoor(room);
        self.room_temps.data.get(room).map_or(1, |d| d.cells)
    }

    /// Debug tool: sets the temperature of the room containing `c`.
    pub fn debug_set_cell_room_temperature(&mut self, c: Cell, t: f32) {
        if let Some(r) = self.regions.room_at(c) {
            self.debug_set_room_temperature(r, t);
        }
    }

    /// Debug tool: sets one room's temperature.
    pub fn debug_set_room_temperature(&mut self, room: usize, t: f32) {
        if let Some(v) = self.room_temps.temps.get_mut(room) {
            *v = t;
        }
    }

    /// Debug tool: sets every room's temperature (a replay starting from
    /// the game's recorded state).
    pub fn debug_set_room_temperatures(&mut self, t: f32) {
        self.room_temps.temps.fill(t);
    }

    /// After the rooms were rebuilt: each new room starts at the average
    /// temperature of the old rooms its cells were in (outdoors if none).
    // COMPATIBILITY TODO: currently approximate — the game carries
    // temperatures over through reused districts and rooms.
    pub(super) fn remap_room_temps(&mut self, old: &crate::region::Regions) {
        let rooms = self.regions.rooms();
        let mut temps = Vec::with_capacity(rooms.len());
        for room in &rooms {
            let (mut sum, mut n) = (0.0f32, 0u32);
            for &r in room {
                for c in self.regions.cells(r) {
                    if let Some(t) = old
                        .room_at(c)
                        .and_then(|o| self.room_temps.temps.get(o).copied())
                    {
                        sum += t;
                        n += 1;
                    }
                }
            }
            temps.push(if n > 0 {
                sum / n as f32
            } else {
                self.outdoor_temperature
            });
        }
        self.room_temps.temps = temps;
        self.room_temps.cycle = vec![0; rooms.len()];
        self.room_temps.data.clear();
    }

    /// `RoomTempTracker.RegenerateEqualizationData` for every room.
    fn regenerate_room_data(&mut self) {
        let rooms = self.regions.rooms();
        let size = self.map.size();
        let edge =
            |c: Cell| c.x == 0 || c.z == 0 || c.x == size.width - 1 || c.z == size.height - 1;
        let mut data = Vec::with_capacity(rooms.len());
        for (id, room) in rooms.iter().enumerate() {
            let cells: Vec<Cell> = room.iter().flat_map(|&r| self.regions.cells(r)).collect();
            let doorway = room
                .iter()
                .any(|&r| self.regions.region(r).kind == RegionType::Portal);
            let open = cells.iter().filter(|&&c| !self.map.roofed(c)).count();
            let uses_outdoor = cells.iter().any(|&c| edge(c))
                || open >= (cells.len() as f32 * 0.25).ceil() as usize;
            let mut d = RoomData {
                cells: cells.len().max(1),
                doorway,
                uses_outdoor,
                ..Default::default()
            };
            if !uses_outdoor && !cells.is_empty() {
                d.no_roof = open as f32 / cells.len() as f32;
                d.thin_roof = 1.0 - d.no_roof;
                d.equalize = self.equalize_cells(id, &cells);
                self.rng.shuffle(&mut d.equalize);
            }
            data.push(d);
        }
        self.room_temps.data = data;
        self.room_temps.roof_revision = Some(self.map.roof_revision);
    }

    /// `RegenerateEqualizeCells`: for each cell of the room and each
    /// cardinal direction, the cell two steps away when it is in another
    /// room and touches none of this room's cells, unless the cell between
    /// is a door joining only this room.
    fn equalize_cells(&self, room: usize, cells: &[Cell]) -> Vec<Cell> {
        const CARDINALS: [Cell; 4] = [
            Cell::new(0, 1),
            Cell::new(1, 0),
            Cell::new(0, -1),
            Cell::new(-1, 0),
        ];
        let size = self.map.size();
        let in_room = |c: Cell| self.regions.room_at(c) == Some(room);
        let mut out = Vec::new();
        for &c in cells {
            for d in CARDINALS {
                let next = c + d;
                let two = c + Cell::new(d.x * 2, d.z * 2);
                if size.contains(next)
                    && let Some(r) = self.regions.region_at(next)
                {
                    if self.regions.region(r).kind != RegionType::Portal {
                        continue;
                    }
                    // A door leading somewhere else exchanges air itself.
                    let leads_out = self.regions.neighbors(r).any(|n| {
                        self.regions.room_of_region(n) != Some(room)
                            && self.regions.region(n).kind != RegionType::Portal
                    });
                    if leads_out {
                        continue;
                    }
                }
                if !size.contains(two) || in_room(two) {
                    continue;
                }
                if CARDINALS.iter().any(|&k| in_room(two + k)) {
                    continue;
                }
                out.push(two);
            }
        }
        out
    }

    /// `MapTemperatureTick`: every 120 ticks each room equalizes.
    /// Runs at the start of a tick, before the tick counter the game
    /// checks has advanced: at our tick `T` when `(T - 1) % 120 == 7`.
    pub(super) fn tick_room_temperatures(&mut self) {
        if self.tick.wrapping_sub(1) % EQUALIZE_INTERVAL != EQUALIZE_OFFSET {
            return;
        }
        if self.room_temps.data.len() != self.room_temps.temps.len()
            || self.room_temps.roof_revision != Some(self.map.roof_revision)
        {
            self.regenerate_room_data();
        }
        let outdoor = self.outdoor_temperature;
        for room in 0..self.room_temps.temps.len() {
            let d = &self.room_temps.data[room];
            let t = self.room_temps.temps[room];
            if d.uses_outdoor {
                self.room_temps.temps[room] = outdoor;
                continue;
            }
            if d.doorway {
                // A door room only follows its door's equalization.
                continue;
            }
            let thin = if d.thin_roof < 0.001 {
                0.0
            } else {
                diff_adjusted(outdoor, t) * d.thin_roof * THIN_ROOF_EQUALIZE_RATE * 120.0
            };
            let open = if d.no_roof < 0.001 {
                0.0
            } else {
                diff_adjusted(outdoor, t) * d.no_roof * NO_ROOF_EQUALIZE_RATE * 120.0
            };
            let wall = self.wall_equalization(room, t, outdoor);
            self.room_temps.temps[room] = (t + thin + open + wall).clamp(-273.15, 1000.0);
        }
    }

    /// `WallEqualizationTempChangePerInterval`: a fifth of the equalize
    /// cells (cycling) pull the room toward their own rooms' temperatures,
    /// or halfway to the outdoors where they have no room.
    fn wall_equalization(&mut self, room: usize, t: f32, outdoor: f32) -> f32 {
        let count = self.room_temps.data[room].equalize.len();
        if count == 0 {
            return 0.0;
        }
        let n = (count as f32 * FRACTION_WALL_EQUALIZE_CELLS).ceil() as usize;
        let mut sum = 0.0;
        for _ in 0..n {
            self.room_temps.cycle[room] += 1;
            let c = self.room_temps.data[room].equalize[self.room_temps.cycle[room] % count];
            sum += match self.regions.room_at(c) {
                Some(r) => self.room_temps.temps[r] - t,
                None => (t + (outdoor - t) * 0.5) - t,
            };
        }
        sum / n as f32 * count as f32 * 120.0 * WALL_EQUALIZE_FACTOR
            / self.room_temps.data[room].cells as f32
    }

    /// Doors mix the air of the rooms on their four sides: every 375 ticks
    /// when closed (hash-staggered), every 34 when open.
    pub(super) fn tick_door_temperatures(&mut self) {
        let t = self.tick;
        let doors: Vec<(Cell, bool, i32)> = self
            .map
            .doors()
            .iter()
            .map(|d| (d.cell, d.open, d.id_number))
            .collect();
        for (cell, open, id) in doors {
            let due = if open {
                (t as i64 + crate::hash::hash_offset(id) as i64)
                    .rem_euclid(DOOR_EQUALIZE_INTERVAL_OPEN as i64)
                    == 0
            } else {
                crate::hash::is_hash_interval_tick(t, id, DOOR_EQUALIZE_INTERVAL_CLOSED as i64)
            };
            if due {
                self.equalize_through(cell);
            }
        }
    }

    /// `EqualizeTemperaturesThroughBuilding` for a one-cell building: the
    /// door's room takes the average of the rooms around it, and those
    /// rooms (not outdoors) move toward it, scaled so none overshoots.
    fn equalize_through(&mut self, cell: Cell) {
        if self.room_temps.data.len() != self.room_temps.temps.len() {
            self.regenerate_room_data();
        }
        const CARDINALS: [Cell; 4] = [
            Cell::new(0, 1),
            Cell::new(1, 0),
            Cell::new(0, -1),
            Cell::new(-1, 0),
        ];
        let mut rooms: Vec<usize> = Vec::new();
        for d in CARDINALS {
            if let Some(r) = self.regions.room_at(cell + d)
                && !rooms.contains(&r)
            {
                rooms.push(r);
            }
        }
        if rooms.is_empty() {
            return;
        }
        let avg = rooms.iter().map(|&r| self.room_temps.temps[r]).sum::<f32>() / rooms.len() as f32;
        if let Some(own) = self.regions.room_at(cell) {
            self.room_temps.temps[own] = avg;
        }
        if rooms.len() == 1 {
            return;
        }
        let mut scale = 1.0f32;
        for &r in &rooms {
            let d = &self.room_temps.data[r];
            if d.uses_outdoor {
                continue;
            }
            let t = self.room_temps.temps[r];
            let change = (avg - t) * DOOR_EQUALIZE_RATE;
            let mut next = t + change / d.cells as f32;
            if (change > 0.0 && next > avg) || (change < 0.0 && next < avg) {
                next = avg;
            }
            let s = ((next - t) * d.cells as f32 / change).abs();
            if s < scale {
                scale = s;
            }
        }
        for &r in &rooms {
            let d = &self.room_temps.data[r];
            if d.uses_outdoor {
                continue;
            }
            let t = self.room_temps.temps[r];
            self.room_temps.temps[r] = t + (avg - t) * DOOR_EQUALIZE_RATE * scale / d.cells as f32;
        }
    }
}
