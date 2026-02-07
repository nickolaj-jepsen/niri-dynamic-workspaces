use gdk4::{Key, ModifierType};
use serde::Deserialize;

// --- Serde structs (TOML representation) ---

#[derive(Default, Deserialize)]
#[serde(default)]
struct Config {
    general: GeneralConfig,
    layout: LayoutConfig,
    keybinds: KeybindsConfig,
}

#[derive(Deserialize)]
#[serde(default)]
struct GeneralConfig {
    workspace_prefix: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_prefix: "dyn-".to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct LayoutConfig {
    max_columns: u32,
    min_columns: u32,
    max_windows_per_card: usize,
    app_name_max_chars: i32,
    window_title_max_chars: i32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            max_columns: 4,
            min_columns: 2,
            max_windows_per_card: 4,
            app_name_max_chars: 12,
            window_title_max_chars: 18,
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct KeybindsConfig {
    close: Vec<String>,
    delete_modifier: String,
}

impl Default for KeybindsConfig {
    fn default() -> Self {
        Self {
            close: vec![
                "Escape".to_string(),
                "Ctrl+c".to_string(),
                "Ctrl+w".to_string(),
                "Ctrl+q".to_string(),
            ],
            delete_modifier: "Shift".to_string(),
        }
    }
}

// --- Runtime structs ---

pub struct ResolvedConfig {
    pub workspace_prefix: String,
    pub max_columns: u32,
    pub min_columns: u32,
    pub max_windows_per_card: usize,
    pub app_name_max_chars: i32,
    pub window_title_max_chars: i32,
    pub close_keybinds: Vec<Keybind>,
    pub delete_modifier: ModifierType,
}

#[derive(Clone, Debug)]
pub struct Keybind {
    pub modifiers: ModifierType,
    pub key: Key,
}

// --- Parsing ---

fn parse_modifier(name: &str) -> Option<ModifierType> {
    match name {
        "Ctrl" | "Control" => Some(ModifierType::CONTROL_MASK),
        "Shift" => Some(ModifierType::SHIFT_MASK),
        "Alt" | "Mod1" => Some(ModifierType::ALT_MASK),
        "Super" | "Mod4" => Some(ModifierType::SUPER_MASK),
        _ => None,
    }
}

fn parse_keybind(s: &str) -> Result<Keybind, String> {
    let parts: Vec<&str> = s.split('+').collect();
    let (modifier_parts, key_name) = parts.split_at(parts.len() - 1);
    let key_name = key_name[0].trim();

    let mut modifiers = ModifierType::empty();
    for part in modifier_parts {
        let part = part.trim();
        modifiers |= parse_modifier(part).ok_or_else(|| format!("unknown modifier '{part}'"))?;
    }

    let key = Key::from_name(key_name).ok_or_else(|| format!("unknown key name '{key_name}'"))?;

    Ok(Keybind { modifiers, key })
}

impl Config {
    fn resolve(self) -> (ResolvedConfig, Vec<String>) {
        let mut warnings = Vec::new();

        let mut close_keybinds = Vec::new();
        for s in &self.keybinds.close {
            match parse_keybind(s) {
                Ok(kb) => close_keybinds.push(kb),
                Err(e) => warnings.push(format!("ignoring close keybind '{s}': {e}")),
            }
        }

        let delete_modifier = if self.keybinds.delete_modifier.is_empty() {
            ModifierType::empty()
        } else if let Some(m) = parse_modifier(&self.keybinds.delete_modifier) {
            m
        } else {
            warnings.push(format!(
                "invalid delete_modifier '{}', using Shift",
                self.keybinds.delete_modifier
            ));
            ModifierType::SHIFT_MASK
        };

        let resolved = ResolvedConfig {
            workspace_prefix: self.general.workspace_prefix,
            max_columns: self.layout.max_columns,
            min_columns: self.layout.min_columns,
            max_windows_per_card: self.layout.max_windows_per_card,
            app_name_max_chars: self.layout.app_name_max_chars,
            window_title_max_chars: self.layout.window_title_max_chars,
            close_keybinds,
            delete_modifier,
        };

        (resolved, warnings)
    }
}

// --- Public API ---

pub fn load_config() -> ResolvedConfig {
    let config_path = if let Some(dir) = dirs::config_dir() {
        dir.join("niri-dynamic-workspaces").join("config.toml")
    } else {
        eprintln!("warning: could not determine config directory, using defaults");
        return Config::default().resolve().0;
    };

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Config::default().resolve().0;
        }
        Err(e) => {
            eprintln!(
                "warning: could not read {}: {e}, using defaults",
                config_path.display()
            );
            return Config::default().resolve().0;
        }
    };

    let config: Config = match toml::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "warning: could not parse {}: {e}, using defaults",
                config_path.display()
            );
            return Config::default().resolve().0;
        }
    };

    let (resolved, warnings) = config.resolve();
    for w in &warnings {
        eprintln!("config warning: {w}");
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_modifier ---

    #[test]
    fn parse_modifier_valid_names() {
        assert_eq!(parse_modifier("Ctrl"), Some(ModifierType::CONTROL_MASK));
        assert_eq!(parse_modifier("Control"), Some(ModifierType::CONTROL_MASK));
        assert_eq!(parse_modifier("Shift"), Some(ModifierType::SHIFT_MASK));
        assert_eq!(parse_modifier("Alt"), Some(ModifierType::ALT_MASK));
        assert_eq!(parse_modifier("Mod1"), Some(ModifierType::ALT_MASK));
        assert_eq!(parse_modifier("Super"), Some(ModifierType::SUPER_MASK));
        assert_eq!(parse_modifier("Mod4"), Some(ModifierType::SUPER_MASK));
    }

    #[test]
    fn parse_modifier_invalid_names() {
        assert_eq!(parse_modifier("invalid"), None);
        assert_eq!(parse_modifier(""), None);
        assert_eq!(parse_modifier("ctrl"), None);
        assert_eq!(parse_modifier("SHIFT"), None);
    }

    // --- parse_keybind ---

    #[test]
    fn parse_keybind_simple_key() {
        let kb = parse_keybind("Escape").unwrap();
        assert!(kb.modifiers.is_empty());
        assert_eq!(kb.key, Key::from_name("Escape").unwrap());
    }

    #[test]
    fn parse_keybind_modifier_and_key() {
        let kb = parse_keybind("Ctrl+c").unwrap();
        assert_eq!(kb.modifiers, ModifierType::CONTROL_MASK);
        assert_eq!(kb.key, Key::from_name("c").unwrap());
    }

    #[test]
    fn parse_keybind_multiple_modifiers() {
        let kb = parse_keybind("Ctrl+Shift+a").unwrap();
        assert_eq!(
            kb.modifiers,
            ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK
        );
        assert_eq!(kb.key, Key::from_name("a").unwrap());
    }

    #[test]
    fn parse_keybind_invalid_modifier() {
        let err = parse_keybind("Bogus+a").unwrap_err();
        assert!(err.contains("unknown modifier"), "got: {err}");
    }

    #[test]
    fn parse_keybind_invalid_key() {
        let err = parse_keybind("Ctrl+nonexistent_key_12345").unwrap_err();
        assert!(err.contains("unknown key"), "got: {err}");
    }

    // --- Config::resolve ---

    #[test]
    fn resolve_defaults_no_warnings() {
        let config = Config::default();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.workspace_prefix, "dyn-");
        assert_eq!(resolved.delete_modifier, ModifierType::SHIFT_MASK);
        assert!(!resolved.close_keybinds.is_empty());
    }

    #[test]
    fn resolve_invalid_close_keybind_produces_warning() {
        let config = Config {
            keybinds: KeybindsConfig {
                close: vec!["Bogus+x".to_string(), "Escape".to_string()],
                ..KeybindsConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Bogus+x"));
        // The valid keybind should still be present
        assert_eq!(resolved.close_keybinds.len(), 1);
    }

    #[test]
    fn resolve_invalid_delete_modifier_falls_back_to_shift() {
        let config = Config {
            keybinds: KeybindsConfig {
                delete_modifier: "BadMod".to_string(),
                ..KeybindsConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("BadMod"));
        assert_eq!(resolved.delete_modifier, ModifierType::SHIFT_MASK);
    }

    #[test]
    fn resolve_empty_delete_modifier_means_no_modifier() {
        let config = Config {
            keybinds: KeybindsConfig {
                delete_modifier: String::new(),
                ..KeybindsConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty());
        assert!(resolved.delete_modifier.is_empty());
    }
}
