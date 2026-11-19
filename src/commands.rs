//! The `/curios` commands.
//!
//! Registration and dispatch are deliberately separate from `Hooks`: `lib.rs`
//! only forwards the two events here. Every mutation is server-authoritative;
//! a command never grants an item or changes a slot directly.
//!
//! The command surface mirrors the Java mod's `CuriosCommand`: listing the
//! registered slot types, rebuilding them after a data reload, opening the GUI
//! and printing help.

use mod_api::{Command, CommandSource, Flow, RegisterCommandsEvent, RunModCommandEvent, ServerHost, Text};

use crate::inventory::INVENTORY;
use crate::net;
use crate::registry;

/// Declares the four `/curios` commands.
pub fn register(event: &mut RegisterCommandsEvent<'_>) {
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
    match event.command.literals.get(1).map(String::as_str) {
        Some("slots") => {
            for line in slot_lines() {
                feedback(host, line);
            }
            Flow::Cancel
        }
        Some("reload") => match reload(host) {
            Ok(count) => {
                feedback(host, Text::literal(format!("§aCurios: reloaded §e{count}§a slot types.")));
                Flow::Cancel
            }
            Err(error) => {
                feedback(host, Text::literal(format!("§cCurios reload failed: {error}")));
                Flow::Cancel
            }
        },
        Some("open") => match open(host, event) {
            Ok(()) => Flow::Cancel,
            Err(error) => {
                feedback(host, Text::literal(format!("§cCurios open failed: {error}")));
                Flow::Cancel
            }
        },
        Some("help") | None => {
            for line in help_lines() {
                feedback(host, line);
            }
            Flow::Cancel
        }
        Some(other) => {
            feedback(host, Text::literal(format!("§cUnknown /curios subcommand: {other}")));
            Flow::Cancel
        }
    }
}

fn feedback(host: &mut dyn ServerHost, line: Text) {
    if let Err(error) = host.commands().feedback(line) {
        eprintln!("[curios] command feedback failed: {error:?}");
    }
}

fn slot_lines() -> Vec<Text> {
    let types = registry::slot_types();
    let mut lines = vec![Text::literal(format!("§6Curios slot types§r (§e{}§r)", types.len()))];
    for slot in types.iter() {
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
        Text::literal("§e/curios slots§7 — list registered slot types"),
        Text::literal("§e/curios reload§7 — rebuild slot types and inventories (OP)"),
        Text::literal("§e/curios open§7 — open the Curios screen (player only)"),
        Text::literal("§e/curios help§7 — this list"),
    ]
}

/// Rebuilds the slot registry from validated definitions, then migrates and
/// resynchronises every online player. Invalid stacks are left in the
/// inventory for `server_tick` to drop, exactly as on a data-pack reload.
pub fn reload(host: &mut dyn ServerHost) -> Result<usize, String> {
    let types = registry::builtin_slot_types().map_err(|error| format!("{error}"))?;
    let count = types.len();
    registry::replace_slot_types(types.clone()).map_err(|error| format!("{error}"))?;

    let players: Vec<_> = host.players().server_players().into_iter().collect();
    for player in players {
        let uuid = player.uuid();
        let entity = player.entity();
        let holder = mod_api::Holder::Entity(entity.id());
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
    Ok(count)
}

/// Sends the client the data it needs to draw the Curios screen.
///
/// The pinned Horizon API has no mod menu with real slots (see
/// `docs/research/horizon-api.md`, §6 and §9), so the server can only provide
/// the snapshot and page; the window itself is client-side. This limitation is
/// reported to the caller instead of pretending a menu was opened.
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

    #[test]
    fn help_and_slot_lines_are_nonempty_and_stable() {
        let help = help_lines();
        assert_eq!(help.len(), 5);
        assert!(slot_lines().len() >= 2);
    }
}
