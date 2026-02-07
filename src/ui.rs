use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, EventControllerKey, FlowBox, GestureClick, Label,
    Orientation, Overlay, Revealer, RevealerTransitionType, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::ResolvedConfig;
use crate::niri;

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
}

pub fn get_window_mode(window: &gtk4::Window) -> Option<Mode> {
    Mode::from_widget_name(window.widget_name().as_str())
}

#[derive(Clone)]
struct ActionContext {
    mode: Mode,
    window: ApplicationWindow,
    error_label: Label,
    error_revealer: Revealer,
    config: Rc<ResolvedConfig>,
}

struct WindowInfo {
    app_name: String,
    title: String,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "four bools represent independent workspace states"
)]
struct DynWorkspaceInfo {
    char_id: char,
    is_focused: bool,
    is_active: bool,
    is_uncreated: bool,
    is_urgent: bool,
    name: Option<String>,
    windows: Vec<WindowInfo>,
    configured_programs: Vec<String>,
}

fn clean_app_id(app_id: &str) -> String {
    let segment = app_id.rsplit('.').next().unwrap_or(app_id);
    let name = segment.replace(['-', '_'], " ");
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn gather_dyn_workspaces(config: &ResolvedConfig) -> Vec<DynWorkspaceInfo> {
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

fn build_dyn_workspace_infos(
    workspaces: &[niri_ipc::Workspace],
    windows: &[niri_ipc::Window],
    config: &ResolvedConfig,
) -> Vec<DynWorkspaceInfo> {
    let prefix = &config.workspace_prefix;

    // Group windows by workspace_id and track urgency
    let mut windows_by_ws: HashMap<u64, Vec<WindowInfo>> = HashMap::new();
    let mut urgent_ws_ids: HashSet<u64> = HashSet::new();
    for w in windows {
        if let Some(ws_id) = w.workspace_id {
            let app_name = w.app_id.as_deref().map(clean_app_id).unwrap_or_default();
            let title = w.title.clone().unwrap_or_default();
            windows_by_ws
                .entry(ws_id)
                .or_default()
                .push(WindowInfo { app_name, title });
            if w.is_urgent {
                urgent_ws_ids.insert(ws_id);
            }
        }
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

            let ws_windows = windows_by_ws.remove(&ws.id).unwrap_or_default();
            let name = config.workspace_names.get(&ch).cloned();
            let is_urgent = urgent_ws_ids.contains(&ws.id);

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                is_uncreated: false,
                is_urgent,
                name,
                windows: ws_windows,
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
        let name = config.workspace_names.get(&ch).cloned();
        let programs = config
            .workspace_programs
            .get(&ch)
            .cloned()
            .unwrap_or_else(|| config.default_programs.clone());

        infos.push(DynWorkspaceInfo {
            char_id: ch,
            is_focused: false,
            is_active: false,
            is_uncreated: true,
            is_urgent: false,
            name,
            windows: Vec::new(),
            configured_programs: programs,
        });
    }

    infos.sort_by_key(|i| i.char_id);
    infos
}

fn build_card_header(info: &DynWorkspaceInfo) -> GtkBox {
    let header = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(6)
        .css_classes(["card-header"])
        .build();

    let key_label = Label::builder()
        .label(info.char_id.to_uppercase().to_string())
        .css_classes(["card-key"])
        .build();
    header.append(&key_label);

    if let Some(ref name) = info.name {
        let name_label = Label::builder()
            .label(name)
            .css_classes(["card-name"])
            .build();
        header.append(&name_label);
    }

    let spacer = GtkBox::builder().hexpand(true).build();
    header.append(&spacer);

    let count_text = if info.is_uncreated {
        "not created".to_string()
    } else {
        match info.windows.len() {
            0 => "empty".to_string(),
            1 => "1 window".to_string(),
            n => format!("{n} windows"),
        }
    };
    let count_label = Label::builder()
        .label(&count_text)
        .css_classes(["card-window-count"])
        .build();
    header.append(&count_label);

    header
}

fn build_card_body(info: &DynWorkspaceInfo, config: &ResolvedConfig) -> GtkBox {
    let body = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .css_classes(["card-body"])
        .build();

    if info.is_uncreated {
        if info.configured_programs.is_empty() {
            let empty_label = Label::builder()
                .label("No programs configured")
                .css_classes(["card-empty"])
                .halign(Align::Start)
                .build();
            body.append(&empty_label);
        } else {
            for prog in &info.configured_programs {
                let prog_label = Label::builder()
                    .label(prog)
                    .css_classes(["card-program"])
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .max_width_chars(config.app_name_max_chars + config.window_title_max_chars)
                    .halign(Align::Start)
                    .build();
                body.append(&prog_label);
            }
        }
    } else if info.windows.is_empty() {
        let empty_label = Label::builder()
            .label("No windows")
            .css_classes(["card-empty"])
            .halign(Align::Start)
            .build();
        body.append(&empty_label);
    } else {
        let max_show = if info.windows.len() > config.max_windows_per_card {
            config.max_windows_per_card - 1
        } else {
            config.max_windows_per_card
        };
        for win in info.windows.iter().take(max_show) {
            let row = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(6)
                .css_classes(["window-row"])
                .build();

            let app_label = Label::builder()
                .label(&win.app_name)
                .css_classes(["window-app-name"])
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(config.app_name_max_chars)
                .halign(Align::Start)
                .build();
            row.append(&app_label);

            let title_label = Label::builder()
                .label(&win.title)
                .css_classes(["window-title"])
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(config.window_title_max_chars)
                .hexpand(true)
                .halign(Align::Start)
                .build();
            row.append(&title_label);

            body.append(&row);
        }

        if info.windows.len() > config.max_windows_per_card {
            let overflow = Label::builder()
                .label(format!(
                    "+{} more",
                    info.windows.len() - (config.max_windows_per_card - 1)
                ))
                .css_classes(["window-overflow"])
                .halign(Align::Start)
                .build();
            body.append(&overflow);
        }
    }

    body
}

fn build_workspace_card(info: &DynWorkspaceInfo, config: &ResolvedConfig) -> GtkBox {
    let mut card_classes = vec!["workspace-card"];
    if info.is_uncreated {
        card_classes.push("uncreated");
    } else if info.is_focused {
        card_classes.push("focused");
    } else if info.is_active {
        card_classes.push("active");
    }
    if info.is_urgent {
        card_classes.push("urgent");
    }

    let card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(card_classes)
        .build();

    card.append(&build_card_header(info));

    let divider = Separator::builder().css_classes(["card-divider"]).build();
    card.append(&divider);

    card.append(&build_card_body(info, config));

    card
}

pub fn build_ui(app: &gtk4::Application, config: &Rc<ResolvedConfig>, mode: Mode) {
    let window = ApplicationWindow::builder().application(app).build();
    window.remove_css_class("background");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);

    populate_overlay(&window, config, mode);
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

fn build_mode_tabs(window: &ApplicationWindow, config: &Rc<ResolvedConfig>, mode: Mode) -> GtkBox {
    let mode_tabs = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(0)
        .css_classes(["mode-tabs"])
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

        let tab_window = window.clone();
        let tab_config = config.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            let w = tab_window.clone();
            let c = tab_config.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(&w, &c, m);
            });
        });
        tab_label.add_controller(click);

        mode_tabs.append(&tab_label);
    }
    mode_tabs
}

fn build_hint_footer(has_workspaces: bool) -> GtkBox {
    let footer = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .css_classes(["hint-footer"])
        .halign(Align::Center)
        .build();
    if has_workspaces {
        let keys_hint = Label::builder()
            .label("press key to select")
            .css_classes(["hint-footer-item"])
            .build();
        footer.append(&keys_hint);
    }
    let tab_hint = Label::builder()
        .label("Tab switch mode")
        .css_classes(["hint-footer-item"])
        .build();
    footer.append(&tab_hint);
    let close_hint = Label::builder()
        .label("Escape close")
        .css_classes(["hint-footer-item"])
        .build();
    footer.append(&close_hint);
    footer
}

/// Build (or rebuild) the overlay content for `mode` inside an existing window.
fn populate_overlay(window: &ApplicationWindow, config: &Rc<ResolvedConfig>, mode: Mode) {
    window.set_widget_name(mode.widget_name());
    remove_app_controllers(window);

    let mut dyn_workspaces = gather_dyn_workspaces(config);
    match mode {
        Mode::Delete => dyn_workspaces.retain(|ws| !ws.is_uncreated),
        Mode::MoveWindow => dyn_workspaces.retain(|ws| !ws.is_uncreated && !ws.is_focused),
        Mode::Normal => {}
    }

    let container_classes: Vec<&str> = match mode {
        Mode::Delete => vec!["popup-container", "delete-mode"],
        Mode::MoveWindow => vec!["popup-container", "move-window-mode"],
        Mode::Normal => vec!["popup-container"],
    };

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(container_classes)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    container.append(&build_mode_tabs(window, config, mode));

    let grid = FlowBox::builder()
        .css_classes(["workspace-grid"])
        .max_children_per_line(config.max_columns)
        .min_children_per_line(config.min_columns)
        .selection_mode(gtk4::SelectionMode::None)
        .homogeneous(true)
        .focusable(false)
        .build();

    if dyn_workspaces.is_empty() {
        let hint_text = match mode {
            Mode::Delete => "No workspaces to delete",
            Mode::MoveWindow => "Press a key to move window to a new workspace",
            Mode::Normal => "Press a key to create a workspace",
        };
        let hint = Label::builder()
            .label(hint_text)
            .css_classes(["hint"])
            .build();
        container.append(&hint);
    }

    for info in &dyn_workspaces {
        let card = build_workspace_card(info, config);
        card.set_widget_name(&crate::config::workspace_name(
            &config.workspace_prefix,
            info.char_id,
        ));
        grid.insert(&card, -1);
    }

    // Disable focus on FlowBoxChild wrappers to suppress focus rings
    let mut child = grid.first_child();
    while let Some(widget) = child {
        widget.set_focusable(false);
        child = widget.next_sibling();
    }

    if !dyn_workspaces.is_empty() {
        container.append(&grid);
    }

    container.append(&build_hint_footer(!dyn_workspaces.is_empty()));

    let error_label = Label::builder()
        .css_classes(["error-message"])
        .wrap(true)
        .build();
    let error_revealer = Revealer::builder()
        .child(&error_label)
        .reveal_child(false)
        .transition_type(RevealerTransitionType::SlideUp)
        .transition_duration(200)
        .build();
    container.append(&error_revealer);

    let backdrop = GtkBox::builder()
        .css_classes(["backdrop"])
        .hexpand(true)
        .vexpand(true)
        .build();

    let overlay = Overlay::builder().child(&backdrop).build();
    overlay.add_overlay(&container);

    window.set_child(Some(&overlay));

    let ctx = ActionContext {
        mode,
        window: window.clone(),
        error_label,
        error_revealer,
        config: config.clone(),
    };

    attach_action_handlers(&grid, &ctx, &config.close_keybinds);
    attach_close_on_backdrop_click(window, &container);
}

fn dispatch_action(ch: char, ctx: &ActionContext) {
    match ctx.mode {
        Mode::Delete => handle_delete_workspace(ch, ctx),
        Mode::MoveWindow => handle_move_window(ch, ctx),
        Mode::Normal => handle_workspace_key(ch, ctx),
    }
}

fn attach_action_handlers(
    grid: &FlowBox,
    ctx: &ActionContext,
    close_keybinds: &[crate::config::Keybind],
) {
    // Click handler on FlowBox children
    let click_ctx = ctx.clone();
    grid.connect_child_activated(move |_, child| {
        let widget = child.child().unwrap();
        let name = widget.widget_name().to_string();
        if let Some(ch) = name
            .strip_prefix(&*click_ctx.config.workspace_prefix)
            .and_then(|s| s.chars().next())
        {
            dispatch_action(ch, &click_ctx);
        }
    });

    // Key handler — capture phase so it fires before child widgets
    let key_ctx = ctx.clone();
    let close_keybinds = close_keybinds.to_vec();
    let key_controller = EventControllerKey::new();
    key_controller.set_name(Some("ndw-key"));
    key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        let relevant_mods = gdk4::ModifierType::CONTROL_MASK
            | gdk4::ModifierType::SHIFT_MASK
            | gdk4::ModifierType::ALT_MASK
            | gdk4::ModifierType::SUPER_MASK;

        for kb in &close_keybinds {
            if key == kb.key && modifier & relevant_mods == kb.modifiers {
                key_ctx.window.close();
                return Propagation::Stop;
            }
        }

        // Tab / Shift+Tab cycle through modes
        if key == gdk4::Key::Tab || key == gdk4::Key::ISO_Left_Tab {
            let next_mode = if key == gdk4::Key::Tab {
                key_ctx.mode.next()
            } else {
                key_ctx.mode.prev()
            };
            let window = key_ctx.window.clone();
            let config = key_ctx.config.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(&window, &config, next_mode);
            });
            return Propagation::Stop;
        }

        // Workspace key: action depends on mode
        // Ignore Super so holding Mod from the opening keybind doesn't block input
        let action_mods = modifier
            & (gdk4::ModifierType::CONTROL_MASK
                | gdk4::ModifierType::SHIFT_MASK
                | gdk4::ModifierType::ALT_MASK);
        if let Some(ch) = key.to_unicode() {
            let ch = ch.to_ascii_lowercase();
            if crate::config::is_workspace_char(ch) && action_mods.is_empty() {
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

fn handle_delete_workspace(ch: char, ctx: &ActionContext) {
    let ws_name = crate::config::workspace_name(&ctx.config.workspace_prefix, ch);
    if let Err(e) = niri::delete_workspace(&ws_name) {
        show_error(ctx, &format!("Failed to delete workspace {ws_name}: {e}"));
        return;
    }
    ctx.window.close();
}

fn handle_move_window(ch: char, ctx: &ActionContext) {
    let ws_name = crate::config::workspace_name(&ctx.config.workspace_prefix, ch);
    if let Err(e) = niri::move_window_to_workspace(&ws_name) {
        show_error(ctx, &format!("Failed to move window to {ws_name}: {e}"));
        return;
    }
    ctx.window.close();
}

fn handle_workspace_key(ch: char, ctx: &ActionContext) {
    let ws_name = crate::config::workspace_name(&ctx.config.workspace_prefix, ch);
    match niri::focus_or_create_workspace(&ws_name) {
        Ok(created) => {
            if created {
                let programs = ctx
                    .config
                    .workspace_programs
                    .get(&ch)
                    .map_or(ctx.config.default_programs.as_slice(), Vec::as_slice);

                match niri::spawn_workspace_programs(&ws_name, programs) {
                    Ok(Some(request)) => {
                        std::thread::Builder::new()
                            .name("reorder".into())
                            .spawn(move || niri::reorder_workspace_columns(&request))
                            .ok();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        show_error(ctx, &format!("Failed to spawn programs: {e:#}"));
                        return;
                    }
                }
            }
        }
        Err(e) => {
            show_error(
                ctx,
                &format!("Failed to switch to workspace {ws_name}: {e}"),
            );
            return;
        }
    }
    ctx.window.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_app_id_variants() {
        assert_eq!(clean_app_id("org.gnome.Terminal"), "Terminal");
        assert_eq!(clean_app_id("firefox"), "Firefox");
        assert_eq!(clean_app_id("com.some.app-name"), "App name");
        assert_eq!(clean_app_id(""), "");
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

    // --- build_dyn_workspace_infos helpers & tests ---

    use crate::test_helpers::{test_window, test_workspace};

    fn default_test_config() -> ResolvedConfig {
        ResolvedConfig {
            workspace_prefix: "dyn-".to_string(),
            max_columns: 4,
            min_columns: 2,
            max_windows_per_card: 4,
            app_name_max_chars: 12,
            window_title_max_chars: 18,
            close_keybinds: Vec::new(),
            default_programs: Vec::new(),
            workspace_programs: HashMap::new(),
            workspace_names: HashMap::new(),
            auto_delete_empty: true,
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
        assert_eq!(infos[1].char_id, 'b');
        // 'a' has 2 windows, 'b' has 1
        assert_eq!(infos[0].windows.len(), 2);
        assert_eq!(infos[1].windows.len(), 1);
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
        assert_eq!(infos[1].configured_programs, vec!["firefox"]);
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
}
