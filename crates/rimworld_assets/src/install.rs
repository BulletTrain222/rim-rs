use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::AppConfig;
use crate::version::GameVersion;
use crate::{CORE_PACKAGE_ID, ENV_RIMWORLD_PATH};

/// Where the install path came from (reported to the user at startup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    CommandLine,
    Environment,
    ConfigFile,
    DefaultLocation,
}

impl fmt::Display for PathSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathSource::CommandLine => "command line",
            PathSource::Environment => ENV_RIMWORLD_PATH,
            PathSource::ConfigFile => "config.toml",
            PathSource::DefaultLocation => "default location",
        })
    }
}

/// A content pack: a folder laid out like a mod (`About/`, `Defs/`, ...).
/// The base game is the pack at `Data/Core`.
#[derive(Debug, Clone)]
pub struct ContentPack {
    pub package_id: String,
    pub root: PathBuf,
}

impl ContentPack {
    pub fn defs_dir(&self) -> PathBuf {
        self.root.join("Defs")
    }
}

/// A validated RimWorld installation.
#[derive(Debug, Clone)]
pub struct RimWorldInstall {
    pub root: PathBuf,
    pub source: PathSource,
    /// `None` when `Version.txt` is missing or unreadable (not fatal).
    pub version: Option<GameVersion>,
    /// Non-fatal problems found while opening the install.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(
        "no RimWorld installation configured.\n\
         Copy config.example.toml to config.toml and set [rimworld] path, \
         pass --rimworld <path>, or set {ENV_RIMWORLD_PATH}.\n\
         Searched default locations:{searched}"
    )]
    NotConfigured { searched: String },

    #[error("RimWorld path {path} (from {origin}) does not exist or is not a folder")]
    NotFound { path: PathBuf, origin: PathSource },

    #[error(
        "{path} (from {origin}) is not a RimWorld installation: missing {missing}.\n\
         The path must be the folder that contains Version.txt and Data/Core.{hint}"
    )]
    MissingCore {
        path: PathBuf,
        origin: PathSource,
        missing: String,
        hint: String,
    },
}

impl RimWorldInstall {
    /// Validates that `root` looks like a RimWorld install with `Data/Core/Defs`.
    pub fn open(root: impl Into<PathBuf>, source: PathSource) -> Result<Self, InstallError> {
        let root = root.into();
        if !root.is_dir() {
            return Err(InstallError::NotFound {
                path: root,
                origin: source,
            });
        }
        for rel in ["Data", "Data/Core", "Data/Core/Defs"] {
            if !root.join(rel).is_dir() {
                return Err(InstallError::MissingCore {
                    hint: hint_for(&root),
                    missing: format!("{rel}/"),
                    path: root,
                    origin: source,
                });
            }
        }

        let mut warnings = Vec::new();
        let version = match std::fs::read_to_string(root.join("Version.txt")) {
            Ok(text) => match GameVersion::parse(&text) {
                Ok(v) => {
                    if !v.is_supported() {
                        warnings.push(format!(
                            "RimWorld {v} has not been tested (tested: {}); continuing anyway",
                            crate::SUPPORTED_VERSIONS.join(", ")
                        ));
                    }
                    Some(v)
                }
                Err(e) => {
                    warnings.push(e.to_string());
                    None
                }
            },
            Err(e) => {
                warnings.push(format!("could not read Version.txt: {e}"));
                None
            }
        };

        Ok(Self {
            root,
            source,
            version,
            warnings,
        })
    }

    /// The base game content pack (`Data/Core`).
    pub fn core(&self) -> ContentPack {
        ContentPack {
            package_id: CORE_PACKAGE_ID.to_owned(),
            root: self.root.join("Data").join("Core"),
        }
    }

    /// Content packs to load, in load order. Base game only for now; DLC and
    /// mods will be appended here later.
    pub fn content_packs(&self) -> Vec<ContentPack> {
        vec![self.core()]
    }
}

/// Suggests a corrected path for common mistakes.
fn hint_for(path: &Path) -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // Pointed at the .exe, Data, Data/Core, or RimWorldWin64_Data.
    for ancestor in path.ancestors().skip(1).take(3) {
        candidates.push(ancestor.to_owned());
    }
    // Pointed at a parent folder (e.g. steamapps/common).
    candidates.push(path.join("RimWorld"));
    for c in candidates {
        if c.join("Data").join("Core").join("Defs").is_dir() {
            return format!("\nDid you mean {}?", c.display());
        }
    }
    String::new()
}

/// Well-known install locations tried when nothing is configured.
pub fn default_locations() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("local/game")];
    if cfg!(windows) {
        for drive in ["C:", "D:", "E:"] {
            v.push(PathBuf::from(format!(
                "{drive}/Program Files (x86)/Steam/steamapps/common/RimWorld"
            )));
            v.push(PathBuf::from(format!(
                "{drive}/SteamLibrary/steamapps/common/RimWorld"
            )));
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        v.push(home.join(".local/share/Steam/steamapps/common/RimWorld"));
        v.push(home.join(".steam/steam/steamapps/common/RimWorld"));
        v.push(
            home.join(
                "Library/Application Support/Steam/steamapps/common/RimWorld/RimWorldMac.app",
            ),
        );
    }
    v
}

/// Resolves the install from (in priority order) the command line, the
/// environment, the config file and finally `defaults`.
///
/// An explicitly given path that turns out to be invalid is an error; we do
/// not silently fall back to another location.
pub fn resolve_install(
    cli: Option<PathBuf>,
    env: Option<PathBuf>,
    config: Option<&AppConfig>,
    defaults: &[PathBuf],
) -> Result<RimWorldInstall, InstallError> {
    let explicit = cli
        .map(|p| (p, PathSource::CommandLine))
        .or_else(|| env.map(|p| (p, PathSource::Environment)))
        .or_else(|| {
            config
                .and_then(|c| c.rimworld.path.clone())
                .map(|p| (p, PathSource::ConfigFile))
        });
    if let Some((path, source)) = explicit {
        return RimWorldInstall::open(path, source);
    }
    for path in defaults {
        if let Ok(install) = RimWorldInstall::open(path, PathSource::DefaultLocation) {
            return Ok(install);
        }
    }
    let searched = defaults
        .iter()
        .map(|p| format!("\n  {}", p.display()))
        .collect::<String>();
    Err(InstallError::NotConfigured { searched })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install(version: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Data/Core/Defs")).unwrap();
        if let Some(v) = version {
            std::fs::write(dir.path().join("Version.txt"), v).unwrap();
        }
        dir
    }

    #[test]
    fn opens_valid_install() {
        let dir = fake_install(Some("1.6.4871 rev590"));
        let install = RimWorldInstall::open(dir.path(), PathSource::CommandLine).unwrap();
        assert_eq!(install.version.as_ref().unwrap().build, 4871);
        assert!(install.warnings.is_empty());
        assert!(install.core().defs_dir().ends_with("Data/Core/Defs"));
        assert_eq!(install.content_packs().len(), 1);
    }

    #[test]
    fn unsupported_version_warns() {
        let dir = fake_install(Some("1.4.3901 rev1"));
        let install = RimWorldInstall::open(dir.path(), PathSource::CommandLine).unwrap();
        assert_eq!(install.warnings.len(), 1);
    }

    #[test]
    fn missing_version_warns() {
        let dir = fake_install(None);
        let install = RimWorldInstall::open(dir.path(), PathSource::CommandLine).unwrap();
        assert!(install.version.is_none());
        assert_eq!(install.warnings.len(), 1);
    }

    #[test]
    fn missing_core_is_error_with_hint() {
        let dir = fake_install(None);
        let core = dir.path().join("Data/Core");
        let err = RimWorldInstall::open(&core, PathSource::ConfigFile).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, InstallError::MissingCore { .. }));
        assert!(msg.contains("Did you mean"), "{msg}");
    }

    #[test]
    fn nonexistent_path_is_error() {
        let err =
            RimWorldInstall::open("/definitely/not/here", PathSource::Environment).unwrap_err();
        assert!(matches!(err, InstallError::NotFound { .. }));
    }

    #[test]
    fn priority_cli_over_env_over_config() {
        let a = fake_install(None);
        let b = fake_install(None);
        let c = fake_install(None);
        let cfg = AppConfig {
            rimworld: crate::RimWorldSection {
                path: Some(c.path().to_owned()),
            },
        };
        let r = |cli: Option<&Path>, env: Option<&Path>| {
            resolve_install(cli.map(Into::into), env.map(Into::into), Some(&cfg), &[]).unwrap()
        };
        assert_eq!(
            r(Some(a.path()), Some(b.path())).source,
            PathSource::CommandLine
        );
        assert_eq!(r(None, Some(b.path())).source, PathSource::Environment);
        assert_eq!(r(None, None).source, PathSource::ConfigFile);
    }

    #[test]
    fn invalid_explicit_path_does_not_fall_back() {
        let good = fake_install(None);
        let err = resolve_install(Some("/nope".into()), None, None, &[good.path().to_owned()])
            .unwrap_err();
        assert!(matches!(err, InstallError::NotFound { .. }));
    }

    #[test]
    fn falls_back_to_defaults_then_reports() {
        let good = fake_install(None);
        let ok = resolve_install(
            None,
            None,
            None,
            &[PathBuf::from("/nope"), good.path().into()],
        )
        .unwrap();
        assert_eq!(ok.source, PathSource::DefaultLocation);
        let err = resolve_install(None, None, None, &[PathBuf::from("/nope")]).unwrap_err();
        assert!(err.to_string().contains("/nope"));
    }
}
