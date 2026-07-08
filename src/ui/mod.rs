mod cards;
mod metrics;
mod picker;
mod variables;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, EventControllerKey, EventControllerMotion,
    GestureClick, Label, Orientation, Overlay, Revealer, RevealerTransitionType,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::actions::HookInfo;
use crate::config::ResolvedConfig;
use crate::niri;

use cards::{
    build_full_keyboard_info, build_keyboard, build_static_workspace_infos,
    build_static_workspace_row, DynWorkspaceInfo,
};
use metrics::{apply_scaled_css, find_monitor_for_output, get_monitor_width, KeyboardMetrics};
use picker::show_template_picker;

/// Modifier mask for matching keybinds (includes Super to detect compositor keybind hold).
const RELEVANT_MODS: gdk4::ModifierType = gdk4::ModifierType::from_bits_retain(
    gdk4::ModifierType::CONTROL_MASK.bits()
        | gdk4::ModifierType::SHIFT_MASK.bits()
        | gdk4::ModifierType::ALT_MASK.bits()
        | gdk4::ModifierType::SUPER_MASK.bits(),
);

/// Modifier mask for workspace key actions (excludes Super so holding Mod doesn't block input).
const ACTION_MODS: gdk4::ModifierType = gdk4::ModifierType::from_bits_retain(
    gdk4::ModifierType::CONTROL_MASK.bits()
        | gdk4::ModifierType::SHIFT_MASK.bits()
        | gdk4::ModifierType::ALT_MASK.bits(),
);

/// Extract the output name of the focused workspace from a pre-fetched list.
fn focused_output_from(workspaces: &[niri_ipc::Workspace]) -> Option<String> {
    workspaces.iter().find(|w| w.is_focused)?.output.clone()
}

/// Extract the name of the focused workspace from a pre-fetched list.
fn focused_workspace_name_from(workspaces: &[niri_ipc::Workspace]) -> Option<String> {
    workspaces.iter().find(|w| w.is_focused)?.name.clone()
}

fn format_workspace_display(ch: char, config: &ResolvedConfig) -> String {
    let key = display_key_char(ch);
    match config.workspace_names.get(&ch) {
        Some(name) => format!("{key} ({name})"),
        None => key,
    }
}

fn display_key_char(ch: char) -> String {
    if ch.is_ascii_lowercase() {
        ch.to_uppercase().to_string()
    } else {
        ch.to_string()
    }
}

// --- Modes ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Delete,
    MoveWindow,
}

impl Mode {
    const fn display_name(self) -> &'static str {
        match self {
            Self::Normal => "Switch",
            Self::Delete => "Delete",
            Self::MoveWindow => "Move Window",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Normal => Self::Delete,
            Self::Delete => Self::MoveWindow,
            Self::MoveWindow => Self::Normal,
        }
    }

    const fn prev(self) -> Self {
        match self {
            Self::Normal => Self::MoveWindow,
            Self::Delete => Self::Normal,
            Self::MoveWindow => Self::Delete,
        }
    }

    const fn all() -> [Self; 3] {
        [Self::Normal, Self::Delete, Self::MoveWindow]
    }

    const fn css_class(self) -> &'static str {
        match self {
            Self::Normal => "switch",
            Self::Delete => "delete",
            Self::MoveWindow => "move-window",
        }
    }

    const fn widget_name(self) -> &'static str {
        match self {
            Self::Normal => "mode-switch",
            Self::Delete => "mode-delete",
            Self::MoveWindow => "mode-move-window",
        }
    }

    fn from_widget_name(name: &str) -> Option<Self> {
        match name {
            "mode-switch" => Some(Self::Normal),
            "mode-delete" => Some(Self::Delete),
            "mode-move-window" => Some(Self::MoveWindow),
            _ => None,
        }
    }

    pub fn from_window(window: &gtk4::Window) -> Option<Self> {
        Self::from_widget_name(window.widget_name().as_str())
    }

    const fn container_css_class(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Delete => Some("delete-mode"),
            Self::MoveWindow => Some("move-window-mode"),
        }
    }
}

// --- Data types ---

#[derive(Clone)]
struct ActionContext {
    mode: Mode,
    window: ApplicationWindow,
    error_label: Label,
    error_revealer: Revealer,
    config: Rc<ResolvedConfig>,
    keyboard_infos: Rc<HashMap<char, DynWorkspaceInfo>>,
    monitor_width: i32,
    /// Original workspace name when overlay opened (for hover-preview restore).
    original_workspace: Rc<RefCell<Option<String>>>,
    /// Set to true when the user makes a selection (skip restore on close).
    selection_made: Rc<Cell<bool>>,
    /// Output name where the overlay is displayed (for hover-preview gating).
    focused_output: Option<String>,
    /// Armed after the first real mouse movement; prevents hover-preview from
    /// triggering when the cursor is already over a card at overlay open.
    hover_armed: Rc<Cell<bool>>,
}

// --- UI construction ---

pub fn build_ui(app: &gtk4::Application, config: &Rc<ResolvedConfig>, mode: Mode) {
    let window = ApplicationWindow::builder().application(app).build();
    window.remove_css_class("background");
    window.init_layer_shell();

    // Single IPC fetch — derive focused output, monitor, and workspace name from it.
    let workspaces = niri::list_workspaces().unwrap_or_default();
    let focused_output = focused_output_from(&workspaces);
    let focused_monitor = focused_output.as_deref().and_then(find_monitor_for_output);
    window.set_monitor(focused_monitor.as_ref());

    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_exclusive_zone(-1);

    // Hover preview state — captured once at overlay open, shared across repopulations.
    let original_workspace = Rc::new(RefCell::new(if config.hover_preview {
        focused_workspace_name_from(&workspaces)
    } else {
        None
    }));
    let selection_made = Rc::new(Cell::new(false));
    let hover_armed = Rc::new(Cell::new(false));

    // Arm hover-preview after the first real mouse movement so that a cursor
    // already resting over a card when the overlay appears does not trigger a
    // workspace switch.
    {
        let armed = hover_armed.clone();
        let motion = EventControllerMotion::new();
        motion.set_name(Some("ndw-hover-arm"));
        motion.connect_motion(move |_, _, _| {
            armed.set(true);
        });
        window.add_controller(motion);
    }

    let monitor_width = get_monitor_width(focused_monitor.as_ref());
    populate_overlay(
        &window,
        config,
        mode,
        monitor_width,
        &original_workspace,
        &selection_made,
        &hover_armed,
        Some(workspaces),
    );

    // Restore original workspace on close if no selection was made.
    {
        let orig = original_workspace.clone();
        let sel = selection_made.clone();
        window.connect_close_request(move |_| {
            if !sel.get() {
                if let Some(ref name) = *orig.borrow() {
                    let _ = niri::focus_workspace_by_name(name);
                }
            }
            Propagation::Proceed
        });
    }

    // Poll for focused-output changes so the overlay follows the cursor.
    let tracked_output = Rc::new(RefCell::new(focused_output));
    let track_window = window.clone();
    let track_config = config.clone();
    let track_orig = original_workspace;
    let track_sel = selection_made;
    let track_armed = hover_armed;
    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
        if !track_window.is_visible() {
            return glib::ControlFlow::Break;
        }
        let fresh_workspaces = niri::list_workspaces().unwrap_or_default();
        let current = focused_output_from(&fresh_workspaces);
        if current != *tracked_output.borrow() {
            tracked_output.borrow_mut().clone_from(&current);
            if let Some(ref output) = current {
                if let Some(monitor) = find_monitor_for_output(output) {
                    let new_width = get_monitor_width(Some(&monitor));
                    track_window.set_monitor(Some(&monitor));
                    let mode =
                        Mode::from_window(&track_window.clone().upcast()).unwrap_or(Mode::Normal);
                    populate_overlay(
                        &track_window,
                        &track_config,
                        mode,
                        new_width,
                        &track_orig,
                        &track_sel,
                        &track_armed,
                        Some(fresh_workspaces),
                    );
                }
            }
        }
        glib::ControlFlow::Continue
    });

    window.present();
}

/// Remove controllers we previously attached (identified by "ndw-" name prefix).
fn remove_app_controllers(window: &ApplicationWindow) {
    let controllers = window.observe_controllers();
    let mut to_remove = Vec::new();
    for i in 0..controllers.n_items() {
        let Some(obj) = controllers.item(i) else {
            continue;
        };
        let Ok(ctrl) = obj.downcast::<gtk4::EventController>() else {
            continue;
        };
        if ctrl.name().is_some_and(|n| n.starts_with("ndw-")) {
            to_remove.push(ctrl);
        }
    }
    for ctrl in &to_remove {
        window.remove_controller(ctrl);
    }
}

fn build_mode_tabs(ctx: &ActionContext, mode: Mode) -> GtkBox {
    let mode_tabs = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(0)
        .css_classes(["mode-tabs"])
        .halign(Align::Center)
        .build();
    for m in Mode::all() {
        let mut classes = vec!["mode-tab", m.css_class()];
        if m == mode {
            classes.push("active");
        }
        let tab_label = Label::builder()
            .label(m.display_name())
            .css_classes(classes)
            .build();

        let tab_ctx = ctx.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            let ctx = tab_ctx.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(
                    &ctx.window,
                    &ctx.config,
                    m,
                    ctx.monitor_width,
                    &ctx.original_workspace,
                    &ctx.selection_made,
                    &ctx.hover_armed,
                    None,
                );
            });
        });
        tab_label.add_controller(click);

        mode_tabs.append(&tab_label);
    }
    mode_tabs
}

fn create_error_revealer() -> (Label, Revealer) {
    let label = Label::builder()
        .css_classes(["error-message"])
        .wrap(true)
        .build();
    let revealer = Revealer::builder()
        .child(&label)
        .reveal_child(false)
        .transition_type(RevealerTransitionType::SlideUp)
        .transition_duration(200)
        .build();
    (label, revealer)
}

fn matches_close_keybind(
    key: gdk4::Key,
    modifier: gdk4::ModifierType,
    keybinds: &[crate::config::Keybind],
) -> bool {
    keybinds
        .iter()
        .any(|kb| key == kb.key && modifier & RELEVANT_MODS == kb.modifiers)
}

fn new_key_controller() -> EventControllerKey {
    let ctrl = EventControllerKey::new();
    ctrl.set_name(Some("ndw-key"));
    ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
    ctrl
}

fn wrap_in_backdrop(window: &ApplicationWindow, container: &GtkBox) {
    let backdrop = GtkBox::builder()
        .css_classes(["backdrop"])
        .hexpand(true)
        .vexpand(true)
        .build();
    let overlay = Overlay::builder().child(&backdrop).build();
    overlay.add_overlay(container);
    window.set_child(Some(&overlay));
}

fn build_hint_footer(metrics: &KeyboardMetrics, hints: &[&str]) -> GtkBox {
    let footer = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(metrics.key_size / 4)
        .css_classes(["hint-footer"])
        .halign(Align::Center)
        .build();
    for text in hints {
        let label = Label::builder()
            .label(*text)
            .css_classes(["hint-footer-item"])
            .build();
        footer.append(&label);
    }
    footer
}

/// Build (or rebuild) the overlay content for `mode` inside an existing window.
///
/// If `prefetched_workspaces` is provided, uses them instead of making a fresh IPC call.
#[expect(
    clippy::too_many_arguments,
    reason = "shared state params for overlay lifecycle"
)]
fn populate_overlay(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    mode: Mode,
    monitor_width: i32,
    original_workspace: &Rc<RefCell<Option<String>>>,
    selection_made: &Rc<Cell<bool>>,
    hover_armed: &Rc<Cell<bool>>,
    prefetched_workspaces: Option<Vec<niri_ipc::Workspace>>,
) {
    window.set_widget_name(mode.widget_name());
    remove_app_controllers(window);

    let mut container_classes = vec!["popup-container"];
    if let Some(cls) = mode.container_css_class() {
        container_classes.push(cls);
    }

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(container_classes)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Error label + revealer (built first so ActionContext is available for keys)
    let (error_label, error_revealer) = create_error_revealer();

    // Compute metrics from monitor size and apply scaled CSS
    let metrics = KeyboardMetrics::from_monitor_width(monitor_width, config.layout);
    apply_scaled_css(&metrics.scaled_css_variables());

    // Use pre-fetched workspaces or fetch fresh; always fetch windows fresh.
    let workspaces =
        prefetched_workspaces.unwrap_or_else(|| niri::list_workspaces().unwrap_or_default());
    let windows = niri::list_windows().unwrap_or_default();

    // Build keyboard
    let infos = Rc::new(build_full_keyboard_info(&workspaces, &windows, config));

    let ctx = ActionContext {
        mode,
        window: window.clone(),
        error_label,
        error_revealer: error_revealer.clone(),
        config: config.clone(),
        keyboard_infos: infos.clone(),
        monitor_width,
        original_workspace: original_workspace.clone(),
        selection_made: selection_made.clone(),
        focused_output: focused_output_from(&workspaces),
        hover_armed: hover_armed.clone(),
    };

    // Assemble: static row → keyboard → hint footer → error revealer → mode tabs
    let static_infos = build_static_workspace_infos(&workspaces, &windows, config);
    if !static_infos.is_empty() {
        container.append(&build_static_workspace_row(
            &static_infos,
            mode,
            &ctx,
            &metrics,
        ));
    }

    let keyboard = build_keyboard(&infos, mode, &ctx, &metrics);
    container.append(&keyboard);
    container.append(&build_hint_footer(
        &metrics,
        &["press key to select", "Tab switch mode", "Escape close"],
    ));
    container.append(&error_revealer);
    container.append(&build_mode_tabs(&ctx, mode));

    wrap_in_backdrop(window, &container);

    attach_key_handler(&ctx, &config.close_keybinds);
    attach_close_on_backdrop_click(window, &container);
}

/// Switch to (or create) a workspace and close the overlay on success.
fn switch_and_close(
    ws_name: &str,
    ws_key: char,
    programs: &[String],
    ctx: &ActionContext,
    hook_info: &HookInfo,
) {
    let Some(app) = ctx.window.application() else {
        show_error(ctx, "Failed: window has no application");
        return;
    };
    let result =
        crate::actions::switch_workspace(&app, &ctx.config, ws_key, ws_name, programs, hook_info);
    if let Err(e) = result {
        show_error(ctx, &format!("Failed: {e:#}"));
        return;
    }
    ctx.window.close();
}

fn dispatch_action(ch: char, ctx: &ActionContext) {
    ctx.selection_made.set(true);
    let prefix = &ctx.config.workspace_prefix;
    let info = ctx.keyboard_infos.get(&ch);
    let ws_name = info
        .and_then(|i| i.ws_name.clone())
        .unwrap_or_else(|| crate::config::workspace_name(prefix, ch));

    let result = match ctx.mode {
        Mode::Normal => {
            let is_uncreated = info.is_none_or(|i| i.is_uncreated);
            if is_uncreated && ctx.config.should_show_templates(ch) {
                show_template_picker(ch, ctx);
                return;
            }
            let programs = ctx.config.programs_for(ch);
            switch_and_close(&ws_name, ch, programs, ctx, &HookInfo::default());
            return;
        }
        Mode::Delete => crate::actions::delete_workspace(&ctx.config, ch, &ws_name),
        Mode::MoveWindow => crate::actions::move_window(&ctx.config, ch, &ws_name),
    };

    if let Err(e) = result {
        show_error(ctx, &format!("Failed: {e:#}"));
        return;
    }
    ctx.window.close();
}

fn attach_key_handler(ctx: &ActionContext, close_keybinds: &[crate::config::Keybind]) {
    let key_ctx = ctx.clone();
    let close_keybinds = close_keybinds.to_vec();
    let key_controller = new_key_controller();
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if matches_close_keybind(key, modifier, &close_keybinds) {
            key_ctx.window.close();
            return Propagation::Stop;
        }

        // Tab / Shift+Tab cycle through modes
        if key == gdk4::Key::Tab || key == gdk4::Key::ISO_Left_Tab {
            let next_mode = if key == gdk4::Key::Tab {
                key_ctx.mode.next()
            } else {
                key_ctx.mode.prev()
            };
            let ctx = key_ctx.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(
                    &ctx.window,
                    &ctx.config,
                    next_mode,
                    ctx.monitor_width,
                    &ctx.original_workspace,
                    &ctx.selection_made,
                    &ctx.hover_armed,
                    None,
                );
            });
            return Propagation::Stop;
        }

        // Workspace key: action depends on mode
        // Ignore Super so holding Mod from the opening keybind doesn't block input
        if let Some(ch) = key.to_unicode() {
            let ch = ch.to_ascii_lowercase();
            if crate::config::is_workspace_char(ch) && (modifier & ACTION_MODS).is_empty() {
                dispatch_action(ch, &key_ctx);
                return Propagation::Stop;
            }
        }

        Propagation::Proceed
    });
    ctx.window.add_controller(key_controller);
}

fn attach_close_on_backdrop_click(window: &ApplicationWindow, container: &GtkBox) {
    let window_ref = window.clone();
    let container_ref = container.clone();
    let click = GestureClick::new();
    click.set_name(Some("ndw-backdrop"));
    click.connect_released(move |_, _, x, y| {
        let (cx, cy) = container_ref
            .translate_coordinates(&window_ref, 0.0, 0.0)
            .unwrap_or((0.0, 0.0));
        let cw = f64::from(container_ref.width());
        let ch = f64::from(container_ref.height());
        if x < cx || x > cx + cw || y < cy || y > cy + ch {
            window_ref.close();
        }
    });
    window.add_controller(click);
}

fn show_error(ctx: &ActionContext, msg: &str) {
    ctx.error_label.set_label(msg);
    ctx.error_revealer.set_reveal_child(true);
}

/// Wrap-around index navigation (Up decrements, Down increments).
fn wrap_index(current: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        if current >= len - 1 {
            0
        } else {
            current + 1
        }
    } else if current == 0 {
        len - 1
    } else {
        current - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::test_workspace;

    // --- focused extraction helpers ---

    #[test]
    fn focused_output_from_returns_focused() {
        let workspaces = vec![
            test_workspace(1, Some("ws-1"), false),
            test_workspace(2, Some("ws-2"), true),
        ];
        assert_eq!(focused_output_from(&workspaces), Some("DP-1".to_string()));
    }

    #[test]
    fn focused_output_from_returns_none_when_unfocused() {
        let workspaces = vec![test_workspace(1, Some("ws-1"), false)];
        assert_eq!(focused_output_from(&workspaces), None);
    }

    #[test]
    fn focused_workspace_name_from_returns_name() {
        let workspaces = vec![
            test_workspace(1, Some("ws-1"), false),
            test_workspace(2, Some("ws-2"), true),
        ];
        assert_eq!(
            focused_workspace_name_from(&workspaces),
            Some("ws-2".to_string())
        );
    }

    #[test]
    fn focused_workspace_name_from_returns_none_when_unnamed() {
        let workspaces = vec![test_workspace(1, None, true)];
        assert_eq!(focused_workspace_name_from(&workspaces), None);
    }

    // --- wrap_index ---

    #[test]
    fn wrap_index_cycles_both_directions() {
        assert_eq!(wrap_index(0, 3, true), 1);
        assert_eq!(wrap_index(2, 3, true), 0);
        assert_eq!(wrap_index(0, 3, false), 2);
        assert_eq!(wrap_index(1, 3, false), 0);
    }

    #[test]
    fn wrap_index_empty_len_is_total() {
        assert_eq!(wrap_index(0, 0, true), 0);
        assert_eq!(wrap_index(5, 0, false), 0);
    }

    #[test]
    fn mode_widget_name_roundtrip() {
        for mode in [Mode::Normal, Mode::Delete, Mode::MoveWindow] {
            let name = mode.widget_name();
            assert_eq!(Mode::from_widget_name(name), Some(mode));
        }
    }

    #[test]
    fn mode_from_unknown_widget_name() {
        assert_eq!(Mode::from_widget_name("unknown"), None);
    }

    #[test]
    fn mode_display_name() {
        assert_eq!(Mode::Normal.display_name(), "Switch");
        assert_eq!(Mode::Delete.display_name(), "Delete");
        assert_eq!(Mode::MoveWindow.display_name(), "Move Window");
    }

    #[test]
    fn mode_next_cycles() {
        assert_eq!(Mode::Normal.next(), Mode::Delete);
        assert_eq!(Mode::Delete.next(), Mode::MoveWindow);
        assert_eq!(Mode::MoveWindow.next(), Mode::Normal);
    }

    #[test]
    fn mode_prev_cycles() {
        assert_eq!(Mode::Normal.prev(), Mode::MoveWindow);
        assert_eq!(Mode::MoveWindow.prev(), Mode::Delete);
        assert_eq!(Mode::Delete.prev(), Mode::Normal);
    }

    #[test]
    fn mode_css_class() {
        assert_eq!(Mode::Normal.css_class(), "switch");
        assert_eq!(Mode::Delete.css_class(), "delete");
        assert_eq!(Mode::MoveWindow.css_class(), "move-window");
    }

    // --- display_key_char ---

    #[test]
    fn display_key_char_letters() {
        assert_eq!(display_key_char('a'), "A");
        assert_eq!(display_key_char('z'), "Z");
        assert_eq!(display_key_char('m'), "M");
    }

    #[test]
    fn display_key_char_digits() {
        assert_eq!(display_key_char('0'), "0");
        assert_eq!(display_key_char('9'), "9");
    }
}
