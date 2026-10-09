//! Read-only access to Unity data files inside the user's install.
//!
//! Base-game textures are not loose files: they live in Unity's
//! `resources.assets` (+ `.resS` stream) and are addressed by resource path
//! through the `ResourceManager` in `globalgamemanagers`. This module reads
//! them **in memory** at runtime; nothing is ever written to disk.

mod bcn;
pub mod library;
pub mod reader;
pub mod resources;
pub mod serialized;
pub mod texture;

#[derive(Debug, thiserror::Error)]
pub enum UnityError {
    #[error("unexpected end of data")]
    Truncated,
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

pub use library::TextureLibrary;
pub use texture::{RgbaImage, TextureFormat};
