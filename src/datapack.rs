//! Data-driven slot types through a Horizon datapack registry.
//!
//! Declares `curios:slot_type` with the flat schema Horizon allows
//! (`docs/research/horizon-api.md` §4) and rebuilds the slot registry from
//! `LevelRef::datapack_entries/handle/read`. A reload is deferred to
//! `server_tick`: data is reloaded before players are available, as in
//! `ProjectE-RS`.
//!
//! **No `data_pack_files` contribution is possible for registry entries.**
//! The hook accepts only `<kind>/<namespace>:<path>` keys for the tables it
//! reads (`recipe`, `tags`, `loot_table`, `advancement`, `trade_set`,
//! `villager_trade`, `dialog`, `data_maps`); a mod-declared registry is not one
//! of those kinds. Entries are read from real pack files at the exact path
//! returned by [`entry_file`], i.e. `data/curios/curios/slot_type/<id>.json`.
//! [`data_pack_files`] therefore only marks a pending rebuild.
//!
//! Adapted from `CuriosSlotResources#populateData` and
//! `CuriosCommonEvents` of TheIllusiveC4/Curios (LGPL-3.0-or-later,
//! copyright 2018-2024 C4).

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use mod_api::{
    AttachmentQueriesMut, DataPackFilesEvent, DatapackField, DatapackFieldKind, DatapackReadError,
    DatapackRegistrationError, DatapackRegistrySync, DatapackValue, Holder, Host, Registrar,
    ServerHost, ServerTickEvent,
};

use crate::drop::DropRule;
use crate::inventory;
use crate::registry;
use crate::slots::{
    DEFAULT_ORDER, SlotDefinition, SlotError, SlotType, TAG_VALIDATOR, slot_bare_id,
};

/// Registry path declared inside the mod's namespace.
pub const REGISTRY_PATH: &str = "slot_type";
/// Full registry key used for reads and file entries.
pub const REGISTRY_KEY: &str = "curios:slot_type";
/// Dimension queried for the server-wide datapack registry set.
pub const DEFAULT_DIMENSION: &str = "overworld";

static RELOAD_PENDING: AtomicBool = AtomicBool::new(false);

/// Records that the next server tick must rebuild the slot registry.
pub fn mark_pending() {
    RELOAD_PENDING.store(true, Ordering::Release);
}

/// Consumes and returns the pending reload flag; public so integration tests
/// can drive the same production state machine as `Hooks::server_tick`.
pub fn take_pending() -> bool {
    RELOAD_PENDING.swap(false, Ordering::AcqRel)
}

/// Errors while reading and decoding datapack slot entries.
#[derive(Debug)]
pub enum ReloadError {
    /// No level is open in the requested dimension.
    NoLevel(String),
    /// The engine could not read a registry entry.
    Read(DatapackReadError),
    /// A schema field had an unexpected type or range.
    InvalidField(String, String),
    /// A decoded slot definition was rejected by the domain model.
    Slot(SlotError),
    /// The rebuilt registry was empty and therefore unusable.
    EmptyRegistry,
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLevel(dimension) => {
                write!(f, "no level is open in dimension `{dimension}`")
            }
            Self::Read(error) => write!(f, "datapack read failed: {error:?}"),
            Self::InvalidField(name, value) => {
                write!(f, "invalid slot field `{name}`: {value}")
            }
            Self::Slot(error) => error.fmt(f),
            Self::EmptyRegistry => f.write_str("slot datapack produced no entries"),
        }
    }
}

impl Error for ReloadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Slot(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DatapackReadError> for ReloadError {
    fn from(error: DatapackReadError) -> Self {
        Self::Read(error)
    }
}

impl From<SlotError> for ReloadError {
    fn from(error: SlotError) -> Self {
        Self::Slot(error)
    }
}

/// Flat schema fields of `curios:slot_type`.
///
/// Horizons's decoder has no arrays or nested records, so a Java slot with a
/// list of validators is flattened to one `validator` id. Extra validators
/// from data packs are not representable without a second registry.
pub fn schema_fields() -> Vec<DatapackField> {
    vec![
        DatapackField::with_default(
            "order",
            DatapackFieldKind::Int {
                min: i64::from(i32::MIN),
                max: i64::from(i32::MAX),
            },
            DatapackValue::Int(i64::from(DEFAULT_ORDER)),
        ),
        DatapackField::with_default(
            "size",
            DatapackFieldKind::Int {
                min: 0,
                max: i64::from(i32::MAX),
            },
            DatapackValue::Int(1),
        ),
        DatapackField::with_default(
            "icon",
            DatapackFieldKind::Identifier,
            DatapackValue::Text("curios:textures/gui/generic_icon.png".to_owned()),
        ),
        DatapackField::with_default(
            "drop_rule",
            DatapackFieldKind::Text { max_len: 32 },
            DatapackValue::Text("default".to_owned()),
        ),
        DatapackField::with_default(
            "use_native_gui",
            DatapackFieldKind::Bool,
            DatapackValue::Bool(true),
        ),
        DatapackField::with_default(
            "add_cosmetic",
            DatapackFieldKind::Bool,
            DatapackValue::Bool(false),
        ),
        DatapackField::with_default(
            "render_toggle",
            DatapackFieldKind::Bool,
            DatapackValue::Bool(true),
        ),
        DatapackField::with_default(
            "validator",
            DatapackFieldKind::Identifier,
            DatapackValue::Text(TAG_VALIDATOR.to_owned()),
        ),
    ]
}

/// Declares the registry from `Hooks::register`.
pub fn declare(r: &mut dyn Registrar) -> Result<(), DatapackRegistrationError> {
    r.declare_datapack_registry(
        REGISTRY_PATH,
        DatapackRegistrySync::ServerOnly,
        schema_fields(),
    )
}

/// Exact data-pack path Horizon uses for one `curios:slot_type` entry.
///
/// This mirrors `DatapackRegistryDeclaration::entry_file`:
/// `data/<entry namespace>/<registry namespace>/<registry path>/<entry>.json`.
pub fn entry_file(id: &str) -> Option<String> {
    if id.is_empty() || id.contains('/') || id.contains(':') {
        return None;
    }
    Some(format!("data/curios/curios/slot_type/{id}.json"))
}

/// Flattens a bundled slot definition to the registry schema, for generators
/// that write the real pack files at [`entry_file`] paths.
pub fn flat_entry(id: &str, definition: &SlotDefinition) -> String {
    let mut object = serde_json::Map::new();
    object.insert(
        "order".to_owned(),
        serde_json::json!(definition.order.unwrap_or(DEFAULT_ORDER)),
    );
    object.insert(
        "size".to_owned(),
        serde_json::json!(definition.size.unwrap_or(1)),
    );
    object.insert(
        "icon".to_owned(),
        serde_json::json!(definition.icon.clone().unwrap_or_else(|| {
            "curios:textures/gui/generic_icon.png".to_owned()
        })),
    );
    object.insert(
        "drop_rule".to_owned(),
        serde_json::json!(
            definition
                .drop_rule
                .map(|rule| rule.as_str().to_owned())
                .unwrap_or_else(|| "default".to_owned())
        ),
    );
    object.insert(
        "use_native_gui".to_owned(),
        serde_json::json!(definition.use_native_gui.unwrap_or(true)),
    );
    object.insert(
        "add_cosmetic".to_owned(),
        serde_json::json!(definition.add_cosmetic.unwrap_or(false)),
    );
    object.insert(
        "render_toggle".to_owned(),
        serde_json::json!(definition.render_toggle.unwrap_or(true)),
    );
    object.insert(
        "validator".to_owned(),
        serde_json::json!(
            definition
                .validators
                .as_ref()
                .and_then(|list| list.first())
                .cloned()
                .unwrap_or_else(|| TAG_VALIDATOR.to_owned())
        ),
    );
    let _ = id;
    serde_json::Value::Object(object).to_string()
}

/// Marks the next server tick for a rebuild when data packs are reloaded.
///
/// No files are contributed: the SDK's `data_pack_files` keys are only for the
/// kinds its tables read, and a mod-declared datapack registry is not among
/// them. Entries must be real pack files at [`entry_file`] paths.
pub fn data_pack_files(_event: &mut DataPackFilesEvent<'_>) {
    mark_pending();
}

fn required<'a>(
    fields: &'a HashMap<String, DatapackValue>,
    name: &str,
) -> Result<&'a DatapackValue, ReloadError> {
    fields
        .get(name)
        .ok_or_else(|| ReloadError::InvalidField(name.to_owned(), "missing".to_owned()))
}

fn int_field(
    fields: &HashMap<String, DatapackValue>,
    name: &str,
) -> Result<i32, ReloadError> {
    match required(fields, name)? {
        DatapackValue::Int(value) => {
            i32::try_from(*value).map_err(|_| {
                ReloadError::InvalidField(name.to_owned(), value.to_string())
            })
        }
        other => Err(ReloadError::InvalidField(name.to_owned(), format!("{other:?}"))),
    }
}

fn bool_field(
    fields: &HashMap<String, DatapackValue>,
    name: &str,
) -> Result<bool, ReloadError> {
    match required(fields, name)? {
        DatapackValue::Bool(value) => Ok(*value),
        other => Err(ReloadError::InvalidField(name.to_owned(), format!("{other:?}"))),
    }
}

fn text_field(
    fields: &HashMap<String, DatapackValue>,
    name: &str,
) -> Result<String, ReloadError> {
    match required(fields, name)? {
        DatapackValue::Text(value) => Ok(value.clone()),
        other => Err(ReloadError::InvalidField(name.to_owned(), format!("{other:?}"))),
    }
}

/// Decodes one registry entry into its slot id and definition.
pub fn definition_from_fields(
    entry: &str,
    values: Vec<(String, DatapackValue)>,
) -> Result<(String, SlotDefinition), ReloadError> {
    let fields: HashMap<String, DatapackValue> = values.into_iter().collect();
    // Keep the full `namespace:id`: `curios:ring` and `mymod:ring` must stay
    // distinct registry entries (review #20).
    let id = entry.to_owned();
    let rule = text_field(&fields, "drop_rule")?;
    let drop_rule = rule
        .parse::<DropRule>()
        .map_err(|_| ReloadError::InvalidField("drop_rule".to_owned(), rule.clone()))?;
    let definition = SlotDefinition {
        replace: false,
        id: None,
        order: Some(int_field(&fields, "order")?),
        size: Some(int_field(&fields, "size")?),
        operation: None,
        use_native_gui: Some(bool_field(&fields, "use_native_gui")?),
        add_cosmetic: Some(bool_field(&fields, "add_cosmetic")?),
        icon: Some(text_field(&fields, "icon")?),
        drop_rule: Some(drop_rule),
        render_toggle: Some(bool_field(&fields, "render_toggle")?),
        conditions: None,
        validators: Some(vec![text_field(&fields, "validator")?]),
        entities: None,
    };
    Ok((id, definition))
}

/// Decodes engine-provided registry entries into slot types.
///
/// The entry id stays qualified (`curios:ring`, `mymod:ring`) as the map key so
/// namespaces cannot collide, while `SlotType` is built from the bare id
/// (`ring`) as Java `ISlotType#getId` expects (review C).
pub fn decode_entries<I>(entries: I) -> Result<Vec<SlotType>, ReloadError>
where
    I: IntoIterator<Item = (String, Vec<(String, DatapackValue)>)>,
{
    let mut decoded: BTreeMap<String, SlotType> = BTreeMap::new();
    for (entry, values) in entries {
        let (id, definition) = definition_from_fields(&entry, values)?;
        let bare = slot_bare_id(&id)
            .map_err(|error| ReloadError::Slot(SlotError::InvalidSlotId(error)))?;
        // A later pack wins for duplicate qualified ids, as in Java datapacks.
        decoded.insert(id, SlotType::from_definition(&bare, &definition)?);
    }
    if decoded.is_empty() {
        return Err(ReloadError::EmptyRegistry);
    }
    Ok(decoded.into_values().collect())
}

/// Reads and decodes every entry of `curios:slot_type` in `dimension`.
pub fn read_slot_types(host: &dyn Host, dimension: &str) -> Result<Vec<SlotType>, ReloadError> {
    let level: mod_api::LevelRef = host
        .level(dimension)
        .ok_or_else(|| ReloadError::NoLevel(dimension.to_owned()))?;
    let entries = level
        .datapack_entries(host, REGISTRY_KEY)
        .ok_or(ReloadError::Read(DatapackReadError::UnknownRegistry))?;

    let mut read = Vec::new();
    for entry in entries {
        let handle = level.datapack_handle(host, REGISTRY_KEY, &entry)?;
        read.push((entry, level.datapack_read(host, &handle)?));
    }
    decode_entries(read)
}

/// Migrates every resident inventory row (players and mobs), including rows
/// whose owner is offline for the current dimension snapshot (review #22).
fn migrate_resident_inventories(host: &mut dyn ServerHost, types: &[SlotType]) {
    let Ok(batch) = host.query_mut(&inventory::INVENTORY) else {
        eprintln!("[curios] could not snapshot inventory attachments for migration");
        return;
    };
    for row in batch.iter() {
        let Ok(row) = row else {
            eprintln!("[curios] could not decode an inventory row during migration");
            continue;
        };
        let mut inventory = row.value().clone();
        let before = inventory.invalid_stacks().len();
        inventory.reload(types);
        if inventory.invalid_stacks().len() > before {
            eprintln!(
                "[curios] {} item(s) no longer fit after a slot reload",
                inventory.invalid_stacks().len() - before
            );
        }
        if let Err(error) = row.set(host, inventory) {
            eprintln!("[curios] could not save migrated inventory: {error:?}");
        }
    }
}

/// Applies a rebuilt registry and migrates every resident inventory, then
/// resynchronises the online players.
fn apply(types: Vec<SlotType>, host: &mut dyn ServerHost) {
    if let Err(error) = registry::replace_slot_types(types.clone()) {
        eprintln!("[curios] slot registry reload refused: {error}");
        return;
    }
    migrate_resident_inventories(host, &types);
    for player in host.players().server_players() {
        let uuid = player.uuid();
        let holder = Holder::Entity(player.entity().id());
        let Ok(inventory) = inventory::INVENTORY.get(host, holder) else {
            eprintln!("[curios] could not read inventory during slot reload");
            continue;
        };
        if let Err(error) = crate::net::sync_inventory(host, uuid, &inventory) {
            eprintln!("[curios] could not resync {uuid} after reload: {error:?}");
        }
        if let Err(error) = crate::net::sync_data(host, uuid, &types) {
            eprintln!("[curios] could not resync slot data to {uuid}: {error:?}");
        }
    }
}

/// Dimension name of the server's first loaded level, used for registry reads.
pub fn server_dimension(host: &dyn ServerHost) -> Option<String> {
    host.levels()
        .list_levels()
        .into_iter()
        .next()?
        .dimension()
        .id(host)
}

/// Deferred reload reaction called from `Hooks::server_tick`.
///
/// Does nothing unless [`mark_pending`] was called by `data_pack_files` or a
/// reload hook. The dimension comes from the live server; a failed read keeps
/// the pending flag so the next tick retries instead of losing the reload.
pub fn reload(host: &mut dyn ServerHost, _event: &mut ServerTickEvent<'_>) {
    if !take_pending() {
        return;
    }
    let Some(dimension) = server_dimension(host) else {
        mark_pending();
        eprintln!("[curios] slot datapack reload deferred: no level dimension is available");
        return;
    };
    match read_slot_types(host, &dimension) {
        Ok(types) => apply(types, host),
        Err(error) => {
            mark_pending();
            eprintln!("[curios] slot datapack reload failed, retrying next tick: {error}");
        }
    }
}

/// Converts a flat registry JSON file into the field/value list the engine
/// hands to `decode_entries`; used by tooling and by the shipped-entry tests.
pub fn flat_json_values(json: &str) -> Result<Vec<(String, DatapackValue)>, ReloadError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| ReloadError::InvalidField("json".to_owned(), error.to_string()))?;
    let object = parsed
        .as_object()
        .ok_or_else(|| ReloadError::InvalidField("json".to_owned(), "not an object".to_owned()))?;
    let mut values = Vec::new();
    for field in schema_fields() {
        let raw = object.get(&field.name).ok_or_else(|| {
            ReloadError::InvalidField(field.name.clone(), "missing".to_owned())
        })?;
        let value = match field.kind {
            DatapackFieldKind::Bool => DatapackValue::Bool(
                raw.as_bool()
                    .ok_or_else(|| ReloadError::InvalidField(field.name.clone(), format!("{raw}")))?,
            ),
            DatapackFieldKind::Int { .. } => DatapackValue::Int(
                raw.as_i64()
                    .ok_or_else(|| ReloadError::InvalidField(field.name.clone(), format!("{raw}")))?,
            ),
            DatapackFieldKind::Text { .. } | DatapackFieldKind::Identifier => {
                DatapackValue::Text(
                    raw.as_str()
                        .ok_or_else(|| {
                            ReloadError::InvalidField(field.name.clone(), format!("{raw}"))
                        })?
                        .to_owned(),
                )
            }
            _ => {
                return Err(ReloadError::InvalidField(
                    field.name.clone(),
                    "unsupported field kind".to_owned(),
                ))
            }
        };
        values.push((field.name.clone(), value));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eleven flat entries shipped at the SDK registry path.
    const FLAT_SLOTS: &[(&str, &str)] = &[
        ("curio", include_str!("../data/curios/curios/slot_type/curio.json")),
        ("head", include_str!("../data/curios/curios/slot_type/head.json")),
        ("necklace", include_str!("../data/curios/curios/slot_type/necklace.json")),
        ("back", include_str!("../data/curios/curios/slot_type/back.json")),
        ("body", include_str!("../data/curios/curios/slot_type/body.json")),
        ("bracelet", include_str!("../data/curios/curios/slot_type/bracelet.json")),
        ("hands", include_str!("../data/curios/curios/slot_type/hands.json")),
        ("ring", include_str!("../data/curios/curios/slot_type/ring.json")),
        ("belt", include_str!("../data/curios/curios/slot_type/belt.json")),
        ("feet", include_str!("../data/curios/curios/slot_type/feet.json")),
        ("charm", include_str!("../data/curios/curios/slot_type/charm.json")),
    ];

    #[test]
    fn schema_is_flat_and_defaults_match() {
        let fields = schema_fields();
        let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "order",
                "size",
                "icon",
                "drop_rule",
                "use_native_gui",
                "add_cosmetic",
                "render_toggle",
                "validator"
            ]
        );
        for field in &fields {
            assert!(field.default.is_some());
            assert!(!matches!(
                field.kind,
                DatapackFieldKind::Reference { .. } | DatapackFieldKind::Float { .. }
            ));
        }
        assert_eq!(REGISTRY_KEY, "curios:slot_type");
    }

    #[test]
    fn registry_entry_path_matches_sdk_declaration() {
        assert_eq!(
            entry_file("ring").as_deref(),
            Some("data/curios/curios/slot_type/ring.json")
        );
        assert_eq!(
            entry_file("back").as_deref(),
            Some("data/curios/curios/slot_type/back.json")
        );
        assert!(entry_file("").is_none());
        assert!(entry_file("curios:ring").is_none());
        assert!(entry_file("armor/ring").is_none());
    }

    #[test]
    fn data_pack_files_adds_no_registry_entries() {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut event = DataPackFilesEvent { files: &mut files };
        data_pack_files(&mut event);
        // The SDK exposes no overlay key that the registry decoder reads;
        // contributing `curios/slot_type/curios:<id>` would be silently ignored.
        assert!(files.is_empty());
        assert!(take_pending());
    }

    #[test]
    fn bundled_slots_flatten_to_schema_fields() {
        let expected: serde_json::Value = serde_json::from_str(
            r#"{"order":80,"size":1,"icon":"curios:slot/empty_back_slot","drop_rule":"default","use_native_gui":true,"add_cosmetic":false,"render_toggle":true,"validator":"curios:tag"}"#,
        )
        .unwrap();
        let definition = SlotDefinition::from_json(registry::BUILTIN[3].1).unwrap();
        assert_eq!(definition.order, Some(80));
        let flat: serde_json::Value =
            serde_json::from_str(&flat_entry("back", &definition)).unwrap();
        assert_eq!(flat, expected);
    }

    #[test]
    fn decodes_registry_fields_back_to_slot_definition() {
        let values = vec![
            ("order".to_owned(), DatapackValue::Int(160)),
            ("size".to_owned(), DatapackValue::Int(2)),
            (
                "icon".to_owned(),
                DatapackValue::Text("curios:slot/empty_ring_slot".to_owned()),
            ),
            ("drop_rule".to_owned(), DatapackValue::Text("destroy".to_owned())),
            ("use_native_gui".to_owned(), DatapackValue::Bool(true)),
            ("add_cosmetic".to_owned(), DatapackValue::Bool(false)),
            ("render_toggle".to_owned(), DatapackValue::Bool(true)),
            (
                "validator".to_owned(),
                DatapackValue::Text("curios:tag".to_owned()),
            ),
        ];
        let (id, definition) = definition_from_fields("curios:ring", values).unwrap();
        assert_eq!(id, "curios:ring");
        let slot = SlotType::from_definition(&id, &definition).unwrap();
        assert_eq!(slot.id(), "ring");
        assert_eq!(slot.qualified_id(), "curios:ring");
        assert_eq!(slot.order(), 160);
        assert_eq!(slot.size(), 2);
        assert_eq!(slot.icon().to_string(), "curios:slot/empty_ring_slot");
        assert_eq!(slot.drop_rule(), DropRule::Destroy);
    }

    #[test]
    fn invalid_field_types_are_rejected() {
        let values = vec![
            ("order".to_owned(), DatapackValue::Text("late".to_owned())),
            ("size".to_owned(), DatapackValue::Int(1)),
            (
                "icon".to_owned(),
                DatapackValue::Text("curios:slot/empty".to_owned()),
            ),
            ("drop_rule".to_owned(), DatapackValue::Text("default".to_owned())),
            ("use_native_gui".to_owned(), DatapackValue::Bool(true)),
            ("add_cosmetic".to_owned(), DatapackValue::Bool(false)),
            ("render_toggle".to_owned(), DatapackValue::Bool(true)),
            (
                "validator".to_owned(),
                DatapackValue::Text("curios:tag".to_owned()),
            ),
        ];
        assert!(matches!(
            definition_from_fields("curios:ring", values),
            Err(ReloadError::InvalidField(name, _)) if name == "order"
        ));
    }

    #[test]
    fn shipped_slot_type_entries_decode_to_builtin_slots() {
        let builtins = registry::builtin_slot_types().unwrap();
        assert_eq!(FLAT_SLOTS.len(), 11);
        // Drive the same production decoder that `read_slot_types` uses with
        // the exact field/value lists the engine hands it for our 11 files.
        let entries = FLAT_SLOTS
            .iter()
            .map(|(id, json)| {
                (
                    format!("curios:{id}"),
                    flat_json_values(json).expect("flat entry decodes"),
                )
            })
            .collect::<Vec<_>>();
        let decoded = decode_entries(entries).expect("all 11 entries decode");
        assert_eq!(decoded.len(), 11);
        for (id, _) in FLAT_SLOTS {
            let slot = decoded
                .iter()
                .find(|slot| slot.id() == *id && slot.qualified_id() == format!("curios:{id}"))
                .unwrap_or_else(|| panic!("missing slot {id}"));
            let expected = builtins
                .iter()
                .find(|builtin| builtin.id() == *id)
                .unwrap();
            assert_eq!(slot.order(), expected.order(), "slot {id}");
            assert_eq!(slot.size(), expected.size(), "slot {id}");
            assert_eq!(slot.icon(), expected.icon(), "slot {id}");
            assert_eq!(slot.drop_rule(), expected.drop_rule(), "slot {id}");
            assert_eq!(slot.validators(), expected.validators(), "slot {id}");
            assert_eq!(slot, expected, "slot {id}");
        }
    }

    #[test]
    fn namespaced_entries_do_not_collide() {
        let values =
            flat_json_values(include_str!("../data/curios/curios/slot_type/ring.json"))
                .unwrap();
        let (curios_id, curios_def) = definition_from_fields("curios:ring", values.clone()).unwrap();
        let (mymod_id, mymod_def) = definition_from_fields("mymod:ring", values).unwrap();
        assert_eq!(curios_id, "curios:ring");
        assert_eq!(mymod_id, "mymod:ring");
        let bundled = SlotType::from_definition(&curios_id, &curios_def).unwrap();
        let external = SlotType::from_definition(&mymod_id, &mymod_def).unwrap();
        assert_eq!(bundled.id(), external.id());
        assert_ne!(bundled.qualified_id(), external.qualified_id());
        assert_ne!(bundled, external);
    }

    #[test]
    fn pending_flag_is_drained_once() {
        mark_pending();
        assert!(take_pending());
        assert!(!take_pending());
    }
}
