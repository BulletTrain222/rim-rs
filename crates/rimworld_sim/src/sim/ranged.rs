//! Drafting and ranged attacks in the simulation (docs/research.md §60):
//! the draft controller, the `AttackStatic` job, warmup/cooldown stances,
//! the verb's shots and projectiles in flight.

use rimworld_defs::{DefId, ThingDef, VerbProperties};

use super::Sim;
use crate::grid::Cell;
use crate::hash::{hash_offset, is_tick_interval};
use crate::job::{Job, JobKind};
use crate::path::LocomotionUrgency;
use crate::pawn::PawnId;
use crate::ranged::{
    Equipped, HIT_INTENDED, HIT_NON_TARGET_PAWNS, HIT_NON_TARGET_WORLD, Projectile, ShotAim,
    ShotMap, Stance, StanceKind, TargetFacts, UsedTarget, Vec3, decide_shot, find_shoot_line,
    hit_report, intercept_distance_factor, launch_destination, pawn_draw_pos, seconds_to_ticks,
};

/// `Verb_Shoot` experience per second of the full cycle against a hostile
/// (otherwise 20).
const SHOOT_XP_HOSTILE: f32 = 170.0;
const SHOOT_XP_OTHER: f32 = 20.0;
/// `Difficulty.friendlyFireChanceFactor` (all built-in difficulties).
const FRIENDLY_FIRE_FACTOR: f32 = 0.4;

/// The map as shots see it.
pub(super) struct SimShotMap<'a>(pub(super) &'a Sim);

impl ShotMap for SimShotMap<'_> {
    fn in_bounds(&self, c: Cell) -> bool {
        self.0.map.size().contains(c)
    }

    fn can_see_over(&self, c: Cell) -> bool {
        let map = &self.0.map;
        if !map.size().contains(c) {
            return false;
        }
        match map.buildings[c] {
            Some(b) if self.0.defs.things[b].fill_percent >= 0.99 => {
                map.door_at(c).is_some_and(|d| d.open)
            }
            _ => true,
        }
    }

    // COMPATIBILITY TODO: currently approximate — of the cell's things
    // only the building and the plant are considered (items with fill,
    // e.g. chunks, give no cover); equal fills keep the building.
    fn cover_at(&self, c: Cell) -> Option<(u64, f32)> {
        let map = &self.0.map;
        if !map.size().contains(c) {
            return None;
        }
        let key = (c.x as u64) << 20 | c.z as u64;
        let mut best: Option<(u64, f32, f32)> = None;
        if let Some(b) = map.buildings[c] {
            let fill = self.0.defs.things[b].fill_percent;
            if fill > 0.001 {
                let block = if fill >= 0.99 {
                    0.75
                } else if map.door_at(c).is_some_and(|d| d.open) {
                    0.0
                } else {
                    fill
                };
                best = Some((key, fill, block));
            }
        }
        if let Some(p) = map.plant_at(c) {
            let fill = self.0.defs.things[p.def].fill_percent;
            if fill > best.map_or(0.001, |b| b.1) {
                let block = if fill >= 0.99 { 0.75 } else { fill };
                best = Some((key | 1 << 40, fill, block));
            }
        }
        best.map(|(k, _, b)| (k, b))
    }
}

/// The equipped firearm's shot (`Verb_Shoot`) with the weapon's stats.
pub(super) struct Firearm {
    pub(super) verb: VerbProperties,
    accuracy: [f32; 4],
    cooldown_seconds: f32,
    projectile: DefId<ThingDef>,
    speed: f32,
    weapon: DefId<ThingDef>,
}

impl Sim {
    pub(super) fn firearm(&self, i: usize) -> Option<Firearm> {
        let eq = self.pawns[i].equipment.as_ref()?;
        let def = &self.defs.things[eq.def];
        let verb = def
            .verbs
            .iter()
            .find(|v| v.is_primary && v.default_projectile.is_some())?
            .clone();
        let projectile = self.defs.things.id(verb.default_projectile.as_deref()?)?;
        let speed = self.defs.things[projectile].projectile.as_ref()?.speed;
        let stat = |s: &str| def.stat(s).unwrap_or(1.0);
        Some(Firearm {
            accuracy: [
                stat("AccuracyTouch"),
                stat("AccuracyShort"),
                stat("AccuracyMedium"),
                stat("AccuracyLong"),
            ],
            cooldown_seconds: def
                .stat("RangedWeapon_Cooldown")
                .unwrap_or(verb.default_cooldown_time),
            verb,
            projectile,
            speed,
            weapon: eq.def,
        })
    }

    /// Whether the two pawns are hostile: wild animals only while
    /// manhunting (toward humanlikes); otherwise colonists and
    /// non-colonist humanlikes.
    // COMPATIBILITY TODO: currently approximate — factions are not
    // modelled.
    pub(super) fn hostile(&self, a: usize, b: usize) -> bool {
        let manhunter = |i: usize| {
            self.pawns[i].mind.mental_state.as_ref().is_some_and(|s| {
                self.defs.mental_states[s.def].state_class.as_deref()
                    == Some("MentalState_Manhunter")
            })
        };
        match (self.is_animal_index(a), self.is_animal_index(b)) {
            (true, true) => false,
            (true, false) => manhunter(a),
            (false, true) => manhunter(b),
            (false, false) => self.pawns[a].is_colonist != self.pawns[b].is_colonist,
        }
    }

    /// `VerbProperties.EffectiveMinRange`: adjacent shots at a standing
    /// hostile pawn are not allowed (at least 1.421).
    fn effective_min_range(&self, i: usize, t: usize, verb: &VerbProperties) -> f32 {
        let allow_adjacent = self.pawns[t].health.downed || !self.hostile(i, t);
        if allow_adjacent {
            verb.min_range
        } else {
            verb.min_range.max(1.421)
        }
    }

    /// `Verb.TryFindShootLineFromTo` from `root` at a pawn target.
    pub(super) fn shoot_line(
        &self,
        i: usize,
        t: usize,
        root: Cell,
    ) -> Option<crate::ranged::ShootLine> {
        let gun = self.firearm(i)?;
        let range = gun.verb.range
            * self.defs.things[gun.weapon]
                .stat("RangedWeapon_RangeMultiplier")
                .unwrap_or(1.0);
        let min = self.effective_min_range(i, t, &gun.verb);
        find_shoot_line(&SimShotMap(self), root, self.pawns[t].position, range, min)
    }

    /// Gives a pawn a weapon as its primary equipment.
    pub fn give_weapon(&mut self, pawn: PawnId, weapon: &str) -> bool {
        let (Some(i), Some(def)) = (self.index_of(pawn), self.defs.things.id(weapon)) else {
            return false;
        };
        self.pawns[i].equipment = Some(Equipped {
            def,
            verb: Default::default(),
        });
        true
    }

    /// Whether `def` is a firearm (a primary verb with a projectile).
    pub fn is_firearm(&self, def: DefId<ThingDef>) -> bool {
        self.defs.things[def]
            .verbs
            .iter()
            .any(|v| v.is_primary && v.default_projectile.is_some())
    }

    /// The player's equip order (`JobDriver_Equip`, forced): walk to the
    /// firearm and take it; a weapon already held is dropped.
    // COMPATIBILITY TODO: currently approximate — only firearms can be
    // equipped; the bonded/biocoded and inventory cases are not modelled.
    pub fn order_equip(
        &mut self,
        pawn: PawnId,
        item: crate::map::ItemId,
    ) -> Result<(), &'static str> {
        let Some(i) = self.index_of(pawn) else {
            return Err("no such pawn");
        };
        let Some(it) = self.map.item(item) else {
            return Err("no such item");
        };
        if !self.is_firearm(it.def) {
            return Err("not a weapon");
        }
        if !self.pawns[i].is_colonist || self.pawns[i].health.downed {
            return Err("can't equip");
        }
        self.interrupt_for_order(i);
        self.drop_carried(i);
        let def = self.job_defs.equip;
        if self.start_job(
            i,
            Job {
                def,
                kind: JobKind::Equip { item },
                forced: true,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        ) {
            Ok(())
        } else {
            Err("can't reach it")
        }
    }

    /// The equip job starts: walk to touch the weapon (or take it now).
    pub(super) fn begin_equip(&mut self, i: usize) -> bool {
        let Some(JobKind::Equip { item }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return false;
        };
        let Some(cell) = self.map.item(item).map(|it| it.position) else {
            return false;
        };
        match self.walk_to_touch(i, cell, false) {
            super::farming::Touch::Here => {
                self.equip_arrived(i);
                true
            }
            super::farming::Touch::Walking => true,
            super::farming::Touch::NoPath => false,
        }
    }

    /// At the weapon: drop the old one, take one of the stack.
    pub(super) fn equip_arrived(&mut self, i: usize) {
        let Some(JobKind::Equip { item }) = self.pawns[i].job.as_ref().map(|j| j.kind) else {
            return;
        };
        let Some(def) = self.map.item(item).map(|it| it.def) else {
            self.end_job(i, false);
            return;
        };
        self.drop_equipment(i);
        self.map.take_from_item(item, 1);
        self.refresh_path_grid();
        self.pawns[i].equipment = Some(Equipped {
            def,
            verb: Default::default(),
        });
        self.end_job(i, true);
    }

    /// `Pawn_EquipmentTracker.TryDropEquipment`: the primary weapon goes
    /// on the ground near the pawn; its warmup stops.
    pub(super) fn drop_equipment(&mut self, i: usize) {
        let Some(eq) = self.pawns[i].equipment.take() else {
            return;
        };
        if self.pawns[i]
            .stance
            .as_ref()
            .is_some_and(|s| s.kind == StanceKind::Warmup)
        {
            self.pawns[i].stance = None;
        }
        let at = self.pawns[i].position;
        let mut c = crate::pawn::Carried {
            id: self.map.allocate_item_id(),
            def: eq.def,
            count: 1,
            rot: 0.0,
            hit_points: None,
        };
        if !self.place_thing_near(&mut c, at) {
            self.map.spawn_carried(&c, at);
        }
        self.refresh_path_grid();
    }

    /// Whether the pawn holds a firearm.
    pub fn has_firearm(&self, pawn: PawnId) -> bool {
        self.index_of(pawn)
            .is_some_and(|i| self.firearm(i).is_some())
    }

    pub fn is_drafted(&self, pawn: PawnId) -> bool {
        self.pawn(pawn).is_some_and(|p| p.drafted)
    }

    /// `Pawn_DraftController.Drafted`: a change clears queued work, ends
    /// the current job (warmup cancelled, cooldown kept) and thinks again
    /// — a drafted pawn stands ready (`Wait_Combat`); undrafting drops
    /// what it carries and releases its destination.
    // COMPATIBILITY TODO: currently approximate — every job counts as
    // player-interruptible; the auto-undrafter and fire-at-will automatic
    // targeting are not modelled.
    pub fn set_drafted(&mut self, pawn: PawnId, drafted: bool) -> bool {
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        let p = &self.pawns[i];
        if p.drafted == drafted {
            return true;
        }
        if drafted
            && (p.health.downed || p.health.dead || p.mind.mental_state.is_some() || !p.is_colonist)
        {
            return false;
        }
        self.pawns[i].drafted = drafted;
        if !drafted {
            self.destinations.release_all_claimed_by(pawn);
            self.drop_carried(i);
        }
        self.interrupt_for_order(i);
        self.find_and_start_job(i);
        true
    }

    /// Ends the current job for a player order (`EndCurrentJob`
    /// InterruptForced): reservations released, queued targets cleared,
    /// warmup soft-cancelled (cooldown kept).
    pub(super) fn interrupt_for_order(&mut self, i: usize) {
        self.cleanup_job(i);
        let p = &mut self.pawns[i];
        p.job = None;
        p.path.clear();
        p.destination = None;
        p.asleep = false;
        p.target_queue.clear();
        p.target_queue_b.clear();
        p.count_queue.clear();
        if p.stance
            .as_ref()
            .is_some_and(|s| s.kind == StanceKind::Warmup)
        {
            p.stance = None;
        }
    }

    /// `FloatMenuUtility.GetRangedAttackAction` and its order: a drafted
    /// colonist with a firearm, a target it can hit from where it stands
    /// (range, line of sight) → a forced `AttackStatic` job; the pawn does
    /// not move.
    pub fn order_ranged_attack(
        &mut self,
        pawn: PawnId,
        target: PawnId,
    ) -> Result<(), &'static str> {
        self.ranged_attack_check(pawn, target)?;
        let (Some(i), Some(t)) = (self.index_of(pawn), self.index_of(target)) else {
            return Err("no such pawn");
        };
        let started_downed = self.pawns[t].health.downed;
        self.interrupt_for_order(i);
        let def = self.job_defs.attack_static;
        self.start_job(
            i,
            Job {
                def,
                kind: JobKind::AttackStatic {
                    target,
                    started_downed,
                    attacks: 0,
                },
                forced: true,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        );
        Ok(())
    }

    /// Why [`Sim::order_ranged_attack`] would refuse (`GetRangedAttackAction`'s
    /// fail reasons: "not drafted", "out of range", "too close", "no line
    /// of sight", ...), without ordering anything.
    pub fn ranged_attack_check(&self, pawn: PawnId, target: PawnId) -> Result<(), &'static str> {
        let (Some(i), Some(t)) = (self.index_of(pawn), self.index_of(target)) else {
            return Err("no such pawn");
        };
        if i == t {
            return Err("can't shoot itself");
        }
        if !self.pawns[i].drafted {
            return Err("not drafted");
        }
        let Some(gun) = self.firearm(i) else {
            return Err("no ranged weapon");
        };
        if self.pawns[t].health.dead {
            return Err("target is dead");
        }
        let root = self.pawns[i].position;
        let d2 = {
            let tc = self.pawns[t].position;
            let (dx, dz) = (tc.x - root.x, tc.z - root.z);
            (dx * dx + dz * dz) as f32
        };
        if d2 > gun.verb.range * gun.verb.range {
            return Err("out of range");
        }
        let min = self.effective_min_range(i, t, &gun.verb);
        if d2 < min * min {
            return Err("too close");
        }
        if self.shoot_line(i, t, root).is_none() {
            return Err("no line of sight");
        }
        Ok(())
    }

    /// `JobDriver_AttackStatic`'s interval action: done when the target is
    /// gone, dead or newly downed; otherwise try to start an attack.
    pub(super) fn attack_static_interval(&mut self, i: usize) {
        let Some(JobKind::AttackStatic {
            target,
            started_downed,
            attacks,
        }) = self.pawns[i].job.as_ref().map(|j| j.kind)
        else {
            return;
        };
        let Some(t) = self.index_of(target) else {
            self.end_job(i, true);
            return;
        };
        let tp = &self.pawns[t];
        if tp.health.dead || (!started_downed && tp.health.downed) {
            self.end_job(i, true);
            return;
        }
        if self.try_start_cast(i, t, true)
            && let Some(JobKind::AttackStatic { attacks: a, .. }) =
                self.pawns[i].job.as_mut().map(|j| &mut j.kind)
        {
            *a = attacks + 1;
        }
    }

    /// `Pawn.TryStartAttack` / `Verb.TryStartCastOn` with the primary
    /// firearm: not while busy or mid-burst; the target must be hittable; a
    /// warmup stance starts. `non_target_pawns`: whether the shot may hit
    /// other pawns (Hunt: no).
    pub(super) fn try_start_cast(&mut self, i: usize, t: usize, non_target_pawns: bool) -> bool {
        if self.pawns[i].stance.is_some() {
            return false;
        }
        let Some(gun) = self.firearm(i) else {
            return false;
        };
        if self.pawns[i]
            .equipment
            .as_ref()
            .is_some_and(|e| e.verb.bursting)
        {
            return false;
        }
        let root = self.pawns[i].position;
        if self.shoot_line(i, t, root).is_none() {
            return false;
        }
        let target = self.pawns[t].id;
        if let Some(eq) = self.pawns[i].equipment.as_mut() {
            eq.verb.target = Some(target);
            eq.verb.can_hit_non_target_pawns = non_target_pawns;
        }
        let warmup = gun.verb.warmup_time
            * self.defs.things[gun.weapon]
                .stat("RangedWeapon_WarmupMultiplier")
                .unwrap_or(1.0);
        if warmup > 0.0 {
            let aiming = self.pawn_stat_of(i, "AimingDelayFactor");
            let ticks = seconds_to_ticks(warmup * aiming);
            let downed = self.pawns[t].health.downed;
            self.pawns[i].stance = Some(Stance {
                kind: StanceKind::Warmup,
                ticks_left: ticks,
                target: Some(target),
                target_started_downed: downed,
            });
        } else {
            self.warmup_complete(i);
        }
        true
    }

    /// `Pawn_StanceTracker.StanceTrackerTick`: a warmup is cancelled when
    /// its target newly downs, despawns or can't be hit from here; busy
    /// stances count down and expire (a warmup then shoots).
    // COMPATIBILITY TODO: currently approximate — stuns and stagger are
    // not modelled.
    pub(super) fn stance_tick(&mut self, i: usize) {
        let Some(stance) = self.pawns[i].stance.clone() else {
            return;
        };
        if stance.kind == StanceKind::Warmup {
            let t = stance.target.and_then(|t| self.index_of(t));
            let cancel = match t {
                None => true,
                Some(t) => {
                    let tp = &self.pawns[t];
                    (!stance.target_started_downed && tp.health.downed)
                        || tp.health.dead
                        || self.shoot_line(i, t, self.pawns[i].position).is_none()
                }
            };
            if cancel {
                self.pawns[i].stance = None;
                return;
            }
        }
        let left = stance.ticks_left - 1;
        if let Some(s) = self.pawns[i].stance.as_mut() {
            s.ticks_left = left;
        }
        if left <= 0 {
            match stance.kind {
                StanceKind::Warmup => {
                    self.warmup_complete(i);
                    // A new cooldown installed by the shot stays.
                    if self.pawns[i]
                        .stance
                        .as_ref()
                        .is_some_and(|s| s.kind == StanceKind::Warmup)
                    {
                        self.pawns[i].stance = None;
                    }
                }
                StanceKind::Cooldown => self.pawns[i].stance = None,
            }
        }
    }

    /// `Verb_Shoot.WarmupComplete`: shooting experience once per burst
    /// at a standing target, then the burst starts with its first shot.
    fn warmup_complete(&mut self, i: usize) {
        let Some(gun) = self.firearm(i) else {
            return;
        };
        let target = self.pawns[i].equipment.as_ref().and_then(|e| e.verb.target);
        if let Some(t) = target.and_then(|t| self.index_of(t))
            && !self.pawns[t].health.downed
        {
            let cycle = gun.verb.warmup_time
                + gun.cooldown_seconds * self.pawn_stat_of(i, "RangedCooldownFactor")
                + (gun.verb.burst_shot_count - 1) as f32
                    * gun.verb.ticks_between_burst_shots as f32
                    / 60.0;
            let rate = if self.hostile(i, t) {
                SHOOT_XP_HOSTILE
            } else {
                SHOOT_XP_OTHER
            };
            self.learn(i, "Shooting", rate * cycle);
        }
        if let Some(eq) = self.pawns[i].equipment.as_mut() {
            eq.verb.burst_shots_left = gun.verb.burst_shot_count;
            eq.verb.bursting = true;
        }
        self.try_cast_next_burst_shot(i);
    }

    /// `Verb.TryCastNextBurstShot`: a shot; more shots left → wait the
    /// burst spacing (cooldown stance of spacing + 1); else the verb's
    /// cooldown (`RangedWeapon_Cooldown` × `RangedCooldownFactor`).
    fn try_cast_next_burst_shot(&mut self, i: usize) {
        let Some(gun) = self.firearm(i) else {
            return;
        };
        let shot = self.try_cast_shot(i);
        let target = self.pawns[i].equipment.as_ref().and_then(|e| e.verb.target);
        let Some(eq) = self.pawns[i].equipment.as_mut() else {
            return;
        };
        if shot {
            eq.verb.burst_shots_left -= 1;
        } else {
            eq.verb.burst_shots_left = 0;
        }
        if eq.verb.burst_shots_left > 0 {
            eq.verb.ticks_to_next_burst_shot = gun.verb.ticks_between_burst_shots;
            self.pawns[i].stance = Some(Stance {
                kind: StanceKind::Cooldown,
                ticks_left: gun.verb.ticks_between_burst_shots + 1,
                target,
                target_started_downed: false,
            });
            return;
        }
        eq.verb.bursting = false;
        let factor = self.pawn_stat_of(i, "RangedCooldownFactor");
        let ticks = seconds_to_ticks(gun.cooldown_seconds * factor);
        self.pawns[i].stance = Some(Stance {
            kind: StanceKind::Cooldown,
            ticks_left: ticks,
            target,
            target_started_downed: false,
        });
    }

    /// The equipment verb's tick (`Verb.VerbTick`): between burst shots.
    pub(super) fn verb_tick(&mut self, i: usize) {
        let Some(eq) = self.pawns[i].equipment.as_mut() else {
            return;
        };
        if !eq.verb.bursting {
            return;
        }
        eq.verb.ticks_to_next_burst_shot -= 1;
        if eq.verb.ticks_to_next_burst_shot <= 0 {
            self.try_cast_next_burst_shot(i);
        }
    }

    /// `Verb_LaunchProjectile.TryCastShot`: the shoot line again (no shot
    /// without one), the report and decision, then the projectile.
    fn try_cast_shot(&mut self, i: usize) -> bool {
        let Some(gun) = self.firearm(i) else {
            return false;
        };
        let Some(target) = self.pawns[i].equipment.as_ref().and_then(|e| e.verb.target) else {
            return false;
        };
        let Some(t) = self.index_of(target) else {
            return false;
        };
        if self.pawns[t].health.dead {
            return false;
        }
        let root = self.pawns[i].position;
        let line = self.shoot_line(i, t, root);
        if gun.verb.stop_burst_without_los && line.is_none() {
            return false;
        }
        let Some(line) = line else {
            return false;
        };
        let tick = self.tick;
        if let Some(eq) = self.pawns[i].equipment.as_mut() {
            eq.verb.last_shot_tick = tick;
        }
        let origin = pawn_draw_pos(self.pawns[i].position, self.pawns[i].id_number);
        let accuracy = self.pawn_stat_of(i, "ShootingAccuracyPawn");
        let race = &self.defs.things[self.pawns[t].race];
        let facts = TargetFacts {
            cell: self.pawns[t].position,
            body_size: race.race.as_ref().map_or(1.0, |r| r.base_body_size),
            standing: !self.pawns[t].health.downed && !self.pawns[t].is_lying_down(),
        };
        let report = hit_report(
            &SimShotMap(self),
            root,
            accuracy,
            gun.accuracy,
            gun.verb.can_go_wild,
            facts,
        );
        // COMPATIBILITY TODO: currently approximate — forced-miss-radius
        // weapons (mortars, launchers) are not supported.
        let non_target = self.pawns[i]
            .equipment
            .as_ref()
            .is_none_or(|e| e.verb.can_hit_non_target_pawns);
        let (aim, flags) = decide_shot(
            &report,
            line,
            gun.verb.can_go_wild,
            non_target,
            &mut self.rng,
        );
        let used = match aim {
            ShotAim::Hit => UsedTarget::Pawn(target),
            ShotAim::Wild(c) => UsedTarget::Cell(c),
            ShotAim::Cover(c) => UsedTarget::Cover(c.thing, c.cell),
        };
        let used_cell = match used {
            UsedTarget::Pawn(_) => self.pawns[t].position,
            UsedTarget::Cell(c) | UsedTarget::Cover(_, c) => c,
        };
        let destination = launch_destination(used_cell, &mut self.rng);
        let start = crate::ranged::starting_ticks(origin, destination, gun.speed);
        let ticks = (start.ceil() as i32).max(1);
        let id_number = self.map.allocate_item_id().0 as i32;
        self.projectiles.push(Projectile {
            id_number,
            def: gun.projectile,
            origin,
            destination,
            ticks_to_impact: ticks,
            lifetime: ticks,
            used,
            intended: Some(target),
            flags,
            launcher: self.pawns[i].id,
            equipment_def: Some(gun.weapon),
            prevent_friendly_fire: false,
            position: line.source,
            tick_delta: 0,
            spawned_tick: self.tick,
        });
        true
    }

    /// Debug tool (the research probe's isolated shot): one shot by the
    /// pawn's firearm at `target` under a seeded random stream
    /// (`Rand.PushState(seed)`), without job or warmup.
    pub fn debug_fire_seeded(&mut self, pawn: PawnId, target: PawnId, seed: i32) -> bool {
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        if let Some(eq) = self.pawns[i].equipment.as_mut() {
            eq.verb.target = Some(target);
        }
        self.rng.push_state_seeded(seed);
        let ok = self.try_cast_shot(i);
        self.rng.pop_state();
        ok
    }

    /// The pawn's stance: (warmup?, ticks left), if busy.
    pub fn stance_of(&self, pawn: PawnId) -> Option<(bool, i32)> {
        let s = self.pawn(pawn)?.stance.as_ref()?;
        Some((s.kind == StanceKind::Warmup, s.ticks_left))
    }

    /// Projectiles in flight.
    pub fn projectiles(&self) -> &[Projectile] {
        &self.projectiles
    }

    /// A projectile's current float position (for drawing).
    pub fn projectile_position(&self, p: &Projectile) -> Vec3 {
        let speed = self.defs.things[p.def]
            .projectile
            .as_ref()
            .map_or(5.0, |x| x.speed);
        p.exact_position(speed)
    }

    /// Debug tool: the next projectile's thing id (sets its tick phase).
    pub fn debug_set_next_thing_id(&mut self, id: u32) {
        self.map.debug_set_next_item_id(id);
    }

    /// Each projectile's tick: its interval part runs when its update rate
    /// (or hash phase) comes up (`Thing.DoTick`).
    pub(super) fn tick_projectiles(&mut self) {
        let t = self.tick;
        let rate = self.default_update_rate.clamp(1, 15);
        let mut k = 0;
        while k < self.projectiles.len() {
            if self.projectiles[k].spawned_tick >= t {
                k += 1;
                continue;
            }
            let p = &mut self.projectiles[k];
            p.tick_delta += 1;
            let due =
                p.tick_delta >= rate || is_tick_interval(t, hash_offset(p.id_number), rate as i32);
            if due {
                let delta = p.tick_delta as i32;
                p.tick_delta = 0;
                if self.projectile_interval(k, delta) {
                    self.projectiles.remove(k);
                    continue;
                }
            }
            k += 1;
        }
    }

    /// `Projectile.TickInterval`; returns whether it is gone.
    fn projectile_interval(&mut self, k: usize, delta: i32) -> bool {
        let speed = {
            let p = &self.projectiles[k];
            self.defs.things[p.def]
                .projectile
                .as_ref()
                .map_or(5.0, |x| x.speed)
        };
        let p = &mut self.projectiles[k];
        p.lifetime -= delta;
        let before = p.exact_position(speed);
        p.ticks_to_impact -= delta;
        let after = p.exact_position(speed);
        if !self.map.size().contains(after.cell()) {
            return true;
        }
        if self.check_free_intercept_between(k, before, after) {
            return true;
        }
        let p = &mut self.projectiles[k];
        p.position = after.cell();
        if p.ticks_to_impact <= 0 {
            let dest = p.destination.cell();
            if self.map.size().contains(dest) {
                self.projectiles[k].position = dest;
            }
            self.impact_something(k);
            return true;
        }
        false
    }

    /// `CheckForFreeInterceptBetween`: cells along the segment in 0.2-cell
    /// steps (the next cell directly when cardinally adjacent).
    fn check_free_intercept_between(&mut self, k: usize, from: Vec3, to: Vec3) -> bool {
        if from == to {
            return false;
        }
        let (a, b) = (from.cell(), to.cell());
        if a == b || !self.map.size().contains(a) || !self.map.size().contains(b) {
            return false;
        }
        if (a.x - b.x).abs() + (a.z - b.z).abs() == 1 {
            return self.check_free_intercept(k, b);
        }
        if intercept_distance_factor(self.projectiles[k].origin, b) <= 0.0 {
            return false;
        }
        let v = to - from;
        let mag = v.magnitude();
        let step = Vec3::new(
            (v.x as f64 / mag as f64 * 0.2) as f32,
            (v.y as f64 / mag as f64 * 0.2) as f32,
            (v.z as f64 / mag as f64 * 0.2) as f32,
        );
        let hmag = ((v.x as f64 * v.x as f64 + v.z as f64 * v.z as f64).sqrt()) as f32;
        let n = (hmag / 0.2) as i32;
        let mut checked: Vec<Cell> = Vec::new();
        let mut pos = from;
        let mut count = 0;
        loop {
            pos = Vec3::new(pos.x + step.x, pos.y + step.y, pos.z + step.z);
            let c = pos.cell();
            if !checked.contains(&c) {
                if self.check_free_intercept(k, c) {
                    return true;
                }
                checked.push(c);
            }
            count += 1;
            if count > n || c == b {
                return false;
            }
        }
    }

    /// Whether a projectile can hit a pawn (`Projectile.CanHit`).
    fn can_hit_pawn(&self, k: usize, pawn: PawnId) -> bool {
        let p = &self.projectiles[k];
        let Some(i) = self.index_of(pawn) else {
            return false;
        };
        if pawn == p.launcher || p.flags == 0 || self.pawns[i].health.dead {
            return false;
        }
        if Some(pawn) == p.intended {
            p.flags & HIT_INTENDED != 0
        } else {
            p.flags & HIT_NON_TARGET_PAWNS != 0
        }
    }

    /// `CheckForFreeIntercept` at a cell: walls stop the bullet; pawns
    /// (0.4 × body size, ×0.1 lying, friendly × 0.4) and partial cover
    /// (fill × 0.15, or × 1 next to the destination) by chance, × the
    /// distance factor; a roll only above 1e-5.
    // COMPATIBILITY TODO: currently approximate — the cell's things are
    // checked building first, then pawns in spawn order (the game's thing
    // list order); plants and items are not interceptors.
    fn check_free_intercept(&mut self, k: usize, c: Cell) -> bool {
        let (origin, dest_cell, launcher) = {
            let p = &self.projectiles[k];
            (p.origin, p.destination.cell(), p.launcher)
        };
        if dest_cell == c {
            return false;
        }
        let factor = intercept_distance_factor(origin, c);
        if factor <= 0.0 {
            return false;
        }
        if let Some(b) = self.map.buildings[c] {
            let def = &self.defs.things[b];
            let world = self.projectiles[k].flags & HIT_NON_TARGET_WORLD != 0;
            if world {
                let open_door = self.map.door_at(c).is_some_and(|d| d.open);
                let chance = if def.fill_percent >= 0.99 && !open_door {
                    self.impact(k, None);
                    return true;
                } else if open_door {
                    0.05
                } else if def.fill_percent > 0.2 {
                    let adj = (dest_cell.x - c.x).abs() <= 1 && (dest_cell.z - c.z).abs() <= 1;
                    def.fill_percent * if adj { 1.0 } else { 0.15 }
                } else {
                    0.0
                };
                let chance = chance * factor;
                if chance > 1e-5 && self.rng.chance(chance) {
                    self.impact(k, None);
                    return true;
                }
            }
        }
        let li = self.index_of(launcher);
        let pawns: Vec<usize> = (0..self.pawns.len())
            .filter(|&j| self.pawns[j].position == c && self.pawns[j].carried_by.is_none())
            .collect();
        for j in pawns {
            let id = self.pawns[j].id;
            if !self.can_hit_pawn(k, id) {
                continue;
            }
            let size = self.defs.things[self.pawns[j].race]
                .race
                .as_ref()
                .map_or(1.0, |r| r.base_body_size);
            let mut chance = 0.4 * size.clamp(0.1, 2.0);
            if self.pawns[j].health.downed || self.pawns[j].is_lying_down() {
                chance *= 0.1;
            }
            if let Some(li) = li
                && !self.hostile(li, j)
            {
                chance = if self.projectiles[k].prevent_friendly_fire {
                    0.0
                } else {
                    chance * FRIENDLY_FIRE_FACTOR
                };
            }
            let chance = chance * factor;
            if chance > 1e-5 && self.rng.chance(chance) {
                self.impact(k, Some(id));
                return true;
            }
        }
        false
    }

    /// `ImpactSomething`: the used pawn if it can still be hit (a lying one
    /// at 4.5+ cells only by a 0.5 roll), else what is on the cell
    /// (shuffled, by chance), else the ground.
    // COMPATIBILITY TODO: currently approximate — cover things and
    // buildings take no damage (bullets hitting them just stop).
    fn impact_something(&mut self, k: usize) {
        let p = self.projectiles[k].clone();
        let far = (p.origin - p.destination).magnitude_horizontal_squared() >= 20.25;
        if let UsedTarget::Pawn(pawn) = p.used
            && self.can_hit_pawn(k, pawn)
        {
            let i = self.index_of(pawn).unwrap();
            let lying = self.pawns[i].health.downed || self.pawns[i].is_lying_down();
            if lying && far && !self.rng.chance(0.5) {
                self.impact(k, None);
            } else {
                self.impact(k, Some(pawn));
            }
            return;
        }
        let mut here: Vec<usize> = (0..self.pawns.len())
            .filter(|&j| {
                self.pawns[j].position == p.position
                    && self.pawns[j].carried_by.is_none()
                    && self.can_hit_pawn(k, self.pawns[j].id)
            })
            .collect();
        self.rng.shuffle(&mut here);
        let li = self.index_of(p.launcher);
        for j in here {
            let size = self.defs.things[self.pawns[j].race]
                .race
                .as_ref()
                .map_or(1.0, |r| r.base_body_size);
            let mut chance = 0.5 * size.clamp(0.1, 2.0);
            if (self.pawns[j].health.downed || self.pawns[j].is_lying_down()) && far {
                chance *= 0.5;
            }
            if let Some(li) = li
                && !self.hostile(li, j)
            {
                chance *= intercept_distance_factor(p.origin, p.position);
            }
            if self.rng.chance(chance) {
                let id = self.pawns[j].id;
                self.impact(k, Some(id));
                return;
            }
        }
        self.impact(k, None);
    }

    /// `Bullet.Impact`: damage to the pawn hit (the projectile's damage,
    /// its armor penetration, no chosen part), handed to the health and
    /// armor pipeline.
    // COMPATIBILITY TODO: currently approximate — the damage angle,
    // instigator/weapon records, battle log, stagger and nearby-impact
    // notifications are not modelled.
    fn impact(&mut self, k: usize, hit: Option<PawnId>) {
        // `Projectile.Impact`'s clamor comes before the damage.
        let (pos, launcher) = {
            let p = &self.projectiles[k];
            let speed = self.defs.things[p.def]
                .projectile
                .as_ref()
                .map_or(5.0, |x| x.speed);
            (p.exact_position(speed), p.launcher)
        };
        self.impact_clamor(pos, launcher);
        let Some(pawn) = hit else {
            return;
        };
        let p = &self.projectiles[k];
        let Some(props) = self.defs.things[p.def].projectile.clone() else {
            return;
        };
        let Some(damage) = props.damage_def.clone() else {
            return;
        };
        let mult = p
            .equipment_def
            .and_then(|w| self.defs.things[w].stat("RangedWeapon_DamageMultiplier"))
            .unwrap_or(1.0);
        // COMPATIBILITY TODO: currently approximate — a projectile without
        // `damageAmountBase` should use its DamageDef's default damage.
        let base = if props.damage_amount_base != -1 {
            props.damage_amount_base as f32
        } else {
            1.0
        };
        let amount = ((base as f64 * mult as f64) as f32).round_ties_even();
        let pen_mult = p
            .equipment_def
            .and_then(|w| self.defs.things[w].stat("RangedWeapon_ArmorPenetrationMultiplier"))
            .unwrap_or(1.0);
        let pen = if props.armor_penetration_base >= 0.0 {
            props.armor_penetration_base
        } else {
            (base.round_ties_even() as f64 * 0.015) as f32
        } * pen_mult;
        self.damage_pawn_full(pawn, &damage, amount, pen, None, None);
        if let Some(t) = self.index_of(pawn) {
            self.notify_damage_taken(t, launcher, &damage);
        }
    }
}
