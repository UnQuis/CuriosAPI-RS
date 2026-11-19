//! In-world game tests for the Curios port.
//!
//! Every check is read-only: it decodes bundled data, exercises the strict
//! attachment codec or resolves pure drop rules. Nothing grants an item,
//! spawns an entity or leaves a value in an attachment store, so the tests are
//! safe to run in a real test world.
//!
//! The vanilla `minecraft:empty` structure is the placement for every test.
//! It is the structure the pinned SDK's own registration example uses, and it
//! needs no mod-owned template: these functions assert on mod state, not on
//! placed blocks.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::collections::BTreeSet;
use std::sync::Arc;

use mod_api::{
    GameTestContext, GameTestDefinition, GameTestFunction, GameTestResult, Host, ItemPlace,
    RegisterGameTestsEvent,
};

use crate::drop::{decide_drop, DropOutcome, DropRule, DropRules};
use crate::inventory::{CurioInventory, INVENTORY};
use crate::{place, registry};

const STRUCTURE: &str = "minecraft:empty";

fn fail(context: &str, message: impl std::fmt::Display) -> Result<(), String> {
    Err(format!("curios {context}: {message}"))
}

/// The eleven bundled slot definitions decode, are distinct and keep stable
/// registry indices.
pub fn check_registry() -> Result<(), String> {
    let types = registry::builtin_slot_types()
        .map_err(|error| format!("curios registry: built-in definition refused: {error:?}"))?;
    if types.len() != 11 {
        return fail("registry", format!("expected 11 slots, got {}", types.len()));
    }
    let mut seen = BTreeSet::new();
    for slot in &types {
        if slot.id().is_empty() {
            return fail("registry", "an empty slot id was decoded");
        }
        if !seen.insert(slot.id()) {
            return fail("registry", format!("duplicate slot id {}", slot.id()));
        }
    }
    for (index, slot) in types.iter().enumerate() {
        if registry::slot_type_index(slot.id()) != Some(index as u16) {
            return fail(
                "registry",
                format!("unstable index for slot {}", slot.id()),
            );
        }
    }
    Ok(())
}

/// The attachment's strict codec round-trips a freshly built inventory
/// without touching an attachment store.
pub fn check_inventory_codec() -> Result<(), String> {
    let codec = INVENTORY
        .codec
        .as_ref()
        .ok_or("curios inventory: INVENTORY has no codec")?;
    let types = registry::builtin_slot_types()
        .map_err(|error| format!("curios inventory: definitions refused: {error:?}"))?;
    let inventory = CurioInventory::from_slot_types(&types);
    let encoded = (codec.encode)(&inventory)
        .map_err(|error| format!("curios inventory: encode refused: {error:?}"))?;
    let decoded = (codec.decode)(&encoded)
        .map_err(|error| format!("curios inventory: decode refused: {error:?}"))?;
    if decoded != inventory {
        return fail("inventory", "codec round-trip changed the inventory");
    }
    Ok(())
}

/// Mod place keys encode entity, slot type and slot (plus the cosmetic bit)
/// and resolve back to the registered place provider.
pub fn check_place_keys() -> Result<(), String> {
    let types = registry::builtin_slot_types()
        .map_err(|error| format!("curios places: definitions refused: {error:?}"))?;
    for (index, slot) in types.iter().enumerate() {
        let type_index = index as u16;
        let key = place::place_key(7, type_index, 3);
        let (entity, resolved_type, resolved_slot) = place::parse_key(key);
        if (entity, resolved_type, resolved_slot) != (7, type_index, 3) {
            return fail("places", format!("key round-trip failed for {}", slot.id()));
        }
        match place::curio_place(7, slot.id(), 3) {
            Some(ItemPlace::Mod { provider, key: place_key })
                if provider == place::PROVIDER && place_key == key => {}
            other => return fail("places", format!("unexpected place for {}: {other:?}", slot.id())),
        }
        let cosmetic = place::place_key(7, type_index, 3 | place::COSMETIC_FLAG);
        if !place::is_cosmetic(cosmetic) {
            return fail("places", "the cosmetic bit was lost");
        }
        // `parse_key` returns the raw low half; callers mask it with `SLOT_MASK`.
        if place::parse_key(cosmetic).2 & place::SLOT_MASK != 3 {
            return fail("places", "the cosmetic bit corrupted the slot index");
        }
        match place::cosmetic_place(7, slot.id(), 3) {
            Some(ItemPlace::Mod { provider, key: place_key })
                if provider == place::PROVIDER && place_key == cosmetic => {}
            other => {
                return fail("places", format!("unexpected cosmetic place for {}: {other:?}", slot.id()))
            }
        }
    }
    Ok(())
}

/// Drop precedence and outcomes match the Java `handleDrops` branches.
pub fn check_drop_rules() -> Result<(), String> {
    let mut rules = DropRules::new();
    rules.add_override(|item: &&str| *item == "curios:band", DropRule::AlwaysKeep);
    let kept = decide_drop(
        &rules,
        &"curios:band",
        DropRule::Default,
        DropRule::Default,
        false,
        false,
    );
    if kept.outcome != DropOutcome::Keep {
        return fail("drops", format!("always-keep override gave {:?}", kept.outcome));
    }
    let ordinary = decide_drop(
        &rules,
        &"curios:ring",
        DropRule::Default,
        DropRule::Default,
        false,
        false,
    );
    if ordinary.outcome != DropOutcome::Drop {
        return fail("drops", format!("an ordinary item gave {:?}", ordinary.outcome));
    }
    let with_keep_inventory = decide_drop(
        &rules,
        &"curios:ring",
        DropRule::Default,
        DropRule::Default,
        true,
        false,
    );
    if with_keep_inventory.outcome != DropOutcome::Keep {
        return fail("drops", "keepInventory did not keep the item");
    }
    let prevented = decide_drop(
        &rules,
        &"curios:ring",
        DropRule::Default,
        DropRule::Default,
        false,
        true,
    );
    if prevented.outcome != DropOutcome::Destroy {
        return fail("drops", "a prevented drop did not become Destroy");
    }
    Ok(())
}

struct RegistryTest;
impl GameTestFunction for RegistryTest {
    fn run(&self, _: &mut dyn mod_api::ServerHost, _: &GameTestContext) -> GameTestResult {
        outcome(check_registry())
    }
}

struct InventoryCodecTest;
impl GameTestFunction for InventoryCodecTest {
    fn run(&self, _: &mut dyn mod_api::ServerHost, _: &GameTestContext) -> GameTestResult {
        outcome(check_inventory_codec())
    }
}

struct PlaceKeysTest;
impl GameTestFunction for PlaceKeysTest {
    fn run(&self, _: &mut dyn mod_api::ServerHost, _: &GameTestContext) -> GameTestResult {
        outcome(check_place_keys())
    }
}

struct DropRulesTest;
impl GameTestFunction for DropRulesTest {
    fn run(&self, _: &mut dyn mod_api::ServerHost, _: &GameTestContext) -> GameTestResult {
        outcome(check_drop_rules())
    }
}

fn outcome(result: Result<(), String>) -> GameTestResult {
    match result {
        Ok(()) => GameTestResult::Pass,
        Err(message) => GameTestResult::Fail(message),
    }
}

const TESTS: [(&str, &str, fn() -> Arc<dyn GameTestFunction>); 4] = [
    ("curios:test_registry", "curios:registry", || {
        Arc::new(RegistryTest)
    }),
    ("curios:test_inventory", "curios:inventory_codec", || {
        Arc::new(InventoryCodecTest)
    }),
    ("curios:test_places", "curios:place_keys", || {
        Arc::new(PlaceKeysTest)
    }),
    ("curios:test_drops", "curios:drop_rules", || {
        Arc::new(DropRulesTest)
    }),
];

/// For `Hooks::register_game_tests`. Functions and definitions are registered
/// in one pass; a refusal is reported with its id, never swallowed.
pub fn register(_host: &dyn Host, event: &mut RegisterGameTestsEvent<'_>) {
    for (test_id, function_id, make) in TESTS {
        if let Err(error) = event.functions.register(function_id, make()) {
            eprintln!("[curios] could not register game test function {function_id}: {error}");
            continue;
        }
        let definition = GameTestDefinition::new(STRUCTURE.to_owned(), function_id.to_owned());
        if let Err(error) = event.functions.register_test(test_id, definition) {
            eprintln!("[curios] could not register game test {test_id}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_game_test_checks_pass_offline() {
        check_registry().unwrap();
        check_inventory_codec().unwrap();
        check_place_keys().unwrap();
        check_drop_rules().unwrap();
    }

    #[test]
    fn outcomes_report_failures_as_messages() {
        assert_eq!(outcome(Ok(())), GameTestResult::Pass);
        assert_eq!(
            outcome(fail("registry", "broken")),
            GameTestResult::Fail("curios registry: broken".to_owned())
        );
    }

    #[test]
    fn a_drop_override_does_not_change_other_items() {
        let mut rules = DropRules::new();
        rules.add_override(|item: &&str| *item == "curios:band", DropRule::AlwaysKeep);
        let decision = decide_drop(
            &rules,
            &"curios:ring",
            DropRule::Default,
            DropRule::AlwaysDrop,
            false,
            false,
        );
        assert_eq!(decision.rule, DropRule::AlwaysDrop);
        assert_eq!(decision.outcome, DropOutcome::Drop);
    }
}
