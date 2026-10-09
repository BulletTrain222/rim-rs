//! Reads textures from a real RimWorld install, if configured. Skipped
//! otherwise. Decoded pixels stay in memory; nothing is written to disk.

use std::path::Path;
use std::time::Instant;

use rimworld_assets::unity::TextureLibrary;
use rimworld_assets::{AppConfig, ENV_RIMWORLD_PATH, resolve_install};

#[test]
fn loads_real_textures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = AppConfig::load(&root.join("config.toml")).ok().flatten();
    let env = std::env::var_os(ENV_RIMWORLD_PATH).map(Into::into);
    let Ok(install) = resolve_install(None, env, config.as_ref(), &[]) else {
        eprintln!("skipping: no RimWorld installation configured");
        return;
    };

    let t = Instant::now();
    let lib = TextureLibrary::open(&install.root).expect("open texture library");
    eprintln!(
        "opened in {:?}: {} resource paths",
        t.elapsed(),
        lib.resource_count()
    );

    for (path, w, h) in [
        ("Terrain/Surfaces/Soil", 1024, 1024),
        ("Things/Building/Linked/Rock_Atlas", 320, 320),
    ] {
        let t = Instant::now();
        let img = lib
            .load(path)
            .unwrap()
            .unwrap_or_else(|| panic!("{path} missing"));
        eprintln!("{path}: {}x{} in {:?}", img.width, img.height, t.elapsed());
        assert_eq!((img.width, img.height), (w, h));
        let distinct: std::collections::HashSet<&[u8]> = img.pixels.chunks(4).take(4096).collect();
        assert!(distinct.len() > 8, "{path} decoded to a flat image");
    }

    // A pawn body, which has transparent surroundings (DXT5 alpha).
    let body = lib
        .load("Things/Pawn/Humanlike/Bodies/Naked_Male_south")
        .unwrap()
        .expect("body texture");
    let transparent = body.pixels.chunks(4).filter(|p| p[3] == 0).count();
    let opaque = body.pixels.chunks(4).filter(|p| p[3] == 255).count();
    assert!(transparent > 0 && opaque > 0, "{transparent} / {opaque}");
    assert!(lib.load("Definitely/Not/A/Texture").unwrap().is_none());

    let mips = lib.load_mips("Terrain/Surfaces/Soil").unwrap().unwrap();
    assert_eq!(mips.len(), 11, "1024 -> 1");
    assert_eq!((mips[10].width, mips[10].height), (1, 1));

    // Folder graphics (stack-count / meal variants) list their textures.
    for folder in [
        "Things/Item/Meal/SurvivalPack",
        "Things/Item/Resource/PlantFoodRaw/Potatoes",
    ] {
        let found = lib.textures_in_folder(folder);
        eprintln!("{folder}: {found:?}");
        assert!(!found.is_empty(), "{folder} has textures");
    }
}
