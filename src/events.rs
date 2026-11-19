//! Event surface exposed to other mods.
//!
//! Mirrors the Java NeoForge events `CurioCanEquipEvent`, `CurioCanUnequipEvent`,
//! `CurioChangeEvent`, `CurioDropsEvent`, `DropRulesEvent` and
//! `SlotModifiersUpdatedEvent` from TheIllusiveC4/Curios
//! (LGPL-3.0-or-later, copyright 2018-2024 C4). Every event is a plain value
//! and every listener is an `Arc<dyn Fn>`; dispatch runs them in registration
//! order, matching the event bus, so the last listener that writes a field wins.
//!
//! `equip.rs` stays untouched: call [`dispatch_can_equip`] and pass the
//! returned [`TriState`] to `equip::can_equip`, and likewise for unequip.

use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use crate::drop::{DropRule, DropRules, ItemPredicate};
use crate::equip::TriState;
use crate::inventory::ItemSnapshot;
use crate::slots::SlotContext;

/// Listener stored in an event registry.
pub type Listener<E> = Arc<dyn Fn(&mut E) + Send + Sync>;

/// Failure to register a listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    /// The listener mutex was poisoned by an earlier panic.
    Poisoned,
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("curio event listener registry is poisoned")
    }
}

impl Error for EventError {}

struct Registry<E> {
    listeners: Mutex<Vec<Listener<E>>>,
}

impl<E> Registry<E> {
    const fn new() -> Self {
        Self {
            listeners: Mutex::new(Vec::new()),
        }
    }

    fn register(&self, listener: Listener<E>) -> Result<(), EventError> {
        let mut guard = self.listeners.lock().map_err(|_| EventError::Poisoned)?;
        guard.push(listener);
        Ok(())
    }

    fn dispatch(&self, event: &mut E) {
        let guard = self.listeners.lock().unwrap_or_else(|e| e.into_inner());
        for listener in guard.iter() {
            listener(event);
        }
    }
}

macro_rules! event_registry {
    ($static:ident, $registry:ident, $event:ty, $register:ident, $dispatch:ident) => {
        static $static: OnceLock<Registry<$event>> = OnceLock::new();

        fn $registry() -> &'static Registry<$event> {
            $static.get_or_init(Registry::new)
        }

        /// Registers a listener; later listeners run last and can override.
        pub fn $register(listener: Listener<$event>) -> Result<(), EventError> {
            $registry().register(listener)
        }

        /// Runs every listener in registration order.
        pub fn $dispatch(event: &mut $event) {
            $registry().dispatch(event);
        }
    };
}

/// `CurioCanEquipEvent`: `original` is the computed result, `result` the
/// listener-controlled `TriState` override.
#[derive(Clone, Debug, PartialEq)]
pub struct CurioCanEquipEvent {
    /// Canonical item id being equipped.
    pub item_id: String,
    /// Slot context supplied by the inventory layer.
    pub context: SlotContext,
    /// Server-computed result before listeners run.
    pub original: bool,
    /// Java `TriState` override.
    pub result: TriState,
}

impl CurioCanEquipEvent {
    /// Creates an event with no override.
    pub fn new(item_id: impl Into<String>, context: SlotContext, original: bool) -> Self {
        Self {
            item_id: item_id.into(),
            context,
            original,
            result: TriState::Default,
        }
    }

    /// Final result after applying the override.
    pub const fn resolve(&self) -> bool {
        self.result.apply(self.original)
    }
}

event_registry!(
    CAN_EQUIP,
    can_equip_registry,
    CurioCanEquipEvent,
    register_can_equip,
    dispatch_can_equip
);

/// `CurioCanUnequipEvent`, mirroring the Java unequip check.
#[derive(Clone, Debug, PartialEq)]
pub struct CurioCanUnequipEvent {
    /// Canonical item id being removed.
    pub item_id: String,
    /// Slot context supplied by the inventory layer.
    pub context: SlotContext,
    /// Server-computed result before listeners run.
    pub original: bool,
    /// Java `TriState` override.
    pub result: TriState,
}

impl CurioCanUnequipEvent {
    /// Creates an event with no override.
    pub fn new(item_id: impl Into<String>, context: SlotContext, original: bool) -> Self {
        Self {
            item_id: item_id.into(),
            context,
            original,
            result: TriState::Default,
        }
    }

    /// Final result after applying the override.
    pub const fn resolve(&self) -> bool {
        self.result.apply(self.original)
    }
}

event_registry!(
    CAN_UNEQUIP,
    can_unequip_registry,
    CurioCanUnequipEvent,
    register_can_unequip,
    dispatch_can_unequip
);

/// Whether a slot change replaced the item or only its state (Java
/// `CurioChangeEvent.State` vs `Item`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Same item, changed render/active state.
    State,
    /// A different item or an empty slot.
    Item,
}

/// `CurioChangeEvent`, emitted after a server-side slot delta is detected.
#[derive(Clone, Debug, PartialEq)]
pub struct CurioChangeEvent {
    /// Owning entity id.
    pub entity: u32,
    /// Slot type id.
    pub slot: String,
    /// Slot index.
    pub index: usize,
    /// Previous item, if any.
    pub previous: Option<ItemSnapshot>,
    /// Current item, if any.
    pub current: Option<ItemSnapshot>,
    /// Kind of change.
    pub kind: ChangeKind,
}

event_registry!(
    CHANGE,
    change_registry,
    CurioChangeEvent,
    register_change,
    dispatch_change
);

/// `CurioDropsEvent`: listeners may edit `drops` or cancel the batch.
#[derive(Clone, Debug, PartialEq)]
pub struct CurioDropsEvent {
    /// Owning entity id.
    pub entity: u32,
    /// Items about to be spawned as drops.
    pub drops: Vec<ItemSnapshot>,
    /// Set by a listener to suppress all drops.
    pub cancelled: bool,
}

event_registry!(
    DROPS,
    drops_registry,
    CurioDropsEvent,
    register_drops,
    dispatch_drops
);

/// Production entry point for the death-drop batch: dispatches
/// `CurioDropsEvent` and returns the drops a listener left in place.
///
/// A cancelled event returns no drops; the caller owns spawning them.
pub fn dispatch_death_drops(entity: u32, drops: Vec<ItemSnapshot>) -> Vec<ItemSnapshot> {
    let mut event = CurioDropsEvent {
        entity,
        drops,
        cancelled: false,
    };
    dispatch_drops(&mut event);
    if event.cancelled {
        Vec::new()
    } else {
        event.drops
    }
}

/// One `DropRulesEvent` override, applied by canonical item id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropOverride {
    /// Item id the predicate accepts.
    pub item_id: String,
    /// Rule applied when the id matches.
    pub rule: DropRule,
}

impl ItemPredicate<ItemSnapshot> for DropOverride {
    fn matches(&self, item: &ItemSnapshot) -> bool {
        item.item_id().as_deref() == Some(self.item_id.as_str())
    }
}

/// `DropRulesEvent`: overrides are appended in listener order, so the last
/// matching one wins, exactly as in Java `handleDrops`.
#[derive(Clone, Debug, PartialEq)]
pub struct DropRulesEvent {
    /// Owning entity id.
    pub entity: u32,
    /// Overrides in registration order.
    pub overrides: Vec<DropOverride>,
}

impl DropRulesEvent {
    /// Adds an override rule for `item_id`.
    pub fn add_override(&mut self, item_id: impl Into<String>, rule: DropRule) {
        self.overrides.push(DropOverride {
            item_id: item_id.into(),
            rule,
        });
    }

    /// Builds the `DropRules` consumed by `equip::decide_death_drop`.
    pub fn drop_rules(&self) -> DropRules<DropOverride> {
        let mut rules = DropRules::new();
        for entry in &self.overrides {
            rules.add_override(entry.clone(), entry.rule);
        }
        rules
    }
}

event_registry!(
    DROP_RULES,
    drop_rules_registry,
    DropRulesEvent,
    register_drop_rules,
    dispatch_drop_rules
);

/// `SlotModifiersUpdatedEvent`, posted when a dynamic resize happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotModifiersUpdatedEvent {
    /// Owning entity id.
    pub entity: u32,
    /// Slot types whose size changed.
    pub slots: Vec<String>,
}

event_registry!(
    SLOT_MODIFIERS_UPDATED,
    slot_modifiers_registry,
    SlotModifiersUpdatedEvent,
    register_slot_modifiers_updated,
    dispatch_slot_modifiers_updated
);

/// Helper for `equip::can_equip` callers: dispatches listeners and returns the
/// final `TriState` override.
pub fn can_equip_override(event: &mut CurioCanEquipEvent) -> TriState {
    dispatch_can_equip(event);
    event.result
}

/// Helper for `equip::can_unequip` callers.
pub fn can_unequip_override(event: &mut CurioCanUnequipEvent) -> TriState {
    dispatch_can_unequip(event);
    event.result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(id: &str) -> SlotContext {
        SlotContext::new(id, Some(1), 0, false, true)
    }

    #[test]
    fn equip_listener_can_override_tri_state() {
        let mut event = CurioCanEquipEvent::new("curios:ring", context("ring"), false);
        assert!(!event.resolve());
        register_can_equip(Arc::new(|event| event.result = TriState::True)).unwrap();
        can_equip_override(&mut event);
        assert_eq!(event.result, TriState::True);
        assert!(event.resolve());
    }

    #[test]
    fn unequip_listener_can_veto() {
        register_can_unequip(Arc::new(|event| event.result = TriState::False)).unwrap();
        let mut event = CurioCanUnequipEvent::new("curios:ring", context("ring"), true);
        can_unequip_override(&mut event);
        assert!(!event.resolve());
    }

    #[test]
    fn change_and_drop_events_are_mutable() {
        register_change(Arc::new(|event| event.kind = ChangeKind::Item)).unwrap();
        let mut change = CurioChangeEvent {
            entity: 7,
            slot: "ring".to_owned(),
            index: 0,
            previous: None,
            current: None,
            kind: ChangeKind::State,
        };
        dispatch_change(&mut change);
        assert_eq!(change.kind, ChangeKind::Item);

        register_drops(Arc::new(|event| event.cancelled = true)).unwrap();
        let mut drops = CurioDropsEvent {
            entity: 7,
            drops: Vec::new(),
            cancelled: false,
        };
        dispatch_drops(&mut drops);
        assert!(drops.cancelled);
    }

    #[test]
    fn death_drop_batch_can_be_cancelled_or_passed_through() {
        register_drops(Arc::new(|event| event.cancelled = true)).unwrap();
        let cancelled = dispatch_death_drops(7, Vec::new());
        assert!(cancelled.is_empty());

        let passed = dispatch_death_drops(7, Vec::new());
        assert!(passed.is_empty());
    }

    #[test]
    fn drop_rules_collect_overrides_in_order() {
        register_drop_rules(Arc::new(|event| {
            event.add_override("curios:ring", DropRule::AlwaysKeep)
        }))
        .unwrap();
        let mut event = DropRulesEvent {
            entity: 7,
            overrides: Vec::new(),
        };
        dispatch_drop_rules(&mut event);
        assert_eq!(event.overrides.len(), 1);
        assert_eq!(event.overrides[0].rule, DropRule::AlwaysKeep);
        assert!(!event.drop_rules().is_empty());
    }

    #[test]
    fn slot_modifiers_event_reaches_listeners() {
        register_slot_modifiers_updated(Arc::new(|event| event.slots.push("ring".to_owned())))
            .unwrap();
        let mut event = SlotModifiersUpdatedEvent {
            entity: 7,
            slots: Vec::new(),
        };
        dispatch_slot_modifiers_updated(&mut event);
        assert_eq!(event.slots, ["ring"]);
    }
}
