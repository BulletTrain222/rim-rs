//! The game's coherent-noise modules (docs/research.md §64): summed
//! gradient-coherent noise (`Perlin`) and the coordinate/value wrappers map
//! generation builds from it (displacement, scale, rotation, scale/bias,
//! multiplication). Everything evaluates in binary64, as the game does;
//! callers cast the final value to binary32.

use crate::noise_vectors::RANDOM_VECTORS;

/// Interpolation quality of gradient noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// t²(3 − 2t).
    Medium,
    /// 6t⁵ − 15t⁴ + 10t³.
    High,
}

/// A noise module: a value for every point.
pub trait Module {
    fn value(&self, x: f64, y: f64, z: f64) -> f64;

    /// `ModuleBase.GetValue(IntVec3)`: the module at the cell (y = 0), cast
    /// to binary32.
    fn at(&self, x: i32, z: i32) -> f32 {
        self.value(x as f64, 0.0, z as f64) as f32
    }
}

fn cubic(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

fn quintic(t: f64) -> f64 {
    let t3 = t * t * t;
    let t4 = t3 * t;
    let t5 = t4 * t;
    6.0 * t5 - 15.0 * t4 + 10.0 * t3
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
}

/// The gradient at lattice corner (ix, iy, iz) dotted with the offset to
/// the point, × 2.12. The gradient index mixes the corner and seed into
/// 32 bits, folds the high bits down by 8 and keeps 8 bits.
fn gradient(fx: f64, fy: f64, fz: f64, ix: i32, iy: i32, iz: i32, seed: u32) -> f64 {
    let mut n = 1619u32
        .wrapping_mul(ix as u32)
        .wrapping_add(31337u32.wrapping_mul(iy as u32))
        .wrapping_add(6971u32.wrapping_mul(iz as u32))
        .wrapping_add(1013u32.wrapping_mul(seed));
    n ^= n >> 8;
    let k = ((n & 0xFF) as usize) << 2;
    let (gx, gy, gz) = (
        RANDOM_VECTORS[k],
        RANDOM_VECTORS[k + 1],
        RANDOM_VECTORS[k + 2],
    );
    (gx * (fx - ix as f64) + gy * (fy - iy as f64) + gz * (fz - iz as f64)) * 2.12
}

/// The lattice cell below `q`: trunc(q) above zero, trunc(q) − 1 otherwise
/// (so exact zero and negative integers use the cell before).
fn lattice(q: f64) -> i32 {
    if q > 0.0 { q as i32 } else { q as i32 - 1 }
}

/// Gradient-coherent noise at a point: the eight corner gradients blended
/// along x, then y, then z.
pub fn gradient_coherent_noise_3d(x: f64, y: f64, z: f64, seed: u32, quality: Quality) -> f64 {
    let (x0, y0, z0) = (lattice(x), lattice(y), lattice(z));
    let (x1, y1, z1) = (x0 + 1, y0 + 1, z0 + 1);
    let s = |t: f64| match quality {
        Quality::Medium => cubic(t),
        Quality::High => quintic(t),
    };
    let (xs, ys, zs) = (s(x - x0 as f64), s(y - y0 as f64), s(z - z0 as f64));
    let g = |ix, iy, iz| gradient(x, y, z, ix, iy, iz, seed);
    let ix0 = lerp(g(x0, y0, z0), g(x1, y0, z0), xs);
    let ix1 = lerp(g(x0, y1, z0), g(x1, y1, z0), xs);
    let iy0 = lerp(ix0, ix1, ys);
    let ix0 = lerp(g(x0, y0, z1), g(x1, y0, z1), xs);
    let ix1 = lerp(g(x0, y1, z1), g(x1, y1, z1), xs);
    let iy1 = lerp(ix0, ix1, ys);
    lerp(iy0, iy1, zs)
}

/// Folds coordinates beyond ±2³⁰ back into range (`IEEERemainder`).
fn make_int32_range(v: f64) -> f64 {
    const LIMIT: f64 = 1_073_741_824.0;
    if v >= LIMIT {
        2.0 * ieee_remainder(v, LIMIT) - LIMIT
    } else if v <= -LIMIT {
        2.0 * ieee_remainder(v, LIMIT) + LIMIT
    } else {
        v
    }
}

/// `Math.IEEERemainder`: x − y × round-half-even(x / y).
fn ieee_remainder(x: f64, y: f64) -> f64 {
    x - y * (x / y).round_ties_even()
}

/// `Perlin`: octaves of gradient noise (seed + octave), the amplitude
/// starting at 1 and × persistence, coordinates × lacunarity, unnormalized.
#[derive(Debug, Clone)]
pub struct Perlin {
    pub frequency: f64,
    pub lacunarity: f64,
    pub persistence: f64,
    pub octaves: i32,
    pub seed: i32,
    pub quality: Quality,
}

impl Perlin {
    pub fn new(
        frequency: f64,
        lacunarity: f64,
        persistence: f64,
        octaves: i32,
        seed: i32,
        quality: Quality,
    ) -> Self {
        Self {
            frequency,
            lacunarity,
            persistence,
            octaves: octaves.clamp(1, 30),
            seed,
            quality,
        }
    }
}

impl Module for Perlin {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let (mut x, mut y, mut z) = (x * self.frequency, y * self.frequency, z * self.frequency);
        let mut sum = 0.0;
        let mut amplitude = 1.0;
        for i in 0..self.octaves {
            let seed = self.seed.wrapping_add(i) as u32;
            let signal = gradient_coherent_noise_3d(
                make_int32_range(x),
                make_int32_range(y),
                make_int32_range(z),
                seed,
                self.quality,
            );
            sum += signal * amplitude;
            x *= self.lacunarity;
            y *= self.lacunarity;
            z *= self.lacunarity;
            amplitude *= self.persistence;
        }
        sum
    }
}

/// A module × a constant (`Multiply` with `Const`).
pub struct Times<M>(pub M, pub f64);

impl<M: Module> Module for Times<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        self.0.value(x, y, z) * self.1
    }
}

/// `ScaleBias`: value × scale + bias.
pub struct ScaleBias<M> {
    pub source: M,
    pub scale: f64,
    pub bias: f64,
}

impl<M: Module> Module for ScaleBias<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        self.source.value(x, y, z) * self.scale + self.bias
    }
}

/// `Scale` with only x stretched (y and z × 1).
pub struct StretchX<M>(pub M, pub f64);

impl<M: Module> Module for StretchX<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        self.0.value(x * self.1, y * 1.0, z * 1.0)
    }
}

/// `Rotate` about the y axis only (x and z angles 0): the general
/// rotation matrix's terms, formed as the game forms them.
pub struct RotateY<M> {
    source: M,
    m: [f64; 9],
}

impl<M> RotateY<M> {
    pub fn new(source: M, y_degrees: f64) -> Self {
        let rad = std::f64::consts::PI / 180.0;
        let (xc, yc, zc) = (
            (0.0f64 * rad).cos(),
            (y_degrees * rad).cos(),
            (0.0f64 * rad).cos(),
        );
        let (xs, ys, zs) = (
            (0.0f64 * rad).sin(),
            (y_degrees * rad).sin(),
            (0.0f64 * rad).sin(),
        );
        let m = [
            ys * xs * zs + yc * zc,
            xc * zs,
            ys * zc - yc * xs * zs,
            ys * xs * zc - yc * zs,
            xc * zc,
            (0.0 - yc) * xs * zc - ys * zs,
            (0.0 - ys) * xc,
            xs,
            yc * xc,
        ];
        Self { source, m }
    }
}

impl<M: Module> Module for RotateY<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let m = &self.m;
        let nx = m[0] * x + m[1] * y + m[2] * z;
        let ny = m[3] * x + m[4] * y + m[5] * z;
        let nz = m[6] * x + m[7] * y + m[8] * z;
        self.source.value(nx, ny, nz)
    }
}

/// `MapNoiseUtility.AddDisplacementNoise`: x and z offset by two Medium
/// Perlins (seeds d and d + 1) × strength, y unchanged (+ 0).
pub struct Displace<M> {
    pub source: M,
    pub dx: Times<Perlin>,
    pub dz: Times<Perlin>,
}

impl<M> Displace<M> {
    pub fn new(source: M, frequency: f32, strength: f32, octaves: i32, seed: i32) -> Self {
        let p = |s: i32| {
            Times(
                Perlin::new(frequency as f64, 2.0, 0.5, octaves, s, Quality::Medium),
                strength as f64,
            )
        };
        Self {
            source,
            dx: p(seed),
            dz: p(seed.wrapping_add(1)),
        }
    }
}

impl<M: Module> Module for Displace<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let x2 = x + self.dx.value(x, y, z);
        let y2 = y + 0.0;
        let z2 = z + self.dz.value(x, y, z);
        self.source.value(x2, y2, z2)
    }
}

impl<M: Module + ?Sized> Module for Box<M> {
    fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        (**self).value(x, y, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The research's isolated native oracle: frequency 0.021 (double
    /// literal), 6 octaves, seed 123, High quality.
    #[test]
    fn perlin_matches_native_points() {
        let p = Perlin::new(0.021, 2.0, 0.5, 6, 123, Quality::High);
        assert_eq!(p.value(0.0, 0.0, 0.0), 0.0);
        assert_eq!(p.value(1.0, 0.0, 1.0), -0.024298296161892743);
        assert_eq!(p.at(1, 1).to_bits(), 0xBCC7_0D38);
        assert_eq!(p.value(20.0, 0.0, 20.0), -0.12352681106972256);
        assert_eq!(p.at(20, 20).to_bits(), 0xBDFC_FBA0);
    }

    /// The gradient table's fingerprint (1024 little-endian doubles).
    #[test]
    fn gradient_table_fingerprint() {
        let bytes: Vec<u8> = RANDOM_VECTORS
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            crate::sha256::hex(&bytes),
            "783B86DB486276222DEFDC756C4542DAE60CAA36FBDAD4316D523055459E7BA0"
        );
    }
}
