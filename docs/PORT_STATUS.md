# Port status

All work is integrated through pull request #1 on `port/horizon-core`.

## Waves 1–3 — complete

Slot model, inventory, item API, equip/drop, place bridge, network, commands,
GUI, rendering, attributes, events, data packs, effects, API guide, gametests,
custom screen rendering and data-pack entries.

## Wave 4 — complete (review fixes)

An independent review found 24 issues; all are fixed and covered by
`src/review_regressions.rs` (23 tests by finding number).

### Critical

- **#1** Slot validity now uses real item tags through `Tags::key/is_in_tag`
  (`equip::is_stack_valid_host`), not string comparisons.
- **#2** GUI clicks perform a real server transaction (`ItemPlace::MenuSlot` ↔
  `curio_place`) with validation and mirror refresh.
- **#3** Invalid stacks from reload are retained and dropped by `server_tick`
  instead of vanishing; no `let _invalid`.
- **#4** `keep_curios=default` follows the `keep_inventory` game rule; On/Off
  remain forced.
- **#5** `DropOutcome::Destroy` clears the slot without spawning a drop entity.
- **#6** Slot sizes and modifier amounts are capped; 1e30 is explicitly refused.

### Major

- **#7** `CurioDropsEvent` is dispatched in `living_death` and can cancel/edit drops.
- **#8** `/curios reload` goes through the data-pack registry.
- **#9** `/curios set|add|remove` apply permanent `ADD_VALUE` size modifiers
  that survive reload.
- **#10** GUI state is per-player (`BTreeMap<u128, PlayerMenu>`).
- **#11** Negative menu slots and foreign windows are rejected.
- **#12** Locale comes from `ClientSession::client_language()`.
- **#13** Screen buttons queue actions drained in `key_mapping_pre`.
- **#14** Failed data-pack reloads re-arm the pending flag; dimension comes
  from the server.

### Minor

- **#15** Render sync targets the actionable player only.
- **#16** Unknown items are refused instead of `unwrap_or(64)`.
- **#17** Per-frame `Box::leak` replaced by a one-time cached map.
- **#18** `for_slot_context` uses Java `all` semantics.
- **#19** Effect cache covers mobs and logs misses explicitly.
- **#20** Namespaced slot ids are kept distinct (`curios:ring` vs `mymod:ring`).
- **#21** Disabling cosmetics marks old cosmetic stacks invalid.
- **#22** Offline inventories are migrated on player join.
- **#23** Modifier-updated events fire only when the size actually changed.
- **#24** Dead APIs are wired or documented (`enable_button`, clamped pages,
  real cosmetic toggle, grabbed-item application).

## Verification

- `cargo test --offline`: **153 passed, 0 failed** (130 lib + 23 regression).
- `git diff --check`: clean.
- `horizon-launcher native-build --version nightly-2026-11-18-dd67b76 .`:
  **success** — `native-dd67b76-0eb81ff40548` built and installed.
