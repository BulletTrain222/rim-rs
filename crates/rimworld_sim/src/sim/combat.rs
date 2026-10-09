//! The melee attack job (`JobDriver_AttackMelee` with
//! `Toils_Combat.FollowAndMeleeAttack`).

use super::{PATH_START_LATENCY_TICKS, Sim};
use crate::combat::{Attacker, dodge_chance, melee_verbs};
use crate::grid::Cell;
use crate::job::{Job, JobKind};
use crate::path::{COLONIST_HEURISTIC_STRENGTH, LocomotionUrgency, find_path};
use crate::pawn::PawnId;

/// What one melee swing did (for tests and the UI).
#[derive(Debug, Clone, PartialEq)]
pub enum Swing {
    Miss,
    Dodged,
    Hit { damage: String, amount: f32 },
}

impl Sim {
    /// Orders `attacker` to fight `target` in melee until the target is
    /// downed (a forced `AttackMelee` job).
    pub fn order_melee_attack(&mut self, attacker: PawnId, target: PawnId) -> bool {
        let Some(i) = self.index_of(attacker) else {
            return false;
        };
        if attacker == target || self.index_of(target).is_none() || self.pawns[i].health.downed {
            return false;
        }
        self.drop_carried(i);
        let def = self.defs.jobs.id("AttackMelee");
        self.start_job(
            i,
            Job {
                def,
                kind: JobKind::AttackMelee { target },
                forced: true,
                urgency: LocomotionUrgency::Jog,
                start_tick: 0,
            },
            false,
        )
    }

    /// The per-tick part of the melee job: the path follower moves the pawn
    /// (decisions happen in [`Sim::melee_interval`]).
    pub(super) fn melee_tick(&mut self, i: usize) {
        let t = self.tick;
        super::tick_movement(&mut self.pawns[i], &self.path_grid, &self.map, t);
    }

    /// `Toils_Combat.FollowAndMeleeAttack`'s interval action: on the pawn's
    /// interval ticks, start a path towards the target when not heading
    /// there (or standing out of reach), else swing when touching it and
    /// the cooldown stance is over. A downed target ends the job.
    // COMPATIBILITY TODO: currently approximate — attack target
    // reservations, surprise attacks, the victim reacting (fleeing,
    // fighting back), melee XP and the 250-tick reachability check are not
    // modelled.
    pub(super) fn melee_interval(&mut self, i: usize) {
        // `PredatorHunt`'s follow toil is the same follow-and-attack, with
        // `killIncappedTarget` (downed prey is attacked on) and a surprise
        // first attack on non-colonists.
        let (target, predator, first_hit) = match self.pawns[i].job.as_ref().map(|j| j.kind) {
            Some(JobKind::AttackMelee { target }) => (target, false, false),
            Some(JobKind::PredatorHunt {
                prey,
                stage: crate::job::PredatorStage::Follow,
                first_hit,
                ..
            }) => (prey, true, first_hit),
            _ => return,
        };
        let Some(ti) = self.index_of(target) else {
            self.end_job(i, false);
            return;
        };
        if self.pawns[ti].health.dead {
            if !predator {
                self.end_job(i, true);
            }
            return;
        }
        let t = self.tick;
        let at = self.pawns[i].position;
        let tpos = self.pawns[ti].position;
        let touching = self.pawns[i].step.is_none() && melee_touch(&self.path_grid, at, tpos);
        let heading = self.pawns[i]
            .destination
            .is_some_and(|d| d.chebyshev(tpos) <= 1);
        let moving = self.pawns[i].is_moving();
        // Out of reach: path when standing still, or when the path no
        // longer leads to the target (it moved).
        if !touching && (!moving || !heading) {
            // `StartPath(target, Touch)`.
            let pawn = &mut self.pawns[i];
            pawn.path.clear();
            pawn.destination = None;
            match find_path(
                &self.path_grid,
                at,
                tpos,
                pawn.move_costs,
                COLONIST_HEURISTIC_STRENGTH,
            ) {
                Ok(path) => {
                    let mut cells: Vec<Cell> = Vec::new();
                    for c in path.cells {
                        cells.push(c);
                        if c.chebyshev(tpos) <= 1 {
                            break;
                        }
                    }
                    if let Some(&end) = cells.last() {
                        pawn.path = cells.into();
                        pawn.destination = Some(end);
                        pawn.move_ready_tick = t + PATH_START_LATENCY_TICKS;
                    }
                }
                Err(_) => self.end_job(i, false),
            }
            return;
        }
        if touching {
            if self.pawns[ti].health.downed && !predator {
                self.end_job(i, true);
                return;
            }
            if t >= self.pawns[i].stance_until {
                let surprise = first_hit && !self.pawns[ti].is_colonist;
                self.melee_swing_with(i, ti, surprise);
                if let Some(crate::job::Job {
                    kind: JobKind::PredatorHunt { first_hit, .. },
                    ..
                }) = &mut self.pawns[i].job
                {
                    *first_hit = false;
                }
            }
        }
    }

    /// One melee attack (`Verb_MeleeAttack.TryCastShot`): choose a verb,
    /// roll to hit (`MeleeHitChance`) and to dodge (`MeleeDodgeChance`),
    /// apply the damage to an outside part, then cool down.
    /// A surprise attack can't miss or be dodged (`Verb_MeleeAttack`).
    // COMPATIBILITY TODO: currently approximate — tools' surprise-attack
    // extra damages are not applied.
    fn melee_swing_with(&mut self, i: usize, ti: usize, surprise: bool) -> Option<Swing> {
        let defs = self.defs.clone();
        let race = &defs.things[self.pawns[i].race];
        let verbs = melee_verbs(&defs, race);
        let (weights, amounts, cooldowns, cooldown_secs, hit) = {
            let view = self.health_view(self.pawns[i].id)?;
            let attacker = Attacker {
                defs: &defs,
                race,
                skills: &self.pawns[i].skills,
                health: &view,
            };
            (
                attacker.final_weights(&verbs),
                verbs
                    .iter()
                    .map(|v| attacker.damage_amount(v))
                    .collect::<Vec<_>>(),
                verbs
                    .iter()
                    .map(|v| (attacker.cooldown_seconds(v) * 60.0).round() as u64)
                    .collect::<Vec<_>>(),
                verbs
                    .iter()
                    .map(|v| attacker.cooldown_seconds(v))
                    .collect::<Vec<_>>(),
                attacker.hit_chance(),
            )
        };
        // `ChooseMeleeVerb`: the terrain-tool roll, then the weighted pick.
        let _terrain = self.rng.chance(0.04);
        let v = crate::region::random_element_by_weight(&weights, &mut self.rng)?;
        let verb = verbs[v].clone();
        let (amount, cooldown) = (amounts[v], cooldowns[v]);
        let target_immobile = self.pawns[ti].health.downed;
        // Melee experience for every swing at a target that can fight back:
        // 200 × the verb's full cycle time (no warmup for melee).
        if !target_immobile {
            self.learn(i, "Melee", 200.0 * cooldown_secs[v]);
        }
        let swing = if !self.rng.chance(if target_immobile || surprise {
            1.0
        } else {
            hit
        }) {
            Swing::Miss
        } else {
            let tview = self.health_view(self.pawns[ti].id)?;
            let traced = &defs.things[self.pawns[ti].race];
            let dodge = if surprise {
                0.0
            } else {
                dodge_chance(
                    &defs,
                    traced,
                    &self.pawns[ti].skills,
                    &tview,
                    target_immobile,
                )
            };
            if self.rng.chance(dodge) {
                Swing::Dodged
            } else {
                let target = self.pawns[ti].id;
                // `VerbProperties.AdjustedArmorPenetration`: the tool's, or
                // 1.5% of the damage.
                let penetration = if verb.armor_penetration < 0.0 {
                    amount * 0.015
                } else {
                    verb.armor_penetration
                };
                self.damage_pawn_full(
                    target,
                    &verb.damage,
                    amount,
                    penetration,
                    None,
                    Some(rimworld_defs::health::PartDepth::Outside),
                );
                // `Pawn_MindState.Notify_DamageTaken`: revenge or flight.
                let attacker = self.pawns[i].id;
                self.notify_damage_taken(ti, attacker, &verb.damage);
                Swing::Hit {
                    damage: verb.damage.clone(),
                    amount,
                }
            }
        };
        self.pawns[i].stance_until = self.tick + cooldown;
        self.last_swings.push((self.pawns[i].id, swing.clone()));
        Some(swing)
    }

    /// Melee swings since the last call (attacker, outcome).
    pub fn take_swings(&mut self) -> Vec<(PawnId, Swing)> {
        std::mem::take(&mut self.last_swings)
    }
}

/// `CanReachImmediate(target, Touch)` for melee: adjacent, and a diagonal
/// only if a side cell is walkable.
fn melee_touch(grid: &crate::path::PathGrid, a: Cell, b: Cell) -> bool {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    if dx.abs() > 1 || dz.abs() > 1 || (dx == 0 && dz == 0) {
        return false;
    }
    if dx == 0 || dz == 0 {
        return true;
    }
    grid.walkable(Cell::new(a.x + dx, a.z)) || grid.walkable(Cell::new(a.x, a.z + dz))
}
