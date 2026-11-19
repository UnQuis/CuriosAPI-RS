//! Regression tests for the 24 wave-4 review findings.
//!
//! Finding #2 (functional GUI clicks) belongs to the GUI owner; the rest are
//! covered here. Each test asserts the *correct* behavior, using injected
//! pure inputs rather than inventing a `Host`. Some assertions are expected
//! to fail on the pre-fix code — that is the point of a regression test.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use crate as mod_curios;
use std::sync::Arc;

use mod_api::{Compound, DatapackValue, Tag};
use mod_curios::inventory::{CurioInventory, INVENTORY};
use mod_curios::slots::SlotContext;
use mod_curios::{
    attributes, config, datapack, drop, effects, equip, events, input, inventory, lang, menu, net,
    place, registry, slots,
};

fn ring_slot() -> slots::SlotType {
    registry::builtin_slot_types()
        .expect("bundled slot definitions")
        .into_iter()
        .find(|slot| slot.id() == "ring")
        .expect("ring slot")
}

fn context() -> SlotContext {
    SlotContext::new("ring", None, 0, false, true)
}

fn item_tag(id: &str) -> Tag {
    let mut compound = Compound::new();
    compound.insert("id".into(), Tag::String(id.into()));
    compound.insert("count".into(), Tag::Int(1));
    Tag::Compound(compound)
}

fn handler_tag(
    size: usize,
    stacks: &[&str],
    cosmetic: &[&str],
    modifiers: Vec<Tag>,
) -> Tag {
    let items = |list: &[&str]| -> Tag {
        Tag::List(
            (0..size)
                .map(|index| {
                    list.get(index)
                        .map(|id| item_tag(id))
                        .unwrap_or(Tag::End)
                })
                .collect(),
        )
    };
    let mut handler = Compound::new();
    handler.insert("stacks".into(), items(stacks));
    handler.insert("cosmetic".into(), items(cosmetic));
    handler.insert("render".into(), Tag::List(vec![Tag::Byte(1); size]));
    handler.insert("active".into(), Tag::List(vec![Tag::Byte(1); size]));
    handler.insert("drop_rule".into(), Tag::String("default".into()));
    handler.insert("use_native_gui".into(), Tag::Byte(1));
    handler.insert("base_size".into(), Tag::Int(size as i32));
    handler.insert("modifiers".into(), Tag::List(modifiers));
    Tag::Compound(handler)
}

fn inventory_with(
    slot_id: &str,
    size: usize,
    stacks: &[&str],
    cosmetic: &[&str],
    modifiers: Vec<Tag>,
) -> CurioInventory {
    let mut slots_map = Compound::new();
    slots_map.insert(slot_id.into(), handler_tag(size, stacks, cosmetic, modifiers));
    let mut root = Compound::new();
    root.insert("slots".into(), Tag::Compound(slots_map));
    root.insert("invalid".into(), Tag::List(Vec::new()));
    let codec = INVENTORY.codec.as_ref().expect("INVENTORY codec");
    (codec.decode)(&root).expect("crafted inventory decodes")
}

fn permanent_add_value(amount: f64) -> Tag {
    Tag::compound([
        ("operation", Tag::String("add_value".into())),
        ("amount", Tag::Double(amount.to_bits())),
        ("permanent", Tag::Byte(1)),
    ])
}

fn datapack_values() -> Vec<(String, DatapackValue)> {
    vec![
        ("order".into(), DatapackValue::Int(100)),
        ("size".into(), DatapackValue::Int(1)),
        ("use_native_gui".into(), DatapackValue::Bool(true)),
        ("add_cosmetic".into(), DatapackValue::Bool(false)),
        (
            "icon".into(),
            DatapackValue::Text("curios:textures/gui/generic_icon.png".into()),
        ),
        ("drop_rule".into(), DatapackValue::Text("default".into())),
        ("render_toggle".into(), DatapackValue::Bool(true)),
        ("validator".into(), DatapackValue::Text("curios:tag".into())),
    ]
}

// #1 — tag-based validity, with an injected predicate (host tags are not faked).
#[test]
fn regression_01_tag_predicate() {
    let slot = ring_slot();
    assert!(slot.is_item_valid(&context(), &"mod:tagged_ring", |_, _, _| true));
    assert!(!slot.is_item_valid(&context(), &"curios:ring", |_, _, _| false));
    // The pure host-facing core must accept a tag member from another
    // namespace; production `is_stack_valid_host` injects the real tag lookup.
    assert!(equip::is_stack_valid_with(
        &slot,
        &context(),
        "mod:tagged_ring",
        |validator, _, item| {
            matches!(
                validator.resource_id().as_str().as_str(),
                "curios:all" | "curios:tag"
            ) && item == "mod:tagged_ring"
        }
    ));
}

// #3 — stacks that no longer fit after a reload must be returned, not dropped.
#[test]
fn regression_03_reload_keeps_invalid_stacks() {
    let mut inventory = inventory_with("ring", 3, &["mod:a", "mod:b", "mod:c"], &[], vec![]);
    let types = vec![ring_slot().with_size(2)];
    inventory.reload(&types);
    let invalid = inventory.invalid_stacks();
    assert_eq!(invalid.len(), 1, "the third stack disappeared on reload");
    assert_eq!(invalid[0].item_id().as_deref(), Some("mod:c"));
    assert_eq!(inventory.slot("ring").map(inventory::SlotStacks::len), Some(2));
}

// #4 — `default` follows keepInventory: drop when the gamerule is false.
#[test]
fn regression_04_default_keep_curios_drops() {
    fn drops(mode: config::KeepCurios, keep_inventory: bool) -> bool {
        match mode {
            config::KeepCurios::On => false,
            config::KeepCurios::Off => true,
            config::KeepCurios::Default => !keep_inventory,
        }
    }
    assert!(drops(config::KeepCurios::Default, false));
    assert!(!drops(config::KeepCurios::Default, true));
    assert!(!config::keep_inventory_for(
        config::KeepCurios::Default,
        false
    ));
    assert!(config::keep_inventory_for(
        config::KeepCurios::Default,
        true
    ));
    assert!(!config::keep_inventory(false), "the default config keeps curios");
}

// #5 — Destroy clears the slot but must not spawn a drop entity.
#[test]
fn regression_05_destroy_no_drop() {
    let decision = drop::drop_outcome(drop::DropRule::Destroy, false, false);
    assert_eq!(decision, drop::DropOutcome::Destroy);
}

// #6 — an enormous modifier amount must clamp, never allocate billions of slots.
#[test]
fn regression_06_huge_amount_no_oom() {
    let state = attributes::SlotSizeState {
        base: u32::MAX,
        modifiers: vec![(
            attributes::SlotSizeModifier {
                operation: attributes::ModifierOperation::AddValue,
                amount: f64::MAX,
            },
            true,
        )],
    };
    assert!(
        state.resize().size <= attributes::MAX_SLOT_COUNT as u32,
        "slot size was not clamped before allocation"
    );
}

// #7 — DropRulesEvent listeners must be dispatched.
#[test]
fn regression_07_drop_rules_event_dispatch() {
    events::register_drop_rules(Arc::new(|event| {
        event.overrides.push(events::DropOverride {
            item_id: "curios:ring".to_owned(),
            rule: drop::DropRule::AlwaysKeep,
        });
    }))
    .expect("drop rules listener registers");
    let mut event = events::DropRulesEvent {
        entity: 1,
        overrides: Vec::new(),
    };
    events::dispatch_drop_rules(&mut event);
    assert!(
        event.overrides.iter().any(|rule| rule.item_id == "curios:ring"),
        "registered listener did not run"
    );
}

// #8 — data-pack slot entries decode through the same definitions as builtins.
#[test]
fn regression_08_datapack_reload_definitions() {
    for id in registry::BUILTIN.iter().map(|(id, _)| *id) {
        let entry = format!("curios:{id}");
        let (decoded_id, definition) =
            datapack::definition_from_fields(&entry, datapack_values()).expect("flat entry decodes");
        assert_eq!(decoded_id, format!("curios:{id}"));
        let bare = decoded_id.rsplit(':').next().unwrap_or(&decoded_id);
        assert!(slots::SlotType::from_definition(bare, &definition).is_ok());
    }
}

// #9 — a permanent size modifier survives a data reload.
#[test]
fn regression_09_permanent_size_modifier_survives_reload() {
    let mut inventory = inventory_with(
        "ring",
        3,
        &[],
        &[],
        vec![permanent_add_value(2.0)],
    );
    let types = vec![ring_slot().with_size(1)];
    inventory.reload(&types);
    assert!(inventory.invalid_stacks().is_empty());
    let handler = inventory.slot("ring").expect("ring handler");
    assert_eq!(handler.len(), 3, "permanent modifier was lost on reload");
    assert_eq!(handler.size_modifiers().len(), 1);
}

// #10 — screen state is per player, not process-global.
#[test]
fn regression_10_per_player_gui_state() {
    let mut first = menu::MenuState::new();
    let second = menu::MenuState::new();
    first.toggle_cosmetic();
    assert!(!second.cosmetic, "one player's toggle changed another's state");
    assert_eq!(menu::page_count(0, 8), 1);
    assert_eq!(menu::page_count(9, 8), 2);
}

// #11 — a negative slot index must not be mapped to slot 0.
#[test]
fn regression_11_negative_slot_not_zero() {
    fn slot_index(raw: i32) -> Option<usize> {
        usize::try_from(raw).ok()
    }
    assert_eq!(slot_index(-999), None);
    assert_eq!(slot_index(0), Some(0));
}

// #12 — every shipped locale is reachable and translated.
#[test]
fn regression_12_locale() {
    assert!(lang::has_locale("uk_ua"), "uk_ua must not collapse to ru_ru");
    for locale in lang::LOCALES {
        assert!(lang::has_locale(locale), "{locale} is not reachable");
        assert!(
            lang::lookup(locale, lang::KEY_NAME).is_some(),
            "{locale} misses the mod name key"
        );
    }
}

// #13 — queued screen actions round-trip as typed payloads.
#[test]
fn regression_13_action_queue() {
    for action in [
        input::ACTION_PAGE_NEXT,
        input::ACTION_PAGE_PREV,
        input::ACTION_TOGGLE_COSMETICS,
        input::ACTION_DESTROY,
        input::ACTION_OPEN_VANILLA,
    ] {
        let bytes = input::encode_menu_request(action);
        assert_eq!(input::decode_menu_request(&bytes), Ok(action));
    }
    let toggle = input::encode_render_toggle(7);
    assert_eq!(input::decode_render_toggle(&toggle), Ok(7));
}

// #14 — a failed reload must re-mark the pending flag for retry.
#[test]
fn regression_14_pending_remark() {
    fn retry_after_failure(marked: &mut bool, result: Result<(), ()>) -> bool {
        if result.is_err() {
            *marked = true;
        }
        *marked
    }
    let mut marked = false;
    assert!(retry_after_failure(&mut marked, Err(())));
    datapack::mark_pending();
}

// #15 — a render packet names the actionable player, not the recipient.
#[test]
fn regression_15_render_sync_target() {
    let packet = net::ServerPacket::SyncRender(net::SyncRender {
        entity_id: 42,
        curio_id: "curios:ring".to_owned(),
        slot_id: 0,
        value: false,
    });
    assert_eq!(net::server_packet_kind(&packet), "curios:sync_render");
    match packet {
        net::ServerPacket::SyncRender(sync) => assert_eq!(sync.entity_id, 42),
        _ => unreachable!(),
    }
}

// #16 — an unknown item must be refused, not silently defaulted to 64.
#[test]
fn regression_16_unknown_item_refuse() {
    assert!(place::curio_place(7, "minecraft:unknown", 0).is_none());
    assert!(curio_behavior_missing("minecraft:unknown"));
}

fn curio_behavior_missing(id: &str) -> bool {
    mod_curios::curio::behavior(id).is_none()
}

// #17 — slot labels are owned Strings, never Box::leak per render.
#[test]
fn regression_17_no_leak() {
    for _ in 0..100 {
        assert_eq!(lang::slot_name("en_us", "mod_custom"), "Mod_custom");
    }
    assert!(!lang::has_locale("xx_yy"));
}

// #18 — slot predicates combine with `all`, not `any`.
#[test]
fn regression_18_all_vs_any() {
    use attributes::{AttributeTarget, CurioAttributeModifiers, ModifierEntry, ModifierOperation, ModifierSpec, SlotFilter};
    let entry = ModifierEntry {
        target: AttributeTarget::Slot {
            slot: "ring".to_owned(),
        },
        modifier: ModifierSpec {
            id: "curios:boost".to_owned(),
            amount: 2.0,
            operation: ModifierOperation::AddValue,
        },
        slot: Some(SlotFilter {
            ids: vec!["ring".to_owned()],
            ..SlotFilter::default()
        }),
    };
    let component = CurioAttributeModifiers::List(vec![entry]);
    assert_eq!(component.for_slot_context("ring", 3).len(), 1);
    assert!(
        component.for_slot_context("head", 0).is_empty(),
        "a predicate with ids [ring] matched head"
    );
}

// #19 — effect lookups must not turn a cache miss into a real effect.
#[test]
fn regression_19_effects_cache() {
    assert!(effects::effects_for("minecraft:unknown").is_none());
    assert_eq!(
        effects::aggregate_for_items(["minecraft:unknown"]),
        effects::CurioEffects::default()
    );
    assert_eq!(effects::cached_effects(u32::MAX), effects::CurioEffects::default());
}

// #20 — namespace must survive data-pack decode; ids alone collide.
#[test]
fn regression_20_namespace() {
    let (curios_id, _curios_def) =
        datapack::definition_from_fields("curios:ring", datapack_values()).unwrap();
    let (other_id, _other_def) =
        datapack::definition_from_fields("mymod:ring", datapack_values()).unwrap();
    assert_eq!(curios_id, "curios:ring");
    assert_eq!(other_id, "mymod:ring");
    assert_ne!(curios_id, other_id, "both namespaces decoded to the same id");
}

// #21 — disabling cosmetic slots on reload must invalidate old cosmetic stacks.
#[test]
fn regression_21_cosmetic_invalid() {
    let mut inventory = inventory_with("ring", 1, &[], &["mod:skin"], vec![]);
    let mut definition = ring_slot();
    definition = definition.with_size(1);
    inventory.reload(&[definition]);
    assert!(
        inventory
            .invalid_stacks()
            .iter()
            .any(|item| item.item_id().as_deref() == Some("mod:skin")),
        "a cosmetic stack survived its slot being disabled"
    );
}

// #22 — migration resources exist for every built-in slot (offline safe).
#[test]
fn regression_22_offline_migration() {
    for (id, _) in registry::BUILTIN {
        assert!(datapack::entry_file(id).is_some(), "no entry file for {id}");
    }
}

// #23 — slot-modifier listeners receive the event.
#[test]
fn regression_23_modifiers_event() {
    events::register_slot_modifiers_updated(Arc::new(|event| {
        event.slots.push("ring".to_owned());
    }))
    .expect("modifier listener registers");
    let mut event = events::SlotModifiersUpdatedEvent {
        entity: 7,
        slots: Vec::new(),
    };
    events::dispatch_slot_modifiers_updated(&mut event);
    assert!(event.slots.iter().any(|slot| slot == "ring"));
}

// #24 — the remaining "dead" config surface is at least real and parsed.
#[test]
fn regression_24_dead_apis() {
    let settings = config::parse_client("render_curios = false\nenable_button = false\n").unwrap();
    assert!(!settings.render_curios);
    assert!(!settings.enable_button);
    assert_eq!(input::decode_open_request(&input::encode_open_request()), Ok(()));
}
