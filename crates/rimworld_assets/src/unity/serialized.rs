//! Unity "SerializedFile" container (`*.assets`, `globalgamemanagers`).
//!
//! Layout, as observed in RimWorld 1.6 (Unity 2022.3, format version 22) and
//! described in public Unity-format documentation (see docs/research.md §9):
//!
//! ```text
//! header (big-endian):
//!   u32 legacy metadata size, u32 legacy file size, u32 version, u32 legacy data offset
//!   u8 endianness (0 = little), 3 reserved
//!   v22+: u32 metadata size, u64 file size, u64 data offset, u64 reserved
//! metadata (file endianness):
//!   cstring unity version, u32 target platform, bool has type trees
//!   types[]:   i32 class id, bool stripped, i16 script index,
//!              [16-byte script id if MonoBehaviour], 16-byte type hash, [type tree]
//!   objects[]: align4, i64 path id, i64/u32 byte start, u32 byte size, i32 type index
//!   script types[], externals[] (cstring, 16-byte guid, i32 type, cstring path)
//! ```
//!
//! Only what we need is parsed; everything is read-only and in memory.

use super::UnityError;
use super::reader::Reader;

/// Unity class IDs we care about.
pub mod class_id {
    pub const TEXTURE_2D: i32 = 28;
    pub const RESOURCE_MANAGER: i32 = 147;
    pub const MONO_BEHAVIOUR: i32 = 114;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub path_id: i64,
    /// Absolute offset of the object's data in the file.
    pub offset: u64,
    pub size: u32,
    pub class_id: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct External {
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct SerializedFile {
    pub version: u32,
    pub unity_version: String,
    pub target_platform: u32,
    pub has_type_trees: bool,
    pub big_endian: bool,
    pub objects: Vec<ObjectInfo>,
    pub externals: Vec<External>,
}

const MIN_VERSION: u32 = 19;
const MAX_VERSION: u32 = 22;

impl SerializedFile {
    pub fn parse(data: &[u8]) -> Result<Self, UnityError> {
        let mut r = Reader::new(data, true);
        let _legacy_meta = r.u32()?;
        let legacy_size = r.u32()?;
        let version = r.u32()?;
        let legacy_offset = r.u32()?;
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(UnityError::Unsupported(format!(
                "SerializedFile version {version} (supported {MIN_VERSION}-{MAX_VERSION})"
            )));
        }
        let big_endian = r.u8()? != 0;
        r.bytes(3)?;
        let (file_size, data_offset) = if version >= 22 {
            let _meta = r.u32()?;
            let size = r.u64()?;
            let offset = r.u64()?;
            let _reserved = r.u64()?;
            (size, offset)
        } else {
            (legacy_size as u64, legacy_offset as u64)
        };
        if file_size as usize != data.len() {
            return Err(UnityError::Invalid(format!(
                "header says {file_size} bytes, file has {}",
                data.len()
            )));
        }

        r.set_big_endian(big_endian);
        let unity_version = r.cstring()?;
        let target_platform = r.u32()?;
        let has_type_trees = r.bool()?;

        let type_count = r.count(23)?;
        let mut class_ids = Vec::with_capacity(type_count);
        for _ in 0..type_count {
            let class_id = r.i32()?;
            let _stripped = r.bool()?;
            let _script_index = r.i16()?;
            if class_id == class_id::MONO_BEHAVIOUR {
                r.bytes(16)?; // script id
            }
            r.bytes(16)?; // old type hash
            if has_type_trees {
                skip_type_tree(&mut r, version)?;
            }
            class_ids.push(class_id);
        }

        let object_count = r.count(20)?;
        let mut objects = Vec::with_capacity(object_count);
        for _ in 0..object_count {
            r.align4();
            let path_id = r.i64()?;
            let start = if version >= 22 {
                r.u64()?
            } else {
                r.u32()? as u64
            };
            let size = r.u32()?;
            let type_index = r.i32()?;
            let class_id = *class_ids
                .get(type_index as usize)
                .ok_or_else(|| UnityError::Invalid(format!("type index {type_index}")))?;
            let offset = data_offset + start;
            if offset + size as u64 > data.len() as u64 {
                return Err(UnityError::Invalid(format!(
                    "object {path_id} out of bounds"
                )));
            }
            objects.push(ObjectInfo {
                path_id,
                offset,
                size,
                class_id,
            });
        }

        let script_count = r.count(12)?;
        for _ in 0..script_count {
            let _file_index = r.i32()?;
            r.align4();
            let _local_id = r.i64()?;
        }

        let external_count = r.count(22)?;
        let mut externals = Vec::with_capacity(external_count);
        for _ in 0..external_count {
            let _empty = r.cstring()?;
            r.bytes(16)?; // guid
            let _kind = r.i32()?;
            externals.push(External { path: r.cstring()? });
        }

        Ok(Self {
            version,
            unity_version,
            target_platform,
            has_type_trees,
            big_endian,
            objects,
            externals,
        })
    }

    pub fn objects_of_class(&self, class_id: i32) -> impl Iterator<Item = &ObjectInfo> {
        self.objects.iter().filter(move |o| o.class_id == class_id)
    }

    pub fn object(&self, path_id: i64) -> Option<&ObjectInfo> {
        self.objects.iter().find(|o| o.path_id == path_id)
    }

    /// The object's bytes within `data` (the same buffer passed to `parse`).
    pub fn object_data<'a>(&self, data: &'a [u8], obj: &ObjectInfo) -> &'a [u8] {
        &data[obj.offset as usize..obj.offset as usize + obj.size as usize]
    }
}

/// Skips an embedded type tree (binary "blob" format, version >= 19).
fn skip_type_tree(r: &mut Reader<'_>, version: u32) -> Result<(), UnityError> {
    let nodes = r.count(32)?;
    let strings = r.count(1)?;
    r.bytes(nodes * 32)?;
    r.bytes(strings)?;
    if version >= 21 {
        let deps = r.count(4)?;
        r.bytes(deps * 4)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a minimal synthetic v22 SerializedFile containing `objects`
    /// (class id, path id, payload). Written from the layout above for tests.
    pub fn build(objects: &[(i32, i64, Vec<u8>)], externals: &[&str]) -> Vec<u8> {
        let mut meta = Vec::new();
        meta.extend_from_slice(b"2022.3.35f1\0");
        meta.extend_from_slice(&19u32.to_le_bytes());
        meta.push(0); // no type trees
        let mut classes: Vec<i32> = objects.iter().map(|o| o.0).collect();
        classes.dedup();
        meta.extend_from_slice(&(classes.len() as i32).to_le_bytes());
        for c in &classes {
            meta.extend_from_slice(&c.to_le_bytes());
            meta.push(0);
            meta.extend_from_slice(&(-1i16).to_le_bytes());
            meta.extend_from_slice(&[0; 16]);
        }
        meta.extend_from_slice(&(objects.len() as i32).to_le_bytes());
        let header_len = 48usize;
        let mut payload = Vec::new();
        let mut entries = Vec::new();
        for (class, path_id, data) in objects {
            while payload.len() % 8 != 0 {
                payload.push(0);
            }
            entries.push((*class, *path_id, payload.len() as u64, data.len() as u32));
            payload.extend_from_slice(data);
        }
        for (class, path_id, start, size) in entries {
            while !(header_len + meta.len()).is_multiple_of(4) {
                meta.push(0);
            }
            meta.extend_from_slice(&path_id.to_le_bytes());
            meta.extend_from_slice(&start.to_le_bytes());
            meta.extend_from_slice(&size.to_le_bytes());
            let idx = classes.iter().position(|&c| c == class).unwrap() as i32;
            meta.extend_from_slice(&idx.to_le_bytes());
        }
        meta.extend_from_slice(&0i32.to_le_bytes()); // script types
        meta.extend_from_slice(&(externals.len() as i32).to_le_bytes());
        for e in externals {
            meta.push(0);
            meta.extend_from_slice(&[0; 16]);
            meta.extend_from_slice(&0i32.to_le_bytes());
            meta.extend_from_slice(e.as_bytes());
            meta.push(0);
        }
        let mut data_offset = header_len + meta.len();
        data_offset = (data_offset + 15) & !15;
        let total = data_offset + payload.len();

        let mut out = Vec::new();
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&22u32.to_be_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&[0; 4]); // little endian + reserved
        out.extend_from_slice(&(meta.len() as u32).to_be_bytes());
        out.extend_from_slice(&(total as u64).to_be_bytes());
        out.extend_from_slice(&(data_offset as u64).to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        assert_eq!(out.len(), header_len);
        out.extend_from_slice(&meta);
        out.resize(data_offset, 0);
        out.extend_from_slice(&payload);
        out
    }

    #[test]
    fn parses_synthetic_file() {
        let bytes = build(
            &[
                (28, 7, vec![1, 2, 3]),
                (28, 9, vec![4; 10]),
                (147, 1, vec![5]),
            ],
            &["resources.assets"],
        );
        let f = SerializedFile::parse(&bytes).unwrap();
        assert_eq!(f.version, 22);
        assert_eq!(f.unity_version, "2022.3.35f1");
        assert!(!f.has_type_trees);
        assert_eq!(f.objects_of_class(28).count(), 2);
        let o = f.object(9).unwrap();
        assert_eq!(f.object_data(&bytes, o), &[4; 10]);
        assert_eq!(f.externals[0].path, "resources.assets");
    }

    #[test]
    fn rejects_bad_files() {
        assert!(SerializedFile::parse(&[0; 10]).is_err());
        let mut bytes = build(&[(28, 1, vec![0])], &[]);
        bytes[11] = 9; // version 9
        assert!(matches!(
            SerializedFile::parse(&bytes),
            Err(UnityError::Unsupported(_))
        ));
        let mut bytes = build(&[(28, 1, vec![0])], &[]);
        bytes.push(0); // size mismatch
        assert!(SerializedFile::parse(&bytes).is_err());
    }
}
