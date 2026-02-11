use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, GestureClick, Label,
    Orientation, Overlay, PolicyType, Revealer, RevealerTransitionType, ScrolledWindow,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::config::{KeyboardLayout, ResolvedConfig, TemplateVariable};
use crate::niri;

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

thread_local! {
    static DYNAMIC_PROVIDER: RefCell<Option<gtk4::CssProvider>> = const { RefCell::new(None) };
}

struct KeyboardMetrics {
    key_size: i32,
    key_gap: i32,
    layout: &'static KeyboardLayout,
}

impl KeyboardMetrics {
    /// Compute key size so the keyboard fills ~80% of the monitor width.
    /// The widest row determines the divisor (varies by layout).
    /// With gap = key/8, total width ≈ divisor * key-widths.
    fn from_monitor_width(width: i32, layout: &'static KeyboardLayout) -> Self {
        let target = f64::from(width) * 0.80;
        let key_size = ((target / layout.widest_row_divisor) as i32).clamp(48, 200);
        let key_gap = (key_size + 7) / 8;
        Self {
            key_size,
            key_gap,
            layout,
        }
    }

    fn row_margin(&self, row_idx: usize) -> i32 {
        (self.layout.row_offsets[row_idx] * f64::from(self.key_size + self.key_gap)) as i32
    }

    /// Generate CSS custom properties scaled to the key size.
    fn scaled_css_variables(&self) -> String {
        let ks = self.key_size;
        format!(
            "window {{\n\
             \x20 --key-margin: {km}px;\n\
             \x20 --key-radius: {kr}px;\n\
             \x20 --key-pad-v: {kpv}px;\n\
             \x20 --key-pad-h: {kph}px;\n\
             \x20 --font-char: {fc}px;\n\
             \x20 --font-name: {fn_}px;\n\
             \x20 --font-detail: {fd}px;\n\
             \x20 --section-gap: {sg}px;\n\
             \x20 --font-tab: {ft}px;\n\
             \x20 --tab-pad-h: {tph}px;\n\
             \x20 --tab-radius: {tr}px;\n\
             \x20 --font-footer: {ff}px;\n\
             }}",
            km = ks / 8,
            kr = ks / 8,
            kpv = ks / 16,
            kph = ks / 10,
            fc = ks * 32 / 100,
            fn_ = ks * 13 / 100,
            fd = ks * 10 / 100,
            sg = ks / 5,
            ft = ks * 14 / 100,
            tph = ks / 6,
            tr = ks / 12,
            ff = ks * 15 / 100,
        )
    }
}

fn get_monitor_width() -> i32 {
    gdk4::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|obj| obj.downcast::<gdk4::Monitor>().ok())
        .map_or(1920, |m| m.geometry().width())
}

fn apply_scaled_css(css: &str) {
    let Some(display) = gdk4::Display::default() else {
        return;
    };
    DYNAMIC_PROVIDER.with(|cell| {
        let mut opt = cell.borrow_mut();
        if let Some(old) = opt.take() {
            gtk4::style_context_remove_provider_for_display(&display, &old);
        }
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(css);
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_USER,
        );
        *opt = Some(provider);
    });
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

fn clean_app_id(app_id: &str) -> String {
    let segment = app_id.rsplit('.').next().unwrap_or(app_id);
    let name = segment.replace(['-', '_'], " ");
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
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

#[derive(Clone, Default)]
struct HookInfo {
    template_name: Option<String>,
    variables: HashMap<String, String>,
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
    window_count: usize,
    app_names: Vec<String>,
    configured_programs: Vec<String>,
}

impl DynWorkspaceInfo {
    fn status_text(&self) -> Option<String> {
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

    fn uncreated(ch: char) -> Self {
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

// --- Keyboard layout builders ---

/// Build a map of all keyboard keys to their workspace info.
/// Keys without a live or configured workspace get a default empty entry.
fn build_full_keyboard_info(config: &ResolvedConfig) -> HashMap<char, DynWorkspaceInfo> {
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

fn build_key_widget(
    info: &DynWorkspaceInfo,
    mode: Mode,
    sender: &ComponentSender<AppOverlay>,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let mut classes = vec!["keyboard-key"];

    if info.is_focused || info.is_active {
        classes.push("active");
    }
    if info.is_urgent {
        classes.push("urgent");
    }

    let is_disabled = match mode {
        Mode::MoveWindow => info.is_uncreated || info.is_focused,
        Mode::Delete | Mode::Normal => info.is_uncreated,
    };
    if is_disabled {
        classes.push("disabled");
    }

    let key_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(classes)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    key_box.set_size_request(metrics.key_size, metrics.key_size);

    // Inner box centers the label group vertically within the fixed-size key
    let inner = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .vexpand(true)
        .valign(Align::Center)
        .halign(Align::Center)
        .build();

    // Key character label
    let char_label = Label::builder()
        .label(display_key_char(info.char_id))
        .css_classes(["key-char"])
        .build();
    inner.append(&char_label);

    // Workspace name (if configured)
    if let Some(ref name) = info.name {
        let name_label = Label::builder()
            .label(name)
            .css_classes(["key-name"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(8)
            .build();
        inner.append(&name_label);
    }

    // App names (live workspaces) or configured programs (uncreated)
    let apps_text = if !info.app_names.is_empty() {
        info.app_names.join(", ")
    } else if info.is_uncreated && !info.configured_programs.is_empty() {
        info.configured_programs.join(", ")
    } else {
        String::new()
    };
    if !apps_text.is_empty() {
        let apps_label = Label::builder()
            .label(&apps_text)
            .css_classes(["key-apps"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(10)
            .build();
        inner.append(&apps_label);
    }

    // Status line
    if let Some(ref text) = info.status_text() {
        let status_label = Label::builder()
            .label(text.as_str())
            .css_classes(["key-status"])
            .build();
        inner.append(&status_label);
    }

    key_box.append(&inner);

    // Click handler
    let ch = info.char_id;
    let click_sender = sender.clone();
    let click = GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        click_sender.input(AppMsg::WorkspaceKeyPressed(ch));
    });
    key_box.add_controller(click);

    key_box
}

fn build_keyboard(
    infos: &HashMap<char, DynWorkspaceInfo>,
    mode: Mode,
    sender: &ComponentSender<AppOverlay>,
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
                row_box.append(&build_key_widget(info, mode, sender, metrics));
            }
        }

        keyboard.append(&row_box);
    }

    keyboard
}

// --- Template picker ---

/// An option in the template picker (either "Empty" or a named template).
struct TemplateOption {
    key: Option<char>,
    name: String,
    programs: Vec<String>,
    variables: Vec<TemplateVariable>,
}

fn build_template_options(config: &ResolvedConfig) -> Vec<TemplateOption> {
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

fn build_template_option_widget(
    opt: &TemplateOption,
    is_selected: bool,
    metrics: &KeyboardMetrics,
) -> GtkBox {
    let mut classes = vec!["template-option"];
    if is_selected {
        classes.push("selected");
    }

    let row = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(metrics.key_gap)
        .css_classes(classes)
        .build();

    // Key badge — styled like a small keyboard key
    let key_text = opt.key.map_or_else(String::new, display_key_char);
    let key_badge = Label::builder()
        .label(&key_text)
        .css_classes(["template-key"])
        .build();
    row.append(&key_badge);

    // Text column: name on top, programs below
    let text_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .halign(Align::Start)
        .hexpand(true)
        .build();

    let name_label = Label::builder()
        .label(&opt.name)
        .css_classes(["template-name"])
        .halign(Align::Start)
        .build();
    text_box.append(&name_label);

    if !opt.programs.is_empty() {
        let programs_text = opt.programs.join(", ");
        let programs_label = Label::builder()
            .label(&programs_text)
            .css_classes(["template-programs"])
            .halign(Align::Start)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(40)
            .build();
        text_box.append(&programs_label);
    }

    row.append(&text_box);
    row
}

fn update_selection(option_widgets: &[GtkBox], selected: usize) {
    for (i, w) in option_widgets.iter().enumerate() {
        if i == selected {
            w.add_css_class("selected");
        } else {
            w.remove_css_class("selected");
        }
    }
}

// --- Shared helpers ---

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

// --- Relm4 Component ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewState {
    Keyboard,
    TemplatePicker,
    VariableInput,
}

#[derive(Debug)]
pub enum AppMsg {
    // Keyboard view
    WorkspaceKeyPressed(char),
    NextMode,
    PrevMode,
    SetMode(Mode),
    // Template picker
    TemplateUp,
    TemplateDown,
    TemplateConfirm,
    TemplateSelectAndConfirm(usize),
    TemplateHotkey(char),
    // Variable input
    VariableSubmit,
    // Navigation
    Back,
    Close,
    ShowError(String),
}

pub struct OverlayInit {
    pub app: gtk4::Application,
    pub config: Rc<ResolvedConfig>,
    pub mode: Mode,
}

pub struct AppOverlay {
    config: Rc<ResolvedConfig>,
    metrics: KeyboardMetrics,
    keyboard_infos: HashMap<char, DynWorkspaceInfo>,
    view: ViewState,
    mode: Mode,
    error_message: Option<String>,
    // Template picker state
    template_selected_idx: usize,
    template_options: Vec<TemplateOption>,
    template_option_widgets: Vec<GtkBox>,
    // Pending action state (workspace char we're creating)
    pending_ws_char: Option<char>,
    pending_template_name: Option<String>,
    pending_programs: Vec<String>,
    pending_var_names: Vec<String>,
}

pub struct AppWidgets {
    popup_container: GtkBox,
    error_label: Label,
    error_revealer: Revealer,
    rendered_view: ViewState,
    rendered_mode: Mode,
    variable_entries: Rc<RefCell<Vec<Entry>>>,
}

impl AppOverlay {
    fn switch_and_close(
        &self,
        ws_name: &str,
        ws_key: char,
        programs: &[String],
        hook_info: &HookInfo,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let result = niri::switch_workspace(ws_name, programs).map(|(created, req)| {
            if let Some(r) = req {
                std::thread::Builder::new()
                    .name("reorder".into())
                    .spawn(move || niri::reorder_workspace_columns(&r))
                    .ok();
            }
            if created {
                let hooks = crate::config::collect_create_hooks(
                    &self.config,
                    hook_info.template_name.as_deref(),
                );
                let env = crate::config::build_hook_env(
                    ws_name,
                    ws_key,
                    hook_info.template_name.as_deref(),
                    &hook_info.variables,
                );
                niri::run_hooks(&hooks, &env);
            }
        });
        if let Err(e) = result {
            sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
            return;
        }
        root.close();
    }

    fn handle_workspace_key(
        &mut self,
        ch: char,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);

        match self.mode {
            Mode::Normal => {
                let is_uncreated = self
                    .keyboard_infos
                    .get(&ch)
                    .is_none_or(|info| info.is_uncreated);
                if is_uncreated && self.config.should_show_templates(ch) {
                    self.pending_ws_char = Some(ch);
                    self.template_selected_idx = 0;
                    self.template_options = build_template_options(&self.config);
                    self.view = ViewState::TemplatePicker;
                    return;
                }
                let programs = self.config.programs_for(ch);
                self.switch_and_close(&ws_name, ch, programs, &HookInfo::default(), root, sender);
            }
            Mode::Delete => {
                let result = niri::delete_workspace(&ws_name);
                if result.is_ok() {
                    let env = crate::config::build_hook_env(&ws_name, ch, None, &HashMap::new());
                    niri::run_hooks(&self.config.hooks.on_delete, &env);
                }
                if let Err(e) = result {
                    sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
                    return;
                }
                root.close();
            }
            Mode::MoveWindow => {
                if let Err(e) = niri::move_window_to_workspace(&ws_name) {
                    sender.input(AppMsg::ShowError(format!("Failed: {e:#}")));
                    return;
                }
                root.close();
            }
        }
    }

    fn handle_template_confirm(
        &mut self,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let Some(ch) = self.pending_ws_char else {
            return;
        };
        let idx = self.template_selected_idx;
        if idx >= self.template_options.len() {
            return;
        }

        let template_name = if self.template_options[idx].name == "Empty" {
            None
        } else {
            Some(self.template_options[idx].name.clone())
        };

        if self.template_options[idx].variables.is_empty() {
            let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);
            let hook_info = HookInfo {
                template_name,
                variables: HashMap::new(),
            };
            self.switch_and_close(
                &ws_name,
                ch,
                &self.template_options[idx].programs,
                &hook_info,
                root,
                sender,
            );
        } else {
            // Transition to variable input
            self.pending_template_name = template_name;
            self.pending_programs = self.template_options[idx].programs.clone();
            self.pending_var_names = self.template_options[idx]
                .variables
                .iter()
                .map(|v| v.name.clone())
                .collect();
            self.view = ViewState::VariableInput;
        }
    }

    fn handle_template_hotkey(
        &mut self,
        pressed: char,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let Some(idx) = self
            .template_options
            .iter()
            .position(|opt| opt.key == Some(pressed))
        else {
            return;
        };
        self.template_selected_idx = idx;
        self.handle_template_confirm(root, sender);
    }

    fn handle_variable_submit(
        &mut self,
        widgets: &AppWidgets,
        root: &ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let Some(ch) = self.pending_ws_char else {
            return;
        };
        let ws_name = crate::config::workspace_name(&self.config.workspace_prefix, ch);

        let entries = widgets.variable_entries.borrow();
        let mut values = HashMap::new();
        for (name, entry) in self.pending_var_names.iter().zip(entries.iter()) {
            values.insert(name.clone(), entry.text().to_string());
        }

        let substituted = crate::config::substitute_variables(&self.pending_programs, &values);
        let hook_info = HookInfo {
            template_name: self.pending_template_name.clone(),
            variables: values,
        };
        self.switch_and_close(&ws_name, ch, &substituted, &hook_info, root, sender);
    }

    /// Build the keyboard view contents into the container.
    fn build_keyboard_view(
        &self,
        container: &GtkBox,
        sender: &ComponentSender<Self>,
        error_revealer: &Revealer,
        metrics: &KeyboardMetrics,
    ) -> GtkBox {
        let keyboard = build_keyboard(&self.keyboard_infos, self.mode, sender, metrics);
        let mode_tabs = self.build_mode_tabs(sender, metrics);

        container.append(&keyboard);
        container.append(&build_hint_footer(
            metrics,
            &["press key to select", "Tab switch mode", "Escape close"],
        ));
        container.append(error_revealer);
        container.append(&mode_tabs);

        mode_tabs
    }

    fn build_mode_tabs(
        &self,
        sender: &ComponentSender<Self>,
        _metrics: &KeyboardMetrics,
    ) -> GtkBox {
        let mode_tabs = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(0)
            .css_classes(["mode-tabs"])
            .halign(Align::Center)
            .build();
        for m in Mode::all() {
            let mut classes = vec!["mode-tab", m.css_class()];
            if m == self.mode {
                classes.push("active");
            }
            let tab_label = Label::builder()
                .label(m.display_name())
                .css_classes(classes)
                .build();

            let tab_sender = sender.clone();
            let click = GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                tab_sender.input(AppMsg::SetMode(m));
            });
            tab_label.add_controller(click);

            mode_tabs.append(&tab_label);
        }
        mode_tabs
    }

    /// Build the template picker view contents into the container.
    fn build_template_picker_view(
        &mut self,
        container: &GtkBox,
        sender: &ComponentSender<Self>,
        error_revealer: &Revealer,
        metrics: &KeyboardMetrics,
    ) {
        let ch = self.pending_ws_char.unwrap_or('?');

        let title = Label::builder()
            .label(format!(
                "Create workspace {}",
                format_workspace_display(ch, &self.config)
            ))
            .css_classes(["template-title"])
            .build();
        container.append(&title);

        let list_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(metrics.key_gap / 2)
            .css_classes(["template-list"])
            .build();

        self.template_option_widgets = self
            .template_options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let widget =
                    build_template_option_widget(opt, i == self.template_selected_idx, metrics);
                list_box.append(&widget);

                let click_sender = sender.clone();
                let click = GestureClick::new();
                click.connect_released(move |_, _, _, _| {
                    click_sender.input(AppMsg::TemplateSelectAndConfirm(i));
                });
                widget.add_controller(click);

                widget
            })
            .collect();

        let scrolled = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .max_content_height(metrics.key_size * 5)
            .propagate_natural_height(true)
            .child(&list_box)
            .build();
        container.append(&scrolled);

        container.append(error_revealer);

        container.append(&build_hint_footer(
            metrics,
            &[
                "press key to select",
                "\u{2191}\u{2193} navigate",
                "Enter confirm",
                "Escape cancel",
            ],
        ));
    }

    /// Build the variable input form contents into the container.
    fn build_variable_input_view(
        &self,
        container: &GtkBox,
        error_revealer: &Revealer,
        metrics: &KeyboardMetrics,
    ) -> Vec<Entry> {
        let ch = self.pending_ws_char.unwrap_or('?');
        let tmpl_name = self.pending_template_name.as_deref().unwrap_or("Template");

        let title = Label::builder()
            .label(format!(
                "{tmpl_name} \u{2192} {}",
                format_workspace_display(ch, &self.config)
            ))
            .css_classes(["variable-title"])
            .build();
        container.append(&title);

        let form = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(metrics.key_gap / 2)
            .css_classes(["variable-form"])
            .build();

        // Get the template variables from the selected template option
        let idx = self.template_selected_idx;
        let variables = if idx < self.template_options.len() {
            &self.template_options[idx].variables
        } else {
            &[] as &[TemplateVariable]
        };

        let entries: Vec<Entry> = variables
            .iter()
            .map(|var| {
                let row = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(0)
                    .css_classes(["variable-row"])
                    .build();

                let label = Label::builder()
                    .label(&var.label)
                    .css_classes(["variable-label"])
                    .halign(Align::Start)
                    .build();
                row.append(&label);

                let entry = Entry::builder()
                    .css_classes(["variable-entry"])
                    .placeholder_text(&var.name)
                    .build();
                row.append(&entry);

                form.append(&row);
                entry
            })
            .collect();

        container.append(&form);
        container.append(error_revealer);

        container.append(&build_hint_footer(
            metrics,
            &["Enter create", "Escape back"],
        ));

        entries
    }
}

impl Component for AppOverlay {
    type Init = OverlayInit;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        let window = ApplicationWindow::builder().build();
        window.remove_css_class("background");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Register window with GTK application
        root.set_application(Some(&init.app));
        root.set_widget_name(init.mode.widget_name());

        // Compute metrics and apply scaled CSS
        let metrics = KeyboardMetrics::from_monitor_width(get_monitor_width(), init.config.layout);
        apply_scaled_css(&metrics.scaled_css_variables());

        // Gather workspace state
        let keyboard_infos = build_full_keyboard_info(&init.config);

        // Build initial model
        let model = AppOverlay {
            config: init.config,
            metrics,
            keyboard_infos,
            view: ViewState::Keyboard,
            mode: init.mode,
            error_message: None,
            template_selected_idx: 0,
            template_options: Vec::new(),
            template_option_widgets: Vec::new(),
            pending_ws_char: None,
            pending_template_name: None,
            pending_programs: Vec::new(),
            pending_var_names: Vec::new(),
        };

        // Build container
        let mut container_classes = vec!["popup-container"];
        if let Some(cls) = model.mode.container_css_class() {
            container_classes.push(cls);
        }

        let popup_container = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(0)
            .css_classes(container_classes)
            .halign(Align::Center)
            .valign(Align::Center)
            .build();

        let (error_label, error_revealer) = create_error_revealer();

        // Build keyboard view
        model.build_keyboard_view(&popup_container, &sender, &error_revealer, &model.metrics);

        // Wrap in backdrop
        wrap_in_backdrop(&root, &popup_container);

        // Attach close-on-backdrop-click
        {
            let close_sender = sender.clone();
            let container_ref = popup_container.clone();
            let window_ref = root.clone();
            let click = GestureClick::new();
            click.set_name(Some("ndw-backdrop"));
            click.connect_released(move |_, _, x, y| {
                let (cx, cy) = container_ref
                    .translate_coordinates(&window_ref, 0.0, 0.0)
                    .unwrap_or((0.0, 0.0));
                let cw = f64::from(container_ref.width());
                let ch = f64::from(container_ref.height());
                if x < cx || x > cx + cw || y < cy || y > cy + ch {
                    close_sender.input(AppMsg::Close);
                }
            });
            root.add_controller(click);
        }

        // Attach keyboard event handler
        attach_keyboard_key_handler(&root, &model.config, &sender);

        let widgets = AppWidgets {
            popup_container,
            error_label,
            error_revealer,
            rendered_view: ViewState::Keyboard,
            rendered_mode: model.mode,
            variable_entries: Rc::new(RefCell::new(Vec::new())),
        };

        root.present();

        ComponentParts { model, widgets }
    }

    #[expect(clippy::too_many_lines, reason = "message dispatch with view rebuild")]
    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        // Clear error on any action except ShowError
        if !matches!(message, AppMsg::ShowError(_)) {
            self.error_message = None;
        }

        match message {
            AppMsg::WorkspaceKeyPressed(ch) => {
                self.handle_workspace_key(ch, root, &sender);
            }
            AppMsg::NextMode => {
                self.mode = self.mode.next();
            }
            AppMsg::PrevMode => {
                self.mode = self.mode.prev();
            }
            AppMsg::SetMode(mode) => {
                self.mode = mode;
            }
            AppMsg::TemplateUp => {
                if !self.template_options.is_empty() {
                    let count = self.template_options.len();
                    self.template_selected_idx = if self.template_selected_idx == 0 {
                        count - 1
                    } else {
                        self.template_selected_idx - 1
                    };
                    update_selection(&self.template_option_widgets, self.template_selected_idx);
                }
            }
            AppMsg::TemplateDown => {
                if !self.template_options.is_empty() {
                    let count = self.template_options.len();
                    self.template_selected_idx = if self.template_selected_idx >= count - 1 {
                        0
                    } else {
                        self.template_selected_idx + 1
                    };
                    update_selection(&self.template_option_widgets, self.template_selected_idx);
                }
            }
            AppMsg::TemplateConfirm => {
                self.handle_template_confirm(root, &sender);
            }
            AppMsg::TemplateSelectAndConfirm(idx) => {
                self.template_selected_idx = idx;
                self.handle_template_confirm(root, &sender);
            }
            AppMsg::TemplateHotkey(ch) => {
                self.handle_template_hotkey(ch, root, &sender);
            }
            AppMsg::VariableSubmit => {
                self.handle_variable_submit(widgets, root, &sender);
            }
            AppMsg::Back => match self.view {
                ViewState::Keyboard => root.close(),
                ViewState::TemplatePicker => {
                    self.view = ViewState::Keyboard;
                    self.template_options.clear();
                    self.template_option_widgets.clear();
                    self.pending_ws_char = None;
                }
                ViewState::VariableInput => {
                    self.view = ViewState::TemplatePicker;
                    self.pending_template_name = None;
                    self.pending_programs.clear();
                    self.pending_var_names.clear();
                }
            },
            AppMsg::Close => root.close(),
            AppMsg::ShowError(msg) => {
                self.error_message = Some(msg);
            }
        }

        // --- Update view ---
        let view_changed = self.view != widgets.rendered_view;
        let mode_changed = self.mode != widgets.rendered_mode;

        // Sync error display
        if let Some(ref msg) = self.error_message {
            widgets.error_label.set_label(msg);
            widgets.error_revealer.set_reveal_child(true);
        } else {
            widgets.error_revealer.set_reveal_child(false);
        }

        if view_changed || (mode_changed && self.view == ViewState::Keyboard) {
            // Rebuild the view
            root.set_widget_name(self.mode.widget_name());
            remove_app_controllers(root);

            // Clear the popup container
            while let Some(child) = widgets.popup_container.first_child() {
                widgets.popup_container.remove(&child);
            }

            // Rebuild container CSS classes
            widgets.popup_container.remove_css_class("delete-mode");
            widgets.popup_container.remove_css_class("move-window-mode");
            widgets.popup_container.remove_css_class("template-picker");

            match self.view {
                ViewState::Keyboard => {
                    if let Some(cls) = self.mode.container_css_class() {
                        widgets.popup_container.add_css_class(cls);
                    }

                    // Recompute metrics
                    self.metrics = KeyboardMetrics::from_monitor_width(
                        get_monitor_width(),
                        self.config.layout,
                    );
                    apply_scaled_css(&self.metrics.scaled_css_variables());

                    let (error_label, error_revealer) = create_error_revealer();
                    self.build_keyboard_view(
                        &widgets.popup_container,
                        &sender,
                        &error_revealer,
                        &self.metrics,
                    );
                    widgets.error_label = error_label;
                    widgets.error_revealer = error_revealer;

                    attach_keyboard_key_handler(root, &self.config, &sender);
                }
                ViewState::TemplatePicker => {
                    widgets.popup_container.add_css_class("template-picker");

                    self.metrics = KeyboardMetrics::from_monitor_width(
                        get_monitor_width(),
                        self.config.layout,
                    );
                    apply_scaled_css(&self.metrics.scaled_css_variables());

                    let (error_label, error_revealer) = create_error_revealer();
                    let key_size = self.metrics.key_size;
                    let key_gap = self.metrics.key_gap;
                    let layout = self.metrics.layout;
                    let metrics_copy = KeyboardMetrics {
                        key_size,
                        key_gap,
                        layout,
                    };
                    self.build_template_picker_view(
                        &widgets.popup_container,
                        &sender,
                        &error_revealer,
                        &metrics_copy,
                    );
                    widgets.error_label = error_label;
                    widgets.error_revealer = error_revealer;

                    attach_template_key_handler(root, &self.config, &sender);
                }
                ViewState::VariableInput => {
                    widgets.popup_container.add_css_class("template-picker");

                    self.metrics = KeyboardMetrics::from_monitor_width(
                        get_monitor_width(),
                        self.config.layout,
                    );
                    apply_scaled_css(&self.metrics.scaled_css_variables());

                    let (error_label, error_revealer) = create_error_revealer();
                    let entries = self.build_variable_input_view(
                        &widgets.popup_container,
                        &error_revealer,
                        &self.metrics,
                    );
                    widgets.error_label = error_label;
                    widgets.error_revealer = error_revealer;

                    if let Some(first) = entries.first() {
                        first.grab_focus();
                    }

                    *widgets.variable_entries.borrow_mut() = entries;

                    attach_variable_key_handler(root, &self.config, &sender);
                }
            }

            widgets.rendered_view = self.view;
            widgets.rendered_mode = self.mode;
        }
    }
}

// --- Key handler attachment ---

fn attach_keyboard_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &ComponentSender<AppOverlay>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if matches_close_keybind(key, modifier, &close_keybinds) {
            key_sender.input(AppMsg::Close);
            return Propagation::Stop;
        }

        // Tab / Shift+Tab cycle through modes
        if key == gdk4::Key::Tab {
            key_sender.input(AppMsg::NextMode);
            return Propagation::Stop;
        }
        if key == gdk4::Key::ISO_Left_Tab {
            key_sender.input(AppMsg::PrevMode);
            return Propagation::Stop;
        }

        // Workspace key
        if let Some(ch) = key.to_unicode() {
            let ch = ch.to_ascii_lowercase();
            if crate::config::is_workspace_char(ch) && (modifier & ACTION_MODS).is_empty() {
                key_sender.input(AppMsg::WorkspaceKeyPressed(ch));
                return Propagation::Stop;
            }
        }

        Propagation::Proceed
    });
    window.add_controller(key_controller);
}

fn attach_template_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &ComponentSender<AppOverlay>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if matches_close_keybind(key, modifier, &close_keybinds) {
            key_sender.input(AppMsg::Back);
            return Propagation::Stop;
        }

        if key == gdk4::Key::Up || key == gdk4::Key::KP_Up {
            key_sender.input(AppMsg::TemplateUp);
            return Propagation::Stop;
        }
        if key == gdk4::Key::Down || key == gdk4::Key::KP_Down {
            key_sender.input(AppMsg::TemplateDown);
            return Propagation::Stop;
        }

        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            key_sender.input(AppMsg::TemplateConfirm);
            return Propagation::Stop;
        }

        if let Some(pressed) = key.to_unicode() {
            let pressed = pressed.to_ascii_lowercase();
            if (modifier & ACTION_MODS).is_empty() {
                key_sender.input(AppMsg::TemplateHotkey(pressed));
                return Propagation::Stop;
            }
        }

        Propagation::Proceed
    });
    window.add_controller(key_controller);
}

fn attach_variable_key_handler(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    sender: &ComponentSender<AppOverlay>,
) {
    let close_keybinds = config.close_keybinds.clone();
    let key_sender = sender.clone();
    let key_controller = new_key_controller();

    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        if matches_close_keybind(key, modifier, &close_keybinds) {
            key_sender.input(AppMsg::Back);
            return Propagation::Stop;
        }

        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            key_sender.input(AppMsg::VariableSubmit);
            return Propagation::Stop;
        }

        // Let GTK handle Tab, text input, etc.
        Propagation::Proceed
    });
    window.add_controller(key_controller);
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

    // --- Keyboard layout coverage ---

    use crate::config::{ALL_LAYOUTS, LAYOUT_DVORAK, LAYOUT_QWERTY};

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

    #[test]
    fn keyboard_metrics_qwerty_from_1920() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_QWERTY);
        // 1920 * 0.80 / 11.6875 ≈ 131
        assert_eq!(m.key_size, 131);
        assert_eq!(m.key_gap, 17); // (131 + 7) / 8 = 17
    }

    #[test]
    fn keyboard_metrics_dvorak_from_1920() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_DVORAK);
        // 1920 * 0.80 / 11.96875 ≈ 128
        assert_eq!(m.key_size, 128);
        assert_eq!(m.key_gap, 16); // (128 + 7) / 8 = 16
    }

    #[test]
    fn keyboard_metrics_row_margins() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_QWERTY);
        assert_eq!(m.row_margin(0), 0);
        // Row 1: 0.5 * (131 + 17) = 74
        assert_eq!(m.row_margin(1), 74);
        // Row 2: 0.75 * 148 = 111
        assert_eq!(m.row_margin(2), 111);
        // Row 3: 1.25 * 148 = 185
        assert_eq!(m.row_margin(3), 185);
    }

    #[test]
    fn keyboard_metrics_clamps_small() {
        let m = KeyboardMetrics::from_monitor_width(400, &LAYOUT_QWERTY);
        assert_eq!(m.key_size, 48); // clamped to minimum
    }

    #[test]
    fn keyboard_metrics_clamps_large() {
        let m = KeyboardMetrics::from_monitor_width(8000, &LAYOUT_QWERTY);
        assert_eq!(m.key_size, 200); // clamped to maximum
    }

    // --- build_dyn_workspace_infos helpers & tests ---

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
            hooks: crate::config::HookConfig::default(),
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
        assert_eq!(infos[0].window_count, 2);
        assert_eq!(infos[1].window_count, 1);
        // app_names are cleaned and sorted
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
        // 'a' is live, 'b' is uncreated
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

        // Uncreated with no windows → None
        assert_eq!(info.status_text(), None);

        // Focused
        info.is_focused = true;
        info.is_uncreated = false;
        assert_eq!(info.status_text().as_deref(), Some("focused"));

        // Active (not focused)
        info.is_focused = false;
        info.is_active = true;
        assert_eq!(info.status_text().as_deref(), Some("active"));

        // Empty (live, no windows)
        info.is_active = false;
        assert_eq!(info.status_text().as_deref(), Some("empty"));

        // 1 window
        info.window_count = 1;
        assert_eq!(info.status_text().as_deref(), Some("1 win"));

        // Multiple windows
        info.window_count = 5;
        assert_eq!(info.status_text().as_deref(), Some("5 win"));
    }

    // --- build_template_options ---

    #[test]
    fn build_template_options_empty_first_with_key_1() {
        use crate::config::Template;

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
        // First option is always "Empty" with key '1'
        assert_eq!(opts[0].name, "Empty");
        assert_eq!(opts[0].key, Some('1'));
        assert_eq!(opts[0].programs, vec!["kitty"]);
        // Templates follow in config order
        assert_eq!(opts[1].name, "dev");
        assert_eq!(opts[1].key, Some('d'));
        assert_eq!(opts[2].name, "browser");
        assert_eq!(opts[2].key, Some('2'));
    }

    #[test]
    fn build_template_options_no_default_programs() {
        use crate::config::Template;

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
