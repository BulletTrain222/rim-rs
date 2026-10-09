# Architecture

```
RimWorld installation (user-provided, read-only)
  └─ Version.txt, Data/Core/Defs/**/*.xml
        │
        ▼
rimworld_assets ── locate + validate install, config.toml, version
        │  ContentPack { package_id, root }
        ▼
rimworld_defs ──── XML → MayRequire filter → inheritance → DefDatabase
        │          → typed views (TerrainDef, ThingDef incl. race/ingestible, PawnKindDef,
        │            NeedDef, JobDef, ThinkTreeDef) + ref validation
        │  Arc<GameDefs>
        ▼
rimworld_sim ───── Map (terrain/buildings/items by DefId), PathGrid, A*, Pawns,
        │          think trees, jobs (Goto/Wait/LayDown/Ingest), needs, food, ticks
        │  Sim (read-only to rendering; mutated via Command + tick())
        ▼
rimworld_app ───── Bevy: rendering, camera, input, selection, debug overlay
```

## Crates

| Crate | Depends on | Engine-free? | Responsibility |
|---|---|---|---|
| `rimworld_assets` | serde, toml | yes | Find the install (CLI > `RIMWORLD_PATH` > `config.toml` > defaults), validate `Data/Core/Defs`, parse `Version.txt`, list content packs. Future: texture/audio readers for Unity asset files. |
| `rimworld_defs` | roxmltree | yes | Generic XML Def loading with RimWorld's inheritance semantics; generic `DefDatabase` keyed by (type, defName); typed views for the Defs we use. |
| `rimworld_sim` | rimworld_defs | yes | Deterministic, headless simulation at 60 ticks/s. Modules: `map` (terrain, buildings, item stacks), `path`, `think` (ThinkTreeDef interpreter), `job`, `needs`, `rest`, `food` (food choice and ingest rules), `sim` (tick loop and job drivers), `hash`/`camera` (game update scheduling). |
| `rimworld_app` | all + bevy | no | Desktop front-end. |

Only `rimworld_app` knows about Bevy. Everything below it is plain Rust and can
be unit-tested headless, driven by a different front-end, or compiled for
mobile.

## Key decisions

1. **Generic XML first, typed later.** RimWorld applies inheritance on XML
   trees before deserialising, and Def types have hundreds of fields. We keep
   every Def as an `XmlNode` tree in `DefDatabase` and build typed structs only
   for what the simulation uses. Adding a field = reading one more child.
2. **Data drives behaviour.** Walkability and movement cost come from
   `TerrainDef.passability/pathCost` and `ThingDef.passability/pathCost`; pawn
   speed from the race's `statBases/MoveSpeed`. No hardcoded terrain rules.
3. **`DefId<T>` handles.** The sim stores compact typed indices into
   `GameDefs` tables instead of strings.
4. **Simulation owns truth; rendering interpolates.** A pawn's authoritative
   position is a grid `Cell`; `visual_position()` derives a smooth position
   from step progress for drawing.
5. **Commands in, ticks forward.** The UI issues `Command::MoveTo`; the sim
   validates and plans. Bevy's `FixedUpdate` at 60 Hz calls `Sim::tick()`,
   decoupling sim rate from frame rate (and enabling speed controls later).
6. **Device-independent input.** Mouse/keyboard systems write a `CameraIntent`
   (pan in screen pixels, zoom factor + anchor) and `PointerClicks`; one system
   applies them. Touch pan/pinch will be another producer.
7. **Never redistribute game data.** Nothing is cached or copied to disk;
   the guarded repository layout is described in `provenance.md`.

## Coordinates

Map cells are `(x, z)` like RimWorld (x east, z north). World space: one cell
= 32 units, cell `(x, z)` covers `[x*32, (x+1)*32) × [z*32, (z+1)*32)`.
