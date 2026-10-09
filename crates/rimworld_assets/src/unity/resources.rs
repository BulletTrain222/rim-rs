//! Unity's `ResourceManager`: maps `Resources/` paths to objects.
//!
//! Layout (Unity 2022.3, no type tree):
//! `m_Container: array of (aligned string path, PPtr { i32 file_id, i64 path_id })`,
//! followed by dependency data we do not need. `file_id` 0 is the file itself;
//! `n > 0` is the n-th entry of that file's externals list.
//!
//! Paths are stored lower-case without extension, e.g.
//! `textures/terrain/surfaces/soil`. RimWorld's `texPath` values are relative
//! to `Textures/` and compared case-insensitively.

use std::collections::HashMap;

use super::UnityError;
use super::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PPtr {
    pub file_id: i32,
    pub path_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceContainer {
    entries: HashMap<String, Vec<PPtr>>,
}

impl ResourceContainer {
    pub fn parse(object: &[u8], big_endian: bool) -> Result<Self, UnityError> {
        let mut r = Reader::new(object, big_endian);
        let n = r.count(16)?;
        let mut entries: HashMap<String, Vec<PPtr>> = HashMap::with_capacity(n);
        for _ in 0..n {
            let path = r.aligned_string()?;
            let file_id = r.i32()?;
            let path_id = r.i64()?;
            entries
                .entry(path.to_ascii_lowercase())
                .or_default()
                .push(PPtr { file_id, path_id });
        }
        Ok(Self { entries })
    }

    /// All objects registered under `path` (several objects of different
    /// types can share one path, e.g. a texture and a sprite).
    pub fn get(&self, path: &str) -> &[PPtr] {
        self.entries
            .get(&path.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(out: &mut Vec<u8>, path: &str, file_id: i32, path_id: i64) {
        out.extend_from_slice(&(path.len() as i32).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out.extend_from_slice(&file_id.to_le_bytes());
        out.extend_from_slice(&path_id.to_le_bytes());
    }

    #[test]
    fn parses_container() {
        let mut data = 3i32.to_le_bytes().to_vec();
        entry(&mut data, "textures/terrain/surfaces/soil", 3, 42);
        entry(&mut data, "Textures/Things/Pawn/Body", 3, 7);
        entry(&mut data, "textures/terrain/surfaces/soil", 3, 43);
        data.extend_from_slice(&0i32.to_le_bytes()); // trailing dependency data
        let c = ResourceContainer::parse(&data, false).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("Textures/Terrain/Surfaces/Soil").len(), 2);
        assert_eq!(
            c.get("textures/things/pawn/body")[0],
            PPtr {
                file_id: 3,
                path_id: 7
            }
        );
        assert!(c.get("nope").is_empty());
    }
}
