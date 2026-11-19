# CuriosAPI-RS

A Rust port of [Curios](https://github.com/TheIllusiveC4/Curios) to the
[Horizon](https://github.com/horizon-rust/horizon) native mod API for Minecraft 26.3.

All work is integrated through pull request #1 on the `port/horizon-core` branch.

## What is implemented

- Data-driven slot types (`src/slots.rs`) with all 11 bundled definitions and
  Java-compatible defaults and ordering.
- Per-entity curio inventory (`src/inventory.rs`) persisted as a strict NBT
  attachment, mirrored to clients, with cosmetic slots and render/active flags.
- Item-side behavior registry (`src/curio.rs`) and a small public facade for
  other mods (`src/api.rs`).
- Equip/drop decision logic (`src/equip.rs`, `src/drop.rs`), including death drops.
- Horizon integration (`src/place.rs`): accessory slots are exposed as
  `ItemPlace::Mod` through a `PlaceProvider`; the engine moves items transactionally.
- Settings, commands, research notes and a living checklist
  (`docs/PORT_STATUS.md`).

Further modules (networking, GUI, rendering, attributes, events, data-pack reload,
item effects) are being added in the completion stage of the first wave.

## Build and test

```bash
cargo test --offline
horizon-launcher native-build .   # needs the game build matching the pinned SDK
```

Compatibility is pinned in `horizon-mod.toml` and `compatibility.json`. The
`mod_api` dependency is a path dependency into the gitignored `.horizon/sdk/api/`.

## License

Adapted from Curios by TheIllusiveC4, LGPL-3.0-or-later, copyright 2018-2024 C4.
See `COPYING` and `COPYING.LESSER`.
