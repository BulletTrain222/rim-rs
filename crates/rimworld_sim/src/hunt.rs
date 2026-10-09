//! Hunting and animal reactions (docs/research.md §61): the cast position
//! search (`CastPositionFinder`), animal flee destinations
//! (`CellFinderLoose.GetFleeDestAnimal`) and the manhunter-on-damage chance
//! (`PawnUtility.GetManhunterOnDamageChance`).
//!
//! Float steps follow the game's runtime: compound expressions are
//! evaluated wide and rounded to binary32 when stored.

use crate::grid::{Cell, GridSize};
use crate::rand::Rand;

/// A cast position request (`CastPositionRequest` as Hunt makes it: no
/// caster or locus range, no regions limit, no preferred cell, no cover).
#[derive(Debug, Clone, Copy)]
pub struct CastRequest {
    pub caster: Cell,
    pub target: Cell,
    pub max_range_from_target: f32,
    /// `verb.EffectiveRange`.
    pub effective_range: f32,
    /// `verb.verbProps.range`.
    pub verb_range: f32,
}

/// What the search asks about cells.
pub struct CastEnv<'a> {
    pub size: GridSize,
    /// Walkable by the caster.
    pub walkable: &'a dyn Fn(Cell) -> bool,
    /// Reachable from the caster (OnCell, Danger.Some).
    pub reachable: &'a dyn Fn(Cell) -> bool,
    /// `verb.CanHitTargetFrom(cell, target)`.
    pub can_hit: &'a dyn Fn(Cell) -> bool,
    /// The caster could reserve the cell as its destination.
    pub reservable: &'a dyn Fn(Cell) -> bool,
    /// A PassThroughOnly thing stands on the cell.
    pub pass_through: &'a dyn Fn(Cell) -> bool,
}

fn len_sq(a: Cell, b: Cell) -> i32 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    dx * dx + dz * dz
}

fn len(a: Cell, b: Cell) -> f32 {
    (len_sq(a, b) as f64).sqrt() as f32
}

/// `CastPositionFinder.TryFindCastPosition`: the caster's own cell first,
/// then the cells on the caster's side of the line through the target
/// perpendicular to caster–target (early return above 0.33), then the far
/// side; the best preference wins (later equal ones replace it).
pub fn find_cast_position(req: CastRequest, env: &CastEnv<'_>) -> Option<Cell> {
    let r = (req.max_range_from_target).ceil() as i32;
    let (min_x, max_x) = (
        (req.target.x - r).max(0),
        (req.target.x + r).min(env.size.width - 1),
    );
    let (min_z, max_z) = (
        (req.target.z - r).max(0),
        (req.target.z + r).min(env.size.height - 1),
    );
    let max_sq = (req.max_range_from_target as f64 * req.max_range_from_target as f64) as f32;
    let range_from_target = len(req.caster, req.target);
    let optimal_sq = (req.effective_range as f64
        * 0.8f32 as f64
        * (req.verb_range as f64 * 0.8f32 as f64) as f32 as f64) as f32;
    let mut best: Option<Cell> = None;
    let mut best_pref = 0.001f32;
    let evaluate = |c: Cell, best: &mut Option<Cell>, best_pref: &mut f32| {
        if max_sq > 0.01 && max_sq < 250_000.0 && len_sq(c, req.target) as f32 > max_sq {
            return;
        }
        if !(env.walkable)(c) || !(env.reachable)(c) {
            return;
        }
        // `CastPositionPreference`.
        let mut num = 0.3f32;
        let mut travel = len(req.caster, c);
        if range_from_target > 100.0 {
            travel -= range_from_target - 100.0;
            if travel < 0.0 {
                travel = 0.0;
            }
        }
        num = (num as f64 * (0.967f32 as f64).powf(travel as f64) as f32 as f64) as f32;
        let d2 = len_sq(c, req.target) as f32;
        let mut f = ((d2 as f64 - optimal_sq as f64).abs() / optimal_sq as f64) as f32;
        f = 1.0 - f;
        f = (0.7f64 + 0.3 * f as f64) as f32;
        let mut factor = 1.0f32 * f;
        if d2 < 25.0 {
            factor *= 0.5;
        }
        num *= factor;
        // COMPATIBILITY TODO: currently approximate — the game's static
        // rangeFromCasterToCellSquared can hold a value from an earlier
        // capped request (×0.4); Hunt's own requests never set it, and no
        // capped request exists here, so it stays 0.
        if (env.pass_through)(c) {
            num *= 0.4;
        }
        if num < *best_pref {
            return;
        }
        if (env.can_hit)(c) && (env.reservable)(c) {
            *best = Some(c);
            *best_pref = num;
        }
    };
    evaluate(req.caster, &mut best, &mut best_pref);
    if best_pref >= 1.0 {
        return Some(req.caster);
    }
    // `CellLine` through the target, perpendicular to target→caster.
    let slope_between = if req.target.x != req.caster.x {
        (req.caster.z - req.target.z) as f32 / (req.caster.x - req.target.x) as f32
    } else {
        100_000_000.0
    };
    let slope = -1.0f32 / slope_between;
    let z_intercept = req.target.z as f32 - req.target.x as f32 * slope;
    let above = |c: Cell| (c.z as f64) > slope as f64 * c.x as f64 + z_intercept as f64;
    let side = above(req.caster);
    let cells = || (min_z..=max_z).flat_map(move |z| (min_x..=max_x).map(move |x| Cell::new(x, z)));
    for c in cells().filter(|&c| above(c) == side) {
        evaluate(c, &mut best, &mut best_pref);
    }
    if best.is_some() && best_pref > 0.33 {
        return best;
    }
    for c in cells().filter(|&c| above(c) != side) {
        evaluate(c, &mut best, &mut best_pref);
    }
    best
}

/// `Vector3.RotatedBy(angle)` about the vertical axis (degrees).
fn rotated(x: f32, z: f32, angle: f32) -> (f32, f32) {
    let (s, c) = (angle as f64).to_radians().sin_cos();
    (
        (x as f64 * c + z as f64 * s) as f32,
        (-(x as f64) * s + z as f64 * c) as f32,
    )
}

/// `CellFinderLoose.GetFleeDestAnimal`: up to 17 random turns of the
/// away-from-threat direction (spreads 200° to 360°) at the displacement
/// `distance − current distance`, then random offsets of shrinking length;
/// else the pawn's own cell.
// COMPATIBILITY TODO: currently approximate — Unity's quaternion and
// trigonometry are computed in double precision.
pub fn flee_dest_animal(
    pawn: Cell,
    threat: Cell,
    distance: f32,
    can_flee: &dyn Fn(Cell) -> bool,
    rng: &mut Rand,
) -> Cell {
    let (dx, dz) = ((pawn.x - threat.x) as f32, (pawn.z - threat.z) as f32);
    let mag = ((dx as f64 * dx as f64 + dz as f64 * dz as f64).sqrt()) as f32;
    let (nx, nz) = if mag > 1e-5 {
        (dx / mag, dz / mag)
    } else {
        (0.0, 0.0)
    };
    let num = distance - len(pawn, threat);
    let mut spread = 200.0f32;
    while spread <= 360.0 {
        let angle = rng.range_f32(-spread / 2.0, spread / 2.0);
        let (rx, rz) = rotated(nx, nz, angle);
        let c = pawn + Cell::new((rx * num) as i32, (rz * num) as i32);
        if can_flee(c) {
            return c;
        }
        spread += 10.0;
    }
    let mut length = num;
    while length * 3.0 > num {
        let r = rng.range_f32(0.0, length);
        let y = rng.range(0, 360) as f32;
        let (sx, sz) = rotated(0.0, 1.0, y);
        let c = pawn + Cell::new((sx * r) as i32, (sz * r) as i32);
        if can_flee(c) {
            return c;
        }
        length -= distance / 10.0;
    }
    pawn
}

/// `GetManhunterOnDamageChance` with an instigator and the default
/// distance −1 (the damage path passes none: the distance factor is 3),
/// × (1 − the instigator's HuntingStealth), clamped to [0, 1].
pub fn manhunter_on_damage_chance(race_chance: f32, difficulty_factor: f32, stealth: f32) -> f32 {
    let mut num = race_chance * difficulty_factor;
    // `GenMath.LerpDoubleClamped(1, 30, 3, 1, -1)`.
    num *= 3.0;
    num *= 1.0 - stealth;
    num.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture AH18: a Hare at (108,100) hit by clamor from a hunter at
    /// (85,100), seed 3: the 0.4 roll passes, the first turn lands on
    /// (117,74).
    #[test]
    fn flee_destination_matches_recorded_cell() {
        let mut rng = Rand::new(0);
        rng.push_state_seeded(3);
        assert!(rng.chance(0.4));
        let pawn = Cell::new(108, 100);
        let threat = Cell::new(85, 100);
        let dist = len(pawn, threat) + 28.0;
        let dest = flee_dest_animal(pawn, threat, dist, &|_| true, &mut rng);
        assert_eq!(dest, Cell::new(117, 74));
        assert_eq!(rng.state().1, 2);
    }

    /// Fixture AH19: Muffalo 0.1 → 0.3 with no stealth; 0.0300000086 at
    /// stealth 0.9.
    #[test]
    fn manhunter_chance_matches_recorded_bits() {
        assert_eq!(
            manhunter_on_damage_chance(0.1, 1.0, 0.0).to_bits(),
            0x3E99_999A
        );
        assert_eq!(
            manhunter_on_damage_chance(0.1, 1.0, 0.9).to_bits(),
            0x3CF5_C294
        );
        assert_eq!(manhunter_on_damage_chance(0.0, 1.0, 0.0), 0.0);
    }

    /// Fixture H/AH16: from 60 cells away on an open map, the revolver's
    /// hunting position is (122,100).
    #[test]
    fn open_field_cast_position_matches_recorded_cell() {
        let caster = Cell::new(85, 100);
        let target = Cell::new(145, 100);
        let range = 25.9f32;
        let can_hit = |c: Cell| {
            let d2 = len_sq(c, target) as f32;
            d2 <= range * range
        };
        let env = CastEnv {
            size: GridSize::new(250, 250),
            walkable: &|_| true,
            reachable: &|_| true,
            can_hit: &can_hit,
            reservable: &|_| true,
            pass_through: &|_| false,
        };
        let req = CastRequest {
            caster,
            target,
            max_range_from_target: (range * 0.95).max(1.42),
            effective_range: range,
            verb_range: range,
        };
        assert_eq!(find_cast_position(req, &env), Some(Cell::new(122, 100)));
    }
}
