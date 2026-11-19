//! Built-in slot definitions and the replaceable process-wide slot registry.
//!
//! The eleven definitions are shipped with the mod (ported byte-for-byte from
//! the LGPL-3.0-or-later Curios reference, copyright 2018-2024 C4) and are
//! decoded strictly through [`SlotType::from_definition`]. A definition with no
//! explicit `size` uses the server setting `default_slot_count`. The registry
//! is an `Arc` snapshot so a data-pack reload can replace it without holding a
//! lock across gameplay hooks.

use std::sync::{Arc, RwLock};

use crate::config;
use crate::slots::{SlotDefinition, SlotError, SlotType, qualify_slot_id};

/// `(id, JSON)` pairs from `data/curios/curios/slots/` in Java declaration order.
pub const BUILTIN: [(&str, &str); 11] = [
    ("curio", include_str!("../data/curios/curios/slots/curio.json")),
    ("head", include_str!("../data/curios/curios/slots/head.json")),
    ("necklace", include_str!("../data/curios/curios/slots/necklace.json")),
    ("back", include_str!("../data/curios/curios/slots/back.json")),
    ("body", include_str!("../data/curios/curios/slots/body.json")),
    (
        "bracelet",
        include_str!("../data/curios/curios/slots/bracelet.json"),
    ),
    ("hands", include_str!("../data/curios/curios/slots/hands.json")),
    ("ring", include_str!("../data/curios/curios/slots/ring.json")),
    ("belt", include_str!("../data/curios/curios/slots/belt.json")),
    ("feet", include_str!("../data/curios/curios/slots/feet.json")),
    ("charm", include_str!("../data/curios/curios/slots/charm.json")),
];

static SLOT_TYPES: RwLock<Option<Arc<Vec<SlotType>>>> = RwLock::new(None);

fn read_registry() -> Option<Arc<Vec<SlotType>>> {
    SLOT_TYPES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Decodes the built-in definitions. Every parse error is returned, never
/// masked by a default: a malformed shipped definition is a packaging bug.
pub fn builtin_slot_types() -> Result<Vec<SlotType>, SlotError> {
    let configured = i32::from(config::default_slot_count());
    BUILTIN
        .iter()
        .map(|(id, json)| {
            let mut definition = SlotDefinition::from_json(json)?;
            if definition.size.is_none() {
                definition.size = Some(configured);
            }
            SlotType::from_definition(id, &definition)
        })
        .collect()
}

/// The installed registry snapshot, initialised from the bundled definitions
/// on first use and replaceable by a validated data-pack reload.
pub fn slot_types() -> Arc<Vec<SlotType>> {
    if let Some(types) = read_registry() {
        return types;
    }
    let types = Arc::new(builtin_slot_types().unwrap_or_else(|error| {
        panic!("invalid built-in Curios slot definition: {error}");
    }));
    let mut guard = SLOT_TYPES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get_or_insert(types).clone()
}

/// Replaces the cached registry after a validated reload.
pub fn replace_slot_types(types: Vec<SlotType>) -> Result<(), SlotError> {
    if types.is_empty() {
        return Err(SlotError::EmptyRegistry);
    }
    let mut seen = std::collections::BTreeSet::new();
    for slot in &types {
        if !seen.insert(slot.qualified_id()) {
            return Err(SlotError::DuplicateSlotId(slot.qualified_id()));
        }
    }
    let mut guard = SLOT_TYPES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(Arc::new(types));
    Ok(())
}

/// Index of a slot type in the deterministic registry order.
///
/// Both bare ids (`ring`, normalised to `curios:ring`) and canonical
/// `namespace:id` ids are accepted (review #20).
pub fn slot_type_index(id: &str) -> Option<u16> {
    let wanted = qualify_slot_id(id);
    slot_types()
        .iter()
        .position(|slot| slot.qualified_id() == wanted)
        .and_then(|index| u16::try_from(index).ok())
}

/// Looks up a slot type by canonical `namespace:id`.
pub fn slot_type_by_qualified_id(id: &str) -> Option<SlotType> {
    let wanted = qualify_slot_id(id);
    slot_types()
        .iter()
        .find(|slot| slot.qualified_id() == wanted)
        .cloned()
}

/// The slot type at an index from [`slot_type_index`].
pub fn slot_type_at(index: u16) -> Option<SlotType> {
    slot_types().get(usize::from(index)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_decode() {
        let types = builtin_slot_types().unwrap();
        assert_eq!(types.len(), 11);
        assert_eq!(types[0].id(), "curio");
        assert_eq!(types[10].id(), "charm");
        assert_eq!(types[1].order(), 40);
        assert_eq!(types[10].order(), 200);
    }

    #[test]
    fn registry_indices_are_stable() {
        let types = slot_types();
        for (index, slot) in types.iter().enumerate() {
            assert_eq!(slot_type_index(slot.id()), Some(index as u16));
            assert_eq!(slot_type_at(index as u16).as_ref(), Some(slot));
        }
        assert_eq!(slot_type_index("missing"), None);
        // Bare and canonical ids resolve to the same bundled slot (review #20).
        assert_eq!(slot_type_index("curios:ring"), slot_type_index("ring"));
        assert_eq!(
            slot_type_by_qualified_id("curios:ring").map(|slot| slot.id().to_owned()),
            Some("ring".to_owned())
        );
        assert!(slot_type_by_qualified_id("mymod:ring").is_none());
    }
}
