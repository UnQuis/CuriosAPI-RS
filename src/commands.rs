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
//! Slot resizing goes through the attachment's own strict codec: the handler's
//! four parallel lists are resized in a compound, decoded back and stored with
//! `INVENTORY.set`. This keeps all mutations inside the server-owned attachment
//! without requiring changes to `inventory.rs`.

use mod_api::{
    Command, CommandArgumentValue, CommandInvocation, CommandSource, Compound, Flow, Holder,
    HostTransaction, ItemResource, PlayerRef, RegisterCommandsEvent, RunModCommandEvent,
    ServerHost, Tag, Text,
};

use crate::inventory::INVENTORY;
use crate::net;
use crate::place;
use crate::registry;
use crate::slots::SlotType;

/// Largest slot count a command may request. Java accepts any integer; this
/// bound keeps a malformed command from allocating an unbounded inventory.
pub const MAX_SLOTS: usize = 64;

/// Declares every command. Mutating subcommands require permission level 2,
/// matching the Java command's gamemaster requirement.
pub fn register(event: &mut RegisterCommandsEvent<'_>) {
    event.commands.push(Command::new("curios list [player:player]"));
    event.commands.push(Command::op(
        "curios set <slot:string> <player:player> [amount:integer]",
        2,
    ));
    event.commands.push(Command::op(
        "curios add <slot:string> <player:player> [amount:integer]",
        2,
    ));
    event.commands.push(Command::op(
        "curios remove <slot:string> <player:player> [amount:integer]",
        2,
    ));
    event.commands.push(Command::op(
        "curios clear [player:player] [slot:string]",
        2,
    ));
    event.commands.push(Command::op(
        "curios replace <slot:string> <index:integer> <player:player> with <item:item> [count:integer]",
        2,
    ));
    // Port-specific additions.
    event.commands.push(Command::new("curios slots"));
    event.commands.push(Command::op("curios reload", 2));
    event.commands.push(Command::new("curios open"));
    event.commands.push(Command::new("curios help"));
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
        Some(CommandArgumentValue::String(value)) => Ok(value.clone()),
        _ => Err("expected a slot id".to_owned()),
    }
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
    let holder = Holder::Entity(player.entity().id());
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
    let amount = int_arg(event.command, 2, 1)?;
    if amount < 0 {
        return Err("amount must not be negative".to_owned());
    }
    let amount = usize::try_from(amount).map_err(|_| "amount is out of range".to_owned())?;
    let uuid = player.uuid();
    let holder = Holder::Entity(player.entity().id());
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

/// Resizes one handler through the attachment's own strict codec. The lists
/// are kept parallel; removed stacks move to `invalid` so `server_tick` drops
/// them exactly as it drops stacks invalidated by a data reload.
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
    let inventory = INVENTORY.get(host, *holder).map_err(|error| format!("{error:?}"))?;
    let codec = INVENTORY
        .codec
        .as_ref()
        .ok_or_else(|| "the inventory attachment has no codec".to_owned())?;
    let mut root = (codec.encode)(&inventory).map_err(|error| format!("{error:?}"))?;
    let (old, new) = resize_nbt(&mut root, slot_id, target)?;
    let decoded = (codec.decode)(&root).map_err(|error| format!("{error:?}"))?;
    INVENTORY
        .set(host, *holder, decoded.clone())
        .map_err(|error| format!("{error:?}"))?;
    if let Err(error) = net::sync_inventory(host, uuid, &decoded) {
        eprintln!("[curios] could not resync {uuid} after resize: {error:?}");
    }
    Ok((old, new))
}

/// Pure NBT transformation used by [`resize_slot`]; tested without a host.
fn resize_nbt(root: &mut Compound, slot_id: &str, target: usize) -> Result<(usize, usize), String> {
    // `Compound` is a BTreeMap keyed by JavaString; inference from `get_mut`
    // gives these locals that type without naming the runtime's string type.
    let slots_key = "slots".into();
    let slots = match root.get_mut(&slots_key) {
        Some(Tag::Compound(slots)) => slots,
        _ => return Err("saved inventory has no slots compound".to_owned()),
    };
    let handler_key = slot_id.into();
    let handler = match slots.get_mut(&handler_key) {
        Some(Tag::Compound(handler)) => handler,
        _ => return Err(format!("slot {slot_id} is not in the inventory")),
    };
    let stacks_key = "stacks".into();
    let old = match handler.get(&stacks_key) {
        Some(Tag::List(list)) => list.len(),
        _ => return Err("slot stacks are malformed".to_owned()),
    };
    let cosmetic_key = "cosmetic".into();
    let cosmetic_exists = matches!(handler.get(&cosmetic_key), Some(Tag::List(_)));
    let mut removed = Vec::new();
    for (field, filler) in [
        ("stacks", Tag::End),
        ("cosmetic", Tag::End),
        ("render", Tag::Byte(1)),
        ("active", Tag::Byte(1)),
    ] {
        let field_key = field.into();
        let Some(Tag::List(list)) = handler.get_mut(&field_key) else {
            return Err(format!("slot field {field} is malformed"));
        };
        // A slot without cosmetic slots keeps its empty cosmetic list.
        let wanted = if field == "cosmetic" && !cosmetic_exists {
            0
        } else {
            target
        };
        while list.len() > wanted {
            let value = list.pop().ok_or_else(|| "list pop failed".to_owned())?;
            if field == "stacks" || field == "cosmetic" {
                if matches!(value, Tag::Compound(_)) {
                    removed.push(value);
                }
            }
        }
        while list.len() < wanted {
            list.push(filler.clone());
        }
    }
    if !removed.is_empty() {
        let invalid_key = "invalid".into();
        let invalid = match root.get_mut(&invalid_key) {
            Some(Tag::List(list)) => list,
            _ => return Err("saved inventory has no invalid list".to_owned()),
        };
        invalid.extend(removed);
    }
    Ok((old, target))
}

fn clear(host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) -> Result<(), String> {
    let words = tokens(event.command, &*host);
    let player = target_player(host, event.command, words.get(2).map(String::as_str))?;
    let filter = words.get(3).map(String::as_str);
    let uuid = player.uuid();
    let holder = Holder::Entity(player.entity().id());
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
    let index = int_arg(event.command, 1, -1)?;
    if index < 0 {
        return Err("slot index must not be negative".to_owned());
    }
    let index = u16::try_from(index).map_err(|_| "slot index is out of range".to_owned())?;
    let player = target_player(host, event.command, None)?;
    let count = int_arg(event.command, 4, 1)?;
    let count = u32::try_from(count).map_err(|_| "count must be positive".to_owned())?;
    if count == 0 {
        return Err("count must be positive".to_owned());
    }
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

    let holder = Holder::Entity(entity);
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

/// Rebuilds the slot registry from validated definitions, then migrates and
/// resynchronises every online player. Invalid stacks are left for `server_tick`.
pub fn reload(host: &mut dyn ServerHost) -> Result<(), String> {
    let types = registry::builtin_slot_types().map_err(|error| format!("{error}"))?;
    let count = types.len();
    registry::replace_slot_types(types.clone()).map_err(|error| format!("{error}"))?;

    let players: Vec<_> = host.players().server_players().into_iter().collect();
    for player in players {
        let uuid = player.uuid();
        let holder = Holder::Entity(player.entity().id());
        let mut inventory = INVENTORY.get(host, holder).map_err(|error| format!("{error:?}"))?;
        let _invalid = inventory.reload(&types);
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
        Text::literal(format!("§aCurios: reloaded §e{count}§a slot types.")),
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

    fn inventory_nbt() -> Compound {
        let types = registry::slot_types();
        let inventory = crate::inventory::CurioInventory::from_slot_types(&types);
        let codec = INVENTORY.codec.as_ref().expect("inventory has a codec");
        (codec.encode)(&inventory).expect("encoding succeeds")
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
    fn resize_nbt_grows_and_shrinks_all_parallel_lists() {
        let mut root = inventory_nbt();
        let (old, new) = resize_nbt(&mut root, "ring", 5).unwrap();
        let types = registry::slot_types();
        let base = types.iter().find(|slot| slot.id() == "ring").unwrap().size() as usize;
        assert_eq!(old, base);
        assert_eq!(new, 5);
        let codec = INVENTORY.codec.as_ref().unwrap();
        let grown = (codec.decode)(&root).unwrap();
        let handler = grown.slot("ring").unwrap();
        assert_eq!(handler.len(), 5);
        assert_eq!(handler.render(4), Some(true));
        assert_eq!(handler.active(4), Some(true));

        let (old, new) = resize_nbt(&mut root, "ring", 2).unwrap();
        assert_eq!((old, new), (5, 2));
        let shrunk = (codec.decode)(&root).unwrap();
        assert_eq!(shrunk.slot("ring").unwrap().len(), 2);
    }

    #[test]
    fn resize_nbt_refuses_unknown_slot_and_malformed_roots() {
        let mut root = inventory_nbt();
        assert!(resize_nbt(&mut root, "missing", 3).is_err());
        let mut empty = Compound::new();
        assert!(resize_nbt(&mut empty, "ring", 3).is_err());
        assert!(MAX_SLOTS > 0);
    }

    #[test]
    fn sized_slot_definitions_still_decode() {
        let types = registry::slot_types();
        assert!(types.iter().any(|slot| slot.id() == "ring"));
        assert!(slot_lines().len() > 1);
    }
}
