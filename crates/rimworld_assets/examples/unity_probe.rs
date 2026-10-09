//! Prints statistics about the Unity files in a RimWorld install (counts per
//! class, externals). Reads only; prints only structural information.
//!
//! usage: cargo run -p rimworld_assets --example unity_probe -- <install>

use std::collections::BTreeMap;

use rimworld_assets::unity::resources::ResourceContainer;
use rimworld_assets::unity::serialized::{SerializedFile, class_id};
use rimworld_assets::unity::texture::TextureHeader;

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: unity_probe <install>");
    let data_dir = std::path::Path::new(&root).join("RimWorldWin64_Data");
    for name in [
        "globalgamemanagers",
        "resources.assets",
        "sharedassets0.assets",
    ] {
        let bytes = std::fs::read(data_dir.join(name)).expect("read");
        match SerializedFile::parse(&bytes) {
            Ok(f) => {
                let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
                for o in &f.objects {
                    *counts.entry(o.class_id).or_default() += 1;
                }
                println!(
                    "{name}: v{} unity {} platform {} typetrees={} objects={} externals={:?}",
                    f.version,
                    f.unity_version,
                    f.target_platform,
                    f.has_type_trees,
                    f.objects.len(),
                    f.externals.iter().map(|e| &e.path).collect::<Vec<_>>()
                );
                println!("  class counts: {counts:?}");
            }
            Err(e) => println!("{name}: {e}"),
        }
    }

    // Resource paths and texture formats.
    let ggm = std::fs::read(data_dir.join("globalgamemanagers")).unwrap();
    let gf = SerializedFile::parse(&ggm).unwrap();
    let rm = gf
        .objects_of_class(class_id::RESOURCE_MANAGER)
        .next()
        .unwrap();
    let container = ResourceContainer::parse(gf.object_data(&ggm, rm), gf.big_endian).unwrap();
    let mut tex_paths: Vec<&str> = container
        .paths()
        .filter(|p| p.starts_with("textures/"))
        .collect();
    tex_paths.sort();
    println!(
        "resource paths: {} total, {} under textures/",
        container.len(),
        tex_paths.len()
    );
    for p in [
        "textures/terrain/surfaces/soil",
        "textures/things/pawn/humanlike/bodies/naked_male_south",
    ] {
        println!("  {p}: {:?}", container.get(p));
    }

    let res = std::fs::read(data_dir.join("resources.assets")).unwrap();
    let rf = SerializedFile::parse(&res).unwrap();
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = 0;
    for o in rf.objects_of_class(class_id::TEXTURE_2D) {
        match TextureHeader::parse(rf.object_data(&res, o), rf.big_endian) {
            Ok(t) => {
                if matches!(
                    t.format,
                    rimworld_assets::unity::texture::TextureFormat::Bc7
                ) {
                    println!("  bc7: {} {}x{}", t.name, t.width, t.height);
                }
                *formats.entry(format!("{:?}", t.format)).or_default() += 1
            }
            Err(e) => {
                {
                    let d = rf.object_data(&res, o);
                    let n = i32::from_le_bytes(d[..4].try_into().unwrap()) as usize;
                    let name = String::from_utf8_lossy(&d[4..4 + n.min(64)]);
                    println!(
                        "  texture {} {name:?} size {} failed: {e}",
                        o.path_id,
                        d.len()
                    );
                }
                failures += 1;
            }
        }
    }
    println!("texture formats: {formats:?}, failures: {failures}");
    for p in [
        "textures/terrain/surfaces/soil",
        "textures/things/building/linked/rock_atlas",
    ] {
        for ptr in container.get(p) {
            if let Some(o) = rf.object(ptr.path_id)
                && o.class_id == class_id::TEXTURE_2D
            {
                let t = TextureHeader::parse(rf.object_data(&res, o), false).unwrap();
                println!(
                    "  {p}: {} {}x{} {:?} mips {} {:?}",
                    t.name,
                    t.width,
                    t.height,
                    t.format,
                    t.mip_count,
                    match &t.pixels {
                        rimworld_assets::unity::texture::PixelSource::Inline(d) =>
                            format!("inline {}", d.len()),
                        rimworld_assets::unity::texture::PixelSource::Stream {
                            path,
                            offset,
                            size,
                        } => format!("{path}@{offset}+{size}"),
                    }
                );
            }
        }
    }

    // Optional filter: print texture paths containing the given substring.
    if let Some(filter) = std::env::args().nth(2) {
        for p in tex_paths
            .iter()
            .filter(|p| p.contains(filter.as_str()))
            .take(40)
        {
            let size = container.get(p).iter().find_map(|ptr| {
                let o = rf.object(ptr.path_id)?;
                (o.class_id == class_id::TEXTURE_2D)
                    .then(|| TextureHeader::parse(rf.object_data(&res, o), false).ok())
                    .flatten()
                    .map(|t| format!("{}x{} {:?}", t.width, t.height, t.format))
            });
            println!("  {p} {}", size.unwrap_or_default());
        }
    }
}
