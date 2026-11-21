# Curio tags

The tag files here declare the two tag keys the port relies on:

- `curios:curio` (`item/curio.json`) — the universal-item tag. An empty `values`
  list is intentional: the file only needs to exist so the engine registers the
  tag; other mods add their items through their own tag files.
- `curios:player_like` (`entity_type/player_like.json`) — entity types that get
  curio slots by default, matching `data/curios/tags/entity_type/player_like.json`
  in the Java reference.

Path convention verified against Horizon's own content tests
(`crates/horizon-client/src/game/functions.rs` and `mod_content_tests.rs`):
tags live under `data/<namespace>/tags/<registry>/<name>.json`, with
`entity_type` as the registry folder for entity types.

Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4);
see the repository root `COPYING` and `COPYING.LESSER`.
