//! Parsers for RimWorld's scalar value syntaxes.

/// Linear RGBA colour with components in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }
}

/// Parses `(r,g,b)` / `(r,g,b,a)`. Components above 1 mean the 0–255 scale
/// (RimWorld accepts both, e.g. `(105,95,97)` and `(0.65, 0.65, 0.35)`).
pub fn parse_color(text: &str) -> Option<Rgba> {
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    let parts: Vec<f32> = inner
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let scale = if parts.iter().any(|&v| v > 1.0) {
        255.0
    } else {
        1.0
    };
    Some(Rgba {
        r: parts[0] / scale,
        g: parts[1] / scale,
        b: parts[2] / scale,
        a: parts.get(3).map_or(1.0, |a| a / scale),
    })
}

/// Parses `true`/`false` case-insensitively.
pub fn parse_bool(text: &str) -> Option<bool> {
    match text.trim() {
        t if t.eq_ignore_ascii_case("true") => Some(true),
        t if t.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// Parses an `a~b` range (also accepts a single number as `a~a`).
pub fn parse_float_range(text: &str) -> Option<(f32, f32)> {
    let text = text.trim();
    match text.split_once('~') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let v = text.parse().ok()?;
            Some((v, v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_both_scales() {
        let c = parse_color("(105,95,97)").unwrap();
        assert!((c.r - 105.0 / 255.0).abs() < 1e-6 && c.a == 1.0);
        let c = parse_color(" (0.65, 0.65, 0.35) ").unwrap();
        assert_eq!(c, Rgba::rgb(0.65, 0.65, 0.35));
        let c = parse_color("(1, 1, 1, 0.43)").unwrap();
        assert!((c.a - 0.43).abs() < 1e-6);
        assert!(parse_color("(1,2)").is_none());
        assert!(parse_color("1,2,3").is_none());
        assert!(parse_color("(a,b,c)").is_none());
    }

    #[test]
    fn bools_and_ranges() {
        assert_eq!(parse_bool("True"), Some(true));
        assert_eq!(parse_bool("false"), Some(false));
        assert_eq!(parse_bool("yes"), None);
        assert_eq!(parse_float_range("350~600"), Some((350.0, 600.0)));
        assert_eq!(parse_float_range("2"), Some((2.0, 2.0)));
        assert_eq!(parse_float_range("x~1"), None);
    }
}
