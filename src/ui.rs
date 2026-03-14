use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use glib::Propagation;
use gtk4::prelude::*;
use gtk4::{
    Align, ApplicationWindow, Box as GtkBox, Entry, EventControllerKey, EventControllerMotion,
    GestureClick, Label, Orientation, Overlay, PolicyType, Revealer, RevealerTransitionType,
    ScrolledWindow,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::config::{KeyboardLayout, ResolvedConfig, Select, TemplateVariable, VariableType};
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

fn get_monitor_width(monitor: Option<&gdk4::Monitor>) -> i32 {
    monitor
        .map(|m| m.geometry().width())
        .or_else(|| {
            gdk4::Display::default()
                .and_then(|d| d.monitors().item(0))
                .and_then(|obj| obj.downcast::<gdk4::Monitor>().ok())
                .map(|m| m.geometry().width())
        })
        .unwrap_or(1920)
}

/// Return the niri output name that contains the focused workspace.
fn find_focused_output() -> Option<String> {
    niri::list_workspaces()
        .ok()?
        .into_iter()
        .find(|w| w.is_focused)?
        .output
}

/// Look up a GDK monitor by its connector (output) name.
fn find_monitor_for_output(output_name: &str) -> Option<gdk4::Monitor> {
    let display = gdk4::Display::default()?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        let monitor = monitors.item(i)?.downcast::<gdk4::Monitor>().ok()?;
        if monitor.connector().as_deref() == Some(output_name) {
            return Some(monitor);
        }
    }
    None
}

/// Find the GDK monitor that contains the focused niri workspace.
fn find_focused_monitor() -> Option<gdk4::Monitor> {
    find_monitor_for_output(&find_focused_output()?)
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
    ws_name: Option<String>,
}

impl DynWorkspaceInfo {
    fn uncreated(ch: char) -> Self {
        Self {
            char_id: ch,
            is_focused: false,
            is_active: false,
            is_uncreated: true,
            is_urgent: false,
            name: None,
            ws_name: None,
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

    // Track urgency per workspace
    let mut urgent_ws_ids: HashSet<u64> = HashSet::new();
    for w in windows {
        if let Some(ws_id) = w.workspace_id {
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

            let name = config
                .workspace_names
                .get(&ch)
                .cloned()
                .or_else(|| crate::config::extract_workspace_title(ws_name, prefix));
            let is_urgent = urgent_ws_ids.contains(&ws.id);

            Some(DynWorkspaceInfo {
                char_id: ch,
                is_focused,
                is_active,
                is_uncreated: false,
                is_urgent,
                name,
                ws_name: Some(ws_name.clone()),
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
    ctx: &ActionContext,
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

    // Workspace name (if configured or extracted from title)
    if let Some(ref name) = info.name {
        let name_label = Label::builder()
            .label(name)
            .css_classes(["key-name"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(8)
            .build();
        inner.append(&name_label);
    }

    key_box.append(&inner);

    // Click handler
    let ch = info.char_id;
    let click_ctx = ctx.clone();
    let click = GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        dispatch_action(ch, &click_ctx);
    });
    key_box.add_controller(click);

    // Hover preview: focus workspace on mouse enter (Normal mode, created cards only)
    if !is_disabled && mode == Mode::Normal && ctx.config.hover_preview {
        if let Some(ref ws_name) = info.ws_name {
            let hover_name = ws_name.clone();
            let motion = EventControllerMotion::new();
            motion.connect_enter(move |_, _, _| {
                let _ = niri::focus_workspace_by_name(&hover_name);
            });
            key_box.add_controller(motion);
        }
    }

    key_box
}

fn build_keyboard(
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

// --- UI construction ---

pub fn build_ui(app: &gtk4::Application, config: &Rc<ResolvedConfig>, mode: Mode) {
    let window = ApplicationWindow::builder().application(app).build();
    window.remove_css_class("background");
    window.init_layer_shell();

    let focused_monitor = find_focused_monitor();
    window.set_monitor(focused_monitor.as_ref());

    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);

    // Hover preview state — captured once at overlay open, shared across repopulations.
    let original_workspace = Rc::new(RefCell::new(if config.hover_preview {
        niri::focused_workspace_name()
    } else {
        None
    }));
    let selection_made = Rc::new(Cell::new(false));

    let monitor_width = get_monitor_width(focused_monitor.as_ref());
    populate_overlay(
        &window,
        config,
        mode,
        monitor_width,
        &original_workspace,
        &selection_made,
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
    let tracked_output = Rc::new(RefCell::new(find_focused_output()));
    let track_window = window.clone();
    let track_config = config.clone();
    let track_orig = original_workspace;
    let track_sel = selection_made;
    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
        if !track_window.is_visible() {
            return glib::ControlFlow::Break;
        }
        let current = find_focused_output();
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
fn populate_overlay(
    window: &ApplicationWindow,
    config: &Rc<ResolvedConfig>,
    mode: Mode,
    monitor_width: i32,
    original_workspace: &Rc<RefCell<Option<String>>>,
    selection_made: &Rc<Cell<bool>>,
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

    // Build keyboard
    let infos = Rc::new(build_full_keyboard_info(config));

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
    };

    let keyboard = build_keyboard(&infos, mode, &ctx, &metrics);

    // Assemble: keyboard → hint footer → error revealer → mode tabs (bottom)
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
    let result = niri::switch_workspace(&ctx.config.workspace_prefix, ws_key, ws_name, programs)
        .map(|(created, req)| {
            if let Some(r) = req {
                std::thread::Builder::new()
                    .name("reorder".into())
                    .spawn(move || niri::reorder_workspace_columns(&r))
                    .ok();
            }
            if created {
                let hooks = crate::config::collect_create_hooks(
                    &ctx.config,
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
        Mode::Delete => {
            let result = niri::delete_workspace(prefix, ch);
            if result.is_ok() {
                let env = crate::config::build_hook_env(&ws_name, ch, None, &HashMap::new());
                niri::run_hooks(&ctx.config.hooks.on_delete, &env);
            }
            result
        }
        Mode::MoveWindow => niri::move_window_to_workspace(prefix, ch, &ws_name),
    };

    if let Err(e) = result {
        show_error(ctx, &format!("Failed: {e:#}"));
        return;
    }
    ctx.window.close();
}

// --- Template picker ---

/// An option in the template picker (either "Empty" or a named template).
struct TemplateOption {
    key: Option<char>,
    name: String,
    programs: Vec<String>,
    variables: Vec<TemplateVariable>,
    title: Option<String>,
}

fn build_template_options(config: &ResolvedConfig) -> Vec<TemplateOption> {
    let mut options = Vec::with_capacity(config.templates.len() + 1);

    // "Empty" option always gets key '1' (reserved during config resolution)
    options.push(TemplateOption {
        key: Some('1'),
        name: "Empty".to_string(),
        programs: config.default_programs.clone(),
        variables: Vec::new(),
        title: None,
    });

    for tmpl in &config.templates {
        options.push(TemplateOption {
            key: tmpl.key,
            name: tmpl.name.clone(),
            programs: tmpl.programs.clone(),
            variables: tmpl.variables.clone(),
            title: tmpl.title.clone(),
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

fn select_template_option(option: &TemplateOption, ch: char, ctx: &ActionContext) {
    let template_name = if option.name == "Empty" {
        None
    } else {
        Some(option.name.clone())
    };
    if option.variables.is_empty() {
        let hook_info = HookInfo {
            template_name,
            variables: HashMap::new(),
        };
        // No variables, so title stays as-is (no substitution needed)
        let full_name = crate::config::workspace_name_with_title(
            &ctx.config.workspace_prefix,
            ch,
            option.title.as_deref(),
        );
        switch_and_close(&full_name, ch, &option.programs, ctx, &hook_info);
    } else {
        show_variable_input(option, ch, ctx, template_name);
    }
}

fn show_template_picker(ch: char, ctx: &ActionContext) {
    let window = &ctx.window;
    remove_app_controllers(window);

    let config = &ctx.config;
    let metrics = KeyboardMetrics::from_monitor_width(ctx.monitor_width, config.layout);
    apply_scaled_css(&metrics.scaled_css_variables());

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["popup-container", "template-picker"])
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Title
    let title = Label::builder()
        .label(format!(
            "Create workspace {}",
            format_workspace_display(ch, config)
        ))
        .css_classes(["template-title"])
        .build();
    container.append(&title);

    // Error revealer
    let (error_label, error_revealer) = create_error_revealer();

    // Build template options
    let options = build_template_options(config);
    let option_count = options.len();

    // Build option widgets
    let list_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap / 2)
        .css_classes(["template-list"])
        .build();

    let selected_idx = Rc::new(Cell::new(0_usize));
    let option_widgets: Vec<GtkBox> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| build_template_option_widget(opt, i == 0, &metrics))
        .collect();

    // Shared context for the template picker
    let picker_ctx = ActionContext {
        mode: ctx.mode,
        window: ctx.window.clone(),
        error_label,
        error_revealer: error_revealer.clone(),
        config: ctx.config.clone(),
        keyboard_infos: ctx.keyboard_infos.clone(),
        monitor_width: ctx.monitor_width,
        original_workspace: ctx.original_workspace.clone(),
        selection_made: ctx.selection_made.clone(),
    };

    // Store options as Rc for sharing with handlers
    let options = Rc::new(options);
    let option_widgets_rc = Rc::new(option_widgets);

    for (i, widget) in option_widgets_rc.iter().enumerate() {
        list_box.append(widget);

        // Click handler
        let click_ctx = picker_ctx.clone();
        let click_options = options.clone();
        let click = GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            select_template_option(&click_options[i], ch, &click_ctx);
        });
        widget.add_controller(click);
    }

    // Wrap in scrolled window for many templates
    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .max_content_height(metrics.key_size * 5)
        .propagate_natural_height(true)
        .child(&list_box)
        .build();
    container.append(&scrolled);

    container.append(&error_revealer);

    container.append(&build_hint_footer(
        &metrics,
        &[
            "press key to select",
            "\u{2191}\u{2193} navigate",
            "Enter confirm",
            "Escape cancel",
        ],
    ));

    wrap_in_backdrop(window, &container);

    // Key handler
    attach_template_key_handler(
        &picker_ctx,
        &options,
        &option_widgets_rc,
        &selected_idx,
        option_count,
        ch,
    );
    attach_close_on_backdrop_click(window, &container);
}

fn attach_template_key_handler(
    ctx: &ActionContext,
    options: &Rc<Vec<TemplateOption>>,
    option_widgets: &Rc<Vec<GtkBox>>,
    selected_idx: &Rc<Cell<usize>>,
    option_count: usize,
    ws_char: char,
) {
    let key_ctx = ctx.clone();
    let close_keybinds = ctx.config.close_keybinds.clone();
    let options = options.clone();
    let widgets = option_widgets.clone();
    let sel = selected_idx.clone();

    let key_controller = new_key_controller();
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        // Close keybinds / Escape → go back to main view
        if matches_close_keybind(key, modifier, &close_keybinds) {
            let ctx = key_ctx.clone();
            glib::idle_add_local_once(move || {
                populate_overlay(
                    &ctx.window,
                    &ctx.config,
                    Mode::Normal,
                    ctx.monitor_width,
                    &ctx.original_workspace,
                    &ctx.selection_made,
                );
            });
            return Propagation::Stop;
        }

        // Arrow Up/Down → navigate
        if key == gdk4::Key::Up || key == gdk4::Key::KP_Up {
            let current = sel.get();
            let new_idx = if current == 0 {
                option_count - 1
            } else {
                current - 1
            };
            sel.set(new_idx);
            update_selection(&widgets, new_idx);
            return Propagation::Stop;
        }
        if key == gdk4::Key::Down || key == gdk4::Key::KP_Down {
            let current = sel.get();
            let new_idx = if current >= option_count - 1 {
                0
            } else {
                current + 1
            };
            sel.set(new_idx);
            update_selection(&widgets, new_idx);
            return Propagation::Stop;
        }

        // Enter → confirm selected
        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            let idx = sel.get();
            select_template_option(&options[idx], ws_char, &key_ctx);
            return Propagation::Stop;
        }

        // Shortcut keys — match template options
        if let Some(pressed) = key.to_unicode() {
            let pressed = pressed.to_ascii_lowercase();
            if (modifier & ACTION_MODS).is_empty() {
                for opt in options.iter() {
                    if opt.key == Some(pressed) {
                        select_template_option(opt, ws_char, &key_ctx);
                        return Propagation::Stop;
                    }
                }
            }
        }

        Propagation::Proceed
    });
    ctx.window.add_controller(key_controller);
}

// --- Variable input form ---

/// Filter `options` by fuzzy-matching against `query`, returning indices sorted
/// by match score (best first). An empty query returns all indices in order.
fn fuzzy_filter(query: &str, options: &[String], matcher: &mut Matcher) -> Vec<usize> {
    if query.is_empty() {
        return (0..options.len()).collect();
    }

    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = options
        .iter()
        .enumerate()
        .filter_map(|(i, opt)| {
            let haystack = Utf32Str::new(opt, &mut buf);
            pattern.score(haystack, matcher).map(|s| (i, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[derive(Clone)]
struct FuzzySelect {
    entry: Entry,
    selected: Rc<Cell<usize>>,
    filtered: Rc<RefCell<Vec<usize>>>,
    options: Vec<String>,
}

impl FuzzySelect {
    fn value(&self) -> String {
        let indices = self.filtered.borrow();
        let idx = self.selected.get();
        indices
            .get(idx)
            .and_then(|&i| self.options.get(i))
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone)]
enum VariableWidget {
    Text(Entry),
    Enum(FuzzySelect),
}

impl VariableWidget {
    fn value(&self) -> String {
        match self {
            Self::Text(entry) => entry.text().to_string(),
            Self::Enum(fuzzy) => fuzzy.value(),
        }
    }

    fn grab_focus(&self) {
        match self {
            Self::Text(entry) => {
                entry.grab_focus();
            }
            Self::Enum(fuzzy) => {
                fuzzy.entry.grab_focus();
            }
        }
    }
}

fn update_fuzzy_selection(labels: &[Label], filtered: &[usize], old_idx: usize, new_idx: usize) {
    if let Some(&old) = filtered.get(old_idx) {
        labels[old].remove_css_class("selected");
    }
    if let Some(&new) = filtered.get(new_idx) {
        labels[new].add_css_class("selected");
    }
}

/// Run a shell command and return each non-empty stdout line as a `String`.
///
/// Returns an empty `Vec` if the command fails or produces no output.
fn run_options_command(cmd: &str) -> Vec<String> {
    match std::process::Command::new("sh").args(["-c", cmd]).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        Ok(output) => {
            eprintln!(
                "warning: enum command failed (exit {}): {cmd}",
                output.status.code().unwrap_or(-1)
            );
            Vec::new()
        }
        Err(e) => {
            eprintln!("warning: could not run enum command '{cmd}': {e}");
            Vec::new()
        }
    }
}

/// Expand a leading `~/` in a path to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{rest}", home.display());
        }
    }
    path.to_string()
}

/// Recursively collect child directories up to `remaining` levels deep.
///
/// Skips hidden entries (names starting with `.`). Only directories are
/// included. Results are pushed as absolute paths.
fn collect_children(current: &std::path::Path, remaining: u32, results: &mut Vec<String>) {
    if remaining == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    let mut child_dirs: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.')))
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .map(|e| e.path())
        .collect();
    child_dirs.sort();
    for child in &child_dirs {
        results.push(child.to_string_lossy().into_owned());
        if remaining > 1 {
            collect_children(child, remaining - 1, results);
        }
    }
}

/// Scan directories for child directories up to a given depth.
///
/// Expands `~/` prefixes, skips missing directories, and returns a sorted
/// deduplicated list of absolute directory paths.
fn scan_dir_options(dirs: &[String], depth: u32) -> Vec<String> {
    let mut results = Vec::new();
    for dir in dirs {
        let expanded = expand_tilde(dir);
        let root = std::path::Path::new(&expanded);
        if root.is_dir() {
            collect_children(root, depth, &mut results);
        }
    }
    results.sort();
    results.dedup();
    results
}

/// Resolve the options for a select variable from its source.
fn resolve_select_options(source: &Select) -> Vec<String> {
    match source {
        Select::Options(opts) => opts.clone(),
        Select::Command(cmd) => run_options_command(cmd),
        Select::Dirs { dirs, depth } => scan_dir_options(dirs, *depth),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "fuzzy select widget with filter and key handling setup"
)]
fn build_fuzzy_select(
    row: &GtkBox,
    options: &[String],
    metrics: &KeyboardMetrics,
) -> VariableWidget {
    let search_entry = Entry::builder()
        .css_classes(["variable-entry"])
        .placeholder_text("Type to filter\u{2026}")
        .build();
    row.append(&search_entry);

    let list_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["fuzzy-list"])
        .build();

    let option_labels: Vec<Label> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let mut classes = vec!["fuzzy-option"];
            if i == 0 {
                classes.push("selected");
            }
            let label = Label::builder()
                .label(opt)
                .css_classes(classes)
                .halign(Align::Start)
                .build();
            list_box.append(&label);
            label
        })
        .collect();

    let all_indices: Vec<usize> = (0..options.len()).collect();
    let filtered = Rc::new(RefCell::new(all_indices));
    let selected = Rc::new(Cell::new(0_usize));
    let labels_rc = Rc::new(option_labels);
    let matcher = Rc::new(RefCell::new(Matcher::new(MatcherConfig::DEFAULT)));

    // Filter on text change
    {
        let opts: Vec<String> = options.to_vec();
        let filt = filtered.clone();
        let sel = selected.clone();
        let labels = labels_rc.clone();
        let matcher = matcher.clone();
        let list = list_box.clone();
        search_entry.connect_changed(move |entry| {
            let query = entry.text();

            // Remove old selection highlight
            {
                let old_filt = filt.borrow();
                if let Some(&old_real) = old_filt.get(sel.get()) {
                    labels[old_real].remove_css_class("selected");
                }
            }

            let new_indices = fuzzy_filter(&query, &opts, &mut matcher.borrow_mut());

            let visible: HashSet<usize> = new_indices.iter().copied().collect();
            for (i, label) in labels.iter().enumerate() {
                label.set_visible(visible.contains(&i));
            }

            // Reorder labels in the list to match score order (best first)
            for (pos, &i) in new_indices.iter().enumerate() {
                if pos == 0 {
                    labels[i].insert_after(&list, None::<&Label>);
                } else {
                    labels[i].insert_after(&list, Some(&labels[new_indices[pos - 1]]));
                }
            }

            sel.set(0);
            if let Some(&first_idx) = new_indices.first() {
                labels[first_idx].add_css_class("selected");
            }

            *filt.borrow_mut() = new_indices;
        });
    }

    // Handle Up/Down keys on the search entry
    {
        let filt = filtered.clone();
        let sel = selected.clone();
        let labels = labels_rc.clone();
        let key_ctrl = EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            let indices = filt.borrow();
            if indices.is_empty() {
                return Propagation::Proceed;
            }

            let current = sel.get();
            let new_idx = if key == gdk4::Key::Up || key == gdk4::Key::KP_Up {
                if current == 0 {
                    indices.len() - 1
                } else {
                    current - 1
                }
            } else if key == gdk4::Key::Down || key == gdk4::Key::KP_Down {
                if current >= indices.len() - 1 {
                    0
                } else {
                    current + 1
                }
            } else {
                return Propagation::Proceed;
            };

            update_fuzzy_selection(&labels, &indices, current, new_idx);
            sel.set(new_idx);

            Propagation::Stop
        });
        search_entry.add_controller(key_ctrl);
    }

    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .max_content_height(metrics.key_size * 3)
        .propagate_natural_height(true)
        .child(&list_box)
        .build();
    row.append(&scrolled);

    VariableWidget::Enum(FuzzySelect {
        entry: search_entry,
        selected,
        filtered,
        options: options.to_vec(),
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "variable input form with widget construction and layout"
)]
fn show_variable_input(
    option: &TemplateOption,
    ch: char,
    ctx: &ActionContext,
    template_name: Option<String>,
) {
    let window = &ctx.window;
    remove_app_controllers(window);

    let config = &ctx.config;
    let metrics = KeyboardMetrics::from_monitor_width(ctx.monitor_width, config.layout);
    apply_scaled_css(&metrics.scaled_css_variables());

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["popup-container", "template-picker"])
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Title — match template picker style: "Template → KEY (Name)"
    let title = Label::builder()
        .label(format!(
            "{} \u{2192} {}",
            option.name,
            format_workspace_display(ch, config)
        ))
        .css_classes(["variable-title"])
        .build();
    container.append(&title);

    // Error revealer
    let (error_label, error_revealer) = create_error_revealer();

    // Variable form
    let form = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(metrics.key_gap / 2)
        .css_classes(["variable-form"])
        .build();

    let widgets: Vec<VariableWidget> = option
        .variables
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

            let widget = match &var.var_type {
                VariableType::Text => {
                    let entry = Entry::builder()
                        .css_classes(["variable-entry"])
                        .placeholder_text(&var.name)
                        .build();
                    row.append(&entry);
                    VariableWidget::Text(entry)
                }
                VariableType::Select(source) => {
                    let resolved = resolve_select_options(source);
                    if resolved.is_empty() {
                        let entry = Entry::builder()
                            .css_classes(["variable-entry"])
                            .placeholder_text(&var.name)
                            .build();
                        row.append(&entry);
                        VariableWidget::Text(entry)
                    } else {
                        build_fuzzy_select(&row, &resolved, &metrics)
                    }
                }
            };

            form.append(&row);
            widget
        })
        .collect();
    container.append(&form);

    container.append(&error_revealer);

    container.append(&build_hint_footer(
        &metrics,
        &["Enter create", "\u{2191}\u{2193} navigate", "Escape back"],
    ));

    wrap_in_backdrop(window, &container);

    // Focus the first widget
    if let Some(first) = widgets.first() {
        first.grab_focus();
    }

    let var_ctx = ActionContext {
        mode: ctx.mode,
        window: ctx.window.clone(),
        error_label,
        error_revealer,
        config: ctx.config.clone(),
        keyboard_infos: ctx.keyboard_infos.clone(),
        monitor_width: ctx.monitor_width,
        original_workspace: ctx.original_workspace.clone(),
        selection_made: ctx.selection_made.clone(),
    };

    let var_names: Vec<String> = option.variables.iter().map(|v| v.name.clone()).collect();
    let programs = option.programs.clone();
    let template_title = option.title.clone();
    let template_variables = option.variables.clone();

    attach_variable_input_key_handler(
        &var_ctx,
        ch,
        &widgets,
        &var_names,
        &programs,
        template_name,
        template_title,
        template_variables,
    );
    attach_close_on_backdrop_click(window, &container);
}

#[expect(
    clippy::too_many_arguments,
    reason = "template context requires passing title and variables for workspace naming"
)]
fn attach_variable_input_key_handler(
    ctx: &ActionContext,
    ch: char,
    widgets: &[VariableWidget],
    var_names: &[String],
    programs: &[String],
    template_name: Option<String>,
    template_title: Option<String>,
    template_variables: Vec<TemplateVariable>,
) {
    let key_ctx = ctx.clone();
    let close_keybinds = ctx.config.close_keybinds.clone();
    let prefix = ctx.config.workspace_prefix.clone();
    let widgets: Vec<VariableWidget> = widgets.to_vec();
    let var_names: Vec<String> = var_names.to_vec();
    let programs: Vec<String> = programs.to_vec();

    let key_controller = new_key_controller();
    key_controller.connect_key_pressed(move |_, key, _, modifier| {
        // Close keybinds / Escape → go back to template picker
        if matches_close_keybind(key, modifier, &close_keybinds) {
            let ctx_clone = key_ctx.clone();
            glib::idle_add_local_once(move || {
                show_template_picker(ch, &ctx_clone);
            });
            return Propagation::Stop;
        }

        // Enter → collect values and create workspace
        if key == gdk4::Key::Return || key == gdk4::Key::KP_Enter {
            let mut values = HashMap::new();
            for (name, widget) in var_names.iter().zip(widgets.iter()) {
                values.insert(name.clone(), widget.value());
            }
            let substituted = crate::config::substitute_variables(&programs, &values);
            let title = crate::config::resolve_workspace_title(
                template_title.as_deref(),
                &template_variables,
                &values,
            );
            let full_name = crate::config::workspace_name_with_title(&prefix, ch, title.as_deref());
            let hook_info = HookInfo {
                template_name: template_name.clone(),
                variables: values,
            };
            switch_and_close(&full_name, ch, &substituted, &key_ctx, &hook_info);
            return Propagation::Stop;
        }

        // Let GTK handle Tab, text input, etc.
        Propagation::Proceed
    });
    ctx.window.add_controller(key_controller);
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- fuzzy_filter ---

    fn test_matcher() -> Matcher {
        Matcher::new(MatcherConfig::DEFAULT)
    }

    #[test]
    fn fuzzy_filter_empty_query() {
        let opts = vec!["a".into(), "b".into(), "c".into()];
        let result = fuzzy_filter("", &opts, &mut test_matcher());
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn fuzzy_filter_exact_match_first() {
        let opts: Vec<String> = vec!["something_main".into(), "main".into(), "xmyaziznw".into()];
        let result = fuzzy_filter("main", &opts, &mut test_matcher());
        // "main" (exact/short) should score highest over substring matches
        assert_eq!(result[0], 1);
        assert!(result.len() >= 2);
    }

    #[test]
    fn fuzzy_filter_no_match() {
        let opts: Vec<String> = vec!["main".into(), "develop".into()];
        let result = fuzzy_filter("xyz", &opts, &mut test_matcher());
        assert!(result.is_empty());
    }

    #[test]
    fn fuzzy_filter_ranks_by_relevance() {
        let opts: Vec<String> = vec!["administrator".into(), "dev".into(), "develop".into()];
        let result = fuzzy_filter("dev", &opts, &mut test_matcher());
        // "dev" (exact) should rank above "develop" (prefix)
        assert_eq!(result[0], 1);
        assert!(result.contains(&2));
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
            hover_preview: true,
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
                title: None,
            },
            Template {
                name: "browser".to_string(),
                programs: vec!["firefox".to_string()],
                key: Some('2'),
                variables: Vec::new(),
                on_create: Vec::new(),
                title: None,
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
            title: None,
        }];

        let opts = build_template_options(&config);

        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].name, "Empty");
        assert!(opts[0].programs.is_empty());
    }

    // --- run_options_command ---

    #[test]
    fn run_options_command_basic() {
        let result = run_options_command("printf 'a\nb\nc'");
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn run_options_command_trims_and_filters() {
        let result = run_options_command("printf '  a \n\n  b  \n\n'");
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn run_options_command_failure() {
        let result = run_options_command("nonexistent_command_12345");
        assert!(result.is_empty());
    }

    // --- resolve_select_options ---

    #[test]
    fn resolve_select_options_static() {
        let source = Select::Options(vec!["a".to_string(), "b".to_string()]);
        let result = resolve_select_options(&source);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn resolve_select_options_command_succeeds() {
        let source = Select::Command("printf 'x\ny'".to_string());
        let result = resolve_select_options(&source);
        assert_eq!(result, vec!["x", "y"]);
    }

    #[test]
    fn resolve_select_options_command_fails() {
        let source = Select::Command("nonexistent_cmd_12345".to_string());
        let result = resolve_select_options(&source);
        assert!(result.is_empty());
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

    // --- scan_dir_options ---

    /// RAII wrapper for a temporary directory that cleans up on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path_str(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
        fn mkdir(&self, sub: &str) {
            std::fs::create_dir_all(self.0.join(sub)).unwrap();
        }
        fn touch(&self, sub: &str) {
            std::fs::write(self.0.join(sub), "").unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_dir_options_missing_dir() {
        let result = scan_dir_options(&["/nonexistent_dir_12345".to_string()], 1);
        assert!(result.is_empty());
    }

    #[test]
    fn scan_dir_options_basic() {
        let tmp = TempDir::new("ndw_test_scan_basic");
        tmp.mkdir("alpha");
        tmp.mkdir("beta");
        tmp.mkdir(".hidden");
        tmp.touch("file.txt");

        let result = scan_dir_options(&[tmp.path_str()], 1);
        let base = tmp.path_str();
        assert_eq!(
            result,
            vec![format!("{base}/alpha"), format!("{base}/beta")]
        );
    }

    #[test]
    fn scan_dir_options_depth_2() {
        let tmp = TempDir::new("ndw_test_scan_depth2");
        tmp.mkdir("a/child");
        tmp.mkdir("b");

        let result = scan_dir_options(&[tmp.path_str()], 2);
        let base = tmp.path_str();
        assert_eq!(
            result,
            vec![
                format!("{base}/a"),
                format!("{base}/a/child"),
                format!("{base}/b"),
            ]
        );
    }

    #[test]
    fn scan_dir_options_multiple_dirs() {
        let tmp1 = TempDir::new("ndw_test_multi1");
        let tmp2 = TempDir::new("ndw_test_multi2");
        tmp1.mkdir("shared");
        tmp1.mkdir("only1");
        tmp2.mkdir("shared");
        tmp2.mkdir("only2");

        let result = scan_dir_options(&[tmp1.path_str(), tmp2.path_str()], 1);
        let b1 = tmp1.path_str();
        let b2 = tmp2.path_str();
        assert_eq!(
            result,
            vec![
                format!("{b1}/only1"),
                format!("{b1}/shared"),
                format!("{b2}/only2"),
                format!("{b2}/shared"),
            ]
        );
    }
}
