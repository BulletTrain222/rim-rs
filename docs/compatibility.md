# Compatibility

## Currently tested

| Item | Value |
|---|---|
| RimWorld version | **1.6.4871 rev591**. The install's `Version.txt` says rev590, but the executable reports rev591 (assembly version 1.6.9676.17735). All comparisons with the original game were made against this build. |
| Platform tested | Windows 11, Steam install |
| Content | Base game (`Data/Core`, packageId `Ludeon.RimWorld`) only |
| DLC | Ignored (Royalty, Ideology, Biotech, Anomaly, Odyssey). `MayRequire` nodes for them are dropped. |
| Mods | Ignored |

At startup the app reads `Version.txt`. A different version is **not an error**:
it logs a warning and continues, since most Def data is stable across builds.

## Design for multiple versions later

- `rimworld_assets::GameVersion` parses `Version.txt`; `SUPPORTED_VERSIONS`
  lists versions that have been tested.
- Content packs are discovered as a list (`Data/Core` today); DLC and mods will
  be additional packs with their own packageIds feeding the same loader.
- The Def loader is data-driven (generic XML tree + inheritance); typed views
  read fields by name and tolerate missing/extra fields, so version-specific
  fields do not require loader changes.
- Version-specific behaviour should be isolated behind small, explicit switches
  keyed on `GameVersion`, never scattered `if` checks.

## Feature compatibility

See [status.md](status.md) for what is implemented and the main gaps.
