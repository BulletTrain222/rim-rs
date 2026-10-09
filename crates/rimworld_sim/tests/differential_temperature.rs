//! Differential check of the outdoor temperature against the temperature
//! probe trace (local, gitignored): the season and sun offsets every tick,
//! a year of season offsets, and the 60-tick cache of the sum. The daily
//! random variation (Perlin noise) is taken from the trace.

use std::path::{Path, PathBuf};

use rimworld_sim::climate::{
    CACHE_TICKS, Climate, local_day_percent, outdoor_temperature, season_offset,
    seasonal_amplitude, sun_offset,
};

fn trace_file(dir: &str, name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        });
    let p = base.join(dir).join(name);
    p.exists().then_some(p)
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn outdoor_temperature_matches_the_original_game() {
    let Some(path) = trace_file("temp1", "trace_temp_outdoor.csv") else {
        eprintln!("skipping: no temperature trace");
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let header = |k: &str| -> f32 {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("#{k},")))
            .unwrap()
            .parse()
            .unwrap()
    };
    let latitude = header("latitude");
    let longitude = header("longitude");
    let climate = Climate {
        tile_temperature: header("tileTemperature"),
    };
    let amplitude = seasonal_amplitude(latitude);
    assert!(
        close(amplitude, header("seasonalAmplitude"), 1e-4),
        "amplitude {amplitude} vs {}",
        header("seasonalAmplitude")
    );
    // A year of season (and sun) offsets.
    let mut year = 0;
    for l in text.lines().filter_map(|l| l.strip_prefix("#season,")) {
        let f: Vec<&str> = l.split(',').collect();
        let abs: i64 = f[0].parse().unwrap();
        let (season, sun): (f32, f32) = (f[1].parse().unwrap(), f[2].parse().unwrap());
        // The game's season at absolute tick 0 uses tick 1.
        let ours = season_offset(abs.max(1), amplitude);
        assert!(
            close(ours, season, 1e-4),
            "abs {abs}: season {ours} vs {season}"
        );
        let ours = sun_offset(local_day_percent(abs, longitude));
        assert!(close(ours, sun, 1e-4), "abs {abs}: sun {ours} vs {sun}");
        year += 1;
    }
    assert_eq!(year, 240);
    // Every tick: offsets, and the cached sum refreshed every 60 ticks.
    let rows: Vec<Vec<f32>> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|l| l.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    let mut refreshes = Vec::new();
    let mut last = f32::NAN;
    for r in &rows {
        let (tick, abs) = (r[0] as u64, r[1] as i64);
        let (outdoor, season, sun, daily) = (r[2], r[3], r[4], r[5]);
        let ours = season_offset(abs, amplitude);
        assert!(
            close(ours, season, 1e-4),
            "tick {tick}: season {ours} vs {season}"
        );
        let ours = sun_offset(local_day_percent(abs, longitude));
        assert!(close(ours, sun, 1e-4), "tick {tick}: sun {ours} vs {sun}");
        // The first logged value was cached before the probe started.
        if last.is_nan() {
            last = outdoor;
            continue;
        }
        if outdoor != last {
            // A refresh: the sum at this tick, with the game's noise.
            let ours =
                outdoor_temperature(&climate, latitude, abs, local_day_percent(abs, longitude))
                    + daily;
            assert!(
                close(ours, outdoor, 1e-4),
                "tick {tick}: {ours} vs {outdoor}"
            );
            refreshes.push(tick);
            last = outdoor;
        }
    }
    // Steady 60-tick refreshes once running.
    let steady: Vec<u64> = refreshes.windows(2).skip(2).map(|w| w[1] - w[0]).collect();
    assert!(steady.iter().all(|&d| d == CACHE_TICKS), "{refreshes:?}");
    eprintln!(
        "temperature: {} ticks, {} refreshes match",
        rows.len(),
        refreshes.len()
    );
}
