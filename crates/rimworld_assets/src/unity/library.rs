//! Loads base-game textures by RimWorld `texPath` at runtime.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::UnityError;
use super::resources::ResourceContainer;
use super::serialized::{SerializedFile, class_id};
use super::texture::{PixelSource, RgbaImage, TextureHeader, decode, decode_mips};

/// A loaded asset file: raw bytes plus its parsed object table.
struct AssetFile {
    bytes: Vec<u8>,
    file: SerializedFile,
}

/// Read-only access to the textures in a RimWorld install's Unity data.
///
/// Opening reads `globalgamemanagers` (resource paths) and `resources.assets`
/// (texture headers, ~7 MB). Pixel data is read on demand from the `.resS`
/// stream file. Nothing is cached on disk.
pub struct TextureLibrary {
    data_dir: PathBuf,
    container: ResourceContainer,
    /// `globalgamemanagers` externals, used to resolve `PPtr::file_id`.
    externals: Vec<String>,
    assets: HashMap<String, AssetFile>,
}

impl TextureLibrary {
    /// Opens the Unity data folder of the install at `root`.
    pub fn open(root: &Path) -> Result<Self, UnityError> {
        let data_dir = find_data_dir(root).ok_or_else(|| {
            UnityError::Invalid(format!(
                "no Unity data folder (*_Data/globalgamemanagers) under {}",
                root.display()
            ))
        })?;
        let ggm_bytes = read(&data_dir.join("globalgamemanagers"))?;
        let ggm = SerializedFile::parse(&ggm_bytes)?;
        let rm = ggm
            .objects_of_class(class_id::RESOURCE_MANAGER)
            .next()
            .ok_or_else(|| UnityError::Invalid("no ResourceManager".into()))?;
        let container = ResourceContainer::parse(ggm.object_data(&ggm_bytes, rm), ggm.big_endian)?;
        let externals: Vec<String> = ggm.externals.iter().map(|e| e.path.clone()).collect();

        let mut assets = HashMap::new();
        let name = "resources.assets".to_owned();
        let bytes = read(&data_dir.join(&name))?;
        let file = SerializedFile::parse(&bytes)?;
        assets.insert(name, AssetFile { bytes, file });

        Ok(Self {
            data_dir,
            container,
            externals,
            assets,
        })
    }

    pub fn resource_count(&self) -> usize {
        self.container.len()
    }

    /// Finds the `Texture2D` header for a RimWorld texture path such as
    /// `Terrain/Surfaces/Soil` (relative to `Textures/`, case-insensitive).
    pub fn header(&self, tex_path: &str) -> Option<(TextureHeader<'_>, &str)> {
        let key = format!("textures/{}", tex_path.trim_start_matches('/'));
        self.container.get(&key).iter().find_map(|ptr| {
            let file_name = match ptr.file_id {
                0 => return None, // globalgamemanagers itself holds no textures
                n => self.externals.get(n as usize - 1)?,
            };
            let asset = self.assets.get(file_name)?;
            let obj = asset.file.object(ptr.path_id)?;
            if obj.class_id != class_id::TEXTURE_2D {
                return None;
            }
            let header = TextureHeader::parse(
                asset.file.object_data(&asset.bytes, obj),
                asset.file.big_endian,
            )
            .ok()?;
            Some((header, file_name.as_str()))
        })
    }

    /// Texture paths directly inside `folder` (relative to `Textures/`,
    /// lower-case as Unity stores them), sorted by name; or the texture at
    /// `folder` itself when it has none. Used for graphics that pick one of
    /// several textures (stack sizes, variants).
    pub fn textures_in_folder(&self, folder: &str) -> Vec<String> {
        let prefix = format!(
            "textures/{}/",
            folder.trim_matches('/').to_ascii_lowercase()
        );
        let mut found: Vec<String> = self
            .container
            .paths()
            .filter_map(|p| p.strip_prefix(&prefix))
            .filter(|rest| !rest.contains('/'))
            .map(|name| format!("{}{name}", &prefix["textures/".len()..]))
            .filter(|p| self.contains(p))
            .collect();
        found.sort();
        // Unity's folder loading also returns an asset stored at the folder
        // path itself (e.g. a stack graphic with a single texture).
        if found.is_empty() && self.contains(folder) {
            found.push(folder.trim_matches('/').to_ascii_lowercase());
        }
        found
    }

    pub fn contains(&self, tex_path: &str) -> bool {
        self.header(tex_path).is_some()
    }

    /// Loads and decodes a texture's top mip level. `Ok(None)` if no texture
    /// exists at that path.
    pub fn load(&self, tex_path: &str) -> Result<Option<RgbaImage>, UnityError> {
        let Some((header, _)) = self.header(tex_path) else {
            return Ok(None);
        };
        let need = header
            .format
            .level0_size(header.width, header.height)
            .ok_or_else(|| UnityError::Unsupported(format!("{:?}", header.format)))?;
        let image = match &header.pixels {
            PixelSource::Inline(data) => decode(header.format, header.width, header.height, data)?,
            PixelSource::Stream { path, offset, size } => {
                let len = need.min(*size as usize);
                let data = self.read_stream(path, *offset, len)?;
                decode(header.format, header.width, header.height, &data)?
            }
        };
        Ok(Some(image))
    }

    /// Loads the full mip chain (largest first), for smooth minification.
    pub fn load_mips(&self, tex_path: &str) -> Result<Option<Vec<RgbaImage>>, UnityError> {
        let Some((header, _)) = self.header(tex_path) else {
            return Ok(None);
        };
        let levels = header.mip_count.max(1) as u32;
        let mips = match &header.pixels {
            PixelSource::Inline(data) => {
                decode_mips(header.format, header.width, header.height, data, levels)?
            }
            PixelSource::Stream { path, offset, size } => {
                let data = self.read_stream(path, *offset, *size as usize)?;
                decode_mips(header.format, header.width, header.height, &data, levels)?
            }
        };
        Ok(Some(mips))
    }

    fn read_stream(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>, UnityError> {
        // Stream paths are file names relative to the data folder; refuse
        // anything that tries to leave it.
        let file_name = Path::new(name)
            .file_name()
            .ok_or_else(|| UnityError::Invalid(format!("stream path {name:?}")))?;
        let path = self.data_dir.join(file_name);
        let io = |source| UnityError::Io {
            path: path.clone(),
            source,
        };
        let mut f = std::fs::File::open(&path).map_err(io)?;
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut buf = vec![0; len];
        f.read_exact(&mut buf).map_err(io)?;
        Ok(buf)
    }
}

fn read(path: &Path) -> Result<Vec<u8>, UnityError> {
    std::fs::read(path).map_err(|source| UnityError::Io {
        path: path.to_owned(),
        source,
    })
}

/// `RimWorldWin64_Data`, `RimWorldLinux_Data`, or the macOS bundle's `Data`.
fn find_data_dir(root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with("_Data"))
        })
        .collect();
    candidates.sort();
    candidates.push(root.join("Contents/Resources/Data"));
    candidates
        .into_iter()
        .find(|d| d.join("globalgamemanagers").is_file())
}

#[cfg(test)]
mod tests {
    use super::super::serialized::tests::build;
    use super::*;

    fn container_object(entries: &[(&str, i32, i64)]) -> Vec<u8> {
        let mut out = (entries.len() as i32).to_le_bytes().to_vec();
        for (p, f, id) in entries {
            out.extend_from_slice(&(p.len() as i32).to_le_bytes());
            out.extend_from_slice(p.as_bytes());
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            out.extend_from_slice(&f.to_le_bytes());
            out.extend_from_slice(&id.to_le_bytes());
        }
        out
    }

    /// A synthetic 2x1 RGBA32 Texture2D with inline pixels.
    fn texture_object(name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        let s = |o: &mut Vec<u8>, s: &str| {
            o.extend_from_slice(&(s.len() as i32).to_le_bytes());
            o.extend_from_slice(s.as_bytes());
            while !o.len().is_multiple_of(4) {
                o.push(0);
            }
        };
        let i = |o: &mut Vec<u8>, v: i32| o.extend_from_slice(&v.to_le_bytes());
        s(&mut o, name);
        i(&mut o, 0);
        o.extend_from_slice(&[0, 0, 0, 0]); // two bools + align
        for v in [2, 1, 8, 0, 4, 1] {
            i(&mut o, v); // width, height, size, mips stripped, RGBA32, mip count
        }
        o.extend_from_slice(&[0; 4]); // four bools
        for _ in 0..3 + 6 + 2 {
            i(&mut o, 0);
        }
        s(&mut o, ""); // mip limit group
        i(&mut o, 0); // platform blob
        i(&mut o, 8);
        o.extend_from_slice(&[255, 0, 0, 255, 0, 255, 0, 255]);
        o.extend_from_slice(&0u64.to_le_bytes());
        i(&mut o, 0);
        s(&mut o, "");
        o
    }

    #[test]
    fn loads_texture_through_resource_path() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Game_Data");
        std::fs::create_dir(&data).unwrap();
        let ggm = build(
            &[(147, 1, container_object(&[("textures/terrain/test", 1, 5)]))],
            &["resources.assets"],
        );
        std::fs::write(data.join("globalgamemanagers"), ggm).unwrap();
        std::fs::write(
            data.join("resources.assets"),
            build(&[(28, 5, texture_object("Test"))], &[]),
        )
        .unwrap();

        let lib = TextureLibrary::open(dir.path()).unwrap();
        assert!(lib.contains("Terrain/Test"));
        assert!(!lib.contains("Terrain/Nope"));
        let img = lib.load("Terrain/Test").unwrap().unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(lib.load("Nope").unwrap().is_none());
    }

    #[test]
    fn missing_data_dir_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TextureLibrary::open(dir.path()).is_err());
    }
}
