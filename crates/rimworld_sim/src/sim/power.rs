//! Electricity (docs/research.md §55): power nets of transmitters
//! (`PowerNet`, `PowerNetMaker`), connectors wired to a transmitter within
//! six cells (`PowerConnectionMaker`), the net's energy balance each tick
//! with batteries and the random start-up and shutdown of parts
//! (`PowerNet.PowerNetTick`), plants, batteries and heaters/coolers.

use std::collections::BTreeMap;

use super::{JobEvent, Sim, tick_movement};
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::job::{FixStage, FlickStage, Job, JobKind};
use crate::map::ItemId;
use crate::map::Map;
use crate::path::PathGrid;
use crate::pawn::Pawn;

/// `CompPower.WattsToWattDaysPerTick`.
pub const WATTS_TO_WATT_DAYS_PER_TICK: f32 = 1.666_666_7e-5;
/// `PowerConnectionMaker.ConnectMaxDist`.
const CONNECT_MAX_DIST: i32 = 6;
/// `PowerNet.MinStoredEnergyToTurnOn`.
const MIN_STORED_ENERGY_TO_TURN_ON: f32 = 5.0;
/// `CompPowerBattery.SelfDischargingWatts`.
const SELF_DISCHARGE_WATTS: f32 = 5.0;
/// `Building_Heater`/`Building_Cooler` run on the rare tick.
const RARE_TICK: u64 = 250;
/// Heat per second over a rare tick (250 / 60).
const RARE_SECONDS: f32 = 4.166_666_5;

/// A connected set of transmitters (`PowerNet`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PowerNet {
    /// Transmitters in the order the net was found (`ContiguousPowerBuildings`).
    pub transmitters: Vec<ItemId>,
    /// `powerComps`: the traders, in registration order.
    pub traders: Vec<ItemId>,
    /// `batteryComps`.
    pub batteries: Vec<ItemId>,
    /// The cells the net claims in the power net grid.
    pub cells: Vec<Cell>,
}

/// `PowerNetManager.DelayedAction`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PowerAction {
    RegisterTransmitter(ItemId, Footprint),
    DeregisterTransmitter(ItemId, Footprint),
    RegisterConnector(ItemId),
    DeregisterConnector(ItemId),
}

/// The map's power nets (`PowerNetManager`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PowerGrid {
    pub nets: Vec<PowerNet>,
    pending: Vec<PowerAction>,
    /// `connectChildren` of each transmitter, in connection order.
    children: BTreeMap<ItemId, Vec<ItemId>>,
}

impl PowerGrid {
    fn net_at(&self, c: Cell) -> Option<usize> {
        self.nets.iter().position(|n| n.cells.contains(&c))
    }
}

impl Sim {
    /// `CompPower.PostSpawnSetup` and friends for a new building.
    pub(super) fn power_spawned(&mut self, id: ItemId) {
        let Some(s) = self.map.structure(id) else {
            return;
        };
        let def = &self.defs.things[s.def];
        let fp = s.footprint;
        if let Some(t) = def.temp_control
            && let Some(s) = self.map.structure_mut(id)
        {
            s.power.target_temperature = t.default_target_temperature;
        }
        let Some(p) = def.power.clone() else {
            return;
        };
        let connect = def.connect_to_power();
        if p.transmits_power {
            self.power
                .pending
                .push(PowerAction::RegisterTransmitter(id, fp));
        }
        if connect {
            self.power.pending.push(PowerAction::RegisterConnector(id));
        }
        self.set_up_power_vars(id);
        // `CompPowerPlant.PostSpawnSetup`: a plant that wants to be on is.
        if p.is_plant()
            && p.base_power_consumption < 0.0
            && let Some(s) = self.map.structure_mut(id)
            && s.power.switch_on
        {
            s.power.on = true;
        }
    }

    /// `CompPower.PostDeSpawn` for a building about to go.
    pub(super) fn power_despawned(&mut self, id: ItemId) {
        let Some(s) = self.map.structure(id) else {
            return;
        };
        let def = &self.defs.things[s.def];
        let fp = s.footprint;
        let Some(p) = def.power.as_ref() else {
            return;
        };
        if p.transmits_power {
            // `LostConnectParent` for each child.
            for child in self.power.children.remove(&id).unwrap_or_default() {
                if let Some(c) = self.map.structure_mut(child) {
                    c.power.connect_parent = None;
                    c.power.on = false;
                }
                self.power
                    .pending
                    .push(PowerAction::RegisterConnector(child));
            }
            self.power
                .pending
                .push(PowerAction::DeregisterTransmitter(id, fp));
        }
        if def.connect_to_power() {
            self.power
                .pending
                .push(PowerAction::DeregisterConnector(id));
        }
    }

    /// `SetUpPowerVars` for a trader: its output (an idle draw while off).
    fn set_up_power_vars(&mut self, id: ItemId) {
        let Some(s) = self.map.structure(id) else {
            return;
        };
        let Some(p) = self.defs.things[s.def].power.clone() else {
            return;
        };
        if !p.is_trader() {
            return;
        }
        let s = self.map.structure_mut(id).expect("checked");
        s.power.output = if !s.power.on && p.idle_power_draw != -1.0 {
            -p.idle_power_draw
        } else {
            -p.base_power_consumption
        };
    }

    /// Whether the building carries power now (`TransmitsPowerNow`: a
    /// switch only while on).
    fn transmits_now(&self, s: &crate::map::Structure) -> bool {
        let def = &self.defs.things[s.def];
        def.power.as_ref().is_some_and(|p| p.transmits_power)
            && (def.thing_class.as_deref() != Some("Building_PowerSwitch")
                || (s.power.switch_on && !s.power.broken_down))
    }

    /// `GetTransmitter`: the first transmitting building on the cell.
    fn transmitter_at(&self, c: Cell) -> Option<ItemId> {
        self.map
            .structures()
            .iter()
            .find(|s| s.footprint.contains(c) && self.transmits_now(s))
            .map(|s| s.id)
    }

    /// `UpdatePowerNetsAndConnections_First`: the delayed actions, in the
    /// game's three passes.
    pub(super) fn update_power_nets(&mut self) {
        if self.power.pending.is_empty() {
            return;
        }
        let actions = std::mem::take(&mut self.power.pending);
        for a in &actions {
            match *a {
                PowerAction::RegisterTransmitter(id, fp) => {
                    if self.map.structure(id).map(|s| s.footprint) != Some(fp) {
                        continue;
                    }
                    self.set_up_power_vars(id);
                    for c in fp.adjacent_cardinal() {
                        self.try_destroy_net_at(c);
                    }
                }
                PowerAction::DeregisterTransmitter(id, fp) => {
                    self.try_destroy_net_at(fp.center);
                    for child in self.power.children.remove(&id).unwrap_or_default() {
                        if let Some(c) = self.map.structure_mut(child) {
                            c.power.connect_parent = None;
                            c.power.on = false;
                        }
                        self.power
                            .pending
                            .push(PowerAction::RegisterConnector(child));
                    }
                }
                _ => {}
            }
        }
        for a in &actions {
            let fp = match *a {
                PowerAction::RegisterTransmitter(id, fp)
                    if self.map.structure(id).map(|s| s.footprint) == Some(fp) =>
                {
                    fp
                }
                PowerAction::DeregisterTransmitter(_, fp) => fp,
                _ => continue,
            };
            self.try_create_net_at(fp.center);
            for c in fp.adjacent_cardinal() {
                self.try_create_net_at(c);
            }
        }
        for a in &actions {
            match *a {
                PowerAction::RegisterConnector(id) => {
                    if self.map.structure(id).is_some() {
                        self.set_up_power_vars(id);
                        self.try_connect_to_any_net(id);
                    }
                }
                PowerAction::DeregisterConnector(id) => self.disconnect(id),
                _ => {}
            }
        }
        self.light_key = None;
    }

    fn try_destroy_net_at(&mut self, c: Cell) {
        if let Some(n) = self.power.net_at(c) {
            self.power.nets.remove(n);
        }
    }

    /// `TryCreateNetAt`: a new net from the transmitter on `c` (unless the
    /// cell already has one), whose transmitters then wire up the free
    /// connectors around them.
    fn try_create_net_at(&mut self, c: Cell) {
        if !self.map.size().contains(c) || self.power.net_at(c).is_some() {
            return;
        }
        let Some(root) = self.transmitter_at(c) else {
            return;
        };
        let transmitters = self.contiguous_transmitters(root);
        let mut net = PowerNet {
            transmitters: Vec::new(),
            traders: Vec::new(),
            batteries: Vec::new(),
            cells: Vec::new(),
        };
        for &t in &transmitters {
            net.transmitters.push(t);
            if let Some(s) = self.map.structure(t) {
                net.cells.extend(s.footprint.cells());
            }
            self.register_comps(&mut net, t);
            for &child in self
                .power
                .children
                .get(&t)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
            {
                self.register_comps(&mut net, child);
            }
        }
        self.power.nets.push(net);
        let n = self.power.nets.len() - 1;
        for t in transmitters {
            self.connect_all_connectors_to(t, n);
        }
    }

    fn register_comps(&self, net: &mut PowerNet, id: ItemId) {
        let Some(p) = self
            .map
            .structure(id)
            .and_then(|s| self.defs.things[s.def].power.as_ref())
        else {
            return;
        };
        if p.is_trader() {
            net.traders.push(id);
        }
        if p.is_battery() {
            net.batteries.push(id);
        }
    }

    /// `ContiguousPowerBuildings`: breadth first over transmitters sharing
    /// an edge, layer by layer in discovery order; per neighbouring cell only
    /// the first transmitter there joins.
    fn contiguous_transmitters(&self, root: ItemId) -> Vec<ItemId> {
        let mut closed: Vec<ItemId> = Vec::new();
        let mut open = vec![root];
        while !open.is_empty() {
            closed.extend(open.iter().copied());
            let current = std::mem::take(&mut open);
            for b in current {
                let Some(fp) = self.map.structure(b).map(|s| s.footprint) else {
                    continue;
                };
                for c in fp.adjacent_cardinal() {
                    if !self.map.size().contains(c) {
                        continue;
                    }
                    let found = self.map.structures().iter().find(|s| {
                        s.footprint.contains(c)
                            && self.transmits_now(s)
                            && !open.contains(&s.id)
                            && !closed.contains(&s.id)
                    });
                    if let Some(s) = found {
                        open.push(s.id);
                    }
                }
            }
        }
        closed
    }

    /// `ConnectAllConnectorsToTransmitter`: free connectors within six
    /// cells of the transmitter, scanned row by row.
    fn connect_all_connectors_to(&mut self, t: ItemId, net: usize) {
        let Some(r) = self.map.structure(t).map(|s| s.footprint.rect()) else {
            return;
        };
        let size = self.map.size();
        let (x0, x1) = (
            (r.min_x - CONNECT_MAX_DIST).max(0),
            (r.max_x + CONNECT_MAX_DIST).min(size.width - 1),
        );
        let (z0, z1) = (
            (r.min_z - CONNECT_MAX_DIST).max(0),
            (r.max_z + CONNECT_MAX_DIST).min(size.height - 1),
        );
        for z in z0..=z1 {
            for x in x0..=x1 {
                let c = Cell::new(x, z);
                let free: Vec<ItemId> = self
                    .map
                    .structures()
                    .iter()
                    .filter(|s| {
                        s.footprint.contains(c)
                            && self.defs.things[s.def].connect_to_power()
                            && s.power.connect_parent.is_none()
                    })
                    .map(|s| s.id)
                    .collect();
                for id in free {
                    self.connect(id, t, Some(net));
                }
            }
        }
    }

    fn connect(&mut self, id: ItemId, t: ItemId, net: Option<usize>) {
        if let Some(s) = self.map.structure_mut(id) {
            s.power.connect_parent = Some(t);
        }
        self.power.children.entry(t).or_default().push(id);
        if let Some(n) = net {
            let mut net = std::mem::replace(
                &mut self.power.nets[n],
                PowerNet {
                    transmitters: Vec::new(),
                    traders: Vec::new(),
                    batteries: Vec::new(),
                    cells: Vec::new(),
                },
            );
            self.register_comps(&mut net, id);
            self.power.nets[n] = net;
        }
    }

    /// The net transmitter `t` belongs to.
    fn net_of_transmitter(&self, t: ItemId) -> Option<usize> {
        self.power
            .nets
            .iter()
            .position(|n| n.transmitters.contains(&t))
    }

    /// `TryConnectToAnyPowerNet`: the transmitter nearest the connector
    /// (squared distance between positions, first found on ties, scanning
    /// rows) within six cells.
    fn try_connect_to_any_net(&mut self, id: ItemId) {
        let Some(s) = self.map.structure(id) else {
            return;
        };
        if s.power.connect_parent.is_some() {
            return;
        }
        let at = s.footprint.center;
        let size = self.map.size();
        let mut best: Option<(i32, ItemId)> = None;
        for z in (at.z - CONNECT_MAX_DIST).max(0)..=(at.z + CONNECT_MAX_DIST).min(size.height - 1) {
            for x in
                (at.x - CONNECT_MAX_DIST).max(0)..=(at.x + CONNECT_MAX_DIST).min(size.width - 1)
            {
                let Some(t) = self.transmitter_at(Cell::new(x, z)) else {
                    continue;
                };
                let tp = self.map.structure(t).expect("found").footprint.center;
                let d = (tp.x - at.x).pow(2) + (tp.z - at.z).pow(2);
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, t));
                }
            }
        }
        if let Some((_, t)) = best {
            let net = self.net_of_transmitter(t);
            self.connect(id, t, net);
        }
    }

    /// `DisconnectFromPowerNet`.
    fn disconnect(&mut self, id: ItemId) {
        let parent = self.map.structure(id).and_then(|s| s.power.connect_parent);
        if let Some(t) = parent {
            if let Some(n) = self.net_of_transmitter(t) {
                let net = &mut self.power.nets[n];
                net.traders.retain(|&x| x != id);
                net.batteries.retain(|&x| x != id);
            }
            if let Some(ch) = self.power.children.get_mut(&t) {
                ch.retain(|&x| x != id);
            }
        }
        // Gone or not, the record is cleared (the structure may be gone).
        for net in &mut self.power.nets {
            net.traders.retain(|&x| x != id);
            net.batteries.retain(|&x| x != id);
        }
        for ch in self.power.children.values_mut() {
            ch.retain(|&x| x != id);
        }
        if let Some(s) = self.map.structure_mut(id) {
            s.power.connect_parent = None;
            s.power.on = false;
        }
    }

    /// The things' per-tick power work (`CompTick`): plants update their
    /// output, batteries lose 5 W.
    // COMPATIBILITY TODO: currently approximate — breakdowns, EMP, wind
    // turbines and power upgrades are not modelled.
    pub(super) fn tick_power_comps(&mut self) {
        let glow = self.map.sky_glow;
        let defs = self.defs.clone();
        let roofs: Vec<(ItemId, f32)> = self
            .map
            .structures()
            .iter()
            .filter(|s| {
                defs.things[s.def]
                    .power
                    .as_ref()
                    .is_some_and(|p| p.comp_class == "CompPowerPlantSolar")
            })
            .map(|s| {
                let n = s.footprint.cells().count() as f32;
                let roofed = s.footprint.cells().filter(|&c| self.map.roofed(c)).count() as f32;
                (s.id, (n - roofed) / n)
            })
            .collect();
        for s in self.map.structures_mut() {
            let def = &defs.things[s.def];
            let Some(p) = def.power.as_ref() else {
                continue;
            };
            if p.is_plant() {
                let has_fuel = def.refuelable.is_none() || s.fuel > 0.0;
                s.power.output =
                    if s.power.broken_down || !has_fuel || !s.power.switch_on || !s.power.on {
                        0.0
                    } else if p.comp_class == "CompPowerPlantSolar" {
                        let roof = roofs.iter().find(|r| r.0 == s.id).map_or(1.0, |r| r.1);
                        -p.base_power_consumption * glow.clamp(0.0, 1.0) * roof
                    } else {
                        -p.base_power_consumption
                    };
            }
            if p.is_battery() {
                s.power.stored -=
                    (SELF_DISCHARGE_WATTS * WATTS_TO_WATT_DAYS_PER_TICK).min(s.power.stored);
            }
        }
    }

    /// `PowerNetManager.PowerNetsTick`.
    pub(super) fn tick_power_nets(&mut self) {
        let before: Vec<bool> = self.lit_power_states();
        for n in 0..self.power.nets.len() {
            self.power_net_tick(n);
        }
        if self.lit_power_states() != before {
            self.light_key = None;
        }
        self.update_door_power();
    }

    /// Powered doors (`DoorPowerOn`): `TicksToOpenNow` = 45 ÷ DoorOpenSpeed
    /// × (0.25 × `poweredDoorOpenSpeedFactor` with power, else
    /// `unpoweredDoorOpenSpeedFactor`); `CloseDelayAdjusted` = ⌊110 × the
    /// matching close factor⌋.
    fn update_door_power(&mut self) {
        let defs = self.defs.clone();
        let doors: Vec<(crate::grid::Cell, bool, f32, f32)> = self
            .map
            .structures()
            .iter()
            .filter(|s| defs.things[s.def].is_door() && defs.things[s.def].power.is_some())
            .map(|s| {
                let def = &defs.things[s.def];
                let stuff = s.stuff.map(|st| &defs.things[st]);
                let speed = crate::stats::def_stat(&defs, def, stuff, "DoorOpenSpeed");
                let on = s.power.on && !s.power.broken_down;
                let b = def.building.as_ref();
                let (open_f, close_f) = if on {
                    (
                        0.25 * b.map_or(1.0, |b| b.powered_door_open_speed_factor),
                        b.map_or(1.0, |b| b.powered_door_close_speed_factor),
                    )
                } else {
                    (
                        b.map_or(1.0, |b| b.unpowered_door_open_speed_factor),
                        b.map_or(1.0, |b| b.unpowered_door_close_speed_factor),
                    )
                };
                (s.footprint.center, on, 45.0 / speed * open_f, close_f)
            })
            .collect();
        for (cell, on, open, close) in doors {
            if let Some(d) = self.map.door_at_mut(cell)
                && d.powered != on
            {
                d.powered = on;
                d.ticks_to_open = open.round_ties_even() as i32;
                d.close_delay = (crate::map::DOOR_CLOSE_DELAY_TICKS as f32 * close).floor() as i32;
            }
        }
    }

    fn lit_power_states(&self) -> Vec<bool> {
        self.map
            .structures()
            .iter()
            .filter(|s| {
                self.defs.things[s.def].glower.is_some() && self.defs.things[s.def].power.is_some()
            })
            .map(|s| s.power.on)
            .collect()
    }

    fn energy_per_tick(&self, id: ItemId) -> f32 {
        self.map
            .structure(id)
            .map_or(0.0, |s| s.power.output * WATTS_TO_WATT_DAYS_PER_TICK)
    }

    /// `PowerNet.PowerNetTick`: with energy to spare, parts that want power
    /// start up a few at a time (every 200 / n ticks, 5% of them at random,
    /// keeping a 5 W·day battery reserve); the balance goes into or comes out
    /// of the batteries. Short of energy, a random 5% of the drawing parts
    /// shut down every 20 ticks.
    fn power_net_tick(&mut self, n: usize) {
        let net = self.power.nets[n].clone();
        let mut gain: f32 = net
            .traders
            .iter()
            .filter(|&&id| self.map.structure(id).is_some_and(|s| s.power.on))
            .map(|&id| self.energy_per_tick(id))
            .sum();
        let stored: f32 = net
            .batteries
            .iter()
            .filter_map(|&id| self.map.structure(id))
            .map(|s| s.power.stored)
            .sum();
        let tick = self.tick;
        if stored + gain >= -1e-7 {
            let reserve = if !net.batteries.is_empty() && stored >= 0.1 {
                stored - MIN_STORED_ENERGY_TO_TURN_ON
            } else {
                stored
            };
            if reserve + gain >= 0.0 {
                let wanting: Vec<ItemId> = net
                    .traders
                    .iter()
                    .copied()
                    .filter(|&id| {
                        self.map.structure(id).is_some_and(|s| {
                            !s.power.on && s.power.switch_on && !s.power.broken_down
                        })
                    })
                    .collect();
                if !wanting.is_empty() {
                    let interval = (200 / wanting.len() as u64).max(30);
                    if tick.is_multiple_of(interval) {
                        let k = ((wanting.len() as f32 * 0.05).round_ties_even() as usize).max(1);
                        for _ in 0..k {
                            let pick = wanting[self.rng.range(0, wanting.len() as i32) as usize];
                            let e = self.energy_per_tick(pick);
                            let s = self.map.structure_mut(pick).expect("listed");
                            if !s.power.on && gain + stored >= -(e + 1e-7) {
                                s.power.on = true;
                                gain += e;
                            }
                        }
                    }
                }
            }
            self.change_stored_energy(&net.batteries, gain);
        } else {
            if !tick.is_multiple_of(20) {
                return;
            }
            let drawing: Vec<ItemId> = net
                .traders
                .iter()
                .copied()
                .filter(|&id| {
                    self.map.structure(id).is_some_and(|s| s.power.on)
                        && self.energy_per_tick(id) < 0.0
                })
                .collect();
            if !drawing.is_empty() {
                let k = ((drawing.len() as f32 * 0.05).round_ties_even() as usize).max(1);
                for _ in 0..k {
                    let pick = drawing[self.rng.range(0, drawing.len() as i32) as usize];
                    if let Some(s) = self.map.structure_mut(pick) {
                        s.power.on = false;
                    }
                }
            }
        }
    }

    /// `ChangeStoredEnergy`: a surplus is shared among the batteries
    /// (shuffled, each at most what it can take, `efficiency` lost); a
    /// deficit is drawn evenly from those with energy.
    fn change_stored_energy(&mut self, batteries: &[ItemId], extra: f32) {
        let defs = self.defs.clone();
        let props = |sim: &Sim, id: ItemId| -> Option<(f32, f32)> {
            sim.map
                .structure(id)
                .and_then(|s| defs.things[s.def].power.as_ref())
                .and_then(|p| p.battery)
        };
        if extra > 0.0 {
            if batteries.is_empty() {
                return;
            }
            let mut energy = extra;
            let mut list: Vec<ItemId> = batteries.to_vec();
            self.rng.shuffle(&mut list);
            let can_accept = |sim: &Sim, id: ItemId| -> f32 {
                let (max, eff) = props(sim, id).unwrap_or((0.0, 1.0));
                match sim.map.structure(id) {
                    Some(s) if !s.power.broken_down => (max - s.power.stored) / eff,
                    _ => 0.0,
                }
            };
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > 10_000 {
                    break;
                }
                let least = list
                    .iter()
                    .map(|&id| can_accept(self, id))
                    .fold(f32::MAX, f32::min);
                if energy >= least * list.len() as f32 {
                    for k in (0..list.len()).rev() {
                        let id = list[k];
                        let accept = can_accept(self, id);
                        let full = accept <= 0.0 || accept == least;
                        if least > 0.0 {
                            self.add_battery_energy(id, least);
                            energy -= least;
                        }
                        if full {
                            list.remove(k);
                        }
                    }
                } else {
                    let each = energy / list.len() as f32;
                    for &id in &list {
                        self.add_battery_energy(id, each);
                    }
                    break;
                }
                if energy < 0.0005 || list.is_empty() {
                    break;
                }
            }
        } else {
            let mut need = -extra;
            let giving: Vec<ItemId> = batteries
                .iter()
                .copied()
                .filter(|&id| {
                    self.map
                        .structure(id)
                        .is_some_and(|s| s.power.stored > 1e-7)
                })
                .collect();
            if giving.is_empty() {
                return;
            }
            let each = need / giving.len() as f32;
            for _ in 0..=10 {
                for &id in &giving {
                    let s = self.map.structure_mut(id).expect("listed");
                    let take = each.min(s.power.stored);
                    s.power.stored = (s.power.stored - take).max(0.0);
                    need -= take;
                    if need < 1e-7 {
                        return;
                    }
                }
            }
        }
    }

    /// `CompPowerBattery.AddEnergy`: at most what fits, times efficiency.
    fn add_battery_energy(&mut self, id: ItemId, amount: f32) {
        let defs = self.defs.clone();
        let Some(s) = self.map.structure_mut(id) else {
            return;
        };
        let Some((max, eff)) = defs.things[s.def].power.as_ref().and_then(|p| p.battery) else {
            return;
        };
        if s.power.broken_down {
            return;
        }
        let accept = (max - s.power.stored) / eff;
        s.power.stored += amount.min(accept) * eff;
    }

    /// Heaters and coolers on their rare tick (`Building_Heater.TickRare`,
    /// `Building_Cooler.TickRare`): while powered, move the room towards the
    /// target by at most energy × 250/60 ÷ cells and draw full power, or
    /// idle at the low power factor.
    pub(super) fn tick_temp_control(&mut self) {
        let bucket = (self.tick % RARE_TICK) as i64;
        let defs = self.defs.clone();
        let due: Vec<ItemId> = self
            .map
            .structures()
            .iter()
            .filter(|s| defs.things[s.def].temp_control.is_some() && s.power.on)
            .filter(|s| (s.id_number as i64).rem_euclid(RARE_TICK as i64) == bucket)
            .map(|s| s.id)
            .collect();
        for id in due {
            let Some(s) = self.map.structure(id) else {
                continue;
            };
            let def = &defs.things[s.def];
            let (Some(tc), Some(p)) = (def.temp_control, def.power.as_ref()) else {
                continue;
            };
            let target = s.power.target_temperature;
            let fp = s.footprint;
            let cooler = def.thing_class.as_deref() == Some("Building_Cooler");
            let changed = if cooler {
                self.cooler_rare(fp, tc.energy_per_second, target)
            } else {
                let ambient = self.cell_temperature(fp.center);
                let efficiency = if ambient < 20.0 {
                    1.0
                } else if ambient > 120.0 {
                    0.0
                } else {
                    ((120.0 - ambient) / 100.0).clamp(0.0, 1.0)
                };
                let limit = tc.energy_per_second * efficiency * RARE_SECONDS;
                let change = self.control_temperature_change(fp.center, limit, target);
                if change.abs() > 1e-6 {
                    self.add_room_temperature(fp.center, change);
                    true
                } else {
                    false
                }
            };
            let s = self.map.structure_mut(id).expect("listed");
            s.power.output = if changed {
                -p.base_power_consumption
            } else {
                -p.base_power_consumption * tc.low_power_consumption_factor
            };
            s.power.high_power = changed;
        }
    }

    /// The cooler pulls heat from the cell in front (south, turned by its
    /// rotation) and pushes 1.25× it out the back.
    fn cooler_rare(&mut self, fp: Footprint, energy_per_second: f32, target: f32) -> bool {
        let rot = crate::geom::rot_index(fp.rot);
        let turn = |c: Cell| -> Cell {
            match rot {
                0 => c,
                1 => Cell::new(c.z, -c.x),
                2 => Cell::new(-c.x, -c.z),
                _ => Cell::new(-c.z, c.x),
            }
        };
        let cold = fp.center + turn(Cell::new(0, -1));
        let hot = fp.center + turn(Cell::new(0, 1));
        if !self.path_grid.walkable(cold) || !self.path_grid.walkable(hot) {
            return false;
        }
        let t_hot = self.cell_temperature(hot);
        let t_cold = self.cell_temperature(cold);
        let mut diff = t_hot - t_cold;
        if t_hot - 40.0 > diff {
            diff = t_hot - 40.0;
        }
        let efficiency = (1.0 - diff / 130.0).max(0.0);
        let energy = energy_per_second * efficiency * RARE_SECONDS;
        let change = self.control_temperature_change(cold, energy, target);
        if change.abs() > 1e-6 {
            self.add_room_temperature(cold, change);
            self.push_heat(hot, -energy * 1.25);
            true
        } else {
            false
        }
    }

    /// `GenTemperature.ControlTemperatureTempChange`: towards the target, at
    /// most energy ÷ the room's cells; nothing outdoors.
    fn control_temperature_change(&mut self, c: Cell, energy: f32, target: f32) -> f32 {
        let Some(room) = self.regions.room_at(c) else {
            return 0.0;
        };
        if self.room_uses_outdoor(room) {
            return 0.0;
        }
        let cells = self.room_cell_count(room) as f32;
        let limit = energy / cells;
        let wanted = target - self.room_temps.temps[room];
        if energy > 0.0 {
            wanted.min(limit).max(0.0)
        } else {
            wanted.max(limit).min(0.0)
        }
    }

    fn add_room_temperature(&mut self, c: Cell, change: f32) {
        if let Some(room) = self.regions.room_at(c)
            && let Some(t) = self.room_temps.temps.get_mut(room)
        {
            *t += change;
        }
    }

    /// Debug/player tool: flips a building's power switch (`CompFlickable`;
    /// the game needs a pawn to flick it).
    pub fn set_power_switch(&mut self, id: ItemId, on: bool) {
        let defs = self.defs.clone();
        let Some(s) = self.map.structure_mut(id) else {
            return;
        };
        if !defs.things[s.def].flickable {
            return;
        }
        let changed = s.power.switch_on != on;
        s.power.switch_on = on;
        if !on {
            // `FlickedOff`: the trader stops at once.
            s.power.on = false;
        }
        // A power switch carries power only while on: its nets are redone
        // (`Notfiy_TransmitterTransmitsPowerNowChanged`).
        if changed && defs.things[s.def].thing_class.as_deref() == Some("Building_PowerSwitch") {
            let fp = s.footprint;
            self.power
                .pending
                .push(PowerAction::DeregisterTransmitter(id, fp));
            self.power
                .pending
                .push(PowerAction::RegisterTransmitter(id, fp));
        }
        self.light_key = None;
    }

    /// Debug tool: processes pending power net changes now (the game does
    /// it between ticks, in `MapUpdate`).
    pub fn debug_update_power_nets(&mut self) {
        self.update_power_nets();
    }

    /// Debug tool: a building vanishes (`Destroy(Vanish)`).
    pub fn debug_destroy_structure(&mut self, id: ItemId) {
        self.power_despawned(id);
        if self.map.remove_structure(id).is_some() {
            self.refresh_path_grid();
            self.light_key = None;
        }
    }

    /// Debug tool: sets a part's power state and output (W).
    pub fn debug_set_power(&mut self, id: ItemId, on: bool, output: f32) {
        if let Some(s) = self.map.structure_mut(id) {
            s.power.on = on;
            s.power.output = output;
        }
    }

    /// `BreakdownManager.MapComponentTick`: every 1,041 ticks each running
    /// breakdownable building may break down (MTB 13,680,000 ticks); a
    /// broken-down part stops (`Breakdown` signal).
    pub(super) fn tick_breakdowns(&mut self) {
        if !self.tick.is_multiple_of(1041) {
            return;
        }
        let defs = self.defs.clone();
        let candidates: Vec<ItemId> = self
            .map
            .structures()
            .iter()
            .filter(|s| defs.things[s.def].breakdownable)
            .map(|s| s.id)
            .collect();
        for id in candidates {
            let can = self.map.structure(id).is_some_and(|s| {
                !s.power.broken_down
                    && (defs.things[s.def]
                        .power
                        .as_ref()
                        .is_none_or(|p| !p.is_trader())
                        || s.power.on)
            });
            if can && self.rng.mtb_event_occurs(13_680_000.0, 1.0, 1041.0) {
                self.break_down(id);
            }
        }
    }

    /// `DoBreakdown`.
    pub fn break_down(&mut self, id: ItemId) {
        let defs = self.defs.clone();
        let Some(s) = self.map.structure_mut(id) else {
            return;
        };
        s.power.broken_down = true;
        s.power.on = false;
        if defs.things[s.def].thing_class.as_deref() == Some("Building_PowerSwitch") {
            let fp = s.footprint;
            self.power
                .pending
                .push(PowerAction::DeregisterTransmitter(id, fp));
            self.power
                .pending
                .push(PowerAction::RegisterTransmitter(id, fp));
        }
        self.light_key = None;
    }

    /// `Notify_Repaired`.
    fn repaired(&mut self, id: ItemId) {
        let defs = self.defs.clone();
        let Some(s) = self.map.structure_mut(id) else {
            return;
        };
        s.power.broken_down = false;
        if defs.things[s.def].thing_class.as_deref() == Some("Building_PowerSwitch") {
            let fp = s.footprint;
            self.power
                .pending
                .push(PowerAction::DeregisterTransmitter(id, fp));
            self.power
                .pending
                .push(PowerAction::RegisterTransmitter(id, fp));
        }
        self.light_key = None;
    }

    /// The repair job starts: walk to touch the component.
    pub(super) fn begin_fix(&mut self, i: usize) -> bool {
        let Some(JobKind::FixBrokenDown { component, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return false;
        };
        let Some(cell) = self.map.item(component).map(|it| it.position) else {
            return false;
        };
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => {
                self.fix_pick_up(i);
                true
            }
            super::farming::Touch::Walking => true,
            super::farming::Touch::NoPath => false,
        }
    }

    /// `StartCarryThing` (one component), then to the building.
    pub(super) fn fix_pick_up(&mut self, i: usize) {
        let Some(JobKind::FixBrokenDown {
            building,
            component,
            ..
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some((def, stack, hp)) = self
            .map
            .item(component)
            .map(|it| (it.def, it.stack_count, it.hit_points))
        else {
            self.end_job(i, false);
            return;
        };
        let taken = self.map.take_from_item(component, 1);
        let id = if taken < stack {
            self.map.allocate_item_id()
        } else {
            component
        };
        self.refresh_path_grid();
        self.pawns[i].carried = Some(crate::pawn::Carried {
            id,
            def,
            count: taken,
            rot: 0.0,
            hit_points: hp,
        });
        set_fix_stage(&mut self.pawns[i], FixStage::GotoBuilding);
        let Some(cell) = self.map.structure(building).map(|s| s.footprint.center) else {
            self.end_job(i, false);
            return;
        };
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => self.start_fix_work(i),
            super::farming::Touch::Walking => {}
            super::farming::Touch::NoPath => self.end_job(i, false),
        }
    }

    /// At the building: 1,000 ticks of work, the first paid on arrival.
    pub(super) fn start_fix_work(&mut self, i: usize) {
        set_fix_stage(
            &mut self.pawns[i],
            FixStage::Work {
                ticks_left: crate::repair::FIX_TICKS - 1,
            },
        );
    }

    /// The component is used up; the repair works with the pawn's
    /// `FixBrokenDownBuildingSuccessChance`.
    pub(super) fn finish_fix(&mut self, i: usize) {
        let Some(JobKind::FixBrokenDown { building, .. }) =
            self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        self.pawns[i].carried = None;
        let chance = self.pawn_stat_of(i, "FixBrokenDownBuildingSuccessChance");
        if self.rng.value() <= chance {
            self.repaired(building);
        }
        self.end_job(i, true);
    }

    /// The player's switch toggle (`CompFlickable` gizmo): flips what the
    /// player wants and marks the building for a colonist to flick
    /// (`FlickUtility.UpdateFlickDesignation`).
    pub fn toggle_switch(&mut self, id: ItemId) {
        let defs = self.defs.clone();
        let Some(s) = self.map.structure_mut(id) else {
            return;
        };
        if !defs.things[s.def].flickable {
            return;
        }
        s.power.want_switch_on = !s.power.want_switch_on;
        let wants = s.power.want_switch_on != s.power.switch_on;
        self.map.flick_designations.retain(|&d| d != id);
        if wants {
            self.map.flick_designations.push(id);
        }
    }

    /// The flick job starts: walk to touch the building.
    pub(super) fn begin_flick(&mut self, i: usize) -> bool {
        let Some(JobKind::Flick { target, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        let Some(cell) = self.map.structure(target).map(|s| s.footprint.center) else {
            return false;
        };
        match self.walk_to_touch(i, cell, true) {
            super::farming::Touch::Here => {
                self.start_flick_wait(i);
                true
            }
            super::farming::Touch::Walking => true,
            super::farming::Touch::NoPath => false,
        }
    }

    /// Touching it: the 15-tick wait, its first tick paid on arrival.
    pub(super) fn start_flick_wait(&mut self, i: usize) {
        if let Some(Job {
            kind: JobKind::Flick { stage, .. },
            ..
        }) = &mut self.pawns[i].job
        {
            *stage = crate::job::FlickStage::Wait {
                ticks_left: crate::flick::FLICK_TICKS - 1,
            };
        }
    }

    /// `DoFlick` if it still wants one, then the designation goes.
    pub(super) fn finish_flick(&mut self, i: usize) {
        let Some(JobKind::Flick { target, .. }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return;
        };
        if let Some(s) = self.map.structure(target)
            && s.power.want_switch_on != s.power.switch_on
        {
            let on = !s.power.switch_on;
            self.set_power_switch(target, on);
        }
        self.map.flick_designations.retain(|&d| d != target);
        self.end_job(i, true);
    }

    /// The power nets (for display and tests).
    pub fn power_nets(&self) -> &[PowerNet] {
        &self.power.nets
    }

    /// Debug tool: sets a building's stored energy or fuel.
    pub fn debug_set_stored_energy(&mut self, id: ItemId, stored: f32) {
        if let Some(s) = self.map.structure_mut(id) {
            s.power.stored = stored;
        }
    }
}

/// The per-tick part of the flick job: walking, then the wait; the job
/// fails when the designation is gone.
pub(super) fn tick_flick(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::Flick { target, stage } = pawn.job.as_ref()?.kind else {
        return None;
    };
    if map.structure(target).is_none() || !map.flick_designations.contains(&target) {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        FlickStage::Goto => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else {
                JobEvent::ArrivedToFlick
            }
        }
        FlickStage::Wait { ticks_left } => {
            let left = ticks_left - 1;
            if let Some(Job {
                kind: JobKind::Flick { stage, .. },
                ..
            }) = &mut pawn.job
            {
                *stage = FlickStage::Wait { ticks_left: left };
            }
            if left <= 0 {
                JobEvent::Flicked
            } else {
                JobEvent::None
            }
        }
    })
}

fn set_fix_stage(pawn: &mut Pawn, new: FixStage) {
    if let Some(Job {
        kind: JobKind::FixBrokenDown { stage, .. },
        ..
    }) = &mut pawn.job
    {
        *stage = new;
    }
}

/// The per-tick part of the repair job.
pub(super) fn tick_fix(pawn: &mut Pawn, map: &Map, grid: &PathGrid, t: u64) -> Option<JobEvent> {
    let JobKind::FixBrokenDown {
        building, stage, ..
    } = pawn.job.as_ref()?.kind
    else {
        return None;
    };
    if map.structure(building).is_none() {
        return Some(JobEvent::Failed);
    }
    Some(match stage {
        FixStage::GotoComponent | FixStage::GotoBuilding => {
            tick_movement(pawn, grid, map, t);
            if pawn.is_moving() {
                JobEvent::None
            } else if stage == FixStage::GotoComponent {
                JobEvent::ArrivedAtComponent
            } else {
                JobEvent::ArrivedToFix
            }
        }
        FixStage::Work { ticks_left } => {
            let left = ticks_left - 1;
            set_fix_stage(pawn, FixStage::Work { ticks_left: left });
            if left <= 0 {
                JobEvent::Fixed
            } else {
                JobEvent::None
            }
        }
    })
}
