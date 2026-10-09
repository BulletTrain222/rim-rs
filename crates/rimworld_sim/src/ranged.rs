//! Ranged attacks with firearms (docs/research.md §60): the shot report
//! (`ShotReport`), cover (`CoverUtility`), the shoot line with leaning
//! (`Verb.TryFindShootLineFromTo`, `ShootLeanUtility`, `GenSight`), the
//! shot decision (`Verb_LaunchProjectile.TryCastShot`), wild misses
//! (`ShootLine.ChangeDestToMissWild`) and projectile flight
//! (`Projectile.Launch`, `TickInterval`).
//!
//! Float steps follow the game's runtime: compound expressions are
//! evaluated wide and rounded to binary32 when stored.

use crate::grid::Cell;
use crate::rand::Rand;

/// `ShootTuning.MinAimOnChance_StandardTarget` and the shooter-factor floor.
const MIN_AIM: f32 = 0.0201;
/// Hit flag bits (`ProjectileHitFlags`).
pub const HIT_INTENDED: u8 = 1;
pub const HIT_NON_TARGET_PAWNS: u8 = 2;
pub const HIT_NON_TARGET_WORLD: u8 = 4;
/// `Altitudes.AltitudeFor` step and the layers bullets and pawns fly at.
const ALT_STEP: f32 = 0.365_853_67;
const ALT_INC: f32 = 0.036_585_37;
const LAYER_PROJECTILE: f32 = 22.0;
const LAYER_PAWN: f32 = 23.0;

/// `GenAdj.AdjacentCells`: N, E, S, W, SE, NE, NW, SW.
pub const ADJACENT: [Cell; 8] = [
    Cell { x: 0, z: 1 },
    Cell { x: 1, z: 0 },
    Cell { x: 0, z: -1 },
    Cell { x: -1, z: 0 },
    Cell { x: 1, z: -1 },
    Cell { x: 1, z: 1 },
    Cell { x: -1, z: 1 },
    Cell { x: -1, z: -1 },
];

/// A float position (`Vector3`; y is altitude).
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// `IntVec3.ToVector3Shifted`.
    pub fn shifted(c: Cell) -> Self {
        Self::new(c.x as f32 + 0.5, 0.0, c.z as f32 + 0.5)
    }

    /// `new IntVec3(Vector3)`: truncation toward zero.
    pub fn cell(self) -> Cell {
        Cell::new(self.x as i32, self.z as i32)
    }

    pub fn magnitude(self) -> f32 {
        let (x, y, z) = (self.x as f64, self.y as f64, self.z as f64);
        (x * x + y * y + z * z).sqrt() as f32
    }

    pub fn magnitude_horizontal_squared(self) -> f32 {
        let (x, z) = (self.x as f64, self.z as f64);
        (x * x + z * z) as f32
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Vec3;

    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

/// A pawn's draw position when standing still: its cell's centre at the
/// Pawn altitude layer plus a small offset seeded by its id
/// (`Pawn_DrawTracker.SeededYOffset`).
pub fn pawn_draw_pos(c: Cell, id_number: i32) -> Vec3 {
    let mut rng = Rand::new(0);
    rng.push_state_seeded(id_number);
    let offset = rng.range_f32(-ALT_INC, ALT_INC);
    let y = ((LAYER_PAWN * ALT_STEP) as f64 + 0.0 + offset as f64) as f32;
    Vec3::new(c.x as f32 + 0.5, y, c.z as f32 + 0.5)
}

/// The Projectile layer's altitude.
pub fn projectile_altitude() -> f32 {
    LAYER_PROJECTILE * ALT_STEP
}

/// `Mathf.Lerp` (clamped).
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    (a as f64 + (b as f64 - a as f64) * t.clamp(0.0, 1.0) as f64) as f32
}

/// The touch/short/medium/long interpolation by distance (3, 12, 25, 40).
fn by_distance(d: f32, v: [f32; 4]) -> f32 {
    if d <= 3.0 {
        v[0]
    } else if d <= 12.0 {
        lerp(v[0], v[1], ((d as f64 - 3.0) / 9.0) as f32)
    } else if d <= 25.0 {
        lerp(v[1], v[2], ((d as f64 - 12.0) / 13.0) as f32)
    } else if d <= 40.0 {
        lerp(v[2], v[3], ((d as f64 - 25.0) / 15.0) as f32)
    } else {
        v[3]
    }
}

/// `VerbProperties.GetHitChanceFactor`: the weapon's accuracy by distance,
/// within [0.01, 1].
pub fn weapon_hit_factor(accuracy: [f32; 4], d: f32) -> f32 {
    by_distance(d, accuracy).clamp(0.01, 1.0)
}

/// `ShotReport.HitFactorFromShooter` for a pawn: accuracy^distance × the
/// pawn's distance factors (1 here), at least 0.0201.
// COMPATIBILITY TODO: currently approximate — the pawn's
// ShootingAccuracyFactor_* stats are taken as 1.
pub fn shooter_hit_factor(accuracy: f32, d: f32) -> f32 {
    let p = (accuracy as f64).powf(d as f64) as f32;
    p.max(MIN_AIM)
}

/// `(cell − caster).LengthHorizontal`.
pub fn shot_distance(from: Cell, to: Cell) -> f32 {
    let (dx, dz) = ((to.x - from.x) as f32, (to.z - from.z) as f32);
    ((dx * dx + dz * dz) as f64).sqrt() as f32
}

/// What a shot sees on the map.
pub trait ShotMap {
    fn in_bounds(&self, c: Cell) -> bool;
    /// `CanBeSeenOverFast`: no full-fill edifice, unless an open door.
    fn can_see_over(&self, c: Cell) -> bool;
    /// `CoverGrid`: the cell's cover thing, (id, base block chance) —
    /// 0.75 for full fill, else its fill percent, 0 for an open door.
    fn cover_at(&self, c: Cell) -> Option<(u64, f32)>;
}

/// `GenSight.LineOfSight`: the integer walk from `start` to `end`; every
/// cell but the last (and the first, with `skip_first`) must be seen over.
pub fn line_of_sight(m: &dyn ShotMap, start: Cell, end: Cell, skip_first: bool) -> bool {
    if !m.in_bounds(start) || !m.in_bounds(end) {
        return false;
    }
    let toward_positive = if start.x != end.x {
        start.x < end.x
    } else {
        start.z < end.z
    };
    let mut dx = (end.x - start.x).abs();
    let mut dz = (end.z - start.z).abs();
    let (mut x, mut z) = (start.x, start.z);
    let mut steps = 1 + dx + dz;
    let sx = if end.x > start.x { 1 } else { -1 };
    let sz = if end.z > start.z { 1 } else { -1 };
    dx *= 4;
    dz *= 4;
    let mut err = dx / 2 - dz / 2;
    while steps > 1 {
        let c = Cell::new(x, z);
        if (!skip_first || c != start) && !m.can_see_over(c) {
            return false;
        }
        if err > 0 || (err == 0 && toward_positive) {
            x += sx;
            err -= dz;
        } else {
            z += sz;
            err += dx;
        }
        steps -= 1;
    }
    true
}

/// `IntVec3.AngleFlat` (`Quaternion.LookRotation(v).eulerAngles.y`):
/// degrees clockwise from north, in [0, 360).
// COMPATIBILITY TODO: currently approximate — computed with atan2 in
// double precision, not Unity's quaternion path (may differ in the last
// bits near the cover angle boundaries).
pub fn angle_flat(dx: f32, dz: f32) -> f32 {
    if dx == 0.0 && dz == 0.0 {
        return 0.0;
    }
    let a = (dx as f64).atan2(dz as f64).to_degrees();
    let a = if a < 0.0 { a + 360.0 } else { a };
    a as f32
}

/// `GenGeo.AngleDifferenceBetween`.
fn angle_difference(a: f32, b: f32) -> f32 {
    let d1 = (a - b).abs();
    let d2 = (a + 360.0 - b).abs();
    let d3 = (a - (b + 360.0)).abs();
    d1.min(d2).min(d3)
}

/// `ShootLeanUtility.LeanShootingSourcesFromTo`: corner cells a pawn can
/// lean out from (E, W, S, N), its own cell, then cover cells toward the
/// target.
pub fn lean_sources(m: &dyn ShotMap, at: Cell, toward: Cell) -> Vec<Cell> {
    let angle = angle_flat((toward.x - at.x) as f32, (toward.z - at.z) as f32);
    let north = angle > 270.0 || angle < 90.0;
    let south = angle > 90.0 && angle < 270.0;
    let west = angle > 180.0;
    let east = angle < 180.0;
    let blocked: Vec<bool> = ADJACENT.iter().map(|&d| !m.can_see_over(at + d)).collect();
    let mut out = Vec::new();
    if !blocked[1] && ((blocked[0] && !blocked[5] && north) || (blocked[2] && !blocked[4] && south))
    {
        out.push(at + ADJACENT[1]);
    }
    if !blocked[3] && ((blocked[0] && !blocked[6] && north) || (blocked[2] && !blocked[7] && south))
    {
        out.push(at + ADJACENT[3]);
    }
    if !blocked[2] && ((blocked[3] && !blocked[7] && west) || (blocked[1] && !blocked[4] && east)) {
        out.push(at + ADJACENT[2]);
    }
    if !blocked[0] && ((blocked[3] && !blocked[6] && west) || (blocked[1] && !blocked[5] && east)) {
        out.push(at + ADJACENT[0]);
    }
    if m.can_see_over(at) {
        out.push(at);
    }
    for (j, &dir_ok) in [north, east, south, west].iter().enumerate() {
        if blocked[j] || !dir_ok {
            continue;
        }
        if m.cover_at(at + ADJACENT[j]).is_some() {
            out.push(at + ADJACENT[j]);
        }
    }
    out
}

/// A shot's source and destination cells (`ShootLine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShootLine {
    pub source: Cell,
    pub dest: Cell,
}

/// `Verb.TryFindShootLineFromTo` for a pawn shooting a pawn (one-cell
/// target): range by squared distance (equality allowed), then the
/// pawn's own cell and its lean sources against the target's lean cells.
pub fn find_shoot_line(
    m: &dyn ShotMap,
    root: Cell,
    target: Cell,
    range: f32,
    min_range: f32,
) -> Option<ShootLine> {
    let d2 = {
        let (dx, dz) = (target.x - root.x, target.z - root.z);
        (dx * dx + dz * dz) as f32
    };
    if d2 > range * range || d2 < min_range * min_range {
        return None;
    }
    let try_from = |src: Cell| {
        lean_sources(m, target, src)
            .into_iter()
            .find(|&dst| line_of_sight(m, src, dst, true))
    };
    if let Some(dest) = try_from(root) {
        return Some(ShootLine { source: root, dest });
    }
    lean_sources(m, root, target)
        .into_iter()
        .find_map(|src| try_from(src).map(|dest| ShootLine { source: src, dest }))
}

/// One cover giver (`CoverInfo`): the thing and its adjusted block chance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverInfo {
    pub thing: u64,
    pub cell: Cell,
    pub block: f32,
}

/// `TryFindAdjustedCoverInCell`: the base block chance scaled by the
/// angle between the shooter and the cover (diagonal cover ×1.75 angle),
/// then by the shooter's closeness to the cover.
fn adjusted_cover(m: &dyn ShotMap, shooter: Cell, target: Cell, adj: Cell) -> Option<CoverInfo> {
    let (thing, base) = m.cover_at(adj)?;
    if shooter == target {
        return None;
    }
    let to_shooter = angle_flat((shooter.x - target.x) as f32, (shooter.z - target.z) as f32);
    let to_cover = angle_flat((adj.x - target.x) as f32, (adj.z - target.z) as f32);
    let mut angle = angle_difference(to_cover, to_shooter);
    let cardinal = (adj.x - target.x).abs() + (adj.z - target.z).abs() == 1;
    if !cardinal {
        angle *= 1.75;
    }
    let factor = if angle < 15.0 {
        1.0
    } else if angle < 27.0 {
        0.8
    } else if angle < 40.0 {
        0.6
    } else if angle < 52.0 {
        0.4
    } else if angle < 65.0 {
        0.2
    } else {
        return None;
    };
    let mut block = base * factor;
    let dist = shot_distance(shooter, adj);
    if dist < 1.9 {
        block *= 0.3333;
    } else if dist < 2.9 {
        block *= 0.66666;
    }
    Some(CoverInfo {
        thing,
        cell: adj,
        block,
    })
}

/// `CalculateCoverGiverSet` and `CalculateOverallBlockChance` (which
/// alone skips a cover cell the shooter stands on).
pub fn covers(m: &dyn ShotMap, shooter: Cell, target: Cell) -> (Vec<CoverInfo>, f32) {
    let mut set = Vec::new();
    let mut overall = 0.0f32;
    for d in ADJACENT {
        let adj = target + d;
        if !m.in_bounds(adj) {
            continue;
        }
        let Some(c) = adjusted_cover(m, shooter, target, adj) else {
            continue;
        };
        if c.block > 0.0 {
            set.push(c);
        }
        if adj != shooter {
            overall = ((overall as f64) + (1.0 - overall as f64) * c.block as f64) as f32;
        }
    }
    (set, overall)
}

/// The parts of `ShotReport` the shot decision uses.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotReport {
    pub distance: f32,
    pub shooter: f32,
    pub equipment: f32,
    pub weather: f32,
    pub gas: f32,
    pub execution: f32,
    pub target_size: f32,
    pub posture: f32,
    pub covers: Vec<CoverInfo>,
    pub overall_block: f32,
}

impl ShotReport {
    /// `AimOnTargetChance_StandardTarget` (darkness offset 0 without
    /// Ideology).
    pub fn standard(&self) -> f32 {
        let num = (self.shooter as f64
            * self.equipment as f64
            * self.weather as f64
            * self.gas as f64
            * self.execution as f64) as f32;
        num.max(MIN_AIM)
    }

    /// `AimOnTargetChance_IgnoringPosture`: the launch roll's chance.
    pub fn ignoring_posture(&self) -> f32 {
        self.standard() * self.target_size
    }

    /// `PassCoverChance`.
    pub fn pass_cover(&self) -> f32 {
        1.0 - self.overall_block
    }

    /// `TotalEstimatedHitChance` (the displayed estimate).
    pub fn total_estimate(&self) -> f32 {
        (self.ignoring_posture() * self.posture * self.pass_cover()).clamp(0.0, 1.0)
    }
}

/// What the target is, for the report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetFacts {
    pub cell: Cell,
    pub body_size: f32,
    pub standing: bool,
}

/// `ShotReport.HitReportFor` for a pawn target.
// COMPATIBILITY TODO: currently approximate — weather accuracy and blind
// smoke are not modelled (factors 1).
pub fn hit_report(
    m: &dyn ShotMap,
    shooter: Cell,
    shooter_accuracy: f32,
    weapon_accuracy: [f32; 4],
    can_go_wild: bool,
    target: TargetFacts,
) -> ShotReport {
    let d = shot_distance(shooter, target.cell);
    let (covers, overall) = covers(m, shooter, target.cell);
    ShotReport {
        distance: d,
        shooter: if can_go_wild {
            shooter_hit_factor(shooter_accuracy, d)
        } else {
            1.0
        },
        equipment: weapon_hit_factor(weapon_accuracy, d),
        weather: 1.0,
        gas: 1.0,
        execution: if !target.standing && d <= 3.9 {
            7.5
        } else {
            1.0
        },
        target_size: target.body_size.clamp(0.5, 2.0),
        posture: if !target.standing && d >= 4.5 {
            0.5
        } else {
            1.0
        },
        covers,
        overall_block: overall,
    }
}

/// `Rand.Gaussian`: Box–Muller with two values.
pub fn gaussian(rng: &mut Rand) -> f32 {
    let v1 = rng.value();
    let v2 = rng.value();
    let l = (v1 as f64).ln() as f32;
    let a = ((-2.0f32 * l) as f64).sqrt() as f32;
    let s = ((std::f32::consts::PI * 2.0 * v2) as f64).sin() as f32;
    (a as f64 * s as f64) as f32
}

/// `Rand.UnitVector2`: two Gaussians, normalized.
pub fn unit_vector2(rng: &mut Rand) -> (f32, f32) {
    let x = gaussian(rng);
    let y = gaussian(rng);
    let m = ((x as f64 * x as f64 + y as f64 * y as f64).sqrt()) as f32;
    if m > 1e-5 { (x / m, y / m) } else { (0.0, 0.0) }
}

/// `ShootTuning.MissDistanceFromAimOnChanceCurves.Evaluate(aim, u)`.
pub fn miss_radius(aim: f32, u: f32) -> f32 {
    const COLS: [(f32, f32); 6] = [
        (0.02, 10.0),
        (0.04, 8.0),
        (0.07, 6.0),
        (0.11, 4.0),
        (0.22, 2.0),
        (1.0, 1.0),
    ];
    // Each column's curve goes from 1 at u = 0 to its top at u = 1.
    let col = |top: f32| {
        let t = u.clamp(0.0, 1.0);
        (1.0f64 + (top as f64 - 1.0) * t as f64) as f32
    };
    if aim <= COLS[0].0 {
        return col(COLS[0].1);
    }
    if aim >= COLS[5].0 {
        return col(COLS[5].1);
    }
    let i = COLS.iter().position(|&(x, _)| aim <= x).unwrap_or(5);
    let (a, b) = (COLS[i - 1], COLS[i]);
    let t = ((aim as f64 - a.0 as f64) / (b.0 as f64 - a.0 as f64)) as f32;
    lerp(col(a.1), col(b.1), t)
}

/// `ShootLine.ChangeDestToMissWild` (one radius draw, four per direction
/// attempt, retried when the miss would land behind the shooter).
// COMPATIBILITY TODO: currently approximate — the truncation of misses
// the shooter can't see is not applied (no lean visibility check).
pub fn miss_wild(line: ShootLine, aim_standard: f32, rng: &mut Rand) -> Cell {
    let radius = miss_radius(aim_standard, rng.value());
    loop {
        let (ux, uy) = unit_vector2(rng);
        let base = Vec3::shifted(line.dest);
        let v = Vec3::new(
            (base.x as f64 + (ux as f64 * radius as f64) as f32 as f64) as f32,
            0.0,
            (base.z as f64 + (uy as f64 * radius as f64) as f32 as f64) as f32,
        );
        let c = v.cell();
        let (ax, az) = (
            (line.dest.x - line.source.x) as f32,
            (line.dest.z - line.source.z) as f32,
        );
        let (bx, bz) = ((c.x - line.source.x) as f32, (c.z - line.source.z) as f32);
        if ax * bx + az * bz >= 0.0 {
            return c;
        }
    }
}

/// How the shot went (`TryCastShot`'s branches).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShotAim {
    /// Aimed at the intended target.
    Hit,
    /// Went wild toward a cell.
    Wild(Cell),
    /// Struck the chosen cover thing.
    Cover(CoverInfo),
}

/// The shot decision after the line was found: weighted cover choice
/// (a draw only for several candidates), the aim roll on the
/// ignoring-posture chance, then (not wild) the pass-cover roll; with the
/// hit flags.
pub fn decide_shot(
    report: &ShotReport,
    line: ShootLine,
    can_go_wild: bool,
    can_hit_non_target_pawns: bool,
    rng: &mut Rand,
) -> (ShotAim, u8) {
    let weights: Vec<f32> = report.covers.iter().map(|c| c.block).collect();
    let cover =
        crate::recreation::try_random_element_by_weight(&weights, rng).map(|i| report.covers[i]);
    if can_go_wild && !rng.chance(report.ignoring_posture()) {
        let dest = miss_wild(line, report.standard(), rng);
        let mut flags = HIT_NON_TARGET_WORLD;
        if rng.chance(0.5) && can_hit_non_target_pawns {
            flags |= HIT_NON_TARGET_PAWNS;
        }
        return (ShotAim::Wild(dest), flags);
    }
    if let Some(c) = cover
        && !rng.chance(report.pass_cover())
    {
        let mut flags = HIT_NON_TARGET_WORLD;
        if can_hit_non_target_pawns {
            flags |= HIT_NON_TARGET_PAWNS;
        }
        return (ShotAim::Cover(c), flags);
    }
    let mut flags = HIT_INTENDED;
    if can_hit_non_target_pawns {
        flags |= HIT_NON_TARGET_PAWNS;
    }
    (ShotAim::Hit, flags)
}

/// `Projectile.Launch`'s destination: the used cell's centre plus a square
/// ±0.3 perturbation (x, then z).
pub fn launch_destination(used: Cell, rng: &mut Rand) -> Vec3 {
    let base = Vec3::shifted(used);
    let rx = rng.range_f32(-0.3, 0.3);
    let rz = rng.range_f32(-0.3, 0.3);
    Vec3::new(base.x + rx, 0.0, base.z + rz)
}

/// `StartingTicksToImpact`: full 3D distance / (speed / 100).
pub fn starting_ticks(origin: Vec3, destination: Vec3, speed: f32) -> f32 {
    let per_tick = speed / 100.0;
    let f = ((origin - destination).magnitude() as f64 / per_tick as f64) as f32;
    if f <= 0.0 { 0.001 } else { f }
}

/// `VerbUtility.InterceptChanceFactorFromDistance`: 0 within 5 cells, 1
/// beyond 12, linear in squared distance between.
pub fn intercept_distance_factor(origin: Vec3, c: Cell) -> f32 {
    let d = (Vec3::shifted(c) - origin).magnitude_horizontal_squared();
    if d <= 25.0 {
        0.0
    } else if d >= 144.0 {
        1.0
    } else {
        ((d as f64 - 25.0) / 119.0) as f32
    }
}

/// Ticks for `seconds` (`GenTicks.SecondsToTicks`: round half to even).
pub fn seconds_to_ticks(seconds: f32) -> i32 {
    ((60.0f64 * seconds as f64) as f32).round_ties_even() as i32
}

/// A pawn's equipped primary weapon (`Pawn_EquipmentTracker.Primary`)
/// with its verb's state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Equipped {
    pub def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
    pub verb: VerbState,
}

/// `Verb` state saved with the weapon.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VerbState {
    /// `state == Bursting`.
    pub bursting: bool,
    pub burst_shots_left: i32,
    pub ticks_to_next_burst_shot: i32,
    pub target: Option<crate::pawn::PawnId>,
    pub last_shot_tick: u64,
    /// `canHitNonTargetPawnsNow` (Hunt casts with false).
    #[serde(default = "yes")]
    pub can_hit_non_target_pawns: bool,
}

fn yes() -> bool {
    true
}

impl Default for VerbState {
    fn default() -> Self {
        Self {
            bursting: false,
            burst_shots_left: 0,
            ticks_to_next_burst_shot: 0,
            target: None,
            last_shot_tick: 0,
            can_hit_non_target_pawns: true,
        }
    }
}

/// A busy stance (`Stance_Warmup` / `Stance_Cooldown`); none = Mobile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StanceKind {
    Warmup,
    Cooldown,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stance {
    pub kind: StanceKind,
    pub ticks_left: i32,
    pub target: Option<crate::pawn::PawnId>,
    /// `targetStartedDowned` (warmup).
    pub target_started_downed: bool,
}

/// What a projectile flies at (`usedTarget`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UsedTarget {
    Pawn(crate::pawn::PawnId),
    Cell(Cell),
    /// A cover thing (by cover id) on a cell.
    Cover(u64, Cell),
}

/// A flying projectile (`Projectile`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Projectile {
    pub id_number: i32,
    pub def: rimworld_defs::DefId<rimworld_defs::ThingDef>,
    pub origin: Vec3,
    pub destination: Vec3,
    pub ticks_to_impact: i32,
    pub lifetime: i32,
    pub used: UsedTarget,
    pub intended: Option<crate::pawn::PawnId>,
    pub flags: u8,
    pub launcher: crate::pawn::PawnId,
    pub equipment_def: Option<rimworld_defs::DefId<rimworld_defs::ThingDef>>,
    pub prevent_friendly_fire: bool,
    /// The integer position (`Position`).
    pub position: Cell,
    /// `Thing.tickDelta` for the interval schedule.
    pub tick_delta: u32,
    /// Spawned this tick: starts ticking with the next one.
    #[serde(default)]
    pub spawned_tick: u64,
}

impl Projectile {
    /// `StartingTicksToImpact`.
    pub fn starting_ticks(&self, speed: f32) -> f32 {
        starting_ticks(self.origin, self.destination, speed)
    }

    /// `ExactPosition`: flattened origin plus the covered fraction of the
    /// flattened path, at the projectile altitude.
    pub fn exact_position(&self, speed: f32) -> Vec3 {
        let start = self.starting_ticks(speed);
        let frac = ((1.0f64 - self.ticks_to_impact as f64 / start as f64) as f32).clamp(0.0, 1.0);
        let dx = ((self.destination.x - self.origin.x) as f64 * frac as f64) as f32;
        let dz = ((self.destination.z - self.origin.z) as f64 * frac as f64) as f32;
        Vec3::new(
            self.origin.x + dx,
            projectile_altitude(),
            self.origin.z + dz,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Open;
    impl ShotMap for Open {
        fn in_bounds(&self, c: Cell) -> bool {
            (0..250).contains(&c.x) && (0..250).contains(&c.z)
        }
        fn can_see_over(&self, c: Cell) -> bool {
            self.in_bounds(c)
        }
        fn cover_at(&self, _: Cell) -> Option<(u64, f32)> {
            None
        }
    }

    /// A barricade (fill 0.55) west of the target, and optionally a wall.
    struct Barricade(Option<Cell>);
    impl ShotMap for Barricade {
        fn in_bounds(&self, c: Cell) -> bool {
            (0..250).contains(&c.x) && (0..250).contains(&c.z)
        }
        fn can_see_over(&self, c: Cell) -> bool {
            self.in_bounds(c) && Some(c) != self.0
        }
        fn cover_at(&self, c: Cell) -> Option<(u64, f32)> {
            (c == Cell::new(104, 100)).then_some((1, 0.55))
        }
    }

    const REVOLVER: [f32; 4] = [0.80, 0.75, 0.55, 0.40];
    const SHOOTING_10: f32 = 0.97;

    fn bits(v: f32) -> u32 {
        v.to_bits()
    }

    /// Fixture E: the weapon and shooter factors just below, at and just
    /// above each distance boundary.
    #[test]
    fn distance_factors_match_recorded_bits() {
        let rows: [(u32, u32, u32); 12] = [
            (0x403F_EF9E, 0x3F4C_CCCD, 0x3F69_A6C4),
            (0x4040_0000, 0x3F4C_CCCD, 0x3F69_A4F1),
            (0x4040_1062, 0x3F4C_CC70, 0x3F69_A31F),
            (0x413F_FBE7, 0x3F40_005D, 0x3F31_A10E),
            (0x4140_0000, 0x3F40_0000, 0x3F31_9FAB),
            (0x4140_0419, 0x3F3F_FEFE, 0x3F31_9E49),
            (0x41C7_FDF4, 0x3F0C_CDCF, 0x3EEF_1938),
            (0x41C8_0000, 0x3F0C_CCCD, 0x3EEF_175B),
            (0x41C8_020C, 0x3F0C_CC25, 0x3EEF_157E),
            (0x421F_FEFA, 0x3ECC_CE1C, 0x3E97_68D4),
            (0x4220_0000, 0x3ECC_CCCD, 0x3E97_67A6),
            (0x4220_0106, 0x3ECC_CCCD, 0x3E97_6677),
        ];
        for (d, w, s) in rows {
            let d = f32::from_bits(d);
            assert_eq!(bits(weapon_hit_factor(REVOLVER, d)), w, "weapon at {d}");
            assert_eq!(
                bits(shooter_hit_factor(SHOOTING_10, d)),
                s,
                "shooter at {d}"
            );
        }
    }

    fn report(m: &dyn ShotMap) -> ShotReport {
        hit_report(
            m,
            Cell::new(85, 100),
            SHOOTING_10,
            REVOLVER,
            true,
            TargetFacts {
                cell: Cell::new(105, 100),
                body_size: 1.0,
                standing: true,
            },
        )
    }

    /// Fixture C: at 20 cells, no cover, seed 3 — the chance, the hit, the
    /// destination bits, the flight time and the draws.
    #[test]
    fn seeded_hit_matches_recorded_bits() {
        let r = report(&Open);
        assert_eq!(bits(r.equipment), 0x3F20_7E08);
        assert_eq!(bits(r.ignoring_posture()), 0x3EAE_8CBB);
        let line =
            find_shoot_line(&Open, Cell::new(85, 100), Cell::new(105, 100), 25.9, 1.421).unwrap();
        let mut rng = Rand::new(0);
        rng.push_state_seeded(3);
        let (aim, flags) = decide_shot(&r, line, true, true, &mut rng);
        assert_eq!((aim, flags), (ShotAim::Hit, 3));
        assert_eq!(rng.state().1, 1);
        let dest = launch_destination(Cell::new(105, 100), &mut rng);
        assert_eq!(rng.state().1, 3);
        assert_eq!((bits(dest.x), bits(dest.z)), (0x42D3_6B3A, 0x42C8_EEFA));
        let origin = pawn_draw_pos(Cell::new(85, 100), 648);
        assert_eq!(bits(origin.y), 0x4106_965F);
        let f = starting_ticks(origin, dest, 55.0);
        assert_eq!(bits(f), 0x421F_337D);
        assert_eq!(f.ceil() as i32, 40);
        assert_eq!(bits(projectile_altitude()), 0x4100_C7CE);
    }

    /// Fixture D: seed 1 goes wild: one radius draw, one direction attempt,
    /// the 0.5 pawn roll (fails), then the destination.
    #[test]
    fn seeded_wild_miss_matches_recorded_bits() {
        let r = report(&Open);
        let line =
            find_shoot_line(&Open, Cell::new(85, 100), Cell::new(105, 100), 25.9, 1.421).unwrap();
        let mut rng = Rand::new(0);
        rng.push_state_seeded(1);
        let (aim, flags) = decide_shot(&r, line, true, true, &mut rng);
        assert_eq!((aim, flags), (ShotAim::Wild(Cell::new(106, 100)), 4));
        assert_eq!(rng.state().1, 7);
        let dest = launch_destination(Cell::new(106, 100), &mut rng);
        assert_eq!(rng.state().1, 9);
        assert_eq!((bits(dest.x), bits(dest.z)), (0x42D4_9EC8, 0x42C8_FA8D));
        let f = starting_ticks(pawn_draw_pos(Cell::new(85, 100), 648), dest, 55.0);
        assert_eq!(bits(f), 0x4223_3E2B);
        assert_eq!(f.ceil() as i32, 41);
    }

    /// Fixture F: a barricade west of the target blocks 0.55; seed 3
    /// passes the aim roll, fails the pass-cover roll and strikes it.
    #[test]
    fn seeded_cover_miss_matches_recorded_result() {
        let m = Barricade(None);
        let r = report(&m);
        assert_eq!(bits(r.pass_cover()), 0x3EE6_6666);
        assert_eq!(bits(r.total_estimate()), 0x3E1D_1842);
        let line =
            find_shoot_line(&m, Cell::new(85, 100), Cell::new(105, 100), 25.9, 1.421).unwrap();
        let mut rng = Rand::new(0);
        rng.push_state_seeded(3);
        let (aim, flags) = decide_shot(&r, line, true, true, &mut rng);
        assert!(matches!(aim, ShotAim::Cover(c) if c.cell == Cell::new(104, 100)));
        assert_eq!(flags, 6);
        assert_eq!(rng.state().1, 2);
    }

    /// Fixture E/G: the revolver reaches 25 cells, not 26; a wall in the
    /// lane blocks the line.
    #[test]
    fn range_and_walls_limit_the_shot() {
        let s = Cell::new(85, 100);
        assert!(find_shoot_line(&Open, s, Cell::new(110, 100), 25.9, 1.421).is_some());
        assert!(find_shoot_line(&Open, s, Cell::new(111, 100), 25.9, 1.421).is_none());
        assert!(find_shoot_line(&Open, s, Cell::new(86, 100), 25.9, 1.421).is_none());
        let walled = Barricade(Some(Cell::new(95, 100)));
        assert!(find_shoot_line(&walled, s, Cell::new(105, 100), 25.9, 1.421).is_none());
    }

    /// Warmup and cooldown: 0.3 s → 18, 1 s → 60, 1.6 s → 96, 1.7 s → 102.
    #[test]
    fn seconds_round_to_recorded_ticks() {
        assert_eq!(seconds_to_ticks(0.3), 18);
        assert_eq!(seconds_to_ticks(1.0), 60);
        assert_eq!(seconds_to_ticks(1.6), 96);
        assert_eq!(seconds_to_ticks(1.7), 102);
    }

    /// Fixture H: the interception distance factor at 1, 5, 6, 12 cells.
    #[test]
    fn intercept_factor_matches_recorded_bits() {
        let origin = pawn_draw_pos(Cell::new(85, 100), 648);
        let f = |dx| intercept_distance_factor(origin, Cell::new(85 + dx, 100));
        assert_eq!((f(1), f(5)), (0.0, 0.0));
        assert_eq!(bits(f(6)), 0x3DBD_4F99);
        assert_eq!(f(12), 1.0);
    }
}
