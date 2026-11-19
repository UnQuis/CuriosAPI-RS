//! Public API for other mods, the Rust counterpart of `CuriosApi`.
//!
//! Adapted from `top.theillusivec4.curios.api.CuriosApi` and `api.SlotResult`
//! of TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4).

pub use crate::curio::{register_curio, CurioBehavior, CurioContext, RegisterError};
pub use crate::drop::DropRule;
pub use crate::equip::{can_equip, can_unequip, is_stack_valid, TriState};
pub use crate::inventory::{CurioInventory, ItemSnapshot, SlotStacks};
pub use crate::registry::slot_types;
pub use crate::slots::{SlotContext, SlotType};

/// One found curio: the slot it occupies and the stored item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotResult {
    /// Slot type id (for example `ring`).
    pub slot: String,
    /// Index inside that slot type.
    pub index: usize,
    /// The item in the slot.
    pub item: ItemSnapshot,
}

/// Finds the first equipped item with the given canonical item id.
pub fn find_first(inventory: &CurioInventory, item_id: &str) -> Option<SlotResult> {
    inventory
        .find_first(item_id)
        .map(|(slot, index, item)| SlotResult {
            slot: slot.to_owned(),
            index,
            item: item.clone(),
        })
}

/// Number of slots of one type available on a freshly created inventory.
pub fn slot_count(slot: &str) -> Option<usize> {
    slot_types()
        .iter()
        .find(|entry| entry.id() == slot)
        .and_then(|entry| usize::try_from(entry.size()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_items_and_reports_slot_counts() {
        assert_eq!(slot_count("ring"), Some(1));
        assert_eq!(slot_count("missing"), None);
        let inventory = CurioInventory::from_slot_types(&slot_types());
        assert!(find_first(&inventory, "minecraft:stone").is_none());
    }
}
