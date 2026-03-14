use std::collections::{HashMap, HashSet};

use gdk4::{Key, ModifierType};
use serde::Deserialize;

// --- Serde structs (TOML representation) ---

#[derive(Default, Deserialize)]
#[serde(default)]
struct Config {
    general: GeneralConfig,
    layout: LayoutConfig,
    keybinds: KeybindsConfig,
    hooks: HooksConfig,
    workspace: HashMap<String, WorkspaceEntry>,
    template: HashMap<String, TemplateEntry>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HooksConfig {
    on_create: Vec<String>,
    on_delete: Vec<String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct VariableEntry {
    name: String,
    #[serde(rename = "type")]
    variable_type: String,
    options: Vec<String>,
    command: Option<String>,
    dirs: Vec<String>,
    depth: Option<u32>,
}

impl Default for VariableEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            variable_type: "text".to_string(),
            options: Vec::new(),
            command: None,
            dirs: Vec::new(),
            depth: None,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TemplateEntry {
    programs: Vec<String>,
    key: Option<String>,
    variables: HashMap<String, VariableEntry>,
    on_create: Vec<String>,
    title: Option<String>,
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
    layout: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_prefix: "dyn-".to_string(),
            default_programs: Vec::new(),
            auto_delete_empty: true,
            layout: "qwerty".to_string(),
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

#[derive(Clone, Debug, Default)]
pub struct HookConfig {
    pub on_create: Vec<String>,
    pub on_delete: Vec<String>,
}

pub struct ResolvedConfig {
    pub workspace_prefix: String,
    pub close_keybinds: Vec<Keybind>,
    pub default_programs: Vec<String>,
    pub workspace_programs: HashMap<char, Vec<String>>,
    pub workspace_names: HashMap<char, String>,
    pub auto_delete_empty: bool,
    pub layout: &'static KeyboardLayout,
    pub templates: Vec<Template>,
    pub hooks: HookConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VariableType {
    #[default]
    Text,
    Select(Select),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Select {
    Options(Vec<String>),
    Command(String),
    Dirs { dirs: Vec<String>, depth: u32 },
}

#[derive(Clone, Debug)]
pub struct TemplateVariable {
    pub name: String,
    pub label: String,
    pub var_type: VariableType,
}

#[derive(Clone, Debug)]
pub struct Template {
    pub name: String,
    pub programs: Vec<String>,
    pub key: Option<char>,
    pub variables: Vec<TemplateVariable>,
    pub on_create: Vec<String>,
    pub title: Option<String>,
}

impl ResolvedConfig {
    /// Return the programs configured for a workspace key, falling back to defaults.
    pub fn programs_for(&self, ch: char) -> &[String] {
        self.workspace_programs
            .get(&ch)
            .map_or(self.default_programs.as_slice(), Vec::as_slice)
    }

    /// Whether the template picker should be shown for a given workspace key.
    ///
    /// Returns `true` when templates are configured and the key has no
    /// per-workspace programs (which would bypass the picker).
    pub fn should_show_templates(&self, ch: char) -> bool {
        !self.templates.is_empty() && !self.workspace_programs.contains_key(&ch)
    }
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
/// Accepts lowercase letters (a–z) and digits (0–9).
pub fn is_workspace_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit()
}

// --- Keyboard layouts ---

pub struct KeyboardLayout {
    pub name: &'static str,
    pub rows: &'static [&'static [char]],
    pub row_offsets: &'static [f64],
    pub widest_row_divisor: f64,
}

impl KeyboardLayout {
    /// Compute the divisor from the row geometry.
    ///
    /// For each row: `(offset + key_count) - 1/8` gives the effective width
    /// in key-units (gap = key/8). The divisor is the maximum across rows
    /// plus one gap: `max * 9/8`.
    #[cfg(test)]
    fn compute_widest_row_divisor(&self) -> f64 {
        self.rows
            .iter()
            .zip(self.row_offsets)
            .map(|(row, &offset)| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "row lengths are at most 10, well within f64 precision"
                )]
                let n = row.len() as f64;
                (9.0 * (offset + n) - 1.0) / 8.0
            })
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

const ROW_OFFSETS: &[f64] = &[0.0, 0.5, 0.75, 1.25];

pub static LAYOUT_QWERTY: KeyboardLayout = KeyboardLayout {
    name: "qwerty",
    rows: &[
        &['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'],
        &['q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'],
        &['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'],
        &['z', 'x', 'c', 'v', 'b', 'n', 'm'],
    ],
    row_offsets: ROW_OFFSETS,
    widest_row_divisor: 11.6875,
};

pub static LAYOUT_AZERTY: KeyboardLayout = KeyboardLayout {
    name: "azerty",
    rows: &[
        &['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'],
        &['a', 'z', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p'],
        &['q', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'm'],
        &['w', 'x', 'c', 'v', 'b', 'n'],
    ],
    row_offsets: ROW_OFFSETS,
    widest_row_divisor: 11.96875,
};

pub static LAYOUT_QWERTZ: KeyboardLayout = KeyboardLayout {
    name: "qwertz",
    rows: &[
        &['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'],
        &['q', 'w', 'e', 'r', 't', 'z', 'u', 'i', 'o', 'p'],
        &['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'],
        &['y', 'x', 'c', 'v', 'b', 'n', 'm'],
    ],
    row_offsets: ROW_OFFSETS,
    widest_row_divisor: 11.6875,
};

pub static LAYOUT_DVORAK: KeyboardLayout = KeyboardLayout {
    name: "dvorak",
    rows: &[
        &['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'],
        &['p', 'y', 'f', 'g', 'c', 'r', 'l'],
        &['a', 'o', 'e', 'u', 'i', 'd', 'h', 't', 'n', 's'],
        &['q', 'j', 'k', 'x', 'b', 'm', 'w', 'v', 'z'],
    ],
    row_offsets: ROW_OFFSETS,
    widest_row_divisor: 11.96875,
};

pub static LAYOUT_COLEMAK: KeyboardLayout = KeyboardLayout {
    name: "colemak",
    rows: &[
        &['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'],
        &['q', 'w', 'f', 'p', 'g', 'j', 'l', 'u', 'y'],
        &['a', 'r', 's', 't', 'd', 'h', 'n', 'e', 'i', 'o'],
        &['z', 'x', 'c', 'v', 'b', 'k', 'm'],
    ],
    row_offsets: ROW_OFFSETS,
    widest_row_divisor: 11.96875,
};

pub static ALL_LAYOUTS: &[&KeyboardLayout] = &[
    &LAYOUT_QWERTY,
    &LAYOUT_AZERTY,
    &LAYOUT_QWERTZ,
    &LAYOUT_DVORAK,
    &LAYOUT_COLEMAK,
];

pub fn lookup_layout(name: &str) -> Option<&'static KeyboardLayout> {
    let lower = name.to_ascii_lowercase();
    ALL_LAYOUTS.iter().find(|l| l.name == lower).copied()
}

/// Resolve a single template variable's type from its TOML entry.
///
/// The `type` field determines the variable kind:
/// - `"text"` (default) — free-form text input
/// - `"options"` — dropdown from a static list (`options` field)
/// - `"command"` — dropdown from shell command output (`command` field)
/// - `"dir"` — dropdown from directory scan (`dirs` + optional `depth` fields)
fn resolve_variable_type(
    template_name: &str,
    var_name: &str,
    entry: &VariableEntry,
    warnings: &mut Vec<String>,
) -> VariableType {
    let type_lower = entry.variable_type.to_ascii_lowercase();

    match type_lower.as_str() {
        "text" => VariableType::Text,
        "options" => {
            if entry.options.is_empty() {
                warnings.push(format!(
                    "template '{template_name}': variable '{var_name}' has type 'options' \
                     but no options provided, falling back to text"
                ));
                VariableType::Text
            } else {
                VariableType::Select(Select::Options(entry.options.clone()))
            }
        }
        "command" => {
            if let Some(ref cmd) = entry.command {
                VariableType::Select(Select::Command(cmd.clone()))
            } else {
                warnings.push(format!(
                    "template '{template_name}': variable '{var_name}' has type 'command' \
                     but no command provided, falling back to text"
                ));
                VariableType::Text
            }
        }
        "dir" => {
            if entry.dirs.is_empty() {
                warnings.push(format!(
                    "template '{template_name}': variable '{var_name}' has type 'dir' \
                     but no dirs provided, falling back to text"
                ));
                VariableType::Text
            } else {
                let depth = match entry.depth {
                    Some(0) => {
                        warnings.push(format!(
                            "template '{template_name}': variable '{var_name}' \
                             has depth=0, clamping to 1"
                        ));
                        1
                    }
                    Some(d) => d,
                    None => 1,
                };
                VariableType::Select(Select::Dirs {
                    dirs: entry.dirs.clone(),
                    depth,
                })
            }
        }
        _ => {
            warnings.push(format!(
                "template '{template_name}': unknown variable type '{}' for '{var_name}', \
                 defaulting to text",
                entry.variable_type
            ));
            VariableType::Text
        }
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
    #[expect(
        clippy::too_many_lines,
        reason = "config resolution with template variable validation"
    )]
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
                    "ignoring [workspace] key '{key}': must be a single workspace key (a-z or 0-9)"
                ));
            }
        }

        let layout = if let Some(l) = lookup_layout(&self.general.layout) {
            l
        } else {
            warnings.push(format!(
                "unknown layout '{}', defaulting to qwerty",
                self.general.layout
            ));
            &LAYOUT_QWERTY
        };

        // --- Templates ---
        let mut templates: Vec<Template> = Vec::new();
        // Reserve '1' for the "Empty" option in the template picker.
        let mut used_hotkeys: HashSet<char> = HashSet::from(['1']);

        // Collect and sort template names for deterministic ordering
        let mut template_names: Vec<String> = self.template.keys().cloned().collect();
        template_names.sort();

        for name in &template_names {
            let entry = &self.template[name];

            if entry.programs.is_empty() {
                warnings.push(format!(
                    "ignoring template '{name}': programs list is empty"
                ));
                continue;
            }

            let key = if let Some(ref k) = entry.key {
                if let Some(ch) = parse_workspace_char(k) {
                    if used_hotkeys.contains(&ch) {
                        warnings.push(format!(
                            "template '{name}': duplicate hotkey '{ch}', ignoring key"
                        ));
                        None
                    } else {
                        used_hotkeys.insert(ch);
                        Some(ch)
                    }
                } else {
                    warnings.push(format!(
                        "template '{name}': invalid key '{k}' (must be a-z or 0-9)"
                    ));
                    None
                }
            } else {
                None
            };

            // Resolve variables
            let mut variables: Vec<TemplateVariable> = entry
                .variables
                .iter()
                .map(|(var_name, var_entry)| {
                    let var_type = resolve_variable_type(name, var_name, var_entry, &mut warnings);
                    let label = if var_entry.name.is_empty() {
                        warnings.push(format!(
                            "template '{name}': variable '{var_name}' has empty name, \
                             using key as label"
                        ));
                        var_name.clone()
                    } else {
                        var_entry.name.clone()
                    };
                    TemplateVariable {
                        name: var_name.clone(),
                        label,
                        var_type,
                    }
                })
                .collect();
            variables.sort_by(|a, b| a.name.cmp(&b.name));

            // Warn about unreferenced variables and undefined references
            let declared_names: HashSet<&str> = variables.iter().map(|v| v.name.as_str()).collect();
            let mut referenced_names: HashSet<String> = HashSet::new();
            for prog in &entry.programs {
                for r in extract_variable_references(prog) {
                    referenced_names.insert(r);
                }
            }
            for r in &referenced_names {
                if !declared_names.contains(r.as_str()) {
                    warnings.push(format!(
                        "template '{name}': program references undefined variable '{{{{{r}}}}}'"
                    ));
                }
            }
            for v in &variables {
                if !referenced_names.contains(&v.name) {
                    warnings.push(format!(
                        "template '{name}': variable '{}' is never referenced in programs",
                        v.name
                    ));
                }
            }

            // Validate title template references
            if let Some(ref title) = entry.title {
                for r in extract_variable_references(title) {
                    if !declared_names.contains(r.as_str()) {
                        warnings.push(format!(
                            "template '{name}': title references undefined variable '{{{{{r}}}}}'"
                        ));
                    }
                }
            }

            templates.push(Template {
                name: name.clone(),
                programs: entry.programs.clone(),
                key,
                variables,
                on_create: entry.on_create.clone(),
                title: entry.title.clone(),
            });
        }

        // Auto-assign shortcut keys to templates without an explicit key.
        // Start from '2' — '1' is reserved for the "Empty" option in the picker.
        let auto_candidates = ('2'..='9').chain('a'..='z');
        let mut auto_iter = auto_candidates.filter(|ch| !used_hotkeys.contains(ch));
        for tmpl in &mut templates {
            if tmpl.key.is_none() {
                tmpl.key = auto_iter.next();
            }
        }

        let resolved = ResolvedConfig {
            workspace_prefix: self.general.workspace_prefix,
            close_keybinds,
            default_programs: self.general.default_programs,
            workspace_programs,
            workspace_names,
            auto_delete_empty: self.general.auto_delete_empty,
            layout,
            templates,
            hooks: HookConfig {
                on_create: self.hooks.on_create,
                on_delete: self.hooks.on_delete,
            },
        };

        (resolved, warnings)
    }
}

/// Extract all `{{name}}` variable references from a program string.
///
/// Returns a deduplicated list of variable names in order of first occurrence.
pub fn extract_variable_references(program: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut rest = program;
    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let name = after_open[..end].trim();
            if !name.is_empty() && seen.insert(name.to_string()) {
                refs.push(name.to_string());
            }
            rest = &after_open[end + 2..];
        } else {
            break;
        }
    }
    refs
}

/// Substitute `{{name}}` placeholders in each program string with values.
///
/// Unmatched placeholders (no matching key in `values`) are left as-is.
pub fn substitute_variables(programs: &[String], values: &HashMap<String, String>) -> Vec<String> {
    programs
        .iter()
        .map(|prog| {
            let mut result = prog.clone();
            for (name, value) in values {
                result = result.replace(&format!("{{{{{name}}}}}"), value);
            }
            result
        })
        .collect()
}

/// Build the environment variable pairs for hook execution.
pub fn build_hook_env(
    workspace_name: &str,
    workspace_key: char,
    template_name: Option<&str>,
    variables: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("NDW_WORKSPACE_NAME".to_string(), workspace_name.to_string()),
        ("NDW_WORKSPACE_KEY".to_string(), workspace_key.to_string()),
        (
            "NDW_TEMPLATE".to_string(),
            template_name.unwrap_or("").to_string(),
        ),
    ];
    for (name, value) in variables {
        env.push((
            format!("NDW_VAR_{}", name.to_ascii_uppercase()),
            value.clone(),
        ));
    }
    env
}

/// Collect all on-create hooks: global hooks followed by template-specific hooks.
pub fn collect_create_hooks(config: &ResolvedConfig, template_name: Option<&str>) -> Vec<String> {
    let mut hooks = config.hooks.on_create.clone();
    if let Some(name) = template_name {
        if let Some(tmpl) = config.templates.iter().find(|t| t.name == name) {
            hooks.extend(tmpl.on_create.iter().cloned());
        }
    }
    hooks
}

/// Format a workspace name from a prefix and a single-character key.
pub fn workspace_name(prefix: &str, ch: char) -> String {
    format!("{prefix}{ch}")
}

/// Format a workspace name with an optional title suffix.
pub fn workspace_name_with_title(prefix: &str, ch: char, title: Option<&str>) -> String {
    match title {
        Some(t) if !t.is_empty() => format!("{prefix}{ch} {t}"),
        _ => format!("{prefix}{ch}"),
    }
}

/// Extract the title suffix from a full workspace name.
///
/// Given `"dyn-a My Project"` with prefix `"dyn-"`, returns `Some("My Project")`.
/// Returns `None` if the name doesn't match the prefix or has no title.
pub fn extract_workspace_title(ws_name: &str, prefix: &str) -> Option<String> {
    let rest = ws_name.strip_prefix(prefix)?;
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if !is_workspace_char(ch) {
        return None;
    }
    let remaining = chars.as_str().strip_prefix(' ')?;
    if remaining.is_empty() {
        None
    } else {
        Some(remaining.to_string())
    }
}

/// Resolve a workspace title from template config and variable values.
///
/// - If `title_template` is `Some`, substitutes `{{var}}` placeholders.
/// - If `None` and variables are non-empty, uses the first variable's value
///   (extracting basename for directory paths).
/// - Returns `None` if no variables exist.
pub fn resolve_workspace_title(
    title_template: Option<&str>,
    variables: &[TemplateVariable],
    values: &std::collections::HashMap<String, String>,
) -> Option<String> {
    if let Some(template) = title_template {
        let mut result = template.to_string();
        for (name, value) in values {
            result = result.replace(&format!("{{{{{name}}}}}"), value);
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    } else if let Some(first_var) = variables.first() {
        let value = values.get(&first_var.name)?;
        if value.is_empty() {
            return None;
        }
        // For dir-type variables, extract basename
        if matches!(
            first_var.var_type,
            VariableType::Select(Select::Dirs { .. })
        ) {
            std::path::Path::new(value)
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
        } else {
            Some(value.clone())
        }
    } else {
        None
    }
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
        assert_eq!(parse_workspace_char("z"), Some('z'));
    }

    #[test]
    fn parse_workspace_char_invalid() {
        assert_eq!(parse_workspace_char(""), None);
        assert_eq!(parse_workspace_char("ab"), None);
        assert_eq!(parse_workspace_char("A"), None);
        assert_eq!(parse_workspace_char("!"), None);
        assert_eq!(parse_workspace_char(","), None);
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
        // Uppercase — rejected
        assert!(!is_workspace_char('A'));
        assert!(!is_workspace_char('Z'));
        // Space — rejected
        assert!(!is_workspace_char(' '));
        // Symbols — rejected
        assert!(!is_workspace_char(','));
        assert!(!is_workspace_char('/'));
        assert!(!is_workspace_char('['));
        assert!(!is_workspace_char('!'));
        assert!(!is_workspace_char('@'));
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
        assert_eq!(resolved.layout.name, "qwerty");
        assert!(resolved.templates.is_empty());
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

    // --- Keyboard layout ---

    #[test]
    fn lookup_layout_known() {
        assert_eq!(lookup_layout("qwerty").unwrap().name, "qwerty");
        assert_eq!(lookup_layout("azerty").unwrap().name, "azerty");
        assert_eq!(lookup_layout("qwertz").unwrap().name, "qwertz");
        assert_eq!(lookup_layout("dvorak").unwrap().name, "dvorak");
        assert_eq!(lookup_layout("colemak").unwrap().name, "colemak");
    }

    #[test]
    fn lookup_layout_case_insensitive() {
        assert_eq!(lookup_layout("QWERTY").unwrap().name, "qwerty");
        assert_eq!(lookup_layout("Dvorak").unwrap().name, "dvorak");
        assert_eq!(lookup_layout("CoLeMaK").unwrap().name, "colemak");
    }

    #[test]
    fn lookup_layout_unknown() {
        assert!(lookup_layout("workman").is_none());
        assert!(lookup_layout("").is_none());
    }

    #[test]
    fn resolve_known_layout() {
        let config = Config {
            general: GeneralConfig {
                layout: "colemak".to_string(),
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty());
        assert_eq!(resolved.layout.name, "colemak");
    }

    #[test]
    fn resolve_unknown_layout_warns_and_defaults() {
        let config = Config {
            general: GeneralConfig {
                layout: "workman".to_string(),
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("workman"));
        assert_eq!(resolved.layout.name, "qwerty");
    }

    #[test]
    fn toml_with_layout() {
        let toml_str = r#"
[general]
layout = "dvorak"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty());
        assert_eq!(resolved.layout.name, "dvorak");
    }

    // --- Templates ---

    #[test]
    fn resolve_templates_basic() {
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string(), "code .".to_string()],
                key: Some("d".to_string()),
                ..TemplateEntry::default()
            },
        );
        template.insert(
            "browser".to_string(),
            TemplateEntry {
                programs: vec!["firefox".to_string()],
                key: None,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.templates.len(), 2);
        // Sorted alphabetically
        assert_eq!(resolved.templates[0].name, "browser");
        assert_eq!(resolved.templates[1].name, "dev");
        // Explicit key preserved
        assert_eq!(resolved.templates[1].key, Some('d'));
        // Auto-assigned key (starts at '2', since '1' is reserved for Empty)
        assert_eq!(resolved.templates[0].key, Some('2'));
    }

    #[test]
    fn resolve_templates_empty_programs_warns() {
        let mut template = HashMap::new();
        template.insert(
            "empty".to_string(),
            TemplateEntry {
                programs: Vec::new(),
                key: None,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("empty"));
        assert!(warnings[0].contains("programs list is empty"));
        assert!(resolved.templates.is_empty());
    }

    #[test]
    fn resolve_templates_hotkey_validation() {
        let mut template = HashMap::new();
        template.insert(
            "good".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("a".to_string()),
                ..TemplateEntry::default()
            },
        );
        template.insert(
            "bad".to_string(),
            TemplateEntry {
                programs: vec!["firefox".to_string()],
                key: Some("AB".to_string()),
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("invalid key"));
        assert_eq!(resolved.templates.len(), 2);
        // 'good' has explicit key 'a'
        let good = resolved
            .templates
            .iter()
            .find(|t| t.name == "good")
            .unwrap();
        assert_eq!(good.key, Some('a'));
        // 'bad' got auto-assigned (invalid key dropped but template kept)
        let bad = resolved.templates.iter().find(|t| t.name == "bad").unwrap();
        assert!(bad.key.is_some());
        assert_ne!(bad.key, Some('a')); // must differ from explicit key
    }

    #[test]
    fn resolve_templates_duplicate_hotkey_warns() {
        let mut template = HashMap::new();
        template.insert(
            "alpha".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("a".to_string()),
                ..TemplateEntry::default()
            },
        );
        template.insert(
            "beta".to_string(),
            TemplateEntry {
                programs: vec!["firefox".to_string()],
                key: Some("a".to_string()),
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("duplicate hotkey"));
        // First alphabetically keeps the key, second gets auto-assigned
        let alpha = resolved
            .templates
            .iter()
            .find(|t| t.name == "alpha")
            .unwrap();
        assert_eq!(alpha.key, Some('a'));
        let beta = resolved
            .templates
            .iter()
            .find(|t| t.name == "beta")
            .unwrap();
        assert!(beta.key.is_some());
        assert_ne!(beta.key, Some('a'));
    }

    #[test]
    fn should_show_templates_cases() {
        let mut config = Config::default();
        let (resolved, _) = config.resolve();
        // No templates → false
        assert!(!resolved.should_show_templates('a'));

        // With templates, no per-workspace programs → true
        config = Config {
            template: {
                let mut m = HashMap::new();
                m.insert(
                    "dev".to_string(),
                    TemplateEntry {
                        programs: vec!["kitty".to_string()],
                        key: None,
                        ..TemplateEntry::default()
                    },
                );
                m
            },
            ..Config::default()
        };
        let (resolved, _) = config.resolve();
        assert!(resolved.should_show_templates('a'));

        // With per-workspace programs → false (picker skipped)
        config = Config {
            template: {
                let mut m = HashMap::new();
                m.insert(
                    "dev".to_string(),
                    TemplateEntry {
                        programs: vec!["kitty".to_string()],
                        key: None,
                        ..TemplateEntry::default()
                    },
                );
                m
            },
            workspace: {
                let mut m = HashMap::new();
                m.insert(
                    "a".to_string(),
                    WorkspaceEntry {
                        programs: vec!["firefox".to_string()],
                        ..WorkspaceEntry::default()
                    },
                );
                m
            },
            ..Config::default()
        };
        let (resolved, _) = config.resolve();
        assert!(!resolved.should_show_templates('a'));
        assert!(resolved.should_show_templates('b'));
    }

    #[test]
    fn resolve_templates_auto_shortcut_assignment() {
        let mut template = HashMap::new();
        template.insert(
            "alpha".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("3".to_string()),
                ..TemplateEntry::default()
            },
        );
        template.insert(
            "beta".to_string(),
            TemplateEntry {
                programs: vec!["firefox".to_string()],
                key: None,
                ..TemplateEntry::default()
            },
        );
        template.insert(
            "gamma".to_string(),
            TemplateEntry {
                programs: vec!["slack".to_string()],
                key: None,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        // 'alpha' has explicit '3'
        let alpha = resolved
            .templates
            .iter()
            .find(|t| t.name == "alpha")
            .unwrap();
        assert_eq!(alpha.key, Some('3'));
        // 'beta' auto-gets '2' (first auto candidate, skipping '1' reserved for Empty)
        let beta = resolved
            .templates
            .iter()
            .find(|t| t.name == "beta")
            .unwrap();
        assert_eq!(beta.key, Some('2'));
        // 'gamma' auto-gets '4' (skipping '3' used by alpha)
        let gamma = resolved
            .templates
            .iter()
            .find(|t| t.name == "gamma")
            .unwrap();
        assert_eq!(gamma.key, Some('4'));
    }

    #[test]
    fn resolve_templates_key_1_reserved_for_empty() {
        let mut template = HashMap::new();
        template.insert(
            "mytemplate".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("1".to_string()),
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        // key '1' is reserved for Empty — should warn as duplicate
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("duplicate hotkey '1'"));
        // Template kept but key cleared and auto-assigned
        assert_eq!(resolved.templates.len(), 1);
        let tmpl = &resolved.templates[0];
        assert_eq!(tmpl.name, "mytemplate");
        assert_eq!(tmpl.key, Some('2'));
    }

    #[test]
    fn toml_with_templates() {
        let toml_str = r#"
[template.dev]
programs = ["kitty", "code ."]
key = "d"

[template.browser]
programs = ["firefox"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.templates.len(), 2);
        assert_eq!(resolved.templates[0].name, "browser");
        assert_eq!(resolved.templates[1].name, "dev");
        assert_eq!(resolved.templates[1].key, Some('d'));
        assert_eq!(resolved.templates[1].programs, vec!["kitty", "code ."]);
    }

    #[test]
    fn all_layouts_divisor_matches_computed() {
        for layout in ALL_LAYOUTS {
            let computed = layout.compute_widest_row_divisor();
            assert!(
                (layout.widest_row_divisor - computed).abs() < f64::EPSILON,
                "{}: stored {} != computed {}",
                layout.name,
                layout.widest_row_divisor,
                computed
            );
        }
    }

    // --- extract_variable_references ---

    #[test]
    fn extract_variable_references_basic() {
        assert_eq!(extract_variable_references("code {{path}}"), vec!["path"]);
    }

    #[test]
    fn extract_variable_references_multiple() {
        assert_eq!(
            extract_variable_references("{{a}} and {{b}}"),
            vec!["a", "b"]
        );
    }

    #[test]
    fn extract_variable_references_none() {
        assert!(extract_variable_references("code .").is_empty());
    }

    #[test]
    fn extract_variable_references_duplicate() {
        assert_eq!(extract_variable_references("{{x}} {{x}}"), vec!["x"]);
    }

    #[test]
    fn extract_variable_references_whitespace() {
        assert_eq!(extract_variable_references("code {{ path }}"), vec!["path"]);
    }

    // --- substitute_variables ---

    #[test]
    fn substitute_variables_basic() {
        let programs = vec!["code {{path}}".to_string()];
        let values = HashMap::from([("path".to_string(), "/home/user".to_string())]);
        assert_eq!(
            substitute_variables(&programs, &values),
            vec!["code /home/user"]
        );
    }

    #[test]
    fn substitute_variables_multiple() {
        let programs = vec!["{{cmd}} {{arg}}".to_string()];
        let values = HashMap::from([
            ("cmd".to_string(), "code".to_string()),
            ("arg".to_string(), "/tmp".to_string()),
        ]);
        assert_eq!(substitute_variables(&programs, &values), vec!["code /tmp"]);
    }

    #[test]
    fn substitute_variables_missing_key() {
        let programs = vec!["code {{path}}".to_string()];
        let values = HashMap::new();
        assert_eq!(
            substitute_variables(&programs, &values),
            vec!["code {{path}}"]
        );
    }

    #[test]
    fn substitute_variables_multiple_programs() {
        let programs = vec![
            "code {{path}}".to_string(),
            "git -C {{path}} checkout {{branch}}".to_string(),
            "kitty".to_string(),
        ];
        let values = HashMap::from([
            ("path".to_string(), "/home/user/project".to_string()),
            ("branch".to_string(), "main".to_string()),
        ]);
        assert_eq!(
            substitute_variables(&programs, &values),
            vec![
                "code /home/user/project",
                "git -C /home/user/project checkout main",
                "kitty"
            ]
        );
    }

    // --- Template variable resolution ---

    #[test]
    fn resolve_templates_with_variables() {
        let mut variables = HashMap::new();
        variables.insert(
            "path".to_string(),
            VariableEntry {
                name: "Project path".to_string(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        variables.insert(
            "branch".to_string(),
            VariableEntry {
                name: "Git branch".to_string(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec![
                    "code {{path}}".to_string(),
                    "git checkout {{branch}}".to_string(),
                ],
                key: Some("d".to_string()),
                variables,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let tmpl = &resolved.templates[0];
        assert_eq!(tmpl.variables.len(), 2);
        // Sorted by name
        assert_eq!(tmpl.variables[0].name, "branch");
        assert_eq!(tmpl.variables[0].label, "Git branch");
        assert_eq!(tmpl.variables[0].var_type, VariableType::Text);
        assert_eq!(tmpl.variables[1].name, "path");
        assert_eq!(tmpl.variables[1].label, "Project path");
    }

    // --- Variable type resolution ---

    /// Resolve a single variable entry through config resolution and return
    /// the resulting `(VariableType, Vec<warnings>)`.
    fn resolve_single_variable(var_entry: VariableEntry) -> (VariableType, Vec<String>) {
        let mut variables = HashMap::new();
        variables.insert("project".to_string(), var_entry);
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["code {{project}}".to_string()],
                key: Some("d".to_string()),
                variables,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        (
            resolved.templates[0].variables[0].var_type.clone(),
            warnings,
        )
    }

    #[test]
    fn resolve_templates_unknown_variable_type_warns() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Path".to_string(),
            variable_type: "bogus".to_string(),
            ..VariableEntry::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown variable type"));
        assert!(warnings[0].contains("bogus"));
        assert_eq!(var_type, VariableType::Text);
    }

    #[test]
    fn resolve_templates_select_from_options() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Git branch".to_string(),
            variable_type: "options".to_string(),
            options: vec![
                "main".to_string(),
                "develop".to_string(),
                "staging".to_string(),
            ],
            ..VariableEntry::default()
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            var_type,
            VariableType::Select(Select::Options(vec![
                "main".to_string(),
                "develop".to_string(),
                "staging".to_string(),
            ]))
        );
    }

    #[test]
    fn resolve_templates_default_type_is_text() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Environment".to_string(),
            ..VariableEntry::default()
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(var_type, VariableType::Text);
    }

    #[test]
    fn toml_with_options_variable() {
        let toml_str = r#"
[template.dev]
programs = ["git checkout {{branch}}"]
key = "d"

[template.dev.variables.branch]
name = "Git branch"
type = "options"
options = ["main", "develop", "staging"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let tmpl = &resolved.templates[0];
        assert_eq!(
            tmpl.variables[0].var_type,
            VariableType::Select(Select::Options(vec![
                "main".to_string(),
                "develop".to_string(),
                "staging".to_string(),
            ]))
        );
    }

    #[test]
    fn resolve_templates_empty_variable_name_warns() {
        let mut variables = HashMap::new();
        variables.insert(
            "path".to_string(),
            VariableEntry {
                name: String::new(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["code {{path}}".to_string()],
                key: Some("d".to_string()),
                variables,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("empty name"));
        assert_eq!(resolved.templates[0].variables[0].label, "path");
    }

    #[test]
    fn resolve_templates_unreferenced_variable_warns() {
        let mut variables = HashMap::new();
        variables.insert(
            "unused".to_string(),
            VariableEntry {
                name: "Unused var".to_string(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("d".to_string()),
                variables,
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (_, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("never referenced"));
        assert!(warnings[0].contains("unused"));
    }

    #[test]
    fn resolve_templates_undefined_reference_warns() {
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["code {{path}}".to_string()],
                key: Some("d".to_string()),
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (_, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("undefined variable"));
        assert!(warnings[0].contains("{{path}}"));
    }

    #[test]
    fn toml_with_template_variables() {
        let toml_str = r#"
[template.dev]
programs = ["kitty", "code {{path}}"]
key = "d"

[template.dev.variables.path]
name = "Project path"
type = "text"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.templates.len(), 1);
        let tmpl = &resolved.templates[0];
        assert_eq!(tmpl.name, "dev");
        assert_eq!(tmpl.variables.len(), 1);
        assert_eq!(tmpl.variables[0].name, "path");
        assert_eq!(tmpl.variables[0].label, "Project path");
        assert_eq!(tmpl.variables[0].var_type, VariableType::Text);
    }

    #[test]
    fn template_variables_field() {
        let tmpl = Template {
            name: "test".to_string(),
            programs: Vec::new(),
            key: None,
            variables: Vec::new(),
            on_create: Vec::new(),
            title: None,
        };
        assert!(tmpl.variables.is_empty());

        let tmpl_with = Template {
            name: "test".to_string(),
            programs: Vec::new(),
            key: None,
            variables: vec![TemplateVariable {
                name: "x".to_string(),
                label: "X".to_string(),
                var_type: VariableType::Text,
            }],
            on_create: Vec::new(),
            title: None,
        };
        assert!(!tmpl_with.variables.is_empty());
    }

    // --- build_hook_env ---

    #[test]
    fn build_hook_env_basic() {
        let env = build_hook_env("dyn-a", 'a', None, &HashMap::new());
        assert!(env.contains(&("NDW_WORKSPACE_NAME".to_string(), "dyn-a".to_string())));
        assert!(env.contains(&("NDW_WORKSPACE_KEY".to_string(), "a".to_string())));
        assert!(env.contains(&("NDW_TEMPLATE".to_string(), String::new())));
    }

    #[test]
    fn build_hook_env_with_template() {
        let env = build_hook_env("dyn-a", 'a', Some("dev"), &HashMap::new());
        assert!(env.contains(&("NDW_TEMPLATE".to_string(), "dev".to_string())));
    }

    #[test]
    fn build_hook_env_with_variables() {
        let vars = HashMap::from([
            ("path".to_string(), "/home/user".to_string()),
            ("branch".to_string(), "main".to_string()),
        ]);
        let env = build_hook_env("dyn-a", 'a', Some("dev"), &vars);
        assert!(env.contains(&("NDW_VAR_PATH".to_string(), "/home/user".to_string())));
        assert!(env.contains(&("NDW_VAR_BRANCH".to_string(), "main".to_string())));
    }

    // --- collect_create_hooks ---

    fn hooks_test_config(global_hooks: Vec<String>, templates: Vec<Template>) -> ResolvedConfig {
        ResolvedConfig {
            workspace_prefix: "dyn-".to_string(),
            close_keybinds: Vec::new(),
            default_programs: Vec::new(),
            workspace_programs: HashMap::new(),
            workspace_names: HashMap::new(),
            auto_delete_empty: true,
            layout: &LAYOUT_QWERTY,
            templates,
            hooks: HookConfig {
                on_create: global_hooks,
                on_delete: Vec::new(),
            },
        }
    }

    #[test]
    fn collect_create_hooks_no_template() {
        let config = hooks_test_config(vec!["notify-send 'created'".to_string()], Vec::new());
        let hooks = collect_create_hooks(&config, None);
        assert_eq!(hooks, vec!["notify-send 'created'"]);
    }

    #[test]
    fn collect_create_hooks_with_template() {
        let config = hooks_test_config(
            vec!["global-hook".to_string()],
            vec![Template {
                name: "dev".to_string(),
                programs: vec!["kitty".to_string()],
                key: None,
                variables: Vec::new(),
                on_create: vec!["template-hook".to_string()],
                title: None,
            }],
        );
        let hooks = collect_create_hooks(&config, Some("dev"));
        assert_eq!(hooks, vec!["global-hook", "template-hook"]);
    }

    #[test]
    fn collect_create_hooks_unknown_template() {
        let config = hooks_test_config(
            vec!["global-hook".to_string()],
            vec![Template {
                name: "dev".to_string(),
                programs: vec!["kitty".to_string()],
                key: None,
                variables: Vec::new(),
                on_create: vec!["template-hook".to_string()],
                title: None,
            }],
        );
        let hooks = collect_create_hooks(&config, Some("unknown"));
        assert_eq!(hooks, vec!["global-hook"]);
    }

    // --- Hook resolution ---

    #[test]
    fn resolve_hooks_default() {
        let config = Config::default();
        let (resolved, _) = config.resolve();
        assert!(resolved.hooks.on_create.is_empty());
        assert!(resolved.hooks.on_delete.is_empty());
    }

    #[test]
    fn resolve_hooks_basic() {
        let config = Config {
            hooks: HooksConfig {
                on_create: vec!["notify-send 'created'".to_string()],
                on_delete: vec!["cleanup.sh".to_string()],
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.hooks.on_create, vec!["notify-send 'created'"]);
        assert_eq!(resolved.hooks.on_delete, vec!["cleanup.sh"]);
    }

    #[test]
    fn resolve_template_on_create() {
        let mut template = HashMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                on_create: vec!["git status".to_string()],
                ..TemplateEntry::default()
            },
        );
        let config = Config {
            template,
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.templates[0].on_create, vec!["git status"]);
    }

    #[test]
    fn toml_with_hooks() {
        let toml_str = r#"
[hooks]
on_create = ['notify-send "Created $NDW_WORKSPACE_NAME"']
on_delete = ["cleanup-workspace.sh"]

[template.dev]
programs = ["kitty", "code ."]
on_create = ['git -C "$NDW_VAR_PATH" status']
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            resolved.hooks.on_create,
            vec![r#"notify-send "Created $NDW_WORKSPACE_NAME""#]
        );
        assert_eq!(resolved.hooks.on_delete, vec!["cleanup-workspace.sh"]);
        assert_eq!(
            resolved.templates[0].on_create,
            vec![r#"git -C "$NDW_VAR_PATH" status"#]
        );
    }

    // --- Select from command ---

    #[test]
    fn resolve_templates_select_from_command() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Project".to_string(),
            variable_type: "command".to_string(),
            command: Some("ls ~/dev".to_string()),
            ..VariableEntry::default()
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            var_type,
            VariableType::Select(Select::Command("ls ~/dev".to_string()))
        );
    }

    #[test]
    fn toml_with_command_variable() {
        let toml_str = r#"
[template.dev]
programs = ["code {{project}}"]
key = "d"

[template.dev.variables.project]
name = "Project"
type = "command"
command = "ls ~/dev"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let var = &resolved.templates[0].variables[0];
        assert_eq!(
            var.var_type,
            VariableType::Select(Select::Command("ls ~/dev".to_string()))
        );
    }

    // --- Select from dirs ---

    #[test]
    fn resolve_templates_select_from_dirs() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Project".to_string(),
            variable_type: "dir".to_string(),
            dirs: vec!["~/dev".to_string()],
            ..VariableEntry::default()
        });
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            var_type,
            VariableType::Select(Select::Dirs {
                dirs: vec!["~/dev".to_string()],
                depth: 1,
            })
        );
    }

    #[test]
    fn resolve_templates_depth_zero_clamps() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Project".to_string(),
            variable_type: "dir".to_string(),
            dirs: vec!["~/dev".to_string()],
            depth: Some(0),
            ..VariableEntry::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("depth=0"));
        assert_eq!(
            var_type,
            VariableType::Select(Select::Dirs {
                dirs: vec!["~/dev".to_string()],
                depth: 1,
            })
        );
    }

    #[test]
    fn toml_with_dir_variable() {
        let toml_str = r#"
[template.dev]
programs = ["code {{project}}"]
key = "d"

[template.dev.variables.project]
name = "Project"
type = "dir"
dirs = ["~/dev", "~/work"]
depth = 2
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let var = &resolved.templates[0].variables[0];
        assert_eq!(
            var.var_type,
            VariableType::Select(Select::Dirs {
                dirs: vec!["~/dev".to_string(), "~/work".to_string()],
                depth: 2,
            })
        );
    }

    // --- Missing source field warnings ---

    #[test]
    fn resolve_options_type_no_options_warns() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Branch".to_string(),
            variable_type: "options".to_string(),
            ..VariableEntry::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no options provided"));
        assert_eq!(var_type, VariableType::Text);
    }

    #[test]
    fn resolve_command_type_no_command_warns() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Project".to_string(),
            variable_type: "command".to_string(),
            ..VariableEntry::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no command provided"));
        assert_eq!(var_type, VariableType::Text);
    }

    #[test]
    fn resolve_dir_type_no_dirs_warns() {
        let (var_type, warnings) = resolve_single_variable(VariableEntry {
            name: "Project".to_string(),
            variable_type: "dir".to_string(),
            ..VariableEntry::default()
        });
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no dirs provided"));
        assert_eq!(var_type, VariableType::Text);
    }

    // --- workspace_name_with_title ---

    #[test]
    fn workspace_name_with_title_no_title() {
        assert_eq!(workspace_name_with_title("dyn-", 'a', None), "dyn-a");
        assert_eq!(workspace_name_with_title("dyn-", 'a', Some("")), "dyn-a");
    }

    #[test]
    fn workspace_name_with_title_with_title() {
        assert_eq!(
            workspace_name_with_title("dyn-", 'a', Some("My Project")),
            "dyn-a My Project"
        );
    }

    // --- extract_workspace_title ---

    #[test]
    fn extract_workspace_title_basic() {
        assert_eq!(
            extract_workspace_title("dyn-a My Project", "dyn-"),
            Some("My Project".to_string())
        );
    }

    #[test]
    fn extract_workspace_title_no_title() {
        assert_eq!(extract_workspace_title("dyn-a", "dyn-"), None);
    }

    #[test]
    fn extract_workspace_title_wrong_prefix() {
        assert_eq!(extract_workspace_title("other-a Title", "dyn-"), None);
    }

    #[test]
    fn extract_workspace_title_no_space() {
        // "dyn-abc" — 'a' is the char, "bc" has no leading space
        assert_eq!(extract_workspace_title("dyn-abc", "dyn-"), None);
    }

    // --- resolve_workspace_title ---

    #[test]
    fn resolve_workspace_title_explicit_template() {
        let variables = vec![];
        let mut values = HashMap::new();
        values.insert("path".to_string(), "/home/user/dev/myproject".to_string());
        let result = resolve_workspace_title(Some("Dev: {{path}}"), &variables, &values);
        assert_eq!(result, Some("Dev: /home/user/dev/myproject".to_string()));
    }

    #[test]
    fn resolve_workspace_title_auto_text_var() {
        let variables = vec![TemplateVariable {
            name: "branch".to_string(),
            label: "Branch".to_string(),
            var_type: VariableType::Text,
        }];
        let mut values = HashMap::new();
        values.insert("branch".to_string(), "main".to_string());
        let result = resolve_workspace_title(None, &variables, &values);
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn resolve_workspace_title_auto_dir_basename() {
        let variables = vec![TemplateVariable {
            name: "path".to_string(),
            label: "Path".to_string(),
            var_type: VariableType::Select(Select::Dirs {
                dirs: vec!["~/dev".to_string()],
                depth: 1,
            }),
        }];
        let mut values = HashMap::new();
        values.insert("path".to_string(), "/home/user/dev/myproject".to_string());
        let result = resolve_workspace_title(None, &variables, &values);
        assert_eq!(result, Some("myproject".to_string()));
    }

    #[test]
    fn resolve_workspace_title_no_variables() {
        let result = resolve_workspace_title(None, &[], &HashMap::new());
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_workspace_title_empty_value() {
        let variables = vec![TemplateVariable {
            name: "branch".to_string(),
            label: "Branch".to_string(),
            var_type: VariableType::Text,
        }];
        let mut values = HashMap::new();
        values.insert("branch".to_string(), String::new());
        let result = resolve_workspace_title(None, &variables, &values);
        assert_eq!(result, None);
    }
}
