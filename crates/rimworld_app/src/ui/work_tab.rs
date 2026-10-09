//! The Work tab (`MainTabWindow_Work`, the `Work` `PawnTableDef`,
//! `PawnColumnWorker_WorkPriority`, `WidgetsWork`; docs/research.md §67).
//!
//! The free colonists, one row each: name (with icon), faction, copy/paste,
//! then a column per visible work type by descending `naturalPriority`,
//! their short labels alternately raised and lowered (the rightmost one
//! lowered). A work box's background blends from awful to excellent by the
//! average of the relevant skills; with manual priorities it shows the
//! number (a left click raises it, a right click lowers it, wrapping
//! through off), without them a check (a click toggles off / 3). Above the
//! table: the "Manual priorities" checkbox and the priority arrows.

use std::cmp::Ordering;

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_defs::{DefId, GameDefs, WorkTypeDef};
use rimworld_sim::stats::Passion;
use rimworld_sim::{PawnId, Sim};

use super::designator::DesignatorManager;
use super::lang::{cap, pawn_label};
use super::select::Selection;
use super::tabs::{
    Clipboards, ColumnSize, CopyPaste, LabelClick, MainTab, MainTabs, NO_LIMIT, TableWindow,
    checkbox_labeled, column_header, copy_paste_cell, free_colonists, header_strip, header_tip,
    jump_and_select, label_cell, label_width, row_overlays, sort_rows, table_window,
};
use super::{
    Anchor, FONT_MEDIUM, FONT_SMALL, FONT_TINY, Lang, Paint, Painter, UiHits, UiInput, layer,
};
use crate::SimState;
use crate::graphics::PawnGraphics;

/// `MainTabWindow_Work.ExtraTopSpace`.
const EXTRA_TOP: f32 = 40.0;
/// `PawnColumnWorker_WorkPriority.LabelRowHeight`.
const HEADER_HEIGHT: f32 = 50.0;
/// `WidgetsWork.WorkBoxSize`.
const BOX: f32 = 25.0;
/// The Work `PawnTableDef`'s `minWidth` (the default).
const MIN_TABLE_WIDTH: f32 = 998.0;

/// The table's columns.
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    /// `LabelWithIcon`.
    Label,
    /// `Faction`: an icon for pawns from other factions.
    Faction,
    CopyPaste,
    /// `WorkPriority_<type>`; `down`: `moveWorkTypeLabelDown`.
    Work {
        t: DefId<WorkTypeDef>,
        down: bool,
    },
    RemainingSpace,
}

impl Column {
    /// Its id for sorting.
    pub fn id(&self, defs: &GameDefs) -> String {
        match self {
            Column::Label => "LabelWithIcon".into(),
            Column::Faction => "Faction".into(),
            Column::CopyPaste => "CopyPasteWorkPriorities".into(),
            Column::Work { t, .. } => format!("WorkPriority_{}", defs.work_types[*t].def_name),
            Column::RemainingSpace => "RemainingSpace".into(),
        }
    }

    fn size(&self) -> ColumnSize {
        match self {
            Column::Label => ColumnSize::new(80.0, 165.0, NO_LIMIT).with_priority(100),
            Column::Faction => ColumnSize::fixed(26.0),
            Column::CopyPaste => ColumnSize::fixed(36.0),
            Column::Work { .. } => ColumnSize::new(32.0, 39.0, 80.0),
            Column::RemainingSpace => ColumnSize::REMAINING,
        }
    }
}

/// `WorkTypeDefsUtility.WorkTypeDefsInPriorityOrder`: by descending
/// `naturalPriority`, ties in database order.
pub fn work_types_in_priority_order(defs: &GameDefs) -> Vec<DefId<WorkTypeDef>> {
    let mut v: Vec<DefId<WorkTypeDef>> = defs.work_type_ids().collect();
    v.sort_by_key(|&t| std::cmp::Reverse(defs.work_types[t].natural_priority));
    v
}

/// The Work table's columns (`PawnColumnDefGenerator`: a column per
/// visible work type after the copy/paste column, the lowest priority's
/// label lowered and alternating from there).
pub fn columns(defs: &GameDefs) -> Vec<Column> {
    let types: Vec<DefId<WorkTypeDef>> = work_types_in_priority_order(defs)
        .into_iter()
        .filter(|&t| defs.work_types[t].visible)
        .collect();
    let n = types.len();
    let mut cols = vec![Column::Label, Column::Faction, Column::CopyPaste];
    cols.extend(types.into_iter().enumerate().map(|(i, t)| Column::Work {
        t,
        down: (n - 1 - i).is_multiple_of(2),
    }));
    cols.push(Column::RemainingSpace);
    cols
}

/// `WidgetsWork.ColorOfPriority`.
pub fn priority_color(p: i32) -> Color {
    match p {
        1 => Color::srgb(0.0, 1.0, 0.0),
        2 => Color::srgb(1.0, 0.9, 0.5),
        3 => Color::srgb(0.8, 0.7, 0.5),
        4 => Color::srgb(0.74, 0.74, 0.74),
        _ => Color::srgb(0.5, 0.5, 0.5),
    }
}

/// `DrawWorkBoxBackground`'s two textures and the second's opacity.
pub fn box_background(average: f32) -> (&'static str, &'static str, f32) {
    if average < 4.0 {
        (
            "UI/Widgets/WorkBoxBG_Awful",
            "UI/Widgets/WorkBoxBG_Bad",
            average / 4.0,
        )
    } else if average <= 14.0 {
        (
            "UI/Widgets/WorkBoxBG_Bad",
            "UI/Widgets/WorkBoxBG_Mid",
            (average - 4.0) / 10.0,
        )
    } else {
        (
            "UI/Widgets/WorkBoxBG_Mid",
            "UI/Widgets/WorkBoxBG_Excellent",
            (average - 14.0) / 6.0,
        )
    }
}

/// A click on a work box: the new priority (`DrawWorkBoxFor`). Manual: a
/// left click one step more urgent (off → 4), a right click one less
/// (4 → off). Otherwise a toggle between off and 3.
pub fn clicked_priority(current: i32, manual: bool, right: bool) -> i32 {
    if manual {
        if right {
            if current + 1 > 4 { 0 } else { current + 1 }
        } else if current - 1 < 0 {
            4
        } else {
            current - 1
        }
    } else if current > 0 {
        0
    } else {
        3
    }
}

/// Shift + a click on a work column's header, for one pawn
/// (`HeaderClicked`): manual, a left click raises (unless already 1), a
/// right click lowers (unless off); otherwise a left click enables (3) and
/// a right click disables. `None`: unchanged.
pub fn shift_header_priority(current: i32, manual: bool, right: bool) -> Option<i32> {
    if manual {
        if !right && current != 1 {
            return Some(if current - 1 < 0 { 4 } else { current - 1 });
        }
        if right && current != 0 {
            return Some(if current + 1 > 4 { 0 } else { current + 1 });
        }
        None
    } else if current > 0 {
        right.then_some(0)
    } else {
        (!right).then_some(3)
    }
}

/// `SkillDef.skillLabel` from the Def data.
fn skill_label(defs: &GameDefs, skill: &str) -> String {
    defs.raw
        .table("SkillDef")
        .and_then(|t| t.iter().find(|d| d.def_name == skill))
        .and_then(|d| {
            d.node
                .child_text("skillLabel")
                .or_else(|| d.node.child_text("label"))
        })
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| skill.to_owned())
}

/// `WidgetsWork.TipForPawnWorker`.
fn cell_tip(sim: &Sim, lang: &Lang, id: PawnId, t: DefId<WorkTypeDef>, incapable: bool) -> String {
    let wt = &sim.defs.work_types[t];
    let prio = sim.work_priority(id, t).unwrap_or(0);
    let mut s = format!(
        "{}: {}\n",
        cap(&wt.gerund_label),
        lang.tr(&format!("Priority{prio}"))
    );
    if sim.work_type_disabled(id, t) {
        s += &lang.tr_args("CannotDoThisWork", &["", &pawn_label(sim, id)]);
        return s;
    }
    let avg = sim.average_relevant_skill(id, t);
    if !wt.relevant_skills.is_empty() {
        let names: Vec<String> = wt
            .relevant_skills
            .iter()
            .map(|k| cap(&skill_label(&sim.defs, k)))
            .collect();
        s += &lang.tr_args(
            "RelevantSkills",
            &[&names.join(", "), &one_decimal(avg), "20"],
        );
        s += "\n";
    }
    if !wt.relevant_skills.is_empty() && avg <= 2.0 && prio > 0 {
        s += "\n";
        s += &lang.tr("SelectedWorkTypeWithVeryBadSkill");
        s += "\n";
    }
    s += "\n";
    s += &wt.description;
    if incapable {
        s += "\n\n";
        s += &lang.tr("IncapableOfWorkTypeBecauseOfCapacities");
    }
    s
}

/// `float.ToString("0.#")`.
fn one_decimal(v: f32) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r:.1}")
    }
}

/// `PawnColumnWorker_WorkPriority.GetHeaderTip`.
fn header_tip_for(sim: &Sim, lang: &Lang, t: DefId<WorkTypeDef>) -> String {
    let wt = &sim.defs.work_types[t];
    let givers: Vec<String> = wt
        .givers_by_priority
        .iter()
        .map(|&g| {
            let d = &sim.defs.work_givers[g];
            let mut l = format!(" - {}", cap(&d.label));
            if d.emergency {
                l += &format!(" ({})", lang.tr("EmergencyWorkMarker"));
            }
            l
        })
        .collect();
    let mut s = format!(
        "{}\n\n{}\n\n{}\n",
        cap(&wt.gerund_label),
        wt.description,
        givers.join("\n")
    );
    s += &format!("\n{}", lang.tr("ClickToSortByThisColumn"));
    s += &format!(
        "\n{}",
        lang.tr(if sim.use_work_priorities {
            "WorkPriorityShiftClickTip"
        } else {
            "WorkPriorityShiftClickEnableDisableTip"
        })
    );
    s
}

/// The columns' comparisons (`Compare`): names by ordinal order, work by
/// the relevant skills' average (−2 without work settings, −1 disabled).
fn compare(sim: &Sim, cols: &[Column], col: &str, a: PawnId, b: PawnId) -> Ordering {
    let Some(c) = cols.iter().find(|c| c.id(&sim.defs) == col) else {
        return Ordering::Equal;
    };
    match c {
        Column::Label => cap(&pawn_label(sim, a)).cmp(&cap(&pawn_label(sim, b))),
        Column::Work { t, .. } => {
            let v = |p: PawnId| {
                if !sim.ever_works(p) {
                    -2.0
                } else if sim.work_type_disabled(p, *t) {
                    -1.0
                } else {
                    sim.average_relevant_skill(p, *t)
                }
            };
            v(a).total_cmp(&v(b))
        }
        _ => Ordering::Equal,
    }
}

/// The Work tab's rows, in table order.
pub fn rows(sim: &Sim, tabs: &MainTabs) -> Vec<PawnId> {
    let cols = columns(&sim.defs);
    let mut pawns = free_colonists(sim);
    sort_rows(&mut pawns, &tabs.work_sort, |c, a, b| {
        compare(sim, &cols, c, a, b)
    });
    pawns
}

/// The Work tab window's geometry for the current colony.
pub fn window(sim: &Sim, screen: Vec2) -> (Vec<Column>, TableWindow) {
    let cols = columns(&sim.defs);
    let sizes: Vec<ColumnSize> = cols
        .iter()
        .map(|c| {
            let mut s = c.size();
            if *c == Column::Label {
                s.min = s.min.max(label_width("Name"));
            }
            s
        })
        .collect();
    let rows = free_colonists(sim).len();
    let tw = table_window(
        &sizes,
        HEADER_HEIGHT,
        rows,
        MIN_TABLE_WIDTH,
        EXTRA_TOP,
        screen,
    );
    (cols, tw)
}

#[allow(clippy::too_many_arguments)]
pub fn work_tab_ui(
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
    mut camera: Query<&mut Transform, With<Camera2d>>,
    time: Res<Time>,
) {
    if !tabs.is_open(MainTab::Work) {
        return;
    }
    // `Window.closeOnCancel`.
    if keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
        tabs.set(None, &mut mgr);
        return;
    }
    let now = time.elapsed_secs();
    let screen = super::screen(&window_q);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let (cols, tw) = window(&sim.0, screen);
    let pawns = rows(&sim.0, &tabs);
    hits.name("tab:Work", tw.window);
    input.absorb(tw.window);
    let mut p = paint.painter(layer::MAIN_TAB);
    super::inspect::window_background(&mut p, tw.window);
    let mut tips: Option<String> = None;
    let manual = sim.0.use_work_priorities;

    // "Manual priorities" (`DoManualPrioritiesCheckbox`).
    let c = tw.content.min;
    let check = Rect::new(c.x + 5.0, c.y + 5.0, c.x + 145.0, c.y + 35.0);
    hits.name("work:manual", check);
    if checkbox_labeled(
        &mut p,
        &mut input,
        check,
        &lang.tr("ManualPriorities"),
        manual,
    ) {
        sim.0.set_use_work_priorities(!manual);
    }
    if sim.0.use_work_priorities {
        p.text(
            Rect::new(
                check.min.x,
                check.max.y - 6.0,
                check.max.x,
                check.max.y + 54.0,
            ),
            lang.tr("PriorityOneDoneFirst"),
            FONT_SMALL,
            Color::srgba(1.0, 1.0, 1.0, 0.5),
            super::Align::Left,
        );
    }
    let faint = Color::srgba(1.0, 1.0, 1.0, 0.5);
    p.label_at(
        Rect::new(c.x + 370.0, c.y + 5.0, c.x + 530.0, c.y + 35.0),
        format!("<= {}", lang.tr("HigherPriority")),
        FONT_TINY,
        faint,
        Anchor::UpperCenter,
    );
    p.label_at(
        Rect::new(c.x + 630.0, c.y + 5.0, c.x + 790.0, c.y + 35.0),
        format!("{} =>", lang.tr("LowerPriority")),
        FONT_TINY,
        faint,
        Anchor::UpperCenter,
    );

    // Headers.
    for (i, col) in cols.iter().enumerate() {
        let r = tw.header(i);
        let id = col.id(&sim.0.defs);
        let icon = tabs.work_sort.icon(&id);
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
            Column::Work { t, down } => {
                let label = cap(&sim.0.defs.work_types[*t].label_short);
                let lw = label_width(&label);
                let lh = super::line_height(FONT_SMALL);
                let x = r.center().x - lw / 2.0;
                let y = r.min.y + if *down { 20.0 } else { 0.0 };
                let lr = Rect::new(x, y, x + lw, y + lh);
                column_header(&mut p, &mut input, r, None, icon, None, "", &mut tips);
                p.label_at(
                    lr,
                    label.clone(),
                    FONT_SMALL,
                    Color::WHITE,
                    Anchor::MiddleCenter,
                );
                let line = Color::srgba(1.0, 1.0, 1.0, 0.3);
                let (top, bottom) = (lr.max.y - 3.0, r.min.y + HEADER_HEIGHT);
                let cx = lr.center().x.floor();
                p.rect(Rect::new(cx, top, cx + 2.0, bottom), line);
                hits.name(
                    format!("workhdr:{}", sim.0.defs.work_types[*t].def_name),
                    lr,
                );
                column_header(
                    &mut p,
                    &mut input,
                    lr,
                    None,
                    None,
                    Some(lr),
                    &header_tip_for(&sim.0, &lang, *t),
                    &mut tips,
                )
            }
            _ => None,
        };
        let Some(right) = clicked else { continue };
        if !shift {
            tabs.work_sort.header_clicked(&id, right);
        } else if let Column::Work { t, .. } = col {
            for &pid in &pawns {
                if !sim.0.ever_works(pid) || sim.0.work_type_disabled(pid, *t) {
                    continue;
                }
                let cur = sim.0.work_priority(pid, *t).unwrap_or(0);
                if let Some(n) = shift_header_priority(cur, manual, right) {
                    sim.0.set_work_type_priority(pid, *t, n);
                }
            }
        }
    }

    // Rows.
    let mut action: Option<(PawnId, LabelClick)> = None;
    for (row, &id) in pawns.iter().enumerate().take(tw.visible_rows) {
        let name = cap(&pawn_label(&sim.0, id));
        for (i, col) in cols.iter().enumerate() {
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
                Column::CopyPaste if sim.0.ever_works(id) => {
                    match copy_paste_cell(
                        &mut p,
                        &mut input,
                        &mut hits,
                        &lang,
                        r,
                        clip.work.is_some(),
                        &format!("work:{name}"),
                        &mut tips,
                    ) {
                        Some(CopyPaste::Copy) => {
                            // Disabled types copy as off; the effective
                            // priority is copied.
                            let types: Vec<DefId<WorkTypeDef>> =
                                sim.0.defs.work_type_ids().collect();
                            clip.work = Some(
                                types
                                    .iter()
                                    .map(|&t| {
                                        let v = if sim.0.work_type_disabled(id, t) {
                                            0
                                        } else {
                                            sim.0.work_priority(id, t).unwrap_or(0)
                                        };
                                        (sim.0.defs.work_types[t].def_name.clone(), v)
                                    })
                                    .collect(),
                            );
                        }
                        Some(CopyPaste::Paste) => {
                            for (name, v) in clip.work.clone().unwrap_or_default() {
                                if let Some(t) = sim.0.defs.work_types.id(&name)
                                    && !sim.0.work_type_disabled(id, t)
                                {
                                    sim.0.set_work_type_priority(id, t, v);
                                }
                            }
                        }
                        None => {}
                    }
                }
                Column::Work { t, .. } => {
                    work_box(
                        &mut p, &mut input, &mut hits, &mut sim.0, &lang, r, id, *t, &name,
                        &mut tips,
                    );
                }
                _ => {}
            }
        }
    }
    row_overlays(&mut p, &input, &tw, &sim.0, &sel, &pawns);
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

/// `PawnColumnWorker_WorkPriority.DoCell` and `WidgetsWork.DrawWorkBoxFor`.
#[allow(clippy::too_many_arguments)]
fn work_box(
    p: &mut Painter,
    input: &mut UiInput,
    hits: &mut UiHits,
    sim: &mut Sim,
    lang: &Lang,
    r: Rect,
    id: PawnId,
    t: DefId<WorkTypeDef>,
    name: &str,
    tips: &mut Option<String>,
) {
    if !sim.ever_works(id) || sim.work_type_disabled(id, t) {
        return;
    }
    let x = r.min.x + (r.width() - BOX) / 2.0;
    let y = r.min.y + 2.5;
    let b = Rect::new(x, y, x + BOX, y + BOX);
    hits.name(
        format!("work:{name}:{}", sim.defs.work_types[t].def_name),
        b,
    );
    let incapable = sim.incapable_of_work_type(id, t);
    let tint = if incapable {
        Color::srgb(1.0, 0.3, 0.3)
    } else {
        Color::WHITE
    };
    let avg = sim.average_relevant_skill(id, t);
    let (a_tex, b_tex, a) = box_background(avg);
    p.tex(b, a_tex, tint);
    p.tex(b, b_tex, tint.with_alpha(a.clamp(0.0, 1.0)));
    let prio = sim.work_priority(id, t).unwrap_or(0);
    let relevant = !sim.defs.work_types[t].relevant_skills.is_empty();
    if relevant && avg <= 2.0 && prio > 0 {
        p.tex(
            Rect::new(b.min.x - 2.0, b.min.y - 2.0, b.max.x + 2.0, b.max.y + 2.0),
            "UI/Widgets/WorkBoxOverlay_Warning",
            Color::WHITE,
        );
    }
    let passion = sim.max_relevant_passion(id, t);
    if passion != Passion::None {
        let quarter = Rect::new(b.center().x, b.center().y, b.max.x, b.max.y);
        let icon = if passion == Passion::Major {
            "UI/Icons/PassionMajorGray"
        } else {
            "UI/Icons/PassionMinorGray"
        };
        p.tex(quarter, icon, Color::srgba(1.0, 1.0, 1.0, 0.4));
    }
    let manual = sim.use_work_priorities;
    if manual {
        if prio > 0 {
            p.label_at(
                Rect::new(b.min.x - 3.0, b.min.y - 3.0, b.max.x + 3.0, b.max.y + 3.0),
                prio.to_string(),
                FONT_MEDIUM,
                priority_color(prio),
                Anchor::MiddleCenter,
            );
        }
    } else if prio > 0 {
        p.tex(b, "UI/Widgets/WorkBoxCheck", Color::WHITE);
    }
    let right = if input.clicked(b) {
        Some(false)
    } else if input.right_clicked(b) {
        // Without manual priorities `Widgets.ButtonInvisible` toggles on
        // either button.
        Some(true)
    } else {
        None
    };
    if let Some(right) = right {
        let next = clicked_priority(prio, manual, right);
        sim.set_work_type_priority(id, t, next);
    }
    if input.hovered(b) {
        *tips = Some(cell_tip(sim, lang, id, t, incapable));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_box_clicks_step_and_wrap() {
        assert_eq!(clicked_priority(3, true, false), 2);
        assert_eq!(clicked_priority(1, true, false), 0);
        assert_eq!(clicked_priority(0, true, false), 4);
        assert_eq!(clicked_priority(4, true, true), 0);
        assert_eq!(clicked_priority(0, true, true), 1);
        assert_eq!(clicked_priority(3, false, false), 0);
        assert_eq!(clicked_priority(0, false, true), 3);
    }

    #[test]
    fn shift_header_clicks_raise_lower_enable_disable() {
        assert_eq!(shift_header_priority(1, true, false), None);
        assert_eq!(shift_header_priority(0, true, false), Some(4));
        assert_eq!(shift_header_priority(2, true, false), Some(1));
        assert_eq!(shift_header_priority(0, true, true), None);
        assert_eq!(shift_header_priority(4, true, true), Some(0));
        assert_eq!(shift_header_priority(3, false, false), None);
        assert_eq!(shift_header_priority(0, false, false), Some(3));
        assert_eq!(shift_header_priority(3, false, true), Some(0));
        assert_eq!(shift_header_priority(0, false, true), None);
    }

    #[test]
    fn work_box_background_blends_by_skill() {
        assert_eq!(box_background(2.0).2, 0.5);
        assert_eq!(box_background(9.0).0, "UI/Widgets/WorkBoxBG_Bad");
        assert_eq!(box_background(14.0).2, 1.0);
        assert_eq!(box_background(17.0).1, "UI/Widgets/WorkBoxBG_Excellent");
        assert_eq!(one_decimal(3.0), "3");
        assert_eq!(one_decimal(2.25), "2.3");
    }
}
