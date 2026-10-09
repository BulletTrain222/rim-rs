//! Mood in the simulation: the situational thought workers, the mood need
//! interval, memories from events, the mind state's mental breaks and the
//! sad-wander mental state (docs/research.md §59).

use rimworld_defs::{DefId, ExpectationDef, MentalBreakDef, MentalBreakIntensity, ThoughtDef};

use super::Sim;
use crate::mood::{
    BREAK_COOLDOWN_TICKS, MentalState, MoodState, ThoughtLine, ThoughtSource, break_thresholds,
    select_break,
};
use crate::needs::{HungerCategory, JoyCategory, NeedKind, RestCategory};
use crate::pawn::PawnId;

/// `ThoughtWorker_Dark`: dark for more than this many ticks.
const DARK_TICKS: i64 = 240;

/// Mental states whose behaviour we implement; breaks into other states
/// are left out of the break pool.
// COMPATIBILITY TODO: currently approximate — only sad wandering is
// implemented, so the break pool is limited to it (a major or extreme
// break falls back to a minor one, as the game does when no break of an
// intensity can occur).
const SUPPORTED_STATES: &[&str] = &["Wander_Sad"];

/// What a pawn's thoughts read about it.
struct PawnThoughts<'a> {
    sim: &'a Sim,
    i: usize,
    last_light_tick: i64,
    expectation: Option<ExpectationDef>,
}

impl PawnThoughts<'_> {
    fn has_hediff(&self, name: &str) -> bool {
        let defs = &self.sim.defs;
        defs.hediffs.id(name).is_some_and(|d| {
            self.sim.pawns[self.i]
                .health
                .hediffs
                .iter()
                .any(|h| h.def == d)
        })
    }
}

impl ThoughtSource for PawnThoughts<'_> {
    // COMPATIBILITY TODO: currently approximate — pawns have no traits,
    // genes, gender or quest status: trait requirements fail and gendered
    // thoughts are never gained.
    fn can_get(&self, def: &ThoughtDef) -> bool {
        if !def.for_adults || def.gender.is_some() {
            return false;
        }
        if let Some(min) = def.min_expectation.as_deref() {
            let order = |name: &str| {
                self.sim
                    .defs
                    .expectations
                    .iter()
                    .find(|e| e.def_name == name)
                    .map(|e| e.order)
            };
            if let (Some(cur), Some(min)) = (self.expectation.as_ref(), order(min))
                && cur.order < min
            {
                return false;
            }
        }
        let mut required = false;
        let mut satisfied = false;
        if !def.required_hediffs.is_empty() {
            required = true;
            satisfied = def.required_hediffs.iter().any(|h| self.has_hediff(h));
        }
        if !def.required_traits.is_empty() && !satisfied {
            required = true;
        }
        if required && !satisfied {
            return false;
        }
        !(def.nullified_if_not_colonist && !self.sim.pawns[self.i].is_colonist)
    }

    fn nullified(&self, def: &ThoughtDef) -> bool {
        def.nullifying_hediffs.iter().any(|h| self.has_hediff(h))
    }

    // COMPATIBILITY TODO: currently approximate — only the needs (food,
    // rest, recreation), pain, sickness, darkness, cold, heat and
    // expectations workers are implemented; every other worker (apparel,
    // rooms, comfort/beauty/space/outdoors needs, game conditions, rot
    // stink, hediff-specific and trait thoughts) is inactive.
    fn worker_state(&mut self, _: DefId<ThoughtDef>, def: &ThoughtDef) -> Option<i32> {
        let sim = self.sim;
        let p = &sim.pawns[self.i];
        match def.worker_class.as_deref()? {
            "ThoughtWorker_NeedFood" => match p.needs.get(NeedKind::Food)?.hunger_category() {
                HungerCategory::Fed => None,
                HungerCategory::Hungry => Some(0),
                HungerCategory::UrgentlyHungry => Some(1),
                HungerCategory::Starving => {
                    let stage = sim.defs.hediffs.id("Malnutrition").and_then(|d| {
                        let h = p.health.hediffs.iter().find(|h| h.def == d)?;
                        let stages = &sim.defs.hediffs[d].stages;
                        stages.iter().rposition(|s| h.severity >= s.min_severity)
                    });
                    Some(2 + stage.unwrap_or(0) as i32)
                }
            },
            "ThoughtWorker_NeedRest" => {
                match RestCategory::of(p.needs.get(NeedKind::Rest)?.level) {
                    RestCategory::Rested => None,
                    RestCategory::Tired => Some(0),
                    RestCategory::VeryTired => Some(1),
                    RestCategory::Exhausted => Some(2),
                }
            }
            "ThoughtWorker_NeedJoy" => match JoyCategory::of(p.needs.get(NeedKind::Joy)?.level) {
                JoyCategory::Empty => Some(0),
                JoyCategory::VeryLow => Some(1),
                JoyCategory::Low => Some(2),
                JoyCategory::Satisfied => None,
                JoyCategory::High => Some(3),
                JoyCategory::Extreme => Some(4),
            },
            "ThoughtWorker_Pain" => {
                if self.nullified(def) {
                    return None;
                }
                let pain = sim.health_view(p.id)?.pain_total();
                if pain < 0.0001 {
                    None
                } else if pain < 0.15 {
                    Some(0)
                } else if pain < 0.4 {
                    Some(1)
                } else if pain < 0.8 {
                    Some(2)
                } else {
                    Some(3)
                }
            }
            // COMPATIBILITY TODO: currently approximate — every hediff
            // counts as visible.
            "ThoughtWorker_Sick" => p
                .health
                .hediffs
                .iter()
                .any(|h| sim.defs.hediffs[h.def].makes_sick_thought)
                .then_some(0),
            "ThoughtWorker_Dark" => {
                (!p.asleep && sim.tick as i64 - self.last_light_tick > DARK_TICKS).then_some(0)
            }
            "ThoughtWorker_Cold" => {
                let over = sim.pawn_stat_of(self.i, "ComfyTemperatureMin")
                    - sim.cell_temperature(p.position);
                temperature_stage(over)
            }
            "ThoughtWorker_Hot" => {
                let over = sim.cell_temperature(p.position)
                    - sim.pawn_stat_of(self.i, "ComfyTemperatureMax");
                temperature_stage(over)
            }
            "ThoughtWorker_Expectations" => {
                let e = if p.is_colonist {
                    self.expectation.as_ref()?.thought_stage
                } else {
                    sim.defs
                        .expectations
                        .iter()
                        .find(|e| e.def_name == "ExtremelyLow")?
                        .thought_stage
                };
                e.map(|s| s as i32)
            }
            _ => None,
        }
    }
}

/// `ThoughtWorker_Cold` / `_Hot`: degrees past the comfortable range.
fn temperature_stage(over: f32) -> Option<i32> {
    if over <= 0.0 {
        None
    } else if over < 10.0 {
        Some(0)
    } else if over < 20.0 {
        Some(1)
    } else if over < 30.0 {
        Some(2)
    } else {
        Some(3)
    }
}

/// The Mood NeedDef's seeker values.
struct MoodParams {
    base: f32,
    rise: f32,
    fall: f32,
    freeze_while_sleeping: bool,
}

impl Sim {
    fn mood_params(&self) -> MoodParams {
        let def = self
            .defs
            .needs
            .iter()
            .map(|(_, d)| d)
            .find(|d| d.need_class.as_deref() == Some("Need_Mood"));
        MoodParams {
            base: def.map_or(0.32, |d| d.base_level),
            rise: def.map_or(0.12, |d| d.seeker_rise_per_hour),
            fall: def.map_or(0.08, |d| d.seeker_fall_per_hour),
            freeze_while_sleeping: def.is_none_or(|d| d.freeze_while_sleeping),
        }
    }

    /// Whether a race has the mood need (`minIntelligence` Humanlike).
    pub(super) fn race_has_mood(&self, race: DefId<rimworld_defs::ThingDef>) -> bool {
        self.defs
            .needs
            .iter()
            .any(|(_, d)| d.need_class.as_deref() == Some("Need_Mood"))
            && self.defs.things[race]
                .race
                .as_ref()
                .and_then(|r| r.intelligence.as_deref())
                == Some("Humanlike")
    }

    /// `PawnRecentMemory.Notify_Spawned`.
    pub(super) fn mood_spawned(&mut self, i: usize) {
        let t = self.tick as i64;
        let outdoors = self.pawn_psychologically_outdoors(i);
        if let Some(m) = self.pawns[i].mood.as_mut() {
            m.last_light_tick = t;
            if outdoors {
                m.last_outdoor_tick = t;
            }
        }
    }

    fn pawn_psychologically_outdoors(&self, i: usize) -> bool {
        self.room_at(self.pawns[i].position)
            .is_some_and(|r| self.room_psychologically_outdoors(r))
    }

    /// Runs `f` with the pawn's mood taken out and a view of the pawn for
    /// its thoughts.
    fn with_mood<R>(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut MoodState, &mut PawnThoughts<'_>) -> R,
    ) -> Option<R> {
        let expectation = self.expectation();
        let mut mood = self.pawns[i].mood.take()?;
        let mut src = PawnThoughts {
            sim: self,
            i,
            last_light_tick: mood.last_light_tick,
            expectation,
        };
        let r = f(&mut mood, &mut src);
        self.pawns[i].mood = Some(mood);
        Some(r)
    }

    fn colonist_offset(&self, i: usize) -> Option<f32> {
        self.pawns[i]
            .is_colonist
            .then_some(self.colonist_mood_offset)
    }

    /// `Need_Mood.NeedInterval`: seek the instant level (not while asleep),
    /// note light and outdoors, update situational thoughts, age memories,
    /// then observe the surroundings every few intervals.
    pub(super) fn mood_need_interval(&mut self, i: usize) {
        if self.pawns[i].mood.is_none() {
            return;
        }
        let params = self.mood_params();
        let frozen = params.freeze_while_sleeping && self.pawns[i].asleep;
        let tick = self.tick;
        let offset = self.colonist_offset(i);
        let dark = self.map.ground_glow(self.pawns[i].position) <= 0.3;
        let outdoors = self.pawn_psychologically_outdoors(i);
        let defs = self.defs.clone();
        self.with_mood(i, |mood, src| {
            if !frozen {
                let target = mood.instant_level(tick, &defs, src, params.base, offset);
                mood.seek(target, params.rise, params.fall);
            }
            if !dark {
                mood.last_light_tick = tick as i64;
            }
            if outdoors {
                mood.last_outdoor_tick = tick as i64;
            }
            src.last_light_tick = mood.last_light_tick;
            mood.update_situational(&defs, src);
            mood.memory_interval(&defs, src);
        });
        let due = self.pawns[i]
            .mood
            .as_mut()
            .is_some_and(|m| m.observer_due());
        if due {
            // COMPATIBILITY TODO: currently approximate — observing nearby
            // corpses, pawns and buildings (and their thoughts and history
            // events) is not implemented; only the reschedule draw is.
            let mut mood = self.pawns[i].mood.take();
            if let Some(m) = mood.as_mut() {
                m.reschedule_observer(&mut self.rng);
            }
            self.pawns[i].mood = mood;
        }
    }

    /// Gains a memory (`TryGainMemory`) of the named ThoughtDef.
    pub(super) fn gain_memory(&mut self, i: usize, thought: &str, stage: usize) {
        let defs = self.defs.clone();
        let Some(id) = defs.thoughts.id(thought) else {
            return;
        };
        self.with_mood(i, |mood, src| mood.gain_memory(&defs, src, id, stage));
    }

    /// `Pawn_MindState.MindStateTickInterval`'s mental parts: the active
    /// state's tick, then the breaker.
    // COMPATIBILITY TODO: currently approximate — mental fits, inspirations
    // (which draw random numbers), terrain and weather thoughts and the
    // other mind-state bookkeeping are not modelled.
    pub(super) fn mind_state_interval(&mut self, i: usize, delta: i32) {
        self.mental_state_interval(i, delta);
        self.mental_breaker_interval(i, delta);
    }

    /// `MentalStateHandlerTickInterval` / `MentalState.MentalStateTick`.
    fn mental_state_interval(&mut self, i: usize, delta: i32) {
        let Some(mut state) = self.pawns[i].mind.mental_state.clone() else {
            return;
        };
        let def = self.defs.mental_states[state.def].clone();
        if self.pawns[i].health.downed && def.recover_from_downed {
            self.recover_mental_state(i);
            return;
        }
        let p = &self.pawns[i];
        if !crate::hash::is_hash_interval_tick_delta(self.tick, p.id_number, 30, delta) {
            return;
        }
        let awake = !p.asleep;
        let recover = state.tick_should_recover(&def, awake, &mut self.rng);
        self.pawns[i].mind.mental_state = Some(state);
        if recover {
            self.recover_mental_state(i);
        }
    }

    /// `MentalBreaker.MentalBreakerTickInterval`.
    fn mental_breaker_interval(&mut self, i: usize, delta: i32) {
        let p = &mut self.pawns[i];
        if p.mind.breaker.cooldown > 0 && !p.asleep {
            p.mind.breaker.cooldown -= delta;
        }
        // COMPATIBILITY TODO: currently approximate — life stages are not
        // modelled: every humanlike pawn with mood can break.
        let Some(mood) = p.mood.as_ref() else {
            return;
        };
        if p.mind.mental_state.is_some()
            || !crate::hash::is_hash_interval_tick_delta(self.tick, p.id_number, 150, delta)
        {
            return;
        }
        let level = mood.level;
        let thresholds = break_thresholds(self.pawn_stat_of(i, "MentalBreakThreshold"));
        let breaker = &mut self.pawns[i].mind.breaker;
        breaker.update_counters(level, thresholds);
        let breaker = breaker.clone();
        if breaker.test_mood_break(&mut self.rng) {
            self.try_random_mood_break(i);
        }
        // COMPATIBILITY TODO: currently approximate — trait mental state
        // givers are not modelled (no traits).
    }

    /// `CanHaveMentalBreak`.
    fn can_have_mental_break(&self, i: usize) -> bool {
        let p = &self.pawns[i];
        if p.health.downed || p.asleep || p.mind.mental_state.is_some() || p.mood.is_none() {
            return false;
        }
        p.is_colonist || p.mind.breaker.desired_intensity() == MentalBreakIntensity::Extreme
    }

    /// `MentalBreakWorker.BreakCanOccur` and the state's `StateCanOccur`.
    fn break_can_occur(&self, i: usize, d: &MentalBreakDef) -> bool {
        if d.required_trait.is_some() {
            return false;
        }
        let Some(state) = d.mental_state.as_deref() else {
            return false;
        };
        if !SUPPORTED_STATES.contains(&state) {
            return false;
        }
        self.state_can_occur(i, state)
    }

    /// `MentalStateWorker.StateCanOccur` (colonists only, downed).
    fn state_can_occur(&self, i: usize, state: &str) -> bool {
        let Some(def) = self.defs.mental_states.get(state) else {
            return false;
        };
        let p = &self.pawns[i];
        !(def.colonists_only && !p.is_colonist) && (def.downed_can_do || !p.health.downed)
    }

    /// `MentalBreakWorker.CommonalityFor`: the base commonality, scaled by
    /// the free-colonist population curve for player pawns.
    fn break_commonality(&self, i: usize, d: &MentalBreakDef) -> f32 {
        let mut c = d.base_commonality;
        if self.pawns[i].is_colonist && !d.commonality_factor_per_population.is_empty() {
            let free = self
                .pawns
                .iter()
                .filter(|p| p.is_colonist && !p.health.dead)
                .count() as f32;
            c *= crate::food::evaluate_curve(&d.commonality_factor_per_population, free);
        }
        c
    }

    /// `TryDoRandomMoodCausedMentalBreak`: pick a break of the wanted
    /// intensity, draw the final straw, then start the state.
    fn try_random_mood_break(&mut self, i: usize) -> bool {
        if !self.can_have_mental_break(i) {
            return false;
        }
        let intensity = self.pawns[i].mind.breaker.desired_intensity();
        let defs = self.defs.clone();
        let mut rng = std::mem::replace(&mut self.rng, crate::rand::Rand::new(0));
        let chosen = select_break(
            &defs,
            intensity,
            &mut |d, _| self.break_can_occur(i, d),
            &|d| self.break_commonality(i, d),
            &mut rng,
        );
        self.rng = rng;
        let Some(chosen) = chosen else {
            return false;
        };
        let mut rng = std::mem::replace(&mut self.rng, crate::rand::Rand::new(0));
        self.with_mood(i, |mood, src| {
            mood.random_final_straw(&defs, src, &mut rng);
        });
        self.rng = rng;
        if !self.can_have_mental_break(i) {
            return false;
        }
        let Some(state) = defs.mental_breaks[chosen].mental_state.clone() else {
            return false;
        };
        self.try_start_mental_state(i, &state, true)
    }

    /// `MentalStateHandler.TryStartMentalState` (not forced): the state
    /// starts at age 0, situational thoughts are dirtied, and a state that
    /// stops jobs ends the current one and drops what the pawn carries.
    pub(super) fn try_start_mental_state(
        &mut self,
        i: usize,
        state: &str,
        caused_by_mood: bool,
    ) -> bool {
        let Some(def_id) = self.defs.mental_states.id(state) else {
            return false;
        };
        let p = &self.pawns[i];
        if p.asleep
            || p.mind
                .mental_state
                .as_ref()
                .is_some_and(|s| s.def == def_id)
            || !self.state_can_occur(i, state)
        {
            return false;
        }
        if self.pawns[i].mind.mental_state.is_some() {
            self.recover_mental_state(i);
        }
        let p = &mut self.pawns[i];
        p.mind.mental_state = Some(MentalState {
            def: def_id,
            age: 0,
            caused_by_mood,
            force_recover_after: -1,
        });
        if let Some(m) = p.mood.as_mut() {
            m.situational_dirty();
        }
        if self.defs.mental_states[def_id].stops_jobs && self.pawns[i].job.is_some() {
            self.stop_all(i, false);
            self.drop_carried(i);
        }
        true
    }

    /// `MentalState.RecoverFromState`: clear the state, gain its recovery
    /// thought (Catharsis) when mood caused it, start the break cooldown
    /// and stop the current job (keeping a lying one).
    pub(super) fn recover_mental_state(&mut self, i: usize) {
        let Some(state) = self.pawns[i].mind.mental_state.take() else {
            return;
        };
        let def = self.defs.mental_states[state.def].clone();
        if !self.pawns[i].health.dead {
            if state.caused_by_mood
                && let Some(t) = def.mood_recovery_thought.as_deref()
            {
                self.gain_memory(i, t, 0);
            }
            self.pawns[i].mind.breaker.cooldown = BREAK_COOLDOWN_TICKS;
        }
        if def.stops_jobs {
            self.stop_all(i, true);
        }
    }

    /// `Pawn_JobTracker.StopAll`: the current job ends (interrupted, its
    /// reservations released) and queued work is dropped; with
    /// `keep_laying` a pawn lying down keeps that job.
    fn stop_all(&mut self, i: usize, keep_laying: bool) {
        if keep_laying && self.pawns[i].is_lying_down() {
            return;
        }
        self.cleanup_job(i);
        let p = &mut self.pawns[i];
        p.job = None;
        p.path.clear();
        p.step = None;
        p.destination = None;
        p.asleep = false;
        p.target_queue.clear();
        p.target_queue_b.clear();
        p.count_queue.clear();
    }

    /// `Toils_Ingest.FinalizeIngest`'s thoughts for a nutritious meal whose
    /// `chairSearchRadius` is over 10: AteWithoutTable when the cell the
    /// eater faces has no eating surface while it stands and the food wants
    /// a table.
    // COMPATIBILITY TODO: currently approximate — AteInImpressiveDiningRoom
    // needs room impressiveness, which is not modelled.
    pub(super) fn table_thoughts(&mut self, i: usize, food: crate::pawn::Carried) {
        let def = &self.defs.things[food.def];
        let Some(ing) = def.ingestible.as_ref() else {
            return;
        };
        let p = &self.pawns[i];
        if p.mood.is_none()
            || crate::food::unit_nutrition(&self.defs, def) <= 0.0
            || ing.chair_search_radius <= 10.0
        {
            return;
        }
        let facing = p.position + p.rotation.facing_offset();
        let surface = self.map.size().contains(facing)
            && self.map.buildings[facing]
                .is_some_and(|b| self.defs.things[b].surface_type.as_deref() == Some("Eat"));
        if !surface && !p.is_lying_down() && ing.table_desired {
            self.gain_memory(i, "AteWithoutTable", 0);
        }
    }

    /// `FoodUtility.ThoughtsFromIngesting`, gained in `Thing.Ingested`: the
    /// taste thought, the ingredients' as-ingredient thoughts, the food's
    /// special thought and AteRottenFood for food that is no longer fresh;
    /// each once.
    // COMPATIBILITY TODO: currently approximate — trait (Ascetic, cannibal)
    // and ideology-event food thoughts, Thought_FoodEaten's food label and
    // nutrient paste hoppers are not modelled.
    pub(super) fn ingestion_thoughts(&mut self, i: usize, food: crate::pawn::Carried) {
        if self.pawns[i].mood.is_none() {
            return;
        }
        let defs = self.defs.clone();
        let def = &defs.things[food.def];
        let Some(ing) = def.ingestible.as_ref() else {
            return;
        };
        let mut thoughts: Vec<String> = Vec::new();
        let mut add = |t: &str| {
            if !thoughts.iter().any(|x| x == t) {
                thoughts.push(t.to_owned());
            }
        };
        if let Some(t) = ing.taste_thought.as_deref() {
            add(t);
        }
        if let Some(meta) = self.map.item_meta.get(&food.id) {
            for &d in &meta.ingredients {
                if let Some(t) = defs.things[d]
                    .ingestible
                    .as_ref()
                    .and_then(|x| x.special_thought_as_ingredient.as_deref())
                {
                    add(t);
                }
            }
        }
        if let Some(t) = ing.special_thought_direct.as_deref() {
            add(t);
        }
        let not_fresh = def
            .rottable
            .as_ref()
            .is_some_and(|r| food.rot >= r.ticks_to_rot_start() as f32);
        if not_fresh {
            add("AteRottenFood");
        }
        for t in thoughts {
            self.gain_memory(i, &t, 0);
        }
    }

    /// The pawn's mood: (current level, instant target), if it has mood.
    pub fn mood_of(&mut self, pawn: PawnId) -> Option<(f32, f32)> {
        let i = self.index_of(pawn)?;
        let params = self.mood_params();
        let offset = self.colonist_offset(i);
        let tick = self.tick;
        let defs = self.defs.clone();
        self.with_mood(i, |mood, src| {
            let target = mood.instant_level(tick, &defs, src, params.base, offset);
            (mood.level, target)
        })
    }

    /// A display view of the pawn's mood without changing the game: the
    /// level and target, and its thoughts as "label +N" (worst first).
    /// Uses the last counted colony wealth.
    pub fn mood_view(&self, pawn: PawnId) -> (Option<(f32, f32)>, Vec<String>) {
        let Some(i) = self.index_of(pawn) else {
            return (None, Vec::new());
        };
        let Some(mut mood) = self.pawns[i].mood.clone() else {
            return (None, Vec::new());
        };
        let wealth = self.joy_cache.wealth.map_or(0.0, |(_, w)| w);
        let mut src = PawnThoughts {
            sim: self,
            i,
            last_light_tick: mood.last_light_tick,
            expectation: self.expectation_for(wealth),
        };
        let params = self.mood_params();
        let target = mood.instant_level(
            self.tick,
            &self.defs,
            &mut src,
            params.base,
            self.colonist_offset(i),
        );
        let mut lines = mood.thought_lines(&self.defs, &mut src);
        lines.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        let thoughts = lines
            .iter()
            .map(|l| {
                let def = &self.defs.thoughts[l.def];
                let label = def
                    .stage(l.stage)
                    .map(|s| s.label.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(def.def_name.as_str());
                format!("{label} {:+.0}", l.offset)
            })
            .collect();
        (Some((mood.level, target)), thoughts)
    }

    /// The pawn's mood thoughts with their offsets.
    pub fn thoughts_of(&mut self, pawn: PawnId) -> Vec<ThoughtLine> {
        let Some(i) = self.index_of(pawn) else {
            return Vec::new();
        };
        let defs = self.defs.clone();
        self.with_mood(i, |mood, src| mood.thought_lines(&defs, src))
            .unwrap_or_default()
    }

    /// The pawn's break thresholds (extreme, major, minor).
    pub fn break_thresholds_of(&self, pawn: PawnId) -> Option<(f32, f32, f32)> {
        let i = self.index_of(pawn)?;
        Some(break_thresholds(
            self.pawn_stat_of(i, "MentalBreakThreshold"),
        ))
    }

    /// The pawn's mental state Def name, if any.
    pub fn mental_state_of(&self, pawn: PawnId) -> Option<&str> {
        let p = self.pawn(pawn)?;
        let s = p.mind.mental_state.as_ref()?;
        Some(self.defs.mental_states[s.def].def_name.as_str())
    }

    /// Gives every colonist a memory (`GiveAllStartingPlayerPawnsThought`,
    /// e.g. NewColonyOptimism at the start of a game).
    pub fn give_colonists_memory(&mut self, thought: &str) {
        for i in 0..self.pawns.len() {
            if self.pawns[i].is_colonist {
                self.gain_memory(i, thought, 0);
            }
        }
    }

    /// Debug tool: sets the pawn's mood level.
    pub fn debug_set_mood(&mut self, pawn: PawnId, level: f32) {
        if let Some(m) = self.pawn_mut(pawn).and_then(|p| p.mood.as_mut()) {
            m.level = level.clamp(0.0, 1.0);
        }
    }

    /// Debug tool: gains a memory.
    pub fn debug_gain_memory(&mut self, pawn: PawnId, thought: &str, stage: usize) {
        if let Some(i) = self.index_of(pawn) {
            self.gain_memory(i, thought, stage);
        }
    }

    /// Debug tool (`TryDoMentalBreak`): starts the named break's state as a
    /// mood-caused break, if the pawn can have one.
    pub fn debug_mental_break(&mut self, pawn: PawnId, break_def: &str) -> bool {
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        let Some(state) = self
            .defs
            .mental_breaks
            .get(break_def)
            .and_then(|b| b.mental_state.clone())
        else {
            return false;
        };
        self.can_have_mental_break(i) && self.try_start_mental_state(i, &state, true)
    }

    /// Debug tool: sets the active mental state's age.
    pub fn debug_set_mental_state_age(&mut self, pawn: PawnId, age: i32) {
        if let Some(s) = self
            .pawn_mut(pawn)
            .and_then(|p| p.mind.mental_state.as_mut())
        {
            s.age = age;
        }
    }

    /// Debug tool: sets the breaker's counters and cooldown.
    pub fn debug_set_breaker(&mut self, pawn: PawnId, breaker: crate::mood::MentalBreaker) {
        if let Some(p) = self.pawn_mut(pawn) {
            p.mind.breaker = breaker;
        }
    }

    /// The pawn's breaker state.
    pub fn breaker_of(&self, pawn: PawnId) -> Option<&crate::mood::MentalBreaker> {
        self.pawn(pawn).map(|p| &p.mind.breaker)
    }
}
