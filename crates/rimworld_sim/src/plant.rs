//! Plants (docs/research.md §24): growth (`Plant.TickLong`), the growth
//! rate factors (`PlantUtility`), harvest yield, and the sun's glow
//! (`GenCelestial`) that drives growth outdoors.

use rimworld_defs::{DefId, PlantProperties, ThingDef};

use crate::grid::Cell;
use crate::map::ItemId;
use crate::rand::Rand;

pub const TICKS_PER_DAY: f32 = 60_000.0;
/// Plants grow in long ticks of this many ticks.
pub const LONG_TICK: i64 = 2000;
/// Damage per tick of a dying plant (`BaseDyingDamagePerTick`).
const DYING_DAMAGE_PER_TICK: f32 = 0.005;

/// A plant on the map (`Plant`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Plant {
    pub id: ItemId,
    pub def: DefId<ThingDef>,
    pub position: Cell,
    /// `growthInt`, 0..1.
    pub growth: f32,
    /// `ageInt` in ticks.
    pub age: i64,
    /// Sown by a pawn (not wild).
    pub sown: bool,
    pub hit_points: f32,
    pub max_hit_points: f32,
    /// `thingIDNumber`: plants tick long when `TicksGame % 2000` equals it
    /// modulo 2000 (the long tick list's bucket).
    #[serde(default)]
    pub id_number: i32,
    /// Tick it last lost its leaves to the cold (`madeLeaflessTick`).
    #[serde(default = "never_leafless")]
    pub made_leafless_tick: i64,
}

fn never_leafless() -> i64 {
    -99_999
}

impl Plant {
    /// `LeaflessNow`: within a day of losing its leaves.
    pub fn leafless_now(&self, now: i64) -> bool {
        now - self.made_leafless_tick < 60_000
    }
}

/// `Plant.LeaflessTemperatureThresh`: the minimum growth temperature
/// minus 10–18 °C, fixed per plant by its thing id.
pub fn leafless_temperature(props: &PlantProperties, id_number: i32) -> f32 {
    let v = crate::rand::Rand::value_seeded(id_number ^ 0x31F3_A5C1_u32 as i32);
    props.min_growth_temperature + (v * 8.0 - 18.0)
}

/// `PlantLifeStage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeStage {
    Sowing,
    Growing,
    Mature,
}

impl Plant {
    pub fn life_stage(&self) -> LifeStage {
        if self.growth < 0.0001 {
            LifeStage::Sowing
        } else if self.growth > 0.999 {
            LifeStage::Mature
        } else {
            LifeStage::Growing
        }
    }

    /// `HarvestableNow`.
    pub fn harvestable_now(&self, props: &PlantProperties) -> bool {
        props.harvestable() && self.growth > props.harvest_min_growth
    }

    /// `YieldNow` (one random draw): the yield scaled by growth past the
    /// harvest threshold and by hit points.
    // COMPATIBILITY TODO: currently approximate — the difficulty's crop
    // yield factor is taken as 1; blight is not modelled.
    pub fn yield_now(&self, props: &PlantProperties, rng: &mut Rand) -> u32 {
        if !props.harvestable() {
            return 0;
        }
        let t = inverse_lerp(props.harvest_min_growth, 1.0, self.growth);
        let mut y = props.harvest_yield * (0.5 + t * 0.5);
        y *= lerp(0.5, 1.0, self.hit_points / self.max_hit_points.max(1.0));
        round_random(y, rng)
    }
}

/// `GenMath.RoundRandom`.
pub fn round_random(v: f32, rng: &mut Rand) -> u32 {
    let whole = v.trunc();
    whole.max(0.0) as u32 + u32::from(rng.value() < v - whole)
}

/// `Mathf.InverseLerp` (clamped).
pub fn inverse_lerp(a: f32, b: f32, v: f32) -> f32 {
    if a != b {
        ((v - a) / (b - a)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// `PlantUtility.GrowthRateFactorFor_Fertility`.
pub fn fertility_factor(props: &PlantProperties, fertility: f32) -> f32 {
    if props.completely_ignore_fertility {
        1.0
    } else {
        fertility * props.fertility_sensitivity + (1.0 - props.fertility_sensitivity)
    }
}

/// `PlantUtility.GrowthRateFactorFor_Light` (`GenMath.InverseLerp`,
/// which is clamped).
pub fn light_factor(props: &PlantProperties, glow: f32) -> f32 {
    if props.grow_min_glow == props.grow_optimal_glow && glow == props.grow_optimal_glow {
        1.0
    } else {
        inverse_lerp(props.grow_min_glow, props.grow_optimal_glow, glow)
    }
}

/// `PlantUtility.GrowthRateFactorFor_Temperature`.
pub fn temperature_factor(props: &PlantProperties, temp: f32) -> f32 {
    if temp < props.min_optimal_growth_temperature {
        inverse_lerp(
            props.min_growth_temperature,
            props.min_optimal_growth_temperature,
            temp,
        )
    } else if temp > props.max_optimal_growth_temperature {
        inverse_lerp(
            props.max_growth_temperature,
            props.max_optimal_growth_temperature,
            temp,
        )
    } else {
        1.0
    }
}

/// `PlantUtility.GrowthSeasonNow` for a cell at `temp`.
pub fn growth_season_now(props: &PlantProperties, temp: f32) -> bool {
    temp > props.min_growth_temperature && temp < props.max_growth_temperature
}

/// `Plant.Resting`: plants rest before 25% and after 80% of the day.
pub fn resting(day_percent: f32) -> bool {
    !(0.25..=0.8).contains(&day_percent)
}

/// Conditions a plant grows under.
#[derive(Debug, Clone, Copy)]
pub struct GrowthConditions {
    /// `TicksGame`.
    pub now: i64,
    pub fertility: f32,
    pub glow: f32,
    pub temperature: f32,
    pub day_percent: f32,
}

/// `Plant.TickLong`: growth (in the growth season), ageing and dying of old
/// age. Returns `false` when the plant died.
// COMPATIBILITY TODO: currently approximate — leafless/cold damage, blight,
// pollution, vacuum, noxious haze, drought and unlit death are not
// modelled; rot damage is applied as plain hit-point loss.
pub fn tick_long(plant: &mut Plant, props: &PlantProperties, c: GrowthConditions) -> bool {
    if !check_make_leafless(plant, props, c.temperature, c.now) {
        return false;
    }
    if growth_season_now(props, c.temperature) {
        let per_tick = if plant.life_stage() != LifeStage::Growing || resting(c.day_percent) {
            0.0
        } else {
            1.0 / (TICKS_PER_DAY * props.grow_days)
                * fertility_factor(props, c.fertility)
                * temperature_factor(props, c.temperature)
                * light_factor(props, c.glow)
        };
        plant.growth = (plant.growth + per_tick * LONG_TICK as f32).min(1.0);
    }
    plant.age += LONG_TICK;
    if props.limited_lifespan() && plant.age > props.lifespan_ticks() {
        plant.hit_points -= (DYING_DAMAGE_PER_TICK * LONG_TICK as f32).ceil();
        if plant.hit_points <= 0.0 {
            return false;
        }
    }
    true
}

/// `Plant.CheckMakeLeafless` for the cold: below its leafless temperature
/// a plant loses its leaves, or dies if it can't survive that (most
/// crops). Returns `false` when it died.
// COMPATIBILITY TODO: currently approximate — every plant counts as
// outdoors (the game only checks plants whose room uses the outdoor
// temperature); pollution leaflessness is not modelled.
pub fn check_make_leafless(
    plant: &mut Plant,
    props: &PlantProperties,
    temperature: f32,
    now: i64,
) -> bool {
    if temperature < leafless_temperature(props, plant.id_number) {
        if props.die_if_leafless {
            return false;
        }
        plant.made_leafless_tick = now;
    }
    true
}

// ---------------------------------------------------------------- sun glow

type V3 = [f32; 3];

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn normalized(a: V3) -> V3 {
    let m = dot(a, a).sqrt();
    if m > 1e-5 {
        scale(a, 1.0 / m)
    } else {
        [0.0; 3]
    }
}

/// `Quaternion.AngleAxis(degrees, axis) * v` (axis normalized).
fn rotate(v: V3, degrees: f32, axis: V3) -> V3 {
    let k = normalized(axis);
    let (s, c) = degrees.to_radians().sin_cos();
    add(
        add(scale(v, c), scale(cross(k, v), s)),
        scale(k, dot(k, v) * (1.0 - c)),
    )
}

/// `Vector3.RotateTowards` for unit vectors with an unlimited magnitude
/// change.
fn rotate_towards(current: V3, target: V3, max_radians: f32) -> V3 {
    let (c, t) = (normalized(current), normalized(target));
    let d = dot(c, t);
    if d > 1.0 - 1e-6 {
        return target;
    }
    let angle = d.clamp(-1.0, 1.0).acos();
    let axis = normalized(cross(c, t));
    rotate(c, max_radians.min(angle).to_degrees(), axis)
}

/// A two-point `SimpleCurve` (clamped outside its points).
fn curve2(x: f32, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> f32 {
    if x <= x0 {
        y0
    } else if x >= x1 {
        y1
    } else {
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    }
}

/// `GenCelestial.CelestialSunGlowPercent`: 0 at night, 1 in full sun.
pub fn sun_glow(latitude: f32, day_of_year: i32, day_percent: f32) -> f32 {
    let normal = rotate([1.0, 0.0, 0.0], latitude, [0.0, 0.0, 1.0]);
    // `SunPositionUnmodified` with the initial position (1, 0, 0).
    let mut v: V3 = [100.0, 0.0, 0.0];
    let season = -(day_of_year as f32 / 60.0 * std::f32::consts::TAU).cos();
    v[1] += season * 100.0 * curve2(latitude, (70.0, 0.2), (75.0, 1.5));
    let sun = normalized(rotate(v, (day_percent - 0.5) * 360.0, [0.0, 1.0, 0.0]));
    // `SunPosition`: peek around towards the surface normal.
    let peek = curve2(latitude, (70.0, 1.0), (75.0, 0.05));
    let mut sun = rotate_towards(sun, normal, std::f32::consts::PI * 19.0 / 180.0 * peek);
    let low = inverse_lerp(60.0, 0.0, latitude.abs());
    if low > 0.0 {
        sun = rotate_towards(sun, normal, std::f32::consts::TAU * (17.0 * low / 360.0));
    }
    let sun = normalized(sun);
    inverse_lerp(0.0, 0.7, dot(normalized(normal), sun))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_is_up_at_noon_and_down_at_midnight() {
        for lat in [0.0, 30.0, -45.0, 60.0] {
            for day in [0, 15, 30, 45] {
                assert!(sun_glow(lat, day, 0.5) > 0.7, "noon {lat} {day}");
                // High latitudes keep a little light in summer nights.
                assert!(sun_glow(lat, day, 0.0) < 0.05, "midnight {lat} {day}");
            }
        }
        // Morning is darker than noon.
        assert!(sun_glow(30.0, 10, 0.3) < sun_glow(30.0, 10, 0.5));
    }

    #[test]
    fn resting_hours() {
        assert!(resting(0.2));
        assert!(!resting(0.25));
        assert!(!resting(0.8));
        assert!(resting(0.81));
    }
}
