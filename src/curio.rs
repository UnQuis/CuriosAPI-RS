//! Item-side Curio registration, the Rust counterpart of `CuriosApi.registerCurio`
//! and the `ICurio`/`ICurioItem` capabilities (LGPL-3.0-or-later reference:
//! TheIllusiveC4/Curios, copyright 2018-2024 C4).
//!
//! Other mods register a [`CurioBehavior`] for an item id. The registry is
//! process-wide and read-only after startup registration; gameplay hooks read
//! it by canonical item id. No Horizon API is called here so registration
//! remains unit-testable.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use crate::drop::DropRule;

static BEHAVIORS: OnceLock<Mutex<BTreeMap<String, Arc<dyn CurioBehavior>>>> = OnceLock::new();

fn behaviors() -> &'static Mutex<BTreeMap<String, Arc<dyn CurioBehavior>>> {
    BEHAVIORS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The slot an item is being equipped into or removed from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurioContext<'a> {
    /// Canonical slot type id, e.g. `curios:ring`.
    pub slot: &'a str,
    /// Zero-based index inside that slot type.
    pub index: u32,
    /// Canonical item id, e.g. `curios:amulet`.
    pub item: &'a str,
}

/// Behaviour supplied by an item's owner for Curios slots.
///
/// Defaults mirror `ICurio`: an item is equippable anywhere, carries no extra
/// attribute modifiers and follows the slot's drop rule.
pub trait CurioBehavior: Send + Sync + 'static {
    /// Slot type ids this item accepts. Empty means every slot type.
    fn supported_slots(&self) -> &[String] {
        &[]
    }

    /// `ICurio#canEquip` / `ICurioItem#canEquip`.
    fn can_equip(&self, ctx: &CurioContext<'_>) -> bool {
        let _ = ctx;
        true
    }

    /// `ICurio#canUnequip`.
    fn can_unequip(&self, ctx: &CurioContext<'_>) -> bool {
        let _ = ctx;
        true
    }

    /// `ICurio#getDropRule`.
    fn drop_rule(&self) -> DropRule {
        DropRule::Default
    }

    /// Called after a successful equip, server side.
    fn on_equip(&self, ctx: &CurioContext<'_>) {
        let _ = ctx;
    }

    /// Called after a successful unequip, server side.
    fn on_unequip(&self, ctx: &CurioContext<'_>) {
        let _ = ctx;
    }
}

/// A registration refusal: either a malformed id or a duplicate registration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterError {
    /// The id is not `namespace:path` with valid identifier characters.
    InvalidId(String),
    /// Another behaviour is already registered for this item id.
    Duplicate(String),
    /// The internal registry lock was poisoned.
    Poisoned,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid item id `{id}` (expected namespace:path)"),
            Self::Duplicate(id) => write!(f, "a Curio behavior is already registered for `{id}`"),
            Self::Poisoned => f.write_str("Curio behavior registry lock is poisoned"),
        }
    }
}

impl Error for RegisterError {}

/// Validates the same identifier shape the game uses for registry names.
pub fn valid_item_id(id: &str) -> bool {
    let Some((namespace, path)) = id.split_once(':') else {
        return false;
    };
    if namespace.is_empty() || path.is_empty() || path.contains(':') {
        return false;
    }
    let ok = |part: &str, slash: bool| {
        part.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'_' | b'.' | b'-')
                || (slash && b == b'/')
        })
    };
    ok(namespace, false) && ok(path, true)
}

/// Registers an item's Curio behaviour. Duplicates are refused rather than
/// silently overriding earlier mods, matching the explicit-failure rule.
pub fn register_curio(
    item_id: impl Into<String>,
    behavior: Arc<dyn CurioBehavior>,
) -> Result<(), RegisterError> {
    let id = item_id.into();
    if !valid_item_id(&id) {
        return Err(RegisterError::InvalidId(id));
    }
    let mut map = behaviors().lock().map_err(|_| RegisterError::Poisoned)?;
    if map.contains_key(&id) {
        return Err(RegisterError::Duplicate(id));
    }
    map.insert(id, behavior);
    Ok(())
}

/// Looks up an item's behaviour by canonical id.
pub fn behavior(item_id: &str) -> Option<Arc<dyn CurioBehavior>> {
    behaviors()
        .lock()
        .ok()?
        .get(item_id)
        .cloned()
}

/// Whether an item accepts a slot, honoring an explicit supported-slot list.
pub fn accepts_slot(behavior: &dyn CurioBehavior, slot: &str) -> bool {
    let supported = behavior.supported_slots();
    supported.is_empty() || supported.iter().any(|id| id == slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ring;
    impl CurioBehavior for Ring {
        fn supported_slots(&self) -> &[String] {
            &[]
        }
    }

    #[test]
    fn identifiers() {
        assert!(valid_item_id("curios:ring"));
        assert!(valid_item_id("minecraft:armor_stand"));
        assert!(!valid_item_id("ring"));
        assert!(!valid_item_id("Curios:Ring"));
        assert!(!valid_item_id("curios:"));
        assert!(!valid_item_id("curios:a:b"));
    }

    #[test]
    fn context_and_defaults() {
        let ring = Ring;
        let ctx = CurioContext {
            slot: "ring",
            index: 0,
            item: "curios:ring_item",
        };
        assert!(ring.can_equip(&ctx));
        assert!(ring.can_unequip(&ctx));
        assert_eq!(ring.drop_rule(), DropRule::Default);
        assert!(accepts_slot(&ring, "curios:ring"));
        assert!(accepts_slot(&ring, "curios:head"));
    }
}
