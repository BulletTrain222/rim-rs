//! The game's random number generator (`Verse.Rand`), docs/research.md §17.
//!
//! A counter-based hash: the state is a 32-bit seed and a 32-bit counter;
//! every draw hashes (seed, counter) with a MurmurHash3-style mix and then
//! advances the counter. Helpers consume draws exactly as the game's do
//! (including the cases that consume none), so a caller that draws in the
//! game's order sees the game's numbers.

/// MurmurHash3-style 32-bit mix of `input` under `seed`, with the game's
/// extra stream constant before the final avalanche (`MurmurHash.GetInt`).
pub fn murmur_hash(seed: u32, input: u32) -> i32 {
    let k = input
        .wrapping_mul(0xCC9E_2D51)
        .rotate_left(15)
        .wrapping_mul(0x1B87_3593);
    let mut h = (seed ^ k)
        .rotate_left(13)
        .wrapping_mul(5)
        .wrapping_add(0xE654_6B64);
    h ^= 0xA8F3_B65A;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h as i32
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rand {
    seed: u32,
    iterations: u32,
    /// Saved (seed, iterations) pairs (`PushState` / `PopState`).
    stack: Vec<(u32, u32)>,
}

impl Rand {
    /// A stream at counter 0 of `seed` (the game's `Seed` setter; the
    /// signed seed keeps its two's-complement bits).
    pub fn new(seed: u32) -> Self {
        Self {
            seed,
            iterations: 0,
            stack: Vec::new(),
        }
    }

    /// `Rand.ValueSeeded`: the first value of a stream seeded with `seed`,
    /// leaving any other stream untouched.
    pub fn value_seeded(seed: i32) -> f32 {
        Self::new(seed as u32).value()
    }

    /// The (seed, counter) pair, e.g. to compare with a recorded game.
    pub fn state(&self) -> (u32, u32) {
        (self.seed, self.iterations)
    }

    pub fn set_state(&mut self, seed: u32, iterations: u32) {
        self.seed = seed;
        self.iterations = iterations;
    }

    pub fn push_state(&mut self) {
        self.stack.push((self.seed, self.iterations));
    }

    /// Saves the current state and switches to counter 0 of `seed`.
    pub fn push_state_seeded(&mut self, seed: i32) {
        self.push_state();
        self.set_state(seed as u32, 0);
    }

    /// Restores the last saved state; `false` if nothing was saved.
    pub fn pop_state(&mut self) -> bool {
        match self.stack.pop() {
            Some((seed, iterations)) => {
                self.set_state(seed, iterations);
                true
            }
            None => false,
        }
    }

    /// One raw draw (`Rand.Int`).
    pub fn int(&mut self) -> i32 {
        let i = self.iterations;
        self.iterations = i.wrapping_add(1);
        murmur_hash(self.seed, i)
    }

    /// Uniform in [0, 1], both ends reachable (`Rand.Value`): the signed
    /// hash offset by 2^31, divided by 2^32 - 1 in double precision.
    pub fn value(&mut self) -> f32 {
        ((self.int() as f64 + 2_147_483_648.0) / 4_294_967_295.0) as f32
    }

    /// `Value < 0.5`.
    pub fn bool(&mut self) -> bool {
        self.value() < 0.5
    }

    /// Integer in `min..max` (`Rand.Range(int, int)`): no draw when
    /// `max <= min`; otherwise `min + |hash % (max - min)|` with a signed
    /// remainder (so a width of one still draws).
    pub fn range(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }
        let width = max.wrapping_sub(min);
        min.wrapping_add(self.int().wrapping_rem(width).wrapping_abs())
    }

    /// Integer in `min..=max` (`Rand.RangeInclusive`, `IntRange.RandomInRange`).
    pub fn range_inclusive(&mut self, min: i32, max: i32) -> i32 {
        if max <= min {
            return min;
        }
        self.range(min, max.wrapping_add(1))
    }

    /// Float in [min, max] (`Rand.Range(float, float)`, `FloatRange.RandomInRange`).
    /// No draw when `max <= min`. The game's runtime evaluates the
    /// expression with wider intermediates; double precision reproduces it.
    pub fn range_f32(&mut self, min: f32, max: f32) -> f32 {
        if max <= min {
            return min;
        }
        let u = self.value() as f64;
        (u * (max as f64 - min as f64) + min as f64) as f32
    }

    /// `Rand.Chance`: no draw for p <= 0 (false) or p >= 1 (true).
    pub fn chance(&mut self, p: f32) -> bool {
        if p <= 0.0 {
            false
        } else if p >= 1.0 {
            true
        } else {
            self.value() < p
        }
    }

    /// Descending Fisher-Yates (`GenList.Shuffle`): n - 1 draws.
    pub fn shuffle<T>(&mut self, list: &mut [T]) {
        let mut n = list.len();
        while n > 1 {
            n -= 1;
            let k = self.range_inclusive(0, n as i32) as usize;
            list.swap(k, n);
        }
    }

    /// Whether an event with mean time between occurrences `mtb` (in units
    /// of `unit` ticks) happens in a check covering `check_ticks` ticks
    /// (`Rand.MTBEventOccurs`, including its guard order). Small chances are
    /// rescaled by 8 until at least 1e-4, with one extra gate draw.
    pub fn mtb_event_occurs(&mut self, mtb: f32, unit: f32, check_ticks: f32) -> bool {
        if mtb == f32::INFINITY {
            return false;
        }
        if mtb == 0.0 {
            return true;
        }
        if mtb < 0.0 {
            return true; // the game logs an error
        }
        if unit <= 0.0 || check_ticks <= 0.0 {
            return false; // the game logs an error
        }
        let mut chance = check_ticks as f64 / (mtb as f64 * unit as f64);
        if chance <= 0.0 {
            return false;
        }
        if chance < 0.0001 {
            let mut gate = 1.0;
            while chance < 0.0001 {
                chance *= 8.0;
                gate /= 8.0;
            }
            if self.value() as f64 > gate {
                return false;
            }
        }
        (self.value() as f64) < chance
    }
}

/// `GenCollection.TryRandomElementByWeight` over a lazy sequence (not a
/// list): a weighted reservoir. Entries before the first positive weight
/// are skipped; that one becomes the choice without a draw. Every later
/// entry (zero weights included) draws `Range(0, total + weight)` and takes
/// over when the draw is at least the total so far. Offer entries in order;
/// work that happens between offers (e.g. availability checks that draw)
/// interleaves as in the game.
#[derive(Debug, Clone, Default)]
pub struct Reservoir<T> {
    total: f32,
    chosen: Option<T>,
}

impl<T> Reservoir<T> {
    pub fn new() -> Self {
        Self {
            total: 0.0,
            chosen: None,
        }
    }

    pub fn offer(&mut self, item: T, weight: f32, rng: &mut Rand) {
        let weight = weight.max(0.0);
        if self.chosen.is_none() {
            if weight > 0.0 {
                self.chosen = Some(item);
                self.total = weight;
            }
            return;
        }
        let total = self.total + weight;
        if rng.range_f32(0.0, total) >= self.total {
            self.chosen = Some(item);
        }
        self.total = total;
    }

    pub fn into_choice(self) -> Option<T> {
        self.chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(v: f32) -> u32 {
        v.to_bits()
    }

    #[test]
    fn hash_sequence_matches_the_game() {
        // (seed, first draws) as recorded in the game (research §17).
        let cases: [(i32, &[i32]); 6] = [
            (0, &[467335754, 1393367368, 1973359712, -1058127798]),
            (1, &[2037871401, 40068302, -1491675492, -1216309121]),
            (-1, &[-1789705223, 1945621784, 1668095287, 1291557104]),
            (12345, &[352537949, -708459585, -2040457364, 982514719]),
            (i32::MIN, &[2042623499, -272848092, -881858728, 1895216501]),
            (
                i32::MAX,
                &[-1940606903, -298158409, 1157310842, -1332763389],
            ),
        ];
        for (seed, expected) in cases {
            let mut r = Rand::new(seed as u32);
            let got: Vec<i32> = expected.iter().map(|_| r.int()).collect();
            assert_eq!(got, expected, "seed {seed}");
        }
    }

    #[test]
    fn values_match_the_game_bit_for_bit() {
        let mut r = Rand::new(0);
        assert_eq!(bits(r.value()), 0x3F1B_DAFA);
        assert_eq!(bits(r.value()), 0x3F53_0D19);
        let mut r = Rand::new(u32::MAX); // seed -1
        let v: Vec<u32> = (0..6).map(|_| bits(r.value())).collect();
        assert_eq!(v[5], 0x3886_9200);
        // Both ends are reachable.
        assert_eq!(Rand::new(-1_024_045_926i32 as u32).value(), 0.0);
        assert_eq!(Rand::new(1_210_403_497).value(), 1.0);
        assert_eq!(Rand::new(2_012_506_776).value(), 0.5);
        assert!(!Rand::new(2_012_506_776).bool(), "0.5 is not < 0.5");
    }

    #[test]
    fn counter_wraps() {
        let mut r = Rand::new(12345);
        r.set_state(12345, u32::MAX);
        assert_eq!(r.int(), -693_916_948);
        assert_eq!(r.state(), (12345, 0));
        assert_eq!(r.int(), 352_537_949);
    }

    #[test]
    fn ranges_and_draw_counts() {
        let fresh = || Rand::new(12345);
        let mut r = fresh();
        assert_eq!(r.range(-10, 10), -1);
        assert_eq!(r.state().1, 1);
        let mut r = fresh();
        assert_eq!(r.range(0, 1), 0);
        assert_eq!(r.state().1, 1, "width one still draws");
        let mut r = fresh();
        assert_eq!((r.range(5, 5), r.range(5, 2)), (5, 5));
        assert_eq!(r.state().1, 0);
        let mut r = fresh();
        assert_eq!(r.range_inclusive(-10, 10), -2);
        let mut r = fresh();
        assert_eq!(r.range_inclusive(0, i32::MAX), 0, "max + 1 wraps: no draw");
        assert_eq!(r.state().1, 0);
        // Float range: the game's wide evaluation (not f32 step by step).
        let mut r = fresh();
        assert_eq!(bits(r.range_f32(-10.0, 10.0)), 0x3FD2_2102);
        let mut r = fresh();
        assert_eq!(bits(r.range_f32(-0.1, 10.0)), 0x40B8_EDC4);
        let mut r = fresh();
        assert_eq!(bits(r.range_f32(-0.1, std::f32::consts::PI)), 0x3FE4_B835);
        let mut r = fresh();
        assert_eq!(r.range_f32(5.0, 5.0), 5.0);
        assert_eq!(r.state().1, 0);
    }

    #[test]
    fn chance_shortcuts() {
        let mut r = Rand::new(12345);
        assert!(!r.chance(-1.0) && !r.chance(0.0) && r.chance(1.0) && r.chance(2.0));
        assert_eq!(r.state().1, 0);
        assert!(!r.chance(0.5));
        assert_eq!(r.state().1, 1);
        assert!(!r.chance(f32::NAN));
        assert_eq!(r.state().1, 2);
    }

    #[test]
    fn mtb_guards_and_draws() {
        let mut r = Rand::new(12345);
        assert!(!r.mtb_event_occurs(f32::INFINITY, 1.0, 1.0));
        assert!(r.mtb_event_occurs(f32::NEG_INFINITY, 1.0, 1.0));
        assert!(r.mtb_event_occurs(-1.0, 1.0, 1.0));
        assert!(r.mtb_event_occurs(0.0, 1.0, 1.0));
        assert!(!r.mtb_event_occurs(1.0, 0.0, 1.0));
        assert!(!r.mtb_event_occurs(1.0, 1.0, 0.0));
        assert!(!r.mtb_event_occurs(1.0, f32::INFINITY, 1.0));
        assert_eq!(r.state().1, 0, "guards draw nothing");
        // Small chance: the gate rejects after one draw (seed 12345) or
        // passes and fails after two (seed 44).
        let mut r = Rand::new(12345);
        assert!(!r.mtb_event_occurs(250.0, 60_000.0, 60.0));
        assert_eq!(r.state().1, 1);
        let mut r = Rand::new(44);
        assert!(!r.mtb_event_occurs(250.0, 60_000.0, 60.0));
        assert_eq!(r.state().1, 2);
        let mut r = Rand::new(12345);
        assert!(r.mtb_event_occurs(1.0, 1.0, 1.0));
        assert_eq!(r.state().1, 1);
    }

    #[test]
    fn shuffle_matches_the_game() {
        let mut r = Rand::new(12345);
        let mut v = [0, 1, 2, 3, 4];
        r.shuffle(&mut v);
        assert_eq!(v, [0, 3, 2, 1, 4]);
        assert_eq!(r.state().1, 4);
        let mut r = Rand::new(12345);
        let mut two = [0, 1];
        r.shuffle(&mut two);
        assert_eq!((two, r.state().1), ([0, 1], 1));
    }

    #[test]
    fn nested_states_restore() {
        let mut r = Rand::new(12345);
        assert_eq!(r.int(), 352_537_949);
        r.push_state();
        assert_eq!(r.int(), -708_459_585);
        assert!(r.pop_state());
        assert_eq!(r.int(), -708_459_585, "the parent sees the same draw");
        r.push_state_seeded(-1);
        assert_eq!(r.int(), -1_789_705_223);
        assert!(r.pop_state());
        assert_eq!(r.state(), (12345, 2));
        assert!(!r.pop_state());
    }
}

#[cfg(test)]
mod reservoir_tests {
    use super::*;

    fn pick(seed: i32, weights: &[f32]) -> (Option<usize>, u32) {
        let mut rng = Rand::new(0);
        rng.push_state_seeded(seed);
        let mut r = Reservoir::new();
        for (i, &w) in weights.iter().enumerate() {
            r.offer(i, w, &mut rng);
        }
        (r.into_choice(), rng.state().1)
    }

    /// The mood report's fixtures: K (major candidates Wander_Psychotic 1,
    /// Tantrum 0.333) and M (three 0.5 weights), one draw per entry after
    /// the first.
    #[test]
    fn reservoir_matches_recorded_selections() {
        for (s, major, three) in [(1, 1, 1), (2, 1, 1), (3, 0, 2), (7, 1, 1), (33771, 1, 1)] {
            assert_eq!(pick(s, &[1.0, 0.333]), (Some(major), 1), "major seed {s}");
            assert_eq!(pick(s, &[0.5, 0.5, 0.5]), (Some(three), 2), "M seed {s}");
        }
        assert_eq!(pick(1, &[0.5]), (Some(0), 0));
        assert_eq!(pick(1, &[0.0, 0.0]), (None, 0));
    }
}
