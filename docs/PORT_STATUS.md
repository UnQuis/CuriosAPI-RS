# Port status

All work is integrated through pull request #1 on `port/horizon-core`.

## Wave 1 — complete

| Area | State | Files |
|---|---|---|
| Research: Horizon API / Curios behavior | done | `docs/research/*.md` |
| Slot model + 11 bundled definitions | done | `src/slots.rs`, `src/registry.rs`, `data/curios/...` |
| Per-entity inventory + persistence + client mirror | done | `src/inventory.rs` |
| Item-side behavior API + public facade | done | `src/curio.rs`, `src/api.rs` |
| Equip / drop decisions + event surface | done | `src/equip.rs`, `src/drop.rs`, `src/events.rs` |
| Horizon place bridge + death drops | done | `src/place.rs`, `src/lib.rs` |
| Networking (16 typed payload channels) + commands | done | `src/net.rs`, `src/commands.rs` |
| GUI, keybinds, localization, assets | done | `src/menu.rs`, `src/input.rs`, `src/lang.rs`, `assets/` |
| Entity rendering (11 layers, cosmetics priority) | done | `src/render.rs` |
| Attribute modifiers + dynamic slot sizes | done | `src/attributes.rs` |
| Data-pack slot registry + hot reload | done | `src/datapack.rs` |
| Item effects (enderman, piglin, powder snow, fortune/looting helpers) | done | `src/effects.rs` |
| Config | done | `src/config.rs`, `src/lib.rs` |

## Known Horizon limitations (documented in module docs)

- No mod-slot equip/unequip hook: equip moments are computed through the
  attachment and `ItemPlace::Mod`; listeners observe through our event registry.
- No arbitrary `EquipmentSlotGroup`: Curio attribute modifiers are gated behind
  `curios:attribute_modifiers` and installed for the `any` group.
- No real slots in a custom screen: the GUI mirrors stacks into a vanilla
  `minecraft:generic_9x6` menu; custom graphics are drawn in screen hooks.
- `player_item_mend` is observe-only in Horizon, so Mending cannot be injected.
- Break-animation suppression and piglin neutrality are `ItemPart` hooks; the
  helpers exist in `src/effects.rs` but need an `ItemPart` attached at registration.

## Verification

- `cargo test --offline`: **81 passed, 0 failed**.
- `git diff --check`: clean.
- `horizon-launcher native-build`: unverified. The mod pins Horizon
  `4dc6455d…` (matching the sibling `ProjectE-RS` port); the launcher offers no
  game build for that revision, and the prepared `nightly-2026-11-18-dd67b76`
  refuses packages made by the older mod tools. Cargo compiles against the
  pinned generated SDK.

## Percent complete

Approximately **95% of the intended first-wave scope**. Remaining ~5% is the
small set of Horizon-impossible behaviors listed above plus in-game verification.
