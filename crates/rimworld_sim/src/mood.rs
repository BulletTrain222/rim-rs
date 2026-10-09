//! Mood (`Need_Mood`), thoughts (`ThoughtHandler` with its memory and
//! situational handlers) and the mental breaker (`MentalBreaker`),
//! docs/research.md §59.
//!
//! Float steps follow the game's runtime: compound expressions are
//! evaluated wide and rounded to binary32 when stored.

use rimworld_defs::{
    DefId, GameDefs, MentalBreakDef, MentalBreakIntensity, MentalStateDef, ThoughtDef,
};

use crate::rand::{Rand, Reservoir};

/// `Need.SetInitialLevel`: every need starts at half (not `baseLevel`).
pub const INITIAL_MOOD: f32 = 0.5;
/// `PawnRecentMemory`'s "never" tick.
pub const NEVER_TICK: i64 = 999_999;
/// Memory age added per need interval.
const MEMORY_AGE_STEP: i32 = 150;

/// What the thoughts need to know about their pawn.
pub trait ThoughtSource {
    /// `ThoughtUtility.CanGetThought` (without the nullification check).
    fn can_get(&self, def: &ThoughtDef) -> bool;
    /// `ThoughtUtility.ThoughtNullified`.
    fn nullified(&self, def: &ThoughtDef) -> bool;
    /// The situational worker's `CurrentStateInternal`: the requested stage
    /// index, or `None` when inactive.
    fn worker_state(&mut self, id: DefId<ThoughtDef>, def: &ThoughtDef) -> Option<i32>;
}

/// A memory thought (`Thought_Memory`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Memory {
    pub def: DefId<ThoughtDef>,
    /// `forcedStage` (saved as `stageIndex`).
    pub stage: usize,
    pub age: i32,
    #[serde(default = "one")]
    pub mood_power_factor: f32,
    #[serde(default)]
    pub mood_offset: i32,
    #[serde(default)]
    pub permanent: bool,
    #[serde(default = "minus_one")]
    pub duration_override: i32,
}

fn one() -> f32 {
    1.0
}

fn minus_one() -> i32 {
    -1
}

fn yes() -> bool {
    true
}

impl Memory {
    pub fn new(def: DefId<ThoughtDef>, stage: usize) -> Self {
        Self {
            def,
            stage,
            age: 0,
            mood_power_factor: 1.0,
            mood_offset: 0,
            permanent: false,
            duration_override: -1,
        }
    }

    pub fn duration_ticks(&self, def: &ThoughtDef) -> i32 {
        if self.duration_override < 0 {
            def.duration_ticks()
        } else {
            self.duration_override
        }
    }

    /// `ShouldDiscard`: strictly older than its duration.
    fn should_discard(&self, def: &ThoughtDef) -> bool {
        !self.permanent && self.age > self.duration_ticks(def)
    }

    /// `Thought.GroupsWith` for memories: same Def, and the same stage
    /// unless stages stack.
    fn groups_with(&self, def: &ThoughtDef, other: DefId<ThoughtDef>, stage: usize) -> bool {
        self.def == other && (self.stage == stage || def.stages_stack)
    }
}

/// A cached situational thought (`Thought_Situational`): its stage while
/// active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Situational {
    pub def: DefId<ThoughtDef>,
    pub stage: Option<usize>,
}

/// One mood thought: a memory or an active situational thought.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThoughtRef {
    Memory(usize),
    Situational(usize),
}

/// A thought as shown to the player: Def, stage and mood offset.
#[derive(Debug, Clone, PartialEq)]
pub struct ThoughtLine {
    pub def: DefId<ThoughtDef>,
    pub stage: usize,
    pub offset: f32,
    /// Memories: ticks old.
    pub age: Option<i32>,
}

/// `Need_Mood` with its `ThoughtHandler` and `PawnRecentMemory`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MoodState {
    pub level: f32,
    /// Memories in gain order.
    pub memories: Vec<Memory>,
    /// The situational cache (not saved; rebuilt lazily).
    #[serde(skip)]
    pub situational: Vec<Situational>,
    #[serde(skip, default = "yes")]
    pub situational_dirty: bool,
    /// `lastInstantMoodCheckTick` / `lastInstantMood`.
    #[serde(skip)]
    pub instant_cache: Option<(u64, f32)>,
    pub last_light_tick: i64,
    pub last_outdoor_tick: i64,
    /// `PawnObserver.intervalsUntilObserve` (not saved).
    #[serde(skip)]
    pub observe_in: i32,
}

impl Default for MoodState {
    fn default() -> Self {
        Self {
            level: INITIAL_MOOD,
            memories: Vec::new(),
            situational: Vec::new(),
            situational_dirty: true,
            instant_cache: None,
            last_light_tick: NEVER_TICK,
            last_outdoor_tick: NEVER_TICK,
            observe_in: 0,
        }
    }
}

/// `ThoughtWorker.CurrentState` then `ThoughtState.ActiveFor` /
/// `StageIndexFor`: the active stage index, clamped to the last stage and
/// requiring a non-null slot.
pub fn current_state(
    defs: &GameDefs,
    src: &mut dyn ThoughtSource,
    id: DefId<ThoughtDef>,
) -> Option<usize> {
    let def = &defs.thoughts[id];
    let raw = if src.can_get(def) {
        src.worker_state(id, def)
    } else {
        None
    };
    let raw = if def.invert {
        match raw {
            Some(_) => None,
            None => Some(0),
        }
    } else {
        raw
    };
    let index = raw?.min(def.stages.len() as i32 - 1);
    if index < 0 {
        return None;
    }
    def.stage(index as usize).map(|_| index as usize)
}

impl MoodState {
    /// `Thought_Memory.MoodOffset`.
    fn memory_offset(defs: &GameDefs, src: &dyn ThoughtSource, m: &Memory) -> f32 {
        let def = &defs.thoughts[m.def];
        let Some(stage) = def.stage(m.stage) else {
            return 0.0;
        };
        if src.nullified(def) {
            return 0.0;
        }
        // COMPATIBILITY TODO: currently approximate — effectMultiplyingStat
        // is not applied.
        let mut num = stage.base_mood_effect;
        num *= m.mood_power_factor;
        num += m.mood_offset as f32;
        if def.lerp_mood_to_zero {
            let d = m.duration_ticks(def);
            num = (num as f64 * (1.0 - m.age as f64 / d as f64)) as f32;
        }
        num
    }

    /// `Thought.MoodOffset` for a situational thought.
    // COMPATIBILITY TODO: currently approximate — effectMultiplyingStat and
    // the workers' MoodMultiplier overrides are not applied.
    fn situational_offset(defs: &GameDefs, src: &dyn ThoughtSource, s: &Situational) -> f32 {
        let def = &defs.thoughts[s.def];
        let Some(stage) = s.stage.and_then(|i| def.stage(i)) else {
            return 0.0;
        };
        if src.nullified(def) {
            return 0.0;
        }
        stage.base_mood_effect
    }

    pub fn offset_of(&self, defs: &GameDefs, src: &dyn ThoughtSource, t: ThoughtRef) -> f32 {
        match t {
            ThoughtRef::Memory(i) => Self::memory_offset(defs, src, &self.memories[i]),
            ThoughtRef::Situational(i) => Self::situational_offset(defs, src, &self.situational[i]),
        }
    }

    fn key(&self, t: ThoughtRef) -> (bool, DefId<ThoughtDef>, usize) {
        match t {
            ThoughtRef::Memory(i) => (true, self.memories[i].def, self.memories[i].stage),
            ThoughtRef::Situational(i) => {
                let s = &self.situational[i];
                (false, s.def, s.stage.unwrap_or(0))
            }
        }
    }

    /// `Thought.GroupsWith` (memories only group with memories).
    fn groups(&self, defs: &GameDefs, a: ThoughtRef, b: ThoughtRef) -> bool {
        let (ma, da, sa) = self.key(a);
        let (mb, db, sb) = self.key(b);
        ma == mb && da == db && (sa == sb || defs.thoughts[da].stages_stack)
    }

    /// `SituationalThoughtHandler.UpdateAllMoodThoughts`: recalculate the
    /// cached thoughts in cache order, then add newly active ones in Def
    /// order.
    pub fn update_situational(&mut self, defs: &GameDefs, src: &mut dyn ThoughtSource) {
        self.situational_dirty = false;
        for i in 0..self.situational.len() {
            let id = self.situational[i].def;
            let was = self.situational[i].stage.is_some();
            let stage = current_state(defs, src, id);
            self.situational[i].stage = stage;
            // `Notify_BecameActive` / `_BecameInactive`.
            if let Some(produced) = defs.thoughts[id]
                .produces_memory_thought
                .as_deref()
                .and_then(|p| defs.thoughts.id(p))
            {
                if was && stage.is_none() {
                    self.gain_memory(defs, src, produced, 0);
                } else if !was && stage.is_some() {
                    self.memories.retain(|m| m.def != produced);
                }
            }
        }
        for (id, def) in defs.thoughts.iter() {
            if !def.is_situational() || def.is_social() {
                continue;
            }
            if self.situational.iter().any(|s| s.def == id) {
                continue;
            }
            // `TryCreateThought` checks `CanGetThought` before the worker:
            // an inverted thought the pawn can't get is never created.
            if !src.can_get(def) {
                continue;
            }
            if let Some(stage) = current_state(defs, src, id) {
                self.situational.push(Situational {
                    def: id,
                    stage: Some(stage),
                });
            }
        }
    }

    /// `Notify_SituationalThoughtsDirty`: clear the cache.
    pub fn situational_dirty(&mut self) {
        self.situational.clear();
        self.situational_dirty = true;
    }

    /// `ThoughtHandler.GetAllMoodThoughts`: memories with a nonzero offset
    /// in order, then the active situational thoughts (rebuilding the cache
    /// first when dirty).
    pub fn mood_thoughts(
        &mut self,
        defs: &GameDefs,
        src: &mut dyn ThoughtSource,
    ) -> Vec<ThoughtRef> {
        if self.situational_dirty {
            self.update_situational(defs, src);
        }
        let mut out: Vec<ThoughtRef> = (0..self.memories.len())
            .filter(|&i| Self::memory_offset(defs, src, &self.memories[i]) != 0.0)
            .map(ThoughtRef::Memory)
            .collect();
        out.extend(
            (0..self.situational.len())
                .filter(|&i| self.situational[i].stage.is_some())
                .map(ThoughtRef::Situational),
        );
        out
    }

    /// `ThoughtHandler.TotalMoodOffset`: distinct groups (first member
    /// kept), each the mean of its members × (1 + m + m² + …).
    pub fn total_mood_offset(&mut self, defs: &GameDefs, src: &mut dyn ThoughtSource) -> f32 {
        let all = self.mood_thoughts(defs, src);
        let mut distinct = all.clone();
        let mut n = distinct.len();
        while n > 0 {
            n -= 1;
            if (0..n).any(|i| self.groups(defs, distinct[i], distinct[n])) {
                distinct.remove(n);
            }
        }
        let mut total = 0.0f32;
        for &g in &distinct {
            let mut sum = 0.0f32;
            let mut power = 1.0f32;
            let mut powers = 0.0f32;
            let mut count = 0;
            for &t in all.iter().filter(|&&t| self.groups(defs, t, g)) {
                let (_, def, _) = self.key(t);
                sum += self.offset_of(defs, src, t);
                powers += power;
                power *= defs.thoughts[def].stacked_effect_multiplier;
                count += 1;
            }
            let mean = sum / count as f32;
            total += mean * powers;
        }
        total
    }

    /// `Need_Mood.CurInstantLevel`, computed once per tick: the base
    /// level (`baseLevel`) plus the thoughts' points / 100 (plus the
    /// difficulty's colonist offset), clamped to [0, 1].
    pub fn instant_level(
        &mut self,
        tick: u64,
        defs: &GameDefs,
        src: &mut dyn ThoughtSource,
        base: f32,
        colonist_offset: Option<f32>,
    ) -> f32 {
        if let Some((t, v)) = self.instant_cache
            && t == tick
        {
            return v;
        }
        let mut s = self.total_mood_offset(defs, src);
        if let Some(o) = colonist_offset {
            s += o;
        }
        let v = ((base as f64 + s as f64 / 100.0) as f32).clamp(0.0, 1.0);
        self.instant_cache = Some((tick, v));
        v
    }

    /// The instant level as last computed (UI).
    pub fn cached_instant(&self) -> Option<f32> {
        self.instant_cache.map(|(_, v)| v)
    }

    /// `Need_Seeker.NeedInterval`: rise by `rise × 0.06` toward a higher
    /// target, or fall by `fall × 0.06` toward a lower one.
    pub fn seek(&mut self, instant: f32, rise_per_hour: f32, fall_per_hour: f32) {
        if instant > self.level {
            let v = (self.level as f64 + rise_per_hour as f64 * 0.06f32 as f64) as f32;
            self.level = v.clamp(0.0, 1.0).min(instant);
        }
        if instant < self.level {
            let v = (self.level as f64 - fall_per_hour as f64 * 0.06f32 as f64) as f32;
            self.level = v.clamp(0.0, 1.0).max(instant);
        }
    }

    /// `MemoryThoughtHandler.TryGainMemory` for a new memory of `id` at
    /// `stage`: merge into (renew) the oldest of a full group, or append;
    /// enforce the stack limits; make `thoughtToMake`; remove
    /// `replaceThoughts`.
    // COMPATIBILITY TODO: currently approximate — mood bubbles (which draw
    // random numbers for their motes) are not made.
    pub fn gain_memory(
        &mut self,
        defs: &GameDefs,
        src: &mut dyn ThoughtSource,
        id: DefId<ThoughtDef>,
        stage: usize,
    ) {
        let def = &defs.thoughts[id];
        if !src.can_get(def) {
            return;
        }
        let in_group = |ms: &[Memory]| {
            ms.iter()
                .filter(|m| m.groups_with(&defs.thoughts[m.def], id, stage))
                .count() as i32
        };
        let oldest = |ms: &[Memory], pred: &dyn Fn(&Memory) -> bool| {
            let mut best: Option<usize> = None;
            let mut age = -9999;
            for (i, m) in ms.iter().enumerate() {
                if pred(m) && m.age > age {
                    best = Some(i);
                    age = m.age;
                }
            }
            best
        };
        let group_pred = |m: &Memory| m.groups_with(&defs.thoughts[m.def], id, stage);
        let mut merged = false;
        if in_group(&self.memories) >= def.stack_limit
            && let Some(i) = oldest(&self.memories, &group_pred)
        {
            self.memories[i].age = 0;
            merged = true;
        }
        if !merged {
            self.memories.push(Memory::new(id, stage));
        }
        if def.stack_limit_for_same_other_pawn >= 0 {
            while in_group(&self.memories) > def.stack_limit_for_same_other_pawn {
                let Some(i) = oldest(&self.memories, &group_pred) else {
                    break;
                };
                self.memories.remove(i);
            }
        }
        if def.stack_limit >= 0 {
            while self.memories.iter().filter(|m| m.def == id).count() as i32 > def.stack_limit {
                let Some(i) = oldest(&self.memories, &|m| m.def == id) else {
                    break;
                };
                self.memories.remove(i);
            }
        }
        if let Some(next) = def
            .thought_to_make
            .as_deref()
            .and_then(|t| defs.thoughts.id(t))
        {
            self.gain_memory(defs, src, next, 0);
        }
        if !def.replace_thoughts.is_empty() {
            self.memories.retain(|m| {
                !def.replace_thoughts
                    .contains(&defs.thoughts[m.def].def_name)
            });
        }
    }

    /// `MemoryThoughtHandler.MemoryThoughtInterval`: every memory ages by
    /// 150; expired ones are removed (from the end), each followed by its
    /// `nextThought`.
    pub fn memory_interval(&mut self, defs: &GameDefs, src: &mut dyn ThoughtSource) {
        for m in &mut self.memories {
            m.age += MEMORY_AGE_STEP;
        }
        let mut i = self.memories.len();
        while i > 0 {
            i -= 1;
            if i >= self.memories.len() {
                continue;
            }
            let def = &defs.thoughts[self.memories[i].def];
            if self.memories[i].should_discard(def) {
                self.memories.remove(i);
                if let Some(next) = def
                    .next_thought
                    .as_deref()
                    .and_then(|t| defs.thoughts.id(t))
                {
                    self.gain_memory(defs, src, next, 0);
                }
            }
        }
    }

    /// `PawnObserver.ObserverInterval`'s countdown: whether to observe now.
    /// After observing, call [`MoodState::reschedule_observer`].
    pub fn observer_due(&mut self) -> bool {
        self.observe_in -= 1;
        self.observe_in <= 0
    }

    /// Next observation in 4 + RangeInclusive(-1, 1) intervals.
    pub fn reschedule_observer(&mut self, rng: &mut Rand) {
        self.observe_in = 4 + rng.range_inclusive(-1, 1);
    }

    /// The pawn's mood thoughts with their offsets (UI).
    pub fn thought_lines(
        &mut self,
        defs: &GameDefs,
        src: &mut dyn ThoughtSource,
    ) -> Vec<ThoughtLine> {
        self.mood_thoughts(defs, src)
            .into_iter()
            .map(|t| {
                let (_, def, stage) = self.key(t);
                ThoughtLine {
                    def,
                    stage,
                    offset: self.offset_of(defs, src, t),
                    age: match t {
                        ThoughtRef::Memory(i) => Some(self.memories[i].age),
                        ThoughtRef::Situational(_) => None,
                    },
                }
            })
            .collect()
    }

    /// `MentalBreaker.RandomFinalStraw`: among individual thoughts at most
    /// half as bad as the worst, a draw weighted by how bad they are.
    pub fn random_final_straw(
        &mut self,
        defs: &GameDefs,
        src: &mut dyn ThoughtSource,
        rng: &mut Rand,
    ) -> Option<ThoughtRef> {
        let all = self.mood_thoughts(defs, src);
        let offsets: Vec<f32> = all.iter().map(|&t| self.offset_of(defs, src, t)).collect();
        let worst = offsets
            .iter()
            .fold(0.0f32, |m, &o| if o < m { o } else { m });
        let max = worst * 0.5;
        let mut pick = Reservoir::new();
        for (&t, &o) in all.iter().zip(&offsets) {
            if o <= max {
                pick.offer(t, -o, rng);
            }
        }
        pick.into_choice()
    }
}

/// `MentalBreaker`'s saved state.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MentalBreaker {
    /// `ticksUntilCanDoMentalBreak`.
    pub cooldown: i32,
    pub below_extreme: i32,
    pub below_major: i32,
    pub below_minor: i32,
}

/// Break thresholds (extreme, major, minor) from `MentalBreakThreshold`.
pub fn break_thresholds(threshold: f32) -> (f32, f32, f32) {
    (threshold * (1.0 / 7.0), threshold * 0.571_428_6, threshold)
}

/// Ticks below a threshold before a break of that intensity is wanted.
const MIN_TICKS_BELOW_TO_BREAK: i32 = 2000;
/// `MinTicksSinceRecoveryToBreak`.
pub const BREAK_COOLDOWN_TICKS: i32 = 15_000;

impl MentalBreaker {
    /// The per-check counter update: each threshold's counter grows by
    /// 150 while mood is strictly below it, else resets.
    pub fn update_counters(&mut self, mood: f32, thresholds: (f32, f32, f32)) {
        let step = |below: bool, c: &mut i32| {
            if below {
                *c += 150;
            } else {
                *c = 0;
            }
        };
        step(mood < thresholds.0, &mut self.below_extreme);
        step(mood < thresholds.1, &mut self.below_major);
        step(mood < thresholds.2, &mut self.below_minor);
    }

    /// `CurrentDesiredMoodBreakIntensity`.
    pub fn desired_intensity(&self) -> MentalBreakIntensity {
        if self.below_extreme >= MIN_TICKS_BELOW_TO_BREAK {
            MentalBreakIntensity::Extreme
        } else if self.below_major >= MIN_TICKS_BELOW_TO_BREAK {
            MentalBreakIntensity::Major
        } else if self.below_minor >= MIN_TICKS_BELOW_TO_BREAK {
            MentalBreakIntensity::Minor
        } else {
            MentalBreakIntensity::None
        }
    }

    /// `TestMoodMentalBreak`: no draw during the cooldown or before 2,000
    /// ticks below; else one MTB check (0.5, 0.8 or 4 days).
    pub fn test_mood_break(&self, rng: &mut Rand) -> bool {
        if self.cooldown > 0 {
            return false;
        }
        let mtb = if self.below_extreme > MIN_TICKS_BELOW_TO_BREAK {
            0.5
        } else if self.below_major > MIN_TICKS_BELOW_TO_BREAK {
            0.8
        } else if self.below_minor > MIN_TICKS_BELOW_TO_BREAK {
            4.0
        } else {
            return false;
        };
        rng.mtb_event_occurs(mtb, 60_000.0, 150.0)
    }
}

/// `GetBreaksForIntensity` fed lazily into `TryRandomElementByWeight`: the
/// breaks of the intensity (in Def order) that can occur, else the next
/// lower intensity's; weighted by commonality.
pub fn select_break(
    defs: &GameDefs,
    intensity: MentalBreakIntensity,
    can_occur: &mut dyn FnMut(&MentalBreakDef, &mut Rand) -> bool,
    commonality: &dyn Fn(&MentalBreakDef) -> f32,
    rng: &mut Rand,
) -> Option<DefId<MentalBreakDef>> {
    let mut pick = Reservoir::new();
    let mut level = intensity;
    while level != MentalBreakIntensity::None {
        let mut any = false;
        for (id, d) in defs.mental_breaks.iter() {
            if d.intensity == level && !d.anomalous_break && can_occur(d, rng) {
                any = true;
                pick.offer(id, commonality(d), rng);
            }
        }
        if any {
            break;
        }
        level = match level {
            MentalBreakIntensity::Extreme => MentalBreakIntensity::Major,
            MentalBreakIntensity::Major => MentalBreakIntensity::Minor,
            _ => MentalBreakIntensity::None,
        };
    }
    pick.into_choice()
}

/// The mental parts of `Pawn_MindState`: the breaker and the active
/// mental state.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MindState {
    pub breaker: MentalBreaker,
    pub mental_state: Option<MentalState>,
    /// `thinkData`: per think node (by save key) its last try tick
    /// (`ThinkNode_ChancePerHour`).
    #[serde(default)]
    pub think_data: std::collections::BTreeMap<i32, i64>,
}

/// An active mental state (`MentalState`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MentalState {
    pub def: DefId<MentalStateDef>,
    pub age: i32,
    pub caused_by_mood: bool,
    #[serde(default = "minus_one")]
    pub force_recover_after: i32,
}

impl MentalState {
    /// `MentalState.MentalStateTick` on its 30-tick hash interval: age by
    /// 30, then whether it ends — at the maximum age, past the minimum by
    /// an MTB roll, at a forced deadline, or (when allowed) asleep.
    pub fn tick_should_recover(
        &mut self,
        def: &MentalStateDef,
        awake: bool,
        rng: &mut Rand,
    ) -> bool {
        self.age += 30;
        if self.age >= def.max_ticks_before_recovery
            || (self.age >= def.min_ticks_before_recovery
                && rng.mtb_event_occurs(def.recovery_mtb_days, 60_000.0, 30.0))
            || (self.force_recover_after != -1 && self.age >= self.force_recover_after)
        {
            return true;
        }
        def.recover_from_sleep && !awake
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimworld_defs::load_documents;
    use rimworld_defs::xml::ActivePackages;

    const DEFS: &str = r#"<Defs>
      <ThoughtDef><defName>AteWithoutTable</defName><durationDays>1</durationDays><stackLimit>1</stackLimit>
        <nullifyingTraits><li>Ascetic</li></nullifyingTraits>
        <stages><li><label>ate without table</label><baseMoodEffect>-3</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>SleepDisturbed</defName><durationDays>1</durationDays><stackLimit>3</stackLimit>
        <stackedEffectMultiplier>1</stackedEffectMultiplier>
        <stages><li><label>sleep disturbed</label><baseMoodEffect>-1</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>NewColonyOptimism</defName><durationDays>8</durationDays><nextThought>NewColonyHope</nextThought>
        <stages><li><label>optimism</label><baseMoodEffect>10</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>NewColonyHope</defName><durationDays>7</durationDays>
        <stages><li><label>hope</label><baseMoodEffect>5</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>Pain</defName><workerClass>ThoughtWorker_Pain</workerClass>
        <stages><li><baseMoodEffect>-5</baseMoodEffect></li><li><baseMoodEffect>-10</baseMoodEffect></li>
        <li><baseMoodEffect>-15</baseMoodEffect></li><li><baseMoodEffect>-20</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>ClothedNudist</defName><workerClass>ThoughtWorker_NudistNude</workerClass>
        <invert>true</invert><requiredTraits><li>Nudist</li></requiredTraits>
        <stages><li><baseMoodEffect>-3</baseMoodEffect></li></stages></ThoughtDef>
      <ThoughtDef><defName>Staged</defName><durationDays>1</durationDays><stackLimit>3</stackLimit>
        <stagesStack>true</stagesStack>
        <stages><li><baseMoodEffect>-2</baseMoodEffect></li><li IsNull="True" /><li><baseMoodEffect>-6</baseMoodEffect></li></stages></ThoughtDef>
      <MentalStateDef Abstract="True" Name="BaseMentalState">
        <minTicksBeforeRecovery>10000</minTicksBeforeRecovery><recoveryMtbDays>0.3</recoveryMtbDays>
      </MentalStateDef>
      <MentalStateDef ParentName="BaseMentalState"><defName>Wander_Sad</defName>
        <minTicksBeforeRecovery>40000</minTicksBeforeRecovery><maxTicksBeforeRecovery>60000</maxTicksBeforeRecovery>
        <recoveryMtbDays>0.166</recoveryMtbDays><recoverFromSleep>true</recoverFromSleep>
        <moodRecoveryThought>Catharsis</moodRecoveryThought></MentalStateDef>
    </Defs>"#;

    fn defs() -> GameDefs {
        let (db, _) = load_documents("core", &[("t.xml", DEFS)], &ActivePackages::default());
        GameDefs::from_database(db).0
    }

    /// No situational thoughts except an optional pain stage; optional
    /// nullification of everything with nullifying traits.
    #[derive(Default)]
    struct Src {
        pain_stage: Option<i32>,
        nullify: bool,
    }

    impl ThoughtSource for Src {
        fn can_get(&self, def: &ThoughtDef) -> bool {
            def.required_traits.is_empty()
        }
        fn nullified(&self, def: &ThoughtDef) -> bool {
            self.nullify && !def.nullifying_traits.is_empty()
        }
        fn worker_state(&mut self, _: DefId<ThoughtDef>, def: &ThoughtDef) -> Option<i32> {
            if def.def_name == "Pain" {
                self.pain_stage
            } else {
                None
            }
        }
    }

    const BASE: f32 = 0.32;
    const RISE: f32 = 0.12;
    const FALL: f32 = 0.08;

    fn bits(v: f32) -> u32 {
        v.to_bits()
    }

    /// Fixture A: from 0.5 toward 0.32, and up from 0.1.
    #[test]
    fn seeking_matches_recorded_bits() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        let target = m.instant_level(10_000, &defs, &mut src, BASE, None);
        assert_eq!(bits(target), 0x3EA3_D70A);
        let mut seen = vec![];
        for _ in 0..5 {
            m.seek(target, RISE, FALL);
            seen.push(bits(m.level));
        }
        assert_eq!(
            seen,
            [
                0x3EFD_8ADB,
                0x3EFB_15B6,
                0x3EF8_A091,
                0x3EF6_2B6C,
                0x3EF3_B647
            ]
        );
        m.level = 0.1;
        let mut seen = vec![];
        for _ in 0..3 {
            m.seek(target, RISE, FALL);
            seen.push(bits(m.level));
        }
        assert_eq!(seen, [0x3DDB_8BAD, 0x3DEA_4A8D, 0x3DF9_096D]);
    }

    /// Fixture B: one AteWithoutTable memory from 0.32; ages 150 per
    /// interval; mood reaches the target at the 7th interval.
    #[test]
    fn one_memory_matches_recorded_bits() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState {
            level: 0.32,
            ..Default::default()
        };
        let id = defs.thoughts.id("AteWithoutTable").unwrap();
        m.gain_memory(&defs, &mut src, id, 0);
        let expected = [
            0x3EA1_61E5,
            0x3E9E_ECC0,
            0x3E9C_779B,
            0x3E9A_0276,
            0x3E97_8D51,
            0x3E95_182C,
            0x3E94_7AE1,
        ];
        for (k, &e) in expected.iter().enumerate() {
            let tick = 20_150 + 150 * k as u64;
            let target = m.instant_level(tick, &defs, &mut src, BASE, None);
            assert_eq!(bits(target), 0x3E94_7AE1);
            m.seek(target, RISE, FALL);
            m.memory_interval(&defs, &mut src);
            assert_eq!(bits(m.level), e, "interval {}", k + 1);
            assert_eq!(m.memories[0].age, 150 * (k as i32 + 1));
        }
    }

    /// Fixture C: a repeat renews the memory; five SleepDisturbed leave
    /// three; the instant level stays cached within the tick.
    #[test]
    fn duplicates_match_recorded_results() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        let table = defs.thoughts.id("AteWithoutTable").unwrap();
        m.gain_memory(&defs, &mut src, table, 0);
        m.memories[0].age = 1234;
        let cached = m.instant_level(30_000, &defs, &mut src, BASE, None);
        m.gain_memory(&defs, &mut src, table, 0);
        assert_eq!((m.memories.len(), m.memories[0].age), (1, 0));
        let sleep = defs.thoughts.id("SleepDisturbed").unwrap();
        for _ in 0..5 {
            m.gain_memory(&defs, &mut src, sleep, 0);
        }
        assert_eq!(m.memories.iter().filter(|x| x.def == sleep).count(), 3);
        assert_eq!(m.total_mood_offset(&defs, &mut src), -6.0);
        assert_eq!(
            bits(m.instant_level(30_000, &defs, &mut src, BASE, None)),
            bits(cached)
        );
        assert_eq!(bits(cached), 0x3E94_7AE1);
        assert_eq!(
            bits(m.instant_level(30_001, &defs, &mut src, BASE, None)),
            0x3E85_1EB8
        );
    }

    /// Fixture D: a one-day memory survives at age 60,000 and is gone at
    /// 60,150; fixture N: an expiring memory's next thought starts at 0.
    #[test]
    fn expiry_matches_recorded_results() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        let table = defs.thoughts.id("AteWithoutTable").unwrap();
        m.gain_memory(&defs, &mut src, table, 0);
        m.memories[0].age = 59_850;
        m.memory_interval(&defs, &mut src);
        assert_eq!(m.memories[0].age, 60_000);
        m.memory_interval(&defs, &mut src);
        assert!(m.memories.is_empty());
        let opt = defs.thoughts.id("NewColonyOptimism").unwrap();
        m.gain_memory(&defs, &mut src, opt, 0);
        m.memories[0].age = defs.thoughts[opt].duration_ticks();
        m.memory_interval(&defs, &mut src);
        assert_eq!(m.memories.len(), 1);
        assert_eq!(defs.thoughts[m.memories[0].def].def_name, "NewColonyHope");
        assert_eq!(m.memories[0].age, 0);
        assert_eq!(
            bits(m.instant_level(141_000, &defs, &mut src, BASE, None)),
            0x3EBD_70A4
        );
    }

    /// Fixture N: nullification zeroes the memory but keeps it.
    #[test]
    fn nullified_memories_stay() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        let table = defs.thoughts.id("AteWithoutTable").unwrap();
        m.gain_memory(&defs, &mut src, table, 0);
        src.nullify = true;
        assert_eq!(
            bits(m.instant_level(1, &defs, &mut src, BASE, None)),
            0x3EA3_D70A
        );
        src.nullify = false;
        assert_eq!(
            bits(m.instant_level(2, &defs, &mut src, BASE, None)),
            0x3E94_7AE1
        );
        assert_eq!(m.memories.len(), 1);
    }

    /// An inverted trait thought (inactive worker → active) is not created
    /// for a pawn without the trait.
    #[test]
    fn inverted_thoughts_need_their_requirements() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        m.update_situational(&defs, &mut src);
        assert!(m.situational.is_empty());
        assert_eq!(m.total_mood_offset(&defs, &mut src), 0.0);
    }

    /// Fixture H: pain stages' targets.
    #[test]
    fn pain_targets_match_recorded_bits() {
        let defs = defs();
        for (stage, expected) in [
            (None, 0x3EA3_D70A),
            (Some(0), 0x3E8A_3D70),
            (Some(1), 0x3E61_47AE),
            (Some(2), 0x3E2E_147A),
            (Some(3), 0x3DF5_C28E),
        ] {
            let mut src = Src {
                pain_stage: stage,
                ..Default::default()
            };
            let mut m = MoodState::default();
            assert_eq!(
                bits(m.instant_level(1, &defs, &mut src, BASE, None)),
                expected
            );
        }
    }

    /// Group arithmetic: the mean of a group's members times the
    /// multiplier series, with stages grouped when they stack; a too-high
    /// requested stage clamps to the last one, a negative one is inactive.
    #[test]
    fn groups_average_then_scale() {
        let defs = defs();
        let mut src = Src::default();
        let mut m = MoodState::default();
        let staged = defs.thoughts.id("Staged").unwrap();
        m.gain_memory(&defs, &mut src, staged, 0);
        m.gain_memory(&defs, &mut src, staged, 2);
        // mean(-2, -6) × (1 + 0.75)
        assert_eq!(m.total_mood_offset(&defs, &mut src), -4.0 * 1.75);
        let pain = defs.thoughts.id("Pain").unwrap();
        let mut src = Src {
            pain_stage: Some(9),
            ..Default::default()
        };
        assert_eq!(current_state(&defs, &mut src, pain), Some(3));
        let mut src = Src {
            pain_stage: Some(-1),
            ..Default::default()
        };
        assert_eq!(current_state(&defs, &mut src, pain), None);
    }

    /// Fixtures I and J: strict thresholds, counter cadence and the MTB
    /// gate's draws (seed 7: none at 1,950 and 2,000, one at 2,001 and
    /// 2,100); fixture Q: seed 1,785 is the first whose value passes the
    /// minor gate, and it breaks.
    #[test]
    fn breaker_matches_recorded_results() {
        let t = break_thresholds(0.35);
        assert_eq!(
            (bits(t.0), bits(t.1), bits(t.2)),
            (0x3D4C_CCCD, 0x3E4C_CCCD, 0x3EB3_3333)
        );
        let mut b = MentalBreaker::default();
        b.update_counters(0.35, t);
        assert_eq!(b.below_minor, 0);
        b.update_counters(f32::from_bits(0x3EB3_3311), t);
        assert_eq!((b.below_minor, b.below_major), (150, 0));
        b.update_counters(0.2, t);
        assert_eq!((b.below_minor, b.below_major), (300, 0));
        b.update_counters(0.05, t);
        assert_eq!((b.below_major, b.below_extreme), (150, 0));
        let mut b = MentalBreaker {
            cooldown: 10_000,
            ..Default::default()
        };
        for k in 1..=15 {
            b.update_counters(0.0, t);
            assert_eq!(b.below_minor, 150 * k);
            let mut rng = Rand::new(33771);
            assert!(!b.test_mood_break(&mut rng));
            assert_eq!(rng.state().1, 0);
        }
        assert_eq!(b.desired_intensity(), MentalBreakIntensity::Extreme);
        for (count, draws) in [(1950, 0), (2000, 0), (2001, 1), (2100, 1)] {
            let b = MentalBreaker {
                below_minor: count,
                ..Default::default()
            };
            let mut rng = Rand::new(7);
            assert!(!b.test_mood_break(&mut rng), "count {count}");
            assert_eq!(rng.state().1, draws, "count {count}");
        }
        let first = (0..100_000).find(|&s| Rand::value_seeded(s) < 0.000625);
        assert_eq!(first, Some(1785));
        let b = MentalBreaker {
            below_minor: 2100,
            ..Default::default()
        };
        let mut rng = Rand::new(1785);
        assert!(b.test_mood_break(&mut rng));
        assert_eq!(rng.state().1, 1);
    }

    /// Fixture O: sad-wander recovery checks with seed 7.
    #[test]
    fn recovery_checks_match_recorded_draws() {
        let defs = defs();
        let id = defs.mental_states.id("Wander_Sad").unwrap();
        let def = &defs.mental_states[id];
        for (age, recovers, draws) in [
            (0, false, 0),
            (39_960, false, 0),
            (39_990, false, 1),
            (59_970, true, 0),
        ] {
            let mut s = MentalState {
                def: id,
                age,
                caused_by_mood: true,
                force_recover_after: -1,
            };
            let mut rng = Rand::new(7);
            assert_eq!(
                s.tick_should_recover(def, true, &mut rng),
                recovers,
                "age {age}"
            );
            assert_eq!(rng.state().1, draws, "age {age}");
        }
        let mut s = MentalState {
            def: id,
            age: 0,
            caused_by_mood: true,
            force_recover_after: -1,
        };
        assert!(s.tick_should_recover(def, false, &mut Rand::new(7)));
    }
}
