//! Outdoor temperature over the day and the year (docs/research.md §29):
//! the world tile's average temperature, a seasonal swing that grows away
//! from the equator, and a daily sun cycle (`GenTemperature`,
//! `TileTemperaturesComp`).

use serde::{Deserialize, Serialize};

/// Ticks between refreshes of the cached outdoor temperature.
pub const CACHE_TICKS: u64 = 60;

/// A map's climate: what the world tile would supply.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Climate {
    /// The tile's average temperature (`Tile.temperature`).
    pub tile_temperature: f32,
}

/// `TemperatureTuning.SeasonalTempVariationCurve` over the normalized
/// distance from the equator.
fn seasonal_variation(distance: f32) -> f32 {
    const POINTS: [(f32, f32); 3] = [(0.0, 3.0), (0.1, 4.0), (1.0, 28.0)];
    if distance <= POINTS[0].0 {
        return POINTS[0].1;
    }
    for w in POINTS.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if distance <= x1 {
            return y0 + (y1 - y0) * (distance - x0) / (x1 - x0);
        }
    }
    POINTS[2].1
}

/// `GenTemperature.SeasonalShiftAmplitudeAt`: positive in the northern
/// hemisphere, negative in the southern. The distance from the equator is
/// the tile centre's height over the planet radius, |sin(latitude)|.
pub fn seasonal_amplitude(latitude: f32) -> f32 {
    let a = seasonal_variation(latitude.to_radians().sin().abs());
    if latitude >= 0.0 { a } else { -a }
}

/// `GenTemperature.OffsetFromSeasonCycle`: coldest in the middle of the
/// eleventh twelfth (northern winter), by the seasonal amplitude.
pub fn season_offset(abs_tick: i64, amplitude: f32) -> f32 {
    let year_pct = abs_tick as f32 / 60_000.0 % 60.0 / 60.0;
    // `Season.Winter.GetMiddleTwelfth(0)` is the eleventh twelfth (index
    // 10), which begins at 10/12 of the year.
    let winter = 10.0 / 12.0;
    (std::f32::consts::TAU * (year_pct - winter)).cos() * -amplitude
}

/// `GenDate.TimeZoneAt`: whole hours from longitude (15° an hour).
pub fn time_zone(longitude: f32) -> i64 {
    (longitude / 15.0).round_ties_even() as i64
}

/// `GenDate.DayPercent`: the local fraction of the day, never exactly 0
/// (tick 0 of the day counts as tick 1).
pub fn local_day_percent(abs_tick: i64, longitude: f32) -> f32 {
    let tick = (abs_tick + time_zone(longitude) * 2500).rem_euclid(60_000);
    tick.max(1) as f32 / 60_000.0
}

/// `GenTemperature.OffsetFromSunCycle`: ±7 °C over the local day, warmest
/// in the afternoon.
pub fn sun_offset(local_day_percent: f32) -> f32 {
    (std::f32::consts::TAU * (local_day_percent + 0.32)).cos() * 7.0
}

/// Outdoor temperature without the daily random variation.
// COMPATIBILITY TODO: currently approximate — the game adds a daily random
// variation (3-octave Perlin noise over the absolute tick, ×7 °C, seeded
// by the tile) and game-condition offsets; neither is modelled.
pub fn outdoor_temperature(
    climate: &Climate,
    latitude: f32,
    abs_tick: i64,
    local_day_percent: f32,
) -> f32 {
    climate.tile_temperature
        + season_offset(abs_tick.max(1), seasonal_amplitude(latitude))
        + sun_offset(local_day_percent)
}

/// `GenTemperature.AverageTemperatureAtTileForTwelfth`: the seasonal
/// temperature (no daily swing) averaged over 120 samples of the twelfth.
pub fn average_twelfth_temperature(climate: &Climate, latitude: f32, twelfth: i64) -> f32 {
    let amplitude = seasonal_amplitude(latitude);
    let start = 300_000 * twelfth.rem_euclid(12);
    let sum: f32 = (0..120)
        .map(|i| {
            let abs = start + 30_000 + (i as f32 / 120.0 * 300_000.0).round_ties_even() as i64;
            climate.tile_temperature + season_offset(abs.max(1), amplitude)
        })
        .sum();
    sum / 120.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seasons_swing_more_away_from_the_equator() {
        assert_eq!(seasonal_amplitude(0.0), 3.0);
        assert!(seasonal_amplitude(45.0) > 15.0);
        assert!(seasonal_amplitude(-45.0) < -15.0);
        // Northern winter (the eleventh twelfth) is the coldest.
        let amp = seasonal_amplitude(40.0);
        let mid_winter = (10.5 / 12.0 * 3_600_000.0) as i64;
        let mid_summer = (4.5 / 12.0 * 3_600_000.0) as i64;
        assert!(season_offset(mid_winter, amp) < -amp * 0.9);
        assert!(season_offset(mid_summer, amp) > amp * 0.9);
    }

    #[test]
    fn afternoons_are_warmest() {
        // cos(2π(p + 0.32)) peaks at p = 0.68 (about 16:19).
        assert!((sun_offset(0.68) - 7.0).abs() < 1e-4);
        assert!((sun_offset(0.18) + 7.0).abs() < 1e-4);
    }
}
