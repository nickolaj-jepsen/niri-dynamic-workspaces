mod cards;
mod keys;
mod metrics;
mod picker;
mod theme;
mod variables;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context as _;
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

use cards::{build_keyboard, build_static_workspace_row, DynWorkspaceInfo, GridModel};
use metrics::{apply_scaled_css, find_monitor_for_output, KeyboardMetrics};
use picker::show_template_picker;
pub use theme::install_base as install_base_styles;

/// Extract the output name of the focused workspace from a pre-fetched list.
fn focused_output_from(workspaces: &[niri_ipc::Workspace]) -> Option<String> {
    workspaces.iter().find(|w| w.is_focused)?.output.clone()
}

/// Id of the focused workspace in a pre-fetched list.
fn focused_workspace_id_from(workspaces: &[niri_ipc::Workspace]) -> Option<u64> {
    workspaces.iter().find(|w| w.is_focused).map(|w| w.id)
}

/// Active window of the focused workspace in a pre-fetched list.
///
/// Layout-based: `Window::is_focused` is false while the overlay has keyboard focus.
fn focused_window_from(workspaces: &[niri_ipc::Workspace]) -> Option<u64> {
    workspaces.iter().find(|w| w.is_focused)?.active_window_id
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

    /// The footer's first hint: what a key press does in this mode.
    const fn hint(self) -> &'static str {
        match self {
            Self::Normal => "press key to select",
            Self::Delete => "press key to delete (closes its windows)",
            Self::MoveWindow => "press key to move the focused window",
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
}

// --- Data types ---

/// What hover preview has focused, so closing undoes only what it changed.
///
/// Re-focusing the active workspace is never sent: with niri's
/// workspace-auto-back-and-forth it jumps to the previous workspace instead.
struct HoverPreview {
    /// Focused workspace when the overlay opened, or after the last output change.
    origin: Cell<Option<u64>>,
    /// Workspace a preview focused instead of `origin`.
    previewed: Cell<Option<u64>>,
}

impl HoverPreview {
    const fn new(origin: Option<u64>) -> Self {
        Self {
            origin: Cell::new(origin),
            previewed: Cell::new(None),
        }
    }

    /// Whether previewing `ws_id` changes focus: not when it already has it.
    fn should_focus(&self, ws_id: u64) -> bool {
        self.previewed.get().or(self.origin.get()) != Some(ws_id)
    }

    /// Record that a preview focused `ws_id`.
    fn focused(&self, ws_id: u64) {
        self.previewed
            .set((self.origin.get() != Some(ws_id)).then_some(ws_id));
    }

    fn is_previewed(&self, ws_id: u64) -> bool {
        self.previewed.get() == Some(ws_id)
    }

    /// The workspace to restore, once; `None` unless a preview moved focus.
    fn take_restore(&self) -> Option<u64> {
        self.previewed.take().and(self.origin.get())
    }

    /// Take `origin` as the focused workspace, dropping any pending restore.
    fn rebase(&self, origin: Option<u64>) {
        self.origin.set(origin);
        self.previewed.set(None);
    }
}

/// State shared across one overlay lifetime: created in [`build_ui`] and
/// threaded through every repopulation and sub-view.
struct OverlaySession {
    config: Rc<ResolvedConfig>,
    /// Sizes for the monitor the overlay currently occupies.
    metrics: Cell<KeyboardMetrics>,
    /// What the grid view last rendered.
    grid: RefCell<Option<GridModel>>,
    preview: HoverPreview,
    /// The window Move Window moves: focused at open, or after the last output change.
    origin_window: Cell<Option<u64>>,
    /// Set once an action succeeded (skip the hover-preview restore on close).
    selection_made: Cell<bool>,
    /// Armed after the first real mouse movement; prevents hover-preview from
    /// triggering when the cursor is already over a card at overlay open.
    hover_armed: Cell<bool>,
    /// True while a sub-view (template picker / variable form) is showing;
    /// suppresses structural refreshes that would destroy it.
    in_subview: Cell<bool>,
    /// Keycode of the last selecting press, whose auto-repeat is swallowed
    /// until release so it cannot act again in the view it opened.
    held_key: keys::HeldKey,
    /// Key whose delete waits for a second press (Delete mode only).
    delete_armed: Cell<Option<char>>,
    /// Config and theme problems, shown on a line under the hints.
    problems: Vec<String>,
}

#[derive(Clone)]
struct ActionContext {
    mode: Mode,
    window: ApplicationWindow,
    error_label: Label,
    error_revealer: Revealer,
    session: Rc<OverlaySession>,
    keyboard_infos: Rc<HashMap<char, DynWorkspaceInfo>>,
    /// Output name where the overlay is displayed (for hover-preview gating).
    focused_output: Option<String>,
}

// --- UI construction ---

pub fn build_ui(app: &gtk4::Application, config: &Rc<ResolvedConfig>, mode: Mode) {
    let window = ApplicationWindow::builder().application(app).build();
    window.remove_css_class("background");
    window.init_layer_shell();
    let theme_warnings = theme::apply(&config.theme);

    // Single IPC fetch — derive focused output, monitor, and workspace id from it.
    let workspaces = niri::list_workspaces();
    let listed = workspaces.as_deref().unwrap_or_default();
    let focused_output = focused_output_from(listed);
    let focused_monitor = focused_output.as_deref().and_then(find_monitor_for_output);
    window.set_monitor(focused_monitor.as_ref());

    configure_layer_shell(&window);

    let session = Rc::new(OverlaySession {
        config: config.clone(),
        metrics: Cell::new(KeyboardMetrics::for_monitor(
            focused_monitor.as_ref(),
            config.layout,
        )),
        grid: RefCell::new(None),
        preview: HoverPreview::new(focused_workspace_id_from(listed)),
        origin_window: Cell::new(focused_window_from(listed)),
        selection_made: Cell::new(false),
        hover_armed: Cell::new(false),
        in_subview: Cell::new(false),
        held_key: keys::HeldKey::default(),
        delete_armed: Cell::new(None),
        problems: config
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .chain(theme_warnings)
            .collect(),
    });

    // Arm hover-preview after the first real mouse movement so that a cursor
    // already resting over a card when the overlay appears does not trigger a
    // workspace switch.
    {
        let arm_session = session.clone();
        let motion = EventControllerMotion::new();
        // No "ndw-" name: remove_app_controllers strips those on every
        // repopulate, and this controller must outlive them all.
        motion.set_name(Some("hover-arm"));
        motion.connect_motion(move |_, _, _| {
            arm_session.hover_armed.set(true);
        });
        window.add_controller(motion);
    }

    // On a failed listing populate_overlay fetches again and shows why.
    let grid = workspaces
        .ok()
        .and_then(|workspaces| fetch_grid(config, Some(workspaces)).ok());
    populate_overlay(&window, &session, mode, grid);
    free_on_close(&window);
    connect_session_close(&window, &session);
    follow_compositor(&window, session, focused_output);

    if config.inhibit_compositor_shortcuts {
        inhibit_compositor_shortcuts(&window);
    }

    window.present();
}

/// Follow compositor state while the overlay is open: a background thread
/// forwards niri events; focus events move the overlay between outputs,
/// structural events refresh the cards.
fn follow_compositor(
    window: &ApplicationWindow,
    session: Rc<OverlaySession>,
    mut tracked_output: Option<String>,
) {
    let (event_tx, event_rx) = async_channel::unbounded();
    let stream_alive = Arc::new(AtomicBool::new(true));
    {
        let alive = stream_alive.clone();
        std::thread::Builder::new()
            .name("overlay-events".into())
            .spawn(move || {
                niri::run_overlay_event_stream(&alive, |event| {
                    let _ = event_tx.send_blocking(event);
                });
            })
            .ok();
    }
    window.connect_close_request(move |_| {
        stream_alive.store(false, Ordering::Relaxed);
        Propagation::Proceed
    });

    // Weak: the future must not keep a closed window alive.
    let weak_window = window.downgrade();
    glib::spawn_future_local(async move {
        while let Ok(event) = event_rx.recv().await {
            let mut structural = event == niri::OverlayEvent::Structural;
            if structural {
                // Debounce: coalesce event bursts into one refresh.
                glib::timeout_future(std::time::Duration::from_millis(100)).await;
            }
            while let Ok(more) = event_rx.try_recv() {
                structural |= more == niri::OverlayEvent::Structural;
            }
            // Checked after the await: repopulating a closed window rebuilds
            // what free_on_close tore down.
            let Some(window) = weak_window.upgrade().filter(WidgetExt::is_visible) else {
                break;
            };

            // A failed listing keeps the current cards and output.
            let Ok(fresh_workspaces) = niri::list_workspaces() else {
                continue;
            };
            let current = focused_output_from(&fresh_workspaces);
            let mode = Mode::from_window(window.upcast_ref()).unwrap_or(Mode::Normal);

            // Focused output changed → move the overlay to that monitor.
            if current != tracked_output {
                tracked_output.clone_from(&current);
                // Previews stay on the overlay's output, so no preview moved focus here.
                let origin = focused_workspace_id_from(&fresh_workspaces);
                session.preview.rebase(origin);
                niri::set_overlay_origin(origin);
                // The grid now shows that output, so Move Window acts on its window.
                session
                    .origin_window
                    .set(focused_window_from(&fresh_workspaces));
                if let Some(monitor) = current.as_deref().and_then(find_monitor_for_output) {
                    window.set_monitor(Some(&monitor));
                    session.metrics.set(KeyboardMetrics::for_monitor(
                        Some(&monitor),
                        session.config.layout,
                    ));
                    // A rebuild would drop the picker or the typed variable
                    // values; the next view built picks up the new monitor.
                    if !session.in_subview.get() {
                        // Rebuilt even when no card changed: the metrics did.
                        if let Ok(grid) = fetch_grid(&session.config, Some(fresh_workspaces)) {
                            populate_overlay(&window, &session, mode, Some(grid));
                        }
                    }
                    continue;
                }
            }

            // Workspaces/windows changed → refresh the cards, but never while
            // a sub-view (picker or variable form) is up. niri reports every
            // window title change, which no card shows.
            if structural && !session.in_subview.get() {
                if let Ok(grid) = fetch_grid(&session.config, Some(fresh_workspaces)) {
                    let changed = session.grid.borrow().as_ref() != Some(&grid);
                    if changed {
                        populate_overlay(&window, &session, mode, Some(grid));
                    }
                }
            }
        }
    });
}

/// Undo a hover preview, if one moved focus.
fn end_preview(session: &OverlaySession) {
    if let Some(id) = session.preview.take_restore() {
        if let Err(e) = niri::focus_workspace_by_id(id) {
            eprintln!("warning: failed to restore workspace {id}: {e:#}");
        }
    }
}

/// Spare the origin from cleanup while the window is open, and undo the
/// hover preview when it closes without a selection.
fn connect_session_close(window: &ApplicationWindow, session: &Rc<OverlaySession>) {
    niri::set_overlay_origin(session.preview.origin.get());
    let session = session.clone();
    window.connect_close_request(move |_| {
        if !session.selection_made.get() {
            end_preview(&session);
        }
        // Here rather than on drop: a leaked session would keep the spare.
        niri::set_overlay_origin(None);
        Propagation::Proceed
    });
}

/// Drop the widget tree and `ndw-*` controllers once the window closes.
///
/// Those closures hold [`ActionContext`], whose strong window reference would
/// otherwise keep every closed overlay alive in the daemon. Runs on idle
/// because close usually fires from inside the `ndw-key` handler.
fn free_on_close(window: &ApplicationWindow) {
    // e2e counts these lines to catch the leak coming back.
    #[cfg(debug_assertions)]
    window.add_weak_ref_notify_local(|| eprintln!("debug: overlay window freed"));
    window.connect_close_request(|window| {
        let window = window.clone();
        glib::idle_add_local_once(move || {
            remove_app_controllers(&window);
            window.set_child(None::<&gtk4::Widget>);
        });
        Propagation::Proceed
    });
}

/// Anchor the window to all edges as an exclusive-keyboard overlay layer surface.
fn configure_layer_shell(window: &ApplicationWindow) {
    // Users write niri layer rules against it (README), so renaming it is breaking.
    window.set_namespace(Some("niri-dynamic-workspaces"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_exclusive_zone(-1);
}

/// Ask the compositor to stop processing its own keybinds while the overlay
/// has keyboard focus (keyboard-shortcuts-inhibit protocol), so a still-held
/// Mod+<key> reaches the overlay instead of firing niri binds. Niri binds
/// marked allow-inhibiting=false still fire.
fn inhibit_compositor_shortcuts(window: &ApplicationWindow) {
    window.connect_map(|window| {
        if let Some(toplevel) = window
            .surface()
            .and_then(|s| s.downcast::<gdk4::Toplevel>().ok())
        {
            toplevel.inhibit_system_shortcuts(gdk4::Event::NONE);
        }
    });
    // GTK never destroys the inhibitor itself, and niri keeps the dead surface (and its
    // fullscreen buffer) alive for as long as the inhibitor exists. Must run before the
    // window hides: restore_system_shortcuts() is a no-op once the toplevel is torn down.
    window.connect_close_request(|window| {
        if let Some(toplevel) = window
            .surface()
            .and_then(|s| s.downcast::<gdk4::Toplevel>().ok())
        {
            toplevel.restore_system_shortcuts();
        }
        Propagation::Proceed
    });
}

/// Remove controllers we previously attached (identified by "ndw-" name prefix).
///
/// Any window controller that captures an [`ActionContext`] must carry that
/// prefix, or [`free_on_close`] cannot break its cycle and the window leaks.
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
                populate_overlay(&ctx.window, &ctx.session, m, None);
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

/// Whether `event` triggers one of `keybinds`, matched like a GTK shortcut.
///
/// Caps Lock and a Shift the keysym consumed are ignored; Ctrl, Alt and
/// Super must match. A Latin bind also fires from its physical key while a
/// group without that keysym (Cyrillic) is active.
fn matches_close_keybind(event: &gdk4::KeyEvent, keybinds: &[crate::config::Keybind]) -> bool {
    keybinds
        .iter()
        .any(|kb| event.matches(kb.key, kb.modifiers) != gdk4::KeyMatch::None)
}

/// A view's key controller. Handlers call `session.held_key.hold(keycode)`
/// before acting on a key whose repeat must not act again.
fn new_key_controller(session: &Rc<OverlaySession>) -> EventControllerKey {
    let ctrl = EventControllerKey::new();
    ctrl.set_name(Some("ndw-key"));
    ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
    // Connected first: its Stop ends the emission before the view's handler.
    let held = session.clone();
    ctrl.connect_key_pressed(move |_, _, keycode, _| {
        Propagation::from(held.held_key.is_repeat(keycode))
    });
    let held = session.clone();
    ctrl.connect_key_released(move |_, _, keycode, _| held.held_key.release(keycode));
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
        .css_classes(["hints"])
        .halign(Align::Center)
        .build();
    for text in hints {
        let label = Label::builder().label(*text).css_classes(["hint"]).build();
        footer.append(&label);
    }
    footer
}

/// One line for the overlay: the first problem, and how many more there are.
///
/// The first `from_config` problems are config diagnostics, which `check`
/// also lists; the line points there only when some of those are hidden.
fn diagnostics_summary(problems: &[String], from_config: usize) -> Option<String> {
    let (first, rest) = problems.split_first()?;
    Some(match rest.len() {
        0 => first.clone(),
        more if from_config > 1 => {
            format!("{first} (+{more} more, run `niri-dynamic-workspaces check`)")
        }
        more => format!("{first} (+{more} more)"),
    })
}

/// The config-problems line; its tooltip lists every problem.
fn build_problems_line(problems: &[String], from_config: usize) -> Option<Label> {
    let summary = diagnostics_summary(problems, from_config)?;
    Some(
        Label::builder()
            .label(summary)
            .css_classes(["error-message", "config-message"])
            .wrap(true)
            .justify(gtk4::Justification::Center)
            // A wrapping label asks for its one-line width; a long path would widen the view.
            .max_width_chars(100)
            .tooltip_text(problems.join("\n"))
            .build(),
    )
}

/// Whether a Delete-mode press on `ch` only arms it: with confirmation on,
/// a workspace with windows needs a second press of the same key.
fn needs_confirmation(enabled: bool, ch: char, window_count: usize, armed: Option<char>) -> bool {
    enabled && window_count > 0 && armed != Some(ch)
}

/// The footer's first hint while the delete of `ch` is armed.
fn confirm_delete_hint(ch: char, window_count: usize) -> String {
    let windows = if window_count == 1 {
        "window"
    } else {
        "windows"
    };
    format!(
        "press {} again to close {window_count} {windows}",
        display_key_char(ch)
    )
}

/// The grid for niri's current state; `workspaces` saves listing them again.
fn fetch_grid(
    config: &ResolvedConfig,
    workspaces: Option<Vec<niri_ipc::Workspace>>,
) -> anyhow::Result<GridModel> {
    let workspaces = match workspaces {
        Some(workspaces) => workspaces,
        None => niri::list_workspaces()?,
    };
    let windows = niri::list_windows()?;
    Ok(GridModel::new(&workspaces, &windows, config))
}

/// Build (or rebuild) the overlay content for `mode` inside an existing window.
///
/// Renders `grid`, or fetches one when it is `None`; a failed fetch renders
/// an empty grid with the error.
fn populate_overlay(
    window: &ApplicationWindow,
    session: &Rc<OverlaySession>,
    mode: Mode,
    grid: Option<GridModel>,
) {
    // The other modes act on the origin: undo the preview before reading focus.
    if mode != Mode::Normal {
        end_preview(session);
    }
    let config = &session.config;
    session.in_subview.set(false);
    window.set_widget_name(mode.widget_name());
    // Same strings as CSS classes, so themes can style per mode (`window.mode-delete`).
    for m in Mode::all() {
        window.remove_css_class(m.widget_name());
    }
    window.add_css_class(mode.widget_name());
    remove_app_controllers(window);

    let container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .css_classes(["content"])
        .halign(Align::Center)
        .valign(Align::Center)
        .build();

    // Error label + revealer (built first so ActionContext is available for keys)
    let (error_label, error_revealer) = create_error_revealer();

    let metrics = session.metrics.get();
    apply_scaled_css(&metrics.scaled_css_variables());

    let (grid, fetch_error) = match grid.map_or_else(|| fetch_grid(config, None), Ok) {
        Ok(grid) => (grid, None),
        Err(e) => (GridModel::new(&[], &[], config), Some(e)),
    };

    // An armed delete survives refreshes while its target still has windows.
    let confirming = session
        .delete_armed
        .get()
        .filter(|_| mode == Mode::Delete)
        .and_then(|ch| grid.keyboard.get(&ch))
        .filter(|i| i.window_count > 0)
        .map(|i| (i.char_id, i.window_count));
    session.delete_armed.set(confirming.map(|(ch, _)| ch));

    let ctx = ActionContext {
        mode,
        window: window.clone(),
        error_label,
        error_revealer: error_revealer.clone(),
        session: session.clone(),
        keyboard_infos: grid.keyboard.clone(),
        focused_output: grid.focused_output.clone(),
    };

    // Assemble: static row → keyboard → hint footer → problems → error revealer → mode tabs
    if !grid.static_row.is_empty() {
        container.append(&build_static_workspace_row(
            &grid.static_row,
            mode,
            &ctx,
            &metrics,
        ));
    }

    let keyboard = build_keyboard(&grid.keyboard, mode, &ctx, &metrics);
    container.append(&keyboard);
    let first_hint = confirming.map_or_else(
        || mode.hint().to_owned(),
        |(ch, n)| confirm_delete_hint(ch, n),
    );
    let footer = build_hint_footer(
        &metrics,
        &[first_hint.as_str(), "Tab switch mode", "Escape close"],
    );
    if let Some(hint) = footer.first_child().filter(|_| confirming.is_some()) {
        hint.add_css_class("confirm");
    }
    container.append(&footer);
    if let Some(line) = build_problems_line(&session.problems, config.diagnostics.len()) {
        container.append(&line);
    }
    container.append(&error_revealer);
    container.append(&build_mode_tabs(&ctx, mode));

    wrap_in_backdrop(window, &container);

    attach_key_handler(&ctx, &config.close_keybinds);
    attach_close_on_backdrop_click(window, &container);
    if let Some(e) = fetch_error {
        show_error(&ctx, &format!("Failed: {e:#}"));
    }
    *session.grid.borrow_mut() = Some(grid);
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
    let result = crate::actions::switch_workspace(
        &app,
        &ctx.session.config,
        ws_key,
        ws_name,
        programs,
        hook_info,
    );
    if let Err(e) = result {
        show_error(ctx, &format!("Failed: {e:#}"));
        return;
    }
    finish(ctx);
}

/// Focus a selected workspace, unless a preview already did: re-focusing the
/// active workspace would trigger niri's workspace-auto-back-and-forth.
fn focus_selected(ctx: &ActionContext, id: u64) -> anyhow::Result<()> {
    if ctx.session.preview.is_previewed(id) {
        return Ok(());
    }
    niri::focus_workspace_by_id(id)
}

/// The window Move Window moves: the captured one, so later focus changes do not redirect it.
fn window_to_move(ctx: &ActionContext) -> anyhow::Result<u64> {
    ctx.session
        .origin_window
        .get()
        .context("no focused window to move")
}

/// Close the overlay after a successful action, keeping the new focus.
fn finish(ctx: &ActionContext) {
    ctx.session.selection_made.set(true);
    ctx.window.close();
}

fn dispatch_action(ch: char, ctx: &ActionContext) {
    let config = &ctx.session.config;

    // Statically mapped key: act on the pinned workspace directly.
    if let Some(target) = config.static_workspaces.get(&ch) {
        // The listing that built the cards: niri ignores an action on a missing workspace.
        let ws_id = ctx.keyboard_infos.get(&ch).and_then(|i| i.ws_id);
        let result = match (ctx.mode, ws_id) {
            (Mode::Delete, _) => {
                show_error(
                    ctx,
                    &format!("'{target}' is a static workspace and cannot be deleted"),
                );
                return;
            }
            (_, None) => {
                show_error(ctx, &format!("Failed: workspace '{target}' not found"));
                return;
            }
            (Mode::Normal, Some(id)) => focus_selected(ctx, id),
            (Mode::MoveWindow, Some(id)) => window_to_move(ctx)
                .and_then(|window| niri::move_window_to_workspace_by_id(id, Some(window))),
        };
        if let Err(e) = result {
            show_error(ctx, &format!("Failed: {e:#}"));
            return;
        }
        finish(ctx);
        return;
    }

    let info = ctx.keyboard_infos.get(&ch);
    let ws_name = info
        .and_then(|i| i.ws_name.clone())
        .unwrap_or_else(|| crate::config::workspace_name(&config.workspace_prefix, ch));

    let result = match ctx.mode {
        Mode::Normal => {
            // A preview focused it already. Only existing workspaces are
            // previewed, so no programs or hooks are skipped.
            if info
                .and_then(|i| i.ws_id)
                .is_some_and(|id| ctx.session.preview.is_previewed(id))
            {
                finish(ctx);
                return;
            }
            let is_uncreated = info.is_none_or(|i| i.is_uncreated);
            if is_uncreated && config.should_show_templates(ch) {
                show_template_picker(ch, ctx);
                return;
            }
            let programs = config.programs_for(ch);
            switch_and_close(&ws_name, ch, programs, ctx, &HookInfo::default());
            return;
        }
        Mode::Delete => {
            let window_count = info.map_or(0, |i| i.window_count);
            let armed = ctx.session.delete_armed.get();
            if needs_confirmation(config.confirm_delete, ch, window_count, armed) {
                ctx.session.delete_armed.set(Some(ch));
                // Deferred like Tab: the rebuild removes the running controller or clicked card.
                let ctx = ctx.clone();
                glib::idle_add_local_once(move || {
                    populate_overlay(&ctx.window, &ctx.session, Mode::Delete, None);
                });
                return;
            }
            let Some(app) = ctx.window.application() else {
                show_error(ctx, "Failed: window has no application");
                return;
            };
            // The overlay closes first: it would cover the apps' own save prompts.
            crate::actions::delete_workspace(&app, config, ch, |result| {
                if let Err(e) = result {
                    eprintln!("warning: {e:#}");
                }
            })
        }
        Mode::MoveWindow => window_to_move(ctx)
            .and_then(|window| crate::actions::move_window(config, ch, &ws_name, Some(window))),
    };

    if let Err(e) = result {
        show_error(ctx, &format!("Failed: {e:#}"));
        return;
    }
    finish(ctx);
}

fn attach_key_handler(ctx: &ActionContext, close_keybinds: &[crate::config::Keybind]) {
    let key_ctx = ctx.clone();
    let close_keybinds = close_keybinds.to_vec();
    let key_controller = new_key_controller(&ctx.session);
    key_controller.connect_key_pressed(move |ctrl, key, keycode, _| {
        let Some(event) = keys::current_key_event(ctrl) else {
            return Propagation::Proceed;
        };
        if matches_close_keybind(&event, &close_keybinds) {
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
                populate_overlay(&ctx.window, &ctx.session, next_mode, None);
            });
            return Propagation::Stop;
        }

        // Workspace key: action depends on mode
        if let Some((ch, _)) = keys::workspace_key_press(&event).filter(|(_, m)| m.is_empty()) {
            key_ctx.session.held_key.hold(keycode);
            dispatch_action(ch, &key_ctx);
            return Propagation::Stop;
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

/// New scroll position that brings the span `top..bottom` into a viewport at
/// `value` with height `page`, or `None` if it is already fully visible.
fn scroll_target(top: f64, bottom: f64, value: f64, page: f64) -> Option<f64> {
    if top < value {
        Some(top)
    } else if bottom > value + page {
        Some(bottom - page)
    } else {
        None
    }
}

/// Scroll `scrolled` vertically so that `child`, a descendant of its content,
/// is fully visible.
fn scroll_to_child(scrolled: &gtk4::ScrolledWindow, child: &impl IsA<gtk4::Widget>) {
    // Non-scrollable content gets wrapped in a Viewport; bounds relative to
    // the content itself are in adjustment coordinates.
    let Some(content) = scrolled
        .child()
        .map(|c| match c.downcast::<gtk4::Viewport>() {
            Ok(viewport) => viewport.child().unwrap_or_else(|| viewport.upcast()),
            Err(other) => other,
        })
    else {
        return;
    };
    let Some(bounds) = child.compute_bounds(&content) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let top = f64::from(bounds.y());
    let bottom = top + f64::from(bounds.height());
    if let Some(value) = scroll_target(top, bottom, adjustment.value(), adjustment.page_size()) {
        adjustment.set_value(value);
    }
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
    fn focused_workspace_id_from_returns_focused() {
        let workspaces = vec![
            test_workspace(1, Some("ws-1"), false),
            test_workspace(2, None, true),
        ];
        assert_eq!(focused_workspace_id_from(&workspaces), Some(2));
    }

    #[test]
    fn focused_workspace_id_from_returns_none_when_unfocused() {
        let workspaces = vec![test_workspace(1, Some("ws-1"), false)];
        assert_eq!(focused_workspace_id_from(&workspaces), None);
    }

    #[test]
    fn focused_window_from_is_the_focused_workspaces_active_window() {
        let mut focused = test_workspace(2, None, true);
        focused.active_window_id = Some(7);
        let mut other = test_workspace(1, Some("ws-1"), false);
        other.active_window_id = Some(3);
        assert_eq!(focused_window_from(&[other.clone(), focused]), Some(7));
        assert_eq!(focused_window_from(&[other]), None);
    }

    // --- HoverPreview ---

    #[test]
    fn hover_preview_skips_the_focused_workspace() {
        let p = HoverPreview::new(Some(1));
        assert!(!p.should_focus(1));
        assert!(p.should_focus(2));
        p.focused(2);
        assert!(!p.should_focus(2));
        assert!(p.should_focus(1));
    }

    #[test]
    fn hover_preview_restores_origin_once() {
        let p = HoverPreview::new(Some(1));
        assert_eq!(p.take_restore(), None);
        p.focused(2);
        p.focused(3);
        assert!(p.is_previewed(3));
        assert_eq!(p.take_restore(), Some(1));
        assert_eq!(p.take_restore(), None);
    }

    #[test]
    fn hover_preview_back_on_origin_needs_no_restore() {
        let p = HoverPreview::new(Some(1));
        p.focused(2);
        p.focused(1);
        assert!(!p.is_previewed(1));
        assert_eq!(p.take_restore(), None);
    }

    #[test]
    fn hover_preview_without_origin_never_restores() {
        let p = HoverPreview::new(None);
        assert!(p.should_focus(2));
        p.focused(2);
        assert!(p.is_previewed(2));
        assert_eq!(p.take_restore(), None);
    }

    #[test]
    fn hover_preview_rebase_moves_the_baseline() {
        let p = HoverPreview::new(Some(1));
        p.focused(2);
        p.rebase(Some(7));
        assert_eq!(p.take_restore(), None);
        assert!(!p.should_focus(7));
        p.focused(2);
        assert_eq!(p.take_restore(), Some(7));
    }

    // --- scroll_target ---

    #[test]
    fn scroll_target_moves_only_when_out_of_view() {
        // Viewport shows 100..200.
        assert_eq!(scroll_target(120.0, 150.0, 100.0, 100.0), None);
        assert_eq!(scroll_target(80.0, 110.0, 100.0, 100.0), Some(80.0));
        assert_eq!(scroll_target(190.0, 220.0, 100.0, 100.0), Some(120.0));
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
    fn mode_hint_is_mode_specific() {
        // The theme gallery and README screenshot render Switch mode.
        assert_eq!(Mode::Normal.hint(), "press key to select");
        assert!(Mode::Delete.hint().contains("closes its windows"));
        let [switch, delete, move_window] = Mode::all().map(Mode::hint);
        assert!(switch != delete && delete != move_window && move_window != switch);
    }

    #[test]
    fn mode_css_class() {
        assert_eq!(Mode::Normal.css_class(), "switch");
        assert_eq!(Mode::Delete.css_class(), "delete");
        assert_eq!(Mode::MoveWindow.css_class(), "move-window");
    }

    // --- delete confirmation ---

    #[test]
    fn needs_confirmation_arms_occupied_workspace() {
        assert!(needs_confirmation(true, 'a', 2, None));
    }

    #[test]
    fn needs_confirmation_same_key_confirms() {
        assert!(!needs_confirmation(true, 'a', 2, Some('a')));
    }

    #[test]
    fn needs_confirmation_other_key_rearms() {
        assert!(needs_confirmation(true, 'b', 1, Some('a')));
    }

    #[test]
    fn needs_confirmation_skipped_when_empty_or_disabled() {
        assert!(!needs_confirmation(true, 'a', 0, None));
        assert!(!needs_confirmation(false, 'a', 3, None));
    }

    #[test]
    fn confirm_delete_hint_counts_windows() {
        assert_eq!(
            confirm_delete_hint('a', 1),
            "press A again to close 1 window"
        );
        assert_eq!(
            confirm_delete_hint('3', 3),
            "press 3 again to close 3 windows"
        );
    }

    // --- diagnostics_summary ---

    fn problems(n: usize) -> Vec<String> {
        (1..=n)
            .map(|i| format!("config warning: problem {i}"))
            .collect()
    }

    #[test]
    fn diagnostics_summary_none_when_clean() {
        assert_eq!(diagnostics_summary(&[], 0), None);
    }

    #[test]
    fn diagnostics_summary_single_is_the_problem() {
        assert_eq!(
            diagnostics_summary(&problems(1), 1),
            Some("config warning: problem 1".to_string())
        );
    }

    #[test]
    fn diagnostics_summary_counts_the_rest() {
        assert_eq!(
            diagnostics_summary(&problems(3), 3),
            Some(
                "config warning: problem 1 (+2 more, run `niri-dynamic-workspaces check`)"
                    .to_string()
            )
        );
    }

    #[test]
    fn diagnostics_summary_points_to_check_only_for_hidden_config_problems() {
        // `check` does not load the theme, so it would not list these.
        let theme = |i| format!("theme warning: problem {i}");
        let mixed = vec![problems(1).remove(0), theme(2), theme(3)];
        assert_eq!(
            diagnostics_summary(&mixed, 1),
            Some("config warning: problem 1 (+2 more)".to_string())
        );
        assert_eq!(
            diagnostics_summary(&[theme(1), theme(2)], 0),
            Some("theme warning: problem 1 (+1 more)".to_string())
        );
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
