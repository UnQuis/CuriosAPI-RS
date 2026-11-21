//! The `/curios` commands.
//!
//! Registration and dispatch are deliberately separate from `Hooks`: `lib.rs`
//! only forwards the two events here. Every mutation is server-authoritative;
//! a command never grants an item or changes a slot from a client request.
//!
//! The six gameplay commands (`list`, `set`, `add`, `remove`, `clear`, `replace`)
//! mirror `CuriosCommand` of TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright
//! 2018-2024 C4; see COPYING and COPYING.LESSER). `slots`, `reload`, `open` and
//! `help` are port-specific additions kept as convenient aliases and diagnostics.
//!
//! Slot resizing uses the handler's persistent `SlotSizeModifier` state plus
//! `resize_to`, so a command-set count survives a data reload. Data-pack reload
//! reads the declared `curios:slot_type` registry instead of the bundled defaults.

use mod_api::{
    Command, CommandArgumentValue, CommandInvocation, CommandSource, Flow, Holder,
    HostTransaction, ItemResource, PlayerRef, RegisterCommandsEvent, RunModCommandEvent,
    ServerHost, Text,
};

use crate::attributes::{ModifierOperation, SlotSizeModifier};
use crate::events::{self, SlotModifiersUpdatedEvent};
use crate::inventory::{self, INVENTORY};
use crate::net;
use crate::place;
use crate::registry;
use crate::slots::SlotType;

/// Largest slot count a command may request. Java accepts any integer; this
/// bound keeps a malformed command from allocating an unbounded inventory.
pub const MAX_SLOTS: usize = 64;

/// Every command specification, with whether it needs permission level 2.
/// Registration and the validation test both read this table.
pub const COMMAND_SPECS: [(&str, bool); 10] = [
    ("curios list [player:player]", false),
    ("curios set <slot:word> <player:player> [amount:integer]", true),
    ("curios add <slot:word> <player:player> [amount:integer]", true),
    ("curios remove <slot:word> <player:player> [amount:integer]", true),
    ("curios clear [player:player] [slot:word]", true),
    (
        "curios replace <slot:word> <index:integer> <player:player> with <item:item> [count:integer]",
        true,
    ),
    ("curios slots", false),
    ("curios reload", true),
    ("curios open", false),
    ("curios help", false),
];

/// Declares every command. Mutating subcommands require permission level 2,
/// matching the Java command's gamemaster requirement.
pub fn register(event: &mut RegisterCommandsEvent<'_>) {
    for (spec, needs_op) in COMMAND_SPECS {
        if let Err(error) = validate_command_spec(spec) {
            eprintln!("[curios] refusing invalid command spec `{spec}`: {error}");
            continue;
        }
        if needs_op {
            event.commands.push(Command::op(spec, 2));
        } else {
            event.commands.push(Command::new(spec));
        }
    }
}

/// Validates one argument kind against Horizon 0.2's closed kind list.
fn valid_argument_kind(kind: &str) -> bool {
    matches!(
        kind,
        "word"
            | "text"
            | "integer"
            | "number"
            | "boolean"
            | "player"
            | "item"
            | "entity"
            | "entities"
            | "players"
            | "block_pos"
            | "mod_id"
            | "custom"
    ) || kind.starts_with("resource:")
}

/// Checks the `<name:kind>` / `[name:kind]` tokens of a registration string.
fn validate_command_spec(spec: &str) -> Result<(), String> {
    for token in spec.split_whitespace() {
        let inner = if let Some(inner) = token.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
            inner
        } else if let Some(inner) = token.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            inner
        } else {
            continue;
        };
        let (name, kind) = inner
            .split_once(':')
            .ok_or_else(|| format!("argument {inner} in `{spec}` has no kind"))?;
        if name.is_empty() || kind.is_empty() {
            return Err(format!("empty argument name or kind in `{spec}`"));
        }
        if !valid_argument_kind(kind) {
            return Err(format!("unknown argument kind `{kind}` in `{spec}`"));
        }
    }
    Ok(())
}

/// Returns whether this invocation belongs to `/curios`.
pub fn is_ours(event: &RunModCommandEvent<'_>) -> bool {
    event
        .command
        .literals
        .first()
        .map(String::as_str)
        == Some("curios")
}

/// Dispatches one `/curios` invocation. `Continue` leaves foreign commands alone.
pub fn run(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Flow {
    if !is_ours(event) {
        return Flow::Continue;
    }
    let sub = event.command.literals.get(1).map(String::as_str).unwrap_or("help");
    let result = match sub {
        "list" => list(host, event),
        "set" => resize(host, event, SizeChange::Set),
        "add" => resize(host, event, SizeChange::Add),
        "remove" => resize(host, event, SizeChange::Remove),
        "clear" => clear(host, event),
        "replace" => replace(host, event),
        "slots" => {
            for line in slot_lines() {
                feedback(host, line);
            }
            Ok(())
        }
        "reload" => reload(host),
        "open" => open(host, event),
        "help" | _ => {
            for line in help_lines() {
                feedback(host, line);
            }
            Ok(())
        }
    };
    if let Err(error) = result {
        feedback(host, Text::literal(format!("§cCurios: {error}")));
    }
    Flow::Cancel
}

fn feedback(host: &mut dyn ServerHost, line: Text) {
    if let Err(error) = host.commands().feedback(line) {
        eprintln!("[curios] command feedback failed: {error:?}");
    }
}

// ---------------------------------------------------------------------------
// Argument parsing (ProjectE-RS `command_tokens` pattern, with typed values).
// ---------------------------------------------------------------------------

fn tokens(invocation: &CommandInvocation, host: &dyn mod_api::Host) -> Vec<String> {
    let mut out = invocation.literals.clone();
    for arg in &invocation.arguments {
        let value = match &arg.value {
            CommandArgumentValue::String(value) => Some(value.clone()),
            CommandArgumentValue::Integer(value) => Some(value.to_string()),
            CommandArgumentValue::Number(value) => Some(value.to_string()),
            CommandArgumentValue::Boolean(value) => Some(value.to_string()),
            CommandArgumentValue::Player(player) => player.name(host),
            _ => None,
        };
        if let Some(value) = value {
            out.push(value);
        }
    }
    out
}

fn argument(invocation: &CommandInvocation, index: usize) -> Option<&CommandArgumentValue> {
    invocation.arguments.get(index).map(|arg| &arg.value)
}

fn int_arg(invocation: &CommandInvocation, index: usize, default: i32) -> Result<i32, String> {
    match argument(invocation, index) {
        None => Ok(default),
        Some(CommandArgumentValue::Integer(value)) => Ok(*value),
        Some(other) => Err(format!("expected an integer, got {other:?}")),
    }
}

fn player_arg(invocation: &CommandInvocation) -> Option<PlayerRef> {
    invocation.arguments.iter().find_map(|arg| match &arg.value {
        CommandArgumentValue::Player(player) => Some(*player),
        _ => None,
    })
}

fn slot_arg(invocation: &CommandInvocation, index: usize) -> Result<String, String> {
    match argument(invocation, index) {
        Some(CommandArgumentValue::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(_) => Err("the slot id is empty".to_owned()),
        None => Err("a slot id is required".to_owned()),
    }
}

/// Validates an amount against the command's explicit bound. Negative and
/// oversized requests are refused with a named reason before any mutation.
fn amount_value(raw: i32) -> Result<usize, String> {
    if raw < 0 {
        return Err(format!("amount {raw} is negative"));
    }
    usize::try_from(raw)
        .ok()
        .filter(|value| *value <= MAX_SLOTS)
        .ok_or_else(|| format!("amount {raw} exceeds the maximum of {MAX_SLOTS}"))
}

/// Validates an item count: strictly positive and bounded like a slot amount.
fn count_value(raw: i32) -> Result<u32, String> {
    if raw <= 0 {
        return Err(format!("count {raw} must be positive"));
    }
    u32::try_from(raw)
        .ok()
        .filter(|value| *value <= MAX_SLOTS as u32)
        .ok_or_else(|| format!("count {raw} exceeds the maximum of {MAX_SLOTS}"))
}

/// Validates a slot index, both as a number and as a place key component.
fn index_value(raw: i32) -> Result<u16, String> {
    if raw < 0 {
        return Err(format!("slot index {raw} is negative"));
    }
    u16::try_from(raw).map_err(|_| format!("slot index {raw} is out of range"))
}

fn player_by_name(host: &dyn ServerHost, name: &str) -> Option<PlayerRef> {
    host.players()
        .server_players()
        .into_iter()
        .find(|player| player.name(host).is_some_and(|found| found == name))
}

/// Resolves the target from the typed player argument, falling back to the
/// literal-name token so the `/curios` syntax stays close to Java's.
fn target_player(
    host: &mut dyn ServerHost,
    invocation: &CommandInvocation,
    name: Option<&str>,
) -> Result<PlayerRef, String> {
    if let Some(player) = player_arg(invocation) {
        return Ok(player);
    }
    let name = name.ok_or_else(|| "a player is required".to_owned())?;
    player_by_name(host, name).ok_or_else(|| format!("player {name} is not online"))
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

fn list(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Result<(), String> {
    let words = tokens(event.command, &*host);
    let name = words.get(2).map(String::as_str);
    if name.is_none() {
        for line in slot_lines() {
            feedback(host, line);
        }
        return Ok(());
    }
    let player = target_player(host, event.command, name)?;
    let holder = inventory::holder_for_player(player.uuid());
    let inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
    feedback(host, Text::literal("§6Curios slots§r"));
    for id in inventory.slot_ids() {
        let handler = inventory.slot(id).ok_or_else(|| format!("slot {id} vanished"))?;
        let filled = handler.filled().count();
        feedback(host, Text::literal(format!(
            "§e{id}§7 — {} slot(s), {} filled",
            handler.len(),
            filled
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeChange {
    Set,
    Add,
    Remove,
}

fn resize(
    host: &mut dyn ServerHost,
    event: &RunModCommandEvent<'_>,
    change: SizeChange,
) -> Result<(), String> {
    let words = tokens(event.command, &*host);
    let slot = slot_arg(event.command, 0)?;
    let player = target_player(host, event.command, words.get(3).map(String::as_str))?;
    let amount = amount_value(int_arg(event.command, 2, 1)?)?;
    let uuid = player.uuid();
    let holder = inventory::holder_for_player(player.uuid());
    let inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
    let base = inventory
        .slot(&slot)
        .ok_or_else(|| format!("unknown slot type {slot}"))?
        .len();
    let target = match change {
        SizeChange::Set => amount,
        SizeChange::Add => base.saturating_add(amount),
        SizeChange::Remove => base.saturating_sub(amount),
    };
    let (old, new) = resize_slot(host, uuid, &holder, &slot, target)?;
    feedback(host, Text::literal(format!(
        "§aCurios: §e{slot}§a resized {old} → {new} for the target player."
    )));
    Ok(())
}

/// The permanent `ADD_VALUE` modifier a command leaves on the slot handler.
/// Its amount is the difference from the slot definition's base, so a data
/// reload recomputes the same count instead of reverting it.
fn command_modifier(current: usize, target: usize) -> (SlotSizeModifier, bool) {
    (
        SlotSizeModifier {
            operation: ModifierOperation::AddValue,
            amount: target as f64 - current as f64,
        },
        true, // permanent
    )
}

/// Adds the command's delta without removing or rewriting any existing
/// modifier. Other mods' permanent `ADD_VALUE` modifiers are therefore kept,
/// and the computed size stays `current + delta == target`.
fn with_command_modifier(
    existing: &[(SlotSizeModifier, bool)],
    current: usize,
    target: usize,
) -> Vec<(SlotSizeModifier, bool)> {
    let mut modifiers = existing.to_vec();
    modifiers.push(command_modifier(current, target));
    modifiers
}

/// Resizes one handler through its persistent modifiers, not by editing NBT
/// lists. The modifier survives `INVENTORY.reload`; stacks that no longer fit
/// are dropped immediately, server-side.
fn resize_slot(
    host: &mut dyn ServerHost,
    uuid: u128,
    holder: &Holder,
    slot_id: &str,
    target: usize,
) -> Result<(usize, usize), String> {
    if target > MAX_SLOTS {
        return Err(format!("slot count must be <= {MAX_SLOTS}"));
    }
    let entity_id = match *holder {
        Holder::Entity(id) => id,
        Holder::Player(owner) => host
            .player(owner)
            .ok_or_else(|| format!("player {owner} is not online"))?
            .entity()
            .id(),
        _ => return Err("curio slots are only held by entities or players".to_owned()),
    };
    let mut inventory = INVENTORY.get(host, *holder).map_err(|error| format!("{error:?}"))?;
    let old = inventory
        .slot(slot_id)
        .ok_or_else(|| format!("unknown slot type {slot_id}"))?
        .len();
    let handler = inventory
        .slot_mut(slot_id)
        .ok_or_else(|| format!("unknown slot type {slot_id}"))?;
    // Append a delta rather than deleting every permanent ADD_VALUE: a different
    // mod's modifier must survive the command unchanged.
    let modifiers = with_command_modifier(handler.size_modifiers(), old, target);
    handler.set_size_modifiers(modifiers);
    let invalid = handler.resize_to(target);

    if !invalid.is_empty() {
        let living = host.entity(entity_id).and_then(|entity| host.living(entity));
        let Some(living) = living else {
            return Err(format!("entity {entity_id} cannot drop overflow stacks"));
        };
        for snapshot in invalid {
            match snapshot.to_stack(host) {
                Ok(stack) => {
                    if let Err(error) = living.drop(host, stack, true, false) {
                        eprintln!("[curios] could not drop an overflow stack: {error:?}");
                    }
                }
                Err(error) => eprintln!("[curios] overflow stack could not be decoded: {error:?}"),
            }
        }
    }

    INVENTORY
        .set(host, *holder, inventory.clone())
        .map_err(|error| format!("{error:?}"))?;
    if let Err(error) = net::sync_inventory(host, uuid, &inventory) {
        eprintln!("[curios] could not resync {uuid} after resize: {error:?}");
    }
    let mut event = SlotModifiersUpdatedEvent {
        entity: entity_id,
        slots: vec![slot_id.to_owned()],
    };
    events::dispatch_slot_modifiers_updated(&mut event);
    Ok((old, target))
}

fn clear(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Result<(), String> {
    let words = tokens(event.command, &*host);
    let player = target_player(host, event.command, words.get(2).map(String::as_str))?;
    let filter = words.get(3).map(String::as_str);
    let uuid = player.uuid();
    let holder = inventory::holder_for_player(player.uuid());
    let mut inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
    let ids: Vec<String> = match filter {
        Some(slot) => {
            if inventory.slot(slot).is_none() {
                return Err(format!("unknown slot type {slot}"));
            }
            vec![slot.to_owned()]
        }
        None => inventory.slot_ids().map(str::to_owned).collect(),
    };
    let mut cleared = 0usize;
    for id in ids {
        let handler = inventory
            .slot_mut(&id)
            .ok_or_else(|| format!("slot {id} vanished"))?;
        for index in 0..handler.len() {
            if handler.stack(index).and_then(Option::as_ref).is_some() {
                cleared += 1;
            }
            handler
                .set_stack(index, None)
                .map_err(|error| format!("{error:?}"))?;
        }
        let cosmetic_len = {
            let mut len = 0;
            while handler.cosmetic(len).is_some() {
                len += 1;
            }
            len
        };
        for index in 0..cosmetic_len {
            handler
                .set_cosmetic(index, None)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    INVENTORY
        .set(host, holder, inventory.clone())
        .map_err(|error| format!("{error:?}"))?;
    if let Err(error) = net::sync_inventory(host, uuid, &inventory) {
        eprintln!("[curios] could not resync {uuid} after clear: {error:?}");
    }
    feedback(host, Text::literal(format!("§aCurios: cleared §e{cleared}§a slot(s).")));
    Ok(())
}

fn replace(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Result<(), String> {
    let slot = slot_arg(event.command, 0)?;
    let index = index_value(int_arg(event.command, 1, -1)?)?;
    let player = target_player(host, event.command, None)?;
    let count = count_value(int_arg(event.command, 4, 1)?)?;
    let stack = match argument(event.command, 3) {
        Some(CommandArgumentValue::Item(stack)) => stack.clone(),
        _ => return Err("an item argument is required after `with`".to_owned()),
    };
    let uuid = player.uuid();
    let entity = player.entity().id();

    // The mod place validates the slot type and routes the write through the
    // registered PlaceProvider, i.e. through the same transaction a GUI click uses.
    let place = place::curio_place(entity, &slot, index)
        .ok_or_else(|| format!("unknown slot type or index {slot}:{index}"))?;
    let resource = ItemResource::of(&stack);
    let moved = host.transaction(|host, tx| {
        Ok::<u32, String>(place.exchange(host, tx, &resource, count))
    })?;
    if moved != count {
        return Err(format!("the slot refused the exchange ({moved}/{count} moved)"));
    }

    let holder = inventory::holder_for_player(uuid);
    let inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
    let item = inventory
        .slot(&slot)
        .and_then(|handler| handler.stack(usize::from(index)))
        .and_then(Option::as_ref);
    if let Err(error) = net::sync_stack(
        host,
        uuid,
        &slot,
        i32::from(index),
        item,
        net::HANDLER_EQUIPMENT,
        &net::NetTag::Compound(Vec::new()),
    ) {
        eprintln!("[curios] could not sync the replaced stack: {error:?}");
    }
    feedback(
        host,
        Text::literal(format!("§aCurios: replaced §e{slot} {index}§a with {count} item(s).")),
    );
    Ok(())
}

fn slot_lines() -> Vec<Text> {
    let types: Vec<SlotType> = registry::slot_types().iter().cloned().collect();
    let mut lines = vec![Text::literal(format!("§6Curios slot types§r (§e{}§r)", types.len()))];
    for slot in &types {
        lines.push(Text::literal(format!(
            "§e{}§7 — order {} · size {} · drop {} · cosmetic {}",
            slot.id(),
            slot.order(),
            slot.size(),
            slot.drop_rule().as_str(),
            slot.has_cosmetic()
        )));
    }
    lines
}

fn help_lines() -> Vec<Text> {
    vec![
        Text::literal("§6Curios commands§r"),
        Text::literal("§e/curios list [player]§7 — list slot types or a player's slots"),
        Text::literal("§e/curios set <slot> <player> [amount]§7 — set the slot count"),
        Text::literal("§e/curios add <slot> <player> [amount]§7 — grow a slot count"),
        Text::literal("§e/curios remove <slot> <player> [amount]§7 — shrink a slot count"),
        Text::literal("§e/curios clear [player] [slot]§7 — empty slots without dropping"),
        Text::literal("§e/curios replace <slot> <index> <player> with <item> [count]§7"),
        Text::literal("§e/curios slots§7 — built-in definitions; §e/curios reload§7 — rebuild"),
        Text::literal("§e/curios open§7 — send the screen state; §e/curios help§7 — this list"),
    ]
}

/// Chooses the slot definitions for a manual reload. The declared datapack
/// registry wins; bundled definitions are only a diagnostic fallback when the
/// registry cannot be read (for example before the first level is available).
fn reload_types(datapack: Result<Vec<SlotType>, String>, fallback: Result<Vec<SlotType>, String>) -> Result<Vec<SlotType>, String> {
    match datapack {
        Ok(types) => Ok(types),
        Err(error) => {
            eprintln!("[curios] datapack slot types unavailable ({error}); using bundled defaults");
            fallback
        }
    }
}

/// Rebuilds the slot registry from the loaded datapack registry, migrates and
/// resynchronises every online player, and drops stacks that no longer fit.
pub fn reload(host: &mut dyn ServerHost) -> Result<(), String> {
    let types = reload_types(
        crate::datapack::read_slot_types(host, crate::datapack::DEFAULT_DIMENSION)
            .map_err(|error| format!("{error:?}")),
        registry::builtin_slot_types().map_err(|error| format!("{error}")),
    )?;
    let count = types.len();
    registry::replace_slot_types(types.clone()).map_err(|error| format!("{error}"))?;

    let players: Vec<_> = host.players().server_players().into_iter().collect();
    for player in players {
        let uuid = player.uuid();
        let holder = inventory::holder_for_player(uuid);
        let mut inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
        // `reload` adopts stacks that no longer fit into `inventory.invalid`;
        // the ordinary server tick drops them from there instead of losing them.
        inventory.reload(&types);
        INVENTORY
            .set(host, holder, inventory.clone())
            .map_err(|error| format!("{error:?}"))?;
        if let Err(error) = net::sync_inventory(host, uuid, &inventory) {
            eprintln!("[curios] could not resync {uuid} after reload: {error:?}");
        }
        if let Err(error) = net::sync_data(host, uuid, &types) {
            eprintln!("[curios] could not resync slot data to {uuid}: {error:?}");
        }
    }
    feedback(
        host,
        Text::literal(format!("§aCurios: reloaded §e{count}§a slot types from data packs.")),
    );
    Ok(())
}

/// Sends the client the data needed to draw the Curios screen. The pinned
/// Horizon API has no mod menu with real slots, so only the snapshot and page
/// are sent; the window itself stays client-side.
pub fn open(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Result<(), String> {
    let CommandSource::Player { player, .. } = *event.source else {
        return Err("this command needs a player".to_owned());
    };
    let uuid = player.uuid();
    let types = registry::slot_types();
    net::sync_data(host, uuid, &types).map_err(|error| format!("{error:?}"))?;
    net::server_page(host, uuid, 0, 0).map_err(|error| format!("{error:?}"))?;
    feedback(
        host,
        Text::literal("§7Curios: screen state sent to the client."),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::SlotSizeState;

    #[test]
    fn every_registered_command_uses_a_valid_argument_kind() {
        assert_eq!(COMMAND_SPECS.len(), 10);
        for (spec, _) in COMMAND_SPECS {
            validate_command_spec(spec)
                .unwrap_or_else(|error| panic!("invalid command spec `{spec}`: {error}"));
            assert!(!spec.contains("slot:string"), " Horizon rejects `string` kinds: {spec}");
        }
        assert!(validate_command_spec("curios set <slot:wat> <player:player>").is_err());
        assert!(validate_command_spec("curios set <slot>").is_err());
        assert!(validate_command_spec("curios value <id:resource:curios:slot_type>").is_ok());
    }

    #[test]
    fn player_commands_select_the_player_holder() {
        let uuid = 42u128;
        assert_eq!(inventory::holder_for_player(uuid), Holder::Player(uuid));
        assert_ne!(inventory::holder_for_player(uuid), Holder::Entity(42));
    }

    #[test]
    fn help_lists_every_java_subcommand() {
        let help = help_lines();
        assert!(help.len() >= 9);
        let joined = help.iter().map(|text| format!("{text:?}")).collect::<String>();
        for name in ["list", "set", "add", "remove", "clear", "replace"] {
            assert!(joined.contains(name), "help omits {name}");
        }
    }

    #[test]
    fn a_command_modifier_survives_reload_and_recomputes_the_size() {
        let state = SlotSizeState {
            base: 1,
            modifiers: vec![command_modifier(1, 5)],
        };
        assert_eq!(state.resize().size, 5);
        assert!(state.resize().changed);

        // The modifier is part of the strict attachment codec, so a serialized
        // and decoded inventory still computes the same size.
        let types = registry::slot_types();
        let mut inventory = crate::inventory::CurioInventory::from_slot_types(&types);
        inventory
            .slot_mut("ring")
            .expect("ring slot exists")
            .set_size_modifiers(vec![command_modifier(1, 5)]);
        let codec = INVENTORY.codec.as_ref().expect("inventory has a codec");
        let encoded = (codec.encode)(&inventory).expect("encoding succeeds");
        let decoded = (codec.decode)(&encoded).expect("decoding succeeds");
        assert_eq!(
            decoded.slot("ring").expect("ring survived").size_state().resize().size,
            5
        );
    }

    #[test]
    fn a_command_resize_keeps_other_mods_permanent_modifiers() {
        let foreign = (
            SlotSizeModifier {
                operation: ModifierOperation::AddValue,
                amount: 2.0,
            },
            true,
        );
        let other = (
            SlotSizeModifier {
                operation: ModifierOperation::AddMultipliedBase,
                amount: 0.0,
            },
            true,
        );
        let existing = vec![foreign, other];
        let updated = with_command_modifier(&existing, 3, 6);
        assert_eq!(&updated[..2], &existing[..], "a foreign modifier was rewritten");
        assert_eq!(updated.len(), 3);
        let state = SlotSizeState {
            base: 1,
            modifiers: updated,
        };
        assert_eq!(state.resize().size, 6);
    }

    #[test]
    fn reload_prefers_datapack_slot_types_with_a_documented_fallback() {
        let datapack = vec![SlotType::new("custom").unwrap()];
        let bundled = vec![SlotType::new("ring").unwrap()];
        let chosen = reload_types(Ok(datapack.clone()), Ok(bundled.clone())).unwrap();
        assert_eq!(chosen[0].id(), "custom");
        let fallback = reload_types(Err("no level".to_owned()), Ok(bundled.clone())).unwrap();
        assert_eq!(fallback[0].id(), "ring");
        assert!(reload_types(Err("no level".to_owned()), Err("bad json".to_owned())).is_err());
    }

    #[test]
    fn sized_slot_definitions_still_decode() {
        let types = registry::slot_types();
        assert!(types.iter().any(|slot| slot.id() == "ring"));
        assert!(slot_lines().len() > 1);
    }

    #[test]
    fn invalid_amounts_counts_and_indices_are_explicitly_refused() {
        assert_eq!(amount_value(3), Ok(3));
        assert_eq!(amount_value(0), Ok(0));
        assert!(amount_value(-1).unwrap_err().contains("negative"));
        assert!(amount_value(MAX_SLOTS as i32 + 1)
            .unwrap_err()
            .contains("maximum"));
        assert!(amount_value(i32::MAX).is_err());

        assert_eq!(count_value(2), Ok(2));
        assert!(count_value(0).unwrap_err().contains("positive"));
        assert!(count_value(-3).unwrap_err().contains("positive"));
        assert!(count_value(i32::MAX).is_err());

        assert_eq!(index_value(4), Ok(4));
        assert!(index_value(-1).unwrap_err().contains("negative"));
        assert!(index_value(i32::MAX).unwrap_err().contains("range"));
    }

    #[test]
    fn empty_slot_ids_are_refused() {
        let mut invocation = CommandInvocation::new(
            vec!["curios".to_owned(), "set".to_owned()],
            Vec::new(),
            mod_api::DimensionKey::OVERWORLD,
            [0.0; 3],
        );
        assert!(slot_arg(&invocation, 0).unwrap_err().contains("required"));
        invocation.arguments.push(mod_api::CommandArgument::new(
            "slot".to_owned(),
            CommandArgumentValue::String(String::new()),
        ));
        assert!(slot_arg(&invocation, 0).unwrap_err().contains("empty"));
        invocation.arguments.clear();
        invocation.arguments.push(mod_api::CommandArgument::new(
            "slot".to_owned(),
            CommandArgumentValue::String("ring".to_owned()),
        ));
        assert_eq!(slot_arg(&invocation, 0), Ok("ring".to_owned()));
    }
}
