//! Equip/unequip decisions and death-drop resolution, kept pure and
//! server-authoritative.
//!
//! Adapted from `CuriosApi.isStackValid`, `CuriosStacksResourceHandler.isValid`,
//! `CurioCanEquipEvent`, `CurioCanUnequipEvent` and `CuriosCommonEvents.handleDrops`
//! of TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4).

use crate::config;
use crate::curio::{self, CurioContext};
use crate::drop::{self, DropDecision, DropRules, ItemPredicate};
use crate::inventory::ItemSnapshot;
use crate::slots::{SlotContext, SlotType};

/// Outcome of an external equip event, mirroring Java `TriState`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TriState {
    /// No override: use the computed result.
    Default,
    /// Force acceptance.
    True,
    /// Force rejection.
    False,
}

impl TriState {
    /// Applies the override to an original result.
    pub const fn apply(self, original: bool) -> bool {
        match self {
            Self::Default => original,
            Self::True => true,
            Self::False => false,
        }
    }
}

/// Whether an item passes a slot's validators.
///
/// The `curios:tag` predicate is approximated by the item's own curio tag
/// (`curios:<slot id>`) or the universal `curios:curio` tag, matching Java
/// `CuriosSlots.testPredicates` for the bundled slot definitions.
pub fn is_stack_valid(slot: &SlotType, context: &SlotContext, item_id: &str) -> bool {
    let expected = format!("curios:{}", slot.id());
    slot.is_item_valid(context, &item_id, |validator, _, item| {
        match validator.resource_id().as_str().as_str() {
            "curios:all" => true,
            "curios:none" => false,
            "curios:tag" => *item == expected || *item == "curios:curio",
            _ => false,
        }
    })
}

/// Server-authoritative `canEquip`: validators, registered behavior, explicit
/// supported slots and an optional event override, in that order.
pub fn can_equip(
    slot: &SlotType,
    context: &SlotContext,
    item_id: &str,
    event: TriState,
) -> bool {
    let original = is_stack_valid(slot, context, item_id)
        && curio::behavior(item_id).is_none_or(|behavior| {
            curio::accepts_slot(behavior.as_ref(), slot.id())
                && behavior.can_equip(&CurioContext {
                    slot: slot.id(),
                    index: context.index as u32,
                    item: item_id,
                })
        });
    let mut dispatched = crate::events::CurioCanEquipEvent::new(item_id, context.clone(), original);
    let result = crate::events::can_equip_override(&mut dispatched);
    event.apply(result.apply(original))
}

/// Server-authoritative `canUnequip`, including an external override.
pub fn can_unequip(context: &SlotContext, item_id: &str, event: TriState) -> bool {
    let original = curio::behavior(item_id).is_none_or(|behavior| {
        behavior.can_unequip(&CurioContext {
            slot: context.identifier.as_str(),
            index: context.index as u32,
            item: item_id,
        })
    });
    let mut dispatched =
        crate::events::CurioCanUnequipEvent::new(item_id, context.clone(), original);
    let result = crate::events::can_unequip_override(&mut dispatched);
    event.apply(result.apply(original))
}

/// Resolves the death-drop outcome for one stored item.
///
/// Overrides come first, then the item's own rule, then the slot's rule.
/// `keep_inventory` is derived from the server's `keep_curios` setting and the
/// game rule; `drop_prevented` stands for `PREVENT_EQUIPMENT_DROP`.
pub fn decide_death_drop<P>(
    rules: &DropRules<P>,
    item: &ItemSnapshot,
    item_rule: drop::DropRule,
    slot_rule: drop::DropRule,
    drop_prevented: bool,
) -> DropDecision
where
    P: ItemPredicate<ItemSnapshot>,
{
    drop::decide_drop(
        rules,
        item,
        item_rule,
        slot_rule,
        !config::drops_on_death(),
        drop_prevented,
    )
}

/// Same as [`decide_death_drop`] but first dispatches `DropRulesEvent` to
/// registered listeners, honoring the last matching override.
pub fn decide_death_drop_with_events(
    entity: u32,
    item: &ItemSnapshot,
    item_rule: drop::DropRule,
    slot_rule: drop::DropRule,
    drop_prevented: bool,
) -> DropDecision {
    let mut event = crate::events::DropRulesEvent {
        entity,
        overrides: Vec::new(),
    };
    crate::events::dispatch_drop_rules(&mut event);
    decide_death_drop(
        &event.drop_rules(),
        item,
        item_rule,
        slot_rule,
        drop_prevented,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::SlotContext;

    fn context(id: &str) -> SlotContext {
        SlotContext {
            identifier: id.to_owned(),
            entity: Some(1),
            index: 0,
            cosmetic: false,
            visible: true,
        }
    }

    #[test]
    fn tri_state_matches_java() {
        assert!(TriState::Default.apply(true));
        assert!(!TriState::Default.apply(false));
        assert!(TriState::True.apply(false));
        assert!(!TriState::False.apply(true));
    }

    #[test]
    fn tag_predicate_accepts_own_and_universal_tag() {
        let types = crate::registry::slot_types();
        let slot = types.iter().find(|slot| slot.id() == "ring").unwrap();
        let ctx = context("ring");
        assert!(is_stack_valid(slot, &ctx, "curios:ring"));
        assert!(is_stack_valid(slot, &ctx, "curios:curio"));
        assert!(!is_stack_valid(slot, &ctx, "curios:necklace"));
        assert!(can_equip(slot, &ctx, "curios:ring", TriState::False) == false);
        assert!(can_equip(slot, &ctx, "curios:ring", TriState::True));
    }
}
