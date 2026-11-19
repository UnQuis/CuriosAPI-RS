//! Pure domain model for Curio slot types.
//!
//! Adapted from `top.theillusivec4.curios.common.slot.SlotType`,
//! `common.data.SlotData`, `api.type.ISlotType`, `api.SlotContext` and
//! `api.common.slot.SlotTypePredicate` of TheIllusiveC4/Curios
//! (LGPL-3.0-or-later). This module performs no Horizon API calls: icons,
//! validators and entity selectors are plain validated identifiers, and item
//! acceptance is delegated to a caller-supplied predicate function.
//!
//! Defaults follow `SlotType.Builder#build` from the Java reference:
//! `order = 1000`, `size = 1`, `use_native_gui = true`, `has_cosmetic = false`,
//! `render_toggle = true`, `icon = curios:textures/gui/generic_icon.png`,
//! `drop_rule = default`, `validators = { curios:tag }`.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize, Serializer};

use crate::drop::{DropRule, InvalidDropRule};

/// Fallback icon used by the Java `ISlotType.GENERIC_ICON`.
pub const GENERIC_ICON: &str = "curios:textures/gui/generic_icon.png";
/// `SlotType.Builder#build` default when a slot defines no order.
pub const DEFAULT_ORDER: i32 = 1000;
/// `SlotType.Builder#build` default slot count.
pub const DEFAULT_SIZE: i32 = 1;
/// Built-in validator registered by `CuriosSlots` for every slot id.
pub const TAG_VALIDATOR: &str = "curios:tag";

/// A validated Minecraft-style resource identifier (`namespace:path`).
///
/// A missing namespace is resolved to `minecraft`, as in Java
/// `Identifier.parse`. Path characters are restricted to the Java charset
/// `[a-z0-9/._-]`; empty segments and leading/trailing slashes are rejected.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ResourceId {
    namespace: String,
    path: String,
}

impl ResourceId {
    /// Builds a `curios:<path>` identifier from a known-valid path.
    pub fn curios(path: &str) -> Self {
        Self {
            namespace: "curios".to_owned(),
            path: path.to_owned(),
        }
    }

    /// Namespace component.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Path component.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Full identifier including the namespace separator.
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.namespace, self.path)
    }
}

/// Error for a resource identifier that Java `Identifier` would reject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidResourceId(pub String);

impl fmt::Display for InvalidResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid resource id `{}` (expected [namespace:]path with [a-z0-9._-] namespace and [a-z0-9/._-] path)",
            self.0
        )
    }
}

impl Error for InvalidResourceId {}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

fn valid_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("//")
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-' | b'/'))
}

impl FromStr for ResourceId {
    type Err = InvalidResourceId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (namespace, path) = match value.split_once(':') {
            Some((namespace, path)) => (namespace, path),
            None => ("minecraft", value),
        };
        if !valid_namespace(namespace) || !valid_path(path) {
            return Err(InvalidResourceId(value.to_owned()));
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        })
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl Serialize for ResourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ResourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// Error for a slot id that cannot be used as a `curios:<id>` tag path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidSlotId(pub String);

impl fmt::Display for InvalidSlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid slot id `{}` (expected a non-empty path without a namespace, using [a-z0-9/._-])",
            self.0
        )
    }
}

impl Error for InvalidSlotId {}

/// Validates a bare slot id, which the Java `tag` predicate embeds into
/// `curios:<id>` via `Identifier.fromNamespaceAndPath`.
pub fn validate_slot_id(id: &str) -> Result<(), InvalidSlotId> {
    if id.contains(':') || !valid_path(id) {
        Err(InvalidSlotId(id.to_owned()))
    } else {
        Ok(())
    }
}

/// Parses a bare or namespaced slot id into `(namespace, bare id)`.
///
/// Bare ids belong to `curios` (the bundled slot namespace); data-pack entries
/// such as `mymod:ring` keep their own namespace so they cannot collide with
/// a bundled slot of the same bare id (review #20).
pub fn parse_qualified_slot_id(id: &str) -> Result<(String, String), InvalidSlotId> {
    match id.split_once(':') {
        Some((namespace, path)) => {
            if !valid_namespace(namespace) || !valid_path(path) {
                return Err(InvalidSlotId(id.to_owned()));
            }
            Ok((namespace.to_owned(), path.to_owned()))
        }
        None => {
            validate_slot_id(id)?;
            Ok(("curios".to_owned(), id.to_owned()))
        }
    }
}

/// Normalises a bare or namespaced slot id to its canonical `namespace:id` form.
pub fn qualify_slot_id(id: &str) -> String {
    match parse_qualified_slot_id(id) {
        Ok((namespace, path)) => format!("{namespace}:{path}"),
        Err(_) => id.to_owned(),
    }
}

/// A registered slot validator identifier (e.g. `curios:tag`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotValidator(ResourceId);

impl SlotValidator {
    /// Returns the built-in tag validator applied to slots that declare none.
    pub fn tag() -> Self {
        Self(ResourceId::curios("tag"))
    }

    /// Parses a validator identifier.
    pub fn parse(value: &str) -> Result<Self, InvalidResourceId> {
        Ok(Self(ResourceId::from_str(value)?))
    }

    /// Underlying resource identifier.
    pub fn resource_id(&self) -> &ResourceId {
        &self.0
    }
}

impl FromStr for SlotValidator {
    type Err = InvalidResourceId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for SlotValidator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An entity selector from a slot definition: a tag (`#namespace:path`) or a
/// concrete entity type (`namespace:path`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum EntitySelector {
    /// Tag reference, written with a leading `#` in data files.
    Tag(ResourceId),
    /// Concrete entity type identifier.
    Type(ResourceId),
}

impl EntitySelector {
    /// Parses a Java `TAG_OR_ELEMENT_ID` string.
    pub fn parse(value: &str) -> Result<Self, InvalidResourceId> {
        if let Some(rest) = value.strip_prefix('#') {
            Ok(Self::Tag(ResourceId::from_str(rest)?))
        } else {
            Ok(Self::Type(ResourceId::from_str(value)?))
        }
    }
}

impl fmt::Display for EntitySelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(id) => write!(f, "#{id}"),
            Self::Type(id) => id.fmt(f),
        }
    }
}

/// Errors produced while decoding or validating slot data.
#[derive(Debug)]
pub enum SlotError {
    /// Malformed JSON or field of the wrong type.
    Json(serde_json::Error),
    /// Slot id cannot be embedded in a `curios:<id>` tag.
    InvalidSlotId(InvalidSlotId),
    /// Icon, validator or entity identifier is malformed.
    InvalidResourceId(InvalidResourceId),
    /// `drop_rule` is not a Java serialized drop rule name.
    InvalidDropRule(InvalidDropRule),
    /// `size` is negative.
    InvalidSize(i32),
    /// `operation` is not `set`, `add` or `remove`.
    InvalidOperation(String),
    /// A reload produced no slot definitions at all.
    EmptyRegistry,
    /// Two entries resolved to the same canonical `namespace:id`.
    DuplicateSlotId(String),
    /// The process-wide registry was already initialised and cannot be replaced.
    RegistryAlreadyInitialised,
}

impl fmt::Display for SlotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(err) => write!(f, "invalid slot JSON: {err}"),
            Self::InvalidSlotId(err) => err.fmt(f),
            Self::InvalidResourceId(err) => err.fmt(f),
            Self::InvalidDropRule(err) => err.fmt(f),
            Self::InvalidSize(size) => write!(f, "slot size must be non-negative, got {size}"),
            Self::InvalidOperation(op) => write!(
                f,
                "unknown slot size operation `{op}` (expected set, add or remove)"
            ),
            Self::EmptyRegistry => f.write_str("slot registry cannot be empty"),
            Self::DuplicateSlotId(id) => write!(f, "duplicate slot id `{id}`"),
            Self::RegistryAlreadyInitialised => {
                f.write_str("slot registry was already initialised")
            }
        }
    }
}

impl Error for SlotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            Self::InvalidSlotId(err) => Some(err),
            Self::InvalidResourceId(err) => Some(err),
            Self::InvalidDropRule(err) => Some(err),
            Self::InvalidSize(_)
            | Self::InvalidOperation(_)
            | Self::DuplicateSlotId(_)
            | Self::EmptyRegistry
            | Self::RegistryAlreadyInitialised => None,
        }
    }
}

impl From<serde_json::Error> for SlotError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<InvalidSlotId> for SlotError {
    fn from(err: InvalidSlotId) -> Self {
        Self::InvalidSlotId(err)
    }
}

impl From<InvalidResourceId> for SlotError {
    fn from(err: InvalidResourceId) -> Self {
        Self::InvalidResourceId(err)
    }
}

impl From<InvalidDropRule> for SlotError {
    fn from(err: InvalidDropRule) -> Self {
        Self::InvalidDropRule(err)
    }
}

/// Data-pack shape of `data/<namespace>/curios/slots/<id>.json`.
///
/// Every field mirrors `ISlotData.Entry`'s codec field names and defaults in
/// the Java reference. Unknown fields are ignored, as by the Java codec.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SlotDefinition {
    /// Replace the accumulated definition instead of merging into it.
    pub replace: bool,
    /// Optional embedded id (the file path supplies the id in the reference).
    pub id: Option<String>,
    /// Ascending display priority; lower values come first.
    pub order: Option<i32>,
    /// Slot count or operand, depending on `operation`.
    pub size: Option<i32>,
    /// `set` (default), `add` or `remove`.
    pub operation: Option<String>,
    /// Whether the slot is shown in the native Curios GUI.
    pub use_native_gui: Option<bool>,
    /// Java field `add_cosmetic`.
    pub add_cosmetic: Option<bool>,
    /// Slot background icon identifier.
    pub icon: Option<String>,
    /// Java serialized drop rule name.
    pub drop_rule: Option<DropRule>,
    /// Whether rendering can be toggled for this slot.
    pub render_toggle: Option<bool>,
    /// Loading conditions (not evaluated in the pure domain).
    pub conditions: Option<Vec<serde_json::Value>>,
    /// Validator identifiers; defaults to `curios:tag`.
    pub validators: Option<Vec<String>>,
    /// Entity type ids, optionally `#`-prefixed for tags.
    pub entities: Option<Vec<String>>,
}

impl SlotDefinition {
    /// Decodes a slot definition from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, SlotError> {
        Ok(serde_json::from_str(json)?)
    }
}

/// Slot type with validated identifiers and no Horizon dependencies.
// Eq/PartialEq are implemented manually below to stay consistent with Ord.
#[derive(Clone, Debug)]
pub struct SlotType {
    id: String,
    namespace: String,
    order: i32,
    size: i32,
    use_native_gui: bool,
    has_cosmetic: bool,
    icon: ResourceId,
    drop_rule: DropRule,
    render_toggle: bool,
    validators: BTreeSet<SlotValidator>,
    entities: BTreeSet<EntitySelector>,
}

impl SlotType {
    /// Creates a slot with the Java `SlotType.Builder#build` defaults.
    pub fn new(id: &str) -> Result<Self, SlotError> {
        let (namespace, bare) = parse_qualified_slot_id(id)?;
        let _ = validate_slot_id(&bare);
        Ok(Self {
            id: bare,
            namespace,
            order: DEFAULT_ORDER,
            size: DEFAULT_SIZE,
            use_native_gui: true,
            has_cosmetic: false,
            icon: ResourceId::from_str(GENERIC_ICON)?,
            drop_rule: DropRule::Default,
            render_toggle: true,
            validators: BTreeSet::from([SlotValidator::tag()]),
            entities: BTreeSet::new(),
        })
    }

    /// Builds a slot from a decoded data-pack definition.
    ///
    /// The `operation` handling mirrors `SlotType.Builder#apply`: `add` and
    /// `remove` modify the default size of one, and `set` assigns the size.
    /// A `replace` definition with no `validators` gets an empty validator set
    /// instead of the built-in `curios:tag`.
    pub fn from_definition(id: &str, definition: &SlotDefinition) -> Result<Self, SlotError> {
        let mut slot = Self::new(id)?;
        slot.order = definition.order.unwrap_or(DEFAULT_ORDER);

        let operand = definition.size.unwrap_or(DEFAULT_SIZE);
        if operand < 0 {
            return Err(SlotError::InvalidSize(operand));
        }
        slot.size = match definition.operation.as_deref().unwrap_or("set") {
            "set" => operand,
            "add" => DEFAULT_SIZE + operand,
            "remove" => (DEFAULT_SIZE - operand).max(0),
            other => return Err(SlotError::InvalidOperation(other.to_owned())),
        };

        slot.use_native_gui = definition.use_native_gui.unwrap_or(true);
        slot.has_cosmetic = definition.add_cosmetic.unwrap_or(false);
        slot.render_toggle = definition.render_toggle.unwrap_or(true);
        slot.icon = match &definition.icon {
            Some(icon) => ResourceId::from_str(icon)?,
            None => ResourceId::from_str(GENERIC_ICON)?,
        };
        slot.drop_rule = definition.drop_rule.unwrap_or_default();

        match &definition.validators {
            Some(list) => {
                slot.validators = list
                    .iter()
                    .map(|value| SlotValidator::parse(value))
                    .collect::<Result<BTreeSet<_>, _>>()?;
            }
            None if definition.replace => slot.validators.clear(),
            None => {}
        }

        if let Some(list) = &definition.entities {
            slot.entities = list
                .iter()
                .map(|value| EntitySelector::parse(value))
                .collect::<Result<BTreeSet<_>, _>>()?;
        }
        Ok(slot)
    }

    /// Decodes and builds a slot from a JSON string.
    pub fn from_json(id: &str, json: &str) -> Result<Self, SlotError> {
        Self::from_definition(id, &SlotDefinition::from_json(json)?)
    }

    /// Bare slot id (`ring`), unchanged from the Java `ISlotType#getId`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Namespace of the slot definition (`curios` for bundled slots).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Canonical `namespace:id` (`curios:ring`, `mymod:ring`).
    pub fn qualified_id(&self) -> String {
        format!("{}:{}", self.namespace, self.id)
    }

    /// Ascending display priority; lower values come first.
    pub const fn order(&self) -> i32 {
        self.order
    }

    /// Default number of slots for this type.
    pub const fn size(&self) -> i32 {
        self.size
    }

    /// Whether the native Curios GUI shows this slot type.
    pub const fn use_native_gui(&self) -> bool {
        self.use_native_gui
    }

    /// Whether cosmetic slot pairs are enabled.
    pub const fn has_cosmetic(&self) -> bool {
        self.has_cosmetic
    }

    /// Slot background icon identifier.
    pub const fn icon(&self) -> &ResourceId {
        &self.icon
    }

    /// Drop rule used when the item and event overrides defer to the slot.
    pub const fn drop_rule(&self) -> DropRule {
        self.drop_rule
    }

    /// Whether rendering can be toggled for this slot type.
    pub const fn render_toggle(&self) -> bool {
        self.render_toggle
    }

    /// Validators in deterministic identifier order.
    pub fn validators(&self) -> &BTreeSet<SlotValidator> {
        &self.validators
    }

    /// Default entity selectors in deterministic order.
    pub fn entities(&self) -> &BTreeSet<EntitySelector> {
        &self.entities
    }

    /// Overrides the display priority.
    pub fn with_order(mut self, order: i32) -> Self {
        self.order = order;
        self
    }

    /// Overrides the default slot count.
    pub fn with_size(mut self, size: i32) -> Self {
        self.size = size;
        self
    }

    /// Overrides the icon identifier.
    pub fn with_icon(mut self, icon: &str) -> Result<Self, SlotError> {
        self.icon = ResourceId::from_str(icon)?;
        Ok(self)
    }

    /// Overrides the slot drop rule.
    pub const fn with_drop_rule(mut self, rule: DropRule) -> Self {
        self.drop_rule = rule;
        self
    }

    /// Tests an item against the slot validators, mirroring Java
    /// `CuriosSlotTypes.testPredicates`: any registered predicate passing is
    /// enough, and an empty validator set accepts nothing.
    pub fn is_item_valid<I, F>(&self, context: &SlotContext, item: &I, test: F) -> bool
    where
        F: Fn(&SlotValidator, &SlotContext, &I) -> bool,
    {
        self.validators
            .iter()
            .any(|validator| test(validator, context, item))
    }
}

impl Ord for SlotType {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order.cmp(&other.order).then_with(|| {
            self.namespace
                .cmp(&other.namespace)
                .then_with(|| self.id.cmp(&other.id))
        })
    }
}

impl PartialOrd for SlotType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SlotType {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SlotType {}

/// Domain-level counterpart of the Java `SlotContext` record.
///
/// The wearing entity is represented by an opaque id so this module stays free
/// of Horizon types; the inventory layer supplies it when available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotContext {
    /// Slot type id.
    pub identifier: String,
    /// Opaque entity id, if the caller has one.
    pub entity: Option<u64>,
    /// Index within this slot type.
    pub index: usize,
    /// Whether this is the cosmetic slot pair.
    pub cosmetic: bool,
    /// Java record component `visible`.
    pub visible: bool,
}

impl SlotContext {
    /// Creates a context for a slot.
    pub fn new(
        identifier: impl Into<String>,
        entity: Option<u64>,
        index: usize,
        cosmetic: bool,
        visible: bool,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            entity,
            index,
            cosmetic,
            visible,
        }
    }
}

/// Inclusive integer bounds used by `SlotTypePredicate#size`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntRange {
    /// Inclusive lower bound.
    #[serde(default)]
    pub min: Option<i32>,
    /// Inclusive upper bound.
    #[serde(default)]
    pub max: Option<i32>,
}

impl IntRange {
    /// Unbounded range (the Java `MinMaxBounds.Ints.ANY`).
    pub const ANY: Self = Self {
        min: None,
        max: None,
    };

    /// Range matching exactly one value.
    pub const fn exactly(value: i32) -> Self {
        Self {
            min: Some(value),
            max: Some(value),
        }
    }

    /// Whether `value` lies within both bounds.
    pub fn matches(self, value: i32) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

/// Data-driven filter over slot types (Java `SlotTypePredicate`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SlotTypePredicate {
    /// Slot ids to compare against; empty matches any id.
    pub ids: Vec<String>,
    /// Allowed default slot sizes.
    pub size: Option<IntRange>,
    /// Required drop rule.
    pub drop_rule: Option<DropRule>,
    /// Required cosmetic-slot flag.
    pub has_cosmetic: Option<bool>,
    /// Required render-toggle flag.
    pub can_toggle_rendering: Option<bool>,
}

impl SlotTypePredicate {
    /// Predicate matching any slot type.
    pub const ANY: Self = Self {
        ids: Vec::new(),
        size: None,
        drop_rule: None,
        has_cosmetic: None,
        can_toggle_rendering: None,
    };

    /// Checks a slot type against this predicate.
    ///
    /// Like the Java implementation, every listed id must equal the slot id;
    /// all absent fields are skipped.
    pub fn matches(&self, slot: &SlotType) -> bool {
        self.ids.iter().all(|id| id == slot.id())
            && self.size.is_none_or(|range| range.matches(slot.size()))
            && self.drop_rule.is_none_or(|rule| rule == slot.drop_rule())
            && self.has_cosmetic.is_none_or(|flag| flag == slot.has_cosmetic())
            && self
                .can_toggle_rendering
                .is_none_or(|flag| flag == slot.render_toggle())
    }

    /// Starts building a predicate.
    pub fn builder() -> SlotTypePredicateBuilder {
        SlotTypePredicateBuilder::default()
    }
}

/// Builder for [`SlotTypePredicate`].
#[derive(Clone, Debug, Default)]
pub struct SlotTypePredicateBuilder {
    predicate: SlotTypePredicate,
}

impl SlotTypePredicateBuilder {
    /// Adds an id to compare against.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.predicate.ids.push(id.into());
        self
    }

    /// Requires an exact default slot size.
    pub fn with_size(mut self, size: i32) -> Self {
        self.predicate.size = Some(IntRange::exactly(size));
        self
    }

    /// Requires a default slot size inside `range`.
    pub fn with_size_range(mut self, range: IntRange) -> Self {
        self.predicate.size = Some(range);
        self
    }

    /// Requires a drop rule.
    pub fn with_drop_rule(mut self, rule: DropRule) -> Self {
        self.predicate.drop_rule = Some(rule);
        self
    }

    /// Requires the cosmetic-slot flag.
    pub fn has_cosmetic(mut self, flag: bool) -> Self {
        self.predicate.has_cosmetic = Some(flag);
        self
    }

    /// Requires the render-toggle flag.
    pub fn can_toggle_rendering(mut self, flag: bool) -> Self {
        self.predicate.can_toggle_rendering = Some(flag);
        self
    }

    /// Completes the predicate.
    pub fn build(self) -> SlotTypePredicate {
        self.predicate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All eleven data-pack slots from the Java reference, verbatim.
    const SLOTS: &[(&str, &str)] = &[
        ("back", include_str!("../data/curios/curios/slots/back.json")),
        ("belt", include_str!("../data/curios/curios/slots/belt.json")),
        ("body", include_str!("../data/curios/curios/slots/body.json")),
        ("bracelet", include_str!("../data/curios/curios/slots/bracelet.json")),
        ("charm", include_str!("../data/curios/curios/slots/charm.json")),
        ("curio", include_str!("../data/curios/curios/slots/curio.json")),
        ("feet", include_str!("../data/curios/curios/slots/feet.json")),
        ("hands", include_str!("../data/curios/curios/slots/hands.json")),
        ("head", include_str!("../data/curios/curios/slots/head.json")),
        ("necklace", include_str!("../data/curios/curios/slots/necklace.json")),
        ("ring", include_str!("../data/curios/curios/slots/ring.json")),
    ];

    const EXPECTED_ORDERS: &[(&str, i32)] = &[
        ("back", 80),
        ("belt", 180),
        ("body", 100),
        ("bracelet", 120),
        ("charm", 200),
        ("curio", 20),
        ("feet", 190),
        ("hands", 140),
        ("head", 40),
        ("necklace", 60),
        ("ring", 160),
    ];

    #[test]
    fn parses_all_eleven_reference_slots() {
        assert_eq!(SLOTS.len(), 11);
        assert_eq!(EXPECTED_ORDERS.len(), 11);
        for (id, json) in SLOTS {
            let slot = SlotType::from_json(id, json).unwrap();
            let expected_order = EXPECTED_ORDERS
                .iter()
                .find(|(name, _)| name == id)
                .map(|(_, order)| *order)
                .expect("every slot has an expected order");
            assert_eq!(slot.order(), expected_order, "slot {id}");
            assert_eq!(slot.id(), *id);
            assert_eq!(slot.size(), DEFAULT_SIZE);
            assert_eq!(slot.icon().to_string(), format!("curios:slot/empty_{id}_slot"));
            assert!(slot.use_native_gui());
            assert!(!slot.has_cosmetic());
            assert!(slot.render_toggle());
            assert_eq!(slot.drop_rule(), DropRule::Default);
            assert_eq!(slot.validators().len(), 1);
            assert!(slot.validators().contains(&SlotValidator::tag()));
            assert!(slot.entities().is_empty());
        }
    }

    #[test]
    fn sort_is_order_then_id() {
        let mut slots = vec![
            SlotType::new("z").unwrap().with_order(10),
            SlotType::new("a").unwrap().with_order(10),
            SlotType::new("early").unwrap().with_order(-1),
        ];
        slots.sort();
        let ids: Vec<&str> = slots.iter().map(|slot| slot.id()).collect();
        assert_eq!(ids, ["early", "a", "z"]);
        assert!(slots[1] < slots[2]);
        assert_eq!(slots[1], SlotType::new("a").unwrap().with_order(10));
        assert_eq!(slots[1].cmp(&slots[1]), Ordering::Equal);
    }

    #[test]
    fn qualified_slot_ids_keep_namespaces_separate() {
        let bundled = SlotType::new("ring").unwrap();
        assert_eq!(bundled.id(), "ring");
        assert_eq!(bundled.namespace(), "curios");
        assert_eq!(bundled.qualified_id(), "curios:ring");

        let external = SlotType::new("mymod:ring").unwrap();
        assert_eq!(external.id(), "ring");
        assert_eq!(external.namespace(), "mymod");
        assert_eq!(external.qualified_id(), "mymod:ring");
        assert_ne!(bundled, external);
        assert_eq!(qualify_slot_id("ring"), "curios:ring");
        assert_eq!(qualify_slot_id("mymod:ring"), "mymod:ring");
        assert!(parse_qualified_slot_id("Bad:ID").is_err());
    }

    #[test]
    fn sorting_all_reference_slots_is_deterministic() {
        let mut slots = SLOTS
            .iter()
            .map(|(id, json)| SlotType::from_json(id, json).unwrap())
            .collect::<Vec<_>>();
        slots.sort();
        let ids: Vec<&str> = slots.iter().map(|slot| slot.id()).collect();
        assert_eq!(
            ids,
            [
                "curio", "head", "necklace", "back", "body", "bracelet", "hands", "ring", "belt",
                "feet", "charm"
            ]
        );
    }

    #[test]
    fn invalid_inputs_are_rejected_explicitly() {
        assert!(matches!(
            validate_slot_id(""),
            Err(InvalidSlotId(_))
        ));
        assert!(validate_slot_id("curios:ring").is_err());
        assert!(validate_slot_id("Bad_ID").is_err());
        assert!(validate_slot_id("bad//id").is_err());
        assert!(validate_slot_id("ring").is_ok());

        assert!(matches!(
            SlotType::from_json("ring", "{not json}"),
            Err(SlotError::Json(_))
        ));
        assert!(matches!(
            SlotType::from_json("ring", r#"{"order":"late"}"#),
            Err(SlotError::Json(_))
        ));
        assert!(matches!(
            SlotType::from_json("ring", r#"{"icon":"BAD!!"}"#),
            Err(SlotError::InvalidResourceId(_))
        ));
        assert!(matches!(
            SlotType::from_json("ring", r#"{"validators":["bad id"]}"#),
            Err(SlotError::InvalidResourceId(_))
        ));
        // serde reports an unknown drop rule as a JSON decode error; the
        // explicit InvalidDropRule variant is produced by `FromStr`.
        assert!(matches!(
            SlotType::from_json("ring", r#"{"drop_rule":"often"}"#),
            Err(SlotError::Json(_))
        ));
        assert!(matches!(
            "often".parse::<DropRule>(),
            Err(InvalidDropRule(_))
        ));
        assert!(matches!(
            SlotType::from_json("ring", r#"{"size":-3}"#),
            Err(SlotError::InvalidSize(-3))
        ));
        assert!(matches!(
            SlotType::from_json("ring", r#"{"operation":"multiply"}"#),
            Err(SlotError::InvalidOperation(_))
        ));
        assert!(matches!(
            SlotType::from_json("BAD:ID", "{}"),
            Err(SlotError::InvalidSlotId(_))
        ));
    }

    #[test]
    fn resource_ids_follow_identifier_syntax() {
        assert_eq!(
            "curios:tag".parse::<ResourceId>().unwrap().to_string(),
            "curios:tag"
        );
        assert_eq!(
            "tag".parse::<ResourceId>().unwrap().to_string(),
            "minecraft:tag"
        );
        assert_eq!("curios:tag".parse::<SlotValidator>().unwrap(), SlotValidator::tag());
        for bad in ["ABC:x", "x:", ":tag", "curios:", "curios:Bad", "curios:a//b"] {
            assert!(ResourceId::from_str(bad).is_err(), "expected rejection: {bad}");
        }
        assert_eq!(
            EntitySelector::parse("#curios:player_like").unwrap(),
            EntitySelector::Tag(ResourceId::curios("player_like"))
        );
        assert_eq!(
            EntitySelector::parse("minecraft:player").unwrap().to_string(),
            "minecraft:player"
        );
        assert!(EntitySelector::parse("#").is_err());
    }

    #[test]
    fn definition_operations_and_replace_match_builder() {
        let added = SlotDefinition::from_json(r#"{"operation":"add","size":2}"#).unwrap();
        assert_eq!(SlotType::from_definition("ring", &added).unwrap().size(), 3);
        let removed = SlotDefinition::from_json(r#"{"operation":"remove","size":5}"#).unwrap();
        assert_eq!(SlotType::from_definition("ring", &removed).unwrap().size(), 0);
        let replaced = SlotDefinition::from_json(r#"{"replace":true,"size":0}"#).unwrap();
        let slot = SlotType::from_definition("ring", &replaced).unwrap();
        assert!(slot.validators().is_empty());
        assert_eq!(slot.size(), 0);
    }

    #[test]
    fn predicate_and_item_validation() {
        let slot = SlotType::new("ring")
            .unwrap()
            .with_order(10)
            .with_size(2)
            .with_drop_rule(DropRule::AlwaysKeep);
        let predicate = SlotTypePredicate::builder()
            .with_id("ring")
            .with_size(2)
            .with_drop_rule(DropRule::AlwaysKeep)
            .has_cosmetic(false)
            .can_toggle_rendering(true)
            .build();
        assert!(predicate.matches(&slot));
        assert!(!predicate.matches(&SlotType::new("necklace").unwrap()));
        assert!(!SlotTypePredicate::builder().with_size(3).build().matches(&slot));
        assert!(SlotTypePredicate::ANY.matches(&slot));

        let context = SlotContext::new("ring", None, 0, false, true);
        let accepted = slot.is_item_valid(&context, &"curios:ring", |validator, ctx, item| {
            validator == &SlotValidator::tag()
                && ctx.identifier == "ring"
                && item.starts_with("curios:")
        });
        assert!(accepted);
        let empty = SlotType::new("plain")
            .unwrap()
            .with_icon("curios:slot/empty")
            .unwrap();
        let replaced = SlotDefinition::from_json(r#"{"replace":true}"#).unwrap();
        let no_validators = SlotType::from_definition("plain", &replaced).unwrap();
        assert!(!no_validators.is_item_valid(&context, &"curios:ring", |_, _, _| true));
        assert_eq!(empty.icon().to_string(), "curios:slot/empty");
    }
}
