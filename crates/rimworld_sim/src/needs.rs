//! Pawn needs: Food, Rest and Joy (recreation), implementing the behaviour
//! of the game's `Need_Food` / `Need_Rest` (docs/research.md §12) and
//! `Need_Joy` (§57).
//!
//! NeedDefs, StatDefs and race properties come from the game data; the
//! per-class rules (rates, categories, multipliers) are implemented here.

use rimworld_defs::{DefId, GameDefs, NeedDef, ThingDef};

use crate::rand::Rand;

/// Needs run their interval logic every 150 ticks (hash-staggered per pawn).
pub const NEED_INTERVAL_TICKS: i64 = 150;
const INTERVAL: f32 = NEED_INTERVAL_TICKS as f32;

const BASE_FOOD_FALL_PER_TICK: f32 = 2.666_666_7e-5;
const BASE_REST_FALL_PER_TICK: f32 = 1.583_333_3e-5;
/// Rest gained per interval while resting at effectiveness 1.0 (10.5 h).
const REST_GAIN_PER_INTERVAL: f32 = 0.005_714_286;

pub const REST_EXHAUSTED: f32 = 0.01;
pub const REST_VERY_TIRED: f32 = 0.14;
pub const REST_TIRED: f32 = 0.28;
/// A lying pawn falls asleep below this rest level.
pub const FALL_ASLEEP_MAX_LEVEL: f32 = 0.75;
/// A sleeping pawn wakes at or above this rest level.
pub const WAKE_THRESHOLD: f32 = 1.0;

/// `FoodLevelPercentageWantEat` when the race is unknown (omnivores).
pub const DEFAULT_FOOD_WANT_EAT: f32 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NeedKind {
    Food,
    Rest,
    Joy,
}

impl NeedKind {
    pub fn from_need_class(class: &str) -> Option<Self> {
        match class {
            "Need_Food" => Some(Self::Food),
            "Need_Rest" => Some(Self::Rest),
            "Need_Joy" => Some(Self::Joy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RestCategory {
    Rested,
    Tired,
    VeryTired,
    Exhausted,
}

impl RestCategory {
    pub fn of(level: f32) -> Self {
        if level < REST_EXHAUSTED {
            Self::Exhausted
        } else if level < REST_VERY_TIRED {
            Self::VeryTired
        } else if level < REST_TIRED {
            Self::Tired
        } else {
            Self::Rested
        }
    }

    fn fall_factor(self) -> f32 {
        match self {
            Self::Rested => 1.0,
            Self::Tired => 0.7,
            Self::VeryTired => 0.3,
            Self::Exhausted => 0.6,
        }
    }
}

/// Ordered like the game's enum (Fed < Hungry < UrgentlyHungry < Starving).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HungerCategory {
    Fed,
    Hungry,
    UrgentlyHungry,
    Starving,
}

impl HungerCategory {
    /// Category for a food level fraction, given the race's
    /// `FoodLevelPercentageWantEat`.
    pub fn of(percent: f32, want_eat: f32) -> Self {
        if percent <= 0.0 {
            Self::Starving
        } else if percent < want_eat * 0.4 {
            Self::UrgentlyHungry
        } else if percent < want_eat * 0.8 {
            Self::Hungry
        } else {
            Self::Fed
        }
    }

    fn multiplier(self) -> f32 {
        match self {
            Self::Fed => 1.0,
            Self::Hungry => 0.5,
            Self::UrgentlyHungry => 0.25,
            Self::Starving => 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Need {
    pub def: DefId<NeedDef>,
    pub kind: NeedKind,
    pub level: f32,
    pub max: f32,
    /// Food only: the race's `FoodLevelPercentageWantEat`.
    pub want_eat: f32,
}

impl Need {
    pub fn percent(&self) -> f32 {
        if self.max > 0.0 {
            self.level / self.max
        } else {
            0.0
        }
    }

    pub fn hunger_category(&self) -> HungerCategory {
        HungerCategory::of(self.percent(), self.want_eat)
    }

    /// UI category label, e.g. "tired", "hungry" (none when fine).
    pub fn category(&self) -> Option<&'static str> {
        match self.kind {
            NeedKind::Rest => match RestCategory::of(self.level) {
                RestCategory::Rested => None,
                RestCategory::Tired => Some("tired"),
                RestCategory::VeryTired => Some("very tired"),
                RestCategory::Exhausted => Some("exhausted"),
            },
            NeedKind::Food => match self.hunger_category() {
                HungerCategory::Fed => None,
                HungerCategory::Hungry => Some("hungry"),
                HungerCategory::UrgentlyHungry => Some("urgently hungry"),
                HungerCategory::Starving => Some("starving"),
            },
            NeedKind::Joy => match JoyCategory::of(self.level) {
                JoyCategory::Empty => Some("recreation-starved"),
                JoyCategory::VeryLow => Some("recreation very low"),
                JoyCategory::Low => Some("recreation low"),
                _ => None,
            },
        }
    }
}

/// Race- and stat-derived constants for a pawn's needs.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NeedRates {
    /// race `baseHungerRate` × life-stage `hungerRateFactor`.
    pub hunger_rate: f32,
    pub rest_fall_rate_factor: f32,
    pub rest_rate_multiplier: f32,
}

/// The pawn's needs, in NeedDef `listPriority` order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Needs {
    pub list: Vec<Need>,
    pub rates: NeedRates,
    /// Ticks spent with rest at (near) zero.
    pub rest_ticks_at_zero: u32,
    /// The joy need's tolerances and last gain.
    #[serde(default)]
    pub joy: JoyState,
}

/// `Need_Joy.CurCategory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JoyCategory {
    Empty,
    VeryLow,
    Low,
    Satisfied,
    High,
    Extreme,
}

impl JoyCategory {
    pub fn of(level: f32) -> Self {
        if level < 0.01 {
            Self::Empty
        } else if level < 0.15 {
            Self::VeryLow
        } else if level < 0.3 {
            Self::Low
        } else if level < 0.7 {
            Self::Satisfied
        } else if level < 0.85 {
            Self::High
        } else {
            Self::Extreme
        }
    }

    /// `FallPerInterval`.
    pub fn fall_per_interval(self) -> f32 {
        match self {
            Self::VeryLow => 0.0006,
            Self::Low => 0.00105,
            _ => 0.0015,
        }
    }
}

/// `Need_Joy`'s state beyond its level: `JoyToleranceSet` (tolerance and
/// boredom per `JoyKindDef`) and `lastGainTick`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct JoyState {
    pub tolerances: std::collections::BTreeMap<String, f32>,
    pub bored: std::collections::BTreeSet<String>,
    /// `lastGainTick` (not in the game's save data: a loaded need starts
    /// at its constructor's −999).
    #[serde(skip)]
    pub last_gain_tick: Option<u64>,
}

impl JoyState {
    pub fn tolerance(&self, kind: &str) -> f32 {
        self.tolerances.get(kind).copied().unwrap_or(0.0)
    }

    pub fn bored_of(&self, kind: &str) -> bool {
        self.bored.contains(kind)
    }

    /// `GainingJoy`: joy gained within the last 15 ticks.
    pub fn gaining(&self, tick: u64) -> bool {
        self.last_gain_tick.is_some_and(|g| tick < g + 15)
    }

    /// `JoyToleranceSet.NeedInterval`: every tolerance drops by `drop`;
    /// boredom ends below 0.3.
    pub fn drop_tolerances(&mut self, drop: f32) {
        for (kind, t) in self.tolerances.iter_mut() {
            *t = ((*t as f64 - drop as f64) as f32).max(0.0);
            if *t < 0.3 {
                self.bored.remove(kind);
            }
        }
    }
}

impl Needs {
    /// Creates the needs we implement for a pawn of race `race`, with the
    /// game's initial levels (rest 0.9–1.0, humanlike food 0.8).
    pub fn for_race(defs: &GameDefs, race: &ThingDef, colonist: bool, rng: &mut Rand) -> Self {
        let props = race.race.as_ref();
        let body_size = props.map_or(1.0, |r| r.base_body_size);
        let humanlike = props.and_then(|r| r.intelligence.as_deref()) == Some("Humanlike");
        let want_eat = props.map_or(DEFAULT_FOOD_WANT_EAT, |r| {
            r.food_level_percentage_want_eat()
        });
        // COMPATIBILITY TODO: currently approximate — life stages are not
        // modelled; adult factors (hungerRateFactor/foodMaxFactor = 1) are
        // assumed, and max food uses body size instead of the MaxNutrition stat.
        let rates = NeedRates {
            hunger_rate: props.map_or(1.0, |r| r.base_hunger_rate),
            rest_fall_rate_factor: defs.base_stat(race, "RestFallRateFactor").unwrap_or(1.0),
            rest_rate_multiplier: defs.base_stat(race, "RestRateMultiplier").unwrap_or(1.0),
        };
        let mut list: Vec<(i32, Need)> = defs
            .needs
            .iter()
            .filter_map(|(id, def)| {
                let kind = NeedKind::from_need_class(def.need_class.as_deref()?)?;
                // Joy: humanlike colonists only (`minIntelligence`,
                // `colonistsOnly`).
                if kind == NeedKind::Joy && !(humanlike && colonist) {
                    return None;
                }
                let (max, start) = match kind {
                    NeedKind::Food => {
                        let pct = if humanlike {
                            0.8
                        } else {
                            rng.range_f32(0.5, 0.9)
                        };
                        (body_size, pct * body_size)
                    }
                    NeedKind::Rest => (1.0, rng.range_f32(0.9, 1.0)),
                    NeedKind::Joy => (1.0, rng.range_f32(0.5, 0.6)),
                };
                Some((
                    def.list_priority,
                    Need {
                        def: id,
                        kind,
                        level: start,
                        max,
                        want_eat,
                    },
                ))
            })
            .collect();
        list.sort_by_key(|(prio, _)| -prio);
        Self {
            list: list.into_iter().map(|(_, n)| n).collect(),
            rates,
            rest_ticks_at_zero: 0,
            joy: JoyState::default(),
        }
    }

    /// `Need_Joy.GainJoy`: the amount shrinks with the kind's tolerance and
    /// is capped at a full need; the tolerance grows by 0.65 × the gain
    /// (boredom above 0.5).
    ///
    /// Float steps follow the game's runtime: each expression is evaluated
    /// wide and rounded to binary32 when stored.
    pub fn gain_joy(&mut self, amount: f32, kind: Option<&str>, tick: u64) {
        if amount <= 0.0 {
            return;
        }
        let factor: f32 = kind.map_or(1.0, |k| 1.0 - self.joy.tolerance(k));
        let Some(need) = self.get_mut(NeedKind::Joy) else {
            return;
        };
        let scaled = (amount as f64 * factor as f64) as f32;
        let room = (1.0f64 - need.level as f64) as f32;
        let gained = scaled.min(room);
        need.level = (need.level as f64 + gained as f64) as f32;
        if let Some(k) = kind {
            let t = (self.joy.tolerance(k) as f64 + gained as f64 * 0.65f32 as f64).min(1.0) as f32;
            self.joy.tolerances.insert(k.to_owned(), t);
            if t > 0.5 {
                self.joy.bored.insert(k.to_owned());
            }
        }
        self.joy.last_gain_tick = Some(tick);
    }

    /// `Need_Joy.NeedInterval` (not while asleep): tolerances drop by the
    /// expectation's daily rate; without a gain in the last 15 ticks the
    /// level falls by its category's rate × `JoyFallRateFactor`.
    pub fn joy_interval(&mut self, tick: u64, tolerance_drop_per_day: f32, fall_factor: f32) {
        if self.get(NeedKind::Joy).is_none() {
            return;
        }
        let drop = (tolerance_drop_per_day as f64 * 150.0 / 60_000.0) as f32;
        self.joy.drop_tolerances(drop);
        let gaining = self.joy.gaining(tick);
        if let Some(need) = self.get_mut(NeedKind::Joy)
            && !gaining
        {
            let fall = JoyCategory::of(need.level).fall_per_interval();
            let level = (need.level as f64 - fall as f64 * fall_factor as f64) as f32;
            need.level = level.clamp(0.0, need.max);
        }
    }

    pub fn get(&self, kind: NeedKind) -> Option<&Need> {
        self.list.iter().find(|n| n.kind == kind)
    }

    pub fn get_mut(&mut self, kind: NeedKind) -> Option<&mut Need> {
        self.list.iter_mut().find(|n| n.kind == kind)
    }

    pub fn rest_level(&self) -> Option<f32> {
        self.get(NeedKind::Rest).map(|r| r.level)
    }

    pub fn is_starving(&self) -> bool {
        self.get(NeedKind::Food)
            .is_some_and(|f| f.hunger_category() == HungerCategory::Starving)
    }

    /// Runs one need interval. `resting_effectiveness` is `Some` while the
    /// pawn is asleep (bed or ground effectiveness); `hunger_factor` is the
    /// hediffs' hunger rate factor. Returns `true` when an involuntary
    /// sleep event fires (exhaustion).
    pub fn interval(
        &mut self,
        resting_effectiveness: Option<f32>,
        hunger_factor: f32,
        rng: &mut Rand,
    ) -> bool {
        let rates = self.rates.clone();
        for need in &mut self.list {
            match need.kind {
                NeedKind::Food => {
                    let cat = need.hunger_category();
                    // COMPATIBILITY TODO: currently approximate — trait and
                    // bed hunger factors are not applied.
                    let fall = BASE_FOOD_FALL_PER_TICK
                        * rates.hunger_rate
                        * cat.multiplier()
                        * hunger_factor
                        * INTERVAL;
                    need.level = (need.level - fall).clamp(0.0, need.max);
                }
                NeedKind::Rest => {
                    match resting_effectiveness {
                        Some(eff) => {
                            let gain = eff * rates.rest_rate_multiplier;
                            if gain > 0.0 {
                                need.level += REST_GAIN_PER_INTERVAL * gain;
                            }
                        }
                        None => {
                            // COMPATIBILITY TODO: currently approximate —
                            // the hediff rest-fall factor is assumed 1.0.
                            let cat = RestCategory::of(need.level);
                            need.level -= BASE_REST_FALL_PER_TICK
                                * cat.fall_factor()
                                * INTERVAL
                                * rates.rest_fall_rate_factor;
                        }
                    }
                    need.level = need.level.clamp(0.0, need.max);
                }
                // Joy runs in `joy_interval` (it needs the tick and the
                // colony's expectations).
                NeedKind::Joy => {}
            }
        }

        let Some(rest) = self.rest_level() else {
            return false;
        };
        if rest < 0.0001 {
            self.rest_ticks_at_zero += NEED_INTERVAL_TICKS as u32;
        } else {
            self.rest_ticks_at_zero = 0;
        }
        // Not when already asleep, and not when the pawn cannot fall asleep
        // at all (starving).
        if resting_effectiveness.is_some() || self.is_starving() {
            return false;
        }
        let mtb_days = match self.rest_ticks_at_zero {
            t if t <= 1000 => return false,
            t if t < 15_000 => 0.25,
            t if t < 30_000 => 0.125,
            t if t < 45_000 => 1.0 / 12.0,
            _ => 0.0625,
        };
        // COMPATIBILITY TODO: currently approximate — the formula and draw
        // counts are the game's, but the shared stream's position differs:
        // the game seeds it from the clock and many systems we do not
        // model (sounds, effects, other things) draw from it too.
        rng.mtb_event_occurs(mtb_days, 60_000.0, NEED_INTERVAL_TICKS as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimworld_defs::load_documents;
    use rimworld_defs::xml::ActivePackages;

    const NEED_DEFS: &str = r#"<Defs>
      <NeedDef><defName>Food</defName><needClass>Need_Food</needClass><listPriority>800</listPriority></NeedDef>
      <NeedDef><defName>Rest</defName><needClass>Need_Rest</needClass><listPriority>700</listPriority></NeedDef>
      <NeedDef><defName>Joy</defName><needClass>Need_Joy</needClass><listPriority>500</listPriority></NeedDef>
      <ThingDef><defName>Human</defName><category>Pawn</category>
        <race><intelligence>Humanlike</intelligence></race></ThingDef>
    </Defs>"#;

    fn needs() -> Needs {
        let (db, _) = load_documents("core", &[("n.xml", NEED_DEFS)], &ActivePackages::default());
        let (defs, _) = GameDefs::from_database(db);
        let human = defs.things.get("Human").unwrap().clone();
        Needs::for_race(&defs, &human, true, &mut Rand::new(1))
    }

    fn set(n: &mut Needs, kind: NeedKind, level: f32) {
        n.get_mut(kind).unwrap().level = level;
    }

    /// The external recreation report's fixture A/B values (binary32 bits):
    /// fall per category, gains with tolerance, the 15-tick gain window
    /// and tolerance decay (expectation drop 0.13 per day).
    #[test]
    fn joy_arithmetic_matches_recorded_bits() {
        let mut n = needs();
        let drop = f32::from_bits(0x3E05_1EB8); // 0.13
        for (from, to) in [
            (0.0f32, 0x0000_0000u32),
            (0.009, 0x3BF5_C28E),
            (0.01, 0x3C1A_0275),
            (0.149, 0x3E17_F62C),
            (0.15, 0x3E18_865A),
            (0.299, 0x3E98_8CE7),
            (0.3, 0x3E98_D4FE),
            (0.7, 0x3F32_D0E5),
            (0.85, 0x3F59_374C),
        ] {
            set(&mut n, NeedKind::Joy, from);
            n.joy = JoyState::default();
            n.joy_interval(120_000, drop, 1.0);
            assert_eq!(
                n.get(NeedKind::Joy).unwrap().level.to_bits(),
                to,
                "from {from}"
            );
        }
        set(&mut n, NeedKind::Joy, 0.2);
        n.joy = JoyState::default();
        n.gain_joy(0.1, Some("Meditative"), 120_000);
        assert_eq!(n.get(NeedKind::Joy).unwrap().level.to_bits(), 0x3E99_999A);
        assert_eq!(n.joy.tolerance("Meditative").to_bits(), 0x3D85_1EB8);
        n.gain_joy(0.1, Some("Meditative"), 120_000);
        assert_eq!(n.get(NeedKind::Joy).unwrap().level.to_bits(), 0x3EC9_78D6);
        assert_eq!(n.joy.tolerance("Meditative").to_bits(), 0x3E00_CB29);
        // 14 ticks after the gain: no fall, tolerance decays.
        n.joy_interval(120_014, drop, 1.0);
        assert_eq!(n.get(NeedKind::Joy).unwrap().level.to_bits(), 0x3EC9_78D6);
        assert_eq!(n.joy.tolerance("Meditative").to_bits(), 0x3E00_75F7);
        // 15 ticks after: it falls.
        n.joy_interval(120_015, drop, 1.0);
        assert_eq!(n.get(NeedKind::Joy).unwrap().level.to_bits(), 0x3EC8_B43A);
        assert_eq!(n.joy.tolerance("Meditative").to_bits(), 0x3E00_20C5);
        // Boredom ends only strictly below 0.3.
        for (tol, after, bored) in [(0.3f32, 0x3E99_6F01u32, false), (0.301, 0x3E99_F213, true)] {
            n.joy = JoyState::default();
            n.joy.tolerances.insert("Meditative".into(), tol);
            n.joy.bored.insert("Meditative".into());
            n.joy_interval(130_000, drop, 1.0);
            assert_eq!(n.joy.tolerance("Meditative").to_bits(), after, "tol {tol}");
            assert_eq!(n.joy.bored_of("Meditative"), bored, "tol {tol}");
        }
    }

    #[test]
    fn initial_levels_and_order() {
        let n = needs();
        let kinds: Vec<_> = n.list.iter().map(|n| n.kind).collect();
        assert_eq!(kinds, vec![NeedKind::Food, NeedKind::Rest, NeedKind::Joy]);
        assert_eq!(n.get(NeedKind::Food).unwrap().level, 0.8);
        let rest = n.rest_level().unwrap();
        assert!((0.9..=1.0).contains(&rest), "{rest}");
        let joy = n.get(NeedKind::Joy).unwrap().level;
        assert!((0.5..=0.6).contains(&joy), "{joy}");
    }

    #[test]
    fn food_fall_depends_on_hunger_category() {
        let mut n = needs();
        let mut rng = Rand::new(2);
        set(&mut n, NeedKind::Food, 0.5);
        n.interval(None, 1.0, &mut rng);
        let fed = 0.5 - n.get(NeedKind::Food).unwrap().level;
        assert!((fed - 0.004).abs() < 1e-6, "{fed}"); // 2.667e-5 * 150
        set(&mut n, NeedKind::Food, 0.2); // hungry (< 0.24)
        n.interval(None, 1.0, &mut rng);
        let hungry = 0.2 - n.get(NeedKind::Food).unwrap().level;
        assert!((hungry - 0.002).abs() < 1e-6, "{hungry}");
        set(&mut n, NeedKind::Food, 0.1); // urgently hungry (< 0.12)
        n.interval(None, 1.0, &mut rng);
        let urgent = 0.1 - n.get(NeedKind::Food).unwrap().level;
        assert!((urgent - 0.001).abs() < 1e-6, "{urgent}");
    }

    #[test]
    fn rest_fall_depends_on_rest_category() {
        let mut n = needs();
        let mut rng = Rand::new(3);
        let base = BASE_REST_FALL_PER_TICK * 150.0;
        for (level, factor) in [(0.9, 1.0), (0.2, 0.7), (0.1, 0.3), (0.005, 0.6)] {
            set(&mut n, NeedKind::Rest, level);
            n.interval(None, 1.0, &mut rng);
            let fell = level - n.rest_level().unwrap();
            assert!((fell - base * factor).abs() < 1e-7, "level {level}: {fell}");
        }
    }

    #[test]
    fn resting_gains_by_effectiveness() {
        let mut n = needs();
        let mut rng = Rand::new(4);
        set(&mut n, NeedKind::Rest, 0.5);
        n.interval(Some(0.8), 1.0, &mut rng);
        let gained = n.rest_level().unwrap() - 0.5;
        assert!((gained - REST_GAIN_PER_INTERVAL * 0.8).abs() < 1e-7);
    }

    #[test]
    fn categories() {
        assert_eq!(RestCategory::of(0.005), RestCategory::Exhausted);
        assert_eq!(RestCategory::of(0.1), RestCategory::VeryTired);
        assert_eq!(RestCategory::of(0.2), RestCategory::Tired);
        assert_eq!(RestCategory::of(0.28), RestCategory::Rested);
        assert_eq!(HungerCategory::of(0.0, 0.3), HungerCategory::Starving);
        assert_eq!(
            HungerCategory::of(0.11, 0.3),
            HungerCategory::UrgentlyHungry
        );
        assert_eq!(HungerCategory::of(0.23, 0.3), HungerCategory::Hungry);
        // 0.3 * 0.8 is 0.24000001 in f32 (as in the game), so 0.24 is hungry.
        assert_eq!(HungerCategory::of(0.24, 0.3), HungerCategory::Hungry);
        assert_eq!(HungerCategory::of(0.241, 0.3), HungerCategory::Fed);
    }

    #[test]
    fn involuntary_sleep_needs_time_at_zero() {
        let mut n = needs();
        let mut rng = Rand::new(5);
        set(&mut n, NeedKind::Rest, 0.0);
        // The first 1000 ticks at zero never trigger (6 intervals = 900).
        for _ in 0..6 {
            assert!(!n.interval(None, 1.0, &mut rng));
        }
        // Afterwards it eventually fires (MTB 0.25 days = 15 000 ticks).
        let fired = (0..2000).any(|_| n.interval(None, 1.0, &mut rng));
        assert!(fired);
        // Never while already asleep.
        set(&mut n, NeedKind::Rest, 0.0);
        n.rest_ticks_at_zero = 20_000;
        assert!(!(0..100).any(|_| n.interval(Some(0.0), 1.0, &mut rng)));
    }

    #[test]
    fn mtb_rates_are_plausible() {
        let mut rng = Rand::new(6);
        // chance per check = 150 / 15000 = 1 %
        let hits = (0..100_000)
            .filter(|_| rng.mtb_event_occurs(0.25, 60_000.0, 150.0))
            .count();
        assert!((800..1200).contains(&hits), "{hits}");
        assert!(!rng.mtb_event_occurs(f32::INFINITY, 1.0, 1.0));
        assert!(rng.mtb_event_occurs(0.0, 1.0, 1.0));
    }
}
