use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, EventControllerKey, FlowBox, GestureClick, Label,
    Orientation, Overlay, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::ResolvedConfig;
use crate::niri;
use crate::niri::ReorderRequest;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Delete,
    MoveWindow,
}

#[derive(Clone)]
struct ActionContext {
    mode: Mode,
    prefix: String,
    window: ApplicationWindow,
    error_label: Label,
    default_programs: Vec<String>,
    workspace_programs: HashMap<char, Vec<String>>,
    reorder_out: Rc<RefCell<Option<ReorderRequest>>>,
}

struct WindowInfo {
    app_name: String,
    title: String,
}

struct DynWorkspaceInfo {
    char_id: char,
    is_focused: bool,
    is_active: bool,
    is_uncreated: bool,
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
    let prefix = &config.workspace_prefix;

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

    // Group windows by workspace_id
    let mut windows_by_ws: HashMap<u64, Vec<WindowInfo>> = HashMap::new();
    for w in &windows {
        if let Some(ws_id) = w.workspace_id {
            let app_name = w.app_id.as_deref().map(clean_app_id).unwrap_or_default();
            let title = w.title.clone().unwrap_or_default();
            windows_by_ws
                .entry(ws_id)
                .or_default()
                .push(WindowInfo { app_name, title });
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
            if !ch.is_ascii_lowercase() {
                return None;
            }

            live_chars.insert(ch);

            let is_focused = Some(ws.id) == focused_ws_id;
            let is_active = !is_focused && ws.is_active;

            let ws_windows = windows_by_ws.remove(&ws.id).unwrap_or_default();
            let name = config.workspace_names.get(&ch).cloned();

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                is_uncreated: false,
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
            name,
            windows: Vec::new(),
            configured_programs: programs,
        });
    }

    infos.sort_by_key(|i| i.char_id);
    infos
}

#[allow(clippy::too_many_lines)]
fn build_workspace_card(info: &DynWorkspaceInfo, config: &ResolvedConfig) -> GtkBox {
    let mut card_classes = vec!["workspace-card"];
    if info.is_uncreated {
        card_classes.push("uncreated");
    } else if info.is_focused {
        card_classes.push("focused");
    } else if info.is_active {
        card_classes.push("active");
    }

    let card = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(card_classes)
        .build();

    // -- Header row --
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

    card.append(&header);

    // -- Divider --
    let divider = Separator::builder().css_classes(["card-divider"]).build();
    card.append(&divider);

    // -- Body --
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

    card.append(&body);
    card
}

pub fn build_ui(
    app: &gtk4::Application,
    config: &ResolvedConfig,
    mode: Mode,
    reorder_out: &Rc<RefCell<Option<ReorderRequest>>>,
) {
    let mut dyn_workspaces = gather_dyn_workspaces(config);
    match mode {
        Mode::Delete => dyn_workspaces.retain(|ws| !ws.is_uncreated),
        Mode::MoveWindow => dyn_workspaces.retain(|ws| !ws.is_uncreated && !ws.is_focused),
        Mode::Normal => {}
    }

    let window = ApplicationWindow::builder().application(app).build();

    window.remove_css_class("background");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);

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

    let grid = FlowBox::builder()
        .css_classes(["workspace-grid"])
        .max_children_per_line(config.max_columns)
        .min_children_per_line(config.min_columns)
        .selection_mode(gtk4::SelectionMode::None)
        .homogeneous(true)
        .build();

    if dyn_workspaces.is_empty() {
        let hint_text = match mode {
            Mode::Delete => "No workspaces to delete",
            Mode::MoveWindow => "Press a\u{2013}z to move window to a new workspace",
            Mode::Normal => "Press a\u{2013}z to create a workspace",
        };
        let hint = Label::builder()
            .label(hint_text)
            .css_classes(["hint"])
            .build();
        container.append(&hint);

        let hint_detail = Label::builder()
            .label("Escape to close")
            .css_classes(["hint-detail"])
            .build();
        container.append(&hint_detail);
    }

    for info in &dyn_workspaces {
        let card = build_workspace_card(info, config);
        card.set_widget_name(&format!("{}{}", config.workspace_prefix, info.char_id));
        grid.insert(&card, -1);
    }

    if !dyn_workspaces.is_empty() {
        container.append(&grid);

        let hint_text = match mode {
            Mode::Delete => "a\u{2013}z delete workspace",
            Mode::MoveWindow => "a\u{2013}z move window to workspace",
            Mode::Normal => "a\u{2013}z switch/create",
        };
        let hint = Label::builder()
            .label(hint_text)
            .css_classes(["hint-detail"])
            .build();
        container.append(&hint);
    }

    let error_label = Label::builder()
        .css_classes(["error-message"])
        .wrap(true)
        .visible(false)
        .build();
    container.append(&error_label);

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
        prefix: config.workspace_prefix.clone(),
        window: window.clone(),
        error_label,
        default_programs: config.default_programs.clone(),
        workspace_programs: config.workspace_programs.clone(),
        reorder_out: Rc::clone(reorder_out),
    };

    attach_action_handlers(&grid, &ctx, &config.close_keybinds);
    attach_close_on_backdrop_click(&window, &container);

    window.present();
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
            .strip_prefix(&*click_ctx.prefix)
            .and_then(|s| s.chars().next())
        {
            dispatch_action(ch, &click_ctx);
        }
    });

    // Key handler — capture phase so it fires before child widgets
    let key_ctx = ctx.clone();
    let close_keybinds = close_keybinds.to_vec();
    let key_controller = EventControllerKey::new();
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

        // a-z: action depends on mode
        // Ignore Super so holding Mod from the opening keybind doesn't block input
        let letter_mods = modifier
            & (gdk4::ModifierType::CONTROL_MASK
                | gdk4::ModifierType::SHIFT_MASK
                | gdk4::ModifierType::ALT_MASK);
        if let Some(ch) = key.to_unicode() {
            let ch = ch.to_ascii_lowercase();
            if ch.is_ascii_lowercase() && letter_mods.is_empty() {
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

fn show_error(error_label: &Label, msg: &str) {
    error_label.set_label(msg);
    error_label.set_visible(true);
}

fn handle_delete_workspace(ch: char, ctx: &ActionContext) {
    let ws_name = format!("{}{ch}", ctx.prefix);
    if let Err(e) = niri::delete_workspace(&ws_name) {
        show_error(
            &ctx.error_label,
            &format!("Failed to delete workspace {ws_name}: {e}"),
        );
        return;
    }
    ctx.window.close();
}

fn handle_move_window(ch: char, ctx: &ActionContext) {
    let ws_name = format!("{}{ch}", ctx.prefix);
    if let Err(e) = niri::move_window_to_workspace(&ws_name) {
        show_error(
            &ctx.error_label,
            &format!("Failed to move window to {ws_name}: {e}"),
        );
        return;
    }
    ctx.window.close();
}

fn snapshot_workspace_window_ids(workspace_name: &str) -> HashSet<u64> {
    let Ok(workspaces) = niri::list_workspaces() else {
        return HashSet::new();
    };
    let ws_id = match workspaces
        .iter()
        .find(|w| w.name.as_deref() == Some(workspace_name))
    {
        Some(w) => w.id,
        None => return HashSet::new(),
    };
    let Ok(windows) = niri::list_windows() else {
        return HashSet::new();
    };
    windows
        .iter()
        .filter(|w| w.workspace_id == Some(ws_id))
        .map(|w| w.id)
        .collect()
}

fn handle_workspace_key(ch: char, ctx: &ActionContext) {
    let ws_name = format!("{}{ch}", ctx.prefix);
    match niri::focus_or_create_workspace(&ws_name) {
        Ok(created) => {
            if created {
                let programs = ctx
                    .workspace_programs
                    .get(&ch)
                    .map_or(ctx.default_programs.as_slice(), Vec::as_slice);

                // Snapshot existing windows before spawning so we can identify new ones
                let existing_ids = if programs.len() >= 2 {
                    snapshot_workspace_window_ids(&ws_name)
                } else {
                    HashSet::new()
                };

                for cmd_str in programs {
                    let parts: Vec<String> = cmd_str.split_whitespace().map(String::from).collect();
                    if parts.is_empty() {
                        continue;
                    }
                    if let Err(e) = niri::spawn_program(&parts) {
                        show_error(
                            &ctx.error_label,
                            &format!("Failed to spawn '{cmd_str}': {e}"),
                        );
                        return;
                    }
                }

                // Request reorder if 2+ programs were spawned
                if programs.len() >= 2 {
                    *ctx.reorder_out.borrow_mut() = Some(ReorderRequest {
                        workspace_name: ws_name,
                        commands: programs.to_vec(),
                        existing_window_ids: existing_ids,
                    });
                }
            }
        }
        Err(e) => {
            show_error(
                &ctx.error_label,
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
}
