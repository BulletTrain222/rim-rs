# Provenance

This file records the external resources the project uses and the rules that
keep the repository free of RimWorld content. **No code from outside projects
has been copied** (the one third-party table in the source is listed in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)).

## What may be committed

Allowed: original Rust source, build scripts, tests, our own documentation,
file-format notes in our own words, diagrams, notes describing observed
behaviour, credits/links, and small **synthetic** fixtures written by hand
for this project (XML fixture files must contain the marker comment
`synthetic-fixture`).

Never allowed: RimWorld executables, DLLs, XML/data copied from the game,
textures, audio, fonts, localisation files, DLC content, decompiled code,
decompiler output, assembly dumps, memory dumps, or any other copyrighted
game asset. Screenshots in `docs/images/` must be reviewed by hand.

Enforcement:

1. Whitelist `.gitignore`: everything is ignored unless re-included.
2. `tools/git-hooks/pre-commit` runs `tools/asset-guard/guard.sh staged`
   (path/extension/size/signature checks, plus byte-identity against your
   local game install if `guard.sh index <install>` was run).
3. `tools/git-hooks/pre-push` runs `guard.sh history`, which audits every
   blob in every reachable commit.
4. Enable the hooks once per clone: `git config core.hooksPath tools/git-hooks`.

Local-only directories (ignored, must never contain tracked files): `local/`
and anything below it.

## How behaviour is reproduced

rim-rs treats the game's own data files and its observed behaviour as
the specification. Behaviour is described in our own words and implemented
independently in Rust; nothing is mechanically translated, and no game code,
data or assets are included in the repository. At runtime the app reads the
data and textures it needs from the user's own installation.

## Resources

| Resource | URL | Author / project | Licence | Use |
|---|---|---|---|---|
| RimWorld (the user's installed copy) | — | Ludeon Studios | Proprietary EULA | Def data and textures read at runtime from the user's install; nothing redistributed |
| RimWorld Wiki — Modding Tutorials | https://rimworldwiki.com/wiki/Modding_Tutorials | RimWorld Wiki contributors | CC BY-SA 3.0 | Def structure and `Name`/`ParentName`/`Abstract` semantics; no content copied |
| Bevy | https://bevyengine.org | Bevy contributors | MIT OR Apache-2.0 | Engine (Cargo dependency) |
| roxmltree | https://github.com/RazrFalcon/roxmltree | Yevhenii Reizner | MIT OR Apache-2.0 | XML parser (Cargo dependency) |
| toml / serde | https://crates.io/crates/toml | toml-rs / serde contributors | MIT OR Apache-2.0 | Config parsing (Cargo dependencies) |
| Unity SerializedFile / Texture2D format (public format knowledge, e.g. as documented by AssetStudio and UnityPy) | https://github.com/Perfare/AssetStudio , https://github.com/K0lb3/UnityPy | Perfare / K0lb3 et al. | MIT | Format knowledge only; the reader is our own implementation |
| S3TC / BCn block compression | https://learn.microsoft.com/windows/win32/direct3d10/d3d10-graphics-programming-guide-resources-block-compression | Microsoft (public spec) | Documentation | BC1/BC3 block layout |
| libnoise `vectortable.h` | http://libnoise.sourceforge.net/ | Jason Bevins | Public domain (stated in the file) | The 256 gradient vectors of gradient noise; see THIRD_PARTY_NOTICES.md |
