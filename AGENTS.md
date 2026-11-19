# AGENTS.md — CuriosAPI-RS contributor guide

## Project identity

CuriosAPI-RS is a Rust implementation of the Curios accessory-slot API for Minecraft 26.3 on the
Horizon native mod API. The upstream Java reference is `TheIllusiveC4/Curios` (LGPL-3.0-or-later),
cloned read-only at `/mnt/Barracuda/tmp/opencode/curios-reference`. It may be used as a
behavioral reference; preserve LGPL attribution for any adapted code, data or assets.

This repository is a porting workspace: the first PR (`port/horizon-core`) establishes the API
surface, slot model and per-entity inventory. Gameplay parity with the Java mod is the goal, but
explicit limitations in PR descriptions are preferred over false claims.

## Hard rules

1. **Reference is read-only.** Never modify `/mnt/Barracuda/tmp/opencode/curios-reference`.
2. **Pin compatibility.** Horizon revision, mixins revision and game version are declared in
   `horizon-mod.toml` and mirrored in `compatibility.json`. Do not silently target Horizon `main`,
   a newer SDK or a different Minecraft version.
3. **Do not invent Horizon APIs.** Verify type names, hook names, signatures, ownership and
   side constraints against the pinned generated SDK in `.horizon/sdk/api/` (gitignored).
   Install it from the pinned SDK release if missing; never hand-write `native-hooks.json`.
4. **Server authority.** Equip/unequip decisions, inventory mutations, attribute modifiers and
   drop rules are server-authoritative. Clients may display and request, never grant.
5. **Fail explicitly.** No `unwrap_or_default()`, `let _ = ...` or silent fallbacks in production
   paths. Handle storage, serialization and parse errors with actionable diagnostics.
6. **Stay in your file scope.** Each agent owns the files listed in its assignment. Do not edit
   another agent's files; report needed changes to the orchestrator (Утра) via mesh instead.
7. **No false verification.** Report the exact commands run and their results. Static inspection
   is not a build or a test.
8. **Attribution.** Keep `COPYING` / `COPYING.LESSER` and note adapted material in file headers.

## Layout

```
Cargo.toml, src/                  native crate at repo root (launcher requirement)
horizon-mod.toml, compatibility.json   pinned Horizon/game compatibility
.horizon/sdk/api/                 generated SDK, gitignored, installed locally
docs/research/                    read-only research notes from the scouts
src/lib.rs                        the single #[horizon_mod] Hooks impl + orchestration
src/slots.rs                      slot type model, predicates, data-driven definitions
src/inventory.rs                  per-entity curio inventory, persistence, sync
src/curio.rs                      item-side curio behavior registration + item part
src/equip.rs                      equip/unequip/drop rules and event dispatch
data/curios/                      ported slot definitions (datapack resources)
```

The module list is the intended boundary; not every module exists on day one.

## Build and verification

```bash
cargo test                          # unit tests
horizon-launcher native-build .     # when the launcher environment is available
git diff --check && git status --short
```

`mod_api` is a path dependency into the gitignored `.horizon/sdk/api/`. Restore it from the
pinned `api-source.tar.xz` release asset of the pinned SDK before running Cargo.

## Collaboration

All work for the first PR lives on `port/horizon-core` targeting `main`. Keep focused commits,
avoid force-push, and report progress/blockers to the orchestrator over mesh.
