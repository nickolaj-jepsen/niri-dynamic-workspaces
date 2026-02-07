use std::collections::HashMap;

use gdk4::ModifierType;
use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, EventControllerKey, FlowBox, GestureClick, Label,
    Orientation, Overlay, Separator,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::ResolvedConfig;
use crate::niri;

struct WindowInfo {
    app_name: String,
    title: String,
}

struct DynWorkspaceInfo {
    char_id: char,
    is_focused: bool,
    is_active: bool,
    output_name: Option<String>,
    windows: Vec<WindowInfo>,
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

fn gather_dyn_workspaces(prefix: &str) -> Vec<DynWorkspaceInfo> {
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

    let first_output = workspaces.first().and_then(|ws| ws.output.as_deref());
    let is_multi_monitor = workspaces
        .iter()
        .any(|ws| ws.output.as_deref() != first_output);

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

    let mut infos: Vec<DynWorkspaceInfo> = workspaces
        .iter()
        .filter_map(|ws| {
            let name = ws.name.as_ref()?;
            let ch = name.strip_prefix(prefix)?.chars().next()?;
            if !ch.is_ascii_lowercase() {
                return None;
            }

            let is_focused = Some(ws.id) == focused_ws_id;
            let is_active = !is_focused && ws.is_active;

            let output_name = if is_multi_monitor {
                ws.output.clone()
            } else {
                None
            };

            let ws_windows = windows_by_ws.remove(&ws.id).unwrap_or_default();

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                output_name,
                windows: ws_windows,
            })
        })
        .collect();

    infos.sort_by_key(|i| i.char_id);
    infos
}

#[allow(clippy::too_many_lines)]
fn build_workspace_card(info: &DynWorkspaceInfo, config: &ResolvedConfig) -> GtkBox {
    let mut card_classes = vec!["workspace-card"];
    if info.is_focused {
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

    if let Some(ref output) = info.output_name {
        let monitor_label = Label::builder()
            .label(output)
            .css_classes(["card-monitor"])
            .build();
        header.append(&monitor_label);
    }

    let spacer = GtkBox::builder().hexpand(true).build();
    header.append(&spacer);

    let count_text = match info.windows.len() {
        0 => "empty".to_string(),
        1 => "1 window".to_string(),
        n => format!("{n} windows"),
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

    if info.windows.is_empty() {
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

#[allow(clippy::too_many_lines)]
pub fn build_ui(app: &gtk4::Application, config: &ResolvedConfig) {
    let dyn_workspaces = gather_dyn_workspaces(&config.workspace_prefix);

    let window = ApplicationWindow::builder().application(app).build();

    window.remove_css_class("background");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["popup-container"])
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
        let hint = Label::builder()
            .label("Press a\u{2013}z to create a workspace")
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

        let hint = Label::builder()
            .label("a\u{2013}z switch/create \u{00b7} Shift+key delete")
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

    // Click handler on FlowBox children
    {
        let window_ref = window.clone();
        let error_ref = error_label.clone();
        let prefix = config.workspace_prefix.clone();
        grid.connect_child_activated(move |_, child| {
            let widget = child.child().unwrap();
            let name = widget.widget_name().to_string();
            if let Some(ch) = name.strip_prefix(&*prefix).and_then(|s| s.chars().next()) {
                handle_workspace_key(ch, &prefix, &window_ref, &error_ref);
            }
        });
    }

    // Key handler — capture phase so it fires before child widgets
    {
        let window_ref = window.clone();
        let error_ref = error_label.clone();
        let close_keybinds = config.close_keybinds.clone();
        let delete_modifier = config.delete_modifier;
        let prefix = config.workspace_prefix.clone();
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            let relevant_mods = ModifierType::CONTROL_MASK
                | ModifierType::SHIFT_MASK
                | ModifierType::ALT_MASK
                | ModifierType::SUPER_MASK;

            // Check close keybinds
            for kb in &close_keybinds {
                if key == kb.key && modifier & relevant_mods == kb.modifiers {
                    window_ref.close();
                    return Propagation::Stop;
                }
            }

            // a-z with delete_modifier or no modifier
            let active = modifier & relevant_mods;
            if let Some(ch) = key.to_unicode() {
                let ch = ch.to_ascii_lowercase();
                if ch.is_ascii_lowercase() {
                    if active == delete_modifier {
                        handle_delete_workspace(ch, &prefix, &window_ref, &error_ref);
                    } else if active.is_empty() {
                        handle_workspace_key(ch, &prefix, &window_ref, &error_ref);
                    }
                    return Propagation::Stop;
                }
            }

            Propagation::Proceed
        });
        window.add_controller(key_controller);
    }

    // Close on click outside the popup container
    {
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

    window.present();
}

fn show_error(error_label: &Label, msg: &str) {
    error_label.set_label(msg);
    error_label.set_visible(true);
}

fn handle_delete_workspace(
    ch: char,
    prefix: &str,
    window: &ApplicationWindow,
    error_label: &Label,
) {
    let ws_name = format!("{prefix}{ch}");
    if let Err(e) = niri::delete_workspace(&ws_name) {
        show_error(
            error_label,
            &format!("Failed to delete workspace {ws_name}: {e}"),
        );
        return;
    }
    window.close();
}

fn handle_workspace_key(ch: char, prefix: &str, window: &ApplicationWindow, error_label: &Label) {
    let ws_name = format!("{prefix}{ch}");
    if let Err(e) = niri::focus_or_create_workspace(&ws_name) {
        show_error(
            error_label,
            &format!("Failed to switch to workspace {ws_name}: {e}"),
        );
        return;
    }
    window.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_app_id_dotted_name() {
        assert_eq!(clean_app_id("org.gnome.Terminal"), "Terminal");
    }

    #[test]
    fn clean_app_id_no_dots() {
        assert_eq!(clean_app_id("firefox"), "Firefox");
    }

    #[test]
    fn clean_app_id_replaces_separators() {
        assert_eq!(clean_app_id("my-app_name"), "My app name");
    }

    #[test]
    fn clean_app_id_empty_string() {
        assert_eq!(clean_app_id(""), "");
    }

    #[test]
    fn clean_app_id_nautilus() {
        assert_eq!(clean_app_id("org.gnome.Nautilus"), "Nautilus");
    }

    #[test]
    fn clean_app_id_dotted_with_separators() {
        assert_eq!(clean_app_id("com.some.app-name"), "App name");
    }
}
