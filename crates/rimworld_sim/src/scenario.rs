//! Scenario data (`ScenarioDef`): what the colony starts with.

use rimworld_defs::{DefId, GameDefs, ThingDef};

use crate::food::unit_nutrition;
use crate::grid::Cell;
use crate::sim::Sim;

/// The scenario the prototype starts (the game's default).
pub const DEFAULT_SCENARIO: &str = "Crashlanded";

/// `ScenPart_StartingThing_Defined` entries of `scenario`: (thing, count).
pub fn starting_things(defs: &GameDefs, scenario: &str) -> Vec<(DefId<ThingDef>, u32)> {
    let Some(def) = defs.raw.get("ScenarioDef", scenario) else {
        return Vec::new();
    };
    let Some(parts) = def.node.child("scenario").and_then(|s| s.child("parts")) else {
        return Vec::new();
    };
    parts
        .children
        .iter()
        .filter(|p| p.attr("Class") == Some("ScenPart_StartingThing_Defined"))
        .filter_map(|p| {
            let thing = defs.things.id(p.child_text("thingDef")?)?;
            let count = p
                .child_text("count")
                .and_then(|c| c.parse().ok())
                .unwrap_or(1);
            Some((thing, count))
        })
        .collect()
}

/// Places the scenario's starting firearms near `around` (the colonists
/// equip them by order).
// COMPATIBILITY TODO: currently approximate — weapon quality and the drop
// pods are not modelled; melee weapons, apparel and silver wait until they
// have a use.
pub fn spawn_starting_weapons(sim: &mut Sim, scenario: &str, around: Cell) -> u32 {
    let guns: Vec<(DefId<ThingDef>, u32)> = starting_things(&sim.defs, scenario)
        .into_iter()
        .filter(|&(t, _)| sim.is_firearm(t))
        .collect();
    let mut cells = (2..20).flat_map(|r| {
        (-r..=r)
            .flat_map(move |dz| (-r..=r).map(move |dx| around + Cell::new(dx, dz)))
            .filter(move |c| c.chebyshev(around) == r)
    });
    let mut spawned = 0;
    for (thing, count) in guns {
        for _ in 0..count {
            let Some(cell) = cells.find(|&c| {
                sim.path_grid().walkable(c)
                    && sim.map.items_at(c).next().is_none()
                    && sim.pawn_at(c).is_none()
            }) else {
                return spawned;
            };
            sim.spawn_item(thing, cell, 1);
            spawned += 1;
        }
    }
    spawned
}

/// Places the scenario's starting food near `around`, split into stacks of
/// at most `stackLimit`, one stack per free walkable cell in rings around
/// the point. Other starting things wait until they have a use.
// COMPATIBILITY TODO: currently approximate — the game delivers starting
// things in drop pods (`DropPodUtility`) next to the arriving colonists.
pub fn spawn_starting_food(sim: &mut Sim, scenario: &str, around: Cell) -> u32 {
    let food: Vec<(DefId<ThingDef>, u32)> = starting_things(&sim.defs, scenario)
        .into_iter()
        .filter(|&(t, _)| {
            let def = &sim.defs.things[t];
            def.ingestible.is_some() && unit_nutrition(&sim.defs, def) > 0.0
        })
        .collect();
    let mut cells = (3..20).flat_map(|r| {
        (-r..=r)
            .flat_map(move |dz| (-r..=r).map(move |dx| around + Cell::new(dx, dz)))
            .filter(move |c| c.chebyshev(around) == r)
    });
    let mut spawned = 0;
    for (thing, mut count) in food {
        let limit = sim.defs.things[thing].stack_limit.max(1) as u32;
        while count > 0 {
            let Some(cell) = cells.find(|&c| {
                sim.path_grid().walkable(c)
                    && sim.map.items_at(c).next().is_none()
                    && sim.pawn_at(c).is_none()
            }) else {
                return spawned;
            };
            let n = count.min(limit);
            sim.spawn_item(thing, cell, n);
            count -= n;
            spawned += n;
        }
    }
    spawned
}

/// `ScenPart_ScatterThingsNearPlayerStart` entries of `scenario`.
pub fn scattered_near_start(defs: &GameDefs, scenario: &str) -> Vec<(DefId<ThingDef>, u32)> {
    let Some(def) = defs.raw.get("ScenarioDef", scenario) else {
        return Vec::new();
    };
    let Some(parts) = def.node.child("scenario").and_then(|s| s.child("parts")) else {
        return Vec::new();
    };
    parts
        .children
        .iter()
        .filter(|p| p.attr("Class") == Some("ScenPart_ScatterThingsNearPlayerStart"))
        .filter_map(|p| {
            let thing = defs.things.id(p.child_text("thingDef")?)?;
            let count = p
                .child_text("count")
                .and_then(|c| c.parse().ok())
                .unwrap_or(1);
            Some((thing, count))
        })
        .collect()
}

/// Places the scenario's things scattered near the start, as full stacks
/// on free walkable cells in rings 6 to 20 cells from `around`.
// COMPATIBILITY TODO: currently approximate — the game scatters with
// `GenStep_ScatterThings` (random clusters, stack sizes and spots).
pub fn spawn_scattered_near_start(sim: &mut Sim, scenario: &str, around: Cell) -> u32 {
    let things = scattered_near_start(&sim.defs, scenario);
    let mut cells = (6..20).flat_map(|r| {
        (-r..=r)
            .flat_map(move |dz| (-r..=r).map(move |dx| around + Cell::new(dx, dz)))
            .filter(move |c| c.chebyshev(around) == r)
    });
    let mut spawned = 0;
    for (thing, mut count) in things {
        if !sim.defs.things[thing].ever_storable() {
            continue;
        }
        let limit = sim.defs.things[thing].stack_limit.max(1) as u32;
        while count > 0 {
            let Some(cell) = cells.find(|&c| {
                sim.map.size().contains(c)
                    && sim.path_grid().walkable(c)
                    && sim.map.items_at(c).next().is_none()
                    && sim.map.storage.zone_at(c).is_none()
            }) else {
                return spawned;
            };
            let n = count.min(limit);
            sim.spawn_item(thing, cell, n);
            count -= n;
            spawned += n;
        }
    }
    spawned
}
