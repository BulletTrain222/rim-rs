# Status

rim-rs is experimental and incomplete. This page lists what the
simulation and the app do today and the main gaps. Reference build:
RimWorld **1.6.4871 rev591**, base game only.

Verification labels used in code comments:

- **STATIC VERIFIED**: the rule was checked against the game's data and
  behaviour description, but not compared tick by tick.
- **DIFFERENTIAL VERIFIED**: replayed against recordings of the original game
  and compared tick by tick. The recordings themselves are not part of this
  repository; the tests that use them skip when they are missing.
- `COMPATIBILITY TODO: currently approximate` marks known approximations.

## Implemented

Data and assets

- Finds the RimWorld install (`--rimworld`, `RIMWORLD_PATH`, `config.toml`,
  common Steam locations) and reads `Version.txt`.
- Loads all base-game Def XML with RimWorld's inheritance rules
  (`Name`/`ParentName`/`Abstract`/`Inherit`, `MayRequire`) into a Def database
  with typed views for the Defs the simulation uses.
- Decodes textures from the install's Unity data in memory at runtime
  (nothing is extracted to disk).

Simulation (headless, deterministic, 60 ticks/s)

- Map generation from the game's rules for a temperate surface tile:
  elevation, fertility, rock types, mountains with natural roofs, ore, ponds,
  wild plants and the biome's wild animals.
- Pathfinding, movement timing, doors, regions and rooms, reservations.
- Think trees interpreted from `ThinkTreeDef` XML; jobs and job drivers.
- Needs: food (food choice, eating, malnutrition), rest (beds, sleeping,
  timetables), recreation (partial), mood (partial, parked).
- Work: work types and work givers with per-pawn priorities, hauling to
  stockpiles with filters, construction (blueprints, frames, materials),
  deconstruction, floors, roofs, mining and tunnels, smoothing, farming and
  plant cutting, cleaning, research, cooking and butchering bills, tending,
  feeding and rescuing patients.
- Health: body parts, injuries, bleeding, pain, capacities, healing, downed
  and dead pawns, corpses, armor, temperature injuries.
- Environment: outdoor and room temperatures, lights, power nets
  (generators, batteries, switches), food rot, deterioration of items left
  outside, filth.
- Combat: melee, drafting, ranged attacks with warmup, cover, projectiles
  and cooldown.
- Animals: wild animals wandering and grazing, hunting, predators hunting
  prey, animals leaving the map.
- Save/load of the simulation state (own JSON format).

App (Bevy)

- Native-style interface: colonist bar, map selection, inspect pane,
  gizmos, right-click orders menus, main buttons, the Architect with
  material menus, draw styles and zones, and the Work and Schedule tabs.
- Scripted play (`--play`) for unattended checks.

## Main limitations

- Mood, thoughts and mental breaks are only partly implemented.
- No storyteller, raids, quests, world map, caravans, trade or factions
  beyond what the colony needs.
- No DLC or mod support; base game only.
- Many buildings, items and work givers are not supported yet; the
  interface leaves unsupported entries out.
- Inspect tabs (bills, storage, health, gear, needs), the research,
  animals and wildlife tabs and other main tabs are not implemented.
- Rendering is approximate in places (portraits, some graphics, text
  metrics and fonts).
- Random draws generally differ from the game's, so behaviour that depends
  on them matches in rules, not in exact outcomes.
