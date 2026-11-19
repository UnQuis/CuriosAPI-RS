//! CuriosAPI-RS settings and their Horizon config registration.
//!
//! Hook wiring lives in `lib.rs`; this module owns the shipped default text,
//! strict parsers and the process-wide values. Parsing is pure so every
//! rejection (out of range, malformed, unknown or duplicate key) is testable.
//!
//! The setting names and defaults follow the upstream Curios common, client
//! and server specs (`CuriosConfig`, `CuriosClientConfig`, LGPL-3.0-or-later,
//! copyright 2018-2024 C4); the `default_slot_count` key is this port's
//! explicit default for a slot definition's size.

use std::collections::BTreeSet;
use std::sync::Mutex;

use mod_api::{Config, ConfigFile, ConfigKind, RegisterConfigsEvent};

pub const MOD_ID: &str = "curios";

/// Shipped `config/curios-common.toml`.
pub const COMMON_DEFAULT: &str = "\
# CuriosAPI-RS common settings.\n\
# Slots granted by a definition that does not specify its own count (1..=16).\n\
default_slot_count = 1\n";

/// Shipped `config/curios-client.toml`.
pub const CLIENT_DEFAULT: &str = "\
# CuriosAPI-RS client settings (rendering only).\n\
render_curios = true\n\
enable_button = true\n";

/// Shipped `config/curios-server.toml`.
pub const SERVER_DEFAULT: &str = "\
# CuriosAPI-RS server settings (authoritative behaviour).\n\
# on: keep Curios on death, default: follow keepInventory, off: drop them.\n\
keep_curios = default\n\
minimum_columns = 1\n\
max_slots_per_page = 48\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepCurios {
    On,
    Default,
    Off,
}

impl KeepCurios {
    pub fn drops_on_death(self) -> bool {
        matches!(self, Self::Off)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotSettings {
    pub default_slot_count: u8,
}

impl Default for SlotSettings {
    fn default() -> Self {
        Self {
            default_slot_count: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSettings {
    pub render_curios: bool,
    pub enable_button: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            render_curios: true,
            enable_button: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerSettings {
    pub keep_curios: KeepCurios,
    pub minimum_columns: u8,
    pub max_slots_per_page: u8,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            keep_curios: KeepCurios::Default,
            minimum_columns: 1,
            max_slots_per_page: 48,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub slots: SlotSettings,
    pub render: RenderSettings,
    pub server: ServerSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            slots: SlotSettings::default(),
            render: RenderSettings::default(),
            server: ServerSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    InvalidLine(usize),
    UnknownKey(String),
    DuplicateKey(String),
    InvalidValue {
        line: usize,
        key: String,
        value: String,
        expected: String,
    },
}

impl ConfigError {
    pub fn message(&self) -> String {
        match self {
            Self::InvalidLine(line) => format!("malformed config line {line}"),
            Self::UnknownKey(key) => format!("unknown config key {key:?}"),
            Self::DuplicateKey(key) => format!("duplicate config key {key:?}"),
            Self::InvalidValue {
                line,
                key,
                value,
                expected,
            } => format!("config line {line}: {key} must be {expected}, got {value:?}"),
        }
    }
}

/// Non-empty, comment-free `line number = text` pairs from a config file.
fn entries(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, raw)| {
            let line = raw.split('#').next().unwrap_or("").trim();
            (!line.is_empty()).then_some((index + 1, line))
        })
}

fn invalid(line: usize, key: &str, value: &str, expected: &str) -> ConfigError {
    ConfigError::InvalidValue {
        line,
        key: key.to_owned(),
        value: value.to_owned(),
        expected: expected.to_owned(),
    }
}

fn parse_u8(line: usize, key: &str, value: &str, min: u8, max: u8) -> Result<u8, ConfigError> {
    let expected = format!("an integer in {min}..={max}");
    let parsed: u8 = value
        .parse()
        .map_err(|_| invalid(line, key, value, &expected))?;
    if !(min..=max).contains(&parsed) {
        return Err(invalid(line, key, value, &expected));
    }
    Ok(parsed)
}

fn parse_bool(line: usize, key: &str, value: &str) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(line, key, value, "true or false")),
    }
}

fn next_entry<'a>(
    entry: &'a str,
    line: usize,
    seen: &mut BTreeSet<&'a str>,
) -> Result<(&'a str, &'a str), ConfigError> {
    let (key, value) = entry
        .split_once('=')
        .ok_or(ConfigError::InvalidLine(line))?;
    let (key, value) = (key.trim(), value.trim());
    if !seen.insert(key) {
        return Err(ConfigError::DuplicateKey(key.to_owned()));
    }
    Ok((key, value))
}

pub fn parse_common(text: &str) -> Result<SlotSettings, ConfigError> {
    let mut settings = SlotSettings::default();
    let mut seen = BTreeSet::new();
    for (line, entry) in entries(text) {
        let (key, value) = next_entry(entry, line, &mut seen)?;
        match key {
            "default_slot_count" => {
                settings.default_slot_count = parse_u8(line, key, value, 1, 16)?;
            }
            _ => return Err(ConfigError::UnknownKey(key.to_owned())),
        }
    }
    Ok(settings)
}

pub fn parse_client(text: &str) -> Result<RenderSettings, ConfigError> {
    let mut settings = RenderSettings::default();
    let mut seen = BTreeSet::new();
    for (line, entry) in entries(text) {
        let (key, value) = next_entry(entry, line, &mut seen)?;
        match key {
            "render_curios" => settings.render_curios = parse_bool(line, key, value)?,
            "enable_button" => settings.enable_button = parse_bool(line, key, value)?,
            _ => return Err(ConfigError::UnknownKey(key.to_owned())),
        }
    }
    Ok(settings)
}

pub fn parse_server(text: &str) -> Result<ServerSettings, ConfigError> {
    let mut settings = ServerSettings::default();
    let mut seen = BTreeSet::new();
    for (line, entry) in entries(text) {
        let (key, value) = next_entry(entry, line, &mut seen)?;
        match key {
            "keep_curios" => {
                settings.keep_curios = match value {
                    "on" => KeepCurios::On,
                    "default" => KeepCurios::Default,
                    "off" => KeepCurios::Off,
                    _ => return Err(invalid(line, key, value, "on, default or off")),
                };
            }
            "minimum_columns" => settings.minimum_columns = parse_u8(line, key, value, 1, 8)?,
            "max_slots_per_page" => {
                settings.max_slots_per_page = parse_u8(line, key, value, 1, 48)?;
            }
            _ => return Err(ConfigError::UnknownKey(key.to_owned())),
        }
    }
    Ok(settings)
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    slots: SlotSettings {
        default_slot_count: 1,
    },
    render: RenderSettings {
        render_curios: true,
        enable_button: true,
    },
    server: ServerSettings {
        keep_curios: KeepCurios::Default,
        minimum_columns: 1,
        max_slots_per_page: 48,
    },
});

fn store(update: impl FnOnce(&mut Settings)) {
    let mut guard = SETTINGS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    update(&mut guard);
}

pub fn current() -> Settings {
    SETTINGS
        .lock()
        .map(|guard| *guard)
        .unwrap_or_else(|poisoned| *poisoned.into_inner())
}

/// Convenience accessors for gameplay hooks.
pub fn default_slot_count() -> u8 {
    current().slots.default_slot_count
}

pub fn render_curios() -> bool {
    current().render.render_curios
}

pub fn drops_on_death() -> bool {
    current().server.keep_curios.drops_on_death()
}

/// For `Hooks::register_configs`.
pub fn register(event: &mut RegisterConfigsEvent<'_>) {
    event
        .configs
        .push(Config::new(MOD_ID, ConfigKind::Common, COMMON_DEFAULT));
    event
        .configs
        .push(Config::new(MOD_ID, ConfigKind::Client, CLIENT_DEFAULT));
    event
        .configs
        .push(Config::new(MOD_ID, ConfigKind::Server, SERVER_DEFAULT));
}

fn report(config: &ConfigFile, error: ConfigError) {
    eprintln!(
        "[curios] ignoring invalid {}: {}; keeping the current value",
        config.file,
        error.message()
    );
}

/// For both `Hooks::config_loading` and `Hooks::config_reloading`.
pub fn loading(config: &ConfigFile) {
    if config.mod_id != MOD_ID {
        return;
    }
    match config.kind {
        ConfigKind::Common => match parse_common(&config.text) {
            Ok(slots) => store(|settings| settings.slots = slots),
            Err(error) => report(config, error),
        },
        ConfigKind::Client => match parse_client(&config.text) {
            Ok(render) => store(|settings| settings.render = render),
            Err(error) => report(config, error),
        },
        ConfigKind::Server => match parse_server(&config.text) {
            Ok(server) => store(|settings| settings.server = server),
            Err(error) => report(config, error),
        },
        other => eprintln!(
            "[curios] unexpected config kind {other:?} for {}; ignoring",
            config.file
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_defaults_parse_to_the_reference_values() {
        assert_eq!(parse_common(COMMON_DEFAULT).unwrap(), SlotSettings::default());
        assert_eq!(parse_client(CLIENT_DEFAULT).unwrap(), RenderSettings::default());
        assert_eq!(parse_server(SERVER_DEFAULT).unwrap(), ServerSettings::default());
    }

    #[test]
    fn missing_keys_keep_defaults() {
        assert_eq!(parse_common("").unwrap(), SlotSettings::default());
        assert_eq!(parse_client("").unwrap(), RenderSettings::default());
        assert_eq!(parse_server("").unwrap(), ServerSettings::default());
    }

    #[test]
    fn comments_and_whitespace_are_allowed() {
        let text = "  # note\nrender_curios = false # inline\n  enable_button = false\n";
        assert_eq!(
            parse_client(text).unwrap(),
            RenderSettings {
                render_curios: false,
                enable_button: false
            }
        );
    }

    #[test]
    fn slot_count_is_bounded_and_refused_when_invalid() {
        assert_eq!(
            parse_common("default_slot_count = 16").unwrap(),
            SlotSettings {
                default_slot_count: 16
            }
        );
        for value in ["0", "17", "-1", "1.5", "many"] {
            let text = format!("default_slot_count = {value}");
            assert!(parse_common(&text).is_err(), "{value}");
        }
    }

    #[test]
    fn menu_dimensions_are_bounded_like_upstream() {
        let text = "minimum_columns = 8\nmax_slots_per_page = 48\n";
        assert_eq!(
            parse_server(text).unwrap(),
            ServerSettings {
                minimum_columns: 8,
                ..ServerSettings::default()
            }
        );
        assert!(parse_server("minimum_columns = 9").is_err());
        assert!(parse_server("max_slots_per_page = 49").is_err());
    }

    #[test]
    fn keep_curios_controls_drop_behaviour() {
        assert_eq!(
            parse_server("keep_curios = on").unwrap().keep_curios,
            KeepCurios::On
        );
        assert_eq!(
            parse_server("keep_curios = off").unwrap().keep_curios,
            KeepCurios::Off
        );
        assert!(parse_server("keep_curios = ON").is_err());
        assert!(KeepCurios::Off.drops_on_death());
        assert!(!KeepCurios::Default.drops_on_death());
        assert!(!KeepCurios::On.drops_on_death());
    }

    #[test]
    fn unknown_duplicate_and_malformed_keys_are_refused() {
        assert!(matches!(
            parse_common("foo = 1"),
            Err(ConfigError::UnknownKey(_))
        ));
        assert!(matches!(
            parse_client("render_curios = true\nrender_curios = false"),
            Err(ConfigError::DuplicateKey(_))
        ));
        assert!(matches!(
            parse_server("no equals sign"),
            Err(ConfigError::InvalidLine(1))
        ));
    }
}
