//! The colonist bar (`ColonistBar`, `ColonistBarDrawLocsFinder`,
//! `ColonistBarColonistDrawer`; docs/research.md §66): the colonists (and
//! colonist corpses) in a row centred at the top, 48-pixel boxes 24 apart,
//! shrinking (and wrapping) to fit the screen less 520 pixels. Each box
//! shows the mood as a fill from the bottom, the portrait, status icons,
//! the name, the weapon underneath while drafted and selection brackets.
//! A click selects; a double click jumps the camera there.

use bevy::math::Rect;
use bevy::prelude::*;
use rimworld_sim::job::JobKind;
use rimworld_sim::sim::ThingRef;
use rimworld_sim::{PawnId, Sim};

use super::lang::pawn_label;
use super::select::{Selectable, Selection};
use super::{Align, FONT_TINY, Paint, Painter, UiHits, UiInput, layer, text_width};
use crate::SimState;
use crate::graphics::{PawnGraphics, SKIN_TINT};
use crate::view::sim_to_world;

/// `ColonistBar.BaseSize` and spacings.
const BASE: f32 = 48.0;
const SPACE_H: f32 = 24.0;
const SPACE_V: f32 = 32.0;
const MARGIN_TOP: f32 = 21.0;

/// The last click (for double clicks within `DoubleClickTime` 0.5 s).
#[derive(Resource, Default)]
pub struct ColonistBarState {
    last_click: Option<(PawnId, f32)>,
}

/// `ColonistBar.Entries`: free colonists and colonist corpses, in display
/// order.
// COMPATIBILITY TODO: currently approximate — display order is spawn
// order (no reordering by dragging); caravans and other maps don't exist.
pub fn entries(sim: &Sim) -> Vec<PawnId> {
    sim.pawns()
        .iter()
        .filter(|p| p.is_colonist && (!p.health.dead || sim.corpse_of(p.id).is_some()))
        .map(|p| p.id)
        .collect()
}

/// `FindBestScale` and `CalculateDrawLocs` for one group.
pub fn draw_locs(n: usize, screen_w: f32) -> (Vec<Vec2>, f32) {
    if n == 0 {
        return (Vec::new(), 1.0);
    }
    let max_w = screen_w - 520.0;
    let mut scale = 1.0f32;
    let (per_row, one_row) = loop {
        let slot = (BASE + SPACE_H) * scale;
        let per = ((max_w / slot).floor() as usize).max(1);
        let rows = n.div_ceil(per);
        let allowed = if scale > 0.58 {
            1
        } else if scale > 0.42 {
            2
        } else {
            3
        };
        if rows <= allowed {
            break (per.min(n), rows == 1);
        }
        scale *= 0.95;
    };
    let slot = (BASE + SPACE_H) * scale;
    let row_slots = if one_row { n } else { per_row };
    let total = row_slots as f32 * slot;
    let start = (screen_w - total) / 2.0;
    let rem = n % per_row;
    let locs = (0..n)
        .map(|i| {
            let mut x = start + (i % per_row) as f32 * slot;
            let y = MARGIN_TOP + (i / per_row) as f32 * scale * (BASE + SPACE_V);
            // The last, shorter row is centred.
            if rem != 0 && i >= n - rem {
                x += (per_row - rem) as f32 * slot * 0.5;
            }
            Vec2::new(x, y)
        })
        .collect();
    (locs, scale)
}

/// `ColonistBarColonistDrawer.DrawIcons`' activity icon.
fn activity_icon(sim: &Sim, id: PawnId) -> Option<&'static str> {
    let p = sim.pawn(id)?;
    if p.health.dead {
        return None;
    }
    if let Some(state) = sim.mental_state_of(id) {
        let aggro = matches!(state, "Berserk" | "Manhunter" | "MurderousRage");
        return Some(if aggro {
            "UI/Icons/ColonistBar/MentalStateAggro"
        } else {
            "UI/Icons/ColonistBar/MentalStateNonAggro"
        });
    }
    if p.asleep {
        return Some("UI/Icons/ColonistBar/Sleeping");
    }
    match p.job.as_ref().map(|j| j.kind) {
        Some(JobKind::AttackMelee { .. } | JobKind::AttackStatic { .. }) => {
            Some("UI/Icons/ColonistBar/Attacking")
        }
        Some(JobKind::Flee { .. }) => Some("UI/Icons/ColonistBar/Fleeing"),
        // `mindState.IsIdle` (idle jobs) after the first day.
        None if sim.day() > 1 => Some("UI/Icons/ColonistBar/Idle"),
        _ => None,
    }
}

/// `SelectionDrawerUtility.DrawSelectionOverlayOnGUI`: four corner
/// brackets around the box.
pub fn selection_brackets(p: &mut Painter, r: Rect, scale: f32) {
    let Some(t) = p.texture("UI/Overlays/SelectionBracketGUI") else {
        p.outline(r, 2.0, Color::WHITE);
        return;
    };
    let s = t.size.as_vec2() * 0.4 * scale;
    let corners = [
        (Vec2::new(r.min.x, r.min.y), false, false),
        (Vec2::new(r.max.x - s.x, r.min.y), true, false),
        (Vec2::new(r.max.x - s.x, r.max.y - s.y), true, true),
        (Vec2::new(r.min.x, r.max.y - s.y), false, true),
    ];
    for (at, fx, fy) in corners {
        let rect = Rect::new(at.x, at.y, at.x + s.x, at.y + s.y);
        // One bracket texture, mirrored into each corner.
        p.image_flipped(rect, t.image.clone(), Color::WHITE, fx, fy);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn colonist_bar_ui(
    mut paint: Paint,
    mut input: ResMut<UiInput>,
    sim: Res<SimState>,
    mut sel: ResMut<Selection>,
    mut state: ResMut<ColonistBarState>,
    graphics: Option<Res<PawnGraphics>>,
    window: Single<&Window>,
    mut camera: Query<&mut Transform, With<Camera2d>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut hits: ResMut<UiHits>,
    time: Res<Time>,
    mgr: Res<super::designator::DesignatorManager>,
) {
    let sim = &sim.0;
    let screen = super::screen(&window);
    // `ColonistBar.Visible`.
    if screen.x < 800.0 || screen.y < 500.0 {
        return;
    }
    let now = time.elapsed_secs();
    let list = entries(sim);
    let (locs, scale) = draw_locs(list.len(), screen.x);
    let size = BASE * scale;
    let mut p = paint.painter(layer::COLONIST_BAR);
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    for (&id, &at) in list.iter().zip(&locs) {
        let Some(pawn) = sim.pawn(id) else { continue };
        let r = Rect::new(at.x, at.y, at.x + size, at.y + size);
        hits.name(format!("bar:{}", pawn.name), r);
        // `ColonistBar.BGTex` (the command background).
        p.tex(r, "UI/Widgets/DesButBG", Color::WHITE);
        // Mood: a fill from the bottom (`MoodBGTex`).
        let (mood, _) = sim.mood_view(id);
        if let Some((level, _)) = mood {
            let inner = Rect::new(r.min.x + 2.0, r.min.y + 2.0, r.max.x - 2.0, r.max.y - 2.0);
            let h = inner.height() * level.clamp(0.0, 1.0);
            p.rect(
                Rect::new(inner.min.x, inner.max.y - h, inner.max.x, inner.max.y),
                Color::srgba(0.4, 0.47, 0.53, 0.44),
            );
        }
        let target = if pawn.health.dead {
            sim.corpse_of(id)
                .map(|c| Selectable::Thing(ThingRef::Item(c)))
        } else {
            Some(Selectable::Thing(ThingRef::Pawn(id)))
        };
        if target.is_some_and(|t| sel.contains(t)) {
            selection_brackets(
                &mut p,
                Rect::new(
                    r.min.x - 2.0 * scale,
                    r.min.y - 2.0 * scale,
                    r.max.x + 2.0 * scale,
                    r.max.y + 2.0 * scale,
                ),
                scale,
            );
        }
        // The portrait: 46×75 from 1 px in, rising above the box.
        // COMPATIBILITY TODO: currently approximate — the game renders the
        // pawn's own portrait (`PortraitsCache`: body, head, hair,
        // apparel); the shared body and head textures stand in.
        let pw = (BASE - 2.0) * scale;
        let ph = 75.0 * scale;
        let pr = Rect::new(
            r.min.x + 1.0,
            r.min.y - (ph - size) - 1.0,
            r.min.x + 1.0 + pw,
            r.min.y - (ph - size) - 1.0 + ph,
        );
        if let Some(g) = &graphics {
            let tint = if pawn.health.dead {
                Color::srgb(0.45, 0.45, 0.45)
            } else {
                SKIN_TINT
            };
            let body = Rect::from_center_size(
                pr.center() + Vec2::new(0.0, ph * 0.22),
                Vec2::splat(pw * 1.05),
            );
            p.image(body, g.body["south"].clone(), tint);
            let head = Rect::from_center_size(
                body.center() - Vec2::new(0.0, body.height() * 0.23),
                Vec2::splat(pw * 1.05),
            );
            p.image(head, g.head["south"].clone(), tint);
        }
        // Status icons along the bottom left.
        if let Some(icon) = activity_icon(sim, id) {
            let s = (BASE - 2.0).min(20.0) * scale;
            p.tex(
                Rect::new(
                    r.min.x + 1.0,
                    r.max.y - s - 1.0,
                    r.min.x + 1.0 + s,
                    r.max.y - 1.0,
                ),
                icon,
                Color::srgba(1.0, 1.0, 1.0, 0.8),
            );
        }
        if pawn.health.dead {
            p.tex(r, "UI/Misc/DeadColonist", Color::WHITE);
        }
        // `GenMapUI.DrawPawnLabel` below the box.
        let name = pawn_label(sim, id);
        let w = (text_width(&name, FONT_TINY) + 6.0).min(size + SPACE_H * scale - 2.0);
        let lr = Rect::new(
            r.center().x - w / 2.0,
            r.max.y - 4.0 * scale,
            r.center().x + w / 2.0,
            r.max.y - 4.0 * scale + 16.0,
        );
        p.rect(lr, Color::srgba(0.0, 0.0, 0.0, 0.55));
        p.label_mid(lr, name, FONT_TINY, Color::WHITE, Align::Center);
        // The weapon under the portrait while drafted
        // (`ShowWeaponsUnderPortraitMode.WhileDrafted`).
        if pawn.drafted
            && let Some(eq) = &pawn.equipment
            && let Some(path) = sim.defs.things[eq.def]
                .graphic
                .as_ref()
                .and_then(|g| g.tex_path.clone())
        {
            let wr = Rect::new(
                r.min.x,
                r.min.y + size * 1.05,
                r.max.x,
                r.max.y + size * 1.05,
            );
            let wr = Rect::from_center_size(wr.center(), wr.size() * 0.75);
            p.tex_fitted(wr, &path, 1.0, Color::WHITE);
        }
        if input.clicked(r) {
            // A double click jumps the camera (`CameraJumper.TryJump`).
            if state
                .last_click
                .is_some_and(|(q, t)| q == id && now - t < 0.5)
            {
                let pos = sim.corpse_of(id).and_then(|c| sim.map.item(c)).map_or_else(
                    || sim_to_world(pawn.visual_position()),
                    |c| crate::view::cell_center(c.position),
                );
                if let Ok(mut t) = camera.single_mut() {
                    t.translation.x = pos.x;
                    t.translation.y = pos.y;
                }
                state.last_click = None;
            } else {
                state.last_click = Some((id, now));
            }
            // `Selector.SelectUnderMouse` with the bar's colonist.
            if let Some(o) = target
                && mgr.selected.is_none()
            {
                if !shift {
                    sel.clear();
                    sel.select(o, now);
                } else if sel.contains(o) {
                    sel.deselect(o);
                } else {
                    sel.select(o, now);
                }
            }
        }
        input.right_clicked(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_colonists_sit_centred_at_full_scale() {
        let (locs, scale) = draw_locs(3, 1400.0);
        assert_eq!(scale, 1.0);
        // Three 72-pixel slots centred: (1400 − 216) / 2 = 592.
        assert_eq!(
            locs,
            vec![
                Vec2::new(592.0, 21.0),
                Vec2::new(664.0, 21.0),
                Vec2::new(736.0, 21.0)
            ]
        );
    }

    #[test]
    fn a_crowd_shrinks_the_bar() {
        // 880 px for 72-px slots: 12 per row at full scale.
        let (_, s12) = draw_locs(12, 1400.0);
        assert_eq!(s12, 1.0);
        let (locs, s13) = draw_locs(13, 1400.0);
        assert!(s13 < 1.0);
        assert!(locs.iter().all(|l| l.y == 21.0), "one row while above 0.58");
    }
}
