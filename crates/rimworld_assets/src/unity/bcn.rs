//! Block-compressed texture decoding (BC1/DXT1, BC3/DXT5), written from the
//! public S3TC format description. Output is bottom-up RGBA8 (the caller
//! flips rows), matching Unity's storage order.

use super::UnityError;
use super::texture::TextureFormat;

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [
        ((r * 255 + 15) / 31) as u8,
        ((g * 255 + 31) / 63) as u8,
        ((b * 255 + 15) / 31) as u8,
    ]
}

/// The 4-colour palette of a BC1 block. `force_four` is set for BC2/BC3
/// colour blocks, which always use the 4-colour mode.
fn color_palette(block: &[u8], force_four: bool) -> [[u8; 4]; 4] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |wa: u32, wb: u32, d: u32| -> [u8; 4] {
        let f = |i: usize| ((a[i] as u32 * wa + b[i] as u32 * wb) / d) as u8;
        [f(0), f(1), f(2), 255]
    };
    if c0 > c1 || force_four {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(2, 1, 3),
            mix(1, 2, 3),
        ]
    } else {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(1, 1, 2),
            [0, 0, 0, 0],
        ]
    }
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (a0, a1) = (a0 as u32, a1 as u32);
    let mut p = [a0 as u8, a1 as u8, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for i in 1..7u32 {
            p[i as usize + 1] = (((7 - i) * a0 + i * a1) / 7) as u8;
        }
    } else {
        for i in 1..5u32 {
            p[i as usize + 1] = (((5 - i) * a0 + i * a1) / 5) as u8;
        }
        p[6] = 0;
        p[7] = 255;
    }
    p
}

pub fn decode_blocks(
    format: TextureFormat,
    width: usize,
    height: usize,
    data: &[u8],
    out: &mut [u8],
) -> Result<(), UnityError> {
    let block_size = match format {
        TextureFormat::Dxt1 => 8,
        TextureFormat::Dxt5 => 16,
        other => {
            return Err(UnityError::Unsupported(format!(
                "block format {other:?} not implemented yet"
            )));
        }
    };
    let bw = width.div_ceil(4);
    let bh = height.div_ceil(4);
    for by in 0..bh {
        for bx in 0..bw {
            let i = (by * bw + bx) * block_size;
            let block = &data[i..i + block_size];
            let mut texels = [[0u8; 4]; 16];
            match format {
                TextureFormat::Dxt1 => decode_color(block, false, &mut texels),
                _ => {
                    decode_color(&block[8..], true, &mut texels);
                    let pal = alpha_palette(block[0], block[1]);
                    let bits = block[2..8]
                        .iter()
                        .rev()
                        .fold(0u64, |acc, &b| (acc << 8) | b as u64);
                    for (t, texel) in texels.iter_mut().enumerate() {
                        texel[3] = pal[((bits >> (3 * t)) & 7) as usize];
                    }
                }
            }
            for (t, texel) in texels.iter().enumerate() {
                let (x, y) = (bx * 4 + t % 4, by * 4 + t / 4);
                if x < width && y < height {
                    let o = (y * width + x) * 4;
                    out[o..o + 4].copy_from_slice(texel);
                }
            }
        }
    }
    Ok(())
}

fn decode_color(block: &[u8], force_four: bool, texels: &mut [[u8; 4]; 16]) {
    let pal = color_palette(block, force_four);
    let idx = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    for (t, texel) in texels.iter_mut().enumerate() {
        let alpha = texel[3];
        *texel = pal[((idx >> (2 * t)) & 3) as usize];
        if force_four {
            texel[3] = alpha;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_solid_and_transparent() {
        // c0 = pure red (0xF800) > c1 = black; all indices 0 -> red.
        let block = [0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0];
        let mut out = vec![0; 4 * 4 * 4];
        decode_blocks(TextureFormat::Dxt1, 4, 4, &block, &mut out).unwrap();
        assert!(out.chunks(4).all(|p| p == [255, 0, 0, 255]));

        // c0 <= c1 selects 3-colour mode; index 3 = transparent black.
        let block = [0, 0, 0x00, 0xF8, 0xFF, 0xFF, 0xFF, 0xFF];
        decode_blocks(TextureFormat::Dxt1, 4, 4, &block, &mut out).unwrap();
        assert!(out.chunks(4).all(|p| p == [0, 0, 0, 0]));
    }

    #[test]
    fn bc1_interpolates() {
        // white / black, index 2 -> 2/3 white.
        let block = [0xFF, 0xFF, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA];
        let mut out = vec![0; 64];
        decode_blocks(TextureFormat::Dxt1, 4, 4, &block, &mut out).unwrap();
        assert_eq!(&out[..4], &[170, 170, 170, 255]);
    }

    #[test]
    fn bc3_alpha() {
        // alpha0 = 255, alpha1 = 0, all alpha indices 1 -> 0; colour white.
        let mut block = [255, 0, 0x49, 0x92, 0x24, 0x49, 0x92, 0x24].to_vec();
        block.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0, 0, 0]);
        let mut out = vec![0; 64];
        decode_blocks(TextureFormat::Dxt5, 4, 4, &block, &mut out).unwrap();
        assert!(out.chunks(4).all(|p| p == [255, 255, 255, 0]), "{out:?}");
    }

    #[test]
    fn partial_blocks_are_clipped() {
        let block = [0x00, 0xF8, 0, 0, 0, 0, 0, 0];
        let mut out = vec![0; 2 * 3 * 4];
        decode_blocks(TextureFormat::Dxt1, 2, 3, &block, &mut out).unwrap();
        assert!(out.chunks(4).all(|p| p == [255, 0, 0, 255]));
    }
}
