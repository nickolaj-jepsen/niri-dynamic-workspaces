use std::collections::{HashMap, HashSet};

use super::helpers::clean_app_id;
use crate::config::{ResolvedConfig, TemplateVariable};
use crate::niri;

#[derive(Clone, Default)]
pub struct HookInfo {
    pub template_name: Option<String>,
    pub variables: HashMap<String, String>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "four bools represent independent workspace states"
)]
pub struct DynWorkspaceInfo {
    pub char_id: char,
    pub is_focused: bool,
    pub is_active: bool,
    pub is_uncreated: bool,
    pub is_urgent: bool,
    pub name: Option<String>,
    pub window_count: usize,
    pub app_names: Vec<String>,
    pub configured_programs: Vec<String>,
}

impl DynWorkspaceInfo {
    pub fn status_text(&self) -> Option<String> {
        if self.is_focused {
            Some("focused".to_string())
        } else if self.is_active {
            Some("active".to_string())
        } else if !self.is_uncreated && self.window_count == 0 {
            Some("empty".to_string())
        } else if self.window_count > 0 {
            Some(match self.window_count {
                1 => "1 win".to_string(),
                n => format!("{n} win"),
            })
        } else {
            None
        }
    }

    pub fn uncreated(ch: char) -> Self {
        Self {
            char_id: ch,
            is_focused: false,
            is_active: false,
            is_uncreated: true,
            is_urgent: false,
            name: None,
            window_count: 0,
            app_names: Vec::new(),
            configured_programs: Vec::new(),
        }
    }
}

/// An option in the template picker (either "Empty" or a named template).
pub struct TemplateOption {
    pub key: Option<char>,
    pub name: String,
    pub programs: Vec<String>,
    pub variables: Vec<TemplateVariable>,
}

pub fn gather_dyn_workspaces(config: &ResolvedConfig) -> Vec<DynWorkspaceInfo> {
    let workspaces = match niri::list_workspaces() {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("Failed to list workspaces: {e}");
            return Vec::new();
        }
    };

    let windows = match niri::list_windows() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to list windows: {e}");
            Vec::new()
        }
    };

    build_dyn_workspace_infos(&workspaces, &windows, config)
}

pub fn build_dyn_workspace_infos(
    workspaces: &[niri_ipc::Workspace],
    windows: &[niri_ipc::Window],
    config: &ResolvedConfig,
) -> Vec<DynWorkspaceInfo> {
    let prefix = &config.workspace_prefix;

    // Count windows per workspace, track urgency, and collect app names
    let mut window_counts: HashMap<u64, usize> = HashMap::new();
    let mut urgent_ws_ids: HashSet<u64> = HashSet::new();
    let mut ws_app_names: HashMap<u64, Vec<String>> = HashMap::new();
    for w in windows {
        if let Some(ws_id) = w.workspace_id {
            *window_counts.entry(ws_id).or_default() += 1;
            if w.is_urgent {
                urgent_ws_ids.insert(ws_id);
            }
            if let Some(ref app_id) = w.app_id {
                if !app_id.is_empty() {
                    ws_app_names
                        .entry(ws_id)
                        .or_default()
                        .push(clean_app_id(app_id));
                }
            }
        }
    }
    // Sort and deduplicate app names per workspace
    for names in ws_app_names.values_mut() {
        names.sort();
        names.dedup();
    }

    // Find the globally focused workspace
    let focused_ws_id = workspaces.iter().find(|ws| ws.is_focused).map(|ws| ws.id);

    let mut live_chars: HashSet<char> = HashSet::new();

    let mut infos: Vec<DynWorkspaceInfo> = workspaces
        .iter()
        .filter_map(|ws| {
            let ws_name = ws.name.as_ref()?;
            let ch = ws_name.strip_prefix(prefix)?.chars().next()?;
            if !crate::config::is_workspace_char(ch) {
                return None;
            }

            live_chars.insert(ch);

            let is_focused = Some(ws.id) == focused_ws_id;
            let is_active = !is_focused && ws.is_active;

            let count = window_counts.get(&ws.id).copied().unwrap_or(0);
            let name = config.workspace_names.get(&ch).cloned();
            let is_urgent = urgent_ws_ids.contains(&ws.id);
            let app_names = ws_app_names.remove(&ws.id).unwrap_or_default();

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                is_uncreated: false,
                is_urgent,
                name,
                window_count: count,
                app_names,
                configured_programs: Vec::new(),
            })
        })
        .collect();

    // Add uncreated configured workspaces
    let configured_chars: HashSet<char> = config
        .workspace_names
        .keys()
        .chain(config.workspace_programs.keys())
        .copied()
        .collect();

    for ch in configured_chars {
        if live_chars.contains(&ch) {
            continue;
        }
        let mut info = DynWorkspaceInfo::uncreated(ch);
        info.name = config.workspace_names.get(&ch).cloned();
        info.configured_programs = config
            .workspace_programs
            .get(&ch)
            .cloned()
            .unwrap_or_else(|| config.default_programs.clone());
        infos.push(info);
    }

    infos.sort_by_key(|i| i.char_id);
    infos
}

/// Build a map of all keyboard keys to their workspace info.
/// Keys without a live or configured workspace get a default empty entry.
pub fn build_full_keyboard_info(config: &ResolvedConfig) -> HashMap<char, DynWorkspaceInfo> {
    let live_infos = gather_dyn_workspaces(config);
    let mut map: HashMap<char, DynWorkspaceInfo> = HashMap::new();

    for info in live_infos {
        map.insert(info.char_id, info);
    }

    for row in config.layout.rows {
        for &ch in *row {
            map.entry(ch)
                .or_insert_with(|| DynWorkspaceInfo::uncreated(ch));
        }
    }

    map
}

pub fn build_template_options(config: &ResolvedConfig) -> Vec<TemplateOption> {
    let mut options = Vec::with_capacity(config.templates.len() + 1);

    // "Empty" option always gets key '1' (reserved during config resolution)
    options.push(TemplateOption {
        key: Some('1'),
        name: "Empty".to_string(),
        programs: config.default_programs.clone(),
        variables: Vec::new(),
    });

    for tmpl in &config.templates {
        options.push(TemplateOption {
            key: tmpl.key,
            name: tmpl.name.clone(),
            programs: tmpl.programs.clone(),
            variables: tmpl.variables.clone(),
        });
    }

    options
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::config::{HookConfig, Template, LAYOUT_QWERTY};
    use crate::test_helpers::{test_window, test_workspace};

    fn default_test_config() -> ResolvedConfig {
        ResolvedConfig {
            workspace_prefix: "dyn-".to_string(),
            close_keybinds: Vec::new(),
            default_programs: Vec::new(),
            workspace_programs: HashMap::new(),
            workspace_names: HashMap::new(),
            auto_delete_empty: true,
            layout: &LAYOUT_QWERTY,
            templates: Vec::new(),
            hooks: HookConfig::default(),
        }
    }

    // --- Keyboard layout coverage ---

    use crate::config::ALL_LAYOUTS;

    #[test]
    fn all_layouts_have_36_keys() {
        for layout in ALL_LAYOUTS {
            let total: usize = layout.rows.iter().map(|r| r.len()).sum();
            assert_eq!(total, 36, "{} has {total} keys", layout.name);
        }
    }

    #[test]
    fn all_layout_keys_are_valid_workspace_chars() {
        for layout in ALL_LAYOUTS {
            for row in layout.rows {
                for &ch in *row {
                    assert!(
                        crate::config::is_workspace_char(ch),
                        "{}: '{ch}' should be a valid workspace char",
                        layout.name
                    );
                }
            }
        }
    }

    #[test]
    fn all_workspace_chars_present_in_all_layouts() {
        for layout in ALL_LAYOUTS {
            let chars: HashSet<char> = layout.rows.iter().flat_map(|r| r.iter().copied()).collect();
            for ch in 'a'..='z' {
                assert!(
                    chars.contains(&ch),
                    "{}: missing letter '{ch}'",
                    layout.name
                );
            }
            for ch in '0'..='9' {
                assert!(chars.contains(&ch), "{}: missing digit '{ch}'", layout.name);
            }
        }
    }

    #[test]
    fn all_layouts_no_duplicate_keys() {
        for layout in ALL_LAYOUTS {
            let all_chars: Vec<char> = layout.rows.iter().flat_map(|r| r.iter().copied()).collect();
            let unique: HashSet<char> = all_chars.iter().copied().collect();
            assert_eq!(
                all_chars.len(),
                unique.len(),
                "{}: has duplicate keys",
                layout.name
            );
        }
    }

    #[test]
    fn all_layouts_row_offsets_match_rows() {
        for layout in ALL_LAYOUTS {
            assert_eq!(
                layout.row_offsets.len(),
                layout.rows.len(),
                "{}: row_offsets length mismatch",
                layout.name
            );
        }
    }

    // --- build_dyn_workspace_infos ---

    #[test]
    fn build_dyn_workspace_infos_basic() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-b"), false),
            test_workspace(20, Some("dyn-a"), true),
        ];
        let windows = vec![
            test_window(1, 20, "firefox"),
            test_window(2, 20, "kitty"),
            test_window(3, 10, "slack"),
        ];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].char_id, 'a');
        assert_eq!(infos[1].char_id, 'b');
        assert_eq!(infos[0].window_count, 2);
        assert_eq!(infos[1].window_count, 1);
        assert_eq!(infos[0].app_names, vec!["Firefox", "Kitty"]);
        assert_eq!(infos[1].app_names, vec!["Slack"]);
    }

    #[test]
    fn build_dyn_workspace_infos_uncreated() {
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true)];
        let windows = vec![];
        let mut config = default_test_config();
        config.workspace_names.insert('b', "Browser".to_string());
        config
            .workspace_programs
            .insert('b', vec!["firefox".to_string()]);

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 2);
        assert!(!infos[0].is_uncreated);
        assert_eq!(infos[0].char_id, 'a');
        assert!(infos[0].app_names.is_empty());
        assert!(infos[1].is_uncreated);
        assert_eq!(infos[1].char_id, 'b');
        assert_eq!(infos[1].name.as_deref(), Some("Browser"));
        assert_eq!(infos[1].configured_programs, vec!["firefox"]);
        assert!(infos[1].app_names.is_empty());
    }

    #[test]
    fn build_dyn_workspace_infos_ignores_non_prefix() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),
            test_workspace(20, Some("other-b"), false),
            test_workspace(30, None, false),
        ];
        let windows = vec![];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].char_id, 'a');
    }

    #[test]
    fn build_dyn_workspace_infos_urgency() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),
            test_workspace(20, Some("dyn-b"), false),
        ];
        let mut urgent_window = test_window(1, 20, "slack");
        urgent_window.is_urgent = true;
        let windows = vec![test_window(2, 10, "firefox"), urgent_window];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 2);
        assert!(!infos[0].is_urgent);
        assert!(infos[1].is_urgent);
    }

    #[test]
    fn build_dyn_workspace_infos_focused_and_active() {
        let mut ws_active = test_workspace(20, Some("dyn-b"), false);
        ws_active.is_active = true;
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),
            ws_active,
            test_workspace(30, Some("dyn-c"), false),
        ];
        let windows = vec![];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 3);
        assert!(infos[0].is_focused);
        assert!(!infos[0].is_active);
        assert!(!infos[1].is_focused);
        assert!(infos[1].is_active);
        assert!(!infos[2].is_focused);
        assert!(!infos[2].is_active);
    }

    #[test]
    fn build_dyn_workspace_infos_deduplicates_app_names() {
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true)];
        let windows = vec![
            test_window(1, 10, "firefox"),
            test_window(2, 10, "org.mozilla.firefox"),
            test_window(3, 10, "firefox"),
        ];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].app_names, vec!["Firefox"]);
    }

    #[test]
    fn build_dyn_workspace_infos_handles_no_app_id() {
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true)];
        let mut window_no_app = test_window(1, 10, "");
        window_no_app.app_id = None;
        let windows = vec![window_no_app, test_window(2, 10, "kitty")];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].window_count, 2);
        assert_eq!(infos[0].app_names, vec!["Kitty"]);
    }

    // --- status_text ---

    #[test]
    fn status_text_variants() {
        let mut info = DynWorkspaceInfo::uncreated('a');
        assert_eq!(info.status_text(), None);

        info.is_focused = true;
        info.is_uncreated = false;
        assert_eq!(info.status_text().as_deref(), Some("focused"));

        info.is_focused = false;
        info.is_active = true;
        assert_eq!(info.status_text().as_deref(), Some("active"));

        info.is_active = false;
        assert_eq!(info.status_text().as_deref(), Some("empty"));

        info.window_count = 1;
        assert_eq!(info.status_text().as_deref(), Some("1 win"));

        info.window_count = 5;
        assert_eq!(info.status_text().as_deref(), Some("5 win"));
    }

    // --- build_template_options ---

    #[test]
    fn build_template_options_empty_first_with_key_1() {
        let mut config = default_test_config();
        config.default_programs = vec!["kitty".to_string()];
        config.templates = vec![
            Template {
                name: "dev".to_string(),
                programs: vec!["code".to_string()],
                key: Some('d'),
                variables: Vec::new(),
                on_create: Vec::new(),
            },
            Template {
                name: "browser".to_string(),
                programs: vec!["firefox".to_string()],
                key: Some('2'),
                variables: Vec::new(),
                on_create: Vec::new(),
            },
        ];

        let opts = build_template_options(&config);

        assert_eq!(opts.len(), 3);
        assert_eq!(opts[0].name, "Empty");
        assert_eq!(opts[0].key, Some('1'));
        assert_eq!(opts[0].programs, vec!["kitty"]);
        assert_eq!(opts[1].name, "dev");
        assert_eq!(opts[1].key, Some('d'));
        assert_eq!(opts[2].name, "browser");
        assert_eq!(opts[2].key, Some('2'));
    }

    #[test]
    fn build_template_options_no_default_programs() {
        let mut config = default_test_config();
        config.templates = vec![Template {
            name: "dev".to_string(),
            programs: vec!["code".to_string()],
            key: Some('2'),
            variables: Vec::new(),
            on_create: Vec::new(),
        }];

        let opts = build_template_options(&config);

        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].name, "Empty");
        assert!(opts[0].programs.is_empty());
    }
}
