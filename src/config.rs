use std::collections::HashMap;

use gdk4::{Key, ModifierType};
use serde::Deserialize;

// --- Serde structs (TOML representation) ---

#[derive(Default, Deserialize)]
#[serde(default)]
struct Config {
    general: GeneralConfig,
    layout: LayoutConfig,
    keybinds: KeybindsConfig,
    workspace: HashMap<String, WorkspaceEntry>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WorkspaceEntry {
    name: Option<String>,
    programs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct GeneralConfig {
    workspace_prefix: String,
    default_programs: Vec<String>,
    auto_delete_empty: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_prefix: "dyn-".to_string(),
            default_programs: Vec::new(),
            auto_delete_empty: true,
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
    pub default_programs: Vec<String>,
    pub workspace_programs: HashMap<char, Vec<String>>,
    pub workspace_names: HashMap<char, String>,
    pub auto_delete_empty: bool,
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

/// Returns `true` if `ch` is a valid workspace key character.
///
/// Accepts lowercase letters (a–z), digits (0–9), and symbols reachable
/// without Shift on a standard US keyboard.
pub fn is_workspace_char(ch: char) -> bool {
    ch.is_ascii_lowercase()
        || ch.is_ascii_digit()
        || matches!(
            ch,
            ',' | '.' | '/' | ';' | '\'' | '[' | ']' | '-' | '=' | '`' | '\\'
        )
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

        let mut workspace_programs = HashMap::new();
        let mut workspace_names = HashMap::new();
        for (key, entry) in self.workspace {
            if let Some(ch) = parse_workspace_char(&key) {
                if !entry.programs.is_empty() {
                    workspace_programs.insert(ch, entry.programs);
                }
                if let Some(name) = entry.name {
                    workspace_names.insert(ch, name);
                }
            } else {
                warnings.push(format!(
                    "ignoring [workspace] key '{key}': must be a single workspace key (a-z, 0-9, or symbol)"
                ));
            }
        }

        let resolved = ResolvedConfig {
            workspace_prefix: self.general.workspace_prefix,
            max_columns: self.layout.max_columns,
            min_columns: self.layout.min_columns,
            max_windows_per_card: self.layout.max_windows_per_card,
            app_name_max_chars: self.layout.app_name_max_chars,
            window_title_max_chars: self.layout.window_title_max_chars,
            close_keybinds,
            default_programs: self.general.default_programs,
            workspace_programs,
            workspace_names,
            auto_delete_empty: self.general.auto_delete_empty,
        };

        (resolved, warnings)
    }
}

/// Format a workspace name from a prefix and a single-character key.
pub fn workspace_name(prefix: &str, ch: char) -> String {
    format!("{prefix}{ch}")
}

/// Parse a string as a single valid workspace-key character.
///
/// Returns `Some(ch)` if the string is exactly one character that satisfies
/// [`is_workspace_char`], otherwise `None`.
pub fn parse_workspace_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if is_workspace_char(ch) => Some(ch),
        _ => None,
    }
}

// --- Public API ---

fn default_config() -> ResolvedConfig {
    Config::default().resolve().0
}

pub fn load_config(path_override: Option<&std::path::Path>) -> ResolvedConfig {
    let config_path = if let Some(p) = path_override {
        p.to_path_buf()
    } else if let Some(dir) = dirs::config_dir() {
        dir.join("niri-dynamic-workspaces").join("config.toml")
    } else {
        eprintln!("warning: could not determine config directory, using defaults");
        return default_config();
    };

    let contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return default_config();
        }
        Err(e) => {
            eprintln!(
                "warning: could not read {}: {e}, using defaults",
                config_path.display()
            );
            return default_config();
        }
    };

    let config: Config = match toml::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "warning: could not parse {}: {e}, using defaults",
                config_path.display()
            );
            return default_config();
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

    // --- workspace_name ---

    #[test]
    fn workspace_name_formats_correctly() {
        assert_eq!(workspace_name("dyn-", 'a'), "dyn-a");
        assert_eq!(workspace_name("ws-", '1'), "ws-1");
        assert_eq!(workspace_name("", 'z'), "z");
    }

    // --- parse_workspace_char ---

    #[test]
    fn parse_workspace_char_valid() {
        assert_eq!(parse_workspace_char("a"), Some('a'));
        assert_eq!(parse_workspace_char("0"), Some('0'));
        assert_eq!(parse_workspace_char(","), Some(','));
    }

    #[test]
    fn parse_workspace_char_invalid() {
        assert_eq!(parse_workspace_char(""), None);
        assert_eq!(parse_workspace_char("ab"), None);
        assert_eq!(parse_workspace_char("A"), None);
        assert_eq!(parse_workspace_char("!"), None);
    }

    // --- is_workspace_char ---

    #[test]
    fn is_workspace_char_variants() {
        // Lowercase letters
        assert!(is_workspace_char('a'));
        assert!(is_workspace_char('z'));
        // Digits
        assert!(is_workspace_char('0'));
        assert!(is_workspace_char('9'));
        // Allowed symbols
        for sym in [',', '.', '/', ';', '\'', '[', ']', '-', '=', '`', '\\'] {
            assert!(is_workspace_char(sym), "expected '{sym}' to be valid");
        }
        // Uppercase — rejected
        assert!(!is_workspace_char('A'));
        assert!(!is_workspace_char('Z'));
        // Space — rejected
        assert!(!is_workspace_char(' '));
        // Shifted symbols — rejected
        assert!(!is_workspace_char('!'));
        assert!(!is_workspace_char('@'));
        assert!(!is_workspace_char('{'));
        // Multi-byte — rejected
        assert!(!is_workspace_char('å'));
        assert!(!is_workspace_char('ñ'));
    }

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
        assert!(!resolved.close_keybinds.is_empty());
        assert!(resolved.default_programs.is_empty());
        assert!(resolved.workspace_programs.is_empty());
        assert!(resolved.workspace_names.is_empty());
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
        assert_eq!(resolved.close_keybinds.len(), 1);
    }

    #[test]
    fn resolve_invalid_workspace_keys_produce_warnings() {
        let mut workspace = HashMap::new();
        workspace.insert(
            "ab".to_string(),
            WorkspaceEntry {
                programs: vec!["firefox".to_string()],
                ..WorkspaceEntry::default()
            },
        );
        workspace.insert(
            "A".to_string(),
            WorkspaceEntry {
                programs: vec!["slack".to_string()],
                ..WorkspaceEntry::default()
            },
        );
        workspace.insert(
            "1".to_string(),
            WorkspaceEntry {
                programs: vec!["kitty".to_string()],
                ..WorkspaceEntry::default()
            },
        );
        workspace.insert("".to_string(), WorkspaceEntry::default());
        let config = Config {
            workspace,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        // "ab" (multi-char), "A" (uppercase), "" (empty) are invalid; "1" is valid
        assert_eq!(warnings.len(), 3);
        assert_eq!(resolved.workspace_programs[&'1'], vec!["kitty"]);
        for w in &warnings {
            assert!(w.contains("[workspace] key"));
        }
    }

    #[test]
    fn resolve_workspace_with_name_and_programs() {
        let mut workspace = HashMap::new();
        workspace.insert(
            "a".to_string(),
            WorkspaceEntry {
                name: Some("Browser".to_string()),
                programs: vec!["firefox".to_string()],
            },
        );
        workspace.insert(
            "b".to_string(),
            WorkspaceEntry {
                name: Some("Terminal".to_string()),
                programs: Vec::new(),
            },
        );
        workspace.insert(
            "bad".to_string(),
            WorkspaceEntry {
                programs: vec!["slack".to_string()],
                ..WorkspaceEntry::default()
            },
        );
        let config = Config {
            workspace,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bad"));
        assert_eq!(resolved.workspace_programs[&'a'], vec!["firefox"]);
        assert!(!resolved.workspace_programs.contains_key(&'b'));
        assert_eq!(resolved.workspace_names[&'a'], "Browser");
        assert_eq!(resolved.workspace_names[&'b'], "Terminal");
    }

    // --- TOML deserialization ---

    #[test]
    fn toml_full_config() {
        let toml_str = r#"
[general]
default_programs = ["kitty"]

[workspace.a]
name = "Browser"
programs = ["firefox", "slack"]

[workspace.b]
name = "Test"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty());
        assert_eq!(resolved.default_programs, vec!["kitty"]);
        assert_eq!(resolved.workspace_programs[&'a'], vec!["firefox", "slack"]);
        assert!(!resolved.workspace_programs.contains_key(&'b'));
        assert_eq!(resolved.workspace_names[&'a'], "Browser");
        assert_eq!(resolved.workspace_names[&'b'], "Test");
    }
}
