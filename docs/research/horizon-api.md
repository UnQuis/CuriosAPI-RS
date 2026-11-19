# Практическая карта Horizon API для порта accessory-слотов

Разведка закреплённого SDK, выполненная агентом #1 по заданию #1 (research only).
Ни один исходный файл проекта, кроме этого документа, не создан и не изменён.

**Пин (из задания и `AGENTS.md`):** Horizon `4dc6455d4a6773224792eb1f121a9b939dff3170`,
mixins `e59da9094f0c2fdda6c627fe9eff22dec6a906f0`, API 0.2, MC 26.3.

## 0. Как устроен SDK и где искать сигнатуры

Все пути ниже — от корня репо `CuriosAPI/`.

```
.horizon/sdk/api/crates/horizon-mod-vocab/     <-почти весь публичный API (lib name: mod_vocab, реэкспорт через mod_api::*)
  src/exports/*.rs                              <- «оглавление» pub use (по одному файлу на тему)
  generated/mod_api_exports_root.rs             <- ИТОГОВЫЙ pub use-список (37 КБ) — authoritative карта имён
  generated/mod_api_generated.rs                <- Hooks, ItemPart, BlockPart, Event-структуры, дисциплина хуков (1.4 МБ)
  generated/mod_api_generated_host.rs           <- Host/ServerHost/ClientHost, LevelRef, LivingRef и др. (366 КБ)
  generated/parts_modules.rs                    <- модули parts и составной trait Registrar
  src/parts/*.rs                                <-_registrar-методы (register/declare/attach_*) и store-таблицы
  src/host/*.rs                                 <- декларации host-методов (документация к сгенерированным')
  src/world/*.rs, src/item/*.rs, src/block/*.rs, src/client/*.rs — типы values
.horizon/sdk/api/crates/horizon-mod-runtime/
  src/attachments.rs                            <- AttachmentType / AttachmentStore / Attachments (всего 1405 строк)
.horizon/sdk/api/crates/horizon-mod-api/        <- тонкая обёртка: lib.rs + #[horizon_mod] макрос
```

`horizon-mod-api::lib.rs` делает `pub use mod_vocab::*;`, поэтому **все типы доступны как `mod_api::*`**.
Маршрут поиска сигнатуры: `generated/mod_api_exports_root.rs` → модуль в `src/exports/<тема>.rs`
→ исходник в `src/<...>/<...>.rs` или `generated/mod_api_generated{,_host}.rs`.

Единая точка входа мода (аналог `@Mod` + `FMLConstructModEvent`):

```rust
// generated/mod_api_generated.rs:1888, 1892
pub trait Hooks: std::any::Any + Send + Sync { /* ~... методов */ }
fn register(&self, r: &mut dyn Registrar, cx: &RegistrationContext) { ... }
```

```rust
// Generated/parts_modules.rs:83 — Registrar — это сумма sub-trait'ов:
pub trait Registrar: crate::parts::ArmPosesRegistrar + crate::parts::AttributesRegistrar
    + ... + crate::parts::DatapackRegistriesRegistrar + crate::parts::ItemsRegistrar
    + crate::parts::PayloadsRegistrar + crate::parts::SettingsRegistrar + ... {}
```

Рабочий шаблон: `../ProjectE-RS/src/lib.rs:60-125` — один `#[mod_api::horizon_mod] impl Hooks for ProjectE`
с `fn register(&self, r: &mut dyn Registrar, _cx: &RegistrationContext)`, затем `r.item_key(..)`,
`r.item_part(..)`, `r.add_payload(..)`, `r.menu_part(..)`.

---

## 1. Attachments (данные на entity)

**Ключевой вывод: отдельного `declare_attachment`/`declare_codec` в `Registrar` НЕТ.**
Attachment объявляется как `static AttachmentType<T>` с структурой codec; регистрации в
`Hooks::register` не требуется. Это подтверждено и рабочим портом: `../ProjectE-RS/src/data.rs:150-160`.

### 1.1 Объявление

```rust
// runtime/src/attachments.rs:357-372
pub struct AttachmentCodec<T> {
    pub encode: fn(&T) -> Result<Compound, AttachmentError>,
    pub decode: fn(&Compound) -> Result<T, AttachmentError>,
}
pub struct AttachmentType<T: 'static> {
    pub namespace: &'static str,
    pub name: &'static str,
    pub default: fn() -> T,
    pub codec: Option<AttachmentCodec<T>>,
    pub copy_on_death: bool,
    pub sync: bool,   // требует codec, иначе InvalidSyncPolicy
}
```

Пример (структура из ProjectE-RS):

```rust
pub static PLAYER: AttachmentType<PlayerData> = AttachmentType {
    namespace: "curios", name: "inventory",
    default: CurioInventory::default,
    codec: Some(AttachmentCodec { encode: encode_inventory, decode: decode_inventory }),
    copy_on_death: false, sync: true,
};
```

`copy_on_death: true` без codec → `AttachmentError::InvalidCopyPolicy`; `sync: true` без codec →
`InvalidSyncPolicy` (`runtime/src/attachments.rs:384-392`). Ключ — `(namespace, name)` в UTF-16,
данные сохраняются в NBT-компаунде `horizon:attachments` (`ATTACHMENTS_KEY`, строка 123).

### 1.2 Хранилище и holder

```rust
// runtime/src/attachments.rs:134-146
pub enum Holder { Entity(u32), Player(u128), BlockEntity([i32;3]), Chunk([i32;2]), Level,
                  OriginalPlayer(u64) }

// runtime/src/attachments.rs:208-254
pub trait AttachmentStore {
    fn holds(&mut self, holder: Holder, namespace: &str, name: &str) -> Result<bool, AttachmentError>;
    fn decoded(&mut self, holder: Holder, namespace: &str, name: &str) -> Result<Option<AttachmentValue>, AttachmentError>;
    fn saved(&mut self, holder: Holder, namespace: &str, name: &str) -> Result<Option<Compound>, AttachmentError>;
    fn store(&mut self, holder: Holder, namespace: &str, name: &str,
             value: AttachmentValue, form: AttachmentForm) -> Result<(), AttachmentError>;
    fn discard(&mut self, holder: Holder, namespace: &str, name: &str, sync: bool) -> Result<(), AttachmentError>;
    fn queue_sync(&mut self, holder: Holder, namespace: &str, name: &str) -> Result<(), AttachmentError>;
}
```

`AttachmentStore` реализован для любого `H: AttachmentHost + ?Sized` (строки 256-353).
Хост, получающий `&mut dyn ServerHost` / `&dyn Host` / `LevelRef`, **уже является store**: ProjectE
вызывает `PLAYER.get(host, Holder::Player(uuid))` напрямую (`data.rs:261,270`).

### 1.3 get / set / remove / sync — точные сигнатуры

```rust
// runtime/src/attachments.rs:403-526, impl<T: Clone + Send + Sync + 'static> AttachmentType<T>
pub fn get<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder) -> Result<T, AttachmentError>;
pub fn has<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder) -> Result<bool, AttachmentError>;
pub fn get_existing<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder) -> Result<Option<T>, AttachmentError>;
pub fn with_decoded<S, R>(&self, store: &mut S, holder: Holder,
                          read: impl FnOnce(Option<&T>) -> R) -> Result<R, AttachmentError>;
pub fn set<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder, value: T) -> Result<(), AttachmentError>;
pub fn remove<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder) -> Result<Option<T>, AttachmentError>;
pub fn sync_data<S: AttachmentStore + ?Sized>(&self, store: &mut S, holder: Holder) -> Result<(), AttachmentError>;
```

`get` вставляет default при отсутствии; `get_existing` — нет; `remove` возвращает старое значение
и публикует removal даже если декодирование не удалось. Все операции fallible — никаких
`unwrap_or_default()` (см. `AGENTS.md`, rule 5; образец обработки — `ProjectE-RS/src/data.rs:36-48, 102-126`).

### 1.4 Синхронизация клиенту

Два независимых механизма:

**(а) Автоматический sync attachment.** `AttachmentType { sync: true, codec: Some(..) }` +
`set`/`remove`/`sync_data` кладут ключ в `pending`; `Attachments::take_sync_changes() ->
Vec<AttachmentSync>`, `synced()`, `apply_sync(AttachmentSync)` (строки 1061-1100). Это движковый
путь — отдельный payload регистрировать **не нужно**.

**(б) Явный payload** (для командно-ориентированных сообщений): `Registrar::add_payload(..)`:

```rust
// vocab/src/parts/payloads.rs:258-289
fn add_payload(&mut self, declaration: PayloadDeclaration,
               server: Option<Arc<dyn IPayloadHandler>>,
               client: Option<Arc<dyn IPayloadHandler>>);
fn add_typed_payload(&mut self, declaration: PayloadDeclaration, schema: u64,
                     server: Option<Arc<dyn IPayloadHandler>>,
                     client: Option<Arc<dyn IPayloadHandler>>) -> PayloadToken;
fn add_raw_payload(&mut self, id: PayloadId, server: Option<Arc<dyn IPayloadHandler>>);
```
Пример вызова: `ProjectE-RS/src/lib.rs:108-112`.

**Индексированные запросы** (не обязательные): `AttachmentQueries` / `AttachmentBatch` /
`AttachmentWriteToken` в `vocab/src/attachment_queries.rs`, `vocab/src/entity/attachment_query.rs`;
`AttachmentType::with_decoded` — дешёвое чтение части большого значения без клона.

---

## 2. Item parts и жизненный цикл экипировки

### 2.1 Привязка поведения к item key

```rust
// vocab/src/parts/items.rs:175-217, trait ItemsRegistrar
fn item_key(&mut self, id: &str) -> crate::ItemKey;
fn attach_item_part(&mut self, item: ItemKey, kind: &'static str, part: Arc<dyn ItemPart>);
fn attach_item_part_tag(&mut self, tag: &str, kind: &'static str, part: Arc<dyn ItemPart>);

// удобные обёртки, vocab/src/parts/items.rs:99-107
impl dyn Registrar + '_ {
    pub fn item_part<P: ItemPart>(&mut self, item: ItemKey, part: P);
    pub fn item_part_tag<P: ItemPart>(&mut self, tag: &str, part: P);
}
```
`kind` — `type_name::<P>()`; один part каждого kind на один item (иначе startup refusal,
`generated/parts_modules.rs:77-83`). Пример: `ProjectE-RS/src/lib.rs:114-123`.

### 2.2 Хуки `ItemPart`, относящиеся к экипировке

`ItemPart` определён в `generated/mod_api_generated.rs:9106` (реализация — `Send + Sync + 'static`).
Точные сигнатуры:

```rust
// generated/mod_api_generated.rs:9122
fn tooltip(&self, host: &dyn ClientHost, event: &mut TooltipEvent<'_>);
// :9145, :9169
fn use_on(&self, host: HostSide, event: &mut UseOnEvent<'_>) -> Option<InteractionResult>;
fn use_item(&self, host: HostSide, event: &mut UseItemEvent<'_>) -> Option<InteractionResult>;
// :9176, :9188
fn hurt_enemy(&self, host: &mut dyn ServerHost, event: &mut HurtEnemyEvent<'_>);
fn inventory_tick(&self, host: &mut dyn ServerHost, event: &mut InventoryTickEvent<'_>);
// :9195, :9217, :9225, :9234
fn item_bar(&self, host: &dyn ClientHost, event: &mut ItemBarEvent<'_>);
fn tooltip_component(&self, host: &dyn ClientHost, event: &mut TooltipComponentEvent<'_>) -> Option<[f32;2]>;
fn tooltip_component_draw(&self, host: &dyn ClientHost, event: &mut TooltipComponentDrawEvent<'_>);
fn item_decorations(&self, host: &dyn ClientHost, event: &mut ItemDecorationsEvent<'_>);
// :9265, :9275, :9291
fn on_use_tick(&self, host: HostSide, event: &mut OnUseTickEvent<'_>);
fn release_using(&self, host: HostSide, event: &mut ReleaseUsingEvent<'_>);
fn apply_enchantments(&self, host: &dyn Host, event: &mut ApplyEnchantmentsEvent<'_>);
// :9324 — главный «можно ли надеть»
fn can_equip(&self, host: &dyn Host, event: &mut CanEquipEvent<'_>);
// :9627 — дроп
fn on_dropped_by_player(&self, host: &dyn Host, event: &mut OnDroppedByPlayerEvent<'_>) -> Flow;
// скрытая подготовка к can_equip (используется движком)
#[doc(hidden)] fn __prepare_equip(&self, stack: &ItemStack, kinds: &[&str],
    reads: &[EquipReadRow]) -> Result<Option<Box<dyn Any>>, &'static str>;
```

`inventory_tick` получает `InventoryTickEvent { stack, slot, selected }` (dispatch в
`generated/mod_api_generated.rs:10379-10380`). Для слотов Curios он **не вызывается** — см. §9.

**Глобальный equip-хук (не часть ItemPart):**

```rust
// generated/mod_api_generated.rs:2233, 10385-10390
fn on_equip(&self, host: &mut dyn ServerHost, event: &mut OnEquipEvent<'_>);
pub struct OnEquipEvent<'a> { pub slot: &'a str, pub new: Option<&'a ItemStack> }
```

`slot` — имя **`EquipmentSlot`** (`head`, `chest`, `legs`, `feet`, `mainhand`, `offhand`, `body`, `saddle`),
не произвольный Curios-слот. `on_equip` — server-side.

**Серверная запись экипировки:**

```rust
// vocab/src/host/living.rs:530-545 (decl), generated/mod_api_generated_host.rs:3079 (impl)
pub fn set_equipment(self, host: &mut dyn ServerHost, slot: EquipmentSlot,
                     stack: Option<ItemStack>) -> Result<bool, StackWriteError>;
```
`LivingRef` из `host.living(..)`. Это **direct write**, в обход транзакции; у мобов сразу вызывает
equip/unequip event и звук, у armor stand / дракона — нет. `None`/воздух/счёт 0 снимает предмет.

### 2.3 Attribute modifiers

```rust
// generated/mod_api_generated.rs:6087 (Hooks), :15846-15850
fn item_attribute_modifier(&self, event: &mut ItemAttributeModifierEvent<'_>);
pub struct ItemAttributeModifierEvent<'a> {
    pub stack: &'a ItemStack,
    pub modifiers: &'a mut ItemAttributeModifiers,
}
// vocab/src/item/types.rs:379-440
impl ItemAttributeModifiers {
    pub fn iter(&self) -> std::slice::Iter<'_, ItemAttributeModifier>;
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
    pub fn add(&mut self, attribute: Identifier, modifier: AttributeModifier,
               slot: EquipmentSlotGroup) -> bool;
    pub fn replace(&mut self, attribute: Identifier, modifier: AttributeModifier,
                   slot: EquipmentSlotGroup);
    pub fn remove(&mut self, attribute: &Identifier, id: &Identifier) -> bool;
    pub fn retain(&mut self, keep: impl FnMut(&ItemAttributeModifier) -> bool) -> bool;
}
```
Третий аргумент — **`EquipmentSlotGroup`** (ванильные группы), тогда как Curios использует
`SlotContext` и произвольные имена слотов. Обходной путь — см. §9.

Для тултипов атрибутов:
```rust
// generated/mod_api_generated.rs:4825, 13817-13824
fn add_attribute_tooltips(&self, host: &dyn ClientHost, event: &mut AddAttributeTooltipsEvent<'_>);
pub struct AddAttributeTooltipsEvent<'a> {
    pub player: Option<u128>, pub player_entity_id: Option<i32>,
    pub stack: &'a ItemStack, pub shown: bool, pub advanced: bool, pub creative: bool,
    pub lines: &'a mut Vec<(Text, u32)>,
}
```

### 2.4 Curios → Horizon: соответствие item-side

| Curios (Java) | Horizon | Где |
|---|---|---|
| `ICurio#canEquip(SlotContext,..)` | `ItemPart::can_equip` + `EquipReadRow` | generated:9324; `vocab/src/entity/equip_read.rs` |
| `ICurio#onEquip/onUnequip` | `Hooks::on_equip` (только equip); unequip — см. §9 | generated:2233 |
| `ICurio#curioTick` | см. §9 (inventory_tick не вызывается для mod-слотов) | — |
| `ICurio#getAttributeModifiers` | `Hooks::item_attribute_modifier` + `ItemAttributeModifiers` | generated:6087, 15846 |
| `ICurio#getDropRules` | `ItemPart::on_dropped_by_player` → `Flow` | generated:9627 |
| `ICurio#getEquipSound` | нет отдельного хука; звук идёт от `set_equipment` | `vocab/src/host/living.rs:530` |
| `ICurio#getRenderLayer/ICurioRenderer` | `Hooks::register_entity_layers` / `register_entity_renderers` | generated:7113, 7124 |
| `SlotType` (data-driven) | `declare_datapack_registry` (§4) | `vocab/src/parts/datapack_registries.rs:57` |

`EquipReadRow` определён в `vocab/src/entity/equip_read.rs`; `prepare_equip_parts` в
`vocab/src/parts/items.rs:150-173` — движок сам готовит part-ы перед обходом слотов/entity.

---

## 3. Container / inventory API — виртуальный инвентарь слотов для entity

Прямого «register entity inventory» нет. Есть три комплементарных механизма.

### 3.1 `ItemPlace::Mod` + `PlaceProvider` — единственный честный путь для accessory-слотов

```rust
// vocab/src/parts/items.rs:203
fn place_provider(&mut self, name: &'static str, provider: Arc<dyn PlaceProvider>);

// vocab/src/block/item_place.rs:102-109 — место, где живёт ваш stack
pub enum ItemPlace {
    Mod { provider: &'static str, key: u64 },   // «such as an accessory slot»
    Equipment { entity: u32, slot: EquipmentSlot },
    EntitySlot { entity: u32, slot: usize },
    PlayerSlot { player: u128, slot: i32 }, /* ... */
}

// vocab/src/block/item_place.rs:282-305
pub trait PlaceProvider: Send + Sync + 'static {
    fn stack(&self, host: &mut dyn ServerHost, key: u64) -> Option<ItemStack>;
    fn insert(&self, host: &mut dyn ServerHost, tx: &Transaction, key: u64,
              resource: &ItemResource, amount: u32) -> u32;
    fn extract(&self, host: &mut dyn ServerHost, tx: &Transaction, key: u64,
               resource: &ItemResource, amount: u32) -> u32;
}
```

`key: u64` кодирует `(entity_id, slot_index)`. Идеальная пара с §1: сами `ItemStack` хранятся
в `AttachmentType<CurioInventory>` на entity, а `PlaceProvider` — адаптер чтения/записи с
транзакционным `tx`. Движок сам вызывает insert/extract, когда item interaction пишет в такое место.

### 3.2 Капасити для item'ов

```rust
// vocab/src/block/item_place.rs:307-344
pub struct ItemCapabilityKey<T: ?Sized + 'static> { pub name: &'static str, /* private */ }
impl<T: ?Sized + 'static> ItemCapabilityKey<T> {
    pub const fn new(name: &'static str) -> Self;
}
pub const ITEM_ITEMS: ItemCapabilityKey<dyn ResourceHandler<ItemResource>> = ...;
pub const ENTITY_ITEMS: ... // equipment всех living entity

// vocab/src/parts/items.rs:108-133, 191-199
r.item_capability(&KEY, "mod:item", |host, place| Some(Box::new(HANDLER)));
fn provide_item_capability(&mut self, name: &'static str, item: &str,
                           before_game: bool, provider: ItemProviderOf);
```

### 3.3 `register_item_container` — только для item-типов с динамическим числом слотов

```rust
// vocab/src/parts/default_capabilities.rs:5-10, 20-28, 41-46
pub trait ItemContainerProvider: Send + Sync {
    fn slots(&self, host: &dyn Host) -> Option<u32>;
}
impl dyn Registrar + '_ { pub fn item_container(&mut self, item: &ItemKey,
                                                provider: impl ItemContainerProvider + 'static); }
fn register_item_container(&mut self, item: &ItemKey, provider: Arc<dyn ItemContainerProvider>);
```
Это **счётчик слотов для item-контейнера** (рюкзак), не инвентарь entity.

Для block entity: `r.block_container(&BlockKey, ModContainer)`:
```rust
// vocab/src/item/containers.rs:209
pub struct ModContainer {
    pub slots: usize,
    pub faces: [Vec<usize>; 6],
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    pub menu: Option<String>,
}
```

---

## 4. Data pack / registry — data-driven slot types

### 4.1 Объявление своего реестра

```rust
// vocab/src/parts/datapack_registries.rs:37-63
pub trait DatapackRegistriesRegistrar {
    fn declare_datapack_registry(&mut self, path: &str, sync: DatapackRegistrySync,
        fields: Vec<DatapackField>) -> Result<(), DatapackRegistrationError>;
}
```
`path` должен быть в namespace мода (`slot_type`CURIO → `curios:slot_type`). Только для
**both-side** пакетов. Файлы: `data/<pack>/curios/slot_type/<name>.json`.

```rust
// vocab/src/world/datapack_registry.rs:13-21, 27-58, 81-108
pub enum DatapackRegistrySync { ServerOnly, Required, Optional }
pub enum DatapackFieldKind { Bool, Int { min: i64, max: i64 },
    Float { min: f64, max: f64 }, Text { max_len: u32 }, Identifier,
    Reference { registry: String } }
pub struct DatapackField { pub name: String, pub kind: DatapackFieldKind,
                           pub default: Option<DatapackValue> }
impl DatapackField {
    pub fn required(name: &str, kind: DatapackFieldKind) -> Self;
    pub fn with_default(name: &str, kind: DatapackFieldKind, default: DatapackValue) -> Self;
}
pub enum DatapackValue { Bool(bool), Int(i64), Float(f64), Text(String) }
```
Схема плоская: **вложенных объектов/массивов нет**. Сложные `SlotType` (списки Cosmetic,
`size`, `order`) придётся разложить в плоские поля (`Reference` на соседние entry-ы).

### 4.2 Чтение записей

```rust
// generated/mod_api_generated_host.rs:1576-1594 — методы LevelRef
pub fn datapack_entries(self, host: &dyn Host, registry: &str) -> Option<Vec<String>>;
pub fn datapack_handle(self, host: &dyn Host, registry: &str, entry: &str)
    -> Result<DatapackHandle, DatapackReadError>;
pub fn datapack_read(self, host: &dyn Host, handle: &DatapackHandle)
    -> Result<Vec<(String, DatapackValue)>, DatapackReadError>;
```
Ошибки: `UnknownRegistry`, `MissingEntry`, `StaleGeneration`, `WrongSession`.

### 4.3 Data Maps и `data_maps_updated`

```rust
// generated/mod_api_generated.rs:5526, 15011-15014
fn data_maps_updated(&self, host: HostSide<'_>, event: &mut DataMapsUpdatedEvent<'_>);
pub struct DataMapsUpdatedEvent<'a> { pub registry: &'a str, pub cause: DataMapsUpdateCause }

// vocab/src/world/data_maps.rs:46-64
pub trait DataMap {
    type Entry: DataMapEntry; type Value;
    fn id(&self) -> &str;
    fn decode(&self, value: &Tag) -> Option<Self::Value>;
    fn ty(&self) -> DataMapType;
}
pub fn data_map_value(..) / data_map_entries(..)  // re-export в mod_api
```
Регистрация типа: `r.data_map_type(map.ty())`; файлы: `r.data_pack_files(event)`.
**Важно:** data maps server-side и не синхронизируются клиенту (ProjectE-RS/API_WORKAROUNDS.md §7).
`data_maps_updated` вызывается в фазе Reloading, где `host.players()` недоступен — отложите
реакцию до `server_tick` (ProjectE-RS/src/lib.rs:127-161).

---

## 5. Config

### 5.1 `register_configs` — файлы конфигурации (Hooks)

```rust
// generated/mod_api_generated.rs:6618, 16530-16533
fn register_configs(&self, host: HostSide<'_>, event: &mut RegisterConfigsEvent<'_>);
pub struct RegisterConfigsEvent<'a> { pub configs: &'a mut Vec<mod_api::Config> }

// vocab/src/game.rs:152-176
pub struct Config { pub mod_id: String, pub kind: ConfigKind,
                    pub file: String, pub default: String }
impl Config {
    pub fn new(mod_id: &str, kind: ConfigKind, default: &str) -> Self;
    pub fn file(mut self, file: &str) -> Self;
}
```
Сопутствующие хуки: `config_loading`, `config_reloading` (`ConfigLoadingEvent.config: &ConfigFile`,
`ConfigReloadingEvent.config`), `config_unloading` (`vocab/src/hooks/mod_configs.rs:3-55`).

Рабочий образец — `../ProjectE-RS/src/config.rs`:
1. `register(event) { event.configs.push(Config::new("projecte", ConfigKind::Common, DEFAULT_TEXT)); }`
2. `parse(text: &str) -> Result<Settings, ConfigError>` — строгий парсер (включая unknown key,
   duplicate, out-of-range; никаких `unwrap_or_default()`).
3. `loading(&ConfigFile)` фильтрует `config.mod_id != MOD_ID`, сохраняет в `static Mutex<Settings>`.
4. Ошибка парсинга → `eprintln!`, старое значение остаётся (fail explicitly).

### 5.2 `register_setting` — onclick-настройки клиента

```rust
// vocab/src/parts/settings.rs:15-22
pub trait SettingsRegistrar {
    fn register_setting(&mut self, setting: crate::SettingSpec) -> Result<(), crate::SettingError>;
}
// vocab/src/content/mod_settings.rs:116-160
impl SettingSpec {
    pub fn toggle(key, label, default: bool) -> Self;
    pub fn choice<S: Into<String>>(key, label, options, default: usize) -> Self;
    pub fn range(key, label, min: f64, max: f64, step: f64, default: f64) -> Self;
    pub fn color(key, label, default: u32) -> Self;
    pub fn tooltip(self, tooltip) -> Self;
    pub fn check(&self) -> Result<(), SettingError>;
}
```
`SettingSpec` — client-only, показывается в Options > Mods. ProjectE-RS его **не использует**:
всё хранится в `Config` + ручной парсер. Для серверных правил Curios (size, drop rules) нужен
именно `Config`, а не `SettingSpec` (последний client-side).

---

## 6. Client

### 6.1 GUI / screen

```rust
// generated/mod_api_generated.rs:7570, 17833-17836
fn screen_opening(&self, host: &dyn ClientHost, event: &mut ScreenOpeningEvent<'_>)
    -> Option<ScreenOpening>;
pub enum ScreenOpening { Cancel, ModScreen }  // vocab/src/client/types.rs:708

// vocab/src/host/screens.rs:73
fn mod_screen_open(&mut self, mod_id: &str) -> bool;
```
`Screen` (`vocab/src/client/types.rs:842`) — layout контейнерного экрана:
```rust
pub struct Screen {
    pub width: f32, pub height: f32,
    pub slots: Vec<Option<[f32; 2]>>,   // позиция каждого слота меню; None = скрыт
    pub inventory_y: f32, pub inventory_x: f32,
    pub own_labels: bool, pub own_background: bool,
    pub recipe_book: Option<RecipeBookTypeKey>,
    pub recipe_book_toggle: Option<[f32; 2]>,
}
```
**НАИВАЖНЕЙШЕЕ ОГРАНИЧЕНИЕ** (ProjectE-RS/API_WORKAROUNDS.md §8): `mod_screen_open`
открывает окно **без меню** — в нём нет реальных слотов. Реальные слоты существуют только
в экране меню (`native_screen` / `block_container(..).menu`). Для Curios GUI нужно:
`ModContainer { menu: Some("minecraft:generic_9xN"), .. }` + `Screen { slots, .. }` + меню-парты,
а кастомные «клетки» рисуются в `screen_draw`, но **не являются слотами**.

```rust
// generated/mod_api_generated.rs:8948, 10082-10087
fn screen_draw(&self, host: &dyn ClientHost, event: &mut ScreenDrawEvent<'_>);
pub struct ScreenDrawEvent<'a> {
    pub block: &str, pub data: &[i32], pub draw: &'a mut dyn DrawContext,
    pub origin: [f32; 2],
}
```
Плюс `screen_render_post`, `screen_mouse_button_pressed_pre/post`, `screen_mouse_scrolled_pre/post`,
`screen_key_pressed_pre/post`, `screen_init_post` и т.д. (`generated/mod_api_generated.rs:2526-2965, 7515-7570`).
`BlockEntityPart::screen_draw`/`screen_click` — рисование/клики по экрану блока.

Меню-парты: `r.menu_part("mod:menu", Part)` (`vocab/src/parts/menus.rs:35-37`), trait `MenuPart`
в `generated/mod_api_generated.rs:9698`.

### 6.2 Рендер слотов на entity

```rust
// generated/mod_api_generated.rs:7113, 7124, 17284-17287
fn register_entity_layers(&self, context: &RegistrationContext,
                          event: &mut RegisterEntityLayersEvent<'_>);
fn register_entity_renderers(&self, context: &RegistrationContext,
                             event: &mut RegisterEntityRenderersEvent<'_>);
pub struct RegisterEntityLayersEvent<'a> { pub layers: &'a mut dyn EntityLayers }
pub struct RegisterEntityRenderersEvent<'a> { pub renderers: &'a mut dyn EntityRenderers }
```
Типы/реализации: `vocab/src/client/entity_layers.rs`, `vocab/src/client/entity_renderers.rs`.
Для Curios layer-рендера это **замена `ICurioRenderer`/`ICurioLayer`**: слой регистрируется
на тип entity, а какиеITEM'ы рисовать — решается из attachment'а с этим entity.

### 6.3 Special model renderer

```rust
// vocab/src/client/special_model_renderers.rs:20-82
pub struct SpecialModelPart { pub model: String, pub transform: [f32;12], pub tints: Vec<u32> }
impl SpecialModelPart { pub const IDENTITY: [f32;12]; pub fn new(model) -> Self;
    pub fn with_transform(..) -> Self; pub fn translated(..) -> Self; pub fn with_tints(..) -> Self; }
pub trait SpecialModelRenderer: Send + Sync + 'static {
    fn extract(&self, host: &dyn ClientHost, stack: &ItemStack, config: &Tag)
        -> Result<Vec<SpecialModelPart>, String>;
}
pub trait SpecialModelRenderers { fn register(&mut self, kind: String,
                                             renderer: Arc<dyn SpecialModelRenderer>); }
```
Хук: `fn register_special_model_renderer(&self, context: &RegistrationContext,
event: &mut RegisterSpecialModelRendererEvent<'_>)` (generated:7292; событие — :17439).
Пример: `ProjectE-RS/src/lib.rs:167-184`, `ProjectE-RS/src/shield.rs`.
Diverges from vanilla: рендер возвращает **resource-модели**, а не геометрию. Максимум
`MAX_SPECIAL_MODEL_PARTS = 256`.

### 6.4 Tooltip appender

```rust
// vocab/src/parts/items.rs:205-216
fn register_tooltip_appender(&mut self, component: &str, placement: crate::TooltipPlacement,
                             appender: Arc<dyn crate::TooltipAppender>);
// vocab/src/client/... (re-export в mod_api): trait TooltipAppender {
//   fn append(&self, host: &dyn ClientHost, stack: &ItemStack,
//             context: &TooltipAppendContext, lines: &mut Vec<(Text, u32)>); }
```
Привязка идёт к **data component** (`curios:slot`), а не к item key. Плюс
`ItemPart::tooltip` (:9122) для строк самого предмета и `Hooks::add_attribute_tooltips` (:4825)
для строк атрибутов.

---

## 7. Команды

```rust
// generated/mod_api_generated.rs:2032, 2041, 10129-10131
fn register_commands(&self, host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>);
fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow;
pub struct RegisterCommandsEvent<'a> { pub commands: &'a mut Vec<mod_api::Command> }
```
`Command::new("curios slots [player:player]")`, `Command::op("...", 2)` — примеры в
`ProjectE-RS/src/lib.rs:405-434`. `RunModCommandEvent.source: CommandSource` (Player/Console/
CommandBlock/Function), `event.command` — строка. Аргументы: `vocab/src/command/command_argument.rs`
(`CommandArgumentValue`, `ChoiceArgument`, `EntitySelector`), кастомные типы — через
`CommandArgumentType` + `Registrar::command_argument`. Клиентские команды:
`fn register_client_commands(&self, host: &mut dyn ClientHost, event: &mut RegisterClientCommandsEvent<'_>)` (:1994).

---

## 8. Сравнение с ProjectE-RS (рабочий порт)

| Приём | ProjectE-RS | Что взять для Curios |
|---|---|---|
| Точка входа | `src/lib.rs:60` — один `#[mod_api::horizon_mod] impl Hooks` | такой же один `impl` в `src/lib.rs` |
| Per-player данные | `src/data.rs:150` — `static AttachmentType<PlayerData>`, codec + `copy_on_death: true`, `sync: false` | `static AttachmentType<CurioInventory>` на `Holder::Player(uuid)`; `sync: true` для клиентского GUI |
| Ошибки | `DataError` с `From<AttachmentError>`, явная проверка баланса | такой же enum для slot/inventory ошибок |
| Config | `src/config.rs` — `register_configs` + строгий `parse` + `Mutex<Settings>` | slot size/drop rules хранить здесь, не в `SettingSpec` |
| Data-driven | `Emc: DataMap`, `data_maps_updated` → отложить до `server_tick` | `SlotType` как datapack registry + отложенная пересборка кэша |
| Payload | `r.add_payload(declaration, Some(handler), None)` | для GUI-команд; для самого инвентаря достаточно attachment sync |
| Item behavior | `r.item_part(key, ChargePart)` | `r.item_part(key, CurioPart)` + `can_equip` |
| GUI | `native_screen` + `ModContainer { menu: Some(..) }` |точно так же: реальные слоты только через меню |
| Renders | `register_special_model_renderer` + `ShieldModel` | `register_entity_layers` для слотов на entity |

---

## 9. Чего в API НЕТ и какие нужны обходные пути

1. **Нет `on_unequip` хука.** Есть только `Hooks::on_equip` (`OnEquipEvent { slot, new }`) и
   `ShouldCauseReequipAnimationEvent` (клиент). `LivingRef::set_equipment(None)` вызывает
   unequip game event и звук, но **не даёт mod-хука**.
   *Обход:* не полагаться на событие; хранить снимок слотов в attachment и вычислять дельту
   (по `ItemPlace::Mod` + `PlaceProvider::stack`) в `server_tick` или в момент собственной записи.
   Все мутации инвентаря вести через единственный server-authoritative слой.

2. **Нет `curioTick` для mod-слотов.** `ItemPart::inventory_tick` срабатывает только для
   стеков в ванильных слотах игрока (`Inventory.tick` → `Item.inventoryTick`,
   `generated/mod_api_generated.rs:9177-9188`). `ItemPlace::Mod`-слоты движок не тикает.
   *Обход:* тикать в `Hooks::server_tick` / `level_tick_post`, перебирая entity с
   attachment'ом; либо через `EntityPart`-хуки, если item лежит в ванильном equipment.

3. **Нет произвольных `EquipmentSlot`/`EquipmentSlotGroup`.** `can_equip` и `ItemAttributeModifiers`
   оперируют фиксированным перечислением (`head..saddle` и ванильные группы).
   *Обход:* `canEquip` для Curios-слота реализовать через `EquipReadRow`/`__prepare_equip`
   (движок поддерживает произвольные `kinds: &[&str]` — `vocab/src/parts/items.rs:150-173`) и
   собственную проверку в `PlaceProvider`; атрибуты добавлять с groups = `any` + gate по
   component/attachment в `item_attribute_modifier`; тултип — `add_attribute_tooltips`.

4. **Нет реальных слотов в кастомном экране.** `mod_screen_open` без меню; см. §6.1 и
   `ProjectE-RS/API_WORKAROUNDS.md §8, §10`. *Обход:* через vanilla menu
   (`minecraft:generic_9xN`) + `ModContainer`, кастомную графику — в `screen_draw`.

5. **Нет вложенных структур в datapack-схеме.** `DatapackFieldKind` — только плоские Bool/Int/Float/
   Text/Identifier/Reference. *Обход:* нормализовать `SlotType` в несколько плоских полей и
   `Reference`-связи между entry-ами; либо хранить JSON-текст в `Text { max_len }` и парсить
   самим строго (см. подход config.rs).

6. **Data maps не синхронизируются клиенту** (`API_WORKAROUNDS.md §7`). Клиент не должен
   показывать «встроенное» значение слота, если сервер его переопределил/удалил. *Обход:* сервер
   шлёт client-совместимую Projection через `AttachmentType { sync: true }` или payload.

7. **Нет locale-хелпера / assets у native-мода** (`API_WORKAROUNDS.md §9`). *Обход:* таблицы
   локализации в коде + `Text::translate`; язык определить пробным переводом `container.inventory`.

8. **Хуки screen — `&self`, а не `&mut dyn Host`.** `Host::chat`/`action_bar` требуют `&mut`,
   поэтому из screen-хука их вызвать нельзя. *Обход:* рисовать сообщение прямо на экране.

9. **Нет нативного доступа к некоторым capability из Host** (energy/item-energy — `API_WORKAROUNDS.md §1,2`).
   Для Curios это не критично, но если слоты дают energy-эффекты — хранить значение в
   attachment/NBT и не притворяться нативным energy handler.

10. **Отсутствует «slot predicate» API.** Curios `ISlotType` predicates (`cosmetic`, `size`,
    tag-based allow/deny) нужно реализовать самим в Rust на данных datapack registry.

---

## 10. Точность и верификация

Документ составлен **только статическим чтением SDK** (`read`/`grep`/`find`) и рабочего порта
ProjectE-RS. команды сборки/тестов **не запускались**, коммиты/push/переключения веток **не
выполнялись**. Все приведённые сигнатуры скопированы из исходников по указанным путям и номерам
строк; при расхождении с компилятором источником истины является закреплённый SDK.

Файлы, созданные/изменённые агентом #1: **только `docs/research/horizon-api.md`**.
