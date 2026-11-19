# Curios: поведение Java-референса (исследование для порта)

Источник: `TheIllusiveC4/Curios` (NeoForge, MC 26.3), read-only копия
`/mnt/Barracuda/tmp/opencode/curios-reference`. Пакет `top.theillusivec4.curios`.
Лицензия LGPL-3.0-or-later, copyright (c) 2018-2024 C4.

Документ описывает **что именно портируем**, а не то, что уже сделано в Rust.
Все правила ниже проверены чтением Java-исходников; где поведение неочевидно,
указан исходный файл и метод.

---

## 1. Карта понятий

| Концепт Java | Суть | Ключевой исходник |
|---|---|---|
| `SlotContext` | запись `(identifier, entity, index, cosmetic, visible)`; `visible` = флаг отрисовки `renders[index]`. `getItemAccess()` даёт доступ к стеку через `CuriosResourceHandler`. | `api/SlotContext.java` |
| `ISlotType` | неизменяемое описание типа слота: `id, icon, order, size, useNativeGui, hasCosmetic, canToggleRendering, dropRule, validators, defaultEntityTypes`. Сортировка: `order`, потом `id`. | `api/type/ISlotType.java`, `common/slot/SlotType.java` |
| `Preset` | 11 встроенных типов: `curio, head, necklace, back, body, bracelet, hands, ring, belt, feet, charm`. `id()` = имя в нижнем регистре. | `api/CuriosSlotTypes.java` |
| Predicate | `BiPredicate<SlotContext, ItemStack>`, реестр по `Identifier`. Встроенные: `curios:all`, `curios:none`, `curios:tag`. | `impl/CuriosSlots.java` (static block) |
| `ICurio` / `ICurioItem` | поведение предмета-курио: `canEquip`, `canUnequip`, `curioTick`, `onEquip/onUnequip/onStateChange`, `getDropRule`, `getEquipSound`, `canSync` + `write/readSyncData`, Fortune/Looting, enderman/piglin/powder-snow. `ICurioItem` — интерфейс на `Item`, `ICurio` — capability на `ItemStack` (`ItemizedCurioCapability` делегирует). | `api/type/capability/ICurio.java`, `ICurioItem.java` |
| `ICuriosItemHandler` | capability сущности: `Map<String, ICurioStacksHandler> getCurios()`, `findCurio*`, `setEquippedCurio`, `saveInventory/loadInventory`, `add*SlotModifier`, `loadDatapacks`. | `api/type/capability/ICuriosItemHandler.java` |
| `ICurioStacksHandler` | один тип слота: `getStacks()` + `getCosmeticStacks()` (`IDynamicStackHandler`), `renders`, `activeStates`, модификаторы размера, `update()`. | `api/type/inventory/ICurioStacksHandler.java`, `common/inventory/CurioStacksHandler.java` |
| `SlotResult` | `(SlotContext, ItemStack)` — результат поиска. | `api/SlotResult.java` |
| `DropRule` | `DEFAULT, ALWAYS_DROP, ALWAYS_KEEP, DESTROY` (ordinal в сети, `StringRepresentable` в JSON). | `api/common/DropRule.java` |
| `CurioAttributeModifiers` | data component предмета: список `(атрибут, модификатор, SlotTypePredicate)` + `show_in_tooltip`. | `api/CurioAttributeModifiers.java`, `api/CuriosDataComponents.java` |
| `SlotAttribute` | псевдо-атрибут `curios.identifier.<id>` со значением 0, позволяющий модификатору добавлять/удалять слоты. | `api/SlotAttribute.java` |
| `SlotTypePredicate` | фильтр по `id`, `size`, `drop_rule`, `add_cosmetic`, `render_toggle`. | `api/common/slot/SlotTypePredicate.java` |
| События | `CurioCanEquipEvent`, `CurioCanUnequipEvent`, `CurioChangeEvent{Item,State}`, `CurioDropsEvent`, `DropRulesEvent`, `CurioAttributeModifierEvent`, `SlotModifiersUpdatedEvent`. | `api/event/**` |
| Хранилище | `AttachmentType<CurioInventory>` с `copyOnDeath()`, сериализуется через `ValueOutput/ValueInput`. | `impl/CuriosRegistry.java`, `common/capability/CurioInventory.java` |
| GUI | `CuriosMenu extends AbstractCraftingMenu`: 2×2 крафт + result + слоты курио + 4 слота брони, постранично. | `common/inventory/container/CuriosMenu.java`, `common/inventory/CurioSlot.java` |

---

## 2. Формат данных слотов

### 2.1. Расположение и порядок загрузки

`CuriosSlotResources.ID = curios:curios_slots`. Загрузчик разбирает **два**
типа файлов из датапаков (путь внутри неймспейса):

1. `curios/curios/slots/<id>.json` — декодится в `ISlotData.Entry` (`SlotData.Entry.CODEC`).
2. `curios/curios/entities/<name>.json` — декодится в `IEntitiesData.Entry`.

Третий источник — конфиг `Common.slots` (список строк), он обрабатывается
**после** `slots/*.json` и **до** `entities/*.json`; записи конфига считаются
применимыми к `curios:player_like`.

`apply()` сортирует ресурсы так, что файлы неймспейса `curios` идут первыми,
затем остальные по `Identifier`. Далее для каждого `slots/<id>` накапливаются
`SlotType.Builder` (по id) и `entityMap` (entity type → set slot id).
`entities/*.json` могут создавать новые типы слотов (`create`, по умолчанию true).
Итог — `Map<String, ISlotType> slots` и `Map<EntityType, Map<String, ISlotType>> entitySlots`.
При datapack reload всё пересчитывается; невалидные старые стеки уходят в
`invalidStacks` и дропаются в следующем тике.

### 2.2. Поля `slots/<id>.json` и значения по умолчанию

Источник: `impl/CuriosCodecs.java#slotDataEntryCodec()` (декод) и
`common/slot/SlotType.java#Builder` (итоговые значения по умолчанию).

| Поле JSON | Тип | Default в Entry (декод) | Итог в `SlotType.Builder.build()` | Смысл |
|---|---|---|---|---|
| `replace` | bool | `false` | — | Полная замена накопленных значений этого типа слота |
| `id` | string | absent | — | Идентификатор (для `entities` и конфига; в `slots/*.json` берётся из пути файла) |
| `order` | int | absent | `1000` | Меньше = раньше в обходе; при равенстве сортировка по `id` |
| `size` | int | absent | `1` | Базовое число слотов до модификаторов |
| `operation` | string (`set`/`add`/`remove`) | `"set"` | — | `add`/`remove` меняют `sizeMod`, `set` задаёт `size` (max при merge, replace — точное значение) |
| `use_native_gui` | bool | absent | `true` | Показывать ли слот в нативном GUI (AND при merge) |
| `add_cosmetic` | bool | absent | `false` | Добавить косметические пары слотов (OR при merge) |
| `icon` | Identifier | absent | `curios:textures/gui/generic_icon.png` | Иконка фона слота |
| `drop_rule` | enum | absent | `DEFAULT` | Правило дропа по умолчанию |
| `render_toggle` | bool | absent | `true` | Кнопка вкл/выкл отрисовки (AND при merge) |
| `conditions` | список condition | `[]` | — | NeoForge conditional-подстановка |
| `validators` | список Identifier | absent | `[curios:tag]` | Предикаты валидности; пустой/отсутствующий → `curios:tag` |
| `entities` | список `#tag` или `id` | absent | `curios:player_like` | Сущности, получающие слот по умолчанию (`#` = entity type tag) |

`size` после `build()`: `max(0, size + sizeMod)`.

**Фактические встроенные определения** (`src/main/resources/data/curios/curios/slots/*.json`):

| id | order | icon | validators |
|---|---|---|---|
| `curio` | 20 | `curios:slot/empty_curio_slot` | *(нет → `curios:tag`)* |
| `head` | 40 | `.../empty_head_slot` | `curios:tag` |
| `necklace` | 60 | `.../empty_necklace_slot` | `curios:tag` |
| `back` | 80 | `.../empty_back_slot` | `curios:tag` |
| `body` | 100 | `.../empty_body_slot` | `curios:tag` |
| `bracelet` | 120 | `.../empty_bracelet_slot` | `curios:tag` |
| `hands` | 140 | `.../empty_hands_slot` | `curios:tag` |
| `ring` | 160 | `.../empty_ring_slot` | `curios:tag` |
| `belt` | 180 | `.../empty_belt_slot` | `curios:tag` |
| `feet` | 190 | `.../empty_feet_slot` | `curios:tag` |
| `charm` | 200 | `.../empty_charm_slot` | `curios:tag` |

`entities/player_like.json` (ресурс `curios:player_like`) даёт тег сущностей
`curios:player_like` → игроки и подобные им сущности.

### 2.3. `entities/<name>.json`

Поля (`IEntitiesData.Entry.CODEC`): `replace=false`, `entities` (обязательный
список tag/id), `slots` (обязательный список: строка = имя типа слота или
объект `{ "create": true, "slot": { ...ISlotData.Entry... } }`), `conditions=[]`.

### 2.4. Серийная форма `ISlotType` (codec/stream, не data-файл)

`slotTypeCodec()` (`CuriosCodecs`, используется и для сети): обязательные поля
`id`, `validators`, `entities`; остальные по умолчанию:
`order=0`, `size=1`, `use_native_gui=true`, `add_cosmetic=false`,
`icon=GENERIC_ICON`, `drop_rule=DEFAULT`, `render_toggle=true`.
Не путать со значениями из §2.2 — это другой codec.

---

## 3. Валидность, equip, unequip

### 3.1. Вычисление подходящих слотов для предмета

`CuriosSlots.getSlotTypes(stack, entity)`:

1. Берём слоты сущности (`entity != null`) или все слоты (`entity == null`).
2. Для каждого типа вызываем `isItemValid` → `testPredicates(ctx, stack, validators)`.
3. Предикаты (OR): `all` → true, `none` → false,
   `tag` → `stack.is(ItemTags.create(curios:<id>)) || stack.is(curios:curio)`.
   Если `validators` пуст, неявно используется `curios:tag`.
4. Если хотя бы один слот подошёл и стек **не** в теге `curios:generic_exclusions`,
   в результат дополнительно кладётся слот `curio` (универсальный).

`CuriosApi.isStackValid(ctx, stack)` = `getItemSlotTypes(stack, entity).containsKey(ctx.identifier())`.
`SlotContext` здесь создаётся с `index=0, cosmetic=false, visible=true`.

### 3.2. `canEquip` (`CuriosStacksResourceHandler.isValid`)

```
original = isStackValid(ctx, stack)
         && getCurio(stack).map(c -> c.canEquip(ctx)).orElse(true);
event = new CurioCanEquipEvent(stack, ctx, original);  // NeoForge EVENT_BUS
result = event.getEquipResult();                        // TriState
return (result == DEFAULT && original) || result == TRUE;
```

`ICurio.canEquip` по умолчанию `true`. `CurioCanEquipEvent` хранит
`originalResult` и `TriState`; `setEquipResult(boolean)` → TRUE/FALSE.

### 3.3. `canUnequip` (`CuriosStacksResourceHandler.extract`)

```
original = (existing.isEmpty() || player.isCreative()
            || !existing.has(PREVENT_EQUIPMENT_DROP))
         && getCurio(existing).map(c -> c.canUnequip(ctx)).orElse(true);
event = new CurioCanUnequipEvent(existing, ctx, original);
result = event.getUnequipResult();
if ((result == DEFAULT && original) || result == TRUE) super.extract(...); else 0;
```

`ICurio.canUnequip` по умолчанию `true`. Это единственное место, где
проверяется возможность снять предмет; прямой `setStackInSlot` её не проверяет.

### 3.4. Equip через ПКМ (`CuriosCommonEvents.curioRightClick`)

Только если у стека есть `ICurio` и `canEquipFromUse(ctx)==true` (по умолчанию
`false`). Ищется первый **активный** (`activeStates[i]`) слот, куда `isValid`
пропускает стек и который пуст → вставить копию, `onEquipFromUse`, уменьшить
стек в руке (кроме creative). Если все подходящие слоты заняты — запоминается первый,
куда можно вставить после извлечения текущего предмета; тогда старый предмет
возвращается в руку. Итог — отмена события с SUCCESS / SUCCESS_SERVER.

### 3.5. Обнаружение изменений (`CuriosCommonEvents.tick`, EntityTickEvent.Post)

Каждый тик на обеих сторонах: `handleInvalidStacks()`, для каждого слота
`updateActiveState(i)`, тик стека и `curioTick`. На сервере сравнивается
`ItemStack.matches(stack, previousStack)`:

- `isSameItem` → `CurioChangeEvent.State`, иначе `CurioChangeEvent.Item`.
- Снимаются модификаторы предыдущего стека (slot-атрибуты → `removeSlotModifier`,
  обычные → `AttributeInstance.removeModifier`), вызывается `onUnequip` (только
  при смене предмета, не при `State`).
- Надеваются модификаторы нового стека (slot-атрибуты → `addTransientSlotModifier`,
  обычные → `addOrUpdateTransientModifier`), вызывается `onEquip` / `onStateChange`,
  триггерится `equip_curio`.
- `SPacketSyncStack` шлётся всегда при изменении; `canSync` + `writeSyncData`
  добавляют `CompoundTag` с дополнительными данными.
- После обхода: `clearCachedSlotModifiers()` и `SPacketSyncModifiers` для
  обновлённых хендлеров.
- Косметический слот синхронизируется отдельно (`HandlerType.COSMETIC`).
- `prevStack` обновляется копией текущего.

---

## 4. DropRule и приоритеты

`handleDrops()` в `CuriosCommonEvents` для каждого слота (functional и cosmetic):

1. **Приоритет 1** — `DropRulesEvent` (глобальный override): список
   `(Predicate<ItemStack>, DropRule)`; применяется **последний**
   совпавший predicate (не первый).
2. **Приоритет 2** — `ICurio.getDropRule(ctx, source, recentlyHit)`, по умолчанию
   `DEFAULT`.
3. **Приоритет 3** — `ISlotType.getDropRule()` (из JSON), если итого `DEFAULT`.

Дальше:

- `ALWAYS_KEEP` или (`DEFAULT` && `keepInventory`) → предмет остаётся, слот не чистится.
- `keepInventory` = gamerule `KEEP_INVENTORY`, но конфиг сервера `keepCurios`
  (`ON`/`OFF`) перекрывает его (`DEFAULT` = следовать gamerule).
- Если у стека есть `PREVENT_EQUIPMENT_DROP` **и** правило не `DESTROY` → в дроп
  не попадает, но слот чистится.
- `DESTROY` → не дропается в любом случае. `ALWAYS_DROP` → дропается даже при
  `keepInventory`.
- Дроп — `ItemEntity` с `pickupDelay=40` и случайным разлётом; собирается в
  `CurioDropsEvent` (отменяемый). Если событие не отменено, дропа добавляются
  в `LivingDropsEvent`.

Значения `DropRule`: `DEFAULT`, `ALWAYS_DROP`, `ALWAYS_KEEP`, `DESTROY`.
В JSON — `getSerializedName()` = имя в нижнем регистре; в сети — ordinal
(вне диапазона → `DEFAULT` через `OutOfBoundsStrategy.ZERO`).

---

## 5. Атрибуты и модификаторы размера слота

### 5.1. Data component предмета

`CuriosDataComponents.ATTRIBUTE_MODIFIERS` (`DataComponentType` с persistent
`CurioAttributeModifiers.CODEC` и network `STREAM_CODEC`):

```
CurioAttributeModifiers {
  modifiers: [ Entry { type: Attribute|SlotAttribute,
                       modifier: {id, amount, operation},
                       slot: SlotTypePredicate = ANY } ],
  show_in_tooltip: bool = true
}
```

- CODEC принимает как объект, так и «голый» список(entry → `showInTooltip=true`).
- `withModifierAdded` удаляет существующий модификатор с тем же id и добавляет новый.
- `forEach(SlotContext, ...)`: для совпавших entry id модификатора получает
  суффикс `/<slotIdentifier>/<index>`; `forEach(ISlotType, ...)` — `/<id>`.
  Благодаря этому один и тот же модификатор можно надеть в несколько слотов.
- `ICurioItem.getAttributeModifiers(stack)`: сначала component; если список пуст —
  `ICurio.getDefaultCurioAttributeModifiers()`; затем `CurioAttributeModifierEvent`
  через `CuriosCommonHooks.computeModifiedAttributes`. Событие может
  добавить/удалить/заменить/clear модификаторы (уникальность по `(attribute, id)`).
- `SlotAttribute.getOrCreate(id)` — псевдо-атрибут `curios.identifier.<id>`;
  именно он позволяет модификатору `add`/`remove` слоты. Тултип:
  `curios.modifiers.slots.plus|take[.multiple]`.

### 5.2. SlotTypePredicate

Поля: `id` (строка или список; default `[]`), `size` (`MinMaxBounds.Ints`,
default ANY), `drop_rule`, `add_cosmetic`, `render_toggle` (optionals).
**Важный нюанс реализации `matches(ISlotType)`**: цикл
`for (String id : id()) { if (!slotType.getId().equals(id)) return false; }`
требует, чтобы *каждый* id совпал со слотом; практически это означает
«не более одного уникального id»; пустой список совпадает с любым слотом.
Остальные поля сравниваются, если присутствуют.

### 5.3. Динамический размер

`CurioStacksHandler.update()` (только после `setDataLoaded()`):

```
base = baseSize
for ADD_VALUE:            base += amount
size = base
for ADD_MULTIPLIED_BASE:  size += baseSize * amount
for ADD_MULTIPLIED_TOTAL: size *= amount
size = max(0, size)
```

При изменении размера: `resize()` → grow/shrink обоих хендлеров (stacks +
cosmetics); при уменьшении предметы из «хвоста» уходят в `invalidStacks`
(будут дропнуты); шлётся `SlotModifiersUpdatedEvent`, открытый `CuriosMenu`
вызывает `resetSlots()`. `PermanentModifiers` сериализуются, transient — нет.

---

## 6. Персистентность

- `CuriosRegistry.INVENTORY`: `AttachmentType.serializable(CurioInventory::new).copyOnDeath()`.
  То есть инвентарь копируется при воскрешении игрока (дальше работает DropRule).
- `CurioInventory.serialize`: `childrenList("Curios")`, каждый дочерний узел —
  `{ <slotId>: <CurioStacksHandler> }` (LinkedHashMap → порядок сохраняется).
- `deserialize`: если список `Curios` пуст → `resetInventory()` (создать дефолтные
  слоты по датапакам), иначе создать `CurioStacksHandler` для каждого id.
- `CurioStacksHandler` ключи сериализации: `Identifier`, `BaseSize`, `Size`,
  `Stacks`, `Cosmetics` (дочерние IDynamicStackHandler), `Renders`, `ActiveStates`
  (списки bool), `Cosmetic`, `Visible`, `ToggleRender`, `DropRule`,
  `PermanentModifiers` (список `AttributeModifier`). При чтении дополнительно
  читается `Modifiers` (transient).
- `IDynamicStackHandler` хранит текущие и **previous** стеки (для детекта изменений
  в тике) и previous active-состояния.
- `loadInventoryConfiguration()` / `loadDatapacks()` при релоаде: строит новый
  дефолтный инвентарь, переносит модификаторы, стек и cosmetic-стек если они ещё
  валидны, копирует renders/active; невалидные/лишние предметы → `invalidStacks`.
- Кэши `findFirstCurio` / `findCurios`: `maximumSize(100)`, `expireAfterWrite(1s)`.

---

## 7. Сетевые пакеты

Имена—`Identifier` в неймспейсе `curios`; все `CustomPacketPayload`.

### Client → Server

| Пакет | Поля | Действие |
|---|---|---|
| `destroy` | — | Очистить все слоты игрока, снять slot-модификаторы, отослать `sync_stack` (убирает и cosmetic); требует подтверждения на клиенте |
| `open_curios` | `carried: ItemStack` | Открыть `CuriosMenu` с перенесённым предметом |
| `open_vanilla` | `carried: ItemStack` | Вернуться в обычный инвентарь |
| `client_page` | `windowId:int, next:bool` | След./пред. страница меню |
| `toggle_cosmetics` | `windowId:int` | `CuriosMenu.toggleCosmetics()` → `resetSlots()` |
| `toggle_render` | `identifier:String, index:int` | Инвертировать `renders[index]` → `SPacketSyncRender` всем отслеживающим + себе |

### Server → Client

| Пакет | Поля | Действие |
|---|---|---|
| `sync_stack` | `entityId, curioId:String, slotId, stack:ItemStack, handlerType:int, compoundTag` | Синхронизация одного слота; `HandlerType.EQUIPMENT=0` / `COSMETIC=1`; `compoundTag` — `canSync`/`writeSyncData` |
| `grabbed_item` | `stack` | Вернуть предмет в курсор клиенту |
| `sync_curios` | `entityId, Map<String, ICurioStacksHandler>` | Полная синхронизация инвентаря (join/track/datapack reload) |
| `sync_data` | `CuriosSlotResources.STREAM_CODEC` | Синхронизация загруженных slot/entity definition |
| `sync_modifiers` | `entityId, Map<String, CompoundTag>` | Синхронизацию требующие хендлеры (`getSyncTag()`) |
| `sync_render` | `entityId, curioId, slotId, value:bool` | Один флаг отрисовки |
| `sync_active` | `entityId, curioId, slotId, value:bool` | Один флаг активности слота |
| `break` | `entityId, curioId, slotId` | Анимация поломки курио на клиенте |
| `server_page` | `windowId, page` | Установить страницу |
| `quick_move` | `windowId, moveIndex` | Отложенный quick-move после `resetSlots()` |

Все client-пакеты обрабатываются на сервере; client-side handle'ы обёрнуты в
`ctx.enqueueWork(...)`.

---

## 8. GUI и конфиг

- `CuriosMenu extends AbstractCraftingMenu`; 2×2 crafting + result, 4 слота
  брони, слоты курио постранично. `maxSlotsPerPage` default 48 (диапазон 1..48),
  `minimumColumns` default 1 (1..8); число колонок = `ceil(slotsOnPage / 8)`,
  но не меньше `min(minimumColumns, slotsOnPage)`.
- `CurioSlot` фон = `ISlotType.getIcon()`; тултип:
  `curios.identifier.<id>` (fallback — капитализированный id),
  `curios.cosmetic` (светло-пурпурный), `curios.tooltip.inactive` (красный).
  Кнопка toggle rendering — в правом верхнем углу слота, доступна только при
  `canToggleRendering`.
- Косметические слоты создаются парами с функциональными, ничего не дают gameplay-у,
  но имеют приоритет в рендере модели; `visible` в `SlotContext` = `isCosmetic || renders[index]`.
- Текстуры: `assets/curios/textures/slot/empty_<id>_slot.png`,
  `.../gui/container/sprites/...`, кнопки и иконки cosmetic on/off;
  model-слои для amulet/crown/knuckles.
- `en_us.json` ключи: `curios.name`, `curios.slot`, `curios.tooltip.slot`,
  `curios.cosmetic`, `curios.identifier.*` (curio/head/necklace/back/body/
  bracelet/hands/ring/belt/charm), `curios.modifiers.slots.*`,
  `curios.modifiers.<slotId>` («When worn as…»), `gui.curios.toggle`,
  `gui.curios.toggle.cosmetics`, `gui.curios.page`, `key.curios.open.desc`,
  `curios.tooltip.inactive`, `curios.networking.failed`, команды `/curios`.
  Локализации: 18 языков. *(Примечание: `curios.identifier.feet` в `en_us` отсутствует —
  унаследованный недочёт референса.)*
- Конфиг: server `keepCurios=DEFAULT`, `menu.minimumColumns=1`,
  `menu.maxSlotsPerPage=48`; common `slots=[]`. В Rust-репо уже есть
  `src/config.rs` с теми же значениями (`default_slot_count=1`,
  `render_curios=true`, `enable_button=true`) — не трогаем этот файл.

---

## 9. Карта «Java-концепт → Rust-модуль»

| Java | Rust (план по `AGENTS.md`) | Примечание |
|---|---|---|
| `ISlotType`, `SlotType`, `Builder`, `Preset`, `CuriosSlotTypes`, `SlotPredicate`, `SlotTypePredicate`, predicates | `src/slots.rs` | Парсинг `data/curios/curios/slots/*.json` + реестр валидаторов; `order/id` sorting; дефолты §2.2 |
| `CurioInventory`, `CurioStacksHandler`, `CuriosStacksResourceHandler`, `ICuriosItemHandler`, `IDynamicStackHandler`, persistence | `src/inventory.rs` | Per-entity storage, stacks/cosmetics/renders/active, serialize/deserialize, cache invalidation |
| `ICurio`, `ICurioItem`, `CuriosCapability`, `CuriosDataComponents`, `CurioAttributeModifiers`, `SlotAttribute` | `src/curio.rs` | Item-side behavior, data component, attribute entries, suffixing ids |
| `canEquip`/`canUnequip`, `handleDrops`/`DropRule`/`DropRulesEvent`, `curioRightClick`, tick-наблюдатель, `CurioCanEquipEvent`/`CurioCanUnequipEvent` | `src/equip.rs` | Сервер-авторитарные решения; TriState-разрешение; приоритеты дропа §4 |
| `CuriosCommonEvents` (datapack reload), `NetworkHandler`, `SPacket*`/`CPacket*` | `src/lib.rs` | Единственный `#[horizon_mod] Hooks`; порядок при datapack reload, синхронизация, пакеты |
| `CuriosConfig`, `CuriosClientConfig` | `src/config.rs` | Уже реализовано другим агентом, только читать |
| `data/curios/curios/slots/*.json` | `data/curios/curios/slots/*.json` | Перенести 11 definition как есть (значения §2.2) |
| `CuriosMenu`, `CurioSlot`, `ICuriosScreen`, renderer'ы | *в первый PR не входит* | GUI/рендер — отдельный этап после ядра |
| `CuriosRegistry` (menu, attachment, component, trigger, argument) | `src/lib.rs` + `src/inventory.rs` | Регистрация типов Horizon |

---

## 10. Минимальный объём первого PR (приоритезировано)

1. **P0 — модель слотов + данные.** `SlotContext`, `SlotType` с дефолтами §2.2,
   загрузка 11 JSON, сортировка `order→id`, entity→slots (`player_like` → все
   встроенные типы), валидаторы `all/none/tag`, `curios:curio` universal и
   `generic_exclusions`. Тесты: парсинг дефолтов, порядок, валидация тегов.
2. **P0 — инвентарь сущности.** Per-entity handler с `stacks`, `cosmetics`,
   `renders`, `active`, `previous*`, CRUD-поиск `findCurio*`, serialize/deserialize
   с ключами §6, reset/load-datapacks. Тесты: round-trip, перенос валидных стеков.
3. **P0 — equip/unequip + server authority.** `isStackValid` → `canEquip` →
   TriState event; `canUnequip` с `PREVENT_ARMOR_CHANGE`; equip-from-use только с
   `canEquipFromUse`; детекция изменений по previous stack; `onEquip/onUnequip/
   onStateChange` хуки предмета. **Клиент только запрашивает**, решение на сервере.
4. **P1 — drop Rule.** Приоритетная цепочка §4, `keep_curios` конфиг, отмена
   через `CurioDropsEvent`-эквивалент, clear слота при drop/destroy.
5. **P1 — attribute modifiers.** `CurioAttributeModifiers`, `SlotTypePredicate`,
   `SlotAttribute`, suffix id, применение/снятие при equip/unequip, dynamic resize
   по ADD_VALUE/ADD_MULTIPLIED_BASE/ADD_MULTIPLIED_TOTAL; permanent vs transient.
6. **P1 — базовая синхронизация.** `sync_curios` (join/track/reload), `sync_stack`,
   `sync_modifiers`, `sync_render`/`sync_active`, `toggle_render` client packet.
   Обязательно статическая проверка имён/сигнатур по `.horizon/sdk/api/`.
7. **P2 — GUI, косметика в UI, страницы, команды `/curios`, advancement trigger,
   JEI/REI, enderman/piglin/powder-snow, Mending/Fortune/Looting, break-анимация.**

**Явные ограничения для описания PR:** первый PR — API-поверхность, модель
слотов и инвентарь; не портируются GUI, команды, интеграции, расширенные
эффекты предметов и cosmetics UI. Gameplay-паритет — цель, а не утверждение.

---

## 11. LGPL-атрибуция

- Референс — LGPL-3.0-or-later, `Copyright (c) 2018-2024 C4`. В репо уже есть
  `COPYING` и `COPYING.LESSER` — сохранять без изменений.
- Файлы-адаптации должны иметь заголовок вида:
  `Adapted from Curios by TheIllusiveC4 (c) 2018-2024 C4, LGPL-3.0-or-later;
   see COPYING and COPYING.LESSER.` (или эквивалентный, в формате Rust `//!`).
- Перенесённые данные (`data/curios/curios/slots/*.json`, имена/order/иконки,
  ключи `en_us.json`) — тоже адаптированный материал; отметить в README или в
  заголовке файла данных (где поддерживаются комментарии).
- Не копировать Java-код буквально без пометки об адаптации; переписанные
  структуры и алгоритмы всё равно являются производным произведением.
- `docs/research/` — исследовательские заметки, тоже производные от референса;
  данный файл снабжён атрибуцией в заголовке.

---

### Источники (файлы прочитаны)

`api/`: `CuriosApi.java`, `CuriosSlotTypes.java`, `CuriosTags.java`,
`CuriosResources.java`, `CuriosCapability.java`, `CuriosDataComponents.java`,
`CurioAttributeModifiers.java`, `SlotAttribute.java`, `SlotContext.java`,
`SlotResult.java`, `SlotPredicate.java`, `common/DropRule.java`,
`common/CuriosCommonHooks.java`, `common/slot/SlotTypePredicate.java`,
`type/ISlotType.java`, `type/capability/{ICurio,ICurioItem,ICuriosItemHandler}.java`,
`type/inventory/{ICurioStacksHandler,IDynamicStackHandler}.java`,
`event/{CurioCanEquipEvent,CurioCanUnequipEvent,CurioChangeEvent,CurioDropsEvent,
DropRulesEvent,CurioAttributeModifierEvent,SlotModifiersUpdatedEvent}.java`.
`common/`: `CuriosCommonEvents.java`, `capability/CurioInventory.java`,
`inventory/{CurioStacksHandler,CuriosStacksResourceHandler,CurioSlot}.java`,
`inventory/container/CuriosMenu.java`, `data/{SlotData,EntitiesData,
CuriosSlotResources}.java`, `slot/SlotType.java`, `network/NetworkHandler.java`,
`network/**` (все пакеты и оба хендлера), `config/CuriosConfig.java`.
`impl/`: `CuriosSlots.java`, `CuriosCodecs.java`, `CuriosRegistry.java`.
`resources/`: `data/curios/curios/slots/*.json`,
`assets/curios/lang/en_us.json`, `data/curios/tags/entity_type/player_like.json`.
