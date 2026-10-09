use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Contents of `config.toml`.
///
/// ```toml
/// [rimworld]
/// path = "C:/path/to/RimWorld"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub rimworld: RimWorldSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RimWorldSection {
    /// Root of the RimWorld installation (the folder containing `Data/`).
    pub path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl AppConfig {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Loads a config file. A missing file is not an error (returns `None`).
    pub fn load(path: &Path) -> Result<Option<Self>, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        Self::parse(&text)
            .map(Some)
            .map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_path() {
        let cfg = AppConfig::parse("[rimworld]\npath = \"D:/Games/RimWorld\"\n").unwrap();
        assert_eq!(cfg.rimworld.path, Some(PathBuf::from("D:/Games/RimWorld")));
    }

    #[test]
    fn empty_config_is_valid() {
        let cfg = AppConfig::parse("").unwrap();
        assert!(cfg.rimworld.path.is_none());
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            AppConfig::load(&dir.path().join("nope.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_toml_is_error() {
        assert!(AppConfig::parse("[rimworld\npath=").is_err());
    }
}
