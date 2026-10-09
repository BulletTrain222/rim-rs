//! Per-thing tick staggering, matching the game's "hash interval tick"
//! behaviour (docs/research.md §12): periodic work for a thing runs when
//! `ticks + hash_offset(id)` is a multiple of the interval, so different
//! pawns do their periodic work on different ticks.

/// The game's hash combine for 32-bit ints. The middle sum is evaluated in
/// 64 bits (as the original's int + uint promotion does) and truncated.
pub fn hash_combine_int(seed: i32, value: i32) -> i32 {
    let shifted_left = seed.wrapping_shl(6) as i64;
    let shifted_right = (seed >> 2) as i64; // arithmetic shift
    let sum = value as i64 + 2_654_435_769_i64 + shifted_left + shifted_right;
    ((seed as i64) ^ sum) as i32
}

/// Offset applied to a thing's id for staggering periodic work.
pub fn hash_offset(id: i32) -> i32 {
    hash_combine_int(id, 169_495_093)
}

/// `TicksGame + hash_offset(id)` with 32-bit wrapping, as the game computes it.
pub fn hash_offset_ticks(tick: u64, id: i32) -> i32 {
    (tick as i32).wrapping_add(hash_offset(id))
}

/// Whether this thing's periodic tick for `interval` falls within the last
/// `delta` ticks: `|hash_offset_ticks mod interval| < delta`, using a
/// truncated (sign-keeping) remainder like the game.
pub fn is_hash_interval_tick_delta(tick: u64, id: i32, interval: i32, delta: i32) -> bool {
    (hash_offset_ticks(tick, id) % interval).abs() < delta
}

/// Single-tick form of [`is_hash_interval_tick_delta`].
pub fn is_hash_interval_tick(tick: u64, id: i32, interval: i64) -> bool {
    is_hash_interval_tick_delta(tick, id, interval as i32, 1)
}

/// Whether `(tick + offset) mod period == 0` (32-bit, like the game).
pub fn is_tick_interval(tick: u64, offset: i32, period: i32) -> bool {
    (tick as i32).wrapping_add(offset) % period == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_matches_hand_computation() {
        // seed 0: 0 ^ (value + 2654435769) truncated to i32.
        assert_eq!(hash_combine_int(0, 1), (2_654_435_770_i64 as u32) as i32);
        // seed -1: shifts of a negative number stay negative.
        let expected = ((-1_i64) ^ (5 + 2_654_435_769 + (-64) + (-1))) as i32;
        assert_eq!(hash_combine_int(-1, 5), expected);
    }

    #[test]
    fn matches_need_ticks_observed_in_the_game() {
        // A colonist with thingIDNumber 1046 had need intervals at these
        // ticks in the original game (docs/research.md §14).
        let hits: Vec<u64> = (0..600)
            .filter(|&t| is_hash_interval_tick(t, 1046, 150))
            .collect();
        assert_eq!(hits, vec![119, 269, 419, 569]);
        // With a 3-tick delta the window check still fires on the same tick
        // when evaluated on the pawn's 3-tick interval ticks.
        let off = hash_offset(1046);
        let hits3: Vec<u64> = (0..600)
            .filter(|&t| {
                is_tick_interval(t, off, 3) && is_hash_interval_tick_delta(t, 1046, 150, 3)
            })
            .collect();
        assert_eq!(hits3, vec![119, 269, 419, 569]);
    }

    #[test]
    fn exactly_one_tick_per_interval() {
        for id in [0, 1, 7, 12345, -3] {
            let hits = (0..150u64)
                .filter(|&t| is_hash_interval_tick(t, id, 150))
                .count();
            assert_eq!(hits, 1, "id {id}");
        }
        // Different ids generally land on different ticks.
        let tick_of = |id| (0..150u64).find(|&t| is_hash_interval_tick(t, id, 150));
        assert_ne!(tick_of(1), tick_of(2));
    }
}
