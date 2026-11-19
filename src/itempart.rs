//! Item-side behaviour for items in the `curios:curio` tag.
//!
//! Java attaches `ICurio` through a capability on each registered item; Horizon
//! uses one `ItemPart` attached to the tag instead, so every tagged item gets
//! the same behaviour without enumerating item keys. The part only forwards to
//! the effect and behavior registries; it never mutates an inventory.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use mod_api::{
    ApplyEnchantmentsEvent, CanEquipEvent, ClientHost, Host, ItemPart, MakesPiglinsNeutralEvent,
    Registrar, ShouldCauseReequipAnimationEvent, TooltipEvent,
};

use crate::curio::{self, CurioBehavior, CurioContext};
use crate::effects;
use crate::lang;
use crate::registry;

/// A slot name as the registry and `CurioBehavior` know it: the bare id.
/// Both vanilla equipment names (`head`) and qualified ids (`curios:head`)
/// resolve to it.
pub fn equip_slot_id(slot: &str) -> String {
    slot.rsplit(':').next().unwrap_or(slot).to_owned()
}

/// Whether a registered behavior refuses the item in this slot.
///
/// Unknown slot types (vanilla hands, saddle) are left alone: mod slots do not
/// reach `ItemPart::can_equip`, so refusing there would break holding a curio.
pub fn behavior_refuses_equip(
    behavior: Option<&dyn CurioBehavior>,
    slot: &str,
    item: &str,
) -> bool {
    let Some(behavior) = behavior else {
        return false;
    };
    let slot_id = equip_slot_id(slot);
    if slot_id.is_empty() || registry::slot_type_index(&slot_id).is_none() {
        return false;
    }
    !curio::accepts_slot(behavior, &slot_id)
        || !behavior.can_equip(&CurioContext {
            slot: &slot_id,
            index: 0,
            item,
        })
}

/// Bare slot ids for a tooltip: translations are keyed by path, not by
/// `namespace:path`. An unrestricted behavior has no slot to show.
pub fn tooltip_slot_ids(behavior: Option<&dyn CurioBehavior>) -> Vec<String> {
    behavior
        .map(|behavior| {
            behavior
                .supported_slots()
                .iter()
                .map(|slot| slot.rsplit(':').next().unwrap_or(slot).to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Tooltip label and, when a behavior restricts the item, its slot names.
pub fn tooltip_lines(host: &dyn ClientHost, item_id: &str, lines: &mut Vec<(mod_api::Text, u32)>) {
    lines.push((lang::text(host, lang::KEY_NAME), 0xd580ff));
    for slot in tooltip_slot_ids(curio::behavior(item_id).as_deref()) {
        lines.push((lang::slot_text(host, &slot), 0x9a7ba8));
    }
}

/// One item part for the whole `curios:curio` tag.
pub struct CurioItemPart;

#[mod_api::horizon_part]
impl ItemPart for CurioItemPart {
    fn makes_piglins_neutral(&self, host: &dyn Host, event: &mut MakesPiglinsNeutralEvent<'_>) {
        effects::makes_piglins_neutral(host, event);
    }

    fn should_cause_reequip_animation(
        &self,
        host: &dyn ClientHost,
        event: &mut ShouldCauseReequipAnimationEvent<'_>,
    ) {
        let host_base: &dyn Host = host;
        let item_id = event.stack.item().id(host_base);
        effects::suppress_reequip_animation(event, &item_id);
    }

    fn tooltip(&self, host: &dyn ClientHost, event: &mut TooltipEvent<'_>) {
        let host_base: &dyn Host = host;
        let item_id = event.stack.item().id(host_base);
        tooltip_lines(host, &item_id, event.lines);
    }

    fn can_equip(&self, host: &dyn Host, event: &mut CanEquipEvent<'_>) {
        let item_id = event.stack.item().id(host);
        if behavior_refuses_equip(curio::behavior(&item_id).as_deref(), event.slot, &item_id) {
            event.can_equip = false;
        }
    }

    fn apply_enchantments(&self, _host: &dyn Host, _event: &mut ApplyEnchantmentsEvent<'_>) {
        // This event enchants the stack itself. Curio Fortune/Looting bonuses
        // apply to the wearer's tool through `Hooks::get_enchantment_level`,
        // using `effects::apply_enchantment_bonus`, not through this part.
    }
}

/// For `Hooks::register`: tag-based attachment mirrors Java's curio tag.
pub fn register(r: &mut dyn Registrar) {
    r.item_part_tag("curios:curio", CurioItemPart);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Open;
    impl CurioBehavior for Open {}

    struct Restricted(Vec<String>);
    impl CurioBehavior for Restricted {
        fn supported_slots(&self) -> &[String] {
            &self.0
        }
    }

    struct Refusing;
    impl CurioBehavior for Refusing {
        fn can_equip(&self, _ctx: &CurioContext<'_>) -> bool {
            false
        }
    }

    #[test]
    fn equip_slot_names_are_normalised_to_bare_ids() {
        assert_eq!(equip_slot_id("head"), "head");
        assert_eq!(equip_slot_id("curios:ring"), "ring");
    }

    #[test]
    fn an_unregistered_behavior_never_refuses() {
        assert!(!behavior_refuses_equip(None, "head", "curios:ring"));
    }

    #[test]
    fn supported_slots_control_refusal() {
        let restricted = Restricted(vec!["ring".to_owned()]);
        assert!(!behavior_refuses_equip(
            Some(&restricted),
            "ring",
            "curios:band"
        ));
        assert!(behavior_refuses_equip(
            Some(&restricted),
            "head",
            "curios:band"
        ));
    }

    #[test]
    fn an_explicit_can_equip_refusal_is_honoured() {
        assert!(behavior_refuses_equip(
            Some(&Refusing),
            "head",
            "curios:band"
        ));
    }

    #[test]
    fn vanilla_slots_are_not_treated_as_curio_slots() {
        let open = Open;
        assert!(!behavior_refuses_equip(
            Some(&open),
            "mainhand",
            "curios:band"
        ));
    }

    #[test]
    fn tooltip_slots_are_bare_ids() {
        assert!(tooltip_slot_ids(None).is_empty());
        let restricted = Restricted(vec!["curios:ring".to_owned(), "curios:head".to_owned()]);
        assert_eq!(
            tooltip_slot_ids(Some(&restricted)),
            vec!["ring".to_owned(), "head".to_owned()]
        );
    }

    #[test]
    fn an_unrestricted_behavior_has_no_slot_label() {
        assert!(tooltip_slot_ids(Some(&Open)).is_empty());
    }
}
