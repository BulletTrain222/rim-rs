# Third-party notices

rim-rs's own code is licensed under MIT OR Apache-2.0 (see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE)). This file
lists material from other sources.

## libnoise gradient vector table

`crates/rimworld_sim/src/noise_vectors.rs` contains the 256 normalized
gradient vectors (1024 numbers) published in `vectortable.h` of libnoise by
Jason Bevins (http://libnoise.sourceforge.net/). That file states:

> Written by Jason Bevins. Actually it's the output of a program written by
> me. [...]
>
> This file is in the public domain.

The values are reproduced unchanged (checked value for value against the
original header). Only this table is used; no other libnoise code is
included.

## Cargo dependencies

Dependencies such as Bevy, roxmltree, serde, toml and thiserror are not
vendored. Cargo downloads them under their own licences (see each crate on
crates.io). All of them are available under permissive terms (MIT,
Apache-2.0, BSD, ISC, Zlib, BSL-1.0, CC0, Unlicense, Unicode-3.0); `r-efi`
offers LGPL-2.1 only as an alternative to MIT or Apache-2.0. Binaries built
from this repository include those crates and must carry their notices.

## RimWorld

RimWorld and its data, textures, sounds and other content are the property
of Ludeon Studios. None of it is included in this repository. rim-rs
reads the data it needs from the user's own installation at runtime.
