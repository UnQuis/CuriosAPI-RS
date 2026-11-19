//! The `ItemPlace::Mod` bridge: Horizon's transactional view of a Curio slot.
//!
//! Curios slots are not vanilla equipment slots, so Horizon cannot own their
//! read/write moments. Instead the engine routes item interactions through the
//! [`PlaceProvider`] registered as [`PROVIDER`]; each `key` encodes an entity id,
//! a slot type index and a slot index. The stack itself lives in the entity's
//! [`INVENTORY`] attachment.

use mod_api::{Holder, ItemPlace, ItemResource, PlaceProvider, ServerHost};

use crate::curio::{self, CurioContext};
use crate::equip::{self, TriState};
use crate::events::{self, ChangeKind, CurioChangeEvent, SlotModifiersUpdatedEvent};
use crate::inventory::{CurioInventory, INVENTORY, InventoryError, ItemSnapshot};
use crate::net::{self, NetTag};
use crate::registry;
use crate::slots::{SlotContext, SlotType};

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

/// Decodes `(entity, slot type, slot)` from a place key. The returned slot is
/// the raw low 16 bits, including [`COSMETIC_FLAG`]; use [`is_cosmetic`] to
/// test the flag and `slot & SLOT_MASK` for the actual slot index.
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

/// Resolves the slot type and `SlotContext` addressed by a place key.
fn context_for(key: u64) -> Option<(u32, SlotType, SlotContext)> {
    let (entity, type_index, raw_slot) = parse_key(key);
    let slot_type = registry::slot_type_at(type_index)?;
    let context = SlotContext::new(
        slot_type.id(),
        Some(u64::from(entity)),
        usize::from(raw_slot & SLOT_MASK),
        is_cosmetic(key),
        true,
    );
    Some((entity, slot_type, context))
}

/// Player uuid for a targeted stack sync; non-player entities get no packet.
fn player_uuid(host: &mut dyn ServerHost, entity: u32) -> Option<u128> {
    host.players()
        .server_players()
        .into_iter()
        .find(|player| player.entity().id() == entity)
        .map(|player| player.uuid())
}

/// Dispatches change/modifier events, item behavior hooks and the targeted
/// `sync_stack` packet after a successful server-side write.
fn notify_change(
    host: &mut dyn ServerHost,
    entity: u32,
    slot_type: &SlotType,
    index: usize,
    cosmetic: bool,
    previous: Option<ItemSnapshot>,
    current: Option<ItemSnapshot>,
    kind: ChangeKind,
    slot_count_changed: bool,
) {
    let previous_id = previous.as_ref().and_then(ItemSnapshot::item_id);
    let current_id = current.as_ref().and_then(ItemSnapshot::item_id);
    let mut change = CurioChangeEvent {
        entity,
        slot: slot_type.id().to_owned(),
        index,
        previous,
        current,
        kind,
    };
    events::dispatch_change(&mut change);

    let behavior_id = current_id.clone().or_else(|| previous_id.clone());
    if let Some(item) = behavior_id {
        if let Some(behavior) = curio::behavior(&item) {
            let context = CurioContext {
                slot: slot_type.id(),
                index: index as u32,
                item: &item,
            };
            if kind == ChangeKind::Item {
                if current_id.is_some() && previous_id.as_deref() != Some(item.as_str()) {
                    behavior.on_equip(&context);
                } else if current_id.is_none() {
                    behavior.on_unequip(&context);
                }
            }
        }
    }

    if slot_count_changed {
        events::dispatch_slot_modifiers_updated(&mut SlotModifiersUpdatedEvent {
            entity,
            slots: vec![slot_type.id().to_owned()],
        });
    }

    let Some(uuid) = player_uuid(host, entity) else {
        return;
    };
    let handler_type = if cosmetic {
        net::HANDLER_COSMETIC
    } else {
        net::HANDLER_EQUIPMENT
    };
    if let Err(error) = net::sync_stack(
        host,
        uuid,
        slot_type.id(),
        index as i32,
        change.current.as_ref(),
        handler_type,
        &NetTag::Compound(Vec::new()),
    ) {
        eprintln!("[curios] stack sync failed for {}: {error:?}", slot_type.id());
    }
    // Agent #2's custom GUI only refreshes on its own actions; an equip from
    // outside the menu needs the same state push (players only).
    if !crate::menu::sync_state(host, uuid) {
        eprintln!(
            "[curios] menu state sync failed for slot {}",
            slot_type.id()
        );
    }
}

fn snapshot_of(
    host: &dyn ServerHost,
    stack: Option<&mod_api::ItemStack>,
) -> Result<Option<ItemSnapshot>, InventoryError> {
    stack.map(|stack| ItemSnapshot::from_stack(host, stack)).transpose()
}

/// Whether the measured slot count really changed (review #23 tail).
fn count_changed(before: Option<usize>, after: Option<usize>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => before != after,
        // A failed read is not evidence of a resize: do not emit the event.
        _ => false,
    }
}

/// Current slot count for a place key, used to detect a real dynamic resize.
fn slot_count(host: &mut dyn ServerHost, key: u64) -> Option<usize> {
    let (entity, type_index, _) = parse_key(key);
    let slot_type = registry::slot_type_at(type_index)?;
    let inventory = INVENTORY.get(host, Holder::Entity(entity)).ok()?;
    inventory.slot(slot_type.id()).map(|handler| handler.len())
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
        let Some((entity, slot_type, context)) = context_for(key) else {
            eprintln!("[curios] insert refused: place key {key} has no slot type");
            return 0;
        };
        let before_count = slot_count(host, key);
        let item = resource.item();
        let item_id = item.id(host);
        if !equip::can_equip_host(host, &slot_type, &context, item, &item_id, TriState::Default) {
            return 0;
        }
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
        let previous = match snapshot_of(host, existing.as_ref()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                report(&error);
                return 0;
            }
        };
        let Some(limit) = host.max_stack_size(resource.item()) else {
            eprintln!(
                "[curios] insert refused: no stack size for item {}",
                item_id
            );
            return 0;
        };
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
        let current = match snapshot_of(host, Some(&next)) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                report(&error);
                return 0;
            }
        };
        let previous_id = previous.as_ref().and_then(ItemSnapshot::item_id);
        let current_id = current.as_ref().and_then(ItemSnapshot::item_id);
        let kind = if previous_id == current_id {
            ChangeKind::State
        } else {
            ChangeKind::Item
        };
        match write_stack(host, key, Some(&next)) {
            Ok(()) => {
                let after_count = slot_count(host, key);
                let slot_count_changed = count_changed(before_count, after_count);
                notify_change(
                    host,
                    entity,
                    &slot_type,
                    usize::from(context.index),
                    is_cosmetic(key),
                    previous,
                    current,
                    kind,
                    slot_count_changed,
                );
                taken
            }
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
        let Some((entity, slot_type, context)) = context_for(key) else {
            eprintln!("[curios] extract refused: place key {key} has no slot type");
            return 0;
        };
        let before_count = slot_count(host, key);
        let Some(existing) = read_stack(host, key).unwrap_or_else(|error| {
            report(&error);
            None
        }) else {
            return 0;
        };
        let previous_id = existing.item().id(host);
        if !equip::can_unequip(&context, &previous_id, TriState::Default) {
            return 0;
        }
        if !resource.matches(&existing) {
            return 0;
        }
        let previous = match snapshot_of(host, Some(&existing)) {
            Ok(snapshot) => snapshot.unwrap_or_else(|| {
                unreachable!("a stack was read from this slot")
            }),
            Err(error) => {
                report(&error);
                return 0;
            }
        };
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
        let current = match snapshot_of(host, next.as_ref()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                report(&error);
                return 0;
            }
        };
        let current_id = current.as_ref().and_then(ItemSnapshot::item_id);
        let kind = if Some(previous_id.as_str()) == current_id.as_deref() {
            ChangeKind::State
        } else {
            ChangeKind::Item
        };
        match write_stack(host, key, next.as_ref()) {
            Ok(()) => {
                let after_count = slot_count(host, key);
                let slot_count_changed = count_changed(before_count, after_count);
                notify_change(
                    host,
                    entity,
                    &slot_type,
                    context.index,
                    is_cosmetic(key),
                    Some(previous),
                    current,
                    kind,
                    slot_count_changed,
                );
                took
            }
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
    fn place_key_boundaries_are_lossless() {
        for entity in [0_u32, 1, u32::MAX] {
            for type_index in [0_u16, 1, u16::MAX] {
                for slot in [0_u16, 1, SLOT_MASK] {
                    let plain = place_key(entity, type_index, slot);
                    assert_eq!(parse_key(plain), (entity, type_index, slot));
                    assert!(!is_cosmetic(plain));
                    let cosmetic = place_key(entity, type_index, slot | COSMETIC_FLAG);
                    assert_eq!(
                        parse_key(cosmetic),
                        (entity, type_index, slot | COSMETIC_FLAG)
                    );
                    assert!(is_cosmetic(cosmetic));
                    assert_eq!(parse_key(cosmetic).2 & SLOT_MASK, slot);
                }
            }
        }
        assert!(curio_place(1, "ring", COSMETIC_FLAG).is_none());
        assert!(cosmetic_place(1, "ring", SLOT_MASK + 1).is_none());
    }

    #[test]
    fn modifiers_event_requires_a_real_size_change() {
        assert!(!count_changed(None, Some(2)));
        assert!(!count_changed(Some(2), Some(2)));
        assert!(!count_changed(Some(2), None));
        assert!(count_changed(Some(2), Some(3)));
        assert!(count_changed(Some(2), Some(1)));
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
