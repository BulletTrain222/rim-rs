//! Loading Def XML files from content packs into a [`DefDatabase`].
//!
//! Pipeline: read every `*.xml` under each pack's `Defs/` → parse (dropping
//! `MayRequire` nodes for inactive packs) → collect top-level Defs → resolve
//! inheritance across all packs → drop abstract Defs → index by `defName`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::database::{Def, DefDatabase};
use crate::inherit::{RawDef, resolve_all};
use crate::xml::{ActivePackages, parse_document};

/// A pack's `Defs` directory and its packageId (for `MayRequire` and parent
/// lookup preference).
#[derive(Debug, Clone)]
pub struct PackSource {
    pub package_id: String,
    pub defs_dir: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub files_loaded: usize,
    pub files_failed: usize,
    /// Top-level Def elements, including abstract ones.
    pub raw_defs: usize,
    pub abstract_defs: usize,
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("Defs directory {0} does not exist")]
    MissingDefsDir(PathBuf),
    #[error("could not list {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Loads all packs in order. Every pack's packageId is considered active.
pub fn load_packs(packs: &[PackSource]) -> Result<(DefDatabase, LoadReport), LoadError> {
    let start = Instant::now();
    let active = ActivePackages::new(packs.iter().map(|p| p.package_id.as_str()));
    let mut report = LoadReport::default();
    let mut raw = Vec::new();

    for pack in packs {
        if !pack.defs_dir.is_dir() {
            return Err(LoadError::MissingDefsDir(pack.defs_dir.clone()));
        }
        let mut files = Vec::new();
        collect_xml_files(&pack.defs_dir, &mut files)?;
        files.sort();
        for path in files {
            let rel = path
                .strip_prefix(&pack.defs_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            match std::fs::read_to_string(&path) {
                Ok(text) => add_document(
                    &text,
                    &rel,
                    &pack.package_id,
                    &active,
                    &mut raw,
                    &mut report,
                ),
                Err(e) => {
                    report.files_failed += 1;
                    report.warnings.push(format!("{rel}: {e}"));
                }
            }
        }
    }

    let db = build_database(raw, &mut report);
    report.elapsed = start.elapsed();
    Ok((db, report))
}

/// Loads Defs from in-memory XML documents `(file name, contents)`. Used by
/// tests and tools; behaves exactly like [`load_packs`] for a single pack.
pub fn load_documents(
    package_id: &str,
    docs: &[(&str, &str)],
    active: &ActivePackages,
) -> (DefDatabase, LoadReport) {
    let start = Instant::now();
    let mut report = LoadReport::default();
    let mut raw = Vec::new();
    for (name, text) in docs {
        add_document(text, name, package_id, active, &mut raw, &mut report);
    }
    let db = build_database(raw, &mut report);
    report.elapsed = start.elapsed();
    (db, report)
}

fn collect_xml_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    let entries = std::fs::read_dir(dir).map_err(|source| LoadError::Io {
        path: dir.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| LoadError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_xml_files(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn add_document(
    text: &str,
    file: &str,
    package_id: &str,
    active: &ActivePackages,
    raw: &mut Vec<RawDef>,
    report: &mut LoadReport,
) {
    let root = match parse_document(text, active) {
        Ok(root) => root,
        Err(e) => {
            report.files_failed += 1;
            report.warnings.push(format!("{file}: XML error: {e}"));
            return;
        }
    };
    if root.name != "Defs" {
        report.files_failed += 1;
        report.warnings.push(format!(
            "{file}: root element is <{}>, expected <Defs>",
            root.name
        ));
        return;
    }
    report.files_loaded += 1;
    for node in root.children {
        raw.push(RawDef {
            node,
            package_id: package_id.to_owned(),
            file: file.to_owned(),
        });
    }
}

fn build_database(raw: Vec<RawDef>, report: &mut LoadReport) -> DefDatabase {
    report.raw_defs += raw.len();
    let (resolved, warnings) = resolve_all(&raw);
    report.warnings.extend(warnings);

    let mut db = DefDatabase::default();
    for (rawdef, node) in raw.into_iter().zip(resolved) {
        if rawdef.is_abstract() {
            report.abstract_defs += 1;
            continue;
        }
        let Some(def_name) = node
            .child_text("defName")
            .map(str::to_owned)
            .or_else(|| implicit_def_name(&node))
        else {
            report.warnings.push(format!(
                "{}: <{}> without defName skipped",
                rawdef.file, node.name
            ));
            continue;
        };
        let def = Def {
            def_type: node.name.clone(),
            def_name,
            node,
            package_id: rawdef.package_id,
            file: rawdef.file,
        };
        if let Some(old) = db.insert(def) {
            report.warnings.push(format!(
                "duplicate {} {} ({} replaced by a later definition)",
                old.def_type, old.def_name, old.file
            ));
        }
    }
    db
}

/// Some Def types omit `defName` in XML and get one derived at load time.
/// SongDef: we use the file name of `clipPath` (observed: base-game SongDefs
/// have only `clipPath`; the exact derivation in the game is unverified).
// COMPATIBILITY TODO: currently approximate — SongDef defName derivation not verified.
fn implicit_def_name(node: &crate::xml::XmlNode) -> Option<String> {
    match node.name.as_str() {
        "SongDef" => node
            .child_text("clipPath")
            .and_then(|p| p.rsplit('/').next())
            .map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_across_documents_and_skips_abstract() {
        let a = r#"<Defs>
            <ThingDef Name="BasePawn" Abstract="True"><category>Pawn</category></ThingDef>
            <TerrainDef><defName>Soil</defName><pathCost>2</pathCost></TerrainDef>
        </Defs>"#;
        let b =
            r#"<Defs><ThingDef ParentName="BasePawn"><defName>Human</defName></ThingDef></Defs>"#;
        let (db, report) = load_documents(
            "core",
            &[("a.xml", a), ("b.xml", b)],
            &ActivePackages::new(["core"]),
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.files_loaded, 2);
        assert_eq!(report.raw_defs, 3);
        assert_eq!(report.abstract_defs, 1);
        assert_eq!(db.count("ThingDef"), 1);
        assert_eq!(db.count("TerrainDef"), 1);
        let human = db.get("ThingDef", "Human").unwrap();
        assert_eq!(human.node.child_text("category"), Some("Pawn"));
        assert_eq!(human.file, "b.xml");
    }

    #[test]
    fn bad_documents_are_reported_not_fatal() {
        let (db, report) = load_documents(
            "core",
            &[
                ("broken.xml", "<Defs><ThingDef>"),
                ("wrongroot.xml", "<Patch></Patch>"),
                (
                    "nodefname.xml",
                    "<Defs><ThingDef><label>x</label></ThingDef></Defs>",
                ),
                (
                    "ok.xml",
                    "<Defs><ThingDef><defName>A</defName></ThingDef></Defs>",
                ),
            ],
            &ActivePackages::default(),
        );
        assert_eq!(report.files_failed, 2);
        assert_eq!(report.files_loaded, 2);
        assert_eq!(report.warnings.len(), 3);
        assert_eq!(db.total(), 1);
    }

    #[test]
    fn song_defs_get_implicit_def_name() {
        let (db, report) = load_documents(
            "core",
            &[(
                "s.xml",
                "<Defs><SongDef><clipPath>Songs/Relax/Tune_a</clipPath></SongDef></Defs>",
            )],
            &ActivePackages::default(),
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(db.get("SongDef", "Tune_a").is_some());
    }

    #[test]
    fn missing_defs_dir_is_error() {
        let err = load_packs(&[PackSource {
            package_id: "x".into(),
            defs_dir: "/no/such/dir".into(),
        }])
        .unwrap_err();
        assert!(matches!(err, LoadError::MissingDefsDir(_)));
    }
}
