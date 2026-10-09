//! The main tab windows (`MainTabsRoot`, `MainTabWindow`,
//! `MainTabWindow_PawnTable`, `PawnTable`, `PawnColumnWorker`;
//! docs/research.md §67).
//!
//! One main tab is open at a time: opening one closes the other, and the
//! inspect pane shows only while none is open. A pawn table tab is a window
//! at the bottom left, above the main buttons, sized to its table: the
//! columns' optimal widths plus 16 for the scrollbar (at least the table's
//! `minWidth`, at most the screen less the margins) by a header row as tall
//! as its tallest column header and 30 pixels per pawn, with 6 pixels of
//! margin, 53 below the table and the tab's own space above it.

use std::cmp::Ordering;

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_sim::rest::TimeAssignment;
use rimworld_sim::sim::ThingRef;
use rimworld_sim::{PawnId, Sim};

use super::colonist_bar::selection_brackets;
use super::designator::DesignatorManager;
use super::lang::{cap, pawn_label};
use super::select::{Selectable, Selection};
use super::{Anchor, FONT_SMALL, Lang, Painter, UiHits, UiInput, text_width, truncate};
use crate::graphics::{PawnGraphics, SKIN_TINT};

/// The main tabs the interface opens (`MainButtonDef`s with a window).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MainTab {
    Architect,
    Work,
    Schedule,
}

impl MainTab {
    pub fn from_def_name(name: &str) -> Option<Self> {
        Some(match name {
            "Architect" => MainTab::Architect,
            "Work" => MainTab::Work,
            "Schedule" => MainTab::Schedule,
            _ => return None,
        })
    }
}

/// `MainTabsRoot.OpenTab` (none: the inspect pane, when something is
/// selected) and the tables' sorting, kept while the game runs.
#[derive(Resource, Default)]
pub struct MainTabs {
    pub open: Option<MainTab>,
    pub work_sort: TableSort,
    pub schedule_sort: TableSort,
}

impl MainTabs {
    pub fn is_open(&self, t: MainTab) -> bool {
        self.open == Some(t)
    }

    /// `MainTabsRoot.ToggleTab`; closing the Architect drops its
    /// designator.
    pub fn toggle(&mut self, t: MainTab, mgr: &mut DesignatorManager) {
        let next = if self.open == Some(t) { None } else { Some(t) };
        self.set(next, mgr);
    }

    /// `MainTabsRoot.SetCurrentTab` / `EscapeCurrentTab`.
    pub fn set(&mut self, t: Option<MainTab>, mgr: &mut DesignatorManager) {
        if self.open == Some(MainTab::Architect) && t != Some(MainTab::Architect) {
            mgr.deselect();
        }
        self.open = t;
    }
}

/// The copy/paste clipboards (`PawnColumnWorker_CopyPasteWorkPriorities`,
/// `PawnColumnWorker_CopyPasteTimetable`) and the schedule's paint
/// (`TimeAssignmentSelector.selectedAssignment`, Work at first).
#[derive(Resource)]
pub struct Clipboards {
    pub work: Option<Vec<(String, i32)>>,
    pub timetable: Option<Vec<TimeAssignment>>,
    pub paint: TimeAssignment,
}

impl Default for Clipboards {
    fn default() -> Self {
        Self {
            work: None,
            timetable: None,
            paint: TimeAssignment::Work,
        }
    }
}

/// `PawnColumnWorker.GetMaxWidth`'s "no limit".
pub const NO_LIMIT: f32 = 1_000_000.0;
/// `PawnColumnWorker.DefaultCellHeight`.
pub const ROW_HEIGHT: f32 = 30.0;
/// `MainTabWindow_PawnTable.Margin` / `ExtraBottomSpace`.
const MARGIN: f32 = 6.0;
const EXTRA_BOTTOM: f32 = 53.0;
/// The vertical scrollbar the table always leaves room for.
const SCROLLBAR: f32 = 16.0;
/// `PawnTable.BorderColor`.
const BORDER: Color = Color::srgba(1.0, 1.0, 1.0, 0.2);

/// A column's widths and its share of spare room (`GetMinWidth`,
/// `GetOptimalWidth`, `GetMaxWidth`, `PawnColumnDef.widthPriority` and
/// `ignoreWhenCalculatingOptimalTableSize`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnSize {
    pub min: f32,
    pub optimal: f32,
    pub max: f32,
    pub priority: i32,
    /// Left out of the table's optimal width (it takes what is left).
    pub fills: bool,
}

impl ColumnSize {
    pub const fn new(min: f32, optimal: f32, max: f32) -> Self {
        Self {
            min,
            optimal,
            max,
            priority: 0,
            fills: false,
        }
    }

    /// A column exactly `w` wide (gaps, icons, copy/paste buttons).
    pub const fn fixed(w: f32) -> Self {
        Self::new(w, w, w)
    }

    /// `RemainingSpace`.
    pub const REMAINING: Self = Self {
        min: 0.0,
        optimal: NO_LIMIT,
        max: NO_LIMIT,
        priority: -1000,
        fills: true,
    };

    pub const fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// The optimal width within min and max, the minimum winning
    /// (`Mathf.Clamp`).
    fn optimal(&self) -> f32 {
        clamp(self.optimal, self.min, self.max).max(0.0)
    }
}

/// `Mathf.Clamp`: the minimum wins when the bounds cross.
fn clamp(v: f32, min: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// `PawnTable.RecacheColumnWidths`: every column starts at its minimum.
/// Too little room takes from every column in proportion to its width;
/// otherwise the room left goes to columns below their optimal width, the
/// highest width priority first, in proportion to their optimal widths,
/// then (all optimal) towards each column's maximum in proportion to
/// its optimal width (at least 1), and any rest past the maximums the
/// same way.
pub fn column_widths(cols: &[ColumnSize], space: f32) -> Vec<f32> {
    let mut w: Vec<f32> = cols.iter().map(|c| c.min.max(0.0)).collect();
    let mut used: f32 = w.iter().sum();
    if used >= space {
        if used > space {
            let over = used - space;
            for x in &mut w {
                *x -= over * *x / used;
            }
        }
        return w;
    }
    // Up to optimal, tier by tier.
    let mut done: Vec<bool> = cols
        .iter()
        .zip(&w)
        .map(|(c, &x)| x >= c.optimal())
        .collect();
    while let Some(tier) = cols
        .iter()
        .zip(&done)
        .filter(|(_, d)| !**d)
        .map(|(c, _)| c.priority)
        .max()
    {
        let total: f32 = cols
            .iter()
            .zip(&done)
            .filter(|(c, d)| !**d && c.priority == tier)
            .map(|(c, _)| c.optimal())
            .sum();
        let free = space - used;
        let (mut waiting, mut capped) = (false, false);
        for (i, c) in cols.iter().enumerate() {
            if done[i] {
                continue;
            }
            if c.priority != tier {
                waiting = true;
                continue;
            }
            let mut add = if total > 0.0 {
                free * c.optimal() / total
            } else {
                0.0
            };
            let need = c.optimal() - w[i];
            if add >= need {
                add = need;
                done[i] = true;
                capped = true;
            } else {
                waiting = true;
            }
            if add > 0.0 {
                w[i] += add;
                used += add;
            }
        }
        if used >= space - 0.1 {
            return w;
        }
        if !(waiting && capped) {
            break;
        }
    }
    // Above optimal, towards the maximums.
    let share = |c: &ColumnSize| c.optimal().max(1.0);
    let mut at_max: Vec<bool> = cols.iter().zip(&w).map(|(c, &x)| x >= c.max).collect();
    loop {
        let total: f32 = cols
            .iter()
            .zip(&at_max)
            .filter(|(_, m)| !**m)
            .map(|(c, _)| share(c))
            .sum();
        let free = space - used;
        let mut growing = false;
        for (i, c) in cols.iter().enumerate() {
            if at_max[i] {
                continue;
            }
            let mut add = free * share(c) / total;
            let room = c.max - w[i];
            if add >= room {
                add = room;
                at_max[i] = true;
            } else {
                growing = true;
            }
            if add > 0.0 {
                w[i] += add;
                used += add;
            }
        }
        if used >= space - 0.1 {
            break;
        }
        if !growing {
            let rest = space - used;
            let total: f32 = cols.iter().map(share).sum();
            for (i, c) in cols.iter().enumerate() {
                w[i] += rest * share(c) / total;
            }
            break;
        }
    }
    w
}

/// A pawn table window's geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct TableWindow {
    pub window: Rect,
    /// Inside the window's margin (the tab's own controls go here).
    pub content: Rect,
    /// The table: header row and rows.
    pub table: Rect,
    pub header_h: f32,
    /// Each column's left edge and width (whole pixels; the last takes
    /// what is left of the table less the scrollbar).
    pub columns: Vec<(f32, f32)>,
    /// Rows that fit.
    pub visible_rows: usize,
}

/// `MainTabWindow_PawnTable.RequestedTabSize`, `MainTabWindow.
/// SetInitialSizeAndPosition` and `PawnTable.RecacheSize`.
pub fn table_window(
    cols: &[ColumnSize],
    header_h: f32,
    rows: usize,
    min_width: f32,
    extra_top: f32,
    screen: Vec2,
) -> TableWindow {
    let max_w = (screen.x - MARGIN * 2.0).floor();
    let max_h = (screen.y - 35.0 - EXTRA_BOTTOM - extra_top - MARGIN * 2.0).floor();
    let optimal: f32 = cols
        .iter()
        .filter(|c| !c.fills)
        .map(ColumnSize::optimal)
        .sum();
    let w = clamp(optimal + SCROLLBAR, min_width, max_w).min(screen.x);
    let h = clamp(header_h + rows as f32 * ROW_HEIGHT, 0.0, max_h).min(screen.y);
    let size = Vec2::new(
        (w + MARGIN * 2.0).min(screen.x),
        (h + EXTRA_BOTTOM + extra_top + MARGIN * 2.0).min(screen.y - 35.0),
    );
    let window = Rect::new(0.0, screen.y - 35.0 - size.y, size.x, screen.y - 35.0);
    let content = Rect::new(
        window.min.x + MARGIN,
        window.min.y + MARGIN,
        window.max.x - MARGIN,
        window.max.y - MARGIN,
    );
    let table = Rect::new(
        content.min.x,
        content.min.y + extra_top,
        content.min.x + w,
        content.min.y + extra_top + h,
    );
    let space = w - SCROLLBAR;
    let widths = column_widths(cols, space);
    let mut columns = Vec::with_capacity(widths.len());
    let mut x = 0.0;
    for (i, &cw) in widths.iter().enumerate() {
        let cw = if i + 1 == widths.len() {
            space - x
        } else {
            cw.floor()
        };
        columns.push((table.min.x + x, cw));
        x += cw;
    }
    let visible_rows = ((h - header_h) / ROW_HEIGHT).floor().max(0.0) as usize;
    TableWindow {
        window,
        content,
        table,
        header_h,
        columns,
        visible_rows,
    }
}

impl TableWindow {
    /// A column's header rect.
    pub fn header(&self, col: usize) -> Rect {
        let (x, w) = self.columns[col];
        Rect::new(x, self.table.min.y, x + w, self.table.min.y + self.header_h)
    }

    /// A column's cell in a row.
    pub fn cell(&self, col: usize, row: usize) -> Rect {
        let (x, w) = self.columns[col];
        let y = self.table.min.y + self.header_h + row as f32 * ROW_HEIGHT;
        Rect::new(x, y, x + w, y + ROW_HEIGHT)
    }

    /// A whole row (less the scrollbar).
    pub fn row(&self, row: usize) -> Rect {
        let y = self.table.min.y + self.header_h + row as f32 * ROW_HEIGHT;
        Rect::new(
            self.table.min.x,
            y,
            self.table.max.x - SCROLLBAR,
            y + ROW_HEIGHT,
        )
    }
}

/// `PawnTable.SortingBy` / `SortingDescending`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableSort {
    pub column: Option<String>,
    pub descending: bool,
}

impl TableSort {
    /// `PawnColumnWorker.HeaderClicked` without Shift: a left click goes
    /// unsorted → descending → ascending → unsorted, a right click
    /// unsorted → ascending → descending → unsorted.
    pub fn header_clicked(&mut self, column: &str, right: bool) {
        let this = self.column.as_deref() == Some(column);
        *self = match (right, this, self.descending) {
            (false, false, _) => Self::by(column, true),
            (false, true, true) => Self::by(column, false),
            (false, true, false) => Self::default(),
            (true, false, _) => Self::by(column, false),
            (true, true, true) => Self::default(),
            (true, true, false) => Self::by(column, true),
        };
    }

    fn by(column: &str, descending: bool) -> Self {
        Self {
            column: Some(column.to_owned()),
            descending,
        }
    }

    /// The sorting icon a column's header shows, if sorted by it.
    pub fn icon(&self, column: &str) -> Option<&'static str> {
        (self.column.as_deref() == Some(column)).then_some(if self.descending {
            "UI/Icons/SortingDescending"
        } else {
            "UI/Icons/Sorting"
        })
    }
}

/// `PawnTable.RecachePawns` after the label order: a stable sort by the
/// sorting column's comparison — as the game sorts, "descending" in its
/// comparison's own order and ascending reversed.
pub fn sort_rows(
    pawns: &mut [PawnId],
    sort: &TableSort,
    compare: impl Fn(&str, PawnId, PawnId) -> Ordering,
) {
    let Some(col) = sort.column.as_deref() else {
        return;
    };
    if sort.descending {
        pawns.sort_by(|&a, &b| compare(col, a, b));
    } else {
        pawns.sort_by(|&a, &b| compare(col, b, a));
    }
}

/// `MapPawns.FreeColonists` in `PlayerPawnsDisplayOrderUtility` order.
// COMPATIBILITY TODO: currently approximate — display order is spawn
// order (the colonist bar can't reorder yet); babies don't exist.
pub fn free_colonists(sim: &Sim) -> Vec<PawnId> {
    sim.pawns()
        .iter()
        .filter(|p| p.is_colonist && !p.health.dead)
        .map(|p| p.id)
        .collect()
}

/// `PawnColumnWorker.DoHeader`'s label (the column def's label, in the
/// small font, by the column's anchor), the sorting icon and, for a
/// sortable or tipped column, the interactable strip along the bottom
/// (at most 25 high): highlight and tip on hover. Returns a click there
/// (right button: true).
#[allow(clippy::too_many_arguments)]
pub fn column_header(
    p: &mut Painter,
    input: &mut UiInput,
    r: Rect,
    label: Option<(&str, Anchor, f32)>,
    sort_icon: Option<&str>,
    interact: Option<Rect>,
    tip: &str,
    tips: &mut Option<String>,
) -> Option<bool> {
    if let Some((text, anchor, offset)) = label {
        let lr = Rect::new(r.min.x + offset, r.min.y, r.max.x, r.max.y);
        p.label_at(
            lr,
            truncate(text, r.width(), FONT_SMALL),
            FONT_SMALL,
            Color::WHITE,
            anchor,
        );
    }
    if let Some(icon) = sort_icon
        && let Some(t) = p.texture(icon)
    {
        let s = t.size.as_vec2();
        p.image(
            Rect::new(
                r.max.x - s.x - 1.0,
                r.max.y - s.y - 1.0,
                r.max.x - 1.0,
                r.max.y - 1.0,
            ),
            t.image,
            Color::WHITE,
        );
    }
    let ir = interact?;
    if input.hovered(ir) {
        p.highlight(ir, 1.0);
        if !tip.is_empty() {
            *tips = Some(tip.to_owned());
        }
    }
    if input.clicked(ir) {
        return Some(false);
    }
    if input.right_clicked(ir) {
        return Some(true);
    }
    None
}

/// The default interactable header strip: the bottom 25 pixels.
pub fn header_strip(r: Rect) -> Rect {
    let h = r.height().min(25.0);
    Rect::new(r.min.x, r.max.y - h, r.max.x, r.max.y)
}

/// `PawnColumnWorker.GetHeaderTip`: the column's tip and, if sortable,
/// "Click to sort by this column".
pub fn header_tip(lang: &Lang, tip: &str, sortable: bool) -> String {
    let mut s = tip.to_owned();
    if sortable {
        if !s.is_empty() {
            s += "\n\n";
        }
        s += &lang.tr("ClickToSortByThisColumn");
    }
    s
}

/// The table's row lines and its per-row overlays
/// (`PawnTable.PawnTableOnGUI`): a border line above each row, the
/// selected rows' highlight at 0.6, the hovered row's highlight, and a red
/// line through downed pawns.
// COMPATIBILITY TODO: currently approximate — the hovered row's pawn is
// not marked on the map (`LookTargets.Highlight`); rows past the window
// are not drawn (no scroll view).
pub fn row_overlays(
    p: &mut Painter,
    input: &UiInput,
    tw: &TableWindow,
    sim: &Sim,
    sel: &Selection,
    pawns: &[PawnId],
) {
    for (row, &id) in pawns.iter().enumerate().take(tw.visible_rows) {
        let r = tw.row(row);
        p.rect(Rect::new(r.min.x, r.min.y, r.max.x, r.min.y + 1.0), BORDER);
        if sel.contains(Selectable::Thing(ThingRef::Pawn(id))) {
            p.highlight(r, 0.6);
        }
        if input.hovered(r) {
            p.highlight(r, 1.0);
        }
        if sim.pawn(id).is_some_and(|q| q.health.downed) {
            let y = r.center().y.floor();
            p.rect(
                Rect::new(r.min.x, y, r.max.x, y + 1.0),
                Color::srgba(1.0, 0.0, 0.0, 0.5),
            );
        }
    }
}

/// What a label cell's click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelClick {
    /// Left: jump, select and close the tab.
    JumpAndClose,
    /// Right: jump and select.
    Jump,
}

/// `PawnColumnWorker_Label.DoCell` with its icon: the pawn's icon (with
/// selection brackets when selected) in the row's first 30 pixels, a red
/// health bar behind the name below 99 %, the name truncated to fit, a
/// highlight on hover with the "Click to jump to" tip.
#[allow(clippy::too_many_arguments)]
pub fn label_cell(
    p: &mut Painter,
    input: &mut UiInput,
    hits: &mut UiHits,
    r: Rect,
    sim: &Sim,
    lang: &Lang,
    sel: &Selection,
    graphics: Option<&PawnGraphics>,
    id: PawnId,
    tips: &mut Option<String>,
) -> Option<LabelClick> {
    let r2 = Rect::new(
        r.min.x,
        r.min.y,
        r.max.x,
        r.min.y + r.height().min(ROW_HEIGHT),
    );
    let icon = Rect::new(r2.min.x, r2.min.y, r2.min.x + r2.height(), r2.max.y);
    let text_r = Rect::new(r2.min.x + 3.0 + r2.height(), r2.min.y, r2.max.x, r2.max.y);
    if sel.contains(Selectable::Thing(ThingRef::Pawn(id))) {
        selection_brackets(
            p,
            Rect::new(
                icon.min.x + 2.0,
                icon.min.y + 2.0,
                icon.max.x - 2.0,
                icon.max.y - 2.0,
            ),
            1.0,
        );
    }
    pawn_icon(p, graphics, icon);
    let health = sim
        .health_view(id)
        .map_or(1.0, |v| v.summary_health_percent());
    if health < 0.99 {
        // `Widgets.FillableBar` with `GenMapUI.OverlayHealthTex`.
        let bar = Rect::new(
            text_r.min.x - 3.0,
            text_r.min.y + 4.0,
            text_r.max.x,
            text_r.max.y - 6.0,
        );
        p.rect(
            Rect::new(
                bar.min.x,
                bar.min.y,
                bar.min.x + bar.width() * health,
                bar.max.y,
            ),
            Color::srgba(1.0, 0.0, 0.0, 0.25),
        );
    }
    if input.hovered(r2) {
        p.highlight(r2, 1.0);
    }
    let name = cap(&pawn_label(sim, id));
    p.label_at(
        text_r,
        truncate(&name, text_r.width(), FONT_SMALL),
        FONT_SMALL,
        Color::WHITE,
        Anchor::MiddleLeft,
    );
    hits.name(format!("tablelabel:{name}"), r2);
    if input.clicked(r2) {
        return Some(LabelClick::JumpAndClose);
    }
    if input.right_clicked(r2) {
        return Some(LabelClick::Jump);
    }
    if input.hovered(r2) {
        // COMPATIBILITY TODO: currently approximate — `Pawn.GetTooltip`
        // is the pawn's label alone here.
        *tips = Some(format!("{}\n\n{name}", lang.tr("ClickToJumpTo")));
    }
    None
}

/// `Widgets.ThingIcon` for a pawn: its south-facing body and head.
// COMPATIBILITY TODO: currently approximate — the shared body and head
// textures stand in for the pawn's own rendered graphic.
pub fn pawn_icon(p: &mut Painter, graphics: Option<&PawnGraphics>, r: Rect) {
    let Some(g) = graphics else { return };
    let s = r.width() * 1.1;
    let body = Rect::from_center_size(
        r.center() + Vec2::new(0.0, r.height() * 0.12),
        Vec2::splat(s),
    );
    p.image(body, g.body["south"].clone(), SKIN_TINT);
    let head = Rect::from_center_size(body.center() - Vec2::new(0.0, s * 0.23), Vec2::splat(s));
    p.image(head, g.head["south"].clone(), SKIN_TINT);
}

/// `Widgets.ButtonImage`: the texture, tinted `GenUI.MouseoverColor`
/// under the mouse.
pub fn button_image(p: &mut Painter, input: &mut UiInput, r: Rect, path: &str) -> bool {
    let color = if input.hovered(r) {
        Color::srgb(0.3, 0.7, 0.9)
    } else {
        Color::WHITE
    };
    p.tex(r, path, color);
    input.clicked(r)
}

/// What a copy/paste cell's buttons did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyPaste {
    Copy,
    Paste,
}

/// `CopyPasteUI.DoCopyPasteButtons` in a row's first 36 pixels: the copy
/// button (18×24, centred vertically) and, with something copied, the
/// paste button after it.
#[allow(clippy::too_many_arguments)]
pub fn copy_paste_cell(
    p: &mut Painter,
    input: &mut UiInput,
    hits: &mut UiHits,
    lang: &Lang,
    r: Rect,
    can_paste: bool,
    name: &str,
    tips: &mut Option<String>,
) -> Option<CopyPaste> {
    let y = r.min.y + (ROW_HEIGHT / 2.0 - 12.0);
    let copy = Rect::new(r.min.x, y, r.min.x + 18.0, y + 24.0);
    let mut out = None;
    hits.name(format!("copy:{name}"), copy);
    if button_image(p, input, copy, "UI/Buttons/Copy") {
        out = Some(CopyPaste::Copy);
    }
    if input.hovered(copy) {
        *tips = Some(lang.tr("Copy"));
    }
    if can_paste {
        let paste = Rect::new(copy.max.x, y, copy.max.x + 18.0, y + 24.0);
        hits.name(format!("paste:{name}"), paste);
        if button_image(p, input, paste, "UI/Buttons/Paste") {
            out = Some(CopyPaste::Paste);
        }
        if input.hovered(paste) {
            *tips = Some(lang.tr("Paste"));
        }
    }
    out
}

/// `Widgets.CheckboxLabeled`: the label at the left (middle), a 24-pixel
/// checkbox at the right; a click anywhere toggles. Returns whether it was
/// toggled.
pub fn checkbox_labeled(
    p: &mut Painter,
    input: &mut UiInput,
    r: Rect,
    label: &str,
    on: bool,
) -> bool {
    p.label_at(
        Rect::new(r.min.x, r.min.y, r.max.x - 24.0, r.max.y),
        label,
        FONT_SMALL,
        Color::WHITE,
        Anchor::MiddleLeft,
    );
    let y = r.min.y + (r.height() - 24.0) / 2.0;
    let boxr = Rect::new(r.max.x - 24.0, y, r.max.x, y + 24.0);
    p.tex(
        boxr,
        if on {
            "UI/Widgets/CheckOn"
        } else {
            "UI/Widgets/CheckOff"
        },
        Color::WHITE,
    );
    input.clicked(r)
}

/// `Widgets.ButtonText`: the button atlas (lighter under the mouse) with
/// the label centred.
pub fn button_text(p: &mut Painter, input: &mut UiInput, r: Rect, label: &str) -> bool {
    let tex = if input.hovered(r) {
        "UI/Widgets/ButtonBGMouseover"
    } else {
        "UI/Widgets/ButtonBG"
    };
    p.atlas(r, tex, Color::WHITE);
    p.label_at(
        r,
        truncate(label, r.width(), FONT_SMALL),
        FONT_SMALL,
        Color::WHITE,
        Anchor::MiddleCenter,
    );
    input.clicked(r)
}

/// `CameraJumper.TryJumpAndSelect`: centres the camera on the pawn and
/// selects it alone.
pub fn jump_and_select(
    sim: &Sim,
    sel: &mut Selection,
    camera: &mut Transform,
    id: PawnId,
    now: f32,
) {
    if let Some(pawn) = sim.pawn(id) {
        let pos = crate::view::sim_to_world(pawn.visual_position());
        camera.translation.x = pos.x;
        camera.translation.y = pos.y;
    }
    sel.clear();
    sel.select(Selectable::Thing(ThingRef::Pawn(id)), now);
}

/// The width a label needs in the small font, rounded up
/// (`Text.CalcSize`).
pub fn label_width(text: &str) -> f32 {
    text_width(text, FONT_SMALL).ceil()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(w: &[f32]) -> f32 {
        w.iter().sum()
    }

    #[test]
    fn widths_reach_optimal_then_remaining_space_takes_the_rest() {
        let cols = [
            ColumnSize::new(80.0, 165.0, NO_LIMIT).with_priority(100),
            ColumnSize::fixed(26.0),
            ColumnSize::fixed(36.0),
            ColumnSize::new(32.0, 39.0, 80.0),
            ColumnSize::new(32.0, 39.0, 80.0),
            ColumnSize::REMAINING,
        ];
        let w = column_widths(&cols, 1000.0);
        assert_eq!(&w[..5], &[165.0, 26.0, 36.0, 39.0, 39.0]);
        assert!((total(&w) - 1000.0).abs() < 0.2);
        // Room for minimums only: the highest priority grows first.
        let w = column_widths(&cols, 80.0 + 26.0 + 36.0 + 64.0 + 50.0);
        assert_eq!(w[0], 130.0);
        assert_eq!(w[3], 32.0);
    }

    #[test]
    fn too_little_room_shrinks_every_column_in_proportion() {
        let cols = [ColumnSize::fixed(100.0), ColumnSize::fixed(50.0)];
        let w = column_widths(&cols, 120.0);
        assert!((w[0] - 80.0).abs() < 1e-4 && (w[1] - 40.0).abs() < 1e-4);
    }

    #[test]
    fn spare_room_goes_past_optimal_towards_maximums() {
        let cols = [
            ColumnSize::new(32.0, 39.0, 80.0),
            ColumnSize::new(32.0, 39.0, 80.0),
        ];
        let w = column_widths(&cols, 120.0);
        assert_eq!(w, vec![60.0, 60.0]);
        // Past every maximum, the rest is shared by optimal width.
        let w = column_widths(&cols, 200.0);
        assert_eq!(w, vec![100.0, 100.0]);
    }

    #[test]
    fn the_work_window_sits_above_the_main_buttons() {
        let mut cols = vec![
            ColumnSize::new(80.0, 165.0, NO_LIMIT).with_priority(100),
            ColumnSize::fixed(26.0),
            ColumnSize::fixed(36.0),
        ];
        cols.extend(std::iter::repeat_n(ColumnSize::new(32.0, 39.0, 80.0), 20));
        cols.push(ColumnSize::REMAINING);
        let tw = table_window(&cols, 50.0, 3, 998.0, 40.0, Vec2::new(1920.0, 1080.0));
        // 165 + 26 + 36 + 20 × 39 + 16 = 1023 wide; 50 + 3 × 30 high.
        assert_eq!(tw.table.width(), 1023.0);
        assert_eq!(tw.table.height(), 140.0);
        assert_eq!(tw.window.width(), 1035.0);
        assert_eq!(tw.window.height(), 140.0 + 53.0 + 40.0 + 12.0);
        assert_eq!(tw.window.max.y, 1080.0 - 35.0);
        assert_eq!(tw.window.min.x, 0.0);
        assert_eq!(tw.table.min, Vec2::new(6.0, tw.window.min.y + 6.0 + 40.0));
        assert_eq!(tw.columns[0], (6.0, 165.0));
        let last = tw.columns.last().unwrap();
        assert_eq!(last.0 + last.1, 6.0 + 1023.0 - 16.0);
        // A small table is held to the def's minimum width.
        let tw = table_window(&cols[..3], 30.0, 1, 998.0, 0.0, Vec2::new(1920.0, 1080.0));
        assert_eq!(tw.table.width(), 998.0);
    }

    #[test]
    fn header_clicks_cycle_the_sorting() {
        let mut s = TableSort::default();
        s.header_clicked("a", false);
        assert_eq!(s, TableSort::by("a", true));
        s.header_clicked("a", false);
        assert_eq!(s, TableSort::by("a", false));
        s.header_clicked("a", false);
        assert_eq!(s, TableSort::default());
        s.header_clicked("a", true);
        assert_eq!(s, TableSort::by("a", false));
        s.header_clicked("a", true);
        assert_eq!(s, TableSort::by("a", true));
        s.header_clicked("a", true);
        assert_eq!(s, TableSort::default());
        s.header_clicked("a", false);
        s.header_clicked("b", true);
        assert_eq!(s, TableSort::by("b", false));
    }
}
