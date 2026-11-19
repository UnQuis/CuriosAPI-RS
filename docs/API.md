# CuriosAPI-RS — API для сторонних модов

Практическое руководство по публичному API `mod_curios` (Rust-порт Curios для Horizon,
API 0.2, MC 26.3). Все сигнатуры ниже сверены с исходниками репо; путь к файлу указан
для каждого раздела. Пример готового мода — `examples/curio_example/`.

Адаптация интерфейсов TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018–2024 C4);
копии лицензий — `COPYING` и `COPYING.LESSER`.

## 0. Подключение

```toml
[dependencies]
mod_api = { package = "horizon-mod-api", path = ".horizon/sdk/api/crates/horizon-mod-api" }
mod_curios = { package = "mod-curios", path = "../CuriosAPI" }
```

В своём `#[mod_api::horizon_mod] impl Hooks` registration и события declare один раз:

```rust
use std::sync::Arc;
use mod_api::{Hooks, Registrar, RegistrationContext};

#[derive(Default)]
struct MyMod;

#[mod_api::horizon_mod]
impl Hooks for MyMod {
    fn register(&self, _r: &mut dyn Registrar, _cx: &RegistrationContext) {
        // register_curio / events::register_* — здесь.
    }
}
```

Важно: реестры Curio (`curio.rs`, `events.rs`) process-wide, поэтому регистрацию нужно
вызывать один раз на загрузке мода; дубликаты отвергаются явно.

---

## 1. Регистрация курио-предмета

`src/api.rs` реэкспортирует из `src/curio.rs`:

```rust
pub fn register_curio(
    item_id: impl Into<String>,
    behavior: Arc<dyn CurioBehavior>,
) -> Result<(), RegisterError>;

pub enum RegisterError {
    InvalidId(String),   // не `namespace:path`
    Duplicate(String),   // уже зарегистрирован
    Poisoned,            // отравлена внутренняя блокировка
}

pub struct CurioContext<'a> {
    pub slot: &'a str,   // например "curios:ring"
    pub index: u32,      // индекс внутри типа слота
    pub item: &'a str,   // канонический id предмета
}
```

Трейт (все методы имеют реализации по умолчанию):

```rust
pub trait CurioBehavior: Send + Sync + 'static {
    fn supported_slots(&self) -> &[String];          // [] = все слоты
    fn can_equip(&self, ctx: &CurioContext<'_>) -> bool;      // default true
    fn can_unequip(&self, ctx: &CurioContext<'_>) -> bool;    // default true
    fn drop_rule(&self) -> DropRule;                  // default DropRule::Default
    fn on_equip(&self, ctx: &CurioContext<'_>);       // сервер, после успеха
    fn on_unequip(&self, ctx: &CurioContext<'_>);     // сервер, после успеха
}
```

Минимальный пример:

```rust
use std::sync::Arc;
use mod_curios::api::{register_curio, CurioBehavior, CurioContext, DropRule};

struct Amulet;
impl CurioBehavior for Amulet {
    fn drop_rule(&self) -> DropRule { DropRule::AlwaysKeep }
    fn can_equip(&self, ctx: &CurioContext<'_>) -> bool { ctx.slot != "curios:feet" }
}

if let Err(error) = register_curio("mymod:amulet", Arc::new(Amulet)) {
    eprintln!("[mymod] curio registration refused: {error}");
}
```

Идентификатор проверяется тем же правилом, что и игровой (`curio::valid_item_id`):
нижний регистр, цифры, `_ . -`, обязательный `namespace:path`.

---

## 2. События

`src/events.rs`. Подписчик — `pub type Listener<E> = Arc<dyn Fn(&mut E) + Send + Sync>;`.
Регистрация возвращает `Result<(), EventError>` (`EventError::Poisoned`). Порядок вызова —
порядок регистрации; последний слушатель, записавший поле, побеждает.

Точные сигнатуры регистрации:

```rust
pub fn register_can_equip(listener: Listener<CurioCanEquipEvent>) -> Result<(), EventError>;
pub fn register_can_unequip(listener: Listener<CurioCanUnequipEvent>) -> Result<(), EventError>;
pub fn register_change(listener: Listener<CurioChangeEvent>) -> Result<(), EventError>;
pub fn register_drops(listener: Listener<CurioDropsEvent>) -> Result<(), EventError>;
pub fn register_drop_rules(listener: Listener<DropRulesEvent>) -> Result<(), EventError>;
pub fn register_slot_modifiers_updated(
    listener: Listener<SlotModifiersUpdatedEvent>,
) -> Result<(), EventError>;
```

Поля событий (`src/events.rs`):

| Событие | Поля |
|---|---|
| `CurioCanEquipEvent` | `item_id: String`, `context: SlotContext`, `original: bool`, `result: TriState`, метод `resolve() -> bool` |
| `CurioCanUnequipEvent` | те же поля, `resolve() -> bool` |
| `CurioChangeEvent` | `entity: u32`, `slot: String`, `index: usize`, `previous: Option<ItemSnapshot>`, `current: Option<ItemSnapshot>`, `kind: ChangeKind` |
| `CurioDropsEvent` | `entity: u32`, `drops: Vec<ItemSnapshot>`, `cancelled: bool` |
| `DropRulesEvent` | `entity: u32`, `overrides: Vec<DropOverride>`; методы `add_override(item_id, rule)`, `drop_rules() -> DropRules<DropOverride>` |
| `SlotModifiersUpdatedEvent` | `entity: u32`, `slots: Vec<String>` |

Пример:

```rust
use std::sync::Arc;
use mod_curios::api::TriState;
use mod_curios::events::{self, CurioCanEquipEvent, CurioDropsEvent};

events::register_can_equip(Arc::new(|event: &mut CurioCanEquipEvent| {
    if event.item_id == "mymod:amulet" {
        event.result = TriState::True; // форсировать надевание
    }
}))?;

events::register_drops(Arc::new(|event: &mut CurioDropsEvent| {
    if event.drops.is_empty() {
        event.cancelled = true; // не рассылать пустую пачку
    }
}))?;
```

`TriState::True` / `False` / `Default` перекрывают серверный результат;
`event.resolve()` возвращает финальное значение после `result.apply(original)`.
Для движка предусмотрены хелперы `events::can_equip_override(&mut CurioCanEquipEvent) -> TriState`
и `can_unequip_override(..)`, которые запускают диспетчер и возвращают `result`.

---

## 3. Чтение инвентаря

`src/api.rs` + `src/inventory.rs`.

```rust
pub struct SlotResult {
    pub slot: String,        // "ring"
    pub index: usize,
    pub item: ItemSnapshot,
}
pub fn find_first(inventory: &CurioInventory, item_id: &str) -> Option<SlotResult>;
pub fn slot_count(slot: &str) -> Option<usize>;   // размер «свежего» инвентаря
```

Хранение — attachment `curios:inventory` на entity (`Holder::Entity(entity_id)`):

```rust
pub static INVENTORY: AttachmentType<CurioInventory> = AttachmentType { /* ... */ };

// чтение на сервере (attachment store требует &mut dyn ServerHost):
let holder = mod_api::Holder::Entity(entity_id);
let inventory = match mod_curios::inventory::INVENTORY.get(host, holder) {
    Ok(inventory) => inventory,
    Err(error) => { eprintln!("[mymod] curio inventory: {error:?}"); return; }
};
if let Some(found) = mod_curios::api::find_first(&inventory, "mymod:amulet") {
    // found.slot, found.index, found.item
}
```

`ItemSnapshot` — NBT-escape стека: `item_id() -> Option<String>`,
`as_nbt() -> &Compound`, `from_stack(&dyn Host, &ItemStack) -> Result<Self, InventoryError>`,
`to_stack(&dyn Host) -> Result<ItemStack, InventoryError>`.

`CurioInventory` предоставляет `slot(id) -> Option<&SlotStacks>`, `slot_ids()`,
`find_first(..)`, `invalid_stacks()`. `SlotStacks` — `len()`, `stack(i)`, `cosmetic(i)`,
`render(i)`, `active(i)`, `filled()`.

---

## 4. Добавление слотов

`src/slots.rs`, `src/registry.rs`. Слоты — данные, не код: JSON-определение декодируется
строго, затем installing-снапшот заменяется целиком.

```rust
// src/slots.rs
pub struct SlotDefinition { /* pub fields: order, size, operation, use_native_gui,
    add_cosmetic, icon, drop_rule, render_toggle, conditions, validators, entities, replace */ }
impl SlotDefinition {
    pub fn from_json(json: &str) -> Result<Self, SlotError>;
}
impl SlotType {
    pub fn new(id: &str) -> Result<Self, SlotError>;
    pub fn from_definition(id: &str, definition: &SlotDefinition) -> Result<Self, SlotError>;
    pub fn from_json(id: &str, json: &str) -> Result<Self, SlotError>;
    pub fn with_size(self, size: i32) -> Self; // и with_order/with_icon/with_drop_rule
}

// src/registry.rs
pub fn builtin_slot_types() -> Result<Vec<SlotType>, SlotError>;
pub fn slot_types() -> Arc<Vec<SlotType>>;
pub fn replace_slot_types(types: Vec<SlotType>) -> Result<(), SlotError>;
pub fn slot_type_index(id: &str) -> Option<u16>;
pub fn slot_type_at(index: u16) -> Option<SlotType>;
```

Пример добавления слота «pocket» поверх встроенных:

```rust
use mod_curios::registry::{builtin_slot_types, replace_slot_types};
use mod_curios::slots::SlotType;

let mut types = builtin_slot_types()?;           // 11 встроенных определений
types.push(SlotType::from_json(
    "pocket",
    r#"{"size": 2, "order": 210, "add_cosmetic": true}"#,
)?);
replace_slot_types(types)?;                      // пустой реестр отвергается
```

После замены реестра вызовите `inventory.reload(&types)` для каждого онлайн-игрока и
запишите результат обратно через `INVENTORY.set`, иначе новые слоты появятся только у
созданных заново entity. `/curios reload` делает именно это.

---

## 5. DropRule и TriState

`src/drop.rs`, `src/equip.rs`.

```rust
pub enum DropRule { Default, AlwaysDrop, AlwaysKeep, Destroy }
impl DropRule { pub const fn as_str(self) -> &'static str; }

pub enum TriState { Default, True, False }
impl TriState { pub const fn apply(self, original: bool) -> bool; }

pub fn can_equip(slot: &SlotType, context: &SlotContext, item_id: &str, event: TriState) -> bool;
pub fn can_unequip(context: &SlotContext, item_id: &str, event: TriState) -> bool;
pub fn is_stack_valid(slot: &SlotType, context: &SlotContext, item_id: &str) -> bool;
pub fn decide_drop<I, P>(rules: &DropRules<P>, item: &I, item_rule: DropRule,
    slot_rule: DropRule, keep_inventory: bool, drop_prevented: bool) -> DropDecision;
```

Приоритет дропа: событийные override'ы → правило самого предмета → правило слота →
`keep_inventory`. `DropRulesEvent::add_override(id, DropRule::AlwaysKeep)` добавляет
правило с приоритетом по id предмета; `DropOverride` реализует `ItemPredicate<ItemSnapshot>`.

```rust
use mod_curios::api::{DropRule, TriState};
use mod_curios::events::{self, DropRulesEvent};

events::register_drop_rules(Arc::new(|event: &mut DropRulesEvent| {
    event.add_override("mymod:amulet", DropRule::AlwaysKeep);
}))?;

let allowed = mod_curios::equip::can_equip(slot, &context, "mymod:amulet", TriState::Default);
```

---

## 6. Сеть и команды (для интеграции)

`src/net.rs` declares 16 типизированных каналов `curios:*`. Все принимающие обработчики
установлены; аудит-тест проверяет покрытие всех 16 имён. Публичные хелперы для сервера:

```rust
pub fn register(r: &mut dyn Registrar);
pub fn sync_inventory(host: &mut dyn ServerHost, uuid: u128,
    inventory: &CurioInventory) -> Result<(), NetError>;
pub fn sync_stack(host: &mut dyn ServerHost, uuid: u128, curio_id: &str, slot_id: i32,
    stack: Option<&ItemSnapshot>, handler_type: u8, compound: &NetTag) -> Result<(), NetError>;
pub fn sync_render(..), sync_active(..), sync_data(..), sync_modifiers(..),
    break_effect(..), server_page(..), quick_move(..), grabbed_item(..);
pub fn handle_client(request: ClientPacket, ctx: &mut dyn IPayloadContext) -> Result<(), String>;
pub fn handle_server(packet: ServerPacket) -> Result<(), String>;
pub fn client_snapshot() -> ClientState;
```

`src/commands.rs` публикует `register(&mut RegisterCommandsEvent)`, `run(&mut dyn ServerHost,
&RunModCommandEvent) -> Flow`, `is_ours(..)` и `reload(&mut dyn ServerHost) -> Result<(), String>`.
Команды: `list`, `set`, `add`, `remove`, `clear`, `replace`, плюс `slots`, `reload`, `open`, `help`.

---

## 7. Ограничения, о которых честно говорят

- Клиент не может изменять инвентарь. `handle_client` проверяет все индексы по серверному
  состоянию (`slot_index`), а мутации делает только сервер.
- Атрибуты добавляются через `EquipmentSlotGroup::Any`; произвольные Curios-слоты как
  группы атрибутов в Horizon 0.2 отсутствуют.
- Mod-экран Horizon не содержит реальных слотов. GUI строится на ванильном меню
  (`minecraft:generic_9xN`) + `ModContainer`; `open` лишь отправляет snapshot/page.
- Data maps не синхронизируются клиенту: сервер шлёт связанное состояние через attachment sync
  или `net::sync_*`.
- Все операции с инвентарем fallible: никаких `unwrap_or_default()` в игровом коде.
