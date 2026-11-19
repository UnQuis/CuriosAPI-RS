//! Per-entity Curio inventory and its persistent attachment.
//!
//! Adapted from `top.theillusivec4.curios.common.capability.CurioInventory`
//! and `common.inventory.CurioStacksHandler` of TheIllusiveC4/Curios
//! (LGPL-3.0-or-later, copyright 2018-2024 C4); see COPYING and COPYING.LESSER.
//!
//! Items are kept as their NBT escape (`ItemStack::to_nbt`) rather than as
//! `ItemStack` so the attachment codec stays host-independent: resolving a
//! process-local `ItemKey` requires the game, and a codec has no host. Gameplay
//! callers convert with [`ItemSnapshot::to_stack`] at the write boundary.

use std::collections::BTreeMap;
use std::str::FromStr;

use mod_api::{AttachmentCodec, AttachmentError, AttachmentType, Compound, Host, ItemStack, Tag};

use crate::drop::{DropRule, DropRules, ItemPredicate, decide_drop};
use crate::registry;
use crate::slots::{SlotType, validate_slot_id};

/// An item stored in a curio slot, encoded in the game's item NBT escape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSnapshot(Compound);

impl ItemSnapshot {
    /// Captures a stack through the host's canonical item id.
    pub fn from_stack(host: &dyn Host, stack: &ItemStack) -> Result<Self, InventoryError> {
        stack
            .to_nbt(host)
            .map(Self)
            .ok_or(InventoryError::ItemEncode)
    }

    /// Restores a stack. Unknown ids and malformed patches are refused.
    pub fn to_stack(&self, host: &dyn Host) -> Result<ItemStack, InventoryError> {
        ItemStack::from_nbt(host, &self.0).ok_or(InventoryError::ItemDecode)
    }

    /// Canonical item id, used by tags, drop rules and behavior lookups.
    pub fn item_id(&self) -> Option<String> {
        match self.0.get(&"id".into())? {
            Tag::String(id) => Some(id.to_string_lossy()),
            _ => None,
        }
    }

    /// The raw NBT escape, for network sync and encoding.
    pub fn as_nbt(&self) -> &Compound {
        &self.0
    }
}

/// One slot type's stacks, mirrored cosmetic slots and per-slot flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotStacks {
    stacks: Vec<Option<ItemSnapshot>>,
    cosmetic: Vec<Option<ItemSnapshot>>,
    render: Vec<bool>,
    active: Vec<bool>,
    drop_rule: DropRule,
    use_native_gui: bool,
}

impl SlotStacks {
    /// Builds the default handler for a slot definition.
    pub fn new(slot: &SlotType) -> Self {
        let len = usize::try_from(slot.size()).unwrap_or(0);
        Self {
            stacks: vec![None; len],
            cosmetic: if slot.has_cosmetic() {
                vec![None; len]
            } else {
                Vec::new()
            },
            render: vec![true; len],
            active: vec![true; len],
            drop_rule: slot.drop_rule(),
            use_native_gui: slot.use_native_gui(),
        }
    }

    /// Number of functional slots.
    pub fn len(&self) -> usize {
        self.stacks.len()
    }

    /// Whether this slot type has no slots at all.
    pub fn is_empty(&self) -> bool {
        self.stacks.is_empty()
    }

    /// Functional stack at `index`.
    pub fn stack(&self, index: usize) -> Option<&Option<ItemSnapshot>> {
        self.stacks.get(index)
    }

    /// Cosmetic stack at `index`, if cosmetic slots exist.
    pub fn cosmetic(&self, index: usize) -> Option<&Option<ItemSnapshot>> {
        self.cosmetic.get(index)
    }

    /// Whether index `index` is active (equippable and ticked).
    pub fn active(&self, index: usize) -> Option<bool> {
        self.active.get(index).copied()
    }

    /// Whether index `index` renders on the entity.
    pub fn render(&self, index: usize) -> Option<bool> {
        self.render.get(index).copied()
    }

    /// Slot-type drop rule used when item and event rules defer.
    pub const fn drop_rule(&self) -> DropRule {
        self.drop_rule
    }

    /// Whether this slot type belongs in the native GUI.
    pub const fn use_native_gui(&self) -> bool {
        self.use_native_gui
    }

    /// All non-empty functional stacks in index order.
    pub fn filled(&self) -> impl Iterator<Item = (usize, &ItemSnapshot)> {
        self.stacks
            .iter()
            .enumerate()
            .filter_map(|(index, stack)| stack.as_ref().map(|item| (index, item)))
    }

    fn check_index(&self, cosmetic: bool, index: usize) -> Result<(), InventoryError> {
        let len = if cosmetic {
            self.cosmetic.len()
        } else {
            self.stacks.len()
        };
        if index >= len {
            return Err(InventoryError::IndexOutOfBounds {
                index,
                len,
            });
        }
        Ok(())
    }

    /// Places `item` in a functional slot, returning the replaced item.
    pub fn set_stack(
        &mut self,
        index: usize,
        item: Option<ItemSnapshot>,
    ) -> Result<Option<ItemSnapshot>, InventoryError> {
        self.check_index(false, index)?;
        Ok(std::mem::replace(&mut self.stacks[index], item))
    }

    /// Places `item` in a cosmetic slot, returning the replaced item.
    pub fn set_cosmetic(
        &mut self,
        index: usize,
        item: Option<ItemSnapshot>,
    ) -> Result<Option<ItemSnapshot>, InventoryError> {
        self.check_index(true, index)?;
        Ok(std::mem::replace(&mut self.cosmetic[index], item))
    }

    /// Sets the render flag for one slot.
    pub fn set_render(&mut self, index: usize, value: bool) -> Result<(), InventoryError> {
        self.check_index(false, index)?;
        self.render[index] = value;
        Ok(())
    }

    /// Sets the active flag for one slot.
    pub fn set_active(&mut self, index: usize, value: bool) -> Result<(), InventoryError> {
        self.check_index(false, index)?;
        self.active[index] = value;
        Ok(())
    }

    /// Copies mutable flags from an older handler on a data reload.
    pub fn carry_flags_from(&mut self, old: &Self) {
        for index in 0..self.len().min(old.len()) {
            self.render[index] = old.render[index];
            self.active[index] = old.active[index];
        }
    }

    /// Applies one drop decision, clearing the slot unless the item is kept.
    pub fn apply_drop(
        &mut self,
        index: usize,
        decision: crate::drop::DropDecision,
    ) -> Result<Option<ItemSnapshot>, InventoryError> {
        self.check_index(false, index)?;
        if decision.outcome == crate::drop::DropOutcome::Keep {
            return Ok(None);
        }
        Ok(self.stacks[index].take())
    }

    /// Resolves a drop rule for one stored item with the given overrides.
    pub fn decide_drop<P>(
        &self,
        index: usize,
        rules: &DropRules<P>,
        item_rule: DropRule,
        keep_inventory: bool,
        drop_prevented: bool,
    ) -> Option<crate::drop::DropDecision>
    where
        P: ItemPredicate<ItemSnapshot>,
    {
        self.stacks.get(index)?.as_ref().map(|item| {
            decide_drop(
                rules,
                item,
                item_rule,
                self.drop_rule,
                keep_inventory,
                drop_prevented,
            )
        })
    }
}

/// All curio slots of one living entity, persisted as an attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CurioInventory {
    slots: BTreeMap<String, SlotStacks>,
    invalid: Vec<ItemSnapshot>,
}

impl CurioInventory {
    /// Creates handlers for the given slot definitions.
    pub fn from_slot_types(types: &[SlotType]) -> Self {
        let slots = types
            .iter()
            .map(|slot| (slot.id().to_owned(), SlotStacks::new(slot)))
            .collect();
        Self {
            slots,
            invalid: Vec::new(),
        }
    }

    /// Rebuilds handlers after a data reload, carrying flags and valid stacks
    /// and returning everything that no longer fits as invalid items.
    pub fn reload(&mut self, types: &[SlotType]) -> Vec<ItemSnapshot> {
        let fresh = Self::from_slot_types(types);
        let mut invalid = Vec::new();
        for (id, handler) in fresh.slots {
            match self.slots.remove(&id) {
                Some(old) => {
                    let mut kept = handler;
                    kept.carry_flags_from(&old);
                    for index in 0..kept.len().min(old.len()) {
                        if let Some(item) = old.stacks[index].clone() {
                            match kept.set_stack(index, Some(item)) {
                                Ok(_) => {}
                                Err(_) => unreachable!("index was checked against kept.len()"),
                            }
                        }
                    }
                    self.slots.insert(id, kept);
                }
                None => {
                    self.slots.insert(id, handler);
                }
            }
        }
        // Slot types that were removed keep no handler; their stacks are invalid.
        for old in self.slots.values() {
            invalid.extend(old.stacks.iter().flatten().cloned());
            invalid.extend(old.cosmetic.iter().flatten().cloned());
        }
        self.slots.retain(|id, _| types.iter().any(|slot| slot.id() == id));
        invalid.extend(std::mem::take(&mut self.invalid));
        invalid
    }

    /// Handler for one slot type.
    pub fn slot(&self, id: &str) -> Option<&SlotStacks> {
        self.slots.get(id)
    }

    /// Mutable handler for one slot type.
    pub fn slot_mut(&mut self, id: &str) -> Option<&mut SlotStacks> {
        self.slots.get_mut(id)
    }

    /// Slot ids in deterministic order.
    pub fn slot_ids(&self) -> impl Iterator<Item = &str> {
        self.slots.keys().map(String::as_str)
    }

    /// First item with a matching id, across all slot types in id order.
    pub fn find_first(&self, item_id: &str) -> Option<(&str, usize, &ItemSnapshot)> {
        self.slots.iter().find_map(|(id, handler)| {
            handler
                .filled()
                .find(|(_, item)| item.item_id().as_deref() == Some(item_id))
                .map(|(index, item)| (id.as_str(), index, item))
        })
    }

    /// Invalid stacks awaiting drop on the next tick.
    pub fn invalid_stacks(&self) -> &[ItemSnapshot] {
        &self.invalid
    }

    /// Takes the invalid stacks, leaving the inventory empty.
    pub fn take_invalid(&mut self) -> Vec<ItemSnapshot> {
        std::mem::take(&mut self.invalid)
    }
}

/// Errors from inventory operations. Nothing is silently defaulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InventoryError {
    /// The item could not be converted to its NBT escape.
    ItemEncode,
    /// A stored NBT escape could not be restored through the host.
    ItemDecode,
    /// A slot index was outside the handler's range.
    IndexOutOfBounds { index: usize, len: usize },
    /// The attachment store refused a read or write.
    Attachment(AttachmentError),
    /// The internal lock was poisoned.
    Poisoned,
}

impl From<AttachmentError> for InventoryError {
    fn from(error: AttachmentError) -> Self {
        Self::Attachment(error)
    }
}

fn strings(list: &[Option<ItemSnapshot>]) -> Vec<Tag> {
    list.iter()
        .map(|item| match item {
            Some(item) => Tag::Compound(item.0.clone()),
            None => Tag::End,
        })
        .collect()
}

fn read_items(value: Option<&Tag>) -> Result<Vec<Option<ItemSnapshot>>, AttachmentError> {
    let Tag::List(list) = value.ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    list.iter()
        .map(|tag| match tag {
            Tag::End => Ok(None),
            Tag::Compound(compound) => Ok(Some(ItemSnapshot(compound.clone()))),
            _ => Err(AttachmentError::Decode),
        })
        .collect()
}

fn read_flags(value: Option<&Tag>, len: usize) -> Result<Vec<bool>, AttachmentError> {
    let Tag::List(list) = value.ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    if list.len() != len {
        return Err(AttachmentError::Decode);
    }
    list.iter()
        .map(|tag| match tag {
            Tag::Byte(0) => Ok(false),
            Tag::Byte(1) => Ok(true),
            _ => Err(AttachmentError::Decode),
        })
        .collect()
}

fn encode_handler(handler: &SlotStacks) -> Compound {
    let mut out = Compound::new();
    out.insert("stacks".into(), Tag::List(strings(&handler.stacks)));
    out.insert("cosmetic".into(), Tag::List(strings(&handler.cosmetic)));
    out.insert(
        "render".into(),
        Tag::List(handler.render.iter().map(|v| Tag::Byte(*v as i8)).collect()),
    );
    out.insert(
        "active".into(),
        Tag::List(handler.active.iter().map(|v| Tag::Byte(*v as i8)).collect()),
    );
    out.insert(
        "drop_rule".into(),
        Tag::String(handler.drop_rule.as_str().into()),
    );
    out.insert(
        "use_native_gui".into(),
        Tag::Byte(i8::from(handler.use_native_gui)),
    );
    out
}

fn decode_handler(value: &Tag) -> Result<SlotStacks, AttachmentError> {
    let Tag::Compound(map) = value else {
        return Err(AttachmentError::Decode);
    };
    let stacks = read_items(map.get(&"stacks".into()))?;
    let cosmetic = read_items(map.get(&"cosmetic".into()))?;
    let render = read_flags(map.get(&"render".into()), stacks.len())?;
    let active = read_flags(map.get(&"active".into()), stacks.len())?;
    let Tag::String(rule) = map.get(&"drop_rule".into()).ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    let drop_rule = DropRule::from_str(&rule.to_string_lossy())
        .map_err(|_| AttachmentError::Decode)?;
    let Tag::Byte(native) = map.get(&"use_native_gui".into()).ok_or(AttachmentError::Decode)?
    else {
        return Err(AttachmentError::Decode);
    };
    if *native != 0 && *native != 1 {
        return Err(AttachmentError::Decode);
    }
    Ok(SlotStacks {
        stacks,
        cosmetic,
        render,
        active,
        drop_rule,
        use_native_gui: *native == 1,
    })
}

fn encode_inventory(inventory: &CurioInventory) -> Result<Compound, AttachmentError> {
    let mut slots = Compound::new();
    for (id, handler) in &inventory.slots {
        slots.insert(id.as_str().into(), Tag::Compound(encode_handler(handler)));
    }
    let mut out = Compound::new();
    out.insert("slots".into(), Tag::Compound(slots));
    out.insert(
        "invalid".into(),
        Tag::List(
            inventory
                .invalid
                .iter()
                .map(|item| Tag::Compound(item.0.clone()))
                .collect(),
        ),
    );
    Ok(out)
}

fn decode_inventory(value: &Compound) -> Result<CurioInventory, AttachmentError> {
    let Tag::Compound(slots) = value.get(&"slots".into()).ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    let mut decoded = BTreeMap::new();
    for (id, handler) in slots {
        let id = id.to_string_lossy();
        if validate_slot_id(&id).is_err() {
            return Err(AttachmentError::Decode);
        }
        decoded.insert(id, decode_handler(handler)?);
    }
    let Tag::List(invalid) = value.get(&"invalid".into()).ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    let invalid = invalid
        .iter()
        .map(|tag| match tag {
            Tag::Compound(compound) => Ok(ItemSnapshot(compound.clone())),
            _ => Err(AttachmentError::Decode),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CurioInventory {
        slots: decoded,
        invalid,
    })
}

/// The persistent per-entity curio inventory. `copy_on_death` in Java keeps
/// the attachment across death; drops are resolved separately by `equip`.
pub static INVENTORY: AttachmentType<CurioInventory> = AttachmentType {
    namespace: "curios",
    name: "inventory",
    default: || CurioInventory::from_slot_types(registry::slot_types().as_slice()),
    codec: Some(AttachmentCodec {
        encode: encode_inventory,
        decode: decode_inventory,
    }),
    copy_on_death: true,
    // The client needs the stacks for GUI and entity rendering; the codec is
    // strict, so the engine can mirror the whole inventory to watching clients.
    sync: true,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str) -> ItemSnapshot {
        let mut nbt = Compound::new();
        nbt.insert("id".into(), Tag::String(id.into()));
        nbt.insert("count".into(), Tag::Int(1));
        ItemSnapshot(nbt)
    }

    fn slot() -> SlotType {
        let id = "ring";
        SlotType::new(id).unwrap().with_size(2)
    }

    #[test]
    fn codec_round_trip() {
        let mut inventory = CurioInventory::from_slot_types(&[slot()]);
        let handler = inventory.slot_mut("ring").unwrap();
        handler.set_stack(0, Some(snapshot("minecraft:stone"))).unwrap();
        handler.set_render(1, false).unwrap();
        handler.set_active(1, false).unwrap();
        inventory.invalid.push(snapshot("minecraft:dirt"));

        let encoded = encode_inventory(&inventory).unwrap();
        let decoded = decode_inventory(&encoded).unwrap();
        assert_eq!(inventory, decoded);
    }

    #[test]
    fn malformed_codec_is_refused() {
        let mut nbt = Compound::new();
        nbt.insert("slots".into(), Tag::List(Vec::new()));
        nbt.insert("invalid".into(), Tag::List(Vec::new()));
        assert_eq!(decode_inventory(&nbt), Err(AttachmentError::Decode));
    }

    #[test]
    fn index_bounds_are_explicit() {
        let mut handler = SlotStacks::new(&slot());
        assert!(matches!(
            handler.set_stack(2, None),
            Err(InventoryError::IndexOutOfBounds { index: 2, len: 2 })
        ));
    }

    #[test]
    fn find_first_and_invalid_drain() {
        let mut inventory = CurioInventory::from_slot_types(&[slot()]);
        inventory
            .slot_mut("ring")
            .unwrap()
            .set_stack(1, Some(snapshot("minecraft:stone")))
            .unwrap();
        assert_eq!(
            inventory.find_first("minecraft:stone").map(|(id, i, _)| (id, i)),
            Some(("ring", 1))
        );
        assert!(inventory.find_first("minecraft:dirt").is_none());
        inventory.invalid.push(snapshot("minecraft:dirt"));
        assert_eq!(inventory.take_invalid().len(), 1);
        assert!(inventory.invalid_stacks().is_empty());
    }
}
