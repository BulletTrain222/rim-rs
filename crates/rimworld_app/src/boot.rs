//! Startup before any window opens: resolve the install, load Defs, build the
//! simulation. Errors here are printed and the process exits.

use std::path::PathBuf;
use std::sync::Arc;

use rimworld_assets::unity::TextureLibrary;
use rimworld_assets::{
    AppConfig, ENV_RIMWORLD_PATH, RimWorldInstall, default_locations, resolve_install,
};
use rimworld_defs::{GameDefs, LoadReport, PackSource, load_packs};
use rimworld_sim::needs::NeedKind;
use rimworld_sim::scenario::{DEFAULT_SCENARIO, spawn_starting_food};
use rimworld_sim::{Cell, GridSize, Sim, TestMapPalette, generate_test_map};

pub const MAP_SIZE: GridSize = GridSize::new(100, 100);

/// Placeholder names (RimWorld's name generation comes later).
const PAWN_NAMES: [&str; 6] = ["Tester", "Ada", "Brin", "Cole", "Dara", "Emil"];

#[derive(Debug, Default)]
pub struct Args {
    pub rimworld: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub seed: u64,
    /// Load everything, print the summary, and exit without opening a window.
    pub check: bool,
    /// Scripted move order for the colonist at startup (`--order x,z`).
    pub order: Option<Cell>,
    /// Save a screenshot after `capture_after` frames, then exit.
    pub screenshot: Option<PathBuf>,
    pub capture_after: u32,
    /// Number of colonists to spawn.
    pub pawns: usize,
    /// Initial game speed in ticks per fixed step (1, 3, 6, 15).
    pub speed: u32,
    /// Take the screenshot once the simulation reaches this tick.
    pub capture_tick: Option<u64>,
    /// Scripted UI test: deselect, click the pawn, then click a cell.
    pub click_test: bool,
    /// Scripted UI test: box-select around the spawn, then order the group.
    pub box_test: bool,
    /// Initial camera zoom (orthographic scale; smaller = closer).
    pub zoom: Option<f32>,
    /// Debug: draw this texture (RimWorld texPath) over the map centre.
    pub view_texture: Option<String>,
    /// Initial camera focus cell (default: the colonist).
    pub focus: Option<Cell>,
    /// Scripted play file (`--play`).
    pub play: Option<std::path::PathBuf>,
    /// Debug: start colonists with this food level (fraction of maximum).
    pub food_level: Option<f32>,
    /// Debug: place a row of this many steel wall blueprints at startup.
    pub walls: usize,
    /// Debug: designate a potato growing zone near the colonists.
    pub farm: bool,
    /// Debug: the colony-loop setup (a farm, plus blueprints for a walled
    /// wooden bedroom with a door and a bed per colonist).
    pub colony: bool,
    /// Demo: a staged colony for short videos (`showcase.rs`).
    pub showcase: bool,
}

impl Args {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut out = Args {
            seed: 1,
            capture_after: 120,
            pawns: 3,
            speed: 1,
            zoom: None,
            ..Default::default()
        };
        while let Some(a) = args.next() {
            let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
            match a.as_str() {
                "--rimworld" => out.rimworld = Some(value("--rimworld")?.into()),
                "--config" => out.config = Some(value("--config")?.into()),
                "--seed" => {
                    out.seed = value("--seed")?
                        .parse()
                        .map_err(|_| "--seed needs a number".to_owned())?
                }
                "--check" => out.check = true,
                "--order" => out.order = Some(parse_cell("--order", &value("--order")?)?),
                "--click-test" => out.click_test = true,
                "--box-test" => out.box_test = true,
                "--speed" => {
                    out.speed = value("--speed")?
                        .parse()
                        .map_err(|_| "--speed needs a number".to_owned())?
                }
                "--capture-tick" => {
                    out.capture_tick = Some(
                        value("--capture-tick")?
                            .parse()
                            .map_err(|_| "--capture-tick needs a number".to_owned())?,
                    )
                }
                "--food-level" => {
                    out.food_level = Some(
                        value("--food-level")?
                            .parse()
                            .map_err(|_| "--food-level needs a number".to_owned())?,
                    )
                }
                "--pawns" => {
                    out.pawns = value("--pawns")?
                        .parse()
                        .map_err(|_| "--pawns needs a number".to_owned())?
                }
                "--zoom" => {
                    out.zoom = Some(
                        value("--zoom")?
                            .parse()
                            .map_err(|_| "--zoom needs a number".to_owned())?,
                    )
                }
                "--focus" => out.focus = Some(parse_cell("--focus", &value("--focus")?)?),
                "--farm" => out.farm = true,
                "--colony" => {
                    out.colony = true;
                    out.farm = true;
                }
                "--showcase" => out.showcase = true,
                "--play" => out.play = Some(value("--play")?.into()),
                "--walls" => {
                    out.walls = value("--walls")?
                        .parse()
                        .map_err(|_| "--walls needs a number".to_owned())?
                }
                "--view-texture" => out.view_texture = Some(value("--view-texture")?),
                "--screenshot" => out.screenshot = Some(value("--screenshot")?.into()),
                "--frames" => {
                    out.capture_after = value("--frames")?
                        .parse()
                        .map_err(|_| "--frames needs a number".to_owned())?
                }
                "-h" | "--help" => return Err(USAGE.to_owned()),
                other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
            }
        }
        Ok(out)
    }
}

fn parse_cell(flag: &str, v: &str) -> Result<Cell, String> {
    v.split_once(',')
        .and_then(|(x, z)| Some(Cell::new(x.trim().parse().ok()?, z.trim().parse().ok()?)))
        .ok_or_else(|| format!("{flag} expects x,z, got {v:?}"))
}

pub const USAGE: &str = "\
usage: rimworld_app [--rimworld <path>] [--config <file>] [--seed <n>] [--check]
                    [--order x,z] [--screenshot <file.png> [--frames <n>]]

  --rimworld <path>  RimWorld install folder (overrides RIMWORLD_PATH and config.toml)
  --config <file>    config file to read (default: ./config.toml)
  --seed <n>         test map seed (default 1)
  --check            load Defs, print the summary and exit
  --pawns <n>        number of colonists to spawn (default 3)
  --food-level <f>   start colonists at this food level (0-1), e.g. 0.25 to see them eat
  --walls <n>        place a row of n steel wall blueprints near the colonists
  --farm             designate a potato growing zone near the colonists
  --colony           --farm plus blueprints for a wooden bedroom (walls, door, beds)
  --showcase         a staged demo colony (built room, research, power, farm, blueprints)
  --order x,z        order the first colonist to cell (x, z) at startup
  --click-test       scripted clicks: select the pawn, then order it to a cell
  --box-test         scripted box selection of all pawns, then a group order
  --focus x,z        initial camera focus cell
  --play FILE        play a script of key presses and clicks (see script.rs)
  --zoom <scale>     initial camera scale (default: the game's starting camera
                     size; clamped to the game's zoom range)
  --view-texture <p> debug: draw texture <p> (e.g. Terrain/Surfaces/Soil) on screen
  --speed <n>        initial speed in ticks per step (1, 3, 6, 15)
  --capture-tick <t> with --screenshot: capture when the sim reaches tick t
  --screenshot <f>   save a screenshot after --frames frames (default 120), then exit";

/// Everything the front-end needs, produced before the window opens.
pub struct Boot {
    pub install: RimWorldInstall,
    /// `None` if the Unity data could not be read (debug colours are used).
    pub textures: Option<TextureLibrary>,
    pub sim: Sim,
    /// Spawned colonists; the first is the one scripted options act on.
    pub colonists: Vec<rimworld_sim::PawnId>,
}

pub fn boot(args: &Args) -> Result<Boot, String> {
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let config = AppConfig::load(&config_path).map_err(|e| e.to_string())?;
    let env = std::env::var_os(ENV_RIMWORLD_PATH).map(PathBuf::from);
    let install = resolve_install(
        args.rimworld.clone(),
        env,
        config.as_ref(),
        &default_locations(),
    )
    .map_err(|e| e.to_string())?;

    println!(
        "RimWorld installation: {} (from {})",
        install.root.display(),
        install.source
    );
    match &install.version {
        Some(v) => println!("RimWorld version:      {v}"),
        None => println!("RimWorld version:      unknown"),
    }
    for w in &install.warnings {
        println!("warning: {w}");
    }

    let packs: Vec<PackSource> = install
        .content_packs()
        .into_iter()
        .map(|p| PackSource {
            defs_dir: p.defs_dir(),
            package_id: p.package_id,
        })
        .collect();
    let (db, report) = load_packs(&packs).map_err(|e| e.to_string())?;
    let (defs, typed_warnings) = GameDefs::from_database(db);
    print_summary(&defs, &report, &typed_warnings);

    if let Some(tree) = defs.think_trees.get("Humanlike") {
        let (total, supported) = rimworld_sim::think::support_stats(tree);
        println!(
            "Think tree Humanlike: {supported}/{total} nodes interpreted (others yield no job)"
        );
    }
    let defs = Arc::new(defs);
    let palette = TestMapPalette::from_defs(&defs).map_err(|e| e.to_string())?;
    if !palette.missing.is_empty() {
        println!(
            "warning: test map defs not found, using fallbacks: {:?}",
            palette.missing
        );
    }
    // COMPATIBILITY TODO: currently approximate — the biome is fixed
    // (temperate forest) instead of coming from a world tile.
    let biome = defs.biomes.id("TemperateForest");
    // The game's elevation, rock, roof and terrain generation for a fixed
    // small-hills inland tile (world seed "ferrocolony-<seed>", tile 100);
    // the staged showcase keeps the simple test map.
    // COMPATIBILITY TODO: currently approximate — no world: the tile,
    // hilliness and world seed string are fixed scenario inputs; plants,
    // ruins, geysers, the player's start spot and fog are not the game's
    // map generation.
    let map = match biome {
        Some(b) if !args.showcase => {
            let input = rimworld_sim::native_mapgen::TileInput {
                world_seed: format!("ferrocolony-{}", args.seed),
                tile: 100,
                size: MAP_SIZE,
                hilliness: rimworld_sim::native_mapgen::Hilliness::SmallHills,
                biome: b,
                starting_map: true,
                forced_rocks: None,
                ore_blotches_override: None,
            };
            let g = rimworld_sim::native_mapgen::generate(&defs, &input);
            let comps = rimworld_sim::native_mapgen::rock_components(&g);
            println!(
                "Map: small hills, rock {:?}, {} rock cells, largest masses {:?}",
                g.rock_types
                    .iter()
                    .map(|&d| defs.things[d].label.clone())
                    .collect::<Vec<_>>(),
                g.rock.iter().filter(|r| r.is_some()).count(),
                &comps[..comps.len().min(4)]
            );
            let mut map = rimworld_sim::native_mapgen::to_map(&g);
            rimworld_sim::mapgen::spawn_biome_plants(&mut map, &defs, &defs.biomes[b], args.seed);
            map
        }
        _ => {
            let mut map = generate_test_map(MAP_SIZE, args.seed, &palette);
            if let Some(b) = biome {
                rimworld_sim::mapgen::apply_biome(&mut map, &defs, &defs.biomes[b], args.seed);
            }
            map
        }
    };
    if let Some(b) = biome {
        println!(
            "Biome: {} ({} wild plants)",
            defs.biomes[b].label,
            map.plants().len()
        );
    }
    let mut sim = Sim::with_seed(defs.clone(), map, args.seed);
    sim.set_biome(biome, args.seed as u32);
    // COMPATIBILITY TODO: currently approximate — without a world, the
    // tile's climate, position and the start date are fixed scenario
    // settings: a temperate tile averaging 15 °C at 35° N, starting on the
    // first day of the second twelfth (spring).
    sim.climate = Some(rimworld_sim::climate::Climate {
        tile_temperature: 15.0,
    });
    sim.latitude = 35.0;
    sim.start_day_of_year = 5;
    // The player faction's starting research (`ClassicStart` projects).
    sim.apply_starting_research("PlayerColony");

    let kind = defs
        .pawn_kinds
        .id("Colonist")
        .ok_or("PawnKindDef Colonist not found in the loaded Defs")?;
    let center = Cell::new(MAP_SIZE.width / 2, MAP_SIZE.height / 2);
    let spawn = sim
        .map
        .nearest_walkable(sim.path_grid(), center)
        .ok_or("test map has no walkable cell")?;
    if args.showcase {
        crate::showcase::stage_map(&mut sim, &defs, spawn);
        // Start in the morning.
        sim.debug_set_tick(60_000 / 24 * 8);
    }
    // `GenStep_Animals`: the biome's wild animals, before the colonists
    // arrive; the spawner keeps the ecosystem topped up from the map edge.
    if !args.showcase {
        let groups = sim.generate_wild_animals();
        let n = sim.pawns().iter().filter(|p| sim.is_animal(p.id)).count();
        println!(
            "Wild animals: {n} in {groups} groups (density {:.2}; F2: hunt tool)",
            sim.desired_animal_density()
        );
    }
    let mut colonists = Vec::new();
    // Fill cells in rings around the spawn point (spawn_pawn refuses
    // impassable or already claimed cells).
    'spawn: for r in 0..20 {
        for dz in -r..=r {
            for dx in -r..=r {
                if colonists.len() >= args.pawns.max(1) {
                    break 'spawn;
                }
                let at = spawn + Cell::new(dx, dz);
                if at.chebyshev(spawn) != r || !MAP_SIZE.contains(at) {
                    continue;
                }
                let name = PAWN_NAMES[colonists.len() % PAWN_NAMES.len()];
                if let Ok(id) = sim.spawn_pawn(kind, name, at) {
                    sim.give_starting_apparel(id);
                    colonists.push(id);
                }
            }
        }
    }
    // `Game.InitNewGame`: the starting colonists' optimism.
    sim.give_colonists_memory("NewColonyOptimism");
    let food = spawn_starting_food(&mut sim, DEFAULT_SCENARIO, spawn);
    let guns = rimworld_sim::scenario::spawn_starting_weapons(&mut sim, DEFAULT_SCENARIO, spawn);
    println!(
        "Starting weapons from scenario {DEFAULT_SCENARIO}: {guns} (right-click one to equip)"
    );
    // Prototype stand-in for the player's stockpile tool: a 5x5 stockpile
    // east of the colonists (the game starts without one).
    let stockpile: Vec<Cell> = (0..5)
        .flat_map(|dz| (0..5).map(move |dx| spawn + Cell::new(8 + dx, -2 + dz)))
        .filter(|&c| MAP_SIZE.contains(c) && sim.path_grid().walkable(c))
        .collect();
    if !stockpile.is_empty() {
        // As the stockpile designator makes it (named, the home area
        // around it).
        sim.zone_add(rimworld_sim::sim::ZoneKind::Stockpile, None, &stockpile);
        println!(
            "Stockpile: {} cells (prototype stand-in for the zone tool)",
            stockpile.len()
        );
    }
    let scattered =
        rimworld_sim::scenario::spawn_scattered_near_start(&mut sim, DEFAULT_SCENARIO, spawn);
    println!("Scattered near the start: {scattered} items (steel, wood, cloth)");
    println!("Starting food from scenario {DEFAULT_SCENARIO}: {food} items near the colonists");
    // COMPATIBILITY TODO: currently approximate — pawn generation (skills
    // from backstories, age and passions) is not modelled; colonists start
    // with Construction, Plants and Intellectual 6 so building, farming and
    // research are neither hopeless nor perfect, and each a specialty
    // (cooking, mining, shooting in turn) so the crew can cook; work types
    // are then assigned from the skills.
    for (n, &id) in colonists.iter().enumerate() {
        sim.set_skill(id, "Construction", 6);
        sim.set_skill(id, "Plants", 6);
        sim.set_skill(id, "Intellectual", 6);
        sim.set_skill(id, ["Cooking", "Mining", "Shooting"][n % 3], 6);
        sim.initialize_work(id);
    }
    if args.showcase {
        crate::showcase::stage_colonists(&mut sim, &defs, &colonists);
    }
    if args.farm
        && let Some(potato) = defs.things.id("Plant_Potato")
    {
        let cells: Vec<Cell> = (0..6)
            .flat_map(|dz| (0..6).map(move |dx| spawn + Cell::new(-12 + dx, -3 + dz)))
            .collect();
        if let Some(z) = sim.designate_growing_zone(potato, &cells) {
            println!(
                "Growing zone: {} potato cells (--farm)",
                sim.map.growing_zone(z).cells.len()
            );
        }
    }
    if args.walls > 0
        && let (Some(wall), Some(steel)) = (defs.things.id("Wall"), defs.things.id("Steel"))
    {
        let placed = (0..args.walls as i32)
            .filter(|&dx| {
                sim.place_blueprint(wall, Some(steel), spawn + Cell::new(-3 + dx, 5))
                    .is_some()
            })
            .count();
        println!("Placed {placed} steel wall blueprints (--walls)");
    }
    if args.colony
        && let (Some(wall), Some(door), Some(bed), Some(wood)) = (
            defs.things.id("Wall"),
            defs.things.id("Door"),
            defs.things.id("Bed"),
            defs.things.id("WoodLog"),
        )
    {
        // A room east-north of the colonists: walls on the border, a door
        // in the south wall, beds along the north side.
        let o = spawn + Cell::new(2, 6);
        let (w, h) = (2 + 2 * colonists.len() as i32, 6);
        let door_cell = o + Cell::new(w / 2, 0);
        let mut placed = 0;
        for x in 0..=w {
            for z in 0..=h {
                let c = o + Cell::new(x, z);
                if (x == 0 || x == w || z == 0 || z == h) && c != door_cell {
                    placed += usize::from(sim.place_blueprint(wall, Some(wood), c).is_some());
                }
            }
        }
        sim.place_blueprint(door, Some(wood), door_cell);
        for n in 0..colonists.len() as i32 {
            sim.place_blueprint_rotated(
                bed,
                Some(wood),
                o + Cell::new(1 + 2 * n, h - 2),
                rimworld_sim::Rot4::North,
            );
        }
        println!(
            "Colony setup: {placed} wall blueprints, a door and {} beds (--colony)",
            colonists.len()
        );
    }
    if let Some(level) = args.food_level {
        for &id in &colonists {
            sim.debug_set_need(id, NeedKind::Food, level.clamp(0.0, 1.0));
        }
    }
    let first = sim.pawn(colonists[0]).expect("spawned");
    println!(
        "Spawned {} colonists (PawnKindDef {} -> race ThingDef {}, MoveSpeed {}) near ({}, {})",
        colonists.len(),
        defs.pawn_kinds[first.kind].def_name,
        defs.things[first.race].def_name,
        defs.things[first.race].stat("MoveSpeed").unwrap_or(0.0),
        spawn.x,
        spawn.z
    );

    let textures = match TextureLibrary::open(&install.root) {
        Ok(lib) => {
            println!("Texture library: {} resource paths", lib.resource_count());
            Some(lib)
        }
        Err(e) => {
            println!("warning: textures unavailable ({e}); using debug colours");
            None
        }
    };

    Ok(Boot {
        install,
        textures,
        sim,
        colonists,
    })
}

fn print_summary(defs: &GameDefs, report: &LoadReport, typed_warnings: &[String]) {
    println!(
        "Loaded {} Def files in {:.0?}: {} Defs ({} abstract skipped), {} Def types",
        report.files_loaded,
        report.elapsed,
        defs.raw.total(),
        report.abstract_defs,
        defs.raw.counts().len()
    );
    println!(
        "  {:>5} ThingDefs ({} pawn races)",
        defs.things.len(),
        defs.things.iter().filter(|(_, t)| t.is_pawn()).count()
    );
    println!("  {:>5} TerrainDefs", defs.terrain.len());
    println!("  {:>5} PawnKindDefs", defs.pawn_kinds.len());
    let mut others: Vec<_> = defs
        .raw
        .counts()
        .into_iter()
        .filter(|(t, _)| !matches!(*t, "ThingDef" | "TerrainDef" | "PawnKindDef"))
        .collect();
    others.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (t, n) in others.iter().take(8) {
        println!("  {n:>5} {t}s");
    }
    if others.len() > 8 {
        let rest: usize = others[8..].iter().map(|(_, n)| n).sum();
        println!("  {rest:>5} others across {} more types", others.len() - 8);
    }
    let warnings: Vec<&String> = report.warnings.iter().chain(typed_warnings).collect();
    if warnings.is_empty() {
        println!("  no load warnings");
    } else {
        println!("  {} load warnings:", warnings.len());
        for w in warnings.iter().take(10) {
            println!("    {w}");
        }
        if warnings.len() > 10 {
            println!("    ... and {} more", warnings.len() - 10);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_args() {
        let a = parse(&[
            "--rimworld",
            "D:/RW",
            "--seed",
            "7",
            "--check",
            "--order",
            "3, 4",
        ])
        .unwrap();
        assert_eq!(a.order, Some(Cell::new(3, 4)));
        assert!(parse(&["--order", "3"]).is_err());
        assert_eq!(a.rimworld, Some(PathBuf::from("D:/RW")));
        assert_eq!(a.seed, 7);
        assert!(a.check);
        assert_eq!(parse(&[]).unwrap().seed, 1);
        assert!(parse(&["--seed"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
