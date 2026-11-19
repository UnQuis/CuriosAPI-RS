//! Pure drop-rule domain for Curio slots.
//!
//! Ported from `top.theillusivec4.curios.api.common.DropRule`,
//! `api.event.DropRulesEvent` and `common.CuriosCommonEvents#handleDrops` of
//! TheIllusiveC4/Curios (LGPL-3.0-or-later). No Horizon API is used here: the
//! caller feeds item-shaped values through [`ItemPredicate`] and applies the
//! resulting [`DropOutcome`] to its own inventory.
//!
//! Reference behaviour (Java `handleDrops`):
//! 1. every matching `DropRulesEvent` override replaces the previous one, so
//!    the *last* matching override wins;
//! 2. if no override matched, the item's own drop rule applies;
//! 3. a resolved `DEFAULT` defers to the slot type's drop rule;
//! 4. `DEFAULT` with keep-inventory and `ALWAYS_KEEP` keep the item; `DESTROY`
//!    and enchanted `prevent equipment drop` remove it without a drop entity;
//!    everything else is dropped. The slot is cleared in all but the keep case.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Determines the behaviour when dropping items from curio slots as loot.
///
/// Serialized names match the Java `StringRepresentable` codec:
/// `default`, `always_drop`, `always_keep`, `destroy`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropRule {
    /// Defer to the slot type's rule, then to keep-inventory.
    Default,
    /// Drop regardless of keep-inventory.
    AlwaysDrop,
    /// Keep the item in its slot regardless of keep-inventory.
    AlwaysKeep,
    /// Clear the slot without spawning a drop.
    Destroy,
}

impl Default for DropRule {
    fn default() -> Self {
        Self::Default
    }
}

impl DropRule {
    /// All variants in Java enum declaration order.
    pub const ALL: [Self; 4] = [
        Self::Default,
        Self::AlwaysDrop,
        Self::AlwaysKeep,
        Self::Destroy,
    ];

    /// Lowercase serialized name used by the Java codec.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AlwaysDrop => "always_drop",
            Self::AlwaysKeep => "always_keep",
            Self::Destroy => "destroy",
        }
    }
}

impl fmt::Display for DropRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned for a drop rule name that is not a Java serialized name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidDropRule(pub String);

impl fmt::Display for InvalidDropRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown drop rule `{}` (expected one of: default, always_drop, always_keep, destroy)",
            self.0
        )
    }
}

impl Error for InvalidDropRule {}

impl FromStr for DropRule {
    type Err = InvalidDropRule;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(Self::Default),
            "always_drop" => Ok(Self::AlwaysDrop),
            "always_keep" => Ok(Self::AlwaysKeep),
            "destroy" => Ok(Self::Destroy),
            _ => Err(InvalidDropRule(s.to_owned())),
        }
    }
}

/// A domain-level item predicate. closures over any item-shaped type implement
/// it, so the inventory layer can supply its own item representation.
pub trait ItemPredicate<I> {
    /// Returns whether this predicate selects `item`.
    fn matches(&self, item: &I) -> bool;
}

impl<I, F> ItemPredicate<I> for F
where
    F: Fn(&I) -> bool,
{
    fn matches(&self, item: &I) -> bool {
        self(item)
    }
}

/// One `DropRulesEvent` override: a predicate paired with its drop rule.
#[derive(Debug)]
pub struct DropOverride<P> {
    /// Predicate applied to each item by the event dispatcher.
    pub predicate: P,
    /// Rule to apply when the predicate matches.
    pub rule: DropRule,
}

impl<P> DropOverride<P> {
    /// Creates a new override.
    pub const fn new(predicate: P, rule: DropRule) -> Self {
        Self { predicate, rule }
    }
}

/// Ordered collection of overrides from `DropRulesEvent`.
///
/// Insertion order is preserved because the Java implementation iterates the
/// list and keeps the last matching rule.
#[derive(Debug, Default)]
pub struct DropRules<P> {
    overrides: Vec<DropOverride<P>>,
}

impl<P> DropRules<P> {
    /// Creates an empty rule set.
    pub fn new() -> Self {
        Self {
            overrides: Vec::new(),
        }
    }

    /// Appends an override; later overrides take precedence over earlier ones.
    pub fn add_override(&mut self, predicate: P, rule: DropRule) {
        self.overrides.push(DropOverride::new(predicate, rule));
    }

    /// Overrides in insertion order.
    pub fn overrides(&self) -> &[DropOverride<P>] {
        &self.overrides
    }

    /// Whether no overrides were registered.
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    /// Removes all overrides.
    pub fn clear(&mut self) {
        self.overrides.clear();
    }

    /// Resolves the effective rule for `item`.
    pub fn resolve<I>(&self, item: &I, item_rule: DropRule, slot_rule: DropRule) -> DropRule
    where
        P: ItemPredicate<I>,
    {
        resolve_drop_rule(&self.overrides, item, item_rule, slot_rule)
    }
}

/// Resolves overrides, item rule and slot rule exactly like Java `handleDrops`.
///
/// The last matching override wins; an explicit `Default` override still
/// defers to the slot rule.
pub fn resolve_drop_rule<I, P>(
    overrides: &[DropOverride<P>],
    item: &I,
    item_rule: DropRule,
    slot_rule: DropRule,
) -> DropRule
where
    P: ItemPredicate<I>,
{
    let mut chosen = None;
    for entry in overrides {
        if entry.predicate.matches(item) {
            // Java iterates the whole list and overwrites, so last match wins.
            chosen = Some(entry.rule);
        }
    }
    let rule = chosen.unwrap_or(item_rule);
    if rule == DropRule::Default {
        slot_rule
    } else {
        rule
    }
}

/// Final disposition of a curio item, mirroring the Java `handleDrops` branch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DropOutcome {
    /// Item stays in its slot (keep-inventory or `ALWAYS_KEEP`).
    Keep,
    /// Item is removed from the slot and added to the drop collection.
    Drop,
    /// Slot is cleared but no drop entity is spawned (`DESTROY` or
    /// `prevent equipment drop` enchantment on a would-be drop).
    Destroy,
}

/// Computes the final outcome for a resolved rule.
///
/// `drop_prevented` stands for the Java
/// `EnchantmentEffectComponents.PREVENT_EQUIPMENT_DROP` check.
pub const fn drop_outcome(
    rule: DropRule,
    keep_inventory: bool,
    drop_prevented: bool,
) -> DropOutcome {
    match rule {
        DropRule::AlwaysKeep => DropOutcome::Keep,
        DropRule::Default if keep_inventory => DropOutcome::Keep,
        DropRule::Destroy => DropOutcome::Destroy,
        DropRule::Default | DropRule::AlwaysDrop => {
            if drop_prevented {
                DropOutcome::Destroy
            } else {
                DropOutcome::Drop
            }
        }
    }
}

/// Rule plus its final disposition, for callers that dispatch drops.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DropDecision {
    /// Effective rule after override/item/slot precedence.
    pub rule: DropRule,
    /// What the inventory layer should do with the item.
    pub outcome: DropOutcome,
}

/// Resolves and decides in one step (`DropRulesEvent` + `CurioDropsEvent`).
pub fn decide_drop<I, P>(
    rules: &DropRules<P>,
    item: &I,
    item_rule: DropRule,
    slot_rule: DropRule,
    keep_inventory: bool,
    drop_prevented: bool,
) -> DropDecision
where
    P: ItemPredicate<I>,
{
    let rule = rules.resolve(item, item_rule, slot_rule);
    DropDecision {
        rule,
        outcome: drop_outcome(rule, keep_inventory, drop_prevented),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniform predicate type so multiple overrides share one `DropRules<P>`.
    #[derive(Clone, Copy)]
    struct NamedItem(&'static str);

    impl ItemPredicate<&str> for NamedItem {
        fn matches(&self, item: &&str) -> bool {
            *item == self.0
        }
    }

    #[test]
    fn rule_serialized_names_match_java() {
        assert_eq!(DropRule::Default.as_str(), "default");
        assert_eq!(DropRule::AlwaysDrop.as_str(), "always_drop");
        assert_eq!(DropRule::AlwaysKeep.as_str(), "always_keep");
        assert_eq!(DropRule::Destroy.as_str(), "destroy");
        assert_eq!(serde_json::to_string(&DropRule::AlwaysDrop).unwrap(), "\"always_drop\"");
        assert_eq!(serde_json::from_str::<DropRule>("\"destroy\"").unwrap(), DropRule::Destroy);
        assert_eq!("always_keep".parse::<DropRule>(), Ok(DropRule::AlwaysKeep));
        assert!("ALWAYS_DROP".parse::<DropRule>().is_err());
        assert_eq!(
            "unknown".parse::<DropRule>(),
            Err(InvalidDropRule("unknown".to_owned()))
        );
    }

    #[test]
    fn last_matching_override_wins() {
        let mut rules = DropRules::new();
        rules.add_override(NamedItem("ring"), DropRule::AlwaysKeep);
        rules.add_override(NamedItem("ring"), DropRule::AlwaysDrop);
        rules.add_override(NamedItem("necklace"), DropRule::Destroy);
        assert_eq!(
            rules.resolve(&"ring", DropRule::Default, DropRule::Destroy),
            DropRule::AlwaysDrop
        );
        assert_eq!(
            rules.resolve(&"necklace", DropRule::AlwaysKeep, DropRule::Default),
            DropRule::Destroy
        );
        assert_eq!(rules.overrides().len(), 3);
        assert_eq!(rules.overrides()[0].rule, DropRule::AlwaysKeep);
    }

    #[test]
    fn default_override_defers_to_slot_rule() {
        let rules: DropRules<NamedItem> = DropRules::new();
        assert_eq!(
            rules.resolve(&"ring", DropRule::AlwaysKeep, DropRule::Destroy),
            DropRule::AlwaysKeep
        );

        let mut explicit = DropRules::new();
        explicit.add_override(|_: &&str| true, DropRule::Default);
        assert_eq!(
            explicit.resolve(&"ring", DropRule::AlwaysDrop, DropRule::AlwaysKeep),
            DropRule::AlwaysKeep
        );
    }

    #[test]
    fn no_matching_override_uses_item_rule_then_slot_rule() {
        let mut rules = DropRules::new();
        rules.add_override(NamedItem("charm"), DropRule::Destroy);
        assert_eq!(
            rules.resolve(&"ring", DropRule::AlwaysDrop, DropRule::AlwaysKeep),
            DropRule::AlwaysDrop
        );
        assert_eq!(
            rules.resolve(&"ring", DropRule::Default, DropRule::Destroy),
            DropRule::Destroy
        );
    }

    #[test]
    fn outcome_matrix_matches_handle_drops() {
        assert_eq!(drop_outcome(DropRule::AlwaysKeep, false, false), DropOutcome::Keep);
        assert_eq!(drop_outcome(DropRule::AlwaysKeep, true, true), DropOutcome::Keep);
        assert_eq!(drop_outcome(DropRule::Default, true, false), DropOutcome::Keep);
        assert_eq!(drop_outcome(DropRule::Default, false, false), DropOutcome::Drop);
        // keep-inventory is checked before the enchantment in Java handleDrops.
        assert_eq!(drop_outcome(DropRule::Default, true, true), DropOutcome::Keep);
        assert_eq!(drop_outcome(DropRule::AlwaysDrop, true, false), DropOutcome::Drop);
        assert_eq!(drop_outcome(DropRule::AlwaysDrop, false, true), DropOutcome::Destroy);
        assert_eq!(drop_outcome(DropRule::Destroy, false, false), DropOutcome::Destroy);
    }

    #[test]
    fn decide_drop_combines_all_stages() {
        let mut rules = DropRules::new();
        rules.add_override(NamedItem("ring"), DropRule::Destroy);
        let decision = decide_drop(&rules, &"ring", DropRule::AlwaysKeep, DropRule::AlwaysDrop, true, false);
        assert_eq!(decision.rule, DropRule::Destroy);
        assert_eq!(decision.outcome, DropOutcome::Destroy);

        rules.clear();
        assert!(rules.is_empty());
        let decision = decide_drop(&rules, &"ring", DropRule::Default, DropRule::AlwaysKeep, false, false);
        assert_eq!(decision.rule, DropRule::AlwaysKeep);
        assert_eq!(decision.outcome, DropOutcome::Keep);
    }
}
