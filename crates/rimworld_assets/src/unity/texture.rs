//! `Texture2D` objects: header parsing and pixel decoding.
//!
//! Field order for Unity 2022.3 without a type tree (each `align` = pad to 4):
//!
//! ```text
//! string name; i32 forced_fallback_format; bool downscale_fallback;
//! bool is_alpha_channel_optional; align
//! i32 width, height, complete_image_size, mips_stripped, texture_format, mip_count
//! bool is_readable, is_preprocessed, ignore_mipmap_limit, streaming_mipmaps; align
//! i32 streaming_mipmaps_priority, image_count, texture_dimension
//! texture settings: i32 filter, i32 aniso, f32 mip_bias, i32 wrap_u, wrap_v, wrap_w
//! i32 lightmap_format, i32 color_space
//! string mipmap_limit_group_name          (2022.2+)
//! byte[] platform_blob; align
//! byte[] image_data; align                (empty when streamed)
//! streaming info: u64 offset, u32 size, string path (e.g. resources.assets.resS)
//! ```
//!
//! Unity stores rows bottom-up; [`decode`] returns top-down RGBA8.

use super::UnityError;
use super::reader::Reader;

/// Unity `TextureFormat` values we can decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    Alpha8,
    Rgb24,
    Rgba32,
    Argb32,
    Bgra32,
    Dxt1,
    Dxt5,
    Bc7,
    Other(i32),
}

impl TextureFormat {
    pub fn from_id(id: i32) -> Self {
        match id {
            1 => Self::Alpha8,
            3 => Self::Rgb24,
            4 => Self::Rgba32,
            5 => Self::Argb32,
            14 => Self::Bgra32,
            10 => Self::Dxt1,
            12 => Self::Dxt5,
            25 => Self::Bc7,
            other => Self::Other(other),
        }
    }

    /// Bytes needed for the top mip level.
    pub fn level0_size(self, width: u32, height: u32) -> Option<usize> {
        let (w, h) = (width as usize, height as usize);
        let blocks = w.div_ceil(4) * h.div_ceil(4);
        Some(match self {
            Self::Alpha8 => w * h,
            Self::Rgb24 => w * h * 3,
            Self::Rgba32 | Self::Argb32 | Self::Bgra32 => w * h * 4,
            Self::Dxt1 => blocks * 8,
            Self::Dxt5 | Self::Bc7 => blocks * 16,
            Self::Other(_) => return None,
        })
    }
}

/// Where a texture's pixels are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelSource<'a> {
    Inline(&'a [u8]),
    Stream {
        path: String,
        offset: u64,
        size: u32,
    },
}

#[derive(Debug, Clone)]
pub struct TextureHeader<'a> {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub mip_count: i32,
    pub pixels: PixelSource<'a>,
}

impl<'a> TextureHeader<'a> {
    pub fn parse(object: &'a [u8], big_endian: bool) -> Result<Self, UnityError> {
        let mut r = Reader::new(object, big_endian);
        let name = r.aligned_string()?;
        let _forced_fallback = r.i32()?;
        let _downscale_fallback = r.bool()?;
        let _alpha_optional = r.bool()?;
        r.align4();
        let width = r.i32()?;
        let height = r.i32()?;
        let _complete_size = r.i32()?;
        let _mips_stripped = r.i32()?;
        let format = TextureFormat::from_id(r.i32()?);
        let mip_count = r.i32()?;
        for _ in 0..4 {
            r.bool()?;
        }
        r.align4();
        let _priority = r.i32()?;
        let _image_count = r.i32()?;
        let _dimension = r.i32()?;
        r.bytes(6 * 4)?; // texture settings
        let _lightmap_format = r.i32()?;
        let _color_space = r.i32()?;
        let _mip_limit_group = r.aligned_string()?;
        let _platform_blob = r.aligned_bytes()?;
        let image = r.aligned_bytes()?;
        let offset = r.u64()?;
        let size = r.u32()?;
        let path = r.aligned_string()?;

        if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
            return Err(UnityError::Invalid(format!(
                "texture {name}: implausible size {width}x{height}"
            )));
        }
        let pixels = if image.is_empty() && !path.is_empty() {
            PixelSource::Stream { path, offset, size }
        } else {
            PixelSource::Inline(image)
        };
        Ok(Self {
            name,
            width: width as u32,
            height: height as u32,
            format,
            mip_count,
            pixels,
        })
    }
}

/// An RGBA8 image, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decodes the top mip level of `data` into top-down RGBA8.
pub fn decode(
    format: TextureFormat,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<RgbaImage, UnityError> {
    let need = format
        .level0_size(width, height)
        .ok_or_else(|| UnityError::Unsupported(format!("texture format {format:?}")))?;
    if data.len() < need {
        return Err(UnityError::Invalid(format!(
            "{format:?} {width}x{height} needs {need} bytes, have {}",
            data.len()
        )));
    }
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    match format {
        TextureFormat::Alpha8 => {
            for (i, &a) in data[..w * h].iter().enumerate() {
                out[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, a]);
            }
        }
        TextureFormat::Rgb24 => {
            for i in 0..w * h {
                out[i * 4..i * 4 + 3].copy_from_slice(&data[i * 3..i * 3 + 3]);
                out[i * 4 + 3] = 255;
            }
        }
        TextureFormat::Rgba32 => out.copy_from_slice(&data[..need]),
        TextureFormat::Argb32 => {
            for i in 0..w * h {
                let p = &data[i * 4..i * 4 + 4];
                out[i * 4..i * 4 + 4].copy_from_slice(&[p[1], p[2], p[3], p[0]]);
            }
        }
        TextureFormat::Bgra32 => {
            for i in 0..w * h {
                let p = &data[i * 4..i * 4 + 4];
                out[i * 4..i * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        TextureFormat::Dxt1 | TextureFormat::Dxt5 | TextureFormat::Bc7 => {
            super::bcn::decode_blocks(format, w, h, data, &mut out)?;
        }
        TextureFormat::Other(_) => unreachable!("rejected by level0_size"),
    }
    flip_rows(&mut out, w * 4);
    Ok(RgbaImage {
        width,
        height,
        pixels: out,
    })
}

/// Decodes up to `max_levels` mip levels stored back to back in `data`
/// (Unity's layout). Stops early if the data runs out.
pub fn decode_mips(
    format: TextureFormat,
    width: u32,
    height: u32,
    data: &[u8],
    max_levels: u32,
) -> Result<Vec<RgbaImage>, UnityError> {
    let mut levels = Vec::new();
    let (mut w, mut h, mut offset) = (width, height, 0usize);
    for _ in 0..max_levels.max(1) {
        let Some(size) = format.level0_size(w, h) else {
            return Err(UnityError::Unsupported(format!(
                "texture format {format:?}"
            )));
        };
        if offset + size > data.len() {
            break;
        }
        levels.push(decode(format, w, h, &data[offset..offset + size])?);
        offset += size;
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    if levels.is_empty() {
        return Err(UnityError::Invalid(format!(
            "no complete mip level for {format:?} {width}x{height}"
        )));
    }
    Ok(levels)
}

fn flip_rows(pixels: &mut [u8], stride: usize) {
    let rows = pixels.len() / stride;
    for y in 0..rows / 2 {
        let (top, bottom) = pixels.split_at_mut((rows - 1 - y) * stride);
        top[y * stride..(y + 1) * stride].swap_with_slice(&mut bottom[..stride]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_uncompressed_and_flips() {
        // 1x2 RGBA32: bottom row red, top row blue (Unity is bottom-up).
        let data = [255, 0, 0, 255, 0, 0, 255, 255];
        let img = decode(TextureFormat::Rgba32, 1, 2, &data).unwrap();
        assert_eq!(img.pixels, vec![0, 0, 255, 255, 255, 0, 0, 255]);
        let argb = decode(TextureFormat::Argb32, 1, 1, &[9, 1, 2, 3]).unwrap();
        assert_eq!(argb.pixels, vec![1, 2, 3, 9]);
        let bgra = decode(TextureFormat::Bgra32, 1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(bgra.pixels, vec![3, 2, 1, 4]);
    }

    #[test]
    fn rejects_short_or_unknown() {
        assert!(decode(TextureFormat::Rgba32, 2, 2, &[0; 15]).is_err());
        assert!(matches!(
            decode(TextureFormat::Other(99), 1, 1, &[0; 64]),
            Err(UnityError::Unsupported(_))
        ));
    }

    #[test]
    fn decodes_mip_chain() {
        // 2x2 RGBA32 (16 bytes) + 1x1 (4 bytes).
        let mut data = vec![10; 16];
        data.extend_from_slice(&[1, 2, 3, 4]);
        let mips = decode_mips(TextureFormat::Rgba32, 2, 2, &data, 8).unwrap();
        assert_eq!(mips.len(), 2);
        assert_eq!((mips[1].width, mips[1].height), (1, 1));
        assert_eq!(mips[1].pixels, vec![1, 2, 3, 4]);
        // Truncated data keeps complete levels only.
        assert_eq!(
            decode_mips(TextureFormat::Rgba32, 2, 2, &data[..18], 8)
                .unwrap()
                .len(),
            1
        );
        assert!(decode_mips(TextureFormat::Rgba32, 2, 2, &data[..3], 8).is_err());
    }

    #[test]
    fn level0_sizes() {
        assert_eq!(TextureFormat::Dxt1.level0_size(5, 4), Some(16));
        assert_eq!(TextureFormat::Dxt5.level0_size(8, 8), Some(64));
    }
}
