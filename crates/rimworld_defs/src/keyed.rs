//! Keyed UI strings (`Languages/<language>/Keyed/*.xml`) read from the
//! user's install at runtime, and the game's argument substitution
//! (`"Prioritize {0} {1}"`, `"Attack {1_labelShort}"`).

use std::collections::HashMap;
use std::path::Path;

/// A language's keyed strings.
#[derive(Debug, Clone, Default)]
pub struct KeyedStrings {
    map: HashMap<String, String>,
}

impl KeyedStrings {
    /// Reads every `*.xml` under `dir` (a `Keyed` folder). Unreadable files
    /// are skipped.
    pub fn load(dir: &Path) -> Self {
        let mut map = HashMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self { map };
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")))
            .collect();
        files.sort();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            let text = text.trim_start_matches('\u{feff}');
            let Ok(doc) = roxmltree::Document::parse(text) else {
                continue;
            };
            for n in doc.root_element().children().filter(|n| n.is_element()) {
                // Keyed strings write line breaks as `\n`.
                let v = n.text().unwrap_or("").replace("\\n", "\n");
                map.insert(n.tag_name().name().to_owned(), v);
            }
        }
        Self { map }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// `Translate`: the string with `{N}` / `{N_anything}` replaced by the
    /// N-th argument; the key itself when missing.
    pub fn tr(&self, key: &str, args: &[&str]) -> String {
        let Some(s) = self.get(key) else {
            return key.to_owned();
        };
        format_args_into(s, args)
    }
}

/// Replaces `{N}` and `{N_name}` with `args[N]`.
pub fn format_args_into(s: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let inner = &after[..close];
                let digits: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
                match digits.parse::<usize>().ok().and_then(|i| args.get(i)) {
                    Some(a) if !digits.is_empty() => out.push_str(a),
                    _ => {
                        out.push('{');
                        out.push_str(inner);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_substituted() {
        assert_eq!(
            format_args_into("Prioritize {0} {1}", &["mining", "granite"]),
            "Prioritize mining granite"
        );
        assert_eq!(
            format_args_into("Attack {1_labelShort}", &["x", "Hare"]),
            "Attack Hare"
        );
        assert_eq!(
            format_args_into("{1_label} is forbidden", &["", "steel"]),
            "steel is forbidden"
        );
        assert_eq!(format_args_into("odd {x} {", &[]), "odd {x} {");
    }
}
