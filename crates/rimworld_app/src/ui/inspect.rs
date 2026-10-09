//! The inspect pane (`MainTabWindow_Inspect`, `InspectPaneUtility`,
//! `InspectPaneFiller`; docs/research.md §66): a 432×165 window at the
//! bottom left, above the main buttons, shown while something is selected
//! and no other main tab is open. The selection's label in the medium font,
//! a row of bars (health; for pawns mood and the schedule), then the
//! inspect string.

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_sim::map::{Buildable, ConstructStage};
use rimworld_sim::sim::{RotStage, ThingRef, ZoneRef};
use rimworld_sim::{NeedKind, PawnId, Sim};

use super::lang::{cap, thing_label, zone_label};
use super::select::{Selectable, Selection};
use super::{
    Align, FONT_MEDIUM, FONT_SMALL, FONT_TINY, Lang, Paint, Painter, UiHits, UiInput, layer,
};
use crate::SimState;

/// `InspectPaneUtility.PaneSizeFor` with no inspect tabs.
// COMPATIBILITY TODO: currently approximate — the inspect tabs (Gear,
// Health, Needs, Bills, Storage, ...) above the pane are not drawn, so the
// pane keeps its minimum width.
pub const PANE_SIZE: Vec2 = Vec2::new(432.0, 165.0);

/// `Widgets.WindowBGFillColor` / `WindowBGBorderColor`.
pub const WINDOW_BG: Color = Color::srgb(21.0 / 255.0, 25.0 / 255.0, 29.0 / 255.0);
pub const WINDOW_BORDER: Color = Color::srgb(97.0 / 255.0, 108.0 / 255.0, 122.0 / 255.0);

/// `Widgets.DrawWindowBackground`.
pub fn window_background(p: &mut Painter, r: Rect) {
    p.rect(r, WINDOW_BG);
    p.outline(r, 1.0, WINDOW_BORDER);
}

/// `InspectPaneUtility.AdjustedLabelFor`.
fn pane_label(sim: &Sim, lang: &Lang, sel: &Selection) -> String {
    match sel.objects.as_slice() {
        [Selectable::Zone(z)] => zone_label(sim, lang, *z),
        [Selectable::Thing(t)] => cap(&thing_label(sim, lang, *t)),
        many => {
            let zones: Vec<ZoneRef> = many
                .iter()
                .filter_map(|o| match o {
                    Selectable::Zone(z) => Some(*z),
                    _ => None,
                })
                .collect();
            if !zones.is_empty() {
                let first = zone_label(sim, lang, zones[0]);
                let same = zones
                    .iter()
                    .all(|&z| sim.zone_label(z).0 == sim.zone_label(zones[0]).0);
                let base = if same {
                    first
                        .rsplit_once(' ')
                        .map_or(first.clone(), |(b, _)| b.to_owned())
                } else {
                    lang.tr("VariousLabel")
                };
                return format!("{base} x{}", zones.len());
            }
            // "Plural xN" when all share a def label, else "Various xN".
            let defs: Vec<String> = many
                .iter()
                .filter_map(|o| match o {
                    Selectable::Thing(t) => Some(def_label(sim, *t)),
                    _ => None,
                })
                .collect();
            let count: u32 = many
                .iter()
                .map(|o| match o {
                    Selectable::Thing(ThingRef::Item(id)) => {
                        sim.map.item(*id).map_or(1, |i| i.stack_count)
                    }
                    _ => 1,
                })
                .sum();
            let base = if defs.iter().all(|d| *d == defs[0]) {
                cap(&defs[0])
            } else {
                lang.tr("VariousLabel")
            };
            format!("{base} x{count}")
        }
    }
}

fn def_label(sim: &Sim, t: ThingRef) -> String {
    match t {
        ThingRef::Pawn(p) => sim
            .pawn(p)
            .map(|x| sim.defs.pawn_kinds[x.kind].label.clone())
            .unwrap_or_default(),
        ThingRef::Rock(c) => sim.map.buildings[c]
            .map(|b| sim.defs.things[b].label.clone())
            .unwrap_or_default(),
        ThingRef::Item(id) => sim
            .map
            .item(id)
            .map(|i| i.def)
            .or_else(|| sim.map.structure(id).map(|s| s.def))
            .or_else(|| sim.map.plant(id).map(|p| p.def))
            .map(|d| sim.defs.things[d].label.clone())
            .unwrap_or_default(),
    }
}

/// `InspectPaneFiller.DrawHealth` and friends: a 93×16 bar.
fn fillable_bar(p: &mut Painter, at: &mut f32, y: f32, fill: f32, label: &str, color: Color) {
    let r = Rect::new(*at, y, *at + 93.0, y + 16.0);
    // `BarBGTex` (10, 10, 10).
    p.rect(r, Color::srgb_u8(10, 10, 10));
    p.rect(
        Rect::new(
            r.min.x,
            r.min.y,
            r.min.x + 93.0 * fill.clamp(0.0, 1.0),
            r.max.y,
        ),
        color,
    );
    p.label_mid(r, label.to_owned(), FONT_TINY, Color::WHITE, Align::Center);
    *at += 93.0 + 6.0;
}

/// `HealthUtility.GetGeneralConditionLabel` (short).
fn condition_label(sim: &Sim, lang: &Lang, p: PawnId) -> String {
    let Some(pawn) = sim.pawn(p) else {
        return String::new();
    };
    if pawn.health.dead {
        lang.tr("Dead")
    } else if pawn.health.downed {
        lang.tr("Incapacitated")
    } else if pawn.health.hediffs.iter().any(|h| h.part.is_some()) {
        lang.tr("Injured")
    } else {
        lang.tr("Healthy")
    }
}

/// `Need_Mood.MoodString` from the thresholds.
fn mood_string(sim: &Sim, lang: &Lang, p: PawnId, level: f32) -> String {
    let key = if sim.mental_state_of(p).is_some() {
        "Mood_MentalState"
    } else {
        match sim.break_thresholds_of(p) {
            Some((extreme, _, _)) if level < extreme => "Mood_AboutToBreak",
            Some((_, major, _)) if level < major => "Mood_OnEdge",
            Some((_, _, minor)) if level < minor => "Mood_Stressed",
            _ if level < 0.65 => "Mood_Neutral",
            _ if level < 0.9 => "Mood_Content",
            _ => "Mood_Happy",
        }
    };
    cap(&lang.tr(key))
}

/// The inspect string lines (`GetInspectString`) of one thing.
fn inspect_lines(sim: &Sim, lang: &Lang, t: ThingRef) -> Vec<String> {
    let mut out = Vec::new();
    match t {
        ThingRef::Pawn(id) => {
            let Some(p) = sim.pawn(id) else { return out };
            // `MainDesc`: the kind, for animals and non-colonists.
            if sim.is_animal(id) || !p.is_colonist {
                out.push(cap(&sim.defs.pawn_kinds[p.kind].label));
            }
            if let Some(state) = sim.mental_state_of(id) {
                let label = sim
                    .defs
                    .mental_states
                    .get(state)
                    .map_or(state, |d| d.label.as_str());
                out.push(cap(label));
            }
            if let Some(eq) = &p.equipment {
                out.push(format!(
                    "{}: {}",
                    lang.tr("Equipped"),
                    cap(&sim.defs.things[eq.def].label)
                ));
            }
            if let Some(c) = &p.carried {
                out.push(format!(
                    "{}: {}",
                    lang.tr("Carrying"),
                    cap(&sim.defs.things[c.def].label)
                ));
            }
            let report = sim.job_report(p);
            if !report.is_empty() {
                let mut r = cap(&report);
                if !r.ends_with('.') {
                    r.push('.');
                }
                out.push(r);
            }
            // COMPATIBILITY TODO: currently approximate — the game shows
            // needs on the Needs tab; until the tabs exist, the pane lists
            // the supported needs and the hunt mark.
            let needs: Vec<String> = [NeedKind::Food, NeedKind::Rest, NeedKind::Joy]
                .iter()
                .filter_map(|&k| {
                    let n = p.needs.get(k)?;
                    Some(format!(
                        "{} {:.0}%",
                        cap(&sim.defs.needs[n.def].label),
                        n.percent() * 100.0
                    ))
                })
                .collect();
            if !needs.is_empty() {
                out.push(needs.join("   "));
            }
            if sim.hunt_designated(id) {
                out.push(lang.tr("DesignatorHunt"));
            }
        }
        ThingRef::Rock(_) => {}
        ThingRef::Item(id) => {
            if let Some(it) = sim.map.item(id) {
                if let Some(stage) = sim.rot_stage(id) {
                    let key = match stage {
                        RotStage::Fresh => "RotStateFresh",
                        RotStage::Rotting => "RotStateRotting",
                        RotStage::Dessicated => "RotStateDessicated",
                    };
                    out.push(cap(&lang.tr(key)));
                }
                if it.forbidden {
                    out.push(cap(&lang.tr("ForbiddenLower")));
                }
            } else if let Some(s) = sim.map.structure(id) {
                let def = &sim.defs.things[s.def];
                if let Some(p) = def.power.as_ref() {
                    if p.is_trader() {
                        let key = if s.power.output >= 0.0 {
                            "PowerOutput"
                        } else {
                            "PowerNeeded"
                        };
                        out.push(format!("{}: {:.0} W", lang.tr(key), s.power.output.abs()));
                    }
                    if let Some((max, _)) = p.battery {
                        out.push(format!(
                            "{}: {:.0} / {:.0} Wd",
                            lang.tr("PowerBatteryStored"),
                            s.power.stored,
                            max
                        ));
                    }
                }
                if let Some(r) = &def.refuelable {
                    out.push(format!(
                        "{}: {:.1} / {:.0}",
                        lang.tr("Fuel"),
                        s.fuel,
                        r.capacity
                    ));
                }
                // COMPATIBILITY TODO: currently approximate — bills are on
                // the Bills tab in the game; a summary stands in for it.
                let bills = sim.bills(id);
                if !bills.is_empty() {
                    let names: Vec<String> = bills
                        .iter()
                        .map(|b| sim.defs.recipes[b.recipe].label.clone())
                        .collect();
                    out.push(format!("Bills: {}", names.join(", ")));
                }
            } else if let Some(k) = sim.map.constructible(id) {
                let defs = &sim.defs;
                let cost = rimworld_sim::construct::total_cost(defs, k);
                if k.stage == ConstructStage::Frame {
                    let total = match k.building {
                        Buildable::Thing(b) => rimworld_sim::stats::def_stat(
                            defs,
                            &defs.things[b],
                            k.stuff.map(|s| &defs.things[s]),
                            "WorkToBuild",
                        ),
                        Buildable::Floor(f) => defs.terrain[f]
                            .stat_bases
                            .get("WorkToBuild")
                            .copied()
                            .unwrap_or(0.0),
                    };
                    out.push(format!(
                        "{}: {:.0}",
                        lang.tr("WorkLeft"),
                        ((total - k.work_done) / 60.0).max(0.0).ceil()
                    ));
                }
                let parts: Vec<String> = cost
                    .iter()
                    .map(|&(d, n)| format!("{} / {} {}", k.delivered(d), n, defs.things[d].label))
                    .collect();
                if !parts.is_empty() {
                    out.push(format!(
                        "{}: {}",
                        lang.tr("ContainedResources"),
                        parts.join(", ")
                    ));
                }
            } else if let Some(p) = sim.map.plant(id) {
                // `Plant.GetInspectString`: "45% grown".
                out.push(cap(&lang.tr_args(
                    "PercentGrowth",
                    &[&format!("{:.0}%", p.growth * 100.0)],
                )));
                if rimworld_sim::sim::plant_ready(sim, p.position) {
                    out.push(lang.tr("ReadyToHarvest"));
                }
            }
        }
    }
    out
}

/// The pane, and its tabs' place (no tabs yet).
#[allow(clippy::too_many_arguments)]
pub fn inspect_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    sim: Res<SimState>,
    lang: Res<Lang>,
    sel: Res<Selection>,
    tabs: Res<super::tabs::MainTabs>,
    window: Single<&Window>,
    mut hits: ResMut<UiHits>,
) {
    if tabs.open.is_some() || sel.is_empty() {
        return;
    }
    let sim = &sim.0;
    let screen = super::screen(&window);
    let r = Rect::new(
        0.0,
        screen.y - 35.0 - PANE_SIZE.y,
        PANE_SIZE.x,
        screen.y - 35.0,
    );
    hits.name("inspect", r);
    input.absorb(r);
    let mut p = paint.painter(layer::INSPECT);
    window_background(&mut p, r);
    // `InspectPaneOnGUI`: contracted by 12, then 4 up and 6 down.
    let inner = Rect::new(
        r.min.x + 12.0,
        r.min.y + 12.0 - 4.0,
        r.max.x - 12.0,
        r.max.y - 12.0 + 6.0,
    );
    let label = pane_label(sim, &lang, &sel);
    p.label_mid(
        Rect::new(
            inner.min.x,
            inner.min.y,
            inner.max.x + 300.0,
            inner.min.y + 30.0,
        ),
        label,
        FONT_MEDIUM,
        Color::WHITE,
        Align::Left,
    );
    // Contents only for one thing or several of the same def
    // (`ShouldShowPaneContents`).
    let single = match sel.objects.as_slice() {
        [o] => Some(*o),
        _ => None,
    };
    let Some(obj) = single else { return };
    let mut y = inner.min.y + 26.0;
    let mut lines: Vec<String> = Vec::new();
    match obj {
        Selectable::Thing(t) => {
            y += 3.0;
            let mut x = inner.min.x;
            // `DrawHealth`.
            match t {
                ThingRef::Pawn(id) => {
                    let pct = sim
                        .health_view(id)
                        .map_or(1.0, |v| v.summary_health_percent());
                    fillable_bar(
                        &mut p,
                        &mut x,
                        y,
                        pct,
                        &condition_label(sim, &lang, id),
                        Color::srgb_u8(35, 35, 35),
                    );
                    let (mood, _) = sim.mood_view(id);
                    if let Some((level, _)) = mood {
                        fillable_bar(
                            &mut p,
                            &mut x,
                            y,
                            level,
                            &mood_string(sim, &lang, id, level),
                            Color::srgb_u8(26, 52, 52),
                        );
                    }
                    if sim.pawn(id).is_some_and(|pw| pw.is_colonist) {
                        // `DrawTimetableSetting`: the current assignment.
                        let a = rimworld_sim::rest::time_assignment(sim.hour_of_day(), true);
                        let (name, col) = match a {
                            rimworld_sim::rest::TimeAssignment::Sleep => {
                                ("Sleep", Color::srgb(0.15, 0.3, 0.6))
                            }
                            rimworld_sim::rest::TimeAssignment::Work => {
                                ("Work", Color::srgb(0.6, 0.55, 0.15))
                            }
                            rimworld_sim::rest::TimeAssignment::Joy => {
                                ("Joy", Color::srgb(0.45, 0.2, 0.55))
                            }
                            rimworld_sim::rest::TimeAssignment::Meditate => {
                                ("Meditate", Color::srgb(0.2, 0.5, 0.5))
                            }
                            rimworld_sim::rest::TimeAssignment::Anything => {
                                ("Anything", Color::srgb(0.35, 0.35, 0.35))
                            }
                        };
                        let label = sim
                            .defs
                            .raw
                            .table("TimeAssignmentDef")
                            .and_then(|t| t.get(name))
                            .and_then(|d| d.node.child_text("label"))
                            .map_or(name.to_owned(), cap);
                        fillable_bar(&mut p, &mut x, y, 1.0, &label, col);
                    }
                }
                ThingRef::Item(id) => {
                    let hp = sim
                        .map
                        .item(id)
                        .and_then(|i| i.hit_points.map(|h| (h, sim.max_hit_points(i.def))))
                        .or_else(|| {
                            sim.map
                                .structure(id)
                                .map(|s| (sim.max_hit_points(s.def), sim.max_hit_points(s.def)))
                        });
                    if let Some((h, max)) = hp
                        && max > 0
                    {
                        hp_bar(&mut p, &mut x, y, h, max);
                    }
                }
                ThingRef::Rock(c) => {
                    if let (Some(h), Some(b)) = (sim.rock_hit_points(c), sim.map.buildings[c]) {
                        hp_bar(&mut p, &mut x, y, h, sim.max_hit_points(b));
                    }
                }
            }
            y += 18.0;
            lines = inspect_lines(sim, &lang, t);
        }
        Selectable::Zone(z) => {
            if let ZoneRef::Growing(g) = z {
                let plant = sim.map.growing_zone(g).plant;
                // COMPATIBILITY TODO: currently approximate — the game sets
                // the crop with the zone's "plant to grow" gizmo; it is
                // shown here.
                lines.push(cap(&sim.defs.things[plant].label));
            }
            lines.push(format!("{} cells", sim.zone_cells(z).len()));
        }
    }
    let text = lines.join("\n");
    p.text(
        Rect::new(inner.min.x, y, inner.max.x, inner.max.y),
        text,
        FONT_SMALL,
        Color::WHITE,
        Align::Left,
    );
}

/// `DrawHealth` for things: hit points, coloured by how many are left.
fn hp_bar(p: &mut Painter, x: &mut f32, y: f32, h: i32, max: i32) {
    let col = if h >= max {
        Color::srgb_u8(35, 35, 35)
    } else if h as f32 > max as f32 * 0.5 {
        Color::srgb(0.45, 0.45, 0.1)
    } else {
        Color::srgb(0.5, 0.1, 0.1)
    };
    fillable_bar(p, x, y, h as f32 / max as f32, &format!("{h} / {max}"), col);
}
