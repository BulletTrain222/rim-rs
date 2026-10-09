//! The rules of the game's order designators and of blueprint placement
//! (`Designator.CanDesignateCell` / `CanDesignateThing`,
//! `GenConstruct.CanPlaceBlueprintAt`; docs/research.md §66), so a front
//! end can preview, validate and apply them through one interface.

use rimworld_defs::{DefId, TerrainDef, ThingDef};

use super::Sim;
use crate::geom::Footprint;
use crate::grid::Cell;
use crate::job::Rot4;
use crate::map::ItemId;
use crate::pawn::PawnId;

/// `GenGrid.NoBuildEdgeWidth`: no blueprints this close to the map edge.
pub const NO_BUILD_EDGE_WIDTH: i32 = 10;

/// A thing on the map the player can select or designate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThingRef {
    /// An item, building, blueprint, frame or plant.
    Item(ItemId),
    /// Natural rock (a `Mineable`; the simulation keeps it per cell).
    Rock(Cell),
    Pawn(PawnId),
}

/// The order designators the simulation supports (`Designator_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderDesignator {
    Cancel,
    Deconstruct,
    Mine,
    HarvestWood,
    CutPlants,
    Harvest,
    Hunt,
    SmoothSurface,
    RemoveFloor,
}

/// Why a designator refuses a cell or thing: silently, or with the
/// game's message (a keyed string).
pub type Rejection = Option<&'static str>;

impl Sim {
    /// `CanDesignateCell` of an order designator.
    pub fn can_designate_cell(&self, d: OrderDesignator, c: Cell) -> Result<(), Rejection> {
        if !self.map.size().contains(c) {
            return Err(None);
        }
        match d {
            OrderDesignator::Mine => {
                if self.map.mine_designations.contains(&c) {
                    return Err(None);
                }
                if !self.map.buildings[c].is_some_and(|b| self.defs.things[b].mineable) {
                    return Err(Some("MessageMustDesignateMineable"));
                }
                Ok(())
            }
            OrderDesignator::Deconstruct => {
                if self
                    .things_at(c)
                    .into_iter()
                    .any(|t| self.can_designate_thing(d, t).is_ok())
                {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            OrderDesignator::Cancel => {
                let marked = self.map.mine_designations.contains(&c)
                    || self.map.smooth_wall_designations.contains(&c)
                    || self.map.smooth_floor_designations.contains(&c)
                    || self.map.remove_floor_designations.contains(&c);
                if marked
                    || self
                        .things_at(c)
                        .into_iter()
                        .any(|t| self.can_designate_thing(d, t).is_ok())
                {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            OrderDesignator::HarvestWood
            | OrderDesignator::CutPlants
            | OrderDesignator::Harvest => {
                let Some(p) = self.map.plant_at(c) else {
                    return Err(Some("MessageMustDesignatePlants"));
                };
                self.can_designate_thing(d, ThingRef::Item(p.id))
            }
            OrderDesignator::Hunt => {
                if self.pawns.iter().any(|p| {
                    p.position == c && self.can_designate_thing(d, ThingRef::Pawn(p.id)).is_ok()
                }) {
                    Ok(())
                } else {
                    Err(Some("MessageMustDesignateHuntable"))
                }
            }
            OrderDesignator::SmoothSurface => {
                if crate::floorwork::smoothable_wall(&self.map, &self.defs, c) {
                    return if self.map.smooth_wall_designations.contains(&c) {
                        Err(None)
                    } else {
                        Ok(())
                    };
                }
                let t = &self.defs.terrain[self.map.terrain[c]];
                if self.map.smooth_floor_designations.contains(&c) {
                    return Err(None);
                }
                if self.map.buildings[c].is_some()
                    || !t.affordances.iter().any(|a| a == "SmoothableStone")
                    || t.smoothed_terrain.is_none()
                {
                    return Err(Some("MessageMustDesignateSmoothableSurface"));
                }
                Ok(())
            }
            OrderDesignator::RemoveFloor => {
                if self.map.remove_floor_designations.contains(&c)
                    || !self.map.can_remove_top_layer(&self.defs, c)
                    || self.full_impassable_edifice(c)
                {
                    Err(None)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// `CanDesignateThing` of an order designator (the reverse designator
    /// gizmos on selected things).
    pub fn can_designate_thing(&self, d: OrderDesignator, t: ThingRef) -> Result<(), Rejection> {
        match (d, t) {
            (OrderDesignator::Mine, ThingRef::Rock(c)) => {
                if self.map.mine_designations.contains(&c)
                    || !self.map.buildings[c].is_some_and(|b| self.defs.things[b].mineable)
                {
                    Err(None)
                } else {
                    Ok(())
                }
            }
            (OrderDesignator::Deconstruct, ThingRef::Item(id)) => {
                let Some(s) = self.map.structure(id) else {
                    return Err(None);
                };
                let deconstructible = self.defs.things[s.def]
                    .building
                    .as_ref()
                    .is_some_and(|b| b.deconstructible);
                if !deconstructible || self.map.deconstruct_designations.contains(&id) {
                    Err(None)
                } else {
                    Ok(())
                }
            }
            (OrderDesignator::Cancel, ThingRef::Item(id)) => {
                if self.map.constructible(id).is_some()
                    || self.map.deconstruct_designations.contains(&id)
                    || self.map.cut_designations().contains(&id)
                    || self.map.harvest_designations().contains(&id)
                    || self.map.flick_designations.contains(&id)
                {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            (OrderDesignator::Cancel, ThingRef::Rock(c)) => {
                if self.map.mine_designations.contains(&c)
                    || self.map.smooth_wall_designations.contains(&c)
                {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            (OrderDesignator::Cancel, ThingRef::Pawn(p)) => {
                if self.map.hunt_designations.contains(&p) {
                    Ok(())
                } else {
                    Err(None)
                }
            }
            (
                OrderDesignator::HarvestWood
                | OrderDesignator::CutPlants
                | OrderDesignator::Harvest,
                ThingRef::Item(id),
            ) => {
                let Some(p) = self.map.plant(id) else {
                    return Err(None);
                };
                let Some(props) = self.defs.things[p.def].plant.as_ref() else {
                    return Err(None);
                };
                let already = match d {
                    OrderDesignator::CutPlants => self.map.cut_designations().contains(&id),
                    _ => self.map.harvest_designations().contains(&id),
                };
                if already {
                    return Err(None);
                }
                match d {
                    OrderDesignator::Harvest => {
                        if !p.harvestable_now(props)
                            || props.harvest_tag.as_deref() != Some("Standard")
                        {
                            return Err(Some("MessageMustDesignateHarvestable"));
                        }
                    }
                    OrderDesignator::HarvestWood => {
                        if !p.harvestable_now(props) || !props.is_tree() {
                            return Err(Some("MessageMustDesignateHarvestableWood"));
                        }
                        if props.is_stump {
                            return Err(None);
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            (OrderDesignator::Hunt, ThingRef::Pawn(id)) => {
                let Some(i) = self.index_of(id) else {
                    return Err(None);
                };
                let p = &self.pawns[i];
                if p.health.dead
                    || p.is_colonist
                    || !self.is_animal(id)
                    || self.map.hunt_designations.contains(&id)
                {
                    Err(None)
                } else {
                    Ok(())
                }
            }
            (OrderDesignator::SmoothSurface | OrderDesignator::RemoveFloor, ThingRef::Rock(c)) => {
                self.can_designate_cell(d, c)
            }
            _ => Err(None),
        }
    }

    /// The things on a cell a designator may act on: pawns, items,
    /// buildings, blueprints and frames, plants and natural rock.
    pub fn things_at(&self, c: Cell) -> Vec<ThingRef> {
        let mut v: Vec<ThingRef> = self
            .pawns
            .iter()
            .filter(|p| p.position == c && !p.health.dead)
            .map(|p| ThingRef::Pawn(p.id))
            .collect();
        v.extend(self.map.items_at(c).map(|i| ThingRef::Item(i.id)));
        v.extend(
            self.map
                .structures()
                .iter()
                .filter(|s| s.footprint.contains(c))
                .map(|s| ThingRef::Item(s.id)),
        );
        if let Some(k) = self.map.constructible_at(c) {
            v.push(ThingRef::Item(k.id));
        }
        if let Some(p) = self.map.plant_at(c) {
            v.push(ThingRef::Item(p.id));
        }
        if self.map.buildings[c].is_some_and(|b| {
            self.defs.things[b]
                .building
                .as_ref()
                .is_some_and(|x| x.is_natural_rock)
                || self.defs.things[b].mineable
        }) {
            v.push(ThingRef::Rock(c));
        }
        v
    }

    /// `DesignateSingleCell` for each accepted cell (`DesignateMultiCell`).
    /// Returns how many cells were designated.
    pub fn designate_cells(&mut self, d: OrderDesignator, cells: &[Cell]) -> usize {
        let accepted: Vec<Cell> = cells
            .iter()
            .copied()
            .filter(|&c| self.can_designate_cell(d, c).is_ok())
            .collect();
        for &c in &accepted {
            match d {
                OrderDesignator::Mine => {
                    self.designate_mine(&[c]);
                }
                OrderDesignator::SmoothSurface => {
                    self.designate_smooth_surface(&[c]);
                }
                OrderDesignator::RemoveFloor => {
                    self.designate_remove_floor(&[c]);
                }
                OrderDesignator::Cancel => {
                    self.cancel_mine(&[c]);
                    self.map.smooth_wall_designations.retain(|&x| x != c);
                    self.map.smooth_floor_designations.retain(|&x| x != c);
                    self.map.remove_floor_designations.retain(|&x| x != c);
                    for t in self.things_at(c) {
                        if self.can_designate_thing(d, t).is_ok() {
                            self.designate_thing(d, t);
                        }
                    }
                }
                OrderDesignator::Deconstruct => {
                    // The topmost deconstructible thing.
                    if let Some(t) = self
                        .things_at(c)
                        .into_iter()
                        .find(|&t| self.can_designate_thing(d, t).is_ok())
                    {
                        self.designate_thing(d, t);
                    }
                }
                OrderDesignator::Hunt => {
                    let animals: Vec<ThingRef> = self
                        .pawns
                        .iter()
                        .filter(|p| p.position == c)
                        .map(|p| ThingRef::Pawn(p.id))
                        .collect();
                    for t in animals {
                        if self.can_designate_thing(d, t).is_ok() {
                            self.designate_thing(d, t);
                        }
                    }
                }
                OrderDesignator::HarvestWood
                | OrderDesignator::CutPlants
                | OrderDesignator::Harvest => {
                    if let Some(id) = self.map.plant_at(c).map(|p| p.id) {
                        self.designate_thing(d, ThingRef::Item(id));
                    }
                }
            }
        }
        accepted.len()
    }

    /// `DesignateThing` (used by the reverse designator gizmos).
    pub fn designate_thing(&mut self, d: OrderDesignator, t: ThingRef) {
        match (d, t) {
            (OrderDesignator::Mine, ThingRef::Rock(c)) => {
                self.designate_mine(&[c]);
            }
            (OrderDesignator::SmoothSurface, ThingRef::Rock(c)) => {
                self.designate_smooth_surface(&[c]);
            }
            (OrderDesignator::Deconstruct, ThingRef::Item(id)) => {
                if !self.map.deconstruct_designations.contains(&id) {
                    self.map.deconstruct_designations.push(id);
                }
            }
            (OrderDesignator::Cancel, ThingRef::Item(id)) => {
                if self.map.constructible(id).is_some() {
                    self.cancel_constructible(id);
                    return;
                }
                self.map.deconstruct_designations.retain(|&x| x != id);
                self.map.flick_designations.retain(|&x| x != id);
                self.map.clear_harvest_designation(id);
                self.map.clear_cut_designation(id);
            }
            (OrderDesignator::Cancel, ThingRef::Rock(c)) => {
                self.cancel_mine(&[c]);
                self.map.smooth_wall_designations.retain(|&x| x != c);
            }
            (OrderDesignator::Cancel, ThingRef::Pawn(p)) => {
                self.map.hunt_designations.retain(|&x| x != p);
            }
            (OrderDesignator::Hunt, ThingRef::Pawn(p)) => {
                if !self.map.hunt_designations.contains(&p) {
                    self.map.hunt_designations.push(p);
                }
            }
            // `Designator_Plants.DesignateThing`: the plant's other
            // designations go first.
            (OrderDesignator::CutPlants, ThingRef::Item(id)) => {
                self.map.clear_harvest_designation(id);
                self.map.designate_cut(id);
            }
            (OrderDesignator::Harvest | OrderDesignator::HarvestWood, ThingRef::Item(id)) => {
                self.map.clear_cut_designation(id);
                self.map.designate_harvest(id);
            }
            _ => {}
        }
    }

    /// `GenConstruct.CanPlaceBlueprintAt` for a building: on the map, at
    /// least 10 cells from the edge, not where the same building (or its
    /// blueprint) already stands, and on free, walkable cells.
    // COMPATIBILITY TODO: currently approximate — terrain affordances,
    // interaction cells, replaceable things and what may share a cell
    // (items, pawns, plants that get cut) are reduced to "walkable, no
    // building, no blueprint or frame".
    pub fn can_place_blueprint(
        &self,
        building: DefId<ThingDef>,
        stuff: Option<DefId<ThingDef>>,
        cell: Cell,
        rot: Rot4,
    ) -> Result<(), &'static str> {
        let def = &self.defs.things[building];
        let fp = Footprint {
            center: cell,
            rot,
            size: def.size,
        };
        let size = self.map.size();
        let e = NO_BUILD_EDGE_WIDTH;
        for c in fp.cells() {
            if !size.contains(c) {
                return Err("OutOfBounds");
            }
            if c.x < e || c.z < e || c.x >= size.width - e || c.z >= size.height - e {
                return Err("TooCloseToMapEdge");
            }
        }
        if self
            .map
            .structures()
            .iter()
            .any(|s| s.def == building && s.footprint.center == cell && s.footprint.rot == rot)
        {
            return Err("IdenticalThingExists");
        }
        if self.map.constructibles().iter().any(|k| {
            k.building == crate::map::Buildable::Thing(building)
                && k.position == cell
                && k.footprint().rot == rot
        }) {
            return Err("IdenticalBlueprintExists");
        }
        let stuff_ok = match stuff {
            Some(s) => def.accepts_stuff(&self.defs.things[s]),
            None => !def.made_from_stuff(),
        };
        if !stuff_ok {
            return Err("UnchosenStuff");
        }
        for c in fp.cells() {
            if self.map.buildings[c].is_some() || self.map.constructible_at(c).is_some() {
                return Err("SpaceAlreadyOccupied");
            }
            if !self.path_grid.walkable(c) {
                return Err("TerrainCannotSupport");
            }
        }
        Ok(())
    }

    /// `GenConstruct.CanPlaceBlueprintAt` for a floor.
    pub fn can_place_floor(
        &self,
        floor: DefId<TerrainDef>,
        cell: Cell,
    ) -> Result<(), &'static str> {
        let size = self.map.size();
        if !size.contains(cell) {
            return Err("OutOfBounds");
        }
        let e = NO_BUILD_EDGE_WIDTH;
        if cell.x < e || cell.z < e || cell.x >= size.width - e || cell.z >= size.height - e {
            return Err("TooCloseToMapEdge");
        }
        let here = self.map.terrain[cell];
        if here == floor {
            return Err("TerrainIsAlready");
        }
        let def = &self.defs.terrain[floor];
        let affords = def
            .terrain_affordance_needed
            .as_ref()
            .is_none_or(|a| self.defs.terrain[here].affordances.contains(a));
        if !affords {
            return Err("TerrainCannotSupport");
        }
        if self.map.buildings[cell].is_some() || self.map.constructible_at(cell).is_some() {
            return Err("SpaceAlreadyOccupied");
        }
        Ok(())
    }
}
