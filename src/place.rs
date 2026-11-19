//! The `ItemPlace::Mod` bridge: Horizon's transactional view of a Curio slot.
//!
//! Curios slots are not vanilla equipment slots, so Horizon cannot own their
//! read/write moments. Instead the engine routes item interactions through the
//! [`PlaceProvider`] registered as [`PROVIDER`]; each `key` encodes an entity id,
//! a slot type index and a slot index. The stack itself lives in the entity's
//! [`INVENTORY`] attachment.

use mod_api::{Holder, ItemPlace, ItemResource, PlaceProvider, ServerHost};

use crate::inventory::{CurioInventory, INVENTORY, InventoryError, ItemSnapshot};
use crate::registry;

/// Registered name of the mod place provider (`ItemPlace::Mod.provider`).
pub const PROVIDER: &str = "curios:slots";

/// Bit set in the low half of a place key to address a cosmetic slot.
pub const COSMETIC_FLAG: u16 = 0x8000;
/// Mask for the slot index, excluding the cosmetic flag.
pub const SLOT_MASK: u16 = 0x7fff;

/// Builds a place key for one entity's slot.
pub fn place_key(entity: u32, slot_type: u16, slot: u16) -> u64 {
    (u64::from(entity) << 32) | (u64::from(slot_type) << 16) | u64::from(slot)
}

/// Decodes `(entity, slot type, slot)` from a place key. The returned slot
/// index has the [`COSMETIC_FLAG`] bit cleared; use [`is_cosmetic`] to test it.
pub fn parse_key(key: u64) -> (u32, u16, u16) {
    (
        (key >> 32) as u32,
        ((key >> 16) & 0xffff) as u16,
        (key & 0xffff) as u16,
    )
}

/// Whether a place key addresses a cosmetic slot.
pub fn is_cosmetic(key: u64) -> bool {
    key & u64::from(COSMETIC_FLAG) != 0
}

/// The mod place that addresses one functional Curio slot.
pub fn curio_place(entity: u32, slot_type: &str, slot: u16) -> Option<ItemPlace> {
    let type_index = registry::slot_type_index(slot_type)?;
    if slot & SLOT_MASK != slot {
        return None;
    }
    Some(ItemPlace::Mod {
        provider: PROVIDER,
        key: place_key(entity, type_index, slot),
    })
}

/// The mod place that addresses one cosmetic Curio slot.
pub fn cosmetic_place(entity: u32, slot_type: &str, slot: u16) -> Option<ItemPlace> {
    let type_index = registry::slot_type_index(slot_type)?;
    if slot & SLOT_MASK != slot {
        return None;
    }
    Some(ItemPlace::Mod {
        provider: PROVIDER,
        key: place_key(entity, type_index, slot | COSMETIC_FLAG),
    })
}

fn report(error: &InventoryError) {
    eprintln!("[curios] place operation refused: {error:?}");
}

/// Reads one stored stack through the host's canonical item ids.
fn read_stack(host: &mut dyn ServerHost, key: u64) -> Result<Option<mod_api::ItemStack>, InventoryError> {
    let (entity, type_index, raw_slot) = parse_key(key);
    let slot = raw_slot & SLOT_MASK;
    let slot_type = registry::slot_type_at(type_index).ok_or(InventoryError::ItemDecode)?;
    let inventory = INVENTORY.get(host, Holder::Entity(entity))?;
    let entry = inventory.slot(slot_type.id()).and_then(|handler| {
        if is_cosmetic(key) {
            handler.cosmetic(usize::from(slot))
        } else {
            handler.stack(usize::from(slot))
        }
    });
    match entry.and_then(|entry| entry.as_ref()) {
        Some(snapshot) => snapshot.to_stack(host).map(Some),
        None => Ok(None),
    }
}

/// Writes one stored stack through the host's canonical item ids.
fn write_stack(
    host: &mut dyn ServerHost,
    key: u64,
    stack: Option<&mod_api::ItemStack>,
) -> Result<(), InventoryError> {
    let (entity, type_index, raw_slot) = parse_key(key);
    let slot = raw_slot & SLOT_MASK;
    let slot_type = registry::slot_type_at(type_index).ok_or(InventoryError::ItemEncode)?;
    let mut inventory: CurioInventory = INVENTORY.get(host, Holder::Entity(entity))?;
    let snapshot = match stack {
        Some(stack) => Some(ItemSnapshot::from_stack(host, stack)?),
        None => None,
    };
    let handler = inventory
        .slot_mut(slot_type.id())
        .ok_or(InventoryError::ItemEncode)?;
    if is_cosmetic(key) {
        handler.set_cosmetic(usize::from(slot), snapshot)?;
    } else {
        handler.set_stack(usize::from(slot), snapshot)?;
    }
    INVENTORY.set(host, Holder::Entity(entity), inventory)?;
    Ok(())
}

/// Bridges engine item transactions to the attachment-backed slots.
pub struct CurioPlaceProvider;

impl PlaceProvider for CurioPlaceProvider {
    fn stack(&self, host: &mut dyn ServerHost, key: u64) -> Option<mod_api::ItemStack> {
        match read_stack(host, key) {
            Ok(stack) => stack,
            Err(error) => {
                report(&error);
                None
            }
        }
    }

    fn insert(
        &self,
        host: &mut dyn ServerHost,
        _tx: &mod_api::Transaction,
        key: u64,
        resource: &ItemResource,
        amount: u32,
    ) -> u32 {
        let existing = match read_stack(host, key) {
            Ok(stack) => stack,
            Err(error) => {
                report(&error);
                return 0;
            }
        };
        if existing.as_ref().is_some_and(|stack| !resource.matches(stack)) {
            return 0;
        }
        let limit = host.max_stack_size(resource.item()).unwrap_or(64);
        let current = existing.as_ref().map_or(0, mod_api::ItemStack::count);
        let room = limit.saturating_sub(current);
        let taken = amount.min(room);
        if taken == 0 {
            return 0;
        }
        let Some(next) = resource.to_stack(current.saturating_add(taken)) else {
            eprintln!("[curios] insert refused: stack count {current}+{taken} is invalid");
            return 0;
        };
        match write_stack(host, key, Some(&next)) {
            Ok(()) => taken,
            Err(error) => {
                report(&error);
                0
            }
        }
    }

    fn extract(
        &self,
        host: &mut dyn ServerHost,
        _tx: &mod_api::Transaction,
        key: u64,
        resource: &ItemResource,
        amount: u32,
    ) -> u32 {
        let Some(existing) = read_stack(host, key).unwrap_or_else(|error| {
            report(&error);
            None
        }) else {
            return 0;
        };
        if !resource.matches(&existing) {
            return 0;
        }
        let took = amount.min(existing.count());
        if took == 0 {
            return 0;
        }
        let remaining = existing.count() - took;
        let next = if remaining == 0 {
            None
        } else {
            let mut stack = existing;
            if stack.set_count(remaining).is_err() {
                return 0;
            }
            Some(stack)
        };
        match write_stack(host, key, next.as_ref()) {
            Ok(()) => took,
            Err(error) => {
                report(&error);
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trip() {
        let key = place_key(42, 7, 3);
        assert_eq!(parse_key(key), (42, 7, 3));
        assert_eq!(place_key(0xffff_ffff, 0xffff, 0xffff), u64::MAX);
    }

    #[test]
    fn places_address_builtin_slots() {
        let place = curio_place(17, "ring", 1).unwrap();
        match place {
            ItemPlace::Mod { provider, key } => {
                assert_eq!(provider, PROVIDER);
                assert_eq!(parse_key(key), (17, registry::slot_type_index("ring").unwrap(), 1));
                assert!(!is_cosmetic(key));
            }
            _ => panic!("unexpected place type"),
        }
        match cosmetic_place(17, "ring", 1).unwrap() {
            ItemPlace::Mod { key, .. } => {
                assert!(is_cosmetic(key));
                assert_eq!(parse_key(key).2 & SLOT_MASK, 1);
            }
            _ => panic!("unexpected place type"),
        }
        assert!(curio_place(17, "missing", 0).is_none());
        assert!(curio_place(17, "ring", COSMETIC_FLAG).is_none());
    }
}
