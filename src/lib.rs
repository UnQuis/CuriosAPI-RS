//! CuriosAPI-RS: Curios accessory slots on the Horizon native mod API.
//!
//! Native mod against the generated `mod_api` (API 0.2). The crate root holds
//! the one `#[horizon_mod] impl Hooks`; item-side behavior lives in `curio`,
//! the storage in `inventory`, and the engine bridge in `place`.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4);
//! see COPYING and COPYING.LESSER.

use std::sync::Arc;

use mod_api::*;

pub mod config;
pub mod curio;
pub mod drop;
pub mod equip;
pub mod inventory;
pub mod place;
pub mod registry;
pub mod slots;

#[derive(Default)]
struct Curios;

#[mod_api::horizon_mod]
impl Hooks for Curios {
    fn register(&self, r: &mut dyn Registrar, _cx: &RegistrationContext) {
        // Curio slots are not vanilla equipment slots. Horizon reaches them
        // through a mod place provider; the stacks live in an attachment.
        r.place_provider(place::PROVIDER, Arc::new(place::CurioPlaceProvider));
    }

    fn register_configs(&self, _host: HostSide<'_>, event: &mut RegisterConfigsEvent<'_>) {
        config::register(event);
    }

    fn config_loading(&self, _host: HostSide<'_>, event: &mut ConfigLoadingEvent<'_>) {
        config::loading(event.config);
    }

    fn config_reloading(&self, _host: HostSide<'_>, event: &mut ConfigReloadingEvent<'_>) {
        config::loading(event.config);
    }

    /// Items that no longer fit a slot after a data reload are dropped on the
    /// next server tick. Dropping is server-authoritative: the client only
    /// displays the result.
    fn server_tick(&self, host: &mut dyn ServerHost, _event: &mut ServerTickEvent<'_>) {
        for player in host.players().server_players() {
            let entity = player.entity();
            let holder = Holder::Entity(entity.id());
            let mut inventory = match inventory::INVENTORY.get(host, holder) {
                Ok(inventory) => inventory,
                Err(error) => {
                    eprintln!("[curios] could not read inventory for player {}: {error:?}", player.uuid());
                    continue;
                }
            };
            let invalid = inventory.take_invalid();
            if invalid.is_empty() {
                continue;
            }
            let Some(living) = host.living(entity) else {
                continue;
            };
            for snapshot in invalid {
                match snapshot.to_stack(host) {
                    Ok(stack) => {
                        if let Err(error) = living.drop(host, stack, true, false) {
                            eprintln!("[curios] could not drop an invalid stack: {error:?}");
                        }
                    }
                    Err(error) => eprintln!("[curios] invalid stack could not be decoded: {error:?}"),
                }
            }
            if let Err(error) = inventory::INVENTORY.set(host, holder, inventory) {
                eprintln!("[curios] could not save inventory after dropping invalid stacks: {error:?}");
            }
        }
    }

    fn register_commands(&self, _host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        event.commands.push(Command::new("curios slots"));
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        let path = &event.command.literals;
        if path.first().map(String::as_str) != Some("curios") {
            return Flow::Continue;
        }
        if path.get(1).map(String::as_str) != Some("slots") {
            feedback(host, Text::literal("§cUsage: /curios slots"));
            return Flow::Cancel;
        }
        let mut lines = vec![Text::literal("§6Curios slot types§r".to_owned())];
        for slot in registry::slot_types() {
            lines.push(Text::literal(format!(
                "§e{}§7 — order {} · size {} · drop {}",
                slot.id(),
                slot.order(),
                slot.size(),
                slot.drop_rule()
            )));
        }
        for line in lines {
            feedback(host, line);
        }
        Flow::Cancel
    }
}

fn feedback(host: &mut dyn ServerHost, line: Text) {
    if let Err(error) = host.commands().feedback(line) {
        eprintln!("[curios] command feedback failed: {error:?}");
    }
}
