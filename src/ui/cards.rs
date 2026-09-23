use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, EventControllerMotion, GestureClick, Label, Orientation};

use crate::config::ResolvedConfig;
use crate::niri;

use super::metrics::KeyboardMetrics;
use super::{
    dispatch_action, display_key_char, finish, focus_selected, focused_output_from, show_error,
    window_to_move, ActionContext, Mode, OverlaySession,
};

#[derive(Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four bools represent independent workspace states"
)]
pub(super) struct StaticWorkspaceInfo {
    id: u64,
    /// Display label: the name, or the index of an unnamed workspace.
    name: String,
    is_focused: bool,
    is_active: bool,
    is_urgent: bool,
    is_empty: bool,
}

#[derive(Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "bools represent independent workspace states"
)]
pub(super) struct DynWorkspaceInfo {
    pub(super) char_id: char,
    pub(super) is_focused: bool,
    pub(super) is_active: bool,
    pub(super) is_uncreated: bool,
    pub(super) is_urgent: bool,
    /// Key is pinned to an existing (non-dynamic) workspace via config.
    pub(super) is_static: bool,
    /// Live workspace exists but holds no windows.
    pub(super) is_empty: bool,
    /// Windows on the live workspace.
    pub(super) window_count: usize,
    pub(super) name: Option<String>,
    pub(super) ws_name: Option<String>,
    /// Id of the live workspace; `None` when it does not exist.
    pub(super) ws_id: Option<u64>,
    pub(super) output: Option<String>,
}

impl DynWorkspaceInfo {
    fn uncreated(ch: char) -> Self {
        Self {
            char_id: ch,
            is_focused: false,
            is_active: false,
            is_uncreated: true,
            is_urgent: false,
            is_static: false,
            is_empty: false,
            window_count: 0,
            name: None,
            ws_name: None,
            ws_id: None,
            output: None,
        }
    }
}

/// Collect workspace IDs that contain at least one urgent window.
fn urgent_workspace_ids(windows: &[niri_ipc::Window]) -> HashSet<u64> {
    windows
        .iter()
        .filter(|w| w.is_urgent)
        .filter_map(|w| w.workspace_id)
        .collect()
}

/// Number of windows on each workspace that has any.
fn window_counts(windows: &[niri_ipc::Window]) -> HashMap<u64, usize> {
    let mut counts = HashMap::new();
    for id in windows.iter().filter_map(|w| w.workspace_id) {
        *counts.entry(id).or_insert(0) += 1;
    }
    counts
}

fn build_dyn_workspace_infos(
    workspaces: &[niri_ipc::Workspace],
    windows: &[niri_ipc::Window],
    config: &ResolvedConfig,
) -> Vec<DynWorkspaceInfo> {
    let prefix = &config.workspace_prefix;
    let urgent_ws_ids = urgent_workspace_ids(windows);
    let counts = window_counts(windows);
    let count_on = |id| counts.get(&id).copied().unwrap_or(0);

    // Find the globally focused workspace
    let focused_ws_id = workspaces.iter().find(|ws| ws.is_focused).map(|ws| ws.id);

    let mut live_chars: HashSet<char> = HashSet::new();

    let mut infos: Vec<DynWorkspaceInfo> = workspaces
        .iter()
        .filter_map(|ws| {
            let ws_name = ws.name.as_ref()?;
            let (ch, title) = crate::config::parse_dynamic_name(ws_name, prefix)?;
            // Statically mapped keys always show their pinned workspace.
            if config.static_workspaces.contains_key(&ch) {
                return None;
            }

            live_chars.insert(ch);

            let is_focused = Some(ws.id) == focused_ws_id;
            let is_active = !is_focused && ws.is_active;

            let name = config
                .workspace_names
                .get(&ch)
                .cloned()
                .or_else(|| title.map(String::from));
            let is_urgent = ws.is_urgent || urgent_ws_ids.contains(&ws.id);

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                is_uncreated: false,
                is_urgent,
                is_static: false,
                is_empty: count_on(ws.id) == 0,
                window_count: count_on(ws.id),
                name,
                ws_name: Some(ws_name.clone()),
                ws_id: Some(ws.id),
                output: ws.output.clone(),
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
        if live_chars.contains(&ch) || config.static_workspaces.contains_key(&ch) {
            continue;
        }
        let mut info = DynWorkspaceInfo::uncreated(ch);
        info.name = config.workspace_names.get(&ch).cloned();
        infos.push(info);
    }

    // Statically mapped keys mirror the state of their pinned workspace;
    // a missing target renders as uncreated (disabled).
    for (&ch, target) in &config.static_workspaces {
        let live = workspaces.iter().find(|ws| {
            ws.name
                .as_deref()
                .is_some_and(|n| niri::same_workspace_name(n, target))
        });
        let is_focused = live.is_some_and(|ws| Some(ws.id) == focused_ws_id);
        infos.push(DynWorkspaceInfo {
            char_id: ch,
            is_focused,
            is_active: !is_focused && live.is_some_and(|ws| ws.is_active),
            is_uncreated: live.is_none(),
            is_urgent: live.is_some_and(|ws| ws.is_urgent || urgent_ws_ids.contains(&ws.id)),
            is_static: true,
            is_empty: live.is_some_and(|ws| count_on(ws.id) == 0),
            window_count: live.map_or(0, |ws| count_on(ws.id)),
            name: config.workspace_names.get(&ch).cloned(),
            ws_name: Some(target.clone()),
            ws_id: live.map(|ws| ws.id),
            output: live.and_then(|ws| ws.output.clone()),
        });
    }

    infos.sort_by_key(|i| i.char_id);
    infos
}

fn build_static_workspace_infos(
    workspaces: &[niri_ipc::Workspace],
    windows: &[niri_ipc::Window],
    config: &ResolvedConfig,
) -> Vec<StaticWorkspaceInfo> {
    let prefix = &config.workspace_prefix;

    let focused_output = workspaces
        .iter()
        .find(|ws| ws.is_focused)
        .and_then(|ws| ws.output.clone());

    let urgent_ws_ids = urgent_workspace_ids(windows);
    let counts = window_counts(windows);

    workspaces
        .iter()
        .filter(|ws| ws.output == focused_output)
        .filter(|ws| {
            ws.name
                .as_ref()
                .is_none_or(|n| crate::config::parse_dynamic_name(n, prefix).is_none())
        })
        // Workspaces pinned to a key appear on the keyboard, not in this row.
        .filter(|ws| {
            ws.name.as_ref().is_none_or(|n| {
                !config
                    .static_workspaces
                    .values()
                    .any(|t| niri::same_workspace_name(t, n))
            })
        })
        // Never hide the focused or urgent workspace, even when empty.
        .filter(|ws| {
            !config.hide_empty_static
                || counts.contains_key(&ws.id)
                || ws.is_focused
                || ws.is_urgent
                || urgent_ws_ids.contains(&ws.id)
        })
        .map(|ws| StaticWorkspaceInfo {
            id: ws.id,
            name: ws.name.clone().unwrap_or_else(|| ws.idx.to_string()),
            is_focused: ws.is_focused,
            is_active: !ws.is_focused && ws.is_active,
            is_urgent: ws.is_urgent || urgent_ws_ids.contains(&ws.id),
            is_empty: !counts.contains_key(&ws.id),
        })
        .collect()
}

// --- Keyboard layout builders ---

/// Build a map of all keyboard keys to their workspace info.
/// Keys without a live or configured workspace get a default empty entry.
fn build_full_keyboard_info(
    workspaces: &[niri_ipc::Workspace],
    windows: &[niri_ipc::Window],
    config: &ResolvedConfig,
) -> HashMap<char, DynWorkspaceInfo> {
    let live_infos = build_dyn_workspace_infos(workspaces, windows, config);
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

/// Everything the grid view renders from niri's state, compared to skip
/// refreshes that change no card. Window titles are not part of it.
#[derive(Debug, PartialEq)]
pub(super) struct GridModel {
    pub(super) keyboard: Rc<HashMap<char, DynWorkspaceInfo>>,
    pub(super) static_row: Vec<StaticWorkspaceInfo>,
    /// Output the grid shows, for hover-preview gating.
    pub(super) focused_output: Option<String>,
    /// Whether the workspaces span more than one output.
    pub(super) multi_output: bool,
}

impl GridModel {
    pub(super) fn new(
        workspaces: &[niri_ipc::Workspace],
        windows: &[niri_ipc::Window],
        config: &ResolvedConfig,
    ) -> Self {
        let mut outputs = workspaces.iter().map(|ws| &ws.output);
        Self {
            keyboard: Rc::new(build_full_keyboard_info(workspaces, windows, config)),
            static_row: build_static_workspace_infos(workspaces, windows, config),
            focused_output: focused_output_from(workspaces),
            multi_output: outputs
                .next()
                .is_some_and(|first| outputs.any(|o| o != first)),
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "bools represent independent workspace states"
)]
struct CardState {
    is_static: bool,
    is_focused: bool,
    is_active: bool,
    is_urgent: bool,
    is_uncreated: bool,
    is_empty: bool,
    is_disabled: bool,
    /// Delete mode is waiting for a second press on this card.
    is_confirm: bool,
}

/// CSS classes for a workspace card; documented in README "Theming", so renaming one is breaking.
fn card_classes(state: &CardState) -> Vec<&'static str> {
    let mut classes = vec![
        "workspace-card",
        if state.is_static { "static" } else { "dynamic" },
    ];
    if state.is_focused {
        classes.push("focused");
    }
    if state.is_focused || state.is_active {
        classes.push("active");
    }
    if state.is_urgent {
        classes.push("urgent");
    }
    if state.is_uncreated {
        classes.push("uncreated");
    } else if state.is_empty {
        classes.push("empty");
    } else {
        classes.push("occupied");
    }
    if state.is_disabled {
        classes.push("disabled");
    }
    if state.is_confirm {
        classes.push("confirm");
    }
    classes
}

/// Create the outer card box (with CSS classes and fixed size) and an inner centering box.
fn build_card_shell(classes: Vec<&str>, key_size: i32) -> (GtkBox, GtkBox) {
    let card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(classes)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    card.set_size_request(key_size, key_size);

    let inner = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .vexpand(true)
        .valign(Align::Center)
        .halign(Align::Center)
        .build();
    card.append(&inner);

    (card, inner)
}

/// Characters per line of a card's name. `--font-name` is 13% of the key and
/// `--key-pad-h` 10%, so about 11 fit at any key size; 10 leaves a margin.
const CARD_NAME_MAX_CHARS: i32 = 10;

/// Show the label's full text as a tooltip while it is ellipsized.
fn tooltip_when_ellipsized(label: &Label) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        let clipped = label.layout().is_ellipsized();
        if clipped {
            tooltip.set_text(Some(&label.text()));
        }
        clipped
    });
}

/// Attach a hover-preview controller that focuses a workspace on mouse enter.
///
/// The session's `hover_armed` flag prevents hover-preview from firing when
/// the cursor is already over a card the moment the overlay appears. It is
/// set to `true` by the window-level motion controller after the first real
/// mouse movement.
fn attach_hover_preview(widget: &GtkBox, ws_id: u64, session: &Rc<OverlaySession>) {
    let hover_session = session.clone();
    let motion = EventControllerMotion::new();
    motion.connect_enter(move |_, _, _| {
        if hover_session.hover_armed.get()
            && hover_session.preview.should_focus(ws_id)
            && niri::focus_workspace_by_id(ws_id).is_ok()
        {
            hover_session.preview.focused(ws_id);
        }
    });
    widget.add_controller(motion);
}

fn build_key_widget(
    info: &DynWorkspaceInfo,
    mode: Mode,
    ctx: &ActionContext,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let is_disabled = match mode {
        Mode::MoveWindow => info.is_uncreated || info.is_focused,
        Mode::Delete => info.is_uncreated || info.is_static,
        // Empty pinned workspaces render dimmed like the static row, but the
        // key still switches to them.
        Mode::Normal => info.is_uncreated || (info.is_static && info.is_empty),
    };
    let classes = card_classes(&CardState {
        is_static: info.is_static,
        is_focused: info.is_focused,
        is_active: info.is_active,
        is_urgent: info.is_urgent,
        is_uncreated: info.is_uncreated,
        is_empty: info.is_empty,
        is_disabled,
        is_confirm: mode == Mode::Delete && ctx.session.delete_armed.get() == Some(info.char_id),
    });

    let (key_box, inner) = build_card_shell(classes, metrics.key_size);

    let char_label = Label::builder()
        .label(display_key_char(info.char_id))
        .css_classes(["card-title"])
        .build();
    inner.append(&char_label);

    if let Some(ref name) = info.name {
        let name_label = Label::builder()
            .label(name)
            .css_classes(["card-name"])
            .wrap(true)
            .wrap_mode(gtk4::pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(CARD_NAME_MAX_CHARS)
            .justify(gtk4::Justification::Center)
            .build();
        tooltip_when_ellipsized(&name_label);
        inner.append(&name_label);
    }

    // Click handler
    let ch = info.char_id;
    let click_ctx = ctx.clone();
    let click = GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        dispatch_action(ch, &click_ctx);
    });
    key_box.add_controller(click);

    // Hover preview: focus workspace on mouse enter (Normal mode, created cards only).
    // Only enable for workspaces on the same output to avoid jumping the cursor.
    if !is_disabled && mode == Mode::Normal && ctx.session.config.hover_preview {
        let same_output = match (&info.output, &ctx.focused_output) {
            (Some(ws_out), Some(focus_out)) => ws_out == focus_out,
            _ => false,
        };
        if same_output {
            if let Some(ws_id) = info.ws_id {
                attach_hover_preview(&key_box, ws_id, &ctx.session);
            }
        }
    }

    key_box
}

pub(super) fn build_keyboard(
    infos: &HashMap<char, DynWorkspaceInfo>,
    mode: Mode,
    ctx: &ActionContext,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let keyboard = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap)
        .css_classes(["keyboard"])
        .halign(Align::Center)
        .build();

    for (row_idx, row) in metrics.layout.rows.iter().enumerate() {
        let row_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(metrics.key_gap)
            .css_classes(["keyboard-row"])
            .margin_start(metrics.row_margin(row_idx))
            .build();

        for &ch in *row {
            if let Some(info) = infos.get(&ch) {
                row_box.append(&build_key_widget(info, mode, ctx, metrics));
            }
        }

        keyboard.append(&row_box);
    }

    keyboard
}

fn build_static_card(
    info: &StaticWorkspaceInfo,
    mode: Mode,
    ctx: &ActionContext,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let is_disabled = match mode {
        Mode::Delete => true,
        Mode::MoveWindow => info.is_empty || info.is_focused,
        Mode::Normal => info.is_empty,
    };
    let classes = card_classes(&CardState {
        is_static: true,
        is_focused: info.is_focused,
        is_active: info.is_active,
        is_urgent: info.is_urgent,
        is_uncreated: false,
        is_empty: info.is_empty,
        is_disabled,
        is_confirm: false,
    });

    let (card, inner) = build_card_shell(classes, metrics.key_size);

    let name_label = Label::builder()
        .label(&info.name)
        .css_classes(["card-title"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(6)
        .build();
    tooltip_when_ellipsized(&name_label);
    inner.append(&name_label);

    if !is_disabled {
        let id = info.id;
        let click_ctx = ctx.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            let result = match click_ctx.mode {
                Mode::Normal => focus_selected(&click_ctx, id),
                Mode::MoveWindow => window_to_move(&click_ctx)
                    .and_then(|window| niri::move_window_to_workspace_by_id(id, Some(window))),
                Mode::Delete => return,
            };
            if let Err(e) = result {
                show_error(&click_ctx, &format!("Failed: {e:#}"));
                return;
            }
            finish(&click_ctx);
        });
        card.add_controller(click);

        if mode == Mode::Normal && ctx.session.config.hover_preview {
            attach_hover_preview(&card, info.id, &ctx.session);
        }
    }

    card
}

pub(super) fn build_static_workspace_row(
    infos: &[StaticWorkspaceInfo],
    mode: Mode,
    ctx: &ActionContext,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(metrics.key_gap)
        .css_classes(["static-workspaces"])
        .halign(Align::Center)
        .build();

    for info in infos {
        row.append(&build_static_card(info, mode, ctx, metrics));
    }

    row
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::config::{ALL_LAYOUTS, LAYOUT_QWERTY};
    use crate::test_helpers::{test_window, test_workspace};

    // --- Keyboard layout coverage ---

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

    // --- build_dyn_workspace_infos helpers & tests ---

    pub(in crate::ui) fn default_test_config() -> ResolvedConfig {
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
            theme: crate::config::Theme::default(),
            templates: Vec::new(),
            hooks: crate::config::HookConfig::default(),
            diagnostics: Vec::new(),
        }
    }

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
        // Sorted by char_id
        assert_eq!(infos[0].char_id, 'a');
        assert_eq!(infos[0].ws_id, Some(20));
        assert_eq!(infos[1].char_id, 'b');
        assert_eq!(infos[1].ws_id, Some(10));
    }

    #[test]
    fn build_dyn_workspace_infos_counts_windows() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),
            test_workspace(20, Some("dyn-b"), false),
        ];
        let windows = vec![test_window(1, 10, "firefox"), test_window(2, 10, "kitty")];

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &default_test_config());

        assert_eq!(infos[0].window_count, 2);
        assert!(!infos[0].is_empty);
        assert_eq!(infos[1].window_count, 0);
        assert!(infos[1].is_empty);
    }

    #[test]
    fn build_dyn_workspace_infos_titled_workspace() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-a My Project"), true),
            test_workspace(20, Some("dyn-b"), false),
        ];
        let windows = vec![];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].char_id, 'a');
        assert_eq!(infos[0].name.as_deref(), Some("My Project"));
        assert_eq!(infos[0].ws_name.as_deref(), Some("dyn-a My Project"));
        // 'b' has no title, no configured name
        assert_eq!(infos[1].char_id, 'b');
        assert!(infos[1].name.is_none());
    }

    #[test]
    fn build_dyn_workspace_infos_configured_name_overrides_title() {
        let workspaces = vec![test_workspace(10, Some("dyn-a Some Title"), true)];
        let windows = vec![];
        let mut config = default_test_config();
        config.workspace_names.insert('a', "Configured".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 1);
        // Configured name takes precedence over title from ws_name
        assert_eq!(infos[0].name.as_deref(), Some("Configured"));
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
        // 'a' is live, 'b' is uncreated
        assert!(!infos[0].is_uncreated);
        assert_eq!(infos[0].char_id, 'a');
        assert!(infos[1].is_uncreated);
        assert_eq!(infos[1].char_id, 'b');
        assert_eq!(infos[1].name.as_deref(), Some("Browser"));
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
        assert!(!infos[0].is_urgent); // 'a' — no urgent windows
        assert!(infos[1].is_urgent); // 'b' — has an urgent window
    }

    #[test]
    fn build_dyn_workspace_infos_focused_and_active() {
        let mut ws_active = test_workspace(20, Some("dyn-b"), false);
        ws_active.is_active = true;
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),  // focused
            ws_active,                                // active but not focused
            test_workspace(30, Some("dyn-c"), false), // neither
        ];
        let windows = vec![];
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert_eq!(infos.len(), 3);
        // 'a' — focused
        assert!(infos[0].is_focused);
        assert!(!infos[0].is_active);
        // 'b' — active (not focused)
        assert!(!infos[1].is_focused);
        assert!(infos[1].is_active);
        // 'c' — neither
        assert!(!infos[2].is_focused);
        assert!(!infos[2].is_active);
    }

    // --- static workspace mappings ---

    #[test]
    fn build_dyn_workspace_infos_static_mapping_live() {
        let mut ws = test_workspace(5, Some("01"), true);
        ws.is_active = true;
        let workspaces = vec![ws];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &[], &config);

        assert_eq!(infos.len(), 1);
        let info = &infos[0];
        assert_eq!(info.char_id, 'q');
        assert!(info.is_static);
        assert!(!info.is_uncreated);
        assert!(info.is_focused);
        assert!(!info.is_active); // focused trumps active
        assert_eq!(info.ws_name.as_deref(), Some("01"));
        assert_eq!(info.ws_id, Some(5));
        // No configured display name → no name label
        assert_eq!(info.name, None);
        assert_eq!(info.output.as_deref(), Some("DP-1"));
        // No windows → empty
        assert!(info.is_empty);
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_occupied_not_empty() {
        let workspaces = vec![test_workspace(5, Some("01"), false)];
        let windows = vec![test_window(1, 5, "firefox")];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &windows, &config);

        assert!(!infos[0].is_empty);
        assert_eq!(infos[0].window_count, 1);
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_workspace_urgency_flag() {
        let mut ws = test_workspace(5, Some("01"), false);
        ws.is_urgent = true;
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        // Urgency from the workspace flag alone, without any urgent window
        let infos = build_dyn_workspace_infos(&[ws], &[], &config);

        assert!(infos[0].is_urgent);
    }

    #[test]
    fn build_dyn_workspace_infos_workspace_urgency_flag() {
        let mut ws = test_workspace(10, Some("dyn-a"), false);
        ws.is_urgent = true;
        let config = default_test_config();

        let infos = build_dyn_workspace_infos(&[ws], &[], &config);

        assert!(infos[0].is_urgent);
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_missing_target() {
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());
        config.workspace_names.insert('q', "Main".to_string());

        let infos = build_dyn_workspace_infos(&[], &[], &config);

        assert_eq!(infos.len(), 1);
        assert!(infos[0].is_static);
        assert!(infos[0].is_uncreated);
        assert_eq!(infos[0].ws_id, None);
        // Configured display name overrides the target workspace name
        assert_eq!(infos[0].name.as_deref(), Some("Main"));
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_ignores_case() {
        let workspaces = vec![test_workspace(5, Some("Mail"), false)];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "mail".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &[], &config);

        assert!(!infos[0].is_uncreated);
        assert_eq!(infos[0].ws_id, Some(5));
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_wins_over_dyn() {
        let workspaces = vec![
            test_workspace(1, Some("dyn-q"), false),
            test_workspace(2, Some("01"), true),
        ];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &[], &config);

        assert_eq!(infos.len(), 1);
        assert!(infos[0].is_static);
        assert_eq!(infos[0].ws_name.as_deref(), Some("01"));
    }

    #[test]
    fn build_dyn_workspace_infos_static_mapping_urgency() {
        let workspaces = vec![test_workspace(5, Some("01"), false)];
        let mut win = test_window(1, 5, "slack");
        win.is_urgent = true;
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        let infos = build_dyn_workspace_infos(&workspaces, &[win], &config);

        assert!(infos[0].is_urgent);
    }

    #[test]
    fn static_infos_excludes_pinned_workspaces() {
        let workspaces = vec![
            test_workspace(1, Some("01"), true),
            test_workspace(2, Some("browser"), false),
        ];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "01".to_string());

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
    }

    #[test]
    fn static_infos_excludes_pinned_workspaces_ignoring_case() {
        let workspaces = vec![
            test_workspace(1, Some("Mail"), true),
            test_workspace(2, Some("browser"), false),
        ];
        let mut config = default_test_config();
        config.static_workspaces.insert('q', "mail".to_string());

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
    }

    // --- build_static_workspace_infos ---

    #[test]
    fn static_infos_filters_out_dynamic() {
        let workspaces = vec![
            test_workspace(1, Some("dyn-a"), true),
            test_workspace(2, Some("browser"), false),
            test_workspace(3, Some("dyn-b"), false),
        ];
        let config = default_test_config();
        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
    }

    #[test]
    fn lookalike_name_is_static_not_dynamic() {
        let workspaces = vec![test_workspace(1, Some("dyn-alpha"), true)];
        let config = default_test_config();

        let static_infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(static_infos.len(), 1);
        assert_eq!(static_infos[0].name, "dyn-alpha");

        let dyn_infos = build_dyn_workspace_infos(&workspaces, &[], &config);
        assert!(dyn_infos.is_empty());
    }

    #[test]
    fn static_infos_only_focused_output() {
        let mut ws1 = test_workspace(1, Some("browser"), true);
        ws1.output = Some("DP-1".to_string());
        let mut ws2 = test_workspace(2, Some("mail"), false);
        ws2.output = Some("HDMI-1".to_string());
        let workspaces = vec![ws1, ws2];
        let config = default_test_config();

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
    }

    #[test]
    fn static_infos_unnamed_falls_back_to_idx() {
        let mut ws = test_workspace(1, None, true);
        ws.idx = 3;
        let workspaces = vec![ws];
        let config = default_test_config();

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "3");
        assert_eq!(infos[0].id, 1);
    }

    #[test]
    fn static_infos_urgency_from_workspace_flag() {
        let mut ws = test_workspace(1, Some("browser"), true);
        ws.is_urgent = true;
        let config = default_test_config();

        let infos = build_static_workspace_infos(&[ws], &[], &config);
        assert!(infos[0].is_urgent);
    }

    #[test]
    fn static_infos_urgency_from_windows() {
        let workspaces = vec![test_workspace(1, Some("browser"), true)];
        let mut win = test_window(100, 1, "firefox");
        win.is_urgent = true;
        let config = default_test_config();

        let infos = build_static_workspace_infos(&workspaces, &[win], &config);
        assert_eq!(infos.len(), 1);
        assert!(infos[0].is_urgent);
    }

    #[test]
    fn static_infos_empty_when_all_dynamic() {
        let workspaces = vec![
            test_workspace(1, Some("dyn-a"), true),
            test_workspace(2, Some("dyn-b"), false),
        ];
        let config = default_test_config();
        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert!(infos.is_empty());
    }

    #[test]
    fn static_infos_focused_and_active() {
        let mut ws1 = test_workspace(1, Some("browser"), true);
        ws1.is_active = true;
        let mut ws2 = test_workspace(2, Some("mail"), false);
        ws2.is_active = true;
        let workspaces = vec![ws1, ws2];
        let config = default_test_config();

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 2);
        assert!(infos[0].is_focused);
        assert!(!infos[0].is_active); // focused trumps active
        assert!(!infos[1].is_focused);
        assert!(infos[1].is_active);
    }

    #[test]
    fn static_infos_empty_without_windows() {
        let workspaces = vec![
            test_workspace(1, Some("browser"), true),
            test_workspace(2, Some("mail"), false),
        ];
        let windows = vec![test_window(100, 1, "firefox")];
        let config = default_test_config();

        let infos = build_static_workspace_infos(&workspaces, &windows, &config);
        assert!(!infos[0].is_empty); // has a window
        assert!(infos[1].is_empty); // no windows
    }

    #[test]
    fn static_infos_hide_empty_filters_windowless() {
        let workspaces = vec![
            test_workspace(1, Some("browser"), true),
            test_workspace(2, Some("mail"), false),
        ];
        let windows = vec![test_window(100, 1, "firefox")];
        let mut config = default_test_config();
        config.hide_empty_static = true;

        let infos = build_static_workspace_infos(&workspaces, &windows, &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
    }

    #[test]
    fn static_infos_hide_empty_keeps_focused() {
        let workspaces = vec![
            test_workspace(1, Some("browser"), true),
            test_workspace(2, Some("mail"), false),
        ];
        let mut config = default_test_config();
        config.hide_empty_static = true;

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "browser");
        assert!(infos[0].is_focused);
    }

    #[test]
    fn static_infos_hide_empty_keeps_urgent() {
        let mut urgent_ws = test_workspace(2, Some("mail"), false);
        urgent_ws.is_urgent = true;
        let workspaces = vec![test_workspace(1, Some("browser"), true), urgent_ws];
        let mut config = default_test_config();
        config.hide_empty_static = true;

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert_eq!(infos.len(), 2);
        assert!(infos[1].is_urgent);
    }

    #[test]
    fn static_infos_hide_empty_can_empty_the_row() {
        // Focused elsewhere (dyn workspace) → every empty static hides.
        let workspaces = vec![
            test_workspace(1, Some("dyn-a"), true),
            test_workspace(2, Some("mail"), false),
            test_workspace(3, Some("scratch"), false),
        ];
        let mut config = default_test_config();
        config.hide_empty_static = true;

        let infos = build_static_workspace_infos(&workspaces, &[], &config);
        assert!(infos.is_empty());
    }

    // --- GridModel ---

    #[test]
    fn grid_model_ignores_window_titles() {
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true)];
        let config = default_test_config();
        let titled = |title: &str| {
            let mut window = test_window(1, 10, "kitty");
            window.title = Some(title.to_string());
            GridModel::new(&workspaces, &[window], &config)
        };
        assert_eq!(titled("~"), titled("~/src"));
    }

    #[test]
    fn grid_model_tracks_window_moves() {
        let workspaces = vec![
            test_workspace(10, Some("dyn-a"), true),
            test_workspace(20, Some("dyn-b"), false),
        ];
        let config = default_test_config();
        let on = |ws_id| GridModel::new(&workspaces, &[test_window(1, ws_id, "kitty")], &config);
        assert_ne!(on(10), on(20));
    }

    #[test]
    fn grid_model_notes_multiple_outputs() {
        let config = default_test_config();
        let mut other = test_workspace(20, Some("dyn-b"), false);
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true), other.clone()];
        assert!(!GridModel::new(&workspaces, &[], &config).multi_output);
        other.output = Some("HDMI-A-1".to_string());
        let workspaces = vec![test_workspace(10, Some("dyn-a"), true), other];
        assert!(GridModel::new(&workspaces, &[], &config).multi_output);
        assert!(!GridModel::new(&[], &[], &config).multi_output);
    }

    fn card_state() -> CardState {
        CardState {
            is_static: false,
            is_focused: false,
            is_active: false,
            is_urgent: false,
            is_uncreated: false,
            is_empty: false,
            is_disabled: false,
            is_confirm: false,
        }
    }

    #[test]
    fn card_classes_occupied_dynamic() {
        assert_eq!(
            card_classes(&card_state()),
            ["workspace-card", "dynamic", "occupied"]
        );
    }

    #[test]
    fn card_classes_focused_implies_active() {
        let state = CardState {
            is_focused: true,
            ..card_state()
        };
        assert_eq!(
            card_classes(&state),
            ["workspace-card", "dynamic", "focused", "active", "occupied"]
        );
    }

    #[test]
    fn card_classes_active_on_other_output() {
        // style.css outlines .active and fills only .focused.
        let state = CardState {
            is_active: true,
            ..card_state()
        };
        assert_eq!(
            card_classes(&state),
            ["workspace-card", "dynamic", "active", "occupied"]
        );
    }

    #[test]
    fn card_classes_uncreated_is_not_empty() {
        let state = CardState {
            is_uncreated: true,
            is_empty: true,
            is_disabled: true,
            ..card_state()
        };
        assert_eq!(
            card_classes(&state),
            ["workspace-card", "dynamic", "uncreated", "disabled"]
        );
    }

    #[test]
    fn card_classes_confirm() {
        let state = CardState {
            is_confirm: true,
            ..card_state()
        };
        assert_eq!(
            card_classes(&state),
            ["workspace-card", "dynamic", "occupied", "confirm"]
        );
    }

    #[test]
    fn card_classes_static_empty_urgent() {
        let state = CardState {
            is_static: true,
            is_urgent: true,
            is_empty: true,
            ..card_state()
        };
        assert_eq!(
            card_classes(&state),
            ["workspace-card", "static", "urgent", "empty"]
        );
    }
}
