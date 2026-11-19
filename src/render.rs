//! Client-side rendering of Curio slots on living entities.
//!
//! Horizon has no per-item `ICurioRenderer`: a layer is attached to a target
//! (`PlayerModel` or living entity type) and reads what to draw from a hook
//! cache. `render_living` snapshots the synced inventory on the client;
//! `register_layers` adds one layer per built-in slot to each target.
//!
//! Slot visibility follows the Java reference: a cosmetic stack always wins
//! over the functional one at the same index and ignores the render flag,
//! while a functional stack draws only when its render flag is set. The
//! process-wide `render_curios` config gates the whole pass.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use mod_api::{
    AttachmentHolder, AttachmentQueries, BlockEntityDrawContext, ClientHost, EntityRef,
    EntityRenderLayer, EntityRenderState, Flow, ItemDisplayContext, ItemStack, LayerTarget,
    PlayerModel, PosedParts, Registrar, RegisterEntityLayersEvent, RenderLivingPreEvent,
};

use crate::config;
use crate::inventory::{CurioInventory, SlotStacks, INVENTORY};
use crate::registry;

/// Which of a slot's two stacks a drawn item came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderSource {
    Functional,
    Cosmetic,
}

/// An item to draw, independent of any Horizon handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedEntry {
    pub slot: String,
    pub index: usize,
    pub source: RenderSource,
}

/// Presence and render flags of one slot handler, as plain booleans.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotPresence {
    pub functional: Vec<bool>,
    pub cosmetic: Vec<bool>,
    pub render: Vec<bool>,
}

/// Pure slot-selection rule. Cosmetic wins at an index and ignores `render`;
/// functional needs both a stack and a true render flag.
pub fn plan_presence<S: Borrow<SlotPresence>>(
    enabled: bool,
    slots: &[(&str, S)],
) -> Vec<PlannedEntry> {
    if !enabled {
        return Vec::new();
    }
    let mut planned = Vec::new();
    for (slot, presence) in slots {
        let presence = presence.borrow();
        let len = presence
            .functional
            .len()
            .max(presence.cosmetic.len())
            .max(presence.render.len());
        for index in 0..len {
            if presence.cosmetic.get(index).copied().unwrap_or(false) {
                planned.push(PlannedEntry {
                    slot: (*slot).to_owned(),
                    index,
                    source: RenderSource::Cosmetic,
                });
            } else if presence.render.get(index).copied().unwrap_or(false)
                && presence.functional.get(index).copied().unwrap_or(false)
            {
                planned.push(PlannedEntry {
                    slot: (*slot).to_owned(),
                    index,
                    source: RenderSource::Functional,
                });
            }
        }
    }
    planned
}

fn presence_of(handler: &SlotStacks) -> SlotPresence {
    let len = handler.len();
    SlotPresence {
        functional: (0..len)
            .map(|index| handler.stack(index).and_then(Option::as_ref).is_some())
            .collect(),
        cosmetic: (0..len)
            .map(|index| handler.cosmetic(index).and_then(Option::as_ref).is_some())
            .collect(),
        render: (0..len)
            .map(|index| handler.render(index).unwrap_or(false))
            .collect(),
    }
}

/// One item resolved on the client and kept for the frame's layers.
#[derive(Clone, Debug)]
struct CachedItem {
    slot: String,
    index: i32,
    stack: ItemStack,
}

static CACHE: Mutex<BTreeMap<u32, Vec<CachedItem>>> = Mutex::new(BTreeMap::new());

fn cache() -> MutexGuard<'static, BTreeMap<u32, Vec<CachedItem>>> {
    CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_inventory(
    host: &dyn ClientHost,
    entity: EntityRef,
) -> Result<Option<CurioInventory>, String> {
    let query = host
        .query(&INVENTORY)
        .map_err(|error| format!("attachment query refused: {error:?}"))?;
    query
        .get(AttachmentHolder::Entity(entity))
        .map_err(|error| format!("inventory row could not be decoded: {error:?}"))
}

/// For `Hooks::render_living_pre`: resolve the synced stacks for this entity
/// once per frame; the layers only read the cache and never touch a host.
pub fn render_living(host: &dyn ClientHost, event: &mut RenderLivingPreEvent<'_>) -> Flow {
    let id = event.entity.id;
    if !config::render_curios() {
        cache().remove(&id);
        return Flow::Continue;
    }
    let Some(entity) = host.entity(id) else {
        cache().remove(&id);
        return Flow::Continue;
    };
    let inventory = match read_inventory(host, entity) {
        Ok(Some(inventory)) => inventory,
        Ok(None) => {
            cache().remove(&id);
            return Flow::Continue;
        }
        Err(message) => {
            eprintln!("[curios] could not read synced inventory for entity {id}: {message}");
            cache().remove(&id);
            return Flow::Continue;
        }
    };

    let types = registry::slot_types();
    let slots: Vec<(&str, SlotPresence)> = types
        .iter()
        .filter_map(|slot| {
            inventory
                .slot(slot.id())
                .map(|handler| (slot.id(), presence_of(handler)))
        })
        .collect();
    let planned = plan_presence(true, &slots);

    let mut items = Vec::with_capacity(planned.len());
    for entry in planned {
        let Some(handler) = inventory.slot(&entry.slot) else {
            continue;
        };
        let snapshot = match entry.source {
            RenderSource::Cosmetic => handler.cosmetic(entry.index).and_then(Option::as_ref),
            RenderSource::Functional => handler.stack(entry.index).and_then(Option::as_ref),
        };
        let Some(snapshot) = snapshot else {
            continue;
        };
        let host_base: &dyn mod_api::Host = host;
        match snapshot.to_stack(host_base) {
            Ok(stack) => items.push(CachedItem {
                slot: entry.slot,
                index: i32::try_from(entry.index).unwrap_or(i32::MAX),
                stack,
            }),
            Err(error) => eprintln!(
                "[curios] could not resolve the {} slot {} item for entity {id}: {error:?}",
                entry.slot, entry.index
            ),
        }
    }
    cache().insert(id, items);
    Flow::Continue
}

/// Drops cached items for an entity whose inventory is no longer available.
pub fn forget(entity_id: u32) {
    cache().remove(&entity_id);
}

/// Curio slots cannot be attached through `Registrar`: layers need the
/// `EntityLayers` registry that `Hooks::register_entity_layers` supplies.
/// Kept so the crate root has one render entry point for registration too.
pub fn register(_r: &mut dyn Registrar) {}

/// One layer per slot id. `submit` draws only that slot's cached items.
struct CurioSlotLayer {
    slot: String,
}

impl CurioSlotLayer {
    fn new(slot: &str) -> Self {
        Self {
            slot: slot.to_owned(),
        }
    }
}

/// Approximate placement in the entity's frame: feet at y=0, +x its left,
/// +z the way it faces. The items use vanilla display transforms; exact
/// per-slot models are the asset owner's follow-up.
fn slot_offset(slot: &str) -> [f32; 3] {
    match slot {
        "head" => [0.0, 1.62, 0.02],
        "necklace" => [0.0, 1.38, 0.14],
        "back" => [0.0, 1.22, -0.24],
        "body" => [0.0, 1.02, 0.16],
        "bracelet" => [0.26, 1.0, 0.04],
        "hands" => [0.3, 0.88, 0.16],
        "ring" => [0.24, 0.7, 0.12],
        "belt" => [0.0, 0.76, 0.16],
        "feet" => [0.18, 0.14, 0.06],
        "charm" => [0.0, 1.12, 0.22],
        _ => [0.0, 1.0, 0.2],
    }
}

impl EntityRenderLayer for CurioSlotLayer {
    fn submit(
        &self,
        entity: &EntityRenderState,
        _parts: Option<&PosedParts>,
        draw: &mut dyn BlockEntityDrawContext,
    ) -> Result<(), String> {
        let items = cache();
        let Some(items) = items.get(&entity.id) else {
            return Ok(());
        };
        let display = if self.slot == "head" {
            ItemDisplayContext::Head
        } else {
            ItemDisplayContext::Fixed
        };
        for item in items.iter().filter(|item| item.slot == self.slot) {
            draw.push();
            draw.translate(slot_offset(&self.slot));
            draw.scale([0.45, 0.45, 0.45]);
            let drawn = draw.item(&item.stack, display, None, item.index);
            draw.pop();
            if !drawn {
                // A refused model drops only this item; blacklisting the whole
                // layer would hide every other slot of this entity.
                eprintln!(
                    "[curios] model refused for slot {} index {}",
                    self.slot, item.index
                );
            }
        }
        Ok(())
    }
}

/// For `Hooks::register_entity_layers`: add the built-in slot layers to both
/// player models and to every living entity type. Entities without a synced
/// inventory simply draw nothing, so a broad target set is harmless.
pub fn register_layers(event: &mut RegisterEntityLayersEvent<'_>) {
    for model in [PlayerModel::Wide, PlayerModel::Slim] {
        for (slot, _) in registry::BUILTIN {
            if let Err(error) = event.layers.add(
                LayerTarget::Player(model),
                Arc::new(CurioSlotLayer::new(slot)),
            ) {
                eprintln!("[curios] could not add {slot} layer to player model {model:?}: {error}");
            }
        }
    }
    for kind in event.layers.entity_types() {
        if !event.layers.is_living(&kind) {
            continue;
        }
        for (slot, _) in registry::BUILTIN {
            if let Err(error) = event.layers.add(
                LayerTarget::Entity(kind.clone()),
                Arc::new(CurioSlotLayer::new(slot)),
            ) {
                eprintln!("[curios] could not add {slot} layer to {kind}: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_items_are_planned_when_rendering_is_disabled() {
        let present = SlotPresence {
            functional: vec![true],
            cosmetic: vec![true],
            render: vec![true],
        };
        assert!(plan_presence(false, &[("ring", &present)]).is_empty());
    }

    #[test]
    fn cosmetic_overrides_functional_and_ignores_the_render_flag() {
        let present = SlotPresence {
            functional: vec![true, true],
            cosmetic: vec![true, false],
            render: vec![false, false],
        };
        assert_eq!(
            plan_presence(true, &[("head", &present)]),
            vec![PlannedEntry {
                slot: "head".to_owned(),
                index: 0,
                source: RenderSource::Cosmetic
            }]
        );
    }

    #[test]
    fn functional_items_need_a_true_render_flag() {
        let present = SlotPresence {
            functional: vec![true, true],
            cosmetic: vec![],
            render: vec![true, false],
        };
        assert_eq!(
            plan_presence(true, &[("belt", &present)]),
            vec![PlannedEntry {
                slot: "belt".to_owned(),
                index: 0,
                source: RenderSource::Functional
            }]
        );
    }

    #[test]
    fn empty_and_missing_indices_are_skipped() {
        let present = SlotPresence {
            functional: vec![false, true],
            cosmetic: vec![false],
            render: vec![true, true, true],
        };
        let planned = plan_presence(true, &[("charm", &present)]);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].index, 1);
        assert_eq!(planned[0].source, RenderSource::Functional);
    }

    #[test]
    fn slot_order_and_index_are_preserved() {
        let ring = SlotPresence {
            functional: vec![true, true],
            cosmetic: vec![],
            render: vec![true, true],
        };
        let head = SlotPresence {
            functional: vec![true],
            cosmetic: vec![],
            render: vec![true],
        };
        let planned = plan_presence(true, &[("ring", &ring), ("head", &head)]);
        assert_eq!(
            planned
                .iter()
                .map(|entry| (entry.slot.as_str(), entry.index))
                .collect::<Vec<_>>(),
            vec![("ring", 0), ("ring", 1), ("head", 0)]
        );
    }

    #[test]
    fn a_cosmetic_slot_falls_back_to_the_functional_item() {
        let present = SlotPresence {
            functional: vec![true],
            cosmetic: vec![false],
            render: vec![true],
        };
        assert_eq!(
            plan_presence(true, &[("necklace", &present)]),
            vec![PlannedEntry {
                slot: "necklace".to_owned(),
                index: 0,
                source: RenderSource::Functional
            }]
        );
    }
}
