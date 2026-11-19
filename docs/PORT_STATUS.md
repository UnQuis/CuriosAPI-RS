# Port status

All work is integrated through pull request #1 on `port/horizon-core`.

## Waves 1–3 — complete

| Wave | Scope | State |
|---|---|---|
| 1 | Slot model, inventory, item API, equip/drop, place bridge, network, commands, GUI, rendering, attributes, events, data packs, effects | done |
| 2 | Runtime equip events, full Java command set, `ItemPart`, dynamic slot sizes, all 16 payload handlers, locales, SDK pin `dd67b76` | done |
| 3 | API guide + example, custom screen rendering, in-game gametests, data-pack entry files, safe enchantment behavior, robustness tests | done |

### Wave 3 additions

- `docs/API.md` and `docs/examples/curio_example.rs.txt` document the public API
  for other mods (`register_curio`, event listeners, inventory reads, drop rules).
- `src/gametests.rs` registers four in-game test functions and definitions
  (`curios:registry`, `curios:inventory_codec`, `curios:place_keys`, `curios:drop_rules`)
  using the vanilla empty structure template.
- `src/menu.rs` now draws a true custom Curios screen: all 11 slot icons,
  cosmetic pairs, hover, page controls, instrumented tooltips and server-side
  state mirroring through a dedicated `curios:screen_state` payload. Equips
  outside the menu also refresh the screen through `place::notify_change`.
- `data/curios/curios/slot_type/*.json`: all 11 flat data-pack entries, decoded
  back into the same slot types as the bundled definitions.
- Robustness: property tests for NBT codec truncation, all place-key boundaries
  and the full drop-precedence tree; command and malformed-payload edge cases.

## Objective Horizon limitations (pin `dd67b76`)

1. `data_pack_files` cannot deliver datapack-registry entries (closed kind set).
2. `get_enchantment_level` has no wearer, so wearer-scoped Fortune/Looting
   bonuses cannot be applied safely; the helper is intentionally a no-op in the
   global hook.
3. `EquipmentSlotGroup` is a fixed enum; only the `any` group is available for
   Curio modifiers.
4. There is no `on_unequip` hook; it is synthesized in `PlaceProvider::extract`.
5. No equip-sound hook; sound comes only from `set_equipment`.
6. Powder-snow behavior is approximated through `living_breathe`.
7. `player_item_mend` is observe-only; Mending cannot be injected.
8. Break/reequip animation suppression is an `ItemPart` client hook.
9. Datapack fields are flat (Bool/Int/Float/Text/Identifier/Reference); nested
   validator lists are compressed into a single identifier.
10. Data maps are not synced to clients; attachment + `sync_stack` is used.
11. There are no typed data-component types for mods; string ids and NBT are used.

## Verification

- `cargo test --offline`: **118 passed, 0 failed**.
- `git diff --check`: clean.
- `horizon-launcher native-build --version nightly-2026-11-18-dd67b76 .`:
  **success** — `native-dd67b76-63471837394f` built and installed.
