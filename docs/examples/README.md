# curio_example

Минимальный мод, показывающий публичный API CuriosAPI-RS. Это исходник, а не отдельный
Cargo-проект: launcher Horizon должен находить ровно один crate-мод. Скопируйте
`src/lib.rs` в `src/lib.rs` своего мода и добавьте зависимости:

```toml
[dependencies]
mod_api = { package = "horizon-mod-api", path = ".horizon/sdk/api/crates/horizon-mod-api" }
mod_curios = { package = "mod-curios", path = "../CuriosAPI" }
```

Что показывает пример:

1. `api::register_curio("mymod:amulet", Arc::new(Amulet))` — регистрация поведения
   предмета; ошибки (неверный id, дубликат) обрабатываются явно.
2. `CurioBehavior` — `can_equip`, `can_unequip`, `drop_rule`, `on_equip`, `on_unequip`.
3. `events::register_can_equip`, `register_change`, `register_drops`, `register_drop_rules`
   — подписка на события; каждый listener — `Arc<dyn Fn(&mut E) + Send + Sync>`.
4. `INVENTORY.get(host, Holder::Entity(id))` и `api::find_first(&inventory, item_id)` —
   чтение инвентаря без доверия к клиенту.

Полные сигнатуры и остальные разделы API — в `docs/API.md`.

Лицензия адаптированных интерфейсов: LGPL-3.0-or-later, TheIllusiveC4/Curios
(copyright 2018–2024 C4); см. `COPYING` и `COPYING.LESSER` в корне репозитория.
