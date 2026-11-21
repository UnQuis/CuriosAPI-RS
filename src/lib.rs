//! CuriosAPI-RS: Curios accessory slots on the Horizon native mod API.
//!
//! Native mod against the generated `mod_api` (API 0.2). The crate root holds
//! the one `#[horizon_mod] impl Hooks`; item behavior, storage, engine bridge,
//! network, GUI, rendering and events live in their own modules.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024 C4);
//! see COPYING and COPYING.LESSER.

use std::sync::Arc;

use mod_api::*;

pub mod api;
pub mod attributes;
pub mod commands;
pub mod config;
pub mod curio;
pub mod datapack;
pub mod drop;
pub mod effects;
pub mod equip;
pub mod events;
pub mod input;
pub mod gametests;
pub mod inventory;
pub mod itempart;
pub mod lang;
pub mod menu;
pub mod net;
pub mod place;
pub mod registry;
pub mod render;
#[cfg(test)]
mod review_regressions;
pub mod slots;

/// Key that toggles rendering of the hovered Curio slot in the open menu.
const RENDER_KEY_ID: &str = "key.curios.toggle_render";
const RENDER_KEY_DEFAULT: &str = "R";
const PAGE_NEXT_KEY: &str = "key.curios.page_next";
const PAGE_PREV_KEY: &str = "key.curios.page_prev";
const COSMETICS_KEY: &str = "key.curios.toggle_cosmetics";

#[derive(Default)]
struct Curios;

#[mod_api::horizon_mod]
impl Hooks for Curios {
    fn register(&self, r: &mut dyn Registrar, _cx: &RegistrationContext) {
        // Curio slots are not vanilla equipment slots: the engine reaches
        // them through a mod place provider backed by an attachment.
        r.place_provider(place::PROVIDER, Arc::new(place::CurioPlaceProvider));
        net::register(r);
        r.register_key_mapping(input::key_registration());
        r.register_key_mapping(
            KeyRegistration::new(RENDER_KEY_ID, RENDER_KEY_DEFAULT)
                .label("curios.key.toggle_render")
                .context(KeyConflictContext::Universal),
        );
        r.add_payload(
            input::key_payload_declaration(),
            Some(input::key_press_handler()),
            None,
        );
        r.add_payload(
            input::render_payload_declaration(),
            Some(input::render_handler()),
            None,
        );
        r.add_payload(
            input::menu_payload_declaration(),
            Some(input::menu_handler()),
            None,
        );
        r.add_payload(
            input::screen_state_payload_declaration(),
            None,
            Some(input::screen_state_handler()),
        );
        for (id, default) in [
            (PAGE_NEXT_KEY, "]"),
            (PAGE_PREV_KEY, "["),
            (COSMETICS_KEY, "C"),
        ] {
            r.register_key_mapping(
                KeyRegistration::new(id, default)
                    .label(id)
                    .context(KeyConflictContext::Universal),
            );
        }
        itempart::register(r);
        render::register(r);
        if let Err(error) = datapack::declare(r) {
            eprintln!("[curios] slot-type registry declaration failed: {error:?}");
        }
    }

    fn register_game_tests(&self, host: &dyn Host, event: &mut RegisterGameTestsEvent<'_>) {
        gametests::register(host, event);
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

    fn component_types(&self, _context: &RegistrationContext, event: &mut ComponentTypesEvent<'_>) {
        attributes::register_component_types(event);
    }

    fn register_entity_layers(
        &self,
        _context: &RegistrationContext,
        event: &mut RegisterEntityLayersEvent<'_>,
    ) {
        render::register_layers(event);
    }

    fn item_attribute_modifier(&self, event: &mut ItemAttributeModifierEvent<'_>) {
        attributes::item_attribute_modifier(event);
    }

    /// Items that no longer fit a slot after a data reload are dropped on the
    /// next server tick. This is server-authoritative.
    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {
        // Data-pack changes are applied outside the Reloading phase, where the
        // host has no players yet (same deferral as ProjectE-RS).
        datapack::reload(host, event);
        effects::refresh_effects(host);
        for player in host.players().server_players() {
            let entity = player.entity();
            let holder = inventory::holder_for_player(player.uuid());
            let mut inventory = match inventory::INVENTORY.get(host, holder) {
                Ok(inventory) => inventory,
                Err(error) => {
                    eprintln!("[curios] inventory read failed for {}: {error:?}", player.uuid());
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
                            eprintln!("[curios] invalid-stack drop failed: {error:?}");
                        }
                    }
                    Err(error) => eprintln!("[curios] invalid stack decode failed: {error:?}"),
                }
            }
            if let Err(error) = inventory::INVENTORY.set(host, holder, inventory) {
                eprintln!("[curios] inventory save after drop failed: {error:?}");
            }
        }
    }

    /// Java `CuriosCommonEvents.handleDrops`: resolve drop rules on death.
    /// Bring an offline player's inventory to the current slot layout when
    /// they join (review #22: data-pack reload only migrates resident players).
    fn player_logged_in(&self, host: &mut dyn ServerHost, event: &mut PlayerLoggedInEvent<'_>) {
        let types = registry::slot_types();
        if let Err(error) = inventory::migrate_offline(host, event.player, types.as_slice()) {
            eprintln!(
                "[curios] offline inventory migration failed for {}: {error:?}",
                event.player
            );
        }
    }

    fn data_pack_files(&self, _host: &mut dyn ServerHost, event: &mut DataPackFilesEvent<'_>) {
        // Contributes both the slot-type entries and the two tags (physical
        // files under data/ alone are not seen by the engine).
        datapack::data_pack_files(event);
    }

    fn data_maps_updated(&self, _host: HostSide<'_>, _event: &mut DataMapsUpdatedEvent<'_>) {
        datapack::mark_pending();
    }

    fn enderman_anger(&self, host: &dyn Host, event: &mut EndermanAngerEvent) -> Flow {
        effects::enderman_anger(host, event)
    }

    fn player_item_mend(&self, _host: &mut dyn ServerHost, event: &mut PlayerItemMendEvent<'_>) {
        effects::player_item_mend(event);
    }

    fn living_breathe(&self, _context: &dyn LoopContext, event: &mut LivingBreatheEvent<'_>) {
        effects::living_breathe(event);
    }

    fn living_death(&self, host: &mut dyn ServerHost, event: &mut LivingDeathEvent<'_>) -> Flow {
        let holder = inventory::holder_for_server_entity(host, event.entity);
        let Ok(mut inventory) = inventory::INVENTORY.get(host, holder) else {
            return Flow::Continue;
        };
        let Some(entity) = host.entity(event.entity).and_then(|entity| host.living(entity)) else {
            return Flow::Continue;
        };
        let game_rule = matches!(host.game_rule("keep_inventory"), Ok(GameRuleValue::Bool(true)));
        let keep = config::keep_inventory(game_rule);
        let mut drops = Vec::new();
        for slot in registry::slot_types().iter() {
            let Some(handler) = inventory.slot_mut(slot.id()) else {
                continue;
            };
            for index in 0..handler.len() {
                let Some(snapshot) = handler
                    .stack(index)
                    .and_then(|entry| entry.as_ref())
                    .cloned()
                else {
                    continue;
                };
                let item_rule = snapshot
                    .item_id()
                    .and_then(|id| curio::behavior(&id))
                    .map_or(drop::DropRule::Default, |behavior| behavior.drop_rule());
                let decision = equip::decide_death_drop_with_events(
                    event.entity,
                    &snapshot,
                    item_rule,
                    handler.drop_rule(),
                    keep,
                    false,
                );
                // `apply_drop` only returns a stack for `DropOutcome::Drop`;
                // `Destroy` items are cleared without spawning anything.
                if let Some(removed) = handler.apply_drop(index, decision).unwrap_or(None) {
                    drops.push(removed);
                }
            }
        }
        for snapshot in events::dispatch_death_drops(event.entity, drops) {
            match snapshot.to_stack(host) {
                Ok(stack) => {
                    if let Err(error) = entity.drop(host, stack, true, false) {
                        eprintln!("[curios] death drop failed: {error:?}");
                    }
                }
                Err(error) => eprintln!("[curios] death stack decode failed: {error:?}"),
            }
        }
        if let Err(error) = inventory::INVENTORY.set(host, holder, inventory) {
            eprintln!("[curios] inventory save after death failed: {error:?}");
        }
        Flow::Continue
    }

    fn register_commands(&self, _host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        commands::register(event);
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        if commands::is_ours(event) {
            commands::run(host, event)
        } else {
            Flow::Continue
        }
    }

    fn menu_clicked(&self, host: &mut dyn ServerHost, event: &mut MenuClickedEvent) {
        // The GUI layer rejects negative slots and foreign windows itself.
        menu::menu_clicked(
            host,
            event.uuid,
            event.window,
            event.slot,
            event.button,
            event.mode,
        );
    }

    fn menu_closed(&self, _host: &mut dyn ServerHost, event: &mut MenuClosedEvent) {
        menu::menu_closed(event.uuid);
    }

    fn screen_opening(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenOpeningEvent<'_>,
    ) -> Option<ScreenOpening> {
        menu::screen_opening(client_uuid(host), event.new);
        None
    }

    fn screen_render_background(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenRenderBackgroundEvent<'_>,
    ) {
        menu::screen_render_background(client_uuid(host), host, event);
    }

    fn screen_render_post(&self, host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
        menu::screen_render_post(client_uuid(host), host, event);
    }

    fn screen_mouse_button_pressed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseButtonPressedPreEvent<'_>,
    ) -> Flow {
        menu::screen_mouse_button_pressed_pre(client_uuid(host), event.screen, event.button)
    }

    fn screen_mouse_scrolled_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseScrolledPreEvent<'_>,
    ) -> Flow {
        menu::screen_mouse_scrolled_pre(client_uuid(host), event.screen, event.scroll)
    }

    fn key_mapping_pre(&self, host: &mut dyn ClientHost, event: &mut KeyMappingPreEvent<'_>) -> Flow {
        // Screen hooks only have `&dyn ClientHost`; button actions are queued
        // there and sent from this mutable hook.
        input::flush_pending_actions(host);
        if input::on_mapping_press(host, event.mapping, event.pressed) {
            return Flow::Cancel;
        }
        if event.pressed {
            let handled = match event.mapping {
                RENDER_KEY_ID => menu::hovered_render_slot(client_uuid(host))
                    .is_some_and(|slot| input::send_render_toggle(host, slot)),
                PAGE_NEXT_KEY => input::send_page(host, true),
                PAGE_PREV_KEY => input::send_page(host, false),
                COSMETICS_KEY => input::send_toggle_cosmetics(host),
                _ => false,
            };
            if handled {
                return Flow::Cancel;
            }
        }
        Flow::Continue
    }

    fn render_living_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut RenderLivingPreEvent<'_>,
    ) -> Flow {
        render::render_living(host, event)
    }
}

/// Local player id for client screen hooks, which have no event field for it.
fn client_uuid(host: &dyn ClientHost) -> u128 {
    host.local_player().map_or(0, |player| player.uuid())
}
