# Port status

All work is integrated through pull request #1 on `port/horizon-core`.

## Wave 1 — complete

Slot model, inventory, item API, equip/drop decisions, Horizon place bridge,
network transport, commands, GUI, rendering, attributes, events, data packs and
item effects. See the PR description for the full list.

## Wave 2 — complete

| Area | Files | Result |
|---|---|---|
| Full Java command set (`list/set/add/remove/clear/replace`) + 16-channel payload audit | `src/commands.rs`, `src/net.rs` | done |
| GUI controls, tooltips, 19 locales, slot icons | `src/menu.rs`, `src/input.rs`, `src/lang.rs`, `assets/` | done |
| `ItemPart` for piglin neutrality, break animation, tooltips | `src/itempart.rs` | done |
| Equip-time validation, change events, behavior callbacks, dynamic slot sizes | `src/place.rs`, `src/equip.rs`, `src/events.rs`, `src/attributes.rs`, `src/inventory.rs` | done |
| Effect helpers (Fortune/Looting, Mending observe, cached wearer effects) | `src/effects.rs` | done |
| SDK/game pin updated to `dd67b76…` | `horizon-mod.toml`, `compatibility.json`, `Cargo.lock` | done |

### Runtime wiring added in wave 2

`PlaceProvider::insert/extract` now run `equip::can_equip` / `can_unequip`
server-side before mutating, dispatch `CurioChangeEvent` and
`SlotModifiersUpdatedEvent`, call `CurioBehavior::on_equip/on_unequip`, and send a
point-to-point `sync_stack`. Slot sizes are recalculated through
`SlotSizeState::resize` on data reload; removed stacks go to `invalid` and are
dropped on the next tick.

## Remaining Horizon limitations

- No native mod-slot equip hook; moments are computed in our own attachment/
  place bridge and exposed through our event registry.
- No arbitrary `EquipmentSlotGroup`: Curio modifiers are gated behind the
  `curios:attribute_modifiers` component and installed for the `any` group.
- No real slots in a custom screen: the GUI mirrors stacks into a vanilla
  `minecraft:generic_9x6` menu; custom graphics are drawn in screen hooks.
- `player_item_mend` is observe-only, so a Curio Mending bonus is reported but
  cannot be injected.
- `get_enchantment_level` receives a stack without its wearer, so the prepared
  `effects::get_enchantment_level` helper is not wired into a global hook yet.

## Verification

- `cargo test --offline`: **98 passed, 0 failed**.
- `git diff --check`: clean.
- `horizon-launcher native-build --version nightly-2026-11-18-dd67b76 .`:
  **success** — native mod `native-dd67b76-36a75a36398d` built and installed.
