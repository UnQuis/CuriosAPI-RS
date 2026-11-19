//! Curio attribute modifiers and dynamic slot sizes.
//!
//! Adapted from `CurioAttributeModifiers`, `SlotAttribute`,
//! `CuriosStacksHandler#update` and `SlotTypePredicate` of TheIllusiveC4/Curios
//! (LGPL-3.0-or-later, copyright 2018-2024 C4).
//!
//! Horizon exposes only `EquipmentSlotGroup` on item attribute modifiers, so
//! Curio slots cannot be named there. The bridge in [`item_attribute_modifier`]
//! gates every modifier behind the `curios:attribute_modifiers` data component
//! on the stack and installs it for `EquipmentSlotGroup::Any`. Register that
//! component id through [`register_component_types`] from `Hooks::component_types`.

use std::error::Error;
use std::fmt;

use mod_api::{
    AttributeModifier, AttributeOperation, Component, ComponentTypesEvent, EquipmentSlotGroup,
    Identifier, ItemAttributeModifierEvent, ItemAttributeModifiers, Tag,
};
use serde::{Deserialize, Serialize};

use crate::drop::DropRule;
use crate::slots::{IntRange, SlotType, SlotTypePredicate};

/// Data component that gates Curio attribute modifiers on an item stack.
pub const ATTRIBUTE_COMPONENT_ID: &str = "curios:attribute_modifiers";

/// Adds the Curio component id to a `component_types` collection event once.
pub fn register_component_types(event: &mut ComponentTypesEvent<'_>) {
    if !event.types.iter().any(|id| id == ATTRIBUTE_COMPONENT_ID) {
        event.types.push(ATTRIBUTE_COMPONENT_ID.to_owned());
    }
}

/// Java `AttributeModifier.Operation` serialized names.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierOperation {
    /// Java `ADD_VALUE`.
    AddValue,
    /// Java `ADD_MULTIPLIED_BASE`.
    AddMultipliedBase,
    /// Java `ADD_MULTIPLIED_TOTAL`.
    AddMultipliedTotal,
}

impl ModifierOperation {
    /// Java serialized operation name, also used by the inventory codec.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddValue => "add_value",
            Self::AddMultipliedBase => "add_multiplied_base",
            Self::AddMultipliedTotal => "add_multiplied_total",
        }
    }
}

/// Parses a serialized modifier operation for persistent slot-size modifiers.
pub fn parse_modifier_operation(value: &str) -> Option<ModifierOperation> {
    match value {
        "add_value" => Some(ModifierOperation::AddValue),
        "add_multiplied_base" => Some(ModifierOperation::AddMultipliedBase),
        "add_multiplied_total" => Some(ModifierOperation::AddMultipliedTotal),
        _ => None,
    }
}

impl From<ModifierOperation> for AttributeOperation {
    fn from(value: ModifierOperation) -> Self {
        match value {
            ModifierOperation::AddValue => Self::AddValue,
            ModifierOperation::AddMultipliedBase => Self::AddMultipliedBase,
            ModifierOperation::AddMultipliedTotal => Self::AddMultipliedTotal,
        }
    }
}

/// What a modifier applies to: a vanilla attribute or a Curio slot pseudo
/// attribute (Java `SlotAttribute`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeTarget {
    /// A slot modifier written as `{"slot": "<slot id>"}`.
    Slot {
        /// Slot type id.
        slot: String,
    },
    /// A vanilla attribute identifier written as a plain string.
    Vanilla(String),
}

impl AttributeTarget {
    /// Canonical attribute identifier used in tooltips and Horizon bridges.
    pub fn attribute_id(&self) -> String {
        match self {
            Self::Slot { slot } => SlotAttribute::get_or_create(slot).java_name(),
            Self::Vanilla(id) => id.clone(),
        }
    }
}

/// A pseudo-attribute representing one slot type (Java `SlotAttribute`).
#[derive(Clone, Debug, PartialEq)]
pub struct SlotAttribute {
    /// Slot type id this pseudo-attribute tracks.
    pub slot_id: String,
}

impl SlotAttribute {
    /// Java `SlotAttribute#getOrCreate` equivalent: a value keyed by slot id.
    pub fn get_or_create(id: &str) -> Self {
        Self {
            slot_id: id.to_owned(),
        }
    }

    /// Java `SlotAttribute` description id (`curios.identifier.<id>`).
    pub fn java_name(&self) -> String {
        format!("curios.identifier.{}", self.slot_id)
    }

    /// A Horizon-safe identifier for the pseudo-attribute.
    ///
    /// Java uses a dotted description name that is not a resource location;
    /// Horizon identifiers cannot contain the `curios.identifier.` prefix as
    /// a namespace, so the bridge uses `curios:slot_<id>` instead.
    pub fn horizon_identifier(&self) -> Result<Identifier, AttributeError> {
        Identifier::parse(&format!("curios:slot_{}", self.slot_id))
            .map_err(|_| AttributeError::InvalidIdentifier(self.slot_id.clone()))
    }
}

/// One attribute modifier entry from the Java data component.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModifierEntry {
    /// Attribute or slot pseudo-attribute.
    #[serde(rename = "type")]
    pub target: AttributeTarget,
    /// Modifier payload.
    pub modifier: ModifierSpec,
    /// Slot predicate; absent means `SlotTypePredicate::ANY`.
    #[serde(default)]
    pub slot: Option<SlotFilter>,
}

/// The `{id, amount, operation}` part of a Java `AttributeModifier`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModifierSpec {
    /// Modifier identifier, suffixed per slot when applied.
    pub id: String,
    /// Numeric amount.
    pub amount: f64,
    /// Application order, matching Java operations.
    pub operation: ModifierOperation,
}

/// Serde shape of the Java `SlotTypePredicate` used by `ModifierEntry`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlotFilter {
    /// Slot ids (`id` in Java is either a string or a list).
    #[serde(rename = "id", default, deserialize_with = "deserialize_id_list")]
    pub ids: Vec<String>,
    /// Allowed default slot sizes.
    pub size: Option<IntRange>,
    /// Required drop rule.
    pub drop_rule: Option<DropRule>,
    /// Java field `add_cosmetic`.
    pub add_cosmetic: Option<bool>,
    /// Java field `render_toggle`.
    pub render_toggle: Option<bool>,
}

fn deserialize_id_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(id) => vec![id],
        OneOrMany::Many(ids) => ids,
    })
}

impl SlotFilter {
    /// Converts the serde shape into the domain predicate from `slots.rs`.
    pub fn predicate(&self) -> SlotTypePredicate {
        SlotTypePredicate {
            ids: self.ids.clone(),
            size: self.size,
            drop_rule: self.drop_rule,
            has_cosmetic: self.add_cosmetic,
            can_toggle_rendering: self.render_toggle,
        }
    }
}

/// Java `CurioAttributeModifiers` data component payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CurioAttributeModifiers {
    /// Full form with the tooltip flag.
    Full {
        /// Modifier entries.
        modifiers: Vec<ModifierEntry>,
        /// Java `showInTooltip`.
        #[serde(default = "yes")]
        show_in_tooltip: bool,
    },
    /// Java shorthand: a bare list implies `showInTooltip = true`.
    List(Vec<ModifierEntry>),
}

const fn yes() -> bool {
    true
}

impl CurioAttributeModifiers {
    /// Empty component value.
    pub const EMPTY: Self = Self::List(Vec::new());

    /// Modifier entries regardless of serialization form.
    pub fn modifiers(&self) -> &[ModifierEntry] {
        match self {
            Self::Full { modifiers, .. } => modifiers,
            Self::List(modifiers) => modifiers,
        }
    }

    /// Java `showInTooltip`, defaulting to true for the list form.
    pub const fn show_in_tooltip(&self) -> bool {
        match self {
            Self::Full { show_in_tooltip, .. } => *show_in_tooltip,
            Self::List(_) => true,
        }
    }

    /// Java `withModifierAdded`: drop every entry with the same modifier id,
    /// then append the new one.
    pub fn with_modifier_added(mut self, entry: ModifierEntry) -> Self {
        self.modifiers_mut()
            .retain(|existing| existing.modifier.id != entry.modifier.id);
        self.modifiers_mut().push(entry);
        self
    }

    fn modifiers_mut(&mut self) -> &mut Vec<ModifierEntry> {
        match self {
            Self::Full { modifiers, .. } => modifiers,
            Self::List(modifiers) => modifiers,
        }
    }

    /// Resolves entries matching a slot type, suffixing ids with `/<slot id>`
    /// as Java `forEach(ISlotType, ...)` does.
    pub fn for_slot_type(&self, slot: &SlotType) -> Vec<ResolvedModifier> {
        self.modifiers()
            .iter()
            .filter(|entry| entry.slot.as_ref().is_none_or(|filter| filter.predicate().matches(slot)))
            .map(|entry| {
                ResolvedModifier::new(
                    entry.target.attribute_id(),
                    format!("{}/{}", entry.modifier.id, slot.id()),
                    entry.modifier.amount,
                    entry.modifier.operation,
                )
            })
            .collect()
    }

    /// Resolves entries matching an equipped slot, suffixing ids with
    /// `/<slot id>/<index>` as Java `forEach(SlotContext, ...)` does.
    ///
    /// Only `ids` can be checked here because the item-attribute hook receives
    /// no `SlotType`; callers with a slot type should use [`Self::for_slot_type`].
    pub fn for_slot_context(&self, slot_id: &str, index: usize) -> Vec<ResolvedModifier> {
        self.modifiers()
            .iter()
            .filter(|entry| {
                entry
                    .slot
                    .as_ref()
                    .is_none_or(|filter| filter.ids.is_empty() || filter.ids.iter().any(|id| id == slot_id))
            })
            .map(|entry| {
                ResolvedModifier::new(
                    entry.target.attribute_id(),
                    format!("{}/{slot_id}/{index}", entry.modifier.id),
                    entry.modifier.amount,
                    entry.modifier.operation,
                )
            })
            .collect()
    }

    /// Resolves vanilla-attribute entries for the Horizon bridge.
    ///
    /// Slot pseudo-attributes are excluded: Horizon has no slot attribute, so
    /// those entries are applied through [`SlotSizeState`] instead.
    pub fn for_horizon(&self) -> Vec<ResolvedModifier> {
        self.modifiers()
            .iter()
            .filter(|entry| matches!(entry.target, AttributeTarget::Vanilla(_)))
            .map(|entry| {
                ResolvedModifier::new(
                    entry.target.attribute_id(),
                    entry.modifier.id.clone(),
                    entry.modifier.amount,
                    entry.modifier.operation,
                )
            })
            .collect()
    }

    /// Encodes the component as NBT. The codec shape is stored under a single
    /// string key so no type-specific NBT encoder is needed.
    pub fn to_nbt(&self) -> Tag {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "[]".to_owned());
        Tag::compound([("json", Tag::String(json.as_str().into()))])
    }

    /// Decodes the component from NBT written by [`Self::to_nbt`].
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let Tag::String(json) = tag.get("json")? else {
            return None;
        };
        serde_json::from_str(&json.to_string_lossy()).ok()
    }
}

/// A modifier after slot filtering and id suffixing.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedModifier {
    /// Attribute id (or slot pseudo-attribute id).
    pub attribute: String,
    /// Suffixed modifier id.
    pub id: String,
    /// Numeric amount.
    pub amount: f64,
    /// Application order.
    pub operation: ModifierOperation,
}

impl ResolvedModifier {
    /// Creates a resolved modifier.
    pub const fn new(
        attribute: String,
        id: String,
        amount: f64,
        operation: ModifierOperation,
    ) -> Self {
        Self {
            attribute,
            id,
            amount,
            operation,
        }
    }

    /// Converts into Horizon's modifier representation.
    pub fn to_horizon(&self) -> Result<(Identifier, AttributeModifier), AttributeError> {
        let attribute = Identifier::parse(&self.attribute)
            .map_err(|_| AttributeError::InvalidIdentifier(self.attribute.clone()))?;
        let id = Identifier::parse(&self.id)
            .map_err(|_| AttributeError::InvalidIdentifier(self.id.clone()))?;
        Ok((
            attribute,
            AttributeModifier::new(id, self.amount, self.operation.into()),
        ))
    }
}

/// Errors from converting domain identifiers into Horizon identifiers.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeError {
    /// The id is not a valid Horizon resource location.
    InvalidIdentifier(String),
}

impl fmt::Display for AttributeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(id) => write!(f, "invalid Horizon identifier `{id}`"),
        }
    }
}

impl Error for AttributeError {}

/// NBT component descriptor read by [`item_attribute_modifier`].
pub struct CurioAttributesComponent;

impl Component for CurioAttributesComponent {
    type Value = CurioAttributeModifiers;

    fn id(&self) -> &str {
        ATTRIBUTE_COMPONENT_ID
    }

    fn decode(&self, value: &Tag) -> Option<Self::Value> {
        CurioAttributeModifiers::from_nbt(value)
    }

    fn encode(&self, value: Self::Value) -> Tag {
        value.to_nbt()
    }
}

/// Installs modifiers from the stack's Curio component.
///
/// This function is called from `Hooks::item_attribute_modifier`. Only stacks
/// carrying `curios:attribute_modifiers` are touched; everything else keeps
/// the engine's modifiers untouched. All modifiers use
/// `EquipmentSlotGroup::Any` because Horizon has no Curio slot groups.
pub fn item_attribute_modifier(event: &mut ItemAttributeModifierEvent<'_>) {
    let Some(modifiers) = event.stack.get(&CurioAttributesComponent) else {
        return;
    };
    for resolved in modifiers.for_horizon() {
        let Ok((attribute, modifier)) = resolved.to_horizon() else {
            eprintln!("[curios] invalid attribute modifier id `{}`", resolved.id);
            continue;
        };
        if !event
            .modifiers
            .add(attribute, modifier, EquipmentSlotGroup::Any)
        {
            eprintln!(
                "[curios] duplicate attribute modifier id `{}` was not added",
                resolved.id
            );
        }
    }
}

/// One transient or permanent slot-size modifier.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SlotSizeModifier {
    /// Application order (Java `AttributeModifier.Operation`).
    pub operation: ModifierOperation,
    /// Numeric amount.
    pub amount: f64,
}

/// The state Java `CurioStacksHandler#update` recomputes after a modifier change.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotSizeState {
    /// Default size before modifiers.
    pub base: u32,
    /// Applied modifiers, with their lifetime.
    pub modifiers: Vec<(SlotSizeModifier, bool)>,
}

/// Result of recomputing a slot's size.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ResizeResult {
    /// New slot count, clamped to zero.
    pub size: u32,
    /// Whether the size changed and the inventory must be resized.
    pub changed: bool,
}

impl SlotSizeState {
    /// Recomputes the size in Java order: `ADD_VALUE` grows the base,
    /// `ADD_MULTIPLIED_BASE` adds `base * amount` and `ADD_MULTIPLIED_TOTAL`
    /// multiplies the current size by `amount`.
    pub fn resize(&self) -> ResizeResult {
        let base = f64::from(self.base);
        let mut grown = base;
        for (modifier, _) in &self.modifiers {
            if modifier.operation == ModifierOperation::AddValue {
                grown += modifier.amount;
            }
        }
        let mut size = grown;
        for (modifier, _) in &self.modifiers {
            if modifier.operation == ModifierOperation::AddMultipliedBase {
                size += base * modifier.amount;
            }
        }
        for (modifier, _) in &self.modifiers {
            if modifier.operation == ModifierOperation::AddMultipliedTotal {
                size *= modifier.amount;
            }
        }
        let size = if size.is_finite() && size > 0.0 {
            size as u32
        } else {
            0
        };
        ResizeResult {
            size,
            changed: size != self.base,
        }
    }

    /// Removes transient modifiers, keeping permanent ones (Java clears
    /// transient slot modifiers after every inventory change).
    pub fn clear_transient(&mut self) {
        self.modifiers.retain(|(_, permanent)| *permanent);
    }
}

/// Adds the modifiers to a Horizon `ItemAttributeModifiers` collection using
/// `EquipmentSlotGroup::Any`; kept separate from the event for unit tests.
pub fn add_to_modifiers(modifiers: &mut ItemAttributeModifiers, resolved: &[ResolvedModifier]) {
    for entry in resolved {
        let Ok((attribute, modifier)) = entry.to_horizon() else {
            eprintln!("[curios] invalid attribute modifier id `{}`", entry.id);
            continue;
        };
        modifiers.add(attribute, modifier, EquipmentSlotGroup::Any);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, amount: f64, operation: ModifierOperation) -> ModifierSpec {
        ModifierSpec {
            id: id.to_owned(),
            amount,
            operation,
        }
    }

    fn slot_entry(slot: &str, spec: ModifierSpec) -> ModifierEntry {
        ModifierEntry {
            target: AttributeTarget::Slot {
                slot: slot.to_owned(),
            },
            modifier: spec,
            slot: None,
        }
    }

    #[test]
    fn parses_full_and_bare_list_forms() {
        let full: CurioAttributeModifiers = serde_json::from_str(
            r#"{"modifiers":[{"type":"curios:ring","modifier":{"id":"curios:boost","amount":2.0,"operation":"add_value"}}],"show_in_tooltip":false}"#,
        )
        .unwrap();
        assert!(!full.show_in_tooltip());
        assert_eq!(full.modifiers().len(), 1);
        assert_eq!(full.modifiers()[0].modifier.operation, ModifierOperation::AddValue);

        let list: CurioAttributeModifiers = serde_json::from_str(
            r#"[{"type":{"slot":"ring"},"modifier":{"id":"curios:slots","amount":1,"operation":"add_multiplied_base"},"slot":{"id":["ring","necklace"]}}]"#,
        )
        .unwrap();
        assert!(list.show_in_tooltip());
        assert_eq!(list.modifiers()[0].slot.as_ref().unwrap().ids, ["ring", "necklace"]);
    }

    #[test]
    fn with_modifier_added_replaces_same_id() {
        let component = CurioAttributeModifiers::List(vec![
            slot_entry("ring", spec("curios:boost", 1.0, ModifierOperation::AddValue)),
            slot_entry("head", spec("curios:other", 2.0, ModifierOperation::AddValue)),
        ]);
        let updated = component.with_modifier_added(slot_entry(
            "ring",
            spec("curios:boost", 5.0, ModifierOperation::AddValue),
        ));
        assert_eq!(updated.modifiers().len(), 2);
        assert_eq!(updated.modifiers()[1].modifier.amount, 5.0);
        assert!(updated.modifiers()[0].modifier.id == "curios:other");
    }

    #[test]
    fn suffix_ids_per_slot_and_context() {
        let mut entry = slot_entry(
            "ring",
            spec("curios:boost", 2.0, ModifierOperation::AddValue),
        );
        entry.slot = Some(SlotFilter {
            ids: vec!["ring".to_owned()],
            ..SlotFilter::default()
        });
        let component = CurioAttributeModifiers::List(vec![entry]);
        let slot = SlotType::new("ring").unwrap();
        assert_eq!(
            component.for_slot_type(&slot)[0].id,
            "curios:boost/ring"
        );
        assert_eq!(
            component.for_slot_context("ring", 3)[0].id,
            "curios:boost/ring/3"
        );
        assert!(component.for_slot_context("head", 0).is_empty());
    }

    #[test]
    fn slot_attribute_identifiers() {
        let attribute = SlotAttribute::get_or_create("ring");
        assert_eq!(attribute.java_name(), "curios.identifier.ring");
        assert_eq!(
            attribute.horizon_identifier().unwrap().to_string(),
            "curios:slot_ring"
        );
        assert!(SlotAttribute::get_or_create("Bad ID").horizon_identifier().is_err());
    }

    #[test]
    fn resize_follows_java_operation_order() {
        let state = SlotSizeState {
            base: 2,
            modifiers: vec![
                (
                    SlotSizeModifier {
                        operation: ModifierOperation::AddValue,
                        amount: 3.0,
                    },
                    true,
                ),
                (
                    SlotSizeModifier {
                        operation: ModifierOperation::AddMultipliedBase,
                        amount: 0.5,
                    },
                    false,
                ),
                (
                    SlotSizeModifier {
                        operation: ModifierOperation::AddMultipliedTotal,
                        amount: 2.0,
                    },
                    true,
                ),
            ],
        };
        // base = 2 + 3 = 5; + 2 * 0.5 = 6; * 2 = 12.
        assert_eq!(state.resize(), ResizeResult { size: 12, changed: true });
        let mut cleared = state;
        cleared.clear_transient();
        assert_eq!(cleared.modifiers.len(), 2);
        // base 2 + 3 (ADD_VALUE), then * 2 (ADD_MULTIPLIED_TOTAL) = 10.
        assert_eq!(cleared.resize().size, 10);
    }

    #[test]
    fn component_roundtrip_and_registration() {
        let component = CurioAttributeModifiers::List(vec![slot_entry(
            "ring",
            spec("curios:boost", 1.5, ModifierOperation::AddValue),
        )]);
        let tag = component.to_nbt();
        assert_eq!(CurioAttributeModifiers::from_nbt(&tag), Some(component));
        assert_eq!(CurioAttributesComponent.id(), ATTRIBUTE_COMPONENT_ID);
    }
}
