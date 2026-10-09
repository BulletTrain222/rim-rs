//! Differential test for the random number generator: compares our `Rand`
//! with outputs the original game's `Verse.Rand` produced in seeded scopes,
//! recorded by the local diagnostic mod (docs/research.md §17).
//!
//! The recording is game output and stays out of the repository; it is read
//! from `local/research/traces/` (or `FERROCOLONY_TRACES`). Skipped without it.

use std::path::{Path, PathBuf};

use rimworld_sim::rand::Rand;

fn trace_file() -> PathBuf {
    std::env::var_os("FERROCOLONY_TRACES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/research/traces")
        })
        .join("rand1/rand_probe.csv")
}

fn hex(v: f32) -> String {
    format!("{:08X}", v.to_bits())
}

#[test]
fn rand_matches_original_game_outputs() {
    let Ok(text) = std::fs::read_to_string(trace_file()) else {
        eprintln!("skipping: no RNG trace at {}", trace_file().display());
        return;
    };
    assert!(text.contains("#complete"), "incomplete recording");
    let mut checked = 0;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let mut parts = line.splitn(3, ',');
        let (kind, seed, data) = (
            parts.next().unwrap(),
            parts.next().unwrap().parse::<i32>().unwrap(),
            parts.next().unwrap(),
        );
        let game: Vec<&str> = data.split(';').collect();
        let mut r = Rand::new(seed as u32);
        let ours: Vec<String> = match kind {
            "int" => (0..game.len()).map(|_| r.int().to_string()).collect(),
            "value" => (0..game.len()).map(|_| hex(r.value())).collect(),
            "ranges" => {
                let f = r.range_f32(-10.0, 10.0);
                let g = r.range_f32(0.9, 1.0);
                let i = r.range(-7, 13);
                let wait = r.range_inclusive(125, 200);
                let chance = r.chance(0.3);
                vec![
                    hex(f),
                    hex(g),
                    i.to_string(),
                    wait.to_string(),
                    if chance { "True" } else { "False" }.to_owned(),
                    r.state().1.to_string(),
                ]
            }
            "mtb" => {
                let a = r.mtb_event_occurs(250.0, 60_000.0, 60.0);
                let after_a = r.state().1;
                let b = r.mtb_event_occurs(0.25, 60_000.0, 150.0);
                let cap = |v: bool| if v { "True" } else { "False" }.to_owned();
                vec![cap(a), after_a.to_string(), cap(b), r.state().1.to_string()]
            }
            "shuffle" => {
                let mut list: Vec<i32> = (0..7).collect();
                r.shuffle(&mut list);
                let after_shuffle = r.state().1;
                // ThinkNode_PrioritySorter's insertion pattern.
                let mut working: Vec<i32> = Vec::new();
                for i in 0..6 {
                    let at = r.range(0, working.len() as i32 - 1) as usize;
                    working.insert(at, i);
                }
                let join = |v: &[i32]| v.iter().map(i32::to_string).collect::<Vec<_>>().join(" ");
                vec![
                    join(&list),
                    after_shuffle.to_string(),
                    join(&working),
                    r.state().1.to_string(),
                ]
            }
            other => panic!("unknown row {other}"),
        };
        assert_eq!(ours, game, "{kind} seed {seed}");
        checked += 1;
    }
    eprintln!("{checked} recorded RNG rows matched");
    assert!(checked >= 18);
}
