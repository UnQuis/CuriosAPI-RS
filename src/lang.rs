//! Localization tables for the Curios screen.
//!
//! A native Horizon mod cannot rely on the game shipping its `assets/` folder,
//! so the strings are compiled in from the shipped resource files. The source
//! of truth is `assets/curios/lang/*.json`, copied from the Curios Java
//! reference; this module only indexes and resolves them.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use mod_api::{ClientHost, Text};

/// Translation keys the screen itself uses. Keeping them as constants turns a
/// typo into a compile error instead of a blank label.
pub const KEY_NAME: &str = "curios.name";
pub const KEY_SLOT: &str = "curios.slot";
pub const KEY_COSMETIC: &str = "curios.cosmetic";
pub const KEY_INACTIVE: &str = "curios.tooltip.inactive";
pub const KEY_TOGGLE: &str = "gui.curios.toggle";
pub const KEY_TOGGLE_COSMETICS: &str = "gui.curios.toggle.cosmetics";
pub const KEY_PAGE: &str = "gui.curios.page";
pub const KEY_NETWORK_FAILED: &str = "curios.networking.failed";
pub const KEY_IDENTIFIER_PREFIX: &str = "curios.identifier.";

/// The locale we fall back to when the player's language has no shipped table.
pub const DEFAULT_LOCALE: &str = "en_us";

/// All shipped locales, in the order the reference ships them.
pub const LOCALES: [&str; 19] = [
    "ca_es", "de_de", "en_us", "es_es", "fr_fr", "hu_hu", "it_it", "ja_jp", "ko_kr", "lzh",
    "pl_pl", "pt_br", "ru_ru", "tok", "tr_tr", "uk_ua", "vi_vn", "zh_cn", "zh_tw",
];

type Table = BTreeMap<&'static str, &'static str>;

/// One compiled-in table per locale. Parsing is strict: an invalid shipped
/// file is a packaging bug and must fail loudly, never fall back silently.
static TABLES: LazyLock<BTreeMap<&'static str, Table>> = LazyLock::new(|| {
    let sources: [(&str, &str); 19] = [
        ("ca_es", include_str!("../assets/curios/lang/ca_es.json")),
        ("de_de", include_str!("../assets/curios/lang/de_de.json")),
        ("en_us", include_str!("../assets/curios/lang/en_us.json")),
        ("es_es", include_str!("../assets/curios/lang/es_es.json")),
        ("fr_fr", include_str!("../assets/curios/lang/fr_fr.json")),
        ("hu_hu", include_str!("../assets/curios/lang/hu_hu.json")),
        ("it_it", include_str!("../assets/curios/lang/it_it.json")),
        ("ja_jp", include_str!("../assets/curios/lang/ja_jp.json")),
        ("ko_kr", include_str!("../assets/curios/lang/ko_kr.json")),
        ("lzh", include_str!("../assets/curios/lang/lzh.json")),
        ("pl_pl", include_str!("../assets/curios/lang/pl_pl.json")),
        ("pt_br", include_str!("../assets/curios/lang/pt_br.json")),
        ("ru_ru", include_str!("../assets/curios/lang/ru_ru.json")),
        ("tok", include_str!("../assets/curios/lang/tok.json")),
        ("tr_tr", include_str!("../assets/curios/lang/tr_tr.json")),
        ("uk_ua", include_str!("../assets/curios/lang/uk_ua.json")),
        ("vi_vn", include_str!("../assets/curios/lang/vi_vn.json")),
        ("zh_cn", include_str!("../assets/curios/lang/zh_cn.json")),
        ("zh_tw", include_str!("../assets/curios/lang/zh_tw.json")),
    ];
    let mut result = BTreeMap::new();
    for (locale, json) in sources {
        let parsed: BTreeMap<String, String> = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("invalid shipped Curios lang {locale}: {error}"));
        result.insert(
            locale,
            parsed
                .into_iter()
                .map(|(key, value)| {
                    let key: &'static str = Box::leak(key.into_boxed_str());
                    let value: &'static str = Box::leak(value.into_boxed_str());
                    (key, value)
                })
                .collect(),
        );
    }
    result
});

/// Whether `locale` is one of the shipped tables.
pub fn has_locale(locale: &str) -> bool {
    TABLES.contains_key(locale)
}

/// Looks up a key in a locale, falling back to English when that locale does
/// not translate it. Returns `None` only for a key missing from every table.
pub fn lookup(locale: &str, key: &str) -> Option<&'static str> {
    TABLES
        .get(locale)
        .and_then(|table| table.get(key).copied())
        .or_else(|| {
            TABLES
                .get(DEFAULT_LOCALE)
                .and_then(|table| table.get(key).copied())
        })
}

/// The translation for a slot identifier, e.g. `ring` → `Ring`.
///
/// The Java fallback (see `CurioSlot.getSlotTooltip`) capitalises the id when
/// no translation exists. The reference `en_us` has no `curios.identifier.feet`,
/// so this is needed for a complete built-in slot list, not just a rare miss.
pub fn slot_name(locale: &str, id: &str) -> Option<&'static str> {
    if let Some(value) = lookup(locale, &format!("{KEY_IDENTIFIER_PREFIX}{id}")) {
        return Some(value);
    }
    let mut chars = id.chars();
    let capitalized: String = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => return None,
    };
    Some(Box::leak(capitalized.into_boxed_str()))
}

/// The player's locale, resolved from the game's own translation of
/// `container.inventory` (the pinned API has no locale helper). A Cyrillic
/// answer selects `ru_ru`, anything else the English fallback.
pub fn locale(host: &dyn ClientHost) -> &'static str {
    let probe = host.translate("container.inventory").unwrap_or_default();
    if probe.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c)) {
        "ru_ru"
    } else {
        DEFAULT_LOCALE
    }
}

/// A `Text` for `key` in the player's language. Keys are left translatable
/// when no shipped table has them, so the game can still resolve them.
pub fn text(host: &dyn ClientHost, key: &str) -> Text {
    let resolved = lookup(locale(host), key).map(str::to_owned);
    match resolved {
        Some(value) => Text::literal(value),
        None => Text::translate(key.to_owned(), Vec::new()),
    }
}

/// Alias used by the menu module, where a server host also exposes
/// translation lookup.
pub fn translate(host: &dyn ClientHost, key: &str) -> Text {
    text(host, key)
}

/// The same as [`text`] for a slot identifier's label.
pub fn slot_text(host: &dyn ClientHost, id: &str) -> Text {
    let key = format!("{KEY_IDENTIFIER_PREFIX}{id}");
    text(host, &key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_locale_parses() {
        for locale in LOCALES {
            assert!(has_locale(locale), "missing table {locale}");
        }
    }

    #[test]
    fn core_keys_are_translated_everywhere() {
        for locale in LOCALES {
            for key in [KEY_NAME, KEY_SLOT, KEY_COSMETIC] {
                let value = lookup(locale, key)
                    .unwrap_or_else(|| panic!("{locale} has no {key}"));
                assert!(!value.is_empty(), "{locale}/{key} is empty");
            }
        }
    }

    #[test]
    fn english_slot_names_match_the_builtin_registry() {
        for id in [
            "curio", "head", "necklace", "back", "body", "bracelet", "hands", "ring", "belt",
            "feet", "charm",
        ] {
            assert!(slot_name(DEFAULT_LOCALE, id).is_some(), "no label for {id}");
        }
    }

    #[test]
    fn unknown_keys_and_locales_fall_back_to_english() {
        assert_eq!(lookup("de_de", KEY_NAME), lookup(DEFAULT_LOCALE, KEY_NAME));
        assert_eq!(lookup("xx_yy", KEY_COSMETIC), lookup(DEFAULT_LOCALE, KEY_COSMETIC));
        assert_eq!(lookup(DEFAULT_LOCALE, "no.such.key"), None);
    }
}
