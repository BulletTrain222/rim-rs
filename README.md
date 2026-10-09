# rim-rs

Open-source Rust and Bevy recreation of RimWorld's colony simulation, using
data from your own game install.

A from-scratch reimplementation of **RimWorld's gameplay behavior** in
**Rust**, using the **Bevy** engine.

rim-rs aims for behavioral compatibility with RimWorld: the same rules,
timings, decisions and interactions where they have been implemented.

**Important**

rim-rs is an independent project. It is **not** a decompiled copy of
RimWorld, **not** a distribution of RimWorld, and **not** an automated
C# → Rust conversion.

The project contains no RimWorld game files. It loads supported data and
textures at runtime from **your own RimWorld installation**, so a legally
obtained copy of RimWorld is required.

> RimWorld is a trademark of Ludeon Studios. rim-rs is not affiliated
> with or endorsed by Ludeon Studios.

---

## Project status

rim-rs is **experimental and incomplete**. You can start a colony and play a
small loop (build, farm, cook, research, haul, mine, hunt, fight, save and
load), but large parts of RimWorld are still missing. Full RimWorld
compatibility is **not** claimed.

| | |
|---|---|
| Reference RimWorld version | **1.6.4871 rev591** (other versions start with a warning) |
| Content | Base game only; DLC and mods are ignored |
| Platforms | Windows (tested); Linux and macOS should build but are untested |
| Engine | Rust (stable) + Bevy 0.19 |

**Working today (in outline):** RimWorld Def loading with inheritance;
textures decoded from your install at runtime; RimWorld-style map generation
(terrain, mountains, ore, plants, wild animals); pathfinding, think trees and
jobs; food, rest and timetables, partial recreation and mood; work
priorities and most basic work (hauling, construction, mining, farming,
cooking, butchering, research, cleaning, doctoring); health, temperature,
lighting and power; melee and ranged combat; hunting and predators;
save/load; and a RimWorld-style interface (colonist bar, selection, inspect
pane, gizmos, right-click orders, Architect, Work and Schedule tabs).

**Not there yet:** storyteller and raids, world map, quests, trade, most of
mood and mental breaks, many buildings and items, inspect tabs and most main
tabs, DLC and mods.

See [docs/status.md](docs/status.md) for the detailed list.

---

## How it works

- **Original game data at runtime.**
  rim-rs reads supported Defs, configuration and textures directly from
  the user's RimWorld installation. The original installation is not modified.

- **Behavior independently implemented in Rust.**
  Game systems are written from scratch in Rust. RimWorld itself is used as
  the behavioral reference rather than copying its implementation.

- **Checked against the original.**
  Compatibility work uses RimWorld's data, controlled observations and
  targeted research to determine rules, timings, ordering and edge cases.
  rim-rs then implements those behaviors independently.

- **Simulation separate from presentation.**
  The deterministic simulation lives in `rimworld_sim` and does not depend on
  Bevy. The Bevy application handles rendering, input and interface behavior.

- **User-owned assets stay outside the repository.**
  RimWorld XML, textures, binaries, decompiler output and diagnostic artifacts
  are not distributed with rim-rs.

---

## Getting started

### 1. Install the prerequisites

- **RimWorld**, legally obtained (Steam, GOG or another store). rim-rs never
  modifies it.
- **Rust (stable)** from [rustup.rs](https://rustup.rs).
- **Windows:** Visual Studio Build Tools with the "Desktop development with
  C++" workload (the MSVC linker).
- **Linux:** Bevy's system packages, for example on Debian/Ubuntu:

  ```sh
  sudo apt install libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev
  ```

### 2. Get the code

```sh
git clone https://github.com/BulletTrain222/rim-rs.git
cd rim-rs
```

### 3. Tell rim-rs where RimWorld is installed

The folder you need is the one that contains `Version.txt` and `Data/Core`.
Typical locations:

| Platform | Default Steam location |
|---|---|
| Windows | `C:\Program Files (x86)\Steam\steamapps\common\RimWorld` |
| Linux | `~/.local/share/Steam/steamapps/common/RimWorld` |
| macOS | `~/Library/Application Support/Steam/steamapps/common/RimWorld/RimWorldMac.app` |

Use **one** of these methods (checked in this order):

1. **Command line:** `--rimworld "<path to RimWorld>"`
2. **Environment variable:** `RIMWORLD_PATH="<path to RimWorld>"`
3. **Config file:** copy `config.example.toml` to `config.toml` and set the
   path (this file is ignored by git, so your local path is never committed):

   ```toml
   [rimworld]
   path = "C:/Program Files (x86)/Steam/steamapps/common/RimWorld"
   ```

4. **Nothing:** the usual Steam locations are tried automatically.

### 4. Check that your install is found

```sh
cargo run --release -p rimworld_app -- --check
```

This loads the game's Defs, prints a summary (number of Defs, the generated
map, the colonists) and exits without opening a window. It should report
`no load warnings`. Add `--rimworld "<path>"` if you didn't use the config
file or environment variable.

The first build compiles Bevy and takes several minutes; later builds are
fast.

### 5. Run it

```sh
cargo run --release -p rimworld_app
```

A window opens with three colonists on a freshly generated map. Use
`--release`: debug builds are much slower.

---

## Playing

The interface follows RimWorld's: the colonist bar along the top, the inspect
pane and the selection's commands (gizmos) at the bottom left, and the main
buttons along the bottom.

| Action | Control |
|---|---|
| Select | Left click (again: the next thing under the cursor); `Shift` adds/removes; drag for a box |
| Jump to a colonist | Double-click their portrait in the colonist bar |
| Orders (move, attack, equip, prioritize work) | Right click with colonists selected |
| Architect (build, zones, orders) | `Tab`, the Architect button, or right click with nothing selected |
| Work tab | `F1` |
| Schedule tab | `F2` |
| Draft / undraft | `R` (gizmo) |
| Rotate while placing | `Q` / `E` |
| Put a tool down / close a window | Right click or `Esc` |
| Pause / speed | `Space` / `1`–`4` |
| Pan / zoom | `WASD` or arrow keys, middle-drag / mouse wheel |
| Debug panel with every shortcut | `Ctrl+F1` |
| Quick save / quick load | `Ctrl+F5` / `Ctrl+F9` (saved to `saves/quicksave.json`) |

A first colony: open the Architect, place a **stockpile zone** next to the
colonists, use **Structure → Wall** and drag out a room (pick a material when
asked), add a **Door** (also under Structure) and **Beds** from Furniture,
and mark a **growing zone**. Colonists haul, build and farm on their own; the Work tab (`F1`)
decides who does what.

### Command-line options

| Option | Meaning |
|---|---|
| `--rimworld <path>` | RimWorld install folder (overrides `RIMWORLD_PATH` and `config.toml`) |
| `--config <file>` | Config file to read (default `./config.toml`) |
| `--check` | Load the Defs, print a summary and exit |
| `--seed <n>` | Map seed (default 1) |
| `--pawns <n>` | Number of colonists (default 3) |
| `--showcase` | A staged demo colony (built room, research, power, farm, blueprints) |
| `--colony` / `--farm` | Start with a growing zone (and blueprints for a bedroom) |
| `--speed <n>` | Initial speed in ticks per step (1, 3, 6, 15) |
| `--zoom <scale>` / `--focus x,z` | Initial camera zoom / focus cell |
| `--play <file>` | Run a script of key presses and clicks (see `crates/rimworld_app/src/script.rs`) |
| `--help` | List every option |

---

## Running the tests

```sh
cargo test --workspace
```

- Tests that need a RimWorld installation **skip themselves** when none is
  configured. Set `RIMWORLD_PATH` (or create `config.toml`) to run them too.
- Tests that replay recordings of the original game skip when the
  recordings are missing; the recordings are not part of this repository.

Before sending changes, also run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Troubleshooting

- **"no RimWorld installation configured", "does not exist" or "is not a
  RimWorld installation".** Point rim-rs at the folder that contains
  `Version.txt` and `Data/Core` (not the `.exe`, not `Data`). The error
  message suggests a corrected path when it can guess one.
- **"RimWorld x has not been tested".** rim-rs continues anyway; most data
  is stable between versions, but behavior is checked against 1.6.4871
  rev591.
- **Build errors about `alsa`, `udev`, `wayland` or `xkbcommon` (Linux).**
  Install the system packages listed above.
- **Linker errors on Windows.** Install the Visual Studio Build Tools (C++
  workload) and reopen your terminal.
- **The game runs slowly.** Use `--release`.
- **Plain coloured squares instead of textures.** The textures in your
  install could not be read; rim-rs falls back to debug colours. Check the
  install path and that the game files are intact.

---

## Repository layout

```text
rim-rs/
├── Cargo.toml
├── crates/
│   ├── rimworld_assets/   # install discovery, config, version, texture loading
│   ├── rimworld_defs/     # XML Def loading, inheritance and typed Def database
│   ├── rimworld_sim/      # deterministic, engine-independent game simulation
│   └── rimworld_app/      # Bevy renderer, input and interface
├── docs/                  # architecture, compatibility, status and provenance
├── tools/                 # asset guards and Git hooks
├── config.example.toml
├── LICENSE-MIT
├── LICENSE-APACHE
└── THIRD_PARTY_NOTICES.md
```

## Documentation

- [docs/status.md](docs/status.md) — what is implemented and the main gaps
- [docs/architecture.md](docs/architecture.md) — how the crates fit together
- [docs/compatibility.md](docs/compatibility.md) — reference version and
  version policy
- [docs/provenance.md](docs/provenance.md) — what may be committed and the
  resources used

Code comments cite sections of the project's compatibility research notes
(`docs/research.md`); those notes are not published yet.

## Contributing

Contributions are welcome. To keep the repository publishable:

- **Never commit anything from a RimWorld installation** (XML, textures,
  sounds, binaries), decompiler output or dumps. Enable the guards once per
  clone:

  ```sh
  git config core.hooksPath tools/git-hooks
  ```

- Implement behavior yourself in Rust and describe findings in your own
  words. Mark known approximations with
  `// COMPATIBILITY TODO: currently approximate`.
- Keep commits small and run the tests, `cargo fmt` and `cargo clippy` first.

## License

rim-rs's original code is dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for third-party material.

RimWorld and all of its content remain the property of Ludeon Studios.
