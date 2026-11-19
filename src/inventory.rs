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

use mod_api::{
    AttachmentCodec, AttachmentError, AttachmentType, Compound, Holder, Host, ItemStack, ServerHost,
    Tag,
};

use crate::attributes::{
    MAX_MODIFIER_AMOUNT, SlotSizeModifier, SlotSizeState, parse_modifier_operation,
};
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
#[derive(Clone, Debug, PartialEq)]
pub struct SlotStacks {
    stacks: Vec<Option<ItemSnapshot>>,
    cosmetic: Vec<Option<ItemSnapshot>>,
    render: Vec<bool>,
    active: Vec<bool>,
    drop_rule: DropRule,
    use_native_gui: bool,
    /// Default size before slot modifiers, from the slot definition.
    base_size: u32,
    /// Slot-size modifiers and their lifetime; preserved across data reloads.
    modifiers: Vec<(SlotSizeModifier, bool)>,
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
            base_size: u32::try_from(slot.size()).unwrap_or(0),
            modifiers: Vec::new(),
        }
    }

    /// The Java `CurioStacksHandler` size state computed from this handler.
    pub fn size_state(&self) -> SlotSizeState {
        SlotSizeState {
            base: self.base_size,
            modifiers: self.modifiers.clone(),
        }
    }

    /// Replaces the slot-size modifiers. Call [`Self::resize_to`] afterwards to
    /// apply the resulting size to the stored stacks.
    pub fn set_size_modifiers(&mut self, modifiers: Vec<(SlotSizeModifier, bool)>) {
        self.modifiers = modifiers;
    }

    /// The lifetime-tagged slot-size modifiers.
    pub fn size_modifiers(&self) -> &[(SlotSizeModifier, bool)] {
        &self.modifiers
    }

    /// Rebuilds all four parallel vectors to `target`, copying what fits from
    /// `old` and returning stacks that no longer fit as invalid.
    fn adopt_from(&mut self, old: &Self, target: usize) -> Vec<ItemSnapshot> {
        // Cosmetic support comes from the NEW slot definition, not the old
        // handler; otherwise disabling `add_cosmetic` resurrects old stacks.
        let has_cosmetic = !self.cosmetic.is_empty();
        self.stacks = (0..target)
            .map(|index| old.stacks.get(index).cloned().flatten())
            .collect();
        self.cosmetic = if has_cosmetic {
            (0..target)
                .map(|index| old.cosmetic.get(index).cloned().flatten())
                .collect()
        } else {
            Vec::new()
        };
        self.render = (0..target)
            .map(|index| old.render.get(index).copied().unwrap_or(true))
            .collect();
        self.active = (0..target)
            .map(|index| old.active.get(index).copied().unwrap_or(true))
            .collect();

        let mut invalid = Vec::new();
        for index in target..old.stacks.len() {
            if let Some(item) = old.stacks[index].clone() {
                invalid.push(item);
            }
        }
        if has_cosmetic {
            for index in target..old.cosmetic.len() {
                if let Some(item) = old.cosmetic[index].clone() {
                    invalid.push(item);
                }
            }
        } else {
            // Disabling `add_cosmetic` must not resurrect old cosmetic stacks
            // or silently discard them (review #21).
            invalid.extend(old.cosmetic.iter().flatten().cloned());
        }
        invalid
    }

    /// Applies a new slot count directly, returning items in the dropped tail.
    pub fn resize_to(&mut self, target: usize) -> Vec<ItemSnapshot> {
        let old = self.clone();
        self.adopt_from(&old, target)
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
        match decision.outcome {
            crate::drop::DropOutcome::Keep => Ok(None),
            crate::drop::DropOutcome::Drop => Ok(self.stacks[index].take()),
            // Destroy clears the slot but must never be spawned as a drop
            // entity (review #5).
            crate::drop::DropOutcome::Destroy => {
                self.stacks[index].take();
                Ok(None)
            }
        }
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
#[derive(Clone, Debug, Default, PartialEq)]
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

    /// Keeps stacks rejected by a reload inside the inventory so the ordinary
    /// server tick can drop them instead of losing them (review #3).
    pub fn adopt_invalid(&mut self, invalid: Vec<ItemSnapshot>) {
        self.invalid.extend(invalid);
    }

    /// Rebuilds handlers after a data reload, carrying flags and valid stacks.
    /// Returns the invalid stacks for callers that want to drop them directly;
    /// the same stacks are also adopted into `self.invalid` so a caller that
    /// only persists the inventory cannot lose items (review #3).
    pub fn reload(&mut self, types: &[SlotType]) -> Vec<ItemSnapshot> {
        let fresh = Self::from_slot_types(types);
        let mut invalid = Vec::new();
        let mut rebuilt = BTreeMap::new();
        for (id, handler) in fresh.slots {
            match self.slots.remove(&id) {
                Some(old) => {
                    let mut kept = handler;
                    // Keep both transient and permanent modifiers so the
                    // dynamic slot count survives a data-pack reload.
                    kept.modifiers = old.modifiers.clone();
                    let target =
                        usize::try_from(kept.size_state().resize().size).unwrap_or(0);
                    invalid.extend(kept.adopt_from(&old, target));
                    rebuilt.insert(id, kept);
                }
                None => {
                    rebuilt.insert(id, handler);
                }
            }
        }
        // Only removed slot types remain here; their stacks are invalid.
        for old in self.slots.values() {
            invalid.extend(old.stacks.iter().flatten().cloned());
            invalid.extend(old.cosmetic.iter().flatten().cloned());
        }
        self.slots = rebuilt;
        let rejected = invalid.clone();
        self.adopt_invalid(invalid);
        rejected
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

/// Migrates a saved but currently unloaded player's inventory on login.
///
/// Horizon cannot enumerate offline attachment rows, so the join hook must
/// call this with the player's uuid (review #22).
pub fn migrate_offline(
    host: &mut dyn ServerHost,
    uuid: u128,
    types: &[SlotType],
) -> Result<(), InventoryError> {
    let holder = Holder::Player(uuid);
    let Some(mut inventory) = INVENTORY.get_existing(host.attachments(), holder)? else {
        return Ok(());
    };
    inventory.reload(types);
    INVENTORY.set(host.attachments(), holder, inventory)?;
    Ok(())
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

fn encode_modifiers(modifiers: &[(SlotSizeModifier, bool)]) -> Tag {
    Tag::List(
        modifiers
            .iter()
            .map(|(modifier, permanent)| {
                Tag::compound([
                    ("operation", Tag::String(modifier.operation.as_str().into())),
                    ("amount", Tag::Double(modifier.amount.to_bits())),
                    ("permanent", Tag::Byte(i8::from(*permanent))),
                ])
            })
            .collect(),
    )
}

fn decode_modifiers(value: Option<&Tag>) -> Result<Vec<(SlotSizeModifier, bool)>, AttachmentError> {
    let Tag::List(list) = value.ok_or(AttachmentError::Decode)? else {
        return Err(AttachmentError::Decode);
    };
    list.iter()
        .map(|tag| {
            let Tag::Compound(map) = tag else {
                return Err(AttachmentError::Decode);
            };
            let Tag::String(operation) =
                map.get(&"operation".into()).ok_or(AttachmentError::Decode)?
            else {
                return Err(AttachmentError::Decode);
            };
            let operation = parse_modifier_operation(&operation.to_string_lossy())
                .ok_or(AttachmentError::Decode)?;
            let Tag::Double(bits) = map.get(&"amount".into()).ok_or(AttachmentError::Decode)?
            else {
                return Err(AttachmentError::Decode);
            };
            let amount = f64::from_bits(*bits);
            if !amount.is_finite() || amount.abs() > MAX_MODIFIER_AMOUNT {
                eprintln!("[curios] slot modifier amount {amount} is out of range");
                return Err(AttachmentError::Decode);
            }
            let Tag::Byte(permanent) =
                map.get(&"permanent".into()).ok_or(AttachmentError::Decode)?
            else {
                return Err(AttachmentError::Decode);
            };
            if *permanent != 0 && *permanent != 1 {
                return Err(AttachmentError::Decode);
            }
            Ok((
                SlotSizeModifier {
                    operation,
                    amount,
                },
                *permanent == 1,
            ))
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
    out.insert(
        "base_size".into(),
        Tag::Int(i32::try_from(handler.base_size).unwrap_or(i32::MAX)),
    );
    out.insert("modifiers".into(), encode_modifiers(&handler.modifiers));
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
    let Tag::Int(base_size) = map.get(&"base_size".into()).ok_or(AttachmentError::Decode)?
    else {
        return Err(AttachmentError::Decode);
    };
    let base_size = u32::try_from(*base_size).map_err(|_| AttachmentError::Decode)?;
    let modifiers = decode_modifiers(map.get(&"modifiers".into()))?;
    Ok(SlotStacks {
        stacks,
        cosmetic,
        render,
        active,
        drop_rule,
        use_native_gui: *native == 1,
        base_size,
        modifiers,
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
    fn codec_rejects_truncated_and_broken_compounds() {
        assert_eq!(
            decode_inventory(&Compound::new()),
            Err(AttachmentError::Decode)
        );

        let mut missing_invalid = Compound::new();
        missing_invalid.insert("slots".into(), Tag::Compound(Compound::new()));
        assert_eq!(
            decode_inventory(&missing_invalid),
            Err(AttachmentError::Decode)
        );

        let mut wrong_slots = Compound::new();
        wrong_slots.insert("slots".into(), Tag::List(Vec::new()));
        wrong_slots.insert("invalid".into(), Tag::List(Vec::new()));
        assert_eq!(decode_inventory(&wrong_slots), Err(AttachmentError::Decode));

        // A handler without the size fields is a truncated record, not a
        // default-sized slot: decoding must fail loudly.
        let mut handler = Compound::new();
        handler.insert("stacks".into(), Tag::List(Vec::new()));
        handler.insert("cosmetic".into(), Tag::List(Vec::new()));
        handler.insert("render".into(), Tag::List(Vec::new()));
        handler.insert("active".into(), Tag::List(Vec::new()));
        handler.insert("drop_rule".into(), Tag::String("default".into()));
        handler.insert("use_native_gui".into(), Tag::Byte(1));
        let mut slots = Compound::new();
        slots.insert("ring".into(), Tag::Compound(handler));
        let mut truncated = Compound::new();
        truncated.insert("slots".into(), Tag::Compound(slots));
        truncated.insert("invalid".into(), Tag::List(Vec::new()));
        assert_eq!(decode_inventory(&truncated), Err(AttachmentError::Decode));

        // A modifier missing its operation is rejected as well.
        let mut broken = encode_inventory(&CurioInventory::from_slot_types(&[slot()])).unwrap();
        if let Some(Tag::Compound(slots)) = broken.get_mut(&"slots".into()) {
            if let Some(Tag::Compound(handler)) = slots.get_mut(&"ring".into()) {
                handler.insert(
                    "modifiers".into(),
                    Tag::List(vec![Tag::compound([(
                        "amount",
                        Tag::Double(1.0_f64.to_bits()),
                    )])]),
                );
            }
        }
        assert_eq!(decode_inventory(&broken), Err(AttachmentError::Decode));

        // A finite but absurd amount is refused before any allocation.
        let mut absurd = encode_inventory(&CurioInventory::from_slot_types(&[slot()])).unwrap();
        if let Some(Tag::Compound(slots)) = absurd.get_mut(&"slots".into()) {
            if let Some(Tag::Compound(handler)) = slots.get_mut(&"ring".into()) {
                handler.insert(
                    "modifiers".into(),
                    Tag::List(vec![Tag::compound([
                        ("operation", Tag::String("add_value".into())),
                        ("amount", Tag::Double(1.0e30_f64.to_bits())),
                        ("permanent", Tag::Byte(1)),
                    ])]),
                );
            }
        }
        assert_eq!(decode_inventory(&absurd), Err(AttachmentError::Decode));
    }

    #[test]
    fn codec_round_trip_property_over_sizes_and_amounts() {
        use crate::attributes::ModifierOperation;

        for size in 0..=5 {
            for amount in [-1.0, 0.0, 2.5] {
                let slot = SlotType::new("ring").unwrap().with_size(size);
                let mut inventory = CurioInventory::from_slot_types(&[slot]);
                inventory.slot_mut("ring").unwrap().set_size_modifiers(vec![(
                    SlotSizeModifier {
                        operation: ModifierOperation::AddValue,
                        amount,
                    },
                    true,
                )]);
                let encoded = encode_inventory(&inventory).unwrap();
                assert_eq!(decode_inventory(&encoded).unwrap(), inventory);
            }
        }
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
    fn apply_drop_only_returns_dropped_items() {
        use crate::drop::{DropDecision, DropOutcome};

        let item = snapshot("minecraft:stone");
        let mut kept = SlotStacks::new(&slot());
        kept.set_stack(0, Some(item.clone())).unwrap();
        assert_eq!(
            kept.apply_drop(
                0,
                DropDecision {
                    rule: crate::drop::DropRule::AlwaysKeep,
                    outcome: DropOutcome::Keep
                }
            ),
            Ok(None)
        );
        assert_eq!(kept.stack(0), Some(&Some(item.clone())));

        let mut dropped = SlotStacks::new(&slot());
        dropped.set_stack(0, Some(item.clone())).unwrap();
        assert_eq!(
            dropped.apply_drop(
                0,
                DropDecision {
                    rule: crate::drop::DropRule::AlwaysDrop,
                    outcome: DropOutcome::Drop
                }
            ),
            Ok(Some(item.clone()))
        );
        assert_eq!(dropped.stack(0), Some(&None));

        let mut destroyed = SlotStacks::new(&slot());
        destroyed.set_stack(0, Some(item.clone())).unwrap();
        assert_eq!(
            destroyed.apply_drop(
                0,
                DropDecision {
                    rule: crate::drop::DropRule::Destroy,
                    outcome: DropOutcome::Destroy
                }
            ),
            Ok(None)
        );
        assert_eq!(destroyed.stack(0), Some(&None));
    }

    #[test]
    fn slot_size_modifiers_round_trip_and_apply_on_reload() {
        use crate::attributes::ModifierOperation;

        let mut inventory = CurioInventory::from_slot_types(&[slot()]);
        let handler = inventory.slot_mut("ring").unwrap();
        handler
            .set_stack(1, Some(snapshot("minecraft:stone")))
            .unwrap();
        handler.set_size_modifiers(vec![(
            SlotSizeModifier {
                operation: ModifierOperation::AddValue,
                amount: 2.0,
            },
            true,
        )]);

        let encoded = encode_inventory(&inventory).unwrap();
        let decoded = decode_inventory(&encoded).unwrap();
        assert_eq!(inventory, decoded);

        let types = [slot()];
        let mut reloaded = decoded;
        reloaded.reload(&types);
        assert!(reloaded.invalid_stacks().is_empty());
        assert_eq!(reloaded.slot("ring").unwrap().len(), 4);
        assert_eq!(reloaded.slot("ring").unwrap().size_modifiers().len(), 1);
        assert_eq!(
            reloaded.slot("ring").unwrap().stack(1),
            Some(&Some(snapshot("minecraft:stone")))
        );
    }

    #[test]
    fn disabling_cosmetics_marks_old_cosmetic_stacks_invalid() {
        use crate::slots::SlotDefinition;

        let definition = SlotDefinition::from_json(r#"{"size":2,"add_cosmetic":true}"#).unwrap();
        let old_slot = SlotType::from_definition("ring", &definition).unwrap();
        let mut inventory = CurioInventory::from_slot_types(&[old_slot]);
        let item = snapshot("minecraft:stone");
        inventory
            .slot_mut("ring")
            .unwrap()
            .set_cosmetic(0, Some(item.clone()))
            .unwrap();

        let new_slot = SlotType::new("ring").unwrap().with_size(2);
        inventory.reload(&[new_slot]);
        assert_eq!(inventory.invalid_stacks(), &[item]);
        assert!(inventory.slot("ring").unwrap().cosmetic(0).is_none());
    }

    #[test]
    fn shrinking_slot_size_reports_invalid_tail() {
        use crate::attributes::ModifierOperation;

        let mut handler = SlotStacks::new(&slot());
        handler
            .set_stack(1, Some(snapshot("minecraft:stone")))
            .unwrap();
        handler.set_size_modifiers(vec![(
            SlotSizeModifier {
                operation: ModifierOperation::AddValue,
                amount: -1.0,
            },
            false,
        )]);
        let target = handler.size_state().resize().size as usize;
        let invalid = handler.resize_to(target);
        assert_eq!(target, 1);
        assert_eq!(invalid, vec![snapshot("minecraft:stone")]);
        assert!(handler.stack(1).is_none());
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
