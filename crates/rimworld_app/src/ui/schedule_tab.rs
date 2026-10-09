//! The Schedule tab (`MainTabWindow_Schedule`, the `Restrict`
//! `PawnTableDef`, `PawnColumnWorker_Timetable`, `TimeAssignmentSelector`;
//! docs/research.md §67).
//!
//! The free colonists, one row each: name (with icon), copy/paste, the
//! 24-hour timetable, a gap and the allowed area. The four assignments
//! (Anything, Work, Joy, Sleep) sit as buttons over the table's top left;
//! the selected one (Work at first) is painted into every hour the mouse
//! passes over while its button is held.

use std::cmp::Ordering;

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::GameDefs;
use rimworld_sim::rest::TimeAssignment;
use rimworld_sim::{PawnId, Sim};

use super::designator::DesignatorManager;
use super::lang::{cap, pawn_label};
use super::select::Selection;
use super::tabs::{
    Clipboards, ColumnSize, CopyPaste, LabelClick, MainTab, MainTabs, NO_LIMIT, TableWindow,
    button_text, column_header, copy_paste_cell, free_colonists, header_strip, header_tip,
    jump_and_select, label_cell, label_width, row_overlays, sort_rows, table_window,
};
use super::{Anchor, FONT_SMALL, FONT_TINY, Lang, Messages, Paint, UiHits, UiInput, layer};
use crate::SimState;
use crate::graphics::PawnGraphics;

/// `PawnColumnWorker_AllowedArea.GetMinHeaderHeight`.
const HEADER_HEIGHT: f32 = 65.0;
/// The Restrict `PawnTableDef`'s `minWidth` (the default).
const MIN_TABLE_WIDTH: f32 = 998.0;
/// `TimeAssignmentSelector` grid size.
const SELECTOR: Vec2 = Vec2::new(191.0, 65.0);
/// The assignments offered (Meditate needs Royalty).
pub const ASSIGNMENTS: [TimeAssignment; 4] = [
    TimeAssignment::Anything,
    TimeAssignment::Work,
    TimeAssignment::Joy,
    TimeAssignment::Sleep,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// `LabelShortWithIcon`.
    Label,
    CopyPaste,
    Timetable,
    /// `GapTiny`.
    Gap,
    AllowedArea,
    RemainingSpace,
}

const COLUMNS: [Column; 6] = [
    Column::Label,
    Column::CopyPaste,
    Column::Timetable,
    Column::Gap,
    Column::AllowedArea,
    Column::RemainingSpace,
];

impl Column {
    fn id(self) -> &'static str {
        match self {
            Column::Label => "LabelShortWithIcon",
            Column::CopyPaste => "CopyPasteTimetable",
            Column::Timetable => "Timetable",
            Column::Gap => "GapTiny",
            Column::AllowedArea => "AllowedArea",
            Column::RemainingSpace => "RemainingSpace",
        }
    }

    fn size(self, lang: &Lang) -> ColumnSize {
        match self {
            Column::Label => {
                ColumnSize::new(label_width("Name").max(80.0), 165.0, NO_LIMIT).with_priority(100)
            }
            Column::CopyPaste => ColumnSize::fixed(36.0),
            Column::Timetable => ColumnSize::new(360.0, 504.0, 600.0),
            Column::Gap => ColumnSize::fixed(4.0),
            Column::AllowedArea => ColumnSize::new(
                label_width(&cap(&lang.tr("AllowedArea"))).max(200.0),
                273.0,
                NO_LIMIT,
            ),
            Column::RemainingSpace => ColumnSize::REMAINING,
        }
    }
}

/// A `TimeAssignmentDef`'s label and colour from the Def data.
pub fn assignment_look(defs: &GameDefs, a: TimeAssignment) -> (String, Color) {
    let def = defs
        .raw
        .table("TimeAssignmentDef")
        .and_then(|t| t.iter().find(|d| d.def_name == a.def_name()));
    let label = def
        .and_then(|d| d.node.child_text("label"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| a.def_name().to_lowercase());
    let color = def
        .and_then(|d| d.node.child_text("color"))
        .and_then(rimworld_defs::values::parse_color)
        .map_or(Color::srgb(0.45, 0.45, 0.45), |c| {
            Color::srgb(c.r, c.g, c.b)
        });
    (label, color)
}

/// The columns' comparisons: names by ordinal order, timetables by their
/// first Work hour (−1 without one).
fn compare(sim: &Sim, col: &str, a: PawnId, b: PawnId) -> Ordering {
    match col {
        "LabelShortWithIcon" => cap(&pawn_label(sim, a)).cmp(&cap(&pawn_label(sim, b))),
        "Timetable" => {
            let v = |p: PawnId| {
                sim.timetable(p)
                    .iter()
                    .position(|&x| x == TimeAssignment::Work)
                    .map_or(-1, |i| i as i32)
            };
            v(a).cmp(&v(b))
        }
        _ => Ordering::Equal,
    }
}

/// The Schedule tab's rows, in table order.
pub fn rows(sim: &Sim, tabs: &MainTabs) -> Vec<PawnId> {
    let mut pawns = free_colonists(sim);
    sort_rows(&mut pawns, &tabs.schedule_sort, |c, a, b| {
        compare(sim, c, a, b)
    });
    pawns
}

/// The Schedule tab window's geometry.
pub fn window(sim: &Sim, lang: &Lang, screen: Vec2) -> TableWindow {
    let sizes: Vec<ColumnSize> = COLUMNS.iter().map(|c| c.size(lang)).collect();
    let rows = free_colonists(sim).len();
    table_window(&sizes, HEADER_HEIGHT, rows, MIN_TABLE_WIDTH, 0.0, screen)
}

#[allow(clippy::too_many_arguments)]
pub fn schedule_tab_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    mut tabs: ResMut<MainTabs>,
    mut sim: ResMut<SimState>,
    lang: Res<Lang>,
    mut sel: ResMut<Selection>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    graphics: Option<Res<PawnGraphics>>,
    window_q: Single<&Window>,
    mut hits: ResMut<UiHits>,
    mut clip: ResMut<Clipboards>,
    mut mgr: ResMut<DesignatorManager>,
    mut messages: ResMut<Messages>,
    mut camera: Query<&mut Transform, With<Camera2d>>,
    time: Res<Time>,
) {
    if !tabs.is_open(MainTab::Schedule) {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
        tabs.set(None, &mut mgr);
        return;
    }
    let now = time.elapsed_secs();
    let screen = super::screen(&window_q);
    let tw = window(&sim.0, &lang, screen);
    let pawns = rows(&sim.0, &tabs);
    hits.name("tab:Schedule", tw.window);
    input.absorb(tw.window);
    let mut p = paint.painter(layer::MAIN_TAB);
    super::inspect::window_background(&mut p, tw.window);
    let mut tips: Option<String> = None;
    let defs = sim.0.defs.clone();
    let look = |a: TimeAssignment| assignment_look(&defs, a);

    // Headers.
    for (i, &col) in COLUMNS.iter().enumerate() {
        let r = tw.header(i);
        let icon = tabs.schedule_sort.icon(col.id());
        let clicked = match col {
            Column::Label => column_header(
                &mut p,
                &mut input,
                r,
                Some(("Name", Anchor::LowerLeft, 33.0)),
                icon,
                Some(header_strip(r)),
                &header_tip(&lang, "Name", true),
                &mut tips,
            ),
            Column::Timetable => {
                // The hours along the bottom, tiny, centred.
                let w = r.width() / 24.0;
                for h in 0..24 {
                    let x = r.min.x + h as f32 * w;
                    p.label_at(
                        Rect::new(x, r.min.y, x + w, r.max.y + 3.0),
                        h.to_string(),
                        FONT_TINY,
                        Color::WHITE,
                        Anchor::LowerCenter,
                    );
                }
                column_header(
                    &mut p,
                    &mut input,
                    r,
                    None,
                    icon,
                    Some(header_strip(r)),
                    &header_tip(&lang, "", true),
                    &mut tips,
                )
            }
            Column::AllowedArea => {
                let clicked = column_header(
                    &mut p,
                    &mut input,
                    r,
                    Some((&cap(&lang.tr("AllowedArea")), Anchor::LowerCenter, 0.0)),
                    icon,
                    Some(header_strip(r)),
                    &header_tip(&lang, "", true),
                    &mut tips,
                );
                let b = Rect::new(
                    r.min.x,
                    r.min.y + (r.height() - 65.0),
                    r.min.x + r.width().min(360.0),
                    r.min.y + (r.height() - 65.0) + 32.0,
                );
                hits.name("sched:ManageAreas", b);
                // COMPATIBILITY TODO: currently approximate — areas are not
                // modelled; "Manage areas..." has no dialog.
                if button_text(&mut p, &mut input, b, &lang.tr("ManageAreas")) {
                    messages.add(
                        format!("{}: not available yet", lang.tr("ManageAreas")),
                        now,
                    );
                }
                clicked
            }
            _ => None,
        };
        // Shift + click on the allowed area header sets areas (none yet).
        if let Some(right) = clicked
            && !keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight])
        {
            tabs.schedule_sort.header_clicked(col.id(), right);
        }
    }

    // Rows.
    let mut action: Option<(PawnId, LabelClick)> = None;
    let paint_with = clip.paint;
    for (row, &id) in pawns.iter().enumerate().take(tw.visible_rows) {
        let name = cap(&pawn_label(&sim.0, id));
        for (i, &col) in COLUMNS.iter().enumerate() {
            let r = tw.cell(i, row);
            match col {
                Column::Label => {
                    if let Some(a) = label_cell(
                        &mut p,
                        &mut input,
                        &mut hits,
                        r,
                        &sim.0,
                        &lang,
                        &sel,
                        graphics.as_deref(),
                        id,
                        &mut tips,
                    ) {
                        action = Some((id, a));
                    }
                }
                Column::CopyPaste => match copy_paste_cell(
                    &mut p,
                    &mut input,
                    &mut hits,
                    &lang,
                    r,
                    clip.timetable.is_some(),
                    &format!("sched:{name}"),
                    &mut tips,
                ) {
                    Some(CopyPaste::Copy) => clip.timetable = Some(sim.0.timetable(id)),
                    Some(CopyPaste::Paste) => {
                        if let Some(t) = clip.timetable.clone() {
                            sim.0.set_timetable(id, &t);
                        }
                    }
                    None => {}
                },
                Column::Timetable => {
                    // `DoTimeAssignment`: each hour contracted by 1, outlined
                    // under the mouse, painted while the button is held.
                    let times = sim.0.timetable(id);
                    let w = r.width() / 24.0;
                    for (h, &a) in times.iter().enumerate().take(24) {
                        let x = r.min.x + h as f32 * w;
                        let cell = Rect::new(x + 1.0, r.min.y + 1.0, x + w - 1.0, r.max.y - 1.0);
                        hits.name(format!("sched:{name}:{h}"), cell);
                        p.rect(cell, look(a).1);
                        let clicked = input.clicked(cell);
                        if !input.hovered(cell) {
                            continue;
                        }
                        p.outline(cell, 2.0, Color::WHITE);
                        if (input.held || clicked) && a != paint_with {
                            sim.0.set_time_assignment(id, h, paint_with);
                        }
                    }
                }
                Column::AllowedArea => {
                    // `AreaAllowedGUI.DoAllowedAreaSelectors` with no areas:
                    // "Unrestricted", selected.
                    let b = Rect::new(r.min.x, r.min.y, r.max.x, r.max.y);
                    p.rect(b, Color::srgba(1.0, 1.0, 1.0, 0.1));
                    p.outline(b, 1.0, Color::srgba(1.0, 1.0, 1.0, 0.5));
                    p.label_at(
                        b,
                        lang.tr("NoAreaAllowed"),
                        FONT_TINY,
                        Color::WHITE,
                        Anchor::MiddleCenter,
                    );
                }
                _ => {}
            }
        }
    }
    row_overlays(&mut p, &input, &tw, &sim.0, &sel, &pawns);

    // `TimeAssignmentSelector.DrawTimeAssignmentSelectorGrid` over the
    // table's top left: half-width, half-height boxes in a row.
    let c = tw.content.min;
    let box_size = Vec2::new(SELECTOR.x / 2.0, (SELECTOR.y - 2.0) / 2.0);
    for (k, &a) in ASSIGNMENTS.iter().enumerate() {
        let x = c.x + k as f32 * box_size.x;
        let r = Rect::new(
            x + 2.0,
            c.y + 2.0,
            x + box_size.x - 2.0,
            c.y + box_size.y - 2.0,
        );
        let (label, color) = look(a);
        p.rect(r, color);
        hits.name(format!("assign:{}", cap(&label)), r);
        if input.clicked(r) {
            clip.paint = a;
        }
        if input.hovered(r) {
            p.highlight(r, 1.0);
        }
        p.label_at(
            r,
            cap(&label),
            FONT_SMALL,
            Color::WHITE,
            Anchor::MiddleCenter,
        );
        if clip.paint == a {
            p.outline(r, 2.0, Color::WHITE);
        }
    }

    if let Some((id, a)) = action {
        if let Ok(mut cam) = camera.single_mut() {
            jump_and_select(&sim.0, &mut sel, &mut cam, id, now);
        }
        if a == LabelClick::JumpAndClose {
            tabs.set(None, &mut mgr);
        }
    }
    if let (Some(t), Some(cur)) = (tips, input.cursor) {
        p.tip(cur, &t, screen);
    }
}
