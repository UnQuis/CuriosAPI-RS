//! Equip/unequip decisions and death-drop resolution, kept pure and
//! server-authoritative.
//!
//! Adapted from `CuriosApi.isStackValid`, `CuriosStacksResourceHandler.isValid`,
//! `CurioCanEquipEvent`, `CurioCanUnequipEvent` and `CuriosCommonEvents.handleDrops`
//! of TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4).

use mod_api::{GameRuleValue, Host, Identifier, ItemKey, LevelRef};

use crate::config;
use crate::curio::{self, CurioContext};
use crate::drop::{self, DropDecision, DropRules, ItemPredicate};
use crate::inventory::ItemSnapshot;
use crate::slots::{SlotContext, SlotType, SlotValidator};

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

/// Whether an item passes a slot's validators, with an injected predicate.
///
/// This is the pure core used by unit tests; production must use
/// [`is_stack_valid_host`], which resolves real item tags through the host.
pub fn is_stack_valid_with<F>(
    slot: &SlotType,
    context: &SlotContext,
    item_id: &str,
    test: F,
) -> bool
where
    F: Fn(&SlotValidator, &SlotContext, &str) -> bool,
{
    slot.is_item_valid(context, &item_id, |validator, context, _| {
        test(validator, context, item_id)
    })
}

/// Server-authoritative validator resolution through Horizon's merged tags.
///
/// Mirrors Java `CuriosSlots`: `curios:<slot id>` or `curios:curio` membership.
/// A missing tag or an unavailable tag table yields `false`, never a guessed
/// string match, so a real item id like `mymod:ring` is accepted only when its
/// registry tag actually contains it.
pub fn is_stack_valid_host(
    host: &dyn Host,
    slot: &SlotType,
    context: &SlotContext,
    item: ItemKey,
) -> bool {
    let Ok(slot_tag_id) = Identifier::parse(&format!("curios:{}", slot.id())) else {
        eprintln!("[curios] slot id `{}` is not a valid tag id", slot.id());
        return false;
    };
    let Ok(curio_tag_id) = Identifier::parse("curios:curio") else {
        return false;
    };
    let slot_tag = host.tags().key(&slot_tag_id);
    let curio_tag = host.tags().key(&curio_tag_id);
    let in_tag = |tag: Option<mod_api::TagKey>| {
        tag.and_then(|key| host.tags().is_in_tag(item, key)).unwrap_or(false)
    };
    let slot_member = in_tag(slot_tag);
    let curio_member = in_tag(curio_tag);
    is_stack_valid_with(slot, context, "", |validator, _, _| {
        match validator.resource_id().as_str().as_str() {
            "curios:all" => true,
            "curios:none" => false,
            "curios:tag" => slot_member || curio_member,
            _ => false,
        }
    })
}

/// Pure string-tag approximation, kept for the public API and unit tests.
/// It cannot validate real items; call [`is_stack_valid_host`] in production.
pub fn is_stack_valid(slot: &SlotType, context: &SlotContext, item_id: &str) -> bool {
    let expected = format!("curios:{}", slot.id());
    is_stack_valid_with(slot, context, item_id, |validator, _, item| {
        match validator.resource_id().as_str().as_str() {
            "curios:all" => true,
            "curios:none" => false,
            "curios:tag" => item == expected.as_str() || item == "curios:curio",
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

/// Host-aware counterpart of [`can_equip`] used by `PlaceProvider::insert`.
pub fn can_equip_host(
    host: &dyn Host,
    slot: &SlotType,
    context: &SlotContext,
    item: ItemKey,
    item_id: &str,
    event: TriState,
) -> bool {
    let original = is_stack_valid_host(host, slot, context, item)
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

/// Resolves the `keep_inventory` rule for one level at the gameplay call site.
///
/// `keep_curios = default` follows the live game rule; `on`/`off` force it.
/// Errors are returned so a failed read never silently keeps or drops items.
pub fn keep_inventory_from_level(host: &dyn Host, level: LevelRef) -> Result<bool, String> {
    let value = level
        .game_rule(host, "keep_inventory")
        .map_err(|error| format!("could not read keep_inventory game rule: {error:?}"))?;
    match value {
        Some(GameRuleValue::Bool(rule)) => Ok(config::keep_inventory(rule)),
        Some(other) => Err(format!("keep_inventory is not a boolean: {other}")),
        None => Err("keep_inventory game rule is absent".to_owned()),
    }
}

/// Resolves the death-drop outcome for one stored item.
///
/// Overrides come first, then the item's own rule, then the slot's rule.
/// `keep_inventory` must be resolved at the call site with
/// [`keep_inventory_from_level`]; `drop_prevented` stands for
/// `PREVENT_EQUIPMENT_DROP`.
pub fn decide_death_drop<P>(
    rules: &DropRules<P>,
    item: &ItemSnapshot,
    item_rule: drop::DropRule,
    slot_rule: drop::DropRule,
    keep_inventory: bool,
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
        keep_inventory,
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
    keep_inventory: bool,
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
        keep_inventory,
        drop_prevented,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drop::{DropRule, DropRules};
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
    fn drop_precedence_is_exhaustive_over_all_rules() {
        let yes: fn(&&str) -> bool = |_| true;
        let no: fn(&&str) -> bool = |_| false;
        let resolve = |rules: &DropRules<fn(&&str) -> bool>, item, slot| {
            drop::resolve_drop_rule(rules.overrides(), &"curios:ring", item, slot)
        };

        for item in DropRule::ALL {
            for slot in DropRule::ALL {
                let none: DropRules<fn(&&str) -> bool> = DropRules::new();
                let expected = if item == DropRule::Default {
                    slot
                } else {
                    item
                };
                assert_eq!(resolve(&none, item, slot), expected);

                for override_rule in DropRule::ALL {
                    let mut rules = DropRules::new();
                    rules.add_override(yes, override_rule);
                    let expected = if override_rule == DropRule::Default {
                        slot
                    } else {
                        override_rule
                    };
                    assert_eq!(resolve(&rules, item, slot), expected);

                    for second in DropRule::ALL {
                        let mut chained = DropRules::new();
                        chained.add_override(yes, override_rule);
                        chained.add_override(yes, second);
                        chained.add_override(no, DropRule::Destroy);
                        let expected = if second == DropRule::Default {
                            slot
                        } else {
                            second
                        };
                        assert_eq!(resolve(&chained, item, slot), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn injected_tag_predicate_accepts_real_item_ids() {
        let types = crate::registry::slot_types();
        let slot = types.iter().find(|slot| slot.id() == "ring").unwrap();
        let ctx = context("ring");
        // The old test asserted that a fake `curios:ring` item id passed. Real
        // ids such as `mymod:ring` are decided by the injected tag predicate.
        let tag_test = |validator: &SlotValidator, _: &SlotContext, item: &str| {
            validator.resource_id().as_str().as_str() == "curios:tag"
                && matches!(item, "mymod:ring" | "curios:curio")
        };
        assert!(is_stack_valid_with(slot, &ctx, "mymod:ring", tag_test));
        assert!(is_stack_valid_with(slot, &ctx, "curios:curio", tag_test));
        assert!(!is_stack_valid_with(slot, &ctx, "curios:necklace", tag_test));
        assert!(!can_equip(slot, &ctx, "curios:ring", TriState::False));
        assert!(can_equip(slot, &ctx, "curios:ring", TriState::True));
    }
}
