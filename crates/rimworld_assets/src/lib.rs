//! Locating and validating the user's own RimWorld installation.
//!
//! Nothing from the game is bundled with this project. Everything is read at
//! runtime from an installation the user points us at, via (highest priority
//! first) a command-line path, the `RIMWORLD_PATH` environment variable, a
//! `config.toml` file, or a few well-known default locations.

mod config;
mod install;
pub mod unity;
mod version;

pub use config::{AppConfig, ConfigError, RimWorldSection};
pub use install::{
    ContentPack, InstallError, PathSource, RimWorldInstall, default_locations, resolve_install,
};
pub use version::{GameVersion, SUPPORTED_VERSIONS, VersionParseError};

/// Environment variable that overrides the configured install path.
pub const ENV_RIMWORLD_PATH: &str = "RIMWORLD_PATH";

/// packageId of the base game content pack (`Data/Core/About/About.xml`).
pub const CORE_PACKAGE_ID: &str = "Ludeon.RimWorld";
