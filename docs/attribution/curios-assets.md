# Curios assets (adapted)

All files in this directory (language JSON, GUI and slot textures, `pack.mcmeta`,
`curios_icon.png`) were copied verbatim from the Curios Java reference at
`/mnt/Barracuda/tmp/opencode/curios-reference/src/main/resources/assets/curios/`
as part of the CuriosAPI-RS port.

Adapted from **Curios** by TheIllusiveC4, copyright (c) 2018-2024 C4,
licensed **LGPL-3.0-or-later**. See the repository root `COPYING` and
`COPYING.LESSER` for the full license text.

Some translation keys used by the Rust screen (`curios.tooltip.inactive`,
`gui.curios.toggle.cosmetics`, `gui.curios.page`, `curios.networking.failed`
and `curios.identifier.feet`) are absent from several upstream language
files. Those entries were filled from the English reference values; `ru_ru`
received Russian text. No upstream translation was overwritten.

The upstream `textures/gui/container/sprites/` set has no `empty_feet_slot.png`;
it was copied from the upstream `textures/slot/empty_feet_slot.png` so all
eleven built-in slot types have a GUI sprite. No new artwork was authored.
