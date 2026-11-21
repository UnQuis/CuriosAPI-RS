//! Item-side Curio effects and what Horizon can actually express.
//!
//! Ported behavior from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright
//! 2018-2024 C4): enderman friendliness, piglin neutrality, powder-snow
//! breathing, Mending/Fortune/Looting bonuses, equip sounds and reequip
//! animations.
//!
//! # Horizon limitations (pinned API 0.2)
//!
//! * Enderman friendliness is expressible through `Hooks::enderman_anger`
//!   ([`enderman_anger`]); there is no per-curio "disguise" component.
//! * Piglin neutrality is an `ItemPart` hook, not a `Hooks` hook
//!   ([`makes_piglins_neutral`]); an item part must call it from its own code.
//! * Powder snow has no dedicated hook. [`living_breathe`] can keep air refilled
//!   while a curio is worn, but it cannot change the block's suffocation rules.
//! * Fortune/Looting: `get_enchantment_level` receives only a stack, never the
//!   wearer, and fires for unequipped stacks on both sides, so the hook is a
//!   deliberate no-op ([`get_enchantment_level`]). [`apply_enchantment_bonus`]
//!   is a pure helper for callers that can prove the owner; it is not wired to
//!   the global hook. There is no global "mining luck" hook for Curio slots.
//! * Mending: `player_item_mend` is observe-only (its `repair`/`experience`
//!   fields are inputs, not edits), so a Curio Mending bonus cannot be injected
//!   into the repair. [`mending_bonus`] computes the intended value only.
//! * Break/reequip animation: only the client-side `ItemPart` hook
//!   `should_cause_reequip_animation` is available ([`suppress_reequip_animation`]).
//! * Equip sound: no `getEquipSound` hook exists. The engine plays sound from
//!   `LivingRef::set_equipment`; Curio sound overrides are not representable.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use mod_api::{
    AttachmentHolder, AttachmentQueries, EndermanAngerEvent, EntityRef, Flow,
    GetEnchantmentLevelEvent, Host, Identifier, ItemEnchantments, LivingBreatheEvent,
    MakesPiglinsNeutralEvent, PlayerItemMendEvent, ServerHost,
    ShouldCauseReequipAnimationEvent,
};
use serde::{Deserialize, Serialize};

use crate::curio;
use crate::inventory;

/// Effects aggregated from the curios an entity wears.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurioEffects {
    /// Cancel the enderman stare check (Java enderman immunity).
    pub enderman_friendly: bool,
    /// Make piglins neutral toward the wearer.
    pub piglin_neutral: bool,
    /// Keep the wearer's air refilled in powder snow.
    pub powder_snow_walkable: bool,
    /// Intended bonus durability repaired per Mending orb.
    pub mending_bonus: u32,
    /// Bonus Fortune levels for the wearer's tool.
    pub fortune: u32,
    /// Bonus Looting levels for the wearer's weapon.
    pub looting: u32,
    /// Suppress the reequip/break animation on equip.
    pub suppress_break_animation: bool,
    /// Equip sound the Java item would request (documentation only).
    pub equip_sound: Option<String>,
}

impl CurioEffects {
    /// Adds two effect sets, saturating numeric bonuses and OR-ing flags.
    pub fn merge(self, other: Self) -> Self {
        Self {
            enderman_friendly: self.enderman_friendly || other.enderman_friendly,
            piglin_neutral: self.piglin_neutral || other.piglin_neutral,
            powder_snow_walkable: self.powder_snow_walkable || other.powder_snow_walkable,
            mending_bonus: self.mending_bonus.saturating_add(other.mending_bonus),
            fortune: self.fortune.saturating_add(other.fortune),
            looting: self.looting.saturating_add(other.looting),
            suppress_break_animation: self.suppress_break_animation
                || other.suppress_break_animation,
            equip_sound: self.equip_sound.or(other.equip_sound),
        }
    }

    /// Aggregates effects from an iterator of per-item effect sets.
    pub fn aggregate(effects: impl IntoIterator<Item = Self>) -> Self {
        effects
            .into_iter()
            .fold(Self::default(), |total, effect| total.merge(effect))
    }
}

static EFFECTS: OnceLock<Mutex<BTreeMap<String, CurioEffects>>> = OnceLock::new();

fn effects_registry() -> &'static Mutex<BTreeMap<String, CurioEffects>> {
    EFFECTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Why an effect registration was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectRegisterError {
    /// Item id is not `namespace:path`.
    InvalidId(String),
    /// An effect set is already registered for this id.
    Duplicate(String),
    /// The registry mutex was poisoned.
    Poisoned,
}

impl fmt::Display for EffectRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid item id `{id}`"),
            Self::Duplicate(id) => write!(f, "effects already registered for `{id}`"),
            Self::Poisoned => f.write_str("effect registry lock is poisoned"),
        }
    }
}

impl Error for EffectRegisterError {}

/// Registers the effects granted by an item id.
pub fn register_effects(
    item_id: impl Into<String>,
    effects: CurioEffects,
) -> Result<(), EffectRegisterError> {
    let id = item_id.into();
    if !curio::valid_item_id(&id) {
        return Err(EffectRegisterError::InvalidId(id));
    }
    let mut registry = effects_registry()
        .lock()
        .map_err(|_| EffectRegisterError::Poisoned)?;
    if registry.contains_key(&id) {
        return Err(EffectRegisterError::Duplicate(id));
    }
    registry.insert(id, effects);
    Ok(())
}

/// Effects declared for a single item id.
pub fn effects_for(item_id: &str) -> Option<CurioEffects> {
    effects_registry().lock().ok()?.get(item_id).cloned()
}

/// Aggregates the effects of every equipped curio on an entity.
///
/// Uses the typed attachment query, which only needs `&dyn Host`; the direct
/// `AttachmentType::get` requires `&mut` and cannot be used in most hooks.
pub fn aggregate_for_entity(host: &dyn Host, entity: EntityRef) -> CurioEffects {
    let query = match host.query(&inventory::INVENTORY) {
        Ok(query) => query,
        Err(error) => {
            eprintln!("[curios] attachment query refused: {error:?}");
            return CurioEffects::default();
        }
    };
    let inventory = match query.get(attachment_holder(host, entity)) {
        Ok(Some(inventory)) => inventory,
        Ok(None) => return CurioEffects::default(),
        Err(error) => {
            eprintln!(
                "[curios] inventory row for entity {} could not be read: {error:?}",
                entity.id()
            );
            return CurioEffects::default();
        }
    };
    aggregate_from_inventory(&inventory)
}

fn aggregate_from_inventory(inventory: &inventory::CurioInventory) -> CurioEffects {
    let mut ids = Vec::new();
    for slot_id in inventory.slot_ids() {
        if let Some(handler) = inventory.slot(slot_id) {
            for (_, item) in handler.filled() {
                if let Some(id) = item.item_id() {
                    ids.push(id);
                }
            }
        }
    }
    aggregate_for_items(ids.iter().map(String::as_str))
}

static TICK_EFFECTS: Mutex<BTreeMap<u32, CurioEffects>> = Mutex::new(BTreeMap::new());

/// Entity-id → player-uuid map captured by [`refresh_effects`].
static TICK_PLAYERS: Mutex<BTreeMap<u32, u128>> = Mutex::new(BTreeMap::new());

/// Resolves the attachment holder for an entity, using `AttachmentHolder::Player`
/// for players (their data is not keyed by entity id) and `Entity` for mobs.
pub fn attachment_holder(host: &dyn Host, entity: EntityRef) -> AttachmentHolder {
    let uuid = TICK_PLAYERS
        .lock()
        .ok()
        .and_then(|players| players.get(&entity.id()).copied());
    match uuid.and_then(|uuid| host.player(uuid)) {
        Some(player) => AttachmentHolder::Player(player),
        None => AttachmentHolder::Entity(entity),
    }
}

/// Refreshes the per-entity effect cache from `server_tick`.
///
/// Some hooks (`living_breathe`) receive only an entity id and no host, so a
/// tick-fresh snapshot is necessary there.
pub fn refresh_effects(host: &mut dyn ServerHost) {
    let mut values = BTreeMap::new();
    let mut players = BTreeMap::new();
    // Mobs receive `living_breathe` too, so the cache must cover every loaded
    // entity, not just online players (review #19).
    for level in host.levels().list_levels() {
        let Some(entities) = level.all_entities(host) else {
            continue;
        };
        for entity in entities {
            if let Some(uuid) = crate::inventory::player_uuid_for_entity(host, entity.id()) {
                players.insert(entity.id(), uuid);
            }
            values.insert(entity.id(), aggregate_for_entity(host, entity));
        }
    }
    let mut player_cache = TICK_PLAYERS.lock().unwrap_or_else(|e| e.into_inner());
    *player_cache = players;
    let mut cache = TICK_EFFECTS.lock().unwrap_or_else(|e| e.into_inner());
    *cache = values;
}

static REPORTED_MISSES: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Cached effects for an entity id, last refreshed by [`refresh_effects`].
///
/// A miss is reported once per entity instead of silently granting no effects
/// (review #19); the hook still gets safe defaults after the diagnostic.
pub fn cached_effects(entity_id: u32) -> CurioEffects {
    let effects = TICK_EFFECTS
        .lock()
        .ok()
        .and_then(|cache| cache.get(&entity_id).cloned());
    if let Some(effects) = effects {
        return effects;
    }
    let mut reported = REPORTED_MISSES.lock().unwrap_or_else(|e| e.into_inner());
    if reported.insert(entity_id) {
        eprintln!("[curios] effects cache miss for entity {entity_id}; refresh_effects did not see it");
    }
    CurioEffects::default()
}

/// Aggregates effects by canonical item id, without needing a host.
pub fn aggregate_for_items<'a>(ids: impl IntoIterator<Item = &'a str>) -> CurioEffects {
    CurioEffects::aggregate(ids.into_iter().filter_map(effects_for))
}

/// `Hooks::enderman_anger`: cancels the stare check when the player wears a
/// curio with [`CurioEffects::enderman_friendly`].
pub fn enderman_anger(host: &dyn Host, event: &mut EndermanAngerEvent) -> Flow {
    let effects = aggregate_for_entity(host, event.player.entity());
    if effects.enderman_friendly {
        event.cancelled = true;
        Flow::Cancel
    } else {
        Flow::Continue
    }
}

/// `ItemPart::makes_piglins_neutral` helper called with the wearer's host.
pub fn makes_piglins_neutral(host: &dyn Host, event: &mut MakesPiglinsNeutralEvent<'_>) {
    if aggregate_for_entity(host, event.wearer.entity()).piglin_neutral {
        event.neutral = true;
    }
}

/// `Hooks::living_breathe`: partial powder-snow support. Keeps air refilled
/// while the entity wears a curio with [`CurioEffects::powder_snow_walkable`].
pub fn living_breathe(event: &mut LivingBreatheEvent<'_>) {
    if cached_effects(event.entity).powder_snow_walkable {
        event.can_breathe = true;
        event.consume = 0;
    }
}

/// Applies a wearer's Fortune/Looting to an enchantment collection.
///
/// This is NOT wired to `Hooks::get_enchantment_level` (which has no wearer;
/// see [`ENCHANTMENT_HOOK_LIMITATION`]). Call it only from a caller that can
/// prove which entity owns the stack and has that entity's effects.
pub fn apply_enchantment_bonus(
    event: &mut GetEnchantmentLevelEvent<'_>,
    effects: &CurioEffects,
) {
    add_enchantment_bonus(event.enchantments, effects);
}

/// Pure part of [`apply_enchantment_bonus`], kept separately for unit tests.
pub fn add_enchantment_bonus(enchantments: &mut ItemEnchantments, effects: &CurioEffects) {
    for (name, bonus) in [
        ("minecraft:fortune", effects.fortune),
        ("minecraft:looting", effects.looting),
    ] {
        if bonus == 0 {
            continue;
        }
        let Ok(enchantment) = Identifier::parse(name) else {
            eprintln!("[curios] invalid enchantment id `{name}`");
            continue;
        };
        let current = enchantments.level(&enchantment);
        enchantments.upgrade(enchantment, current.saturating_add(bonus));
    }
}

/// Why Curio Fortune/Looting cannot be added from the global enchantment hook.
///
/// In the pinned SDK, `GetEnchantmentLevelEvent` has exactly three fields:
/// `stack: &ItemStack`, `target: Option<&Identifier>` and
/// `enchantments: &mut ItemEnchantments` (generated/mod_api_generated.rs:15635).
/// `Hooks::get_enchantment_level` receives no `Host`, no entity and no wearer,
/// and fires on both sides for every enchantment read (often many times per
/// tick) on stacks that may not be equipped at all. Applying a cached wearer's
/// effects here would grant bonuses from somebody else's curios to an arbitrary
/// stack, so the hook is intentionally a no-op. `apply_enchantment_bonus`
/// remains available only to callers that can prove the wearer independently.
pub const ENCHANTMENT_HOOK_LIMITATION: &str = concat!(
    "GetEnchantmentLevelEvent exposes only stack, target and enchantments; ",
    "there is no host, entity or wearer in the hook, and it also fires for ",
    "unequipped stacks on both sides. Wearer-scoped Curio bonuses cannot be ",
    "attributed safely and are intentionally omitted."
);

/// `Hooks::get_enchantment_level` entry point: deliberately applies nothing.
///
/// See [`ENCHANTMENT_HOOK_LIMITATION`] for the exact SDK evidence. Do not
/// replace this with a cached-owner lookup: the event gives no owner to match.
pub fn get_enchantment_level(event: &mut GetEnchantmentLevelEvent<'_>) {
    let _ = event;
}

/// Intended Mending bonus; Horizon's `player_item_mend` cannot inject it.
pub const fn mending_bonus(effects: &CurioEffects) -> u32 {
    effects.mending_bonus
}

/// Observe-only `Hooks::player_item_mend` payload for diagnostics.
pub fn describe_mend(event: &PlayerItemMendEvent<'_>) -> String {
    format!(
        "player {} mended {} with {} experience (repair {})",
        event.player, event.item, event.experience, event.repair
    )
}

/// `Hooks::player_item_mend` entry point: logs the event. Horizon exposes the
/// repair and experience amounts as inputs, so a Curio Mending bonus cannot be
/// applied here.
pub fn player_item_mend(event: &mut PlayerItemMendEvent<'_>) {
    eprintln!("[curios] {}", describe_mend(event));
}

/// `ItemPart::should_cause_reequip_animation` helper: suppress the animation
/// for registered curio items whose effects request it.
pub fn suppress_reequip_animation(
    event: &mut ShouldCauseReequipAnimationEvent<'_>,
    item_id: &str,
) {
    if effects_for(item_id).is_some_and(|effects| effects.suppress_break_animation) {
        event.reequip = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_merge_saturates_and_keeps_first_sound() {
        let a = CurioEffects {
            fortune: 2,
            mending_bonus: 1,
            equip_sound: Some("curios:equip".to_owned()),
            ..CurioEffects::default()
        };
        let b = CurioEffects {
            fortune: u32::MAX,
            looting: 3,
            equip_sound: Some("other:sound".to_owned()),
            ..CurioEffects::default()
        };
        let merged = a.merge(b);
        assert_eq!(merged.fortune, u32::MAX);
        assert_eq!(merged.looting, 3);
        assert_eq!(merged.mending_bonus, 1);
        assert_eq!(merged.equip_sound.as_deref(), Some("curios:equip"));
    }

    #[test]
    fn registry_refuses_invalid_and_duplicate_ids() {
        let effects = CurioEffects {
            piglin_neutral: true,
            ..CurioEffects::default()
        };
        assert!(matches!(
            register_effects("ring", effects.clone()),
            Err(EffectRegisterError::InvalidId(_))
        ));
        register_effects("curios:ring_of_neutrality", effects.clone()).unwrap();
        assert_eq!(
            register_effects("curios:ring_of_neutrality", effects.clone()),
            Err(EffectRegisterError::Duplicate(
                "curios:ring_of_neutrality".to_owned()
            ))
        );
        assert_eq!(
            effects_for("curios:ring_of_neutrality"),
            Some(effects.clone())
        );
        assert_eq!(aggregate_for_items(["curios:ring_of_neutrality"]), effects);
    }

    #[test]
    fn enchantment_bonus_upgrades_existing_level() {
        let mut enchantments = ItemEnchantments::default();
        let fortune = Identifier::parse("minecraft:fortune").unwrap();
        enchantments.set(fortune.clone(), 3);
        let effects = CurioEffects {
            fortune: 2,
            looting: 1,
            ..CurioEffects::default()
        };
        add_enchantment_bonus(&mut enchantments, &effects);
        assert_eq!(enchantments.level(&fortune), 5);
        assert_eq!(
            enchantments.level(&Identifier::parse("minecraft:looting").unwrap()),
            1
        );
    }

    #[test]
    fn enchantment_hook_limitation_is_documented() {
        assert!(ENCHANTMENT_HOOK_LIMITATION.contains("no host"));
        assert!(ENCHANTMENT_HOOK_LIMITATION.contains("unequipped"));
    }

    #[test]
    fn mending_helper_reports_intended_bonus() {
        let effects = CurioEffects {
            mending_bonus: 4,
            ..CurioEffects::default()
        };
        assert_eq!(mending_bonus(&effects), 4);
    }
}
