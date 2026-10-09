//! Textures for buildings and plants from the install (`Graphic` classes):
//! `Graphic_Single`, `Graphic_Multi` (one texture per facing; west mirrors
//! east), `Graphic_Appearances` (walls by the stuff's appearance),
//! `Graphic_Random` (plants: one of a folder's textures per thing) and
//! linked atlases (walls, conduits). `CutoutComplex` textures are tinted
//! through their `m` mask.

use std::collections::HashMap;

use bevy::math::{Rect, UVec2};
use bevy::prelude::*;
use rimworld_assets::unity::{RgbaImage, TextureLibrary};
use rimworld_defs::ThingDef;
use rimworld_sim::Rot4;

use crate::graphics::to_bevy_image;

/// A loaded texture and its pixel size.
#[derive(Clone)]
pub struct Tex {
    pub image: Handle<Image>,
    pub size: UVec2,
}

/// Loaded thing textures (by path and mask colour) and folder listings.
#[derive(Resource, Default)]
pub struct ThingTextures {
    images: HashMap<(String, [u8; 3]), Option<Tex>>,
    folders: HashMap<String, Vec<String>>,
}

impl ThingTextures {
    /// The texture at `path`; with `mask`, its `…m` mask's red channel
    /// blends the colour in (`CutoutComplex`), else it is plain.
    pub fn get(
        &mut self,
        lib: &TextureLibrary,
        path: &str,
        mask: Option<Color>,
        images: &mut Assets<Image>,
    ) -> Option<Tex> {
        let key_color = mask.map_or([255, 255, 255], |c| {
            let s = c.to_srgba();
            [
                (s.red * 255.0) as u8,
                (s.green * 255.0) as u8,
                (s.blue * 255.0) as u8,
            ]
        });
        self.images
            .entry((path.to_owned(), key_color))
            .or_insert_with(|| {
                let mut mips = lib.load_mips(path).ok().flatten()?;
                if let Some(color) = mask
                    && let Some(mask_mips) = lib.load_mips(&format!("{path}m")).ok().flatten()
                {
                    apply_mask(&mut mips, &mask_mips, color);
                }
                let size = UVec2::new(mips[0].width, mips[0].height);
                Some(Tex {
                    image: images.add(to_bevy_image(&mips, false)),
                    size,
                })
            })
            .clone()
    }

    pub fn folder(&mut self, lib: &TextureLibrary, path: &str) -> Vec<String> {
        self.folders
            .entry(path.to_owned())
            .or_insert_with(|| lib.textures_in_folder(path))
            .clone()
    }
}

/// `CutoutComplex`: where the mask's red channel is set, the texture is
/// multiplied by the colour.
fn apply_mask(mips: &mut [RgbaImage], mask: &[RgbaImage], color: Color) {
    let c = color.to_srgba();
    let col = [c.red, c.green, c.blue];
    for (img, m) in mips.iter_mut().zip(mask) {
        if img.width != m.width || img.height != m.height {
            continue;
        }
        for (px, mp) in img.pixels.chunks_mut(4).zip(m.pixels.chunks(4)) {
            let w = mp[0] as f32 / 255.0;
            for k in 0..3 {
                let v = px[k] as f32 * (1.0 - w + w * col[k]);
                px[k] = v.clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// How to draw a thing: texture path, mirroring, rotation, size in cells,
/// and whether its colour goes through a mask.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub path: String,
    pub flip_x: bool,
    /// Clockwise rotation in radians.
    pub angle: f32,
    pub size: Vec2,
    pub masked: bool,
    /// Drawn as a linked atlas (walls, conduits).
    pub linked: bool,
}

fn rot_angle(rot: Rot4) -> f32 {
    use std::f32::consts::{FRAC_PI_2, PI};
    match rot {
        Rot4::North => 0.0,
        Rot4::East => FRAC_PI_2,
        Rot4::South => PI,
        Rot4::West => -FRAC_PI_2,
    }
}

/// Which texture `def` (made of `stuff`) shows at `rot`; `id` picks among
/// random variants.
// COMPATIBILITY TODO: currently approximate — `Graphic_Random` picks by
// thing id modulo the variant count, draw offsets, damage overlays,
// leafless/immature plant graphics and shadows are not reproduced.
pub fn look(
    tt: &mut ThingTextures,
    lib: &TextureLibrary,
    def: &ThingDef,
    stuff: Option<&ThingDef>,
    rot: Rot4,
    id: i32,
) -> Option<Look> {
    let g = def.graphic.as_ref()?;
    let base = g.tex_path.as_deref()?.trim();
    let size = Vec2::new(g.draw_size.0, g.draw_size.1);
    let masked = g.shader_type.as_deref() == Some("CutoutComplex");
    let linked = g.link_type.as_deref().is_some_and(|l| l != "None");
    let mut out = Look {
        path: base.to_owned(),
        flip_x: false,
        angle: 0.0,
        size,
        masked,
        linked,
    };
    match g.graphic_class.as_deref().unwrap_or("Graphic_Single") {
        "Graphic_Multi" => {
            let tex = |s: &str| format!("{base}_{s}");
            let (name, flip) = match rot {
                Rot4::North => (tex("north"), false),
                Rot4::South => (tex("south"), false),
                Rot4::East => (tex("east"), false),
                Rot4::West => (tex("west"), false),
            };
            let candidates: Vec<(String, bool)> = match rot {
                Rot4::North => vec![(name, flip), (tex("south"), false)],
                Rot4::South => vec![(name, flip), (tex("north"), false)],
                Rot4::East => vec![(name, flip), (tex("west"), true)],
                Rot4::West => vec![(name, flip), (tex("east"), true)],
            };
            let (path, flip) = candidates.into_iter().find(|(p, _)| lib.contains(p))?;
            out.path = path;
            out.flip_x = flip;
            if matches!(rot, Rot4::East | Rot4::West) {
                out.size = Vec2::new(size.y, size.x);
            }
        }
        "Graphic_Appearances" => {
            let appearance = stuff
                .and_then(|s| s.stuff_props.as_ref())
                .and_then(|p| p.appearance.as_deref())
                .unwrap_or("Smooth")
                .to_ascii_lowercase();
            let list = tt.folder(lib, base);
            let pick = list
                .iter()
                .find(|p| p.ends_with(&format!("_{appearance}")))
                .or_else(|| list.iter().find(|p| p.ends_with("_smooth")))
                .or_else(|| list.iter().find(|p| !p.contains("menuicon")))?;
            out.path = pick.clone();
        }
        "Graphic_Random" => {
            let list: Vec<String> = tt
                .folder(lib, base)
                .into_iter()
                .filter(|p| !p.ends_with('m') || !lib.contains(&p[..p.len() - 1]))
                .collect();
            if list.is_empty() {
                return None;
            }
            out.path = list[id.rem_euclid(list.len() as i32) as usize].clone();
        }
        _ => {
            if g.draw_rotated {
                out.angle = rot_angle(rot);
            }
        }
    }
    Some(out)
}

/// The colour a thing is drawn in: its stuff's colour, else the
/// graphic's colour, else white.
pub fn draw_color(def: &ThingDef, stuff: Option<&ThingDef>) -> Color {
    let c = stuff
        .and_then(|s| s.stuff_props.as_ref().and_then(|p| p.color))
        .or(def.graphic.as_ref().and_then(|g| g.color));
    c.map_or(Color::WHITE, |c| Color::srgba(c.r, c.g, c.b, c.a))
}

/// The pixel rectangle of linked-atlas tile `index` (4×4 tiles).
pub fn atlas_rect(size: UVec2, index: u32) -> Rect {
    let tw = size.x as f32 / 4.0;
    let th = size.y as f32 / 4.0;
    let col = (index % 4) as f32;
    let row_from_top = 3.0 - (index / 4) as f32;
    let inset = 0.5;
    Rect::new(
        col * tw + inset,
        row_from_top * th + inset,
        (col + 1.0) * tw - inset,
        (row_from_top + 1.0) * th - inset,
    )
}
