use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gdk4::{Key, ModifierType};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::niri::CleanupConfig;

// --- Serde structs (TOML representation) ---
// No #[serde(flatten)]: serde_ignored can't see unknown keys through it.

#[derive(Default, Deserialize)]
#[serde(default)]
struct Config {
    general: GeneralConfig,
    keybinds: KeybindsConfig,
    hooks: HooksConfig,
    workspace: HashMap<String, WorkspaceEntry>,
    /// In declaration order, which sets the picker order and automatic keys.
    template: IndexMap<String, TemplateEntry>,
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
    #[serde(deserialize_with = "string_or_integer")]
    key: Option<String>,
    /// In declaration order: the form's order, and the first fills in the title.
    variables: IndexMap<String, VariableEntry>,
    on_create: Vec<String>,
    title: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WorkspaceEntry {
    name: Option<String>,
    programs: Vec<String>,
    #[serde(rename = "static", deserialize_with = "string_or_integer")]
    static_workspace: Option<String>,
}

/// An optional string that may also be written as an integer: `key = 2`,
/// `static = 1`.
fn string_or_integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct StringOrInteger;

    impl serde::de::Visitor<'_> for StringOrInteger {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or an integer")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }

    d.deserialize_any(StringOrInteger).map(Some)
}

#[derive(Deserialize)]
#[serde(default)]
#[expect(clippy::struct_excessive_bools, reason = "independent config flags")]
struct GeneralConfig {
    workspace_prefix: String,
    default_programs: Vec<String>,
    auto_delete_empty: bool,
    layout: String,
    hover_preview: bool,
    hide_empty_static: bool,
    inhibit_compositor_shortcuts: bool,
    confirm_delete: bool,
    theme: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_prefix: "dyn-".to_string(),
            default_programs: Vec::new(),
            auto_delete_empty: true,
            layout: "qwerty".to_string(),
            hover_preview: true,
            hide_empty_static: false,
            inhibit_compositor_shortcuts: true,
            confirm_delete: true,
            theme: "gtk".to_string(),
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

#[expect(clippy::struct_excessive_bools, reason = "independent config flags")]
pub struct ResolvedConfig {
    pub workspace_prefix: String,
    pub close_keybinds: Vec<Keybind>,
    pub default_programs: Vec<String>,
    pub workspace_programs: HashMap<char, Vec<String>>,
    pub workspace_names: HashMap<char, String>,
    /// Keys pinned to existing (non-dynamic) niri workspaces by name.
    pub static_workspaces: HashMap<char, String>,
    pub auto_delete_empty: bool,
    pub hover_preview: bool,
    /// Hide windowless workspaces from the static row (focused/urgent stay).
    pub hide_empty_static: bool,
    /// Suppress compositor keybinds while the overlay is open, so a held
    /// Mod+<key> reaches the overlay instead of firing niri binds.
    pub inhibit_compositor_shortcuts: bool,
    /// Delete mode asks for a second press before closing a workspace's windows.
    pub confirm_delete: bool,
    pub layout: &'static KeyboardLayout,
    pub theme: Theme,
    pub templates: Vec<Template>,
    pub hooks: HookConfig,
    /// Problems found while loading; the config falls back to defaults where they apply.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The file could not be used; the whole config fell back to defaults.
    Error,
    /// One setting was ignored or replaced by its default.
    Warning,
}

/// A config problem to show the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.severity {
            Severity::Error => "config error",
            Severity::Warning => "config warning",
        };
        write!(f, "{label}: {}", self.message)
    }
}

/// A palette compiled into the binary from `themes/<name>.css`.
#[derive(Debug, PartialEq, Eq)]
pub struct BuiltinTheme {
    pub name: &'static str,
    pub css: &'static str,
}

// `BUILTIN_THEMES`, generated by build.rs from the files in `themes/`.
include!(concat!(env!("OUT_DIR"), "/builtin_themes.rs"));

/// Colour palette for the overlay, from `general.theme`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Theme {
    Builtin(&'static BuiltinTheme),
    /// CSS file layered over the GTK primaries; [`load_config`] makes relative paths absolute.
    File(std::path::PathBuf),
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin("gtk").expect("build.rs guarantees themes/gtk.css")
    }
}

impl Theme {
    fn builtin(name: &str) -> Option<Self> {
        BUILTIN_THEMES
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
            .map(Self::Builtin)
    }

    /// A value containing `/` or ending in `.css` is a file; anything else must be a built-in name.
    fn parse(value: &str) -> Result<Self, String> {
        let is_path = value.contains('/')
            || std::path::Path::new(value)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("css"));
        if is_path {
            return Ok(Self::File(expand_tilde(value).into()));
        }
        Self::builtin(value).ok_or_else(|| {
            let names: Vec<_> = BUILTIN_THEMES.iter().map(|theme| theme.name).collect();
            format!(
                "unknown theme '{value}' (expected a .css path or one of: {}), defaulting to gtk",
                names.join(", ")
            )
        })
    }
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

    /// Whether loading fell back to the defaults for the whole file.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
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
        "Super" | "Mod4" | "Mod" => Some(ModifierType::SUPER_MASK),
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

    // Lowercase, as GTK accelerators do: "Ctrl+C" is Ctrl+c, and Shift must be written out.
    let key = Key::from_name(key_name)
        .ok_or_else(|| format!("unknown key name '{key_name}'"))?
        .to_lower();

    Ok(Keybind { modifiers, key })
}

/// The workspace key a close bind takes from the overlay, which checks close
/// binds first. Super counts as no modifier: keys are often pressed with the
/// launch bind's Super still held. A Ctrl, Shift or Alt bind only takes a
/// press made on purpose.
fn shadowed_workspace_key(kb: &Keybind) -> Option<char> {
    let distinct = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK | ModifierType::ALT_MASK;
    if kb.modifiers.intersects(distinct) {
        return None;
    }
    kb.key.to_unicode().filter(|&c| is_workspace_char(c))
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
                Ok(kb) => {
                    if let Some(ch) = shadowed_workspace_key(&kb) {
                        warnings.push(format!("close keybind '{s}' shadows workspace key '{ch}'"));
                    }
                    close_keybinds.push(kb);
                }
                Err(e) => warnings.push(format!("ignoring close keybind '{s}': {e}")),
            }
        }
        if close_keybinds.is_empty() && !self.keybinds.close.is_empty() {
            warnings.push("no valid close keybind, using the defaults".to_string());
            close_keybinds = KeybindsConfig::default()
                .close
                .iter()
                .filter_map(|s| parse_keybind(s).ok())
                .collect();
        }

        // An empty prefix would make every single-character niri workspace dynamic.
        let prefix = if self.general.workspace_prefix.trim().is_empty() {
            let fallback = GeneralConfig::default().workspace_prefix;
            warnings.push(format!(
                "workspace_prefix must not be empty, using '{fallback}'"
            ));
            fallback
        } else {
            self.general.workspace_prefix
        };

        let mut workspace_programs = HashMap::new();
        let mut workspace_names = HashMap::new();
        let mut static_workspaces = HashMap::new();
        for (key, entry) in self.workspace {
            let Some(ch) = parse_workspace_char(&key) else {
                warnings.push(format!(
                    "ignoring [workspace] key '{key}': must be a single workspace key (a-z or 0-9)"
                ));
                continue;
            };
            match entry.static_workspace {
                Some(target) if target.is_empty() => {
                    warnings.push(format!("[workspace.{key}]: 'static' is empty, ignoring"));
                }
                // niri matches names case-insensitively, so "DYN-A" is dyn-a.
                Some(target)
                    if parse_dynamic_name(
                        &target.to_ascii_lowercase(),
                        &prefix.to_ascii_lowercase(),
                    )
                    .is_some() =>
                {
                    warnings.push(format!(
                        "[workspace.{key}]: 'static' target '{target}' is a dynamic \
                         workspace name, ignoring"
                    ));
                }
                Some(target) => {
                    if !entry.programs.is_empty() {
                        warnings.push(format!(
                            "[workspace.{key}]: 'programs' are ignored for static workspaces"
                        ));
                    }
                    static_workspaces.insert(ch, target);
                    if let Some(name) = entry.name.filter(|n| !n.is_empty()) {
                        workspace_names.insert(ch, name);
                    }
                    continue;
                }
                None => {}
            }
            if !entry.programs.is_empty() {
                warn_unbalanced(
                    &format!("[workspace.{key}]"),
                    &entry.programs,
                    &mut warnings,
                );
                workspace_programs.insert(ch, entry.programs);
            }
            if let Some(name) = entry.name {
                workspace_names.insert(ch, name);
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

        let theme = Theme::parse(&self.general.theme).unwrap_or_else(|w| {
            warnings.push(w);
            Theme::default()
        });

        warn_unbalanced(
            "[general] default_programs",
            &self.general.default_programs,
            &mut warnings,
        );

        // --- Templates ---
        let mut templates: Vec<Template> = Vec::new();
        // Reserve '1' for the "Empty" option in the template picker.
        let mut used_hotkeys: HashSet<char> = HashSet::from(['1']);

        for (name, entry) in &self.template {
            if entry.programs.is_empty() {
                warnings.push(format!(
                    "ignoring template '{name}': programs list is empty"
                ));
                continue;
            }
            warn_unbalanced(
                &format!("template '{name}'"),
                &entry.programs,
                &mut warnings,
            );

            let key = if let Some(ref k) = entry.key {
                if let Some(ch) = parse_workspace_char(k) {
                    if ch == '1' {
                        warnings.push(format!(
                            "template '{name}': key '1' is reserved for the Empty option, \
                             ignoring key"
                        ));
                        None
                    } else if used_hotkeys.contains(&ch) {
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
            let variables: Vec<TemplateVariable> = entry
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

            // Warn about variables whose values would overwrite each other in hooks
            let mut env_names: HashMap<String, &str> = HashMap::new();
            for v in &variables {
                let env_name = hook_env_var(&v.name);
                if let Some(other) = env_names.insert(env_name.clone(), &v.name) {
                    warnings.push(format!(
                        "template '{name}': variables '{other}' and '{}' share hook variable {env_name}",
                        v.name
                    ));
                }
            }

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
            for v in unused_variables(&variables, entry, &self.hooks.on_create) {
                warnings.push(format!(
                    "template '{name}': variable '{v}' is never referenced in programs, \
                     title or hooks"
                ));
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
            workspace_prefix: prefix,
            close_keybinds,
            default_programs: self.general.default_programs,
            workspace_programs,
            workspace_names,
            static_workspaces,
            auto_delete_empty: self.general.auto_delete_empty,
            hover_preview: self.general.hover_preview,
            hide_empty_static: self.general.hide_empty_static,
            inhibit_compositor_shortcuts: self.general.inhibit_compositor_shortcuts,
            confirm_delete: self.general.confirm_delete,
            layout,
            theme,
            templates,
            hooks: HookConfig {
                on_create: self.hooks.on_create,
                on_delete: self.hooks.on_delete,
            },
            diagnostics: Vec::new(),
        };

        (resolved, warnings)
    }
}

/// Warn about each of `programs` that cannot be split into arguments; it
/// would fail when a workspace is created.
fn warn_unbalanced(context: &str, programs: &[String], warnings: &mut Vec<String>) {
    for program in programs {
        if let Err(e) = build_argv(program, &HashMap::new()) {
            warnings.push(format!("{context}: cannot split program `{program}` ({e})"));
        }
    }
}

/// The names of `variables` that nothing reads: no `{{name}}` in the
/// template's programs or title, no `NDW_VAR_*` in its or the global
/// `on_create` hooks, and not the first one while it fills in the title.
fn unused_variables<'a>(
    variables: &'a [TemplateVariable],
    entry: &TemplateEntry,
    global_on_create: &[String],
) -> Vec<&'a str> {
    let placeholders: HashSet<String> = entry
        .programs
        .iter()
        .chain(&entry.title)
        .flat_map(|text| extract_variable_references(text))
        .collect();
    let hooks: Vec<&String> = entry.on_create.iter().chain(global_on_create).collect();
    variables
        .iter()
        .enumerate()
        .filter(|&(i, v)| {
            let fills_title = i == 0 && entry.title.is_none();
            let env_name = hook_env_var(&v.name);
            !fills_title
                && !placeholders.contains(&v.name)
                && !hooks.iter().any(|hook| mentions_env_var(hook, &env_name))
        })
        .map(|(_, v)| v.name.as_str())
        .collect()
}

/// Whether shell `command` contains `var` as a whole word, as in
/// `"$NDW_VAR_PATH"` but not `$NDW_VAR_PATHS`.
fn mentions_env_var(command: &str, var: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    command.match_indices(var).any(|(i, _)| {
        !command[..i].chars().next_back().is_some_and(is_word)
            && !command[i + var.len()..].chars().next().is_some_and(is_word)
    })
}

/// A piece of a template string: literal text or a `{{name}}` placeholder.
enum Segment<'a> {
    Literal(&'a str),
    /// `raw` is the full `{{ name }}` text, `name` the trimmed variable name.
    Placeholder {
        raw: &'a str,
        name: &'a str,
    },
}

/// Split a template string into literal and `{{name}}` placeholder segments.
///
/// An unterminated `{{` is literal text.
fn segments(template: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else {
            break;
        };
        if start > 0 {
            out.push(Segment::Literal(&rest[..start]));
        }
        let end = start + 2 + len + 2;
        out.push(Segment::Placeholder {
            raw: &rest[start..end],
            name: rest[start + 2..end - 2].trim(),
        });
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        out.push(Segment::Literal(rest));
    }
    out
}

/// Extract all `{{name}}` variable references from a program string.
///
/// Returns a deduplicated list of variable names in order of first occurrence.
pub fn extract_variable_references(program: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for segment in segments(program) {
        if let Segment::Placeholder { name, .. } = segment {
            if !name.is_empty() && !refs.iter().any(|r| r == name) {
                refs.push(name.to_string());
            }
        }
    }
    refs
}

/// Substitute `{{name}}` placeholders with their values in a single pass.
///
/// Inserted values are never rescanned, so a value containing `{{other}}`
/// stays verbatim. Placeholders with no matching key are left as-is.
pub fn substitute(template: &str, values: &HashMap<String, String>) -> String {
    segments(template)
        .into_iter()
        .map(|segment| match segment {
            Segment::Literal(text) => text,
            Segment::Placeholder { raw, name } => values.get(name).map_or(raw, String::as_str),
        })
        .collect()
}

/// Turn a program string into an argument vector: split with shell quoting
/// rules (no shell is invoked), then substitute placeholders in each word.
///
/// Splitting first keeps every value inside the argument its placeholder was
/// written in, whatever spaces or quotes the value contains.
///
/// # Errors
///
/// Fails when the program string has unbalanced quotes.
pub fn build_argv(
    program: &str,
    values: &HashMap<String, String>,
) -> Result<Vec<String>, shell_words::ParseError> {
    // `{{ name }}` would otherwise split into three words.
    let normalized: String = segments(program)
        .into_iter()
        .map(|segment| match segment {
            Segment::Literal(text) => text.to_string(),
            Segment::Placeholder { name, .. } => format!("{{{{{name}}}}}"),
        })
        .collect();
    Ok(shell_words::split(&normalized)?
        .iter()
        .map(|word| substitute(word, values))
        .collect())
}

/// The hook environment variable for template variable `name`: `NDW_VAR_`
/// followed by `name` uppercased, with every character other than an ASCII
/// letter or digit replaced by `_`.
///
/// The result is always a valid shell variable name: `project-dir` becomes
/// `NDW_VAR_PROJECT_DIR`.
pub fn hook_env_var(name: &str) -> String {
    let suffix: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("NDW_VAR_{suffix}")
}

/// Build the environment variable pairs for hook execution.
///
/// Variables follow the fixed entries under their [`hook_env_var`] names,
/// ordered by variable name.
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
    let mut variables: Vec<_> = variables.iter().collect();
    variables.sort();
    env.extend(
        variables
            .into_iter()
            .map(|(name, value)| (hook_env_var(name), value.clone())),
    );
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
        _ => workspace_name(prefix, ch),
    }
}

/// Parse a dynamic workspace name into its key and optional title.
///
/// Accepts `{prefix}{key}` and `{prefix}{key} {title}`; anything else after
/// the key (as in `dyn-alpha`) is not a dynamic workspace. An empty title
/// yields `None`.
pub fn parse_dynamic_name<'a>(ws_name: &'a str, prefix: &str) -> Option<(char, Option<&'a str>)> {
    let rest = ws_name.strip_prefix(prefix)?;
    let mut chars = rest.chars();
    let ch = chars.next().filter(|&ch| is_workspace_char(ch))?;
    match chars.as_str() {
        "" => Some((ch, None)),
        tail => {
            let title = tail.strip_prefix(' ')?;
            Some((ch, (!title.is_empty()).then_some(title)))
        }
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
        let result = substitute(template, values);
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

/// Expand a leading `~/` in a path to the user's home directory.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{rest}", home.display());
        }
    }
    path.to_string()
}

fn default_config() -> ResolvedConfig {
    Config::default().resolve().0
}

/// Resolve the config file location: the override if given, else the XDG default.
pub(crate) fn config_path(path_override: Option<&Path>) -> Option<PathBuf> {
    path_override.map(Path::to_path_buf).or_else(|| {
        dirs::config_dir().map(|dir| dir.join("niri-dynamic-workspaces").join("config.toml"))
    })
}

/// Read the config file: `Ok(None)` when the default file does not exist,
/// `Err` with a user-facing message on any other I/O failure, including a
/// missing `explicit` path (one the user named).
fn read_config(path: &Path, explicit: bool) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(None),
        Err(e) => Err(format!(
            "could not read {}: {e}, using defaults",
            path.display()
        )),
    }
}

/// Resolve the result of [`read_config`] for `path`.
fn from_contents(path: &Path, read: &Result<Option<String>, String>) -> ResolvedConfig {
    match read {
        Ok(Some(text)) => parse_config(text, path),
        Ok(None) => default_config(),
        Err(message) => {
            let mut config = default_config();
            config.diagnostics.push(Diagnostic::error(message.as_str()));
            config
        }
    }
}

/// Parse and resolve config text read from `path`, which anchors relative theme paths.
///
/// Text that does not parse yields the defaults plus one error diagnostic.
/// Keys the config does not know are warnings, listed before the others.
fn parse_config(text: &str, path: &Path) -> ResolvedConfig {
    let mut unknown = Vec::new();
    let parsed: Result<Config, _> = toml::Deserializer::parse(text)
        .and_then(|de| serde_ignored::deserialize(de, |key| unknown.push(key.to_string())));
    let config = match parsed {
        Ok(config) => config,
        Err(e) => {
            let mut config = default_config();
            config.diagnostics.push(Diagnostic::error(format!(
                "could not parse {}: {}, using defaults",
                path.display(),
                toml_error_summary(&e, text)
            )));
            return config;
        }
    };
    let (mut resolved, warnings) = config.resolve();
    resolved.diagnostics.extend(
        unknown
            .iter()
            .map(|key| unknown_key_warning(key))
            .chain(warnings)
            .map(Diagnostic::warning),
    );
    if let (Theme::File(theme), Some(dir)) = (&mut resolved.theme, path.parent()) {
        *theme = dir.join(&*theme);
    }
    resolved
}

/// The keys of `[general]`, which also parse at the top level without effect.
const GENERAL_KEYS: &[&str] = &[
    "workspace_prefix",
    "default_programs",
    "auto_delete_empty",
    "layout",
    "hover_preview",
    "hide_empty_static",
    "inhibit_compositor_shortcuts",
    "confirm_delete",
    "theme",
];

/// The warning for an ignored key at dotted `path`, with a hint for the
/// likely mistakes.
fn unknown_key_warning(path: &str) -> String {
    let parts: Vec<&str> = path.split('.').collect();
    let hint = match parts.as_slice() {
        ["workspaces", ..] => Some("did you mean [workspace]?"),
        ["templates", ..] => Some("did you mean [template]?"),
        ["hook", ..] => Some("did you mean [hooks]?"),
        ["keybind", ..] => Some("did you mean [keybinds]?"),
        [key] if GENERAL_KEYS.contains(key) => Some("general settings go under [general]"),
        ["template", _, "on_delete"] => Some("templates only support on_create"),
        _ => None,
    };
    match hint {
        Some(hint) => format!("unknown key '{path}'; {hint}"),
        None => format!("unknown key '{path}'"),
    }
}

/// `line L, column C: <message>` for a TOML error in `text`; just the message
/// when the error has no position.
fn toml_error_summary(e: &toml::de::Error, text: &str) -> String {
    let Some(span) = e.span() else {
        return e.message().to_string();
    };
    let before = text.get(..span.start).unwrap_or("");
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("line {line}, column {column}: {}", e.message())
}

/// Follows the config file for the daemon: re-reads it on every poll,
/// reloads when the contents change, and keeps the last config that loaded
/// without errors.
///
/// Comparing contents rather than mtimes also catches Home Manager's symlink
/// swaps, where every generation's file has mtime 1.
pub(crate) struct ConfigWatcher {
    /// `None` when there is no config directory; the defaults then apply.
    path: Option<PathBuf>,
    /// The user named the path, so a missing file is an error.
    explicit: bool,
    last_read: Option<Result<Option<String>, String>>,
    good: Option<ResolvedConfig>,
}

impl ConfigWatcher {
    pub(crate) fn new(path_override: Option<&Path>) -> Self {
        let path = config_path(path_override);
        let good = path.is_none().then(|| {
            let config = load_config(None);
            print_diagnostics(&config);
            config
        });
        Self {
            path,
            explicit: path_override.is_some(),
            last_read: None,
            good,
        }
    }

    /// The config to act on: the newest one that loaded without errors, or
    /// `None` while none has.
    ///
    /// Prints the diagnostics to stderr once per change of the contents.
    pub(crate) fn poll(&mut self) -> Option<&ResolvedConfig> {
        let Some(path) = &self.path else {
            return self.good.as_ref();
        };
        let read = read_config(path, self.explicit);
        if self.last_read.as_ref() != Some(&read) {
            let config = from_contents(path, &read);
            print_diagnostics(&config);
            if !config.has_errors() {
                self.good = Some(config);
            } else if self.good.is_some() {
                // The diagnostic says "using defaults", which the daemon does not do.
                eprintln!("config: keeping the last config that loaded");
            } else {
                eprintln!("config: nothing has loaded yet, waiting for a config without errors");
            }
            self.last_read = Some(read);
        }
        self.good.as_ref()
    }
}

fn print_diagnostics(config: &ResolvedConfig) {
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
}

/// Build the config source for the daemon's cleanup loop.
///
/// The returned closure yields the current workspace prefix and on-delete
/// hooks while `auto_delete_empty` is enabled, or `None` to skip cleanup.
/// Each call goes through a [`ConfigWatcher`], so daemon behavior follows
/// config edits without a restart, a broken edit keeps the last config that
/// loaded, and a config broken from the start skips cleanup until it loads.
pub fn cleanup_source(
    path_override: Option<&Path>,
) -> impl FnMut() -> Option<CleanupConfig> + Send + 'static {
    let mut watcher = ConfigWatcher::new(path_override);
    // Report startup problems now rather than at the first cleanup pass.
    watcher.poll();
    move || {
        watcher
            .poll()
            .filter(|config| config.auto_delete_empty)
            .map(|config| CleanupConfig {
                prefix: config.workspace_prefix.clone(),
                on_delete: config.hooks.on_delete.clone(),
            })
    }
}

/// Load the config from `path_override` or the default location.
///
/// Never fails and prints nothing: problems land in
/// [`ResolvedConfig::diagnostics`], and whatever they affect falls back to
/// the defaults.
pub fn load_config(path_override: Option<&Path>) -> ResolvedConfig {
    let Some(path) = config_path(path_override) else {
        let mut config = default_config();
        config.diagnostics.push(Diagnostic::warning(
            "could not determine the config directory, using defaults",
        ));
        return config;
    };
    from_contents(&path, &read_config(&path, path_override.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

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
        assert_eq!(parse_modifier("Mod"), Some(ModifierType::SUPER_MASK));
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
    fn parse_keybind_lowercases_letter_keys() {
        assert_eq!(parse_keybind("Ctrl+C").unwrap().key, Key::c);
        let kb = parse_keybind("Ctrl+Shift+Q").unwrap();
        assert_eq!(kb.key, Key::q);
        assert_eq!(
            kb.modifiers,
            ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK
        );
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
        assert_eq!(resolved.theme, Theme::default());
        assert!(resolved.templates.is_empty());
        assert!(!resolved.hide_empty_static);
        assert!(resolved.inhibit_compositor_shortcuts);
        assert!(resolved.confirm_delete);
    }

    #[test]
    fn resolve_inhibit_compositor_shortcuts_disabled() {
        let config = Config {
            general: GeneralConfig {
                inhibit_compositor_shortcuts: false,
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(!resolved.inhibit_compositor_shortcuts);
    }

    #[test]
    fn resolve_confirm_delete_disabled() {
        let config = Config {
            general: GeneralConfig {
                confirm_delete: false,
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(!resolved.confirm_delete);
    }

    #[test]
    fn resolve_hide_empty_static() {
        let config = Config {
            general: GeneralConfig {
                hide_empty_static: true,
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(resolved.hide_empty_static);
    }

    #[test]
    fn resolve_invalid_close_keybind_produces_warning() {
        let config = Config {
            keybinds: KeybindsConfig {
                close: vec!["Bogus+x".to_string(), "Escape".to_string()],
            },
            ..Config::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Bogus+x"));
        assert_eq!(resolved.close_keybinds.len(), 1);
    }

    /// Resolve `[keybinds] close = binds`.
    fn resolve_close(binds: &[&str]) -> (ResolvedConfig, Vec<String>) {
        Config {
            keybinds: KeybindsConfig {
                close: binds.iter().map(ToString::to_string).collect(),
            },
            ..Config::default()
        }
        .resolve()
    }

    #[test]
    fn resolve_bare_close_keybind_shadowing_warns() {
        for (bind, ch) in [("q", 'q'), ("Q", 'q'), ("7", '7')] {
            let (resolved, warnings) = resolve_close(&["Escape", bind]);
            assert_eq!(
                warnings,
                vec![format!(
                    "close keybind '{bind}' shadows workspace key '{ch}'"
                )]
            );
            assert_eq!(resolved.close_keybinds.len(), 2);
        }
    }

    #[test]
    fn resolve_super_close_keybind_shadowing_warns() {
        for bind in ["Super+q", "Mod+q", "Mod4+q"] {
            let (resolved, warnings) = resolve_close(&[bind]);
            assert_eq!(warnings.len(), 1, "{bind}: {warnings:?}");
            assert!(warnings[0].contains("shadows workspace key 'q'"));
            assert_eq!(
                resolved.close_keybinds[0].modifiers,
                ModifierType::SUPER_MASK
            );
        }
    }

    #[test]
    fn resolve_modified_close_keybinds_do_not_warn() {
        let binds = [
            "Ctrl+q",
            "Shift+q",
            "Alt+q",
            "Super+Ctrl+q",
            "Escape",
            "F1",
            "space",
        ];
        let (resolved, warnings) = resolve_close(&binds);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.close_keybinds.len(), binds.len());
    }

    #[test]
    fn resolve_all_invalid_close_keybinds_fall_back() {
        let (resolved, warnings) = resolve_close(&["Esc", "Bogus+x"]);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(
            warnings[2].contains("using the defaults"),
            "{}",
            warnings[2]
        );
        let (defaults, _) = Config::default().resolve();
        assert_eq!(resolved.close_keybinds.len(), defaults.close_keybinds.len());
    }

    #[test]
    fn resolve_empty_close_list_stays_empty() {
        let (resolved, warnings) = resolve_close(&[]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(resolved.close_keybinds.is_empty());
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
        workspace.insert(String::new(), WorkspaceEntry::default());
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
                ..WorkspaceEntry::default()
            },
        );
        workspace.insert(
            "b".to_string(),
            WorkspaceEntry {
                name: Some("Terminal".to_string()),
                programs: Vec::new(),
                ..WorkspaceEntry::default()
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

    // --- Static workspace mappings ---

    #[test]
    fn resolve_static_workspace_mapping() {
        let toml_str = r#"
[workspace.q]
static = "01"

[workspace.w]
static = "02"
name = "Web"

[workspace.e]
static = "03"
name = ""
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(resolved.static_workspaces[&'q'], "01");
        assert_eq!(resolved.static_workspaces[&'w'], "02");
        assert_eq!(resolved.static_workspaces[&'e'], "03");
        // Display name kept, no dyn programs registered
        assert_eq!(resolved.workspace_names[&'w'], "Web");
        // Missing or empty name → no display name
        assert!(!resolved.workspace_names.contains_key(&'q'));
        assert!(!resolved.workspace_names.contains_key(&'e'));
        assert!(resolved.workspace_programs.is_empty());
    }

    #[test]
    fn resolve_static_with_programs_warns_and_ignores_programs() {
        let toml_str = r#"
[workspace.q]
static = "01"
programs = ["firefox"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'programs' are ignored"));
        assert_eq!(resolved.static_workspaces[&'q'], "01");
        assert!(resolved.workspace_programs.is_empty());
    }

    #[test]
    fn toml_integer_static_target() {
        let config = parse_config("[workspace.q]\nstatic = 1\n", Path::new("c.toml"));
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        assert_eq!(config.static_workspaces[&'q'], "1");
    }

    #[test]
    fn resolve_static_empty_target_warns() {
        let toml_str = r#"
[workspace.q]
static = ""
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (resolved, warnings) = config.resolve();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'static' is empty"));
        assert!(resolved.static_workspaces.is_empty());
    }

    /// Resolve a config pinning key q to `target`, with `extra` [general] lines.
    fn resolve_pin(target: &str, extra: &str) -> (ResolvedConfig, Vec<String>) {
        let toml_str = format!("[general]\n{extra}\n[workspace.q]\nstatic = {target:?}\n");
        toml::from_str::<Config>(&toml_str).unwrap().resolve()
    }

    #[test]
    fn resolve_static_prefix_target_warns() {
        for target in ["dyn-a", "dyn-a Title", "DYN-A"] {
            let (resolved, warnings) = resolve_pin(target, "");
            assert_eq!(warnings.len(), 1, "{target}: {warnings:?}");
            assert!(
                warnings[0].contains("is a dynamic workspace name"),
                "{}",
                warnings[0]
            );
            assert!(resolved.static_workspaces.is_empty());
        }
    }

    #[test]
    fn resolve_static_lookalike_target_accepted() {
        for target in ["dyn-alpha", "dyn-", "dyn-a-b"] {
            let (resolved, warnings) = resolve_pin(target, "");
            assert!(warnings.is_empty(), "{target}: {warnings:?}");
            assert_eq!(resolved.static_workspaces[&'q'], target);
        }
    }

    #[test]
    fn resolve_empty_prefix_falls_back() {
        for prefix in ["", "  "] {
            let (resolved, warnings) =
                resolve_pin("main", &format!("workspace_prefix = {prefix:?}"));
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("workspace_prefix"), "{}", warnings[0]);
            assert_eq!(resolved.workspace_prefix, "dyn-");
        }
    }

    #[test]
    fn resolve_empty_prefix_keeps_static_pins() {
        let (resolved, warnings) = resolve_pin("1", "workspace_prefix = \"\"");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(resolved.static_workspaces[&'q'], "1");
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
        let mut template = IndexMap::new();
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
        // In insertion order
        assert_eq!(resolved.templates[0].name, "dev");
        assert_eq!(resolved.templates[1].name, "browser");
        // Explicit key preserved
        assert_eq!(resolved.templates[0].key, Some('d'));
        // Auto-assigned key (starts at '2', since '1' is reserved for Empty)
        assert_eq!(resolved.templates[1].key, Some('2'));
    }

    #[test]
    fn resolve_templates_empty_programs_warns() {
        let mut template = IndexMap::new();
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
        let mut template = IndexMap::new();
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
        let mut template = IndexMap::new();
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
        // The first declared keeps the key, the second gets auto-assigned
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
                let mut m = IndexMap::new();
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
                let mut m = IndexMap::new();
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
        let mut template = IndexMap::new();
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
        let mut template = IndexMap::new();
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
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("key '1' is reserved"),
            "{}",
            warnings[0]
        );
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
        assert_eq!(resolved.templates[0].name, "dev");
        assert_eq!(resolved.templates[1].name, "browser");
        assert_eq!(resolved.templates[0].key, Some('d'));
        assert_eq!(resolved.templates[0].programs, vec!["kitty", "code ."]);
    }

    #[test]
    fn toml_templates_keep_declaration_order() {
        let config = parse_config(
            "[template.zeta]\nprograms = [\"z\"]\n\n[template.alpha]\nprograms = [\"a\"]\n",
            Path::new("c.toml"),
        );
        let names: Vec<_> = config.templates.iter().map(|t| t.name.as_str()).collect();
        let keys: Vec<_> = config.templates.iter().map(|t| t.key).collect();
        assert_eq!(names, ["zeta", "alpha"]);
        assert_eq!(keys, [Some('2'), Some('3')]);
    }

    #[test]
    fn toml_variables_keep_declaration_order_for_title() {
        let config = parse_config(
            r#"
[template.dev]
programs = ["code {{project}}", "git switch {{branch}}"]

[template.dev.variables.project]
name = "Project"
type = "dir"
dirs = ["~/dev"]

[template.dev.variables.branch]
name = "Branch"
type = "options"
options = ["main"]
"#,
            Path::new("c.toml"),
        );
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let variables = &config.templates[0].variables;
        assert_eq!(variables[0].name, "project");
        assert_eq!(variables[1].name, "branch");
        let values = HashMap::from([
            ("project".to_string(), "/home/me/dev/ndw".to_string()),
            ("branch".to_string(), "main".to_string()),
        ]);
        assert_eq!(
            resolve_workspace_title(None, variables, &values),
            Some("ndw".to_string())
        );
    }

    #[test]
    fn toml_integer_template_key() {
        let config = parse_config(
            "[template.dev]\nprograms = [\"kitty\"]\nkey = 7\n",
            Path::new("c.toml"),
        );
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        // Not '2', which the template would get automatically.
        assert_eq!(config.templates[0].key, Some('7'));
    }

    #[test]
    fn toml_template_without_key_gets_one() {
        let config = parse_config(
            "[template.dev]\nprograms = [\"kitty\"]\n",
            Path::new("c.toml"),
        );
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        assert_eq!(config.templates[0].key, Some('2'));
    }

    #[test]
    fn load_bool_template_key_is_error() {
        let config = parse_config(
            "[template.dev]\nkey = true\nprograms = [\"kitty\"]\n",
            Path::new("c.toml"),
        );
        assert!(config.has_errors());
        let message = &config.diagnostics[0].message;
        assert!(message.contains("line 2"), "{message}");
        assert!(
            message.contains("expected a string or an integer"),
            "{message}"
        );
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

    // --- parse_dynamic_name ---

    #[test]
    fn parse_dynamic_name_accepts_bare_and_titled() {
        assert_eq!(parse_dynamic_name("dyn-a", "dyn-"), Some(('a', None)));
        assert_eq!(parse_dynamic_name("dyn-7", "dyn-"), Some(('7', None)));
        assert_eq!(
            parse_dynamic_name("dyn-a My Project", "dyn-"),
            Some(('a', Some("My Project")))
        );
        assert_eq!(parse_dynamic_name("dyn-a ", "dyn-"), Some(('a', None)));
    }

    #[test]
    fn parse_dynamic_name_rejects_lookalikes() {
        assert_eq!(parse_dynamic_name("dyn-alpha", "dyn-"), None);
        assert_eq!(parse_dynamic_name("dyn-A", "dyn-"), None);
        assert_eq!(parse_dynamic_name("dyn-", "dyn-"), None);
        assert_eq!(parse_dynamic_name("other-a", "dyn-"), None);
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

    // --- substitute ---

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn substitute_replaces_placeholders() {
        let values = values(&[("cmd", "code"), ("arg", "/tmp")]);
        assert_eq!(substitute("{{cmd}} {{ arg }}!", &values), "code /tmp!");
    }

    #[test]
    fn substitute_leaves_unknown_and_unterminated_placeholders() {
        let values = values(&[("a", "1")]);
        assert_eq!(substitute("{{a}} {{b}} {{a", &values), "1 {{b}} {{a");
    }

    #[test]
    fn substitute_does_not_rescan_inserted_values() {
        let values = values(&[("a", "{{b}}"), ("b", "{{a}}")]);
        assert_eq!(substitute("{{a}} {{b}}", &values), "{{b}} {{a}}");
    }

    // --- build_argv ---

    #[test]
    fn build_argv_value_with_spaces_stays_one_argument() {
        let values = values(&[("path", "/home/me/my project")]);
        assert_eq!(
            build_argv("code {{path}}", &values).unwrap(),
            vec!["code", "/home/me/my project"]
        );
    }

    #[test]
    fn build_argv_placeholder_inside_quotes() {
        let values = values(&[("x", "it's a \"test\"")]);
        assert_eq!(
            build_argv("kitty --title 'ws: {{x}}'", &values).unwrap(),
            vec!["kitty", "--title", "ws: it's a \"test\""]
        );
    }

    #[test]
    fn build_argv_spaced_placeholder_is_one_word() {
        let values = values(&[("path", "/tmp")]);
        assert_eq!(
            build_argv("code {{ path }}", &values).unwrap(),
            vec!["code", "/tmp"]
        );
    }

    #[test]
    fn build_argv_empty_value_keeps_the_argument() {
        let values = values(&[("x", "")]);
        assert_eq!(build_argv("echo {{x}}", &values).unwrap(), vec!["echo", ""]);
    }

    #[test]
    fn build_argv_rejects_unbalanced_quotes() {
        assert!(build_argv("code 'unclosed", &HashMap::new()).is_err());
    }

    /// The README's safe form for a shell program: the value is `$1`, never script text.
    #[test]
    fn build_argv_positional_shell_idiom_keeps_value_out_of_script() {
        let readme: toml::Value =
            toml::from_str(r#"p = '''sh -c 'cd "$1" && exec nvim' sh {{project}}'''"#).unwrap();
        let values = values(&[("project", "Bob's notes")]);
        assert_eq!(
            build_argv(readme["p"].as_str().unwrap(), &values).unwrap(),
            vec!["sh", "-c", r#"cd "$1" && exec nvim"#, "sh", "Bob's notes"]
        );
    }

    // --- load_config and diagnostics ---

    /// Write `contents` to a per-process temp file named after `tag`.
    fn temp_config(tag: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ndw-config-test-{}-{tag}.toml", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn parse_config_syntax_error_is_error() {
        let config = parse_config("[general\n", Path::new("/cfg/config.toml"));
        assert_eq!(config.workspace_prefix, "dyn-");
        assert_eq!(config.diagnostics.len(), 1, "{:?}", config.diagnostics);
        let d = &config.diagnostics[0];
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("/cfg/config.toml"), "{}", d.message);
        assert!(d.message.contains("line 1, column 9"), "{}", d.message);
    }

    #[test]
    fn parse_config_type_error_reports_position() {
        let config = parse_config(
            "[general]\nauto_delete_empty = \"no\"\n",
            Path::new("config.toml"),
        );
        assert!(config.auto_delete_empty);
        assert_eq!(config.diagnostics.len(), 1, "{:?}", config.diagnostics);
        let d = &config.diagnostics[0];
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("line 2, column 21"), "{}", d.message);
        assert!(d.message.contains("expected a boolean"), "{}", d.message);
    }

    #[test]
    fn parse_config_resolve_warnings_become_diagnostics() {
        let config = parse_config("[general]\nlayout = \"workman\"\n", Path::new("c.toml"));
        assert_eq!(
            config.diagnostics,
            vec![Diagnostic::warning(
                "unknown layout 'workman', defaulting to qwerty"
            )]
        );
    }

    #[test]
    fn parse_config_clean_file_has_no_diagnostics() {
        let config = parse_config(
            "[general]\nworkspace_prefix = \"ws-\"\n",
            Path::new("c.toml"),
        );
        assert_eq!(config.workspace_prefix, "ws-");
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    }

    #[test]
    fn parse_config_anchors_theme_file_at_config_dir() {
        let config = parse_config(
            "[general]\ntheme = \"mine.css\"\n",
            Path::new("/cfg/c.toml"),
        );
        assert_eq!(config.theme, Theme::File("/cfg/mine.css".into()));
    }

    #[test]
    fn load_unknown_keys_warn() {
        for (text, expected) in [
            (
                "layout = \"dvorak\"\n",
                "unknown key 'layout'; general settings go under [general]",
            ),
            (
                "[general]\nhover_previw = false\n",
                "unknown key 'general.hover_previw'",
            ),
            (
                "[templates.dev]\nprograms = [\"kitty\"]\n",
                "unknown key 'templates'; did you mean [template]?",
            ),
            (
                "[workspace.q]\nprogram = \"firefox\"\n",
                "unknown key 'workspace.q.program'",
            ),
            (
                "[template.zeta]\nprograms = [\"kitty\"]\non_delete = [\"x\"]\n",
                "unknown key 'template.zeta.on_delete'; templates only support on_create",
            ),
            (
                "[template.zeta]\nprograms = [\"kitty {{b}}\"]\n\
                 [template.zeta.variables.b]\nlabel = \"Branch\"\n",
                "unknown key 'template.zeta.variables.b.label'",
            ),
        ] {
            let config = parse_config(text, Path::new("c.toml"));
            assert_eq!(
                config.diagnostics.first(),
                Some(&Diagnostic::warning(expected)),
                "{text}"
            );
        }
    }

    #[test]
    fn load_unknown_key_keeps_the_rest() {
        let config = parse_config(
            "[general]\nworkspace_prefix = \"ws-\"\nhover_previw = false\n",
            Path::new("c.toml"),
        );
        assert_eq!(config.workspace_prefix, "ws-");
        assert!(config.hover_preview);
        assert!(!config.has_errors());
    }

    #[test]
    fn unknown_key_warning_hints() {
        assert_eq!(
            unknown_key_warning("workspaces"),
            "unknown key 'workspaces'; did you mean [workspace]?"
        );
        assert_eq!(
            unknown_key_warning("hook"),
            "unknown key 'hook'; did you mean [hooks]?"
        );
        assert_eq!(
            unknown_key_warning("keybind"),
            "unknown key 'keybind'; did you mean [keybinds]?"
        );
        assert_eq!(
            unknown_key_warning("theme"),
            "unknown key 'theme'; general settings go under [general]"
        );
        // General settings misplaced deeper, and on_delete outside a template, get no hint.
        assert_eq!(
            unknown_key_warning("keybinds.theme"),
            "unknown key 'keybinds.theme'"
        );
        assert_eq!(
            unknown_key_warning("workspace.a.on_delete"),
            "unknown key 'workspace.a.on_delete'"
        );
    }

    /// Sets every key the config reads, in every section.
    const FULL_CONFIG: &str = r#"
[general]
workspace_prefix = "ws-"
default_programs = ["kitty"]
auto_delete_empty = false
layout = "dvorak"
hover_preview = false
hide_empty_static = true
inhibit_compositor_shortcuts = false
confirm_delete = false
theme = "nord"

[keybinds]
close = ["Escape"]

[hooks]
on_create = ["notify-send created"]
on_delete = ["notify-send deleted"]

[workspace.a]
name = "Browser"
programs = ["firefox"]

[workspace.q]
static = "main"
name = "Main"

[template.dev]
programs = ["code {{project}}", "kitty {{branch}} {{tool}} {{note}}"]
key = "d"
title = "{{project}}"
on_create = ["true"]

[template.dev.variables.project]
name = "Project"
type = "dir"
dirs = ["~/dev"]
depth = 2

[template.dev.variables.branch]
name = "Branch"
type = "options"
options = ["main"]

[template.dev.variables.tool]
name = "Tool"
type = "command"
command = "ls"

[template.dev.variables.note]
name = "Note"
"#;

    #[test]
    fn load_full_config_has_no_unknown_keys() {
        let config = parse_config(FULL_CONFIG, Path::new("c.toml"));
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    }

    /// The keys serde reads for struct `T`, taken from its derived `Deserialize`.
    fn struct_fields<T: serde::de::DeserializeOwned>() -> BTreeSet<&'static str> {
        struct Capture<'a>(&'a mut &'static [&'static str]);

        impl<'de> serde::Deserializer<'de> for Capture<'_> {
            type Error = serde::de::value::Error;

            fn deserialize_any<V: serde::de::Visitor<'de>>(
                self,
                _: V,
            ) -> Result<V::Value, Self::Error> {
                Err(serde::de::Error::custom("not a struct"))
            }

            fn deserialize_struct<V: serde::de::Visitor<'de>>(
                self,
                _: &'static str,
                fields: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Self::Error> {
                *self.0 = fields;
                Err(serde::de::Error::custom("fields captured"))
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq tuple
                tuple_struct map enum identifier ignored_any
            }
        }

        let mut fields: &'static [&'static str] = &[];
        assert!(T::deserialize(Capture(&mut fields)).is_err());
        fields.iter().copied().collect()
    }

    /// The keys of every table in `values`.
    fn table_keys<'a>(values: impl IntoIterator<Item = &'a toml::Value>) -> BTreeSet<&'a str> {
        values
            .into_iter()
            .flat_map(|value| value.as_table().expect("a table").keys())
            .map(String::as_str)
            .collect()
    }

    /// Keeps [`FULL_CONFIG`] complete, so a new or renamed field fails
    /// [`load_full_config_has_no_unknown_keys`] until the fixture has it.
    #[test]
    fn full_config_sets_every_field() {
        let root: toml::Value = toml::from_str(FULL_CONFIG).unwrap();
        let workspaces = root["workspace"].as_table().unwrap().values();
        let templates: Vec<_> = root["template"].as_table().unwrap().values().collect();
        let variables = templates
            .iter()
            .flat_map(|template| template["variables"].as_table().unwrap().values());
        assert_eq!(table_keys([&root]), struct_fields::<Config>());
        assert_eq!(
            table_keys([&root["general"]]),
            struct_fields::<GeneralConfig>()
        );
        assert_eq!(
            table_keys([&root["keybinds"]]),
            struct_fields::<KeybindsConfig>()
        );
        assert_eq!(table_keys([&root["hooks"]]), struct_fields::<HooksConfig>());
        assert_eq!(table_keys(workspaces), struct_fields::<WorkspaceEntry>());
        assert_eq!(
            table_keys(templates.iter().copied()),
            struct_fields::<TemplateEntry>()
        );
        assert_eq!(table_keys(variables), struct_fields::<VariableEntry>());
    }

    #[test]
    fn general_keys_match_general_config() {
        let keys: BTreeSet<&str> = GENERAL_KEYS.iter().copied().collect();
        assert_eq!(keys, struct_fields::<GeneralConfig>());
    }

    #[test]
    fn toml_error_summary_without_span_uses_message() {
        let e = <toml::de::Error as serde::de::Error>::custom("boom");
        assert_eq!(toml_error_summary(&e, "anything"), "boom");
    }

    #[test]
    fn toml_error_summary_counts_characters_not_bytes() {
        let text = "[general]\nlayout = \"æøå\" x\n";
        let e = toml::from_str::<Config>(text).err().unwrap();
        assert!(
            toml_error_summary(&e, text).starts_with("line 2, column 16:"),
            "{}",
            toml_error_summary(&e, text)
        );
    }

    #[test]
    fn diagnostic_display_prefixes() {
        assert_eq!(
            Diagnostic::error("bad").to_string(),
            "config error: bad".to_string()
        );
        assert_eq!(
            Diagnostic::warning("odd").to_string(),
            "config warning: odd".to_string()
        );
    }

    #[test]
    fn read_config_missing_default_path_is_none() {
        let path = temp_config("missing", "");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_config(&path, false), Ok(None));
        assert!(read_config(&path, true).is_err());
    }

    #[test]
    fn load_config_missing_explicit_path_is_error() {
        let path = temp_config("typo", "");
        std::fs::remove_file(&path).unwrap();
        let config = load_config(Some(&path));
        assert!(config.has_errors(), "{:?}", config.diagnostics);
        assert!(
            config.diagnostics[0].message.contains("could not read"),
            "{:?}",
            config.diagnostics
        );
    }

    #[test]
    fn read_config_unreadable_path_is_error() {
        // A directory exists but cannot be read as a file.
        let err = read_config(&std::env::temp_dir(), false).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
        let config = from_contents(Path::new("dir"), &Err(err));
        assert_eq!(config.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn load_config_reads_file() {
        let path = temp_config("reads", "[general]\nworkspace_prefix = \"rd-\"\n");
        let config = load_config(Some(&path));
        std::fs::remove_file(&path).ok();
        assert_eq!(config.workspace_prefix, "rd-");
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    }

    // --- cleanup_source ---

    #[test]
    fn cleanup_source_reloads_on_content_change() {
        let path = temp_config("reload", "[general]\nworkspace_prefix = \"aaa-\"\n");

        let mut source = cleanup_source(Some(&path));
        assert_eq!(source().map(|c| c.prefix), Some("aaa-".to_string()));

        std::fs::write(
            &path,
            "[general]\nworkspace_prefix = \"bbb-\"\nauto_delete_empty = false\n",
        )
        .unwrap();
        assert_eq!(source().map(|c| c.prefix), None);

        std::fs::write(&path, "[general]\nworkspace_prefix = \"ccc-\"\n").unwrap();
        assert_eq!(source().map(|c| c.prefix), Some("ccc-".to_string()));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cleanup_source_follows_symlink_swap_with_same_mtime() {
        let dir = std::env::temp_dir().join(format!("ndw-config-test-{}-swap", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, link) = (dir.join("a.toml"), dir.join("b.toml"), dir.join("cfg.toml"));
        std::fs::write(&a, "[general]\nworkspace_prefix = \"aaa-\"\n").unwrap();
        std::fs::write(
            &b,
            "[general]\nworkspace_prefix = \"bbb-\"\nauto_delete_empty = false\n",
        )
        .unwrap();
        // Like the Nix store, where Home Manager's generations live.
        let store_mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        for file in [&a, &b] {
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(store_mtime)
                .unwrap();
        }
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(&a, &link).unwrap();

        let mut source = cleanup_source(Some(&link));
        assert_eq!(source().map(|c| c.prefix), Some("aaa-".to_string()));

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&b, &link).unwrap();
        assert_eq!(source().map(|c| c.prefix), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_source_keeps_last_good_config_on_parse_error() {
        let path = temp_config("keep", "[general]\nauto_delete_empty = false\n");

        let mut source = cleanup_source(Some(&path));
        assert_eq!(source().map(|c| c.prefix), None);

        // Falling back to the defaults would turn auto-delete back on.
        std::fs::write(&path, "[general\n").unwrap();
        assert_eq!(source().map(|c| c.prefix), None);

        std::fs::write(&path, "[general]\nworkspace_prefix = \"ccc-\"\n").unwrap();
        assert_eq!(source().map(|c| c.prefix), Some("ccc-".to_string()));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cleanup_source_broken_at_startup_skips_cleanup() {
        let path = temp_config("broken", "[general\n");
        let mut source = cleanup_source(Some(&path));
        assert_eq!(source().map(|c| c.prefix), None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_watcher_accepts_warnings() {
        let path = temp_config("warned", "[general]\nlayout = \"workman\"\n");
        let mut watcher = ConfigWatcher::new(Some(&path));
        let config = watcher.poll().expect("warnings still load");
        assert_eq!(config.diagnostics.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cleanup_source_missing_explicit_file_skips_cleanup() {
        let path = temp_config("later", "");
        std::fs::remove_file(&path).unwrap();

        // A mistyped --config must not clean up with the default settings.
        let mut source = cleanup_source(Some(&path));
        assert_eq!(source().map(|c| c.prefix), None);

        std::fs::write(&path, "[general]\nworkspace_prefix = \"ddd-\"\n").unwrap();
        assert_eq!(source().map(|c| c.prefix), Some("ddd-".to_string()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cleanup_source_carries_on_delete_hooks() {
        let path = temp_config("cleanup-hooks", "[hooks]\non_delete = [\"x\"]\n");

        let mut source = cleanup_source(Some(&path));
        assert_eq!(source().map(|c| c.on_delete), Some(vec!["x".to_string()]));

        std::fs::write(&path, "[hooks]\non_delete = [\"y\"]\n").unwrap();
        assert_eq!(source().map(|c| c.on_delete), Some(vec!["y".to_string()]));

        std::fs::remove_file(&path).ok();
    }

    // --- Template variable resolution ---

    #[test]
    fn resolve_templates_with_variables() {
        let mut variables = IndexMap::new();
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
        let mut template = IndexMap::new();
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
        // In insertion order
        assert_eq!(tmpl.variables[0].name, "path");
        assert_eq!(tmpl.variables[0].label, "Project path");
        assert_eq!(tmpl.variables[0].var_type, VariableType::Text);
        assert_eq!(tmpl.variables[1].name, "branch");
        assert_eq!(tmpl.variables[1].label, "Git branch");
    }

    // --- Variable type resolution ---

    /// Resolve a single variable entry through config resolution and return
    /// the resulting `(VariableType, Vec<warnings>)`.
    fn resolve_single_variable(var_entry: VariableEntry) -> (VariableType, Vec<String>) {
        let mut variables = IndexMap::new();
        variables.insert("project".to_string(), var_entry);
        let mut template = IndexMap::new();
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
        let mut variables = IndexMap::new();
        variables.insert(
            "path".to_string(),
            VariableEntry {
                name: String::new(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        let mut template = IndexMap::new();
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
        let mut variables = IndexMap::new();
        variables.insert(
            "unused".to_string(),
            VariableEntry {
                name: "Unused var".to_string(),
                variable_type: "text".to_string(),
                ..VariableEntry::default()
            },
        );
        let mut template = IndexMap::new();
        template.insert(
            "dev".to_string(),
            TemplateEntry {
                programs: vec!["kitty".to_string()],
                key: Some("d".to_string()),
                variables,
                // Without a title the first variable would fill it in.
                title: Some("dev".to_string()),
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

    /// Warnings for `text`, a config whose parsing must succeed.
    fn toml_warnings(text: &str) -> Vec<String> {
        toml::from_str::<Config>(text).unwrap().resolve().1
    }

    #[test]
    fn resolve_variable_used_only_in_title_is_referenced() {
        let warnings = toml_warnings(
            r#"
[template.dev]
programs = ["kitty {{path}}"]
title = "{{topic}}"
[template.dev.variables.path]
name = "Path"
[template.dev.variables.topic]
name = "Topic"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn resolve_variable_used_only_in_hook_env_is_referenced() {
        for hooks in [
            "[template.dev]\non_create = ['git -C \"$NDW_VAR_PATH\" status']",
            "[hooks]\non_create = ['echo \"${NDW_VAR_PATH}\"']\n[template.dev]",
        ] {
            let warnings = toml_warnings(&format!(
                "{hooks}\nprograms = [\"kitty\"]\ntitle = \"dev\"\n\
                 [template.dev.variables.path]\nname = \"Path\"\n"
            ));
            assert!(warnings.is_empty(), "{hooks}: {warnings:?}");
        }
    }

    #[test]
    fn resolve_longer_hook_env_name_is_not_a_reference() {
        let warnings = toml_warnings(
            r#"
[template.dev]
programs = ["kitty"]
title = "dev"
on_create = ['echo "$NDW_VAR_PATHS"']
[template.dev.variables.path]
name = "Path"
"#,
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("'path' is never referenced"));
    }

    #[test]
    fn mentions_env_var_matches_whole_words() {
        assert!(mentions_env_var("echo $NDW_VAR_X", "NDW_VAR_X"));
        assert!(mentions_env_var("echo \"${NDW_VAR_X}\"", "NDW_VAR_X"));
        assert!(mentions_env_var(
            "echo $NDW_VAR_X/a $NDW_VAR_XY",
            "NDW_VAR_X"
        ));
        assert!(!mentions_env_var("echo $NDW_VAR_XY", "NDW_VAR_X"));
        assert!(!mentions_env_var("echo $MY_NDW_VAR_X", "NDW_VAR_X"));
    }

    #[test]
    fn resolve_first_variable_feeds_auto_title() {
        let warnings = toml_warnings(
            r#"
[template.dev]
programs = ["kitty"]
[template.dev.variables.branch]
name = "Branch"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn resolve_undefined_title_ref_warns_once() {
        let warnings = toml_warnings(
            r#"
[template.dev]
programs = ["kitty"]
title = "{{nope}}"
"#,
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("title references undefined variable"));
    }

    #[test]
    fn resolve_unbalanced_quotes_warn() {
        for (text, context) in [
            (
                "[general]\ndefault_programs = [\"kitty 'x\"]\n",
                "[general] default_programs",
            ),
            (
                "[workspace.a]\nprograms = [\"kitty 'x\"]\n",
                "[workspace.a]",
            ),
            (
                "[template.dev]\nprograms = [\"kitty 'x\"]\n",
                "template 'dev'",
            ),
        ] {
            let (resolved, warnings) = toml::from_str::<Config>(text).unwrap().resolve();
            assert_eq!(
                warnings,
                vec![format!(
                    "{context}: cannot split program `kitty 'x` (missing closing quote)"
                )]
            );
            // Kept: creating the workspace reports the same problem.
            let kept = resolved
                .default_programs
                .iter()
                .chain(resolved.workspace_programs.values().flatten())
                .chain(resolved.templates.iter().flat_map(|t| &t.programs));
            assert_eq!(kept.count(), 1, "{text}");
        }
    }

    #[test]
    fn resolve_quoted_placeholders_split_cleanly() {
        let warnings = toml_warnings(
            r#"
[template.dev]
programs = ["kitty --title 'ws: {{ path }}'"]
[template.dev.variables.path]
name = "Path"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn resolve_templates_undefined_reference_warns() {
        let mut template = IndexMap::new();
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

    /// Warnings for template `dev` running `program` with text variables `names`.
    fn variable_warnings(program: &str, names: &[&str]) -> Vec<String> {
        use std::fmt::Write as _;

        let mut toml_str = format!("[template.dev]\nprograms = [{program:?}]\n");
        for name in names {
            writeln!(
                toml_str,
                "[template.dev.variables.{name}]\nname = \"{name}\""
            )
            .unwrap();
        }
        let config: Config = toml::from_str(&toml_str).unwrap();
        config.resolve().1
    }

    #[test]
    fn resolve_templates_colliding_hook_variable_names_warn() {
        for (a, b, env_name) in [
            ("project-dir", "project_dir", "NDW_VAR_PROJECT_DIR"),
            ("PATH", "path", "NDW_VAR_PATH"),
        ] {
            let warnings = variable_warnings(&format!("code {{{{{a}}}}} {{{{{b}}}}}"), &[a, b]);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0].contains(&format!("'{a}'"))
                    && warnings[0].contains(&format!("'{b}'"))
                    && warnings[0].contains(env_name),
                "{}",
                warnings[0]
            );
        }
    }

    #[test]
    fn resolve_templates_hyphenated_variable_no_warning() {
        let warnings = variable_warnings("code {{project-dir}}", &["project-dir"]);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
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

    #[test]
    fn hook_env_var_maps_non_alnum_to_underscore() {
        assert_eq!(hook_env_var("path"), "NDW_VAR_PATH");
        assert_eq!(hook_env_var("project-dir"), "NDW_VAR_PROJECT_DIR");
        assert_eq!(hook_env_var("a.b"), "NDW_VAR_A_B");
        assert_eq!(hook_env_var("n\u{e4}me"), "NDW_VAR_N_ME");
        assert_eq!(hook_env_var("x_1"), "NDW_VAR_X_1");
    }

    #[test]
    fn build_hook_env_uses_shell_safe_names() {
        let vars = HashMap::from([("project-dir".to_string(), "/x".to_string())]);
        let env = build_hook_env("dyn-a", 'a', Some("dev"), &vars);
        assert!(env.contains(&("NDW_VAR_PROJECT_DIR".to_string(), "/x".to_string())));
    }

    #[test]
    fn build_hook_env_orders_variables_by_name() {
        let vars: HashMap<String, String> = ["c", "a", "d", "b"]
            .into_iter()
            .map(|name| (name.to_string(), String::new()))
            .collect();
        let env = build_hook_env("dyn-a", 'a', None, &vars);
        let names: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "NDW_WORKSPACE_NAME",
                "NDW_WORKSPACE_KEY",
                "NDW_TEMPLATE",
                "NDW_VAR_A",
                "NDW_VAR_B",
                "NDW_VAR_C",
                "NDW_VAR_D",
            ]
        );
    }

    // --- collect_create_hooks ---

    fn hooks_test_config(global_hooks: Vec<String>, templates: Vec<Template>) -> ResolvedConfig {
        ResolvedConfig {
            workspace_prefix: "dyn-".to_string(),
            close_keybinds: Vec::new(),
            default_programs: Vec::new(),
            workspace_programs: HashMap::new(),
            workspace_names: HashMap::new(),
            static_workspaces: HashMap::new(),
            auto_delete_empty: true,
            hover_preview: true,
            hide_empty_static: false,
            inhibit_compositor_shortcuts: true,
            confirm_delete: true,
            layout: &LAYOUT_QWERTY,
            theme: Theme::default(),
            templates,
            hooks: HookConfig {
                on_create: global_hooks,
                on_delete: Vec::new(),
            },
            diagnostics: Vec::new(),
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
        let mut template = IndexMap::new();
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

    // --- Theme ---

    fn builtin_name(theme: &Theme) -> Option<&'static str> {
        match theme {
            Theme::Builtin(builtin) => Some(builtin.name),
            Theme::File(_) => None,
        }
    }

    #[test]
    fn theme_parse_builtin_names() {
        assert_eq!(builtin_name(&Theme::default()), Some("gtk"));
        assert_eq!(builtin_name(&Theme::parse("gtk").unwrap()), Some("gtk"));
        assert_eq!(builtin_name(&Theme::parse("dark").unwrap()), Some("dark"));
        assert_eq!(builtin_name(&Theme::parse("Light").unwrap()), Some("light"));
    }

    #[test]
    fn builtin_themes_exclude_gtk_support_files() {
        assert!(BUILTIN_THEMES.iter().all(|t| !t.name.starts_with("gtk-")));
    }

    #[test]
    fn builtin_theme_names_cannot_be_mistaken_for_paths() {
        for theme in BUILTIN_THEMES {
            assert!(
                theme
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "themes/{}.css: use lowercase, digits and dashes",
                theme.name
            );
        }
    }

    /// A palette that leaves a primary out silently inherits it from GTK.
    #[test]
    fn builtin_palettes_set_every_primary() {
        for theme in BUILTIN_THEMES.iter().filter(|t| t.name != "gtk") {
            for primary in ["--bg:", "--fg:", "--accent:", "--urgent:", "--danger:"] {
                assert!(
                    theme.css.contains(primary),
                    "themes/{}.css does not set {primary}",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn theme_parse_paths() {
        assert_eq!(Theme::parse("mine.css"), Ok(Theme::File("mine.css".into())));
        assert_eq!(
            Theme::parse("/etc/ndw/theme.CSS"),
            Ok(Theme::File("/etc/ndw/theme.CSS".into()))
        );
        assert_eq!(
            Theme::parse("themes/mine"),
            Ok(Theme::File("themes/mine".into()))
        );
    }

    #[test]
    fn theme_parse_expands_tilde() {
        let Ok(Theme::File(path)) = Theme::parse("~/themes/mine.css") else {
            panic!("expected a file theme");
        };
        assert!(path.is_absolute());
        assert!(path.ends_with("themes/mine.css"));
    }

    #[test]
    fn theme_parse_unknown_name() {
        assert!(Theme::parse("no-such-theme").is_err());
        assert!(Theme::parse("").is_err());
    }

    #[test]
    fn resolve_unknown_theme_warns_and_defaults() {
        let config = Config {
            general: GeneralConfig {
                theme: "no-such-theme".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let (resolved, warnings) = config.resolve();
        assert_eq!(resolved.theme, Theme::default());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no-such-theme"));
    }

    #[test]
    fn toml_with_theme() {
        let config: Config = toml::from_str("[general]\ntheme = \"dark\"\n").unwrap();
        let (resolved, warnings) = config.resolve();
        assert!(warnings.is_empty());
        assert_eq!(builtin_name(&resolved.theme), Some("dark"));
    }

    // --- expand_tilde ---

    #[test]
    fn expand_tilde_with_home() {
        let result = expand_tilde("~/dev");
        assert!(!result.starts_with('~'));
        assert!(result.ends_with("/dev"));
    }

    #[test]
    fn expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/tmp/foo"), "/tmp/foo");
    }

    #[test]
    fn expand_tilde_only_tilde_slash() {
        let result = expand_tilde("~/");
        assert!(!result.starts_with('~'));
    }
}
