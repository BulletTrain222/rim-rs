//! The native-style gameplay UI (docs/research.md §66): an immediate-mode
//! layer in the spirit of the game's IMGUI. Widgets lay themselves out
//! in screen pixels each frame (top-left origin, like `UI.screenWidth`),
//! push draw commands and take clicks; a pool of Bevy UI nodes renders the
//! commands. Clicks a widget takes never reach the map.
//!
//! The UI never changes the simulation itself: its actions are the same
//! orders, designations and settings the simulation offers to any input.

pub mod architect;
pub mod colonist_bar;
pub mod designator;
pub mod float_menu;
pub mod gizmo;
pub mod inspect;
pub mod lang;
pub mod schedule_tab;
pub mod select;
pub mod tabs;
pub mod work_tab;

use bevy::math::Rect;
use bevy::prelude::*;
use bevy::sprite::{BorderRect, TextureSlicer};
use bevy::text::{FontSource, Justify, LineBreak, TextLayout};

use crate::camera::{CameraInputSet, PointerClicks, ScriptCursor};
use crate::graphics::GameTextures;
use crate::thing_graphics::{Tex, ThingTextures};

/// The game's `GameFont` sizes, as drawn here.
// COMPATIBILITY TODO: currently approximate — the game draws Calibri at
// its own sizes; the system's Calibri (else Arial, else Bevy's font) is
// used at these pixel sizes, and text widths are estimated.
pub const FONT_TINY: f32 = 11.0;
pub const FONT_SMALL: f32 = 13.0;
pub const FONT_MEDIUM: f32 = 19.0;

/// Draw layers, bottom to top.
pub mod layer {
    pub const COLONIST_BAR: u16 = 10;
    pub const INSPECT: u16 = 20;
    pub const GIZMOS: u16 = 30;
    pub const MAIN_BUTTONS: u16 = 40;
    pub const ARCHITECT: u16 = 50;
    pub const MAIN_TAB: u16 = 50;
    pub const MOUSE_ATTACHMENT: u16 = 60;
    pub const MESSAGES: u16 = 70;
    pub const FLOAT_MENU: u16 = 90;
    pub const TOOLTIP: u16 = 100;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
}

/// `TextAnchor` for one line of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    UpperCenter,
    MiddleLeft,
    MiddleCenter,
    LowerLeft,
    LowerCenter,
}

#[derive(Clone, Debug)]
enum Draw {
    Rect {
        r: Rect,
        color: Color,
    },
    Image {
        r: Rect,
        image: Handle<Image>,
        color: Color,
        /// Pixel region of the texture.
        uv: Option<Rect>,
        /// Nine-slice border in texture pixels.
        slice: Option<f32>,
        flip_x: bool,
        flip_y: bool,
    },
    Text {
        r: Rect,
        text: String,
        size: f32,
        color: Color,
        align: Align,
        wrap: bool,
    },
}

/// This frame's draw commands.
#[derive(Resource, Default)]
pub struct UiFrame {
    items: Vec<(u16, Draw)>,
}

/// Pointer input for the UI this frame, and what the widgets took.
#[derive(Resource, Default)]
pub struct UiInput {
    pub cursor: Option<Vec2>,
    pub click: Option<Vec2>,
    pub right_click: Option<Vec2>,
    /// The left drag that started this frame or earlier (start, current).
    pub drag: Option<(Vec2, Vec2)>,
    /// The left button is down (`Input.GetMouseButton(0)`).
    pub held: bool,
    /// Rects that absorb the pointer this frame.
    blockers: Vec<Rect>,
    click_taken: bool,
    right_click_taken: bool,
}

impl UiInput {
    /// The cursor is over a UI element drawn this frame (so far).
    pub fn cursor_over_ui(&self) -> bool {
        self.cursor
            .is_some_and(|c| self.blockers.iter().any(|r| r.contains(c)))
    }

    /// `GenUI.AbsorbClicksInRect`: the pointer there belongs to the UI
    /// (the map never gets clicks over it; widgets inside still do).
    pub fn absorb(&mut self, r: Rect) {
        self.blockers.push(r);
    }

    pub fn hovered(&self, r: Rect) -> bool {
        self.cursor.is_some_and(|c| r.contains(c))
    }

    /// `Widgets.ButtonInvisible`: a left click inside, not yet taken.
    pub fn clicked(&mut self, r: Rect) -> bool {
        self.blockers.push(r);
        if !self.click_taken && self.click.is_some_and(|c| r.contains(c)) {
            self.click_taken = true;
            return true;
        }
        false
    }

    /// A right click inside, not yet taken.
    pub fn right_clicked(&mut self, r: Rect) -> bool {
        self.blockers.push(r);
        if !self.right_click_taken && self.right_click.is_some_and(|c| r.contains(c)) {
            self.right_click_taken = true;
            return true;
        }
        false
    }

    /// Takes this frame's left click wherever it is (a window that closes
    /// on clicks outside it).
    pub fn take_click(&mut self) -> Option<Vec2> {
        if self.click_taken {
            return None;
        }
        self.click_taken = true;
        self.click
    }

    pub fn take_right_click(&mut self) -> Option<Vec2> {
        if self.right_click_taken {
            return None;
        }
        self.right_click_taken = true;
        self.right_click
    }

    pub fn click_free(&self) -> bool {
        !self.click_taken && self.click.is_some()
    }

    pub fn right_click_free(&self) -> bool {
        !self.right_click_taken && self.right_click.is_some()
    }
}

/// Named UI regions of the last frame (scripts click them by name).
#[derive(Resource, Default)]
pub struct UiHits {
    current: Vec<(String, Rect)>,
    pub last: Vec<(String, Rect)>,
}

impl UiHits {
    pub fn name(&mut self, id: impl Into<String>, r: Rect) {
        self.current.push((id.into(), r));
    }

    /// The last frame's region with this name (exact, else the first that
    /// starts with it).
    pub fn find(&self, id: &str) -> Option<Rect> {
        self.last
            .iter()
            .find(|(n, _)| n == id)
            .or_else(|| self.last.iter().find(|(n, _)| n.starts_with(id)))
            .map(|(_, r)| *r)
    }
}

/// The UI font (the system's Calibri when present).
#[derive(Resource, Default)]
pub struct UiFont(pub Option<Handle<Font>>);

/// Keyed strings from the install.
#[derive(Resource, Default)]
pub struct Lang(pub rimworld_defs::keyed::KeyedStrings);

impl Lang {
    pub fn tr(&self, key: &str) -> String {
        self.0.tr(key, &[])
    }

    pub fn tr_args(&self, key: &str, args: &[&str]) -> String {
        self.0.tr(key, args)
    }
}

/// A message in the corner (`Messages.Message`): text and when it came.
#[derive(Resource, Default)]
pub struct Messages(pub Vec<(String, f32)>);

impl Messages {
    pub fn add(&mut self, text: impl Into<String>, now: f32) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.0.retain(|(t, _)| *t != text);
        self.0.push((text, now));
        if self.0.len() > 8 {
            self.0.remove(0);
        }
    }
}

/// The painter widgets use: draw commands on a layer, UI textures.
pub struct Painter<'a> {
    frame: &'a mut UiFrame,
    pub layer: u16,
    textures: &'a mut ThingTextures,
    lib: Option<&'a rimworld_assets::unity::TextureLibrary>,
    images: &'a mut Assets<Image>,
}

impl Painter<'_> {
    pub fn rect(&mut self, r: Rect, color: Color) {
        self.frame.items.push((self.layer, Draw::Rect { r, color }));
    }

    /// `Widgets.DrawBox`: an outline `t` pixels thick.
    pub fn outline(&mut self, r: Rect, t: f32, color: Color) {
        self.rect(Rect::new(r.min.x, r.min.y, r.max.x, r.min.y + t), color);
        self.rect(Rect::new(r.min.x, r.max.y - t, r.max.x, r.max.y), color);
        self.rect(Rect::new(r.min.x, r.min.y, r.min.x + t, r.max.y), color);
        self.rect(Rect::new(r.max.x - t, r.min.y, r.max.x, r.max.y), color);
    }

    /// A texture from the install by path, if it loads.
    pub fn texture(&mut self, path: &str) -> Option<Tex> {
        let lib = self.lib?;
        self.textures.get(lib, path, None, self.images)
    }

    pub fn textures(
        &mut self,
    ) -> (
        &mut ThingTextures,
        Option<&rimworld_assets::unity::TextureLibrary>,
        &mut Assets<Image>,
    ) {
        (self.textures, self.lib, self.images)
    }

    pub fn image(&mut self, r: Rect, image: Handle<Image>, color: Color) {
        self.frame.items.push((
            self.layer,
            Draw::Image {
                r,
                image,
                color,
                uv: None,
                slice: None,
                flip_x: false,
                flip_y: false,
            },
        ));
    }

    /// An image mirrored on either axis.
    pub fn image_flipped(
        &mut self,
        r: Rect,
        image: Handle<Image>,
        color: Color,
        flip_x: bool,
        flip_y: bool,
    ) {
        self.frame.items.push((
            self.layer,
            Draw::Image {
                r,
                image,
                color,
                uv: None,
                slice: None,
                flip_x,
                flip_y,
            },
        ));
    }

    pub fn image_ex(
        &mut self,
        r: Rect,
        image: Handle<Image>,
        color: Color,
        uv: Option<Rect>,
        flip_x: bool,
    ) {
        self.frame.items.push((
            self.layer,
            Draw::Image {
                r,
                image,
                color,
                uv,
                slice: None,
                flip_x,
                flip_y: false,
            },
        ));
    }

    /// `ThingDef.uiIcon`: the def's graphic; collections (stack counts,
    /// random variants) use their folder's textures by name, stack counts
    /// the last (`Graphic_StackCount.MatSingle`), others the first.
    pub fn thing_icon(
        &mut self,
        sim: &rimworld_sim::Sim,
        def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
    ) -> Option<Tex> {
        let g = sim.defs.things[def].graphic.as_ref()?;
        let path = g.tex_path.clone()?;
        let class = g.graphic_class.clone().unwrap_or_default();
        let lib = self.lib?;
        let collection = ["StackCount", "Random", "Collection", "MealVariants"]
            .iter()
            .any(|c| class.contains(c));
        if collection {
            let mut files = self.textures.folder(lib, &path);
            // `Graphic_Single.MaskSuffix`: masks end in "m".
            files.retain(|f| !f.ends_with('m'));
            files.sort();
            let pick = if class.contains("StackCount") {
                files.last()
            } else {
                files.first()
            };
            if let Some(f) = pick.cloned() {
                return self.textures.get(lib, &f, None, self.images);
            }
        }
        self.textures
            .get(lib, &path, None, self.images)
            .or_else(|| {
                self.textures
                    .get(lib, &format!("{path}_south"), None, self.images)
            })
    }

    /// Draws a thing def's icon fitted into a rect (a linked graphic's
    /// atlas shows its lone piece).
    pub fn thing_icon_fitted(
        &mut self,
        r: Rect,
        sim: &rimworld_sim::Sim,
        def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
        color: Color,
    ) {
        let linked = sim.defs.things[def]
            .graphic
            .as_ref()
            .is_some_and(|g| g.link_type.as_deref().is_some_and(|l| l != "None"));
        if let Some(t) = self.thing_icon(sim, def) {
            if linked {
                let uv = crate::thing_graphics::atlas_rect(t.size, 0);
                self.image_ex(fit(r, Vec2::ONE, 1.0), t.image, color, Some(uv), false);
            } else {
                let fitted = fit(r, t.size.as_vec2(), 1.0);
                self.image(fitted, t.image, color);
            }
        }
    }

    /// A texture by path; a grey box when it doesn't load.
    pub fn tex(&mut self, r: Rect, path: &str, color: Color) {
        match self.texture(path) {
            Some(t) => self.image(r, t.image, color),
            None => self.rect(r, Color::srgba(0.4, 0.4, 0.4, 0.6 * color.alpha())),
        }
    }

    /// `Widgets.DrawTextureFitted`: the texture scaled to fit, centred.
    pub fn tex_fitted(&mut self, r: Rect, path: &str, scale: f32, color: Color) {
        let Some(t) = self.texture(path) else {
            return;
        };
        let fitted = fit(r, t.size.as_vec2(), scale);
        self.image(fitted, t.image, color);
    }

    /// `Widgets.DrawAtlas`: a nine-sliced atlas texture.
    pub fn atlas(&mut self, r: Rect, path: &str, color: Color) {
        let Some(t) = self.texture(path) else {
            self.rect(r, color.with_alpha(0.8 * color.alpha()));
            return;
        };
        // `Widgets.DrawAtlas`: corners of a quarter of the texture, at most
        // a third of the rect.
        let border = (t.size.x as f32 * 0.25).floor();
        self.frame.items.push((
            self.layer,
            Draw::Image {
                r,
                image: t.image,
                color,
                uv: None,
                slice: Some(border),
                flip_x: false,
                flip_y: false,
            },
        ));
    }

    pub fn text(
        &mut self,
        r: Rect,
        text: impl Into<String>,
        size: f32,
        color: Color,
        align: Align,
    ) {
        self.frame.items.push((
            self.layer,
            Draw::Text {
                r,
                text: text.into(),
                size,
                color,
                align,
                wrap: true,
            },
        ));
    }

    /// One line placed in the rect by a `TextAnchor`.
    pub fn label_at(
        &mut self,
        r: Rect,
        text: impl Into<String>,
        size: f32,
        color: Color,
        anchor: Anchor,
    ) {
        let h = line_height(size);
        let top = match anchor {
            Anchor::UpperCenter => r.min.y,
            Anchor::MiddleLeft | Anchor::MiddleCenter => r.min.y + (r.height() - h) * 0.5,
            Anchor::LowerLeft | Anchor::LowerCenter => r.max.y - h,
        };
        let align = match anchor {
            Anchor::UpperCenter | Anchor::MiddleCenter | Anchor::LowerCenter => Align::Center,
            _ => Align::Left,
        };
        self.frame.items.push((
            self.layer,
            Draw::Text {
                r: Rect::new(r.min.x, top, r.max.x, top + h),
                text: text.into(),
                size,
                color,
                align,
                wrap: align == Align::Center,
            },
        ));
    }

    /// `Widgets.DrawHighlight`: `TexUI.HighlightTex` (white at 0.1) at
    /// an opacity.
    pub fn highlight(&mut self, r: Rect, opacity: f32) {
        self.rect(r, Color::srgba(1.0, 1.0, 1.0, 0.1 * opacity));
    }

    /// A tooltip (`TooltipHandler.TipRegion`, `ActiveTip`): the text
    /// wrapped at 260 pixels, beside the cursor (`GenUI.
    /// GetMouseAttachedWindowPos`).
    // COMPATIBILITY TODO: currently approximate — tips show at once (the
    // game waits `TipSignal.delay`), coloured spans are drawn white.
    pub fn tip(&mut self, cursor: Vec2, text: &str, screen: Vec2) {
        if text.is_empty() {
            return;
        }
        let inner_w = text
            .split('\n')
            .map(|l| text_width(l, FONT_SMALL))
            .fold(0.0f32, f32::max)
            .min(260.0)
            .ceil();
        let w = inner_w + 8.0;
        let h = text_height(text, FONT_SMALL, inner_w) + 8.0;
        let x = if cursor.x + 16.0 + w < screen.x {
            cursor.x + 16.0
        } else {
            cursor.x - 4.0 - w
        };
        let y = if cursor.y + 14.0 + h < screen.y {
            cursor.y + 14.0
        } else if cursor.y - 5.0 - h >= 0.0 {
            cursor.y - 5.0 - h
        } else {
            0.0
        };
        let r = Rect::new(x, y, x + w, y + h);
        let layer = self.layer;
        self.layer = layer::TOOLTIP;
        // `ActiveTip.TooltipBGAtlas`.
        self.atlas(r, "UI/Widgets/TooltipBG", Color::WHITE);
        self.text(
            Rect::new(r.min.x + 4.0, r.min.y + 4.0, r.max.x - 4.0 + 2.0, r.max.y),
            text,
            FONT_SMALL,
            Color::WHITE,
            Align::Left,
        );
        self.layer = layer;
    }

    /// One line, vertically centred in the rect.
    pub fn label_mid(
        &mut self,
        r: Rect,
        text: impl Into<String>,
        size: f32,
        color: Color,
        align: Align,
    ) {
        let h = line_height(size);
        let top = r.min.y + ((r.height() - h) * 0.5).max(0.0);
        self.frame.items.push((
            self.layer,
            Draw::Text {
                r: Rect::new(r.min.x, top, r.max.x, top + h),
                text: text.into(),
                size,
                color,
                align,
                // Centred text needs the node's width to centre in.
                wrap: align == Align::Center,
            },
        ));
    }
}

/// A rect of `size` fitted into `r` (aspect kept), scaled, centred.
pub fn fit(r: Rect, size: Vec2, scale: f32) -> Rect {
    let s = (r.width() / size.x).min(r.height() / size.y) * scale;
    let d = size * s;
    let c = r.center();
    Rect::from_center_size(c, d)
}

/// Estimated text metrics.
pub fn line_height(size: f32) -> f32 {
    (size * 1.35).round()
}

pub fn text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | 'I' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' => 0.28,
            'm' | 'w' | 'M' | 'W' => 0.8,
            ' ' => 0.26,
            c if c.is_uppercase() => 0.6,
            _ => 0.49,
        })
        .sum::<f32>()
        * size
}

/// Lines a text wraps into at `width`.
pub fn text_lines(text: &str, size: f32, width: f32) -> usize {
    let mut lines = 0;
    for para in text.split('\n') {
        let mut line = 0.0;
        let mut n = 1;
        for word in para.split(' ') {
            let w = text_width(word, size) + text_width(" ", size);
            if line > 0.0 && line + w > width {
                n += 1;
                line = w;
            } else {
                line += w;
            }
        }
        lines += n;
    }
    lines.max(1)
}

pub fn text_height(text: &str, size: f32, width: f32) -> f32 {
    text_lines(text, size, width) as f32 * line_height(size)
}

/// `GenText.Truncate`: the text cut to fit `width`, ending in "...".
pub fn truncate(text: &str, width: f32, size: f32) -> String {
    if text_width(text, size) <= width {
        return text.to_owned();
    }
    let mut s: String = text.to_owned();
    while !s.is_empty() && text_width(&format!("{s}..."), size) > width {
        s.pop();
    }
    format!("{}...", s.trim_end())
}

/// Systems: input → widgets (topmost first) → render.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum UiSet {
    Begin,
    Widgets,
    End,
}

/// The ingredients of a [`Painter`] as system parameters.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Paint<'w> {
    frame: ResMut<'w, UiFrame>,
    textures: ResMut<'w, ThingTextures>,
    lib: Res<'w, GameTextures>,
    images: ResMut<'w, Assets<Image>>,
}

impl Paint<'_> {
    pub fn painter(&mut self, layer: u16) -> Painter<'_> {
        Painter {
            frame: &mut self.frame,
            layer,
            textures: &mut self.textures,
            lib: self.lib.0.as_ref(),
            images: &mut self.images,
        }
    }
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiFrame>()
            .init_resource::<UiInput>()
            .init_resource::<UiHits>()
            .init_resource::<UiFont>()
            .init_resource::<Messages>()
            .init_resource::<UiPool>()
            .init_resource::<select::Selection>()
            .init_resource::<designator::DesignatorManager>()
            .init_resource::<float_menu::FloatMenuState>()
            .init_resource::<architect::ArchitectState>()
            .init_resource::<tabs::MainTabs>()
            .init_resource::<tabs::Clipboards>()
            .init_resource::<gizmo::GizmoState>()
            .init_resource::<colonist_bar::ColonistBarState>()
            .configure_sets(
                Update,
                (UiSet::Begin, UiSet::Widgets, UiSet::End)
                    .chain()
                    .after(CameraInputSet),
            )
            .add_systems(Startup, (load_font, spawn_pool))
            .add_systems(Update, begin_frame.in_set(UiSet::Begin))
            .add_systems(
                Update,
                (
                    messages_ui,
                    float_menu::float_menu_ui,
                    architect::main_buttons_ui,
                    architect::architect_ui,
                    work_tab::work_tab_ui,
                    schedule_tab::schedule_tab_ui,
                    gizmo::gizmos_ui,
                    inspect::inspect_ui,
                    colonist_bar::colonist_bar_ui,
                    designator::designator_mouse_ui,
                    float_menu::map_float_menu,
                )
                    .chain()
                    .in_set(UiSet::Widgets),
            )
            .add_systems(Update, (end_frame, render_frame).chain().in_set(UiSet::End))
            .add_systems(
                Update,
                architect::low_priority_map_clicks
                    .after(designator::DesignatorSet)
                    .before(select::SelectSet),
            );
        select::build(app);
        designator::build(app);
    }
}

fn load_font(mut fonts: ResMut<Assets<Font>>, mut font: ResMut<UiFont>) {
    for path in [
        "C:/Windows/Fonts/calibri.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            font.0 = Some(fonts.add(Font::from_bytes(bytes)));
            return;
        }
    }
}

/// Takes this frame's pointer from the camera input.
fn begin_frame(
    clicks: Res<PointerClicks>,
    cursor: Res<ScriptCursor>,
    window: Single<&Window>,
    mut input: ResMut<UiInput>,
    mut frame: ResMut<UiFrame>,
    mut hits: ResMut<UiHits>,
) {
    *input = UiInput {
        cursor: cursor.get(&window),
        click: clicks.primary,
        right_click: clicks.secondary,
        drag: clicks.primary_drag,
        held: clicks.primary_held,
        ..default()
    };
    frame.items.clear();
    hits.last = std::mem::take(&mut hits.current);
}

/// Clicks the UI took (or that landed on it) don't reach the map.
fn end_frame(mut clicks: ResMut<PointerClicks>, input: Res<UiInput>) {
    let on_ui = |p: Vec2| input.blockers.iter().any(|r| r.contains(p));
    if input.click_taken || clicks.primary.is_some_and(on_ui) {
        clicks.primary = None;
    }
    if input.right_click_taken || clicks.secondary.is_some_and(on_ui) {
        clicks.secondary = None;
    }
    if clicks.primary_drag.is_some_and(|(a, _)| on_ui(a)) {
        clicks.primary_drag = None;
    }
    if clicks.primary_drag_end.is_some_and(|(a, _)| on_ui(a)) {
        clicks.primary_drag_end = None;
    }
}

/// Messages in the upper left (`Messages.MessagesDoGUI`), fading out.
fn messages_ui(mut paint: Paint, mut messages: ResMut<Messages>, time: Res<Time>) {
    let now = time.elapsed_secs();
    messages.0.retain(|(_, t)| now - t < 13.0);
    let mut p = paint.painter(layer::MESSAGES);
    let mut y = 12.0;
    for (text, t) in &messages.0 {
        let age = now - t;
        let alpha = if age > 12.0 { 13.0 - age } else { 1.0 };
        let w = text_width(text, FONT_SMALL) + 12.0;
        let r = Rect::new(12.0, y, 12.0 + w, y + 26.0);
        p.rect(r, Color::srgba(0.0, 0.0, 0.0, 0.35 * alpha));
        p.label_mid(
            Rect::new(18.0, y, 18.0 + w, y + 26.0),
            text.clone(),
            FONT_SMALL,
            Color::srgba(1.0, 1.0, 1.0, alpha),
            Align::Left,
        );
        y += 28.0;
    }
}

/// Pooled render nodes.
#[derive(Resource, Default)]
struct UiPool {
    boxes: Vec<Entity>,
    texts: Vec<Entity>,
}

#[derive(Component)]
struct PoolBox;

#[derive(Component)]
struct PoolText;

fn box_bundle() -> impl Bundle {
    (
        PoolBox,
        Node {
            position_type: PositionType::Absolute,
            ..default()
        },
        BackgroundColor(Color::NONE),
        ImageNode {
            color: Color::NONE,
            image_mode: NodeImageMode::Stretch,
            ..default()
        },
        GlobalZIndex(0),
        Visibility::Hidden,
        Pickable::IGNORE,
    )
}

fn text_bundle() -> impl Bundle {
    (
        PoolText,
        Node {
            position_type: PositionType::Absolute,
            ..default()
        },
        Text::new(""),
        TextFont::from_font_size(FONT_SMALL),
        TextColor(Color::WHITE),
        TextLayout::new(Justify::Left, LineBreak::WordBoundary),
        GlobalZIndex(0),
        Visibility::Hidden,
        Pickable::IGNORE,
    )
}

fn spawn_pool(mut commands: Commands, mut pool: ResMut<UiPool>) {
    for _ in 0..600 {
        pool.boxes.push(commands.spawn(box_bundle()).id());
    }
    for _ in 0..300 {
        pool.texts.push(commands.spawn(text_bundle()).id());
    }
}

type BoxQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Node,
        &'static mut BackgroundColor,
        &'static mut ImageNode,
        &'static mut GlobalZIndex,
        &'static mut Visibility,
    ),
    (With<PoolBox>, Without<PoolText>),
>;

type TextQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Node,
        &'static mut Text,
        &'static mut TextFont,
        &'static mut TextColor,
        &'static mut TextLayout,
        &'static mut GlobalZIndex,
        &'static mut Visibility,
    ),
    (With<PoolText>, Without<PoolBox>),
>;

fn place(node: &mut Mut<Node>, r: Rect) {
    let (l, t, w, h) = (
        r.min.x.round(),
        r.min.y.round(),
        r.width().round(),
        r.height().round(),
    );
    if node.left != Val::Px(l)
        || node.top != Val::Px(t)
        || node.width != Val::Px(w)
        || node.height != Val::Px(h)
    {
        node.left = Val::Px(l);
        node.top = Val::Px(t);
        node.width = Val::Px(w);
        node.height = Val::Px(h);
    }
}

/// Renders the frame's commands through the node pool, in layer order.
fn render_frame(
    mut commands: Commands,
    mut frame: ResMut<UiFrame>,
    mut pool: ResMut<UiPool>,
    font: Res<UiFont>,
    mut boxes: BoxQuery,
    mut texts: TextQuery,
) {
    frame.items.sort_by_key(|(l, _)| *l);
    let (mut nb, mut nt) = (0usize, 0usize);
    for (z, (_, d)) in frame.items.iter().enumerate() {
        let z = z as i32 + 1000;
        match d {
            Draw::Rect { .. } | Draw::Image { .. } => {
                let Some(&e) = pool.boxes.get(nb) else {
                    pool.boxes.push(commands.spawn(box_bundle()).id());
                    nb += 1;
                    continue;
                };
                nb += 1;
                let Ok((mut node, mut bg, mut img, mut gz, mut vis)) = boxes.get_mut(e) else {
                    continue;
                };
                vis.set_if_neq(Visibility::Inherited);
                gz.set_if_neq(GlobalZIndex(z));
                match d {
                    Draw::Rect { r, color } => {
                        place(&mut node, *r);
                        bg.set_if_neq(BackgroundColor(*color));
                        if img.color != Color::NONE {
                            img.color = Color::NONE;
                        }
                    }
                    Draw::Image {
                        r,
                        image,
                        color,
                        uv,
                        slice,
                        flip_x,
                        flip_y,
                    } => {
                        place(&mut node, *r);
                        bg.set_if_neq(BackgroundColor(Color::NONE));
                        let mode = match slice {
                            Some(b) => NodeImageMode::Sliced(TextureSlicer {
                                border: BorderRect::all(*b),
                                max_corner_scale: 1.0,
                                ..default()
                            }),
                            None => NodeImageMode::Stretch,
                        };
                        if img.image != *image
                            || img.color != *color
                            || img.rect != *uv
                            || img.flip_x != *flip_x
                            || img.flip_y != *flip_y
                            || img.image_mode != mode
                        {
                            img.image = image.clone();
                            img.color = *color;
                            img.rect = *uv;
                            img.flip_x = *flip_x;
                            img.flip_y = *flip_y;
                            img.image_mode = mode;
                        }
                    }
                    Draw::Text { .. } => {}
                }
            }
            Draw::Text {
                r,
                text,
                size,
                color,
                align,
                wrap,
            } => {
                let Some(&e) = pool.texts.get(nt) else {
                    pool.texts.push(commands.spawn(text_bundle()).id());
                    nt += 1;
                    continue;
                };
                nt += 1;
                let Ok((mut node, mut t, mut f, mut c, mut layout, mut gz, mut vis)) =
                    texts.get_mut(e)
                else {
                    continue;
                };
                vis.set_if_neq(Visibility::Inherited);
                gz.set_if_neq(GlobalZIndex(z));
                place(&mut node, *r);
                if t.0 != *text {
                    t.0.clone_from(text);
                }
                let font_src = font
                    .0
                    .clone()
                    .map_or(FontSource::default(), FontSource::Handle);
                if f.font != font_src || f.font_size != bevy::text::FontSize::Px(*size) {
                    f.font = font_src;
                    f.font_size = bevy::text::FontSize::Px(*size);
                }
                c.set_if_neq(TextColor(*color));
                let justify = match align {
                    Align::Left => Justify::Left,
                    Align::Center => Justify::Center,
                };
                let lb = if *wrap {
                    LineBreak::WordBoundary
                } else {
                    LineBreak::NoWrap
                };
                if layout.justify != justify || layout.linebreak != lb {
                    layout.justify = justify;
                    layout.linebreak = lb;
                }
            }
        }
    }
    for &e in pool.boxes.iter().skip(nb) {
        if let Ok((_, _, _, _, mut vis)) = boxes.get_mut(e) {
            vis.set_if_neq(Visibility::Hidden);
        }
    }
    for &e in pool.texts.iter().skip(nt) {
        if let Ok((_, _, _, _, _, _, mut vis)) = texts.get_mut(e) {
            vis.set_if_neq(Visibility::Hidden);
        }
    }
}

/// Screen size in UI pixels.
pub fn screen(window: &Window) -> Vec2 {
    Vec2::new(window.width(), window.height())
}
