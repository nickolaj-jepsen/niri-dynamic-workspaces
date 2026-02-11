pub mod hyprland;
pub mod niri;

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;

/// Compositor-agnostic workspace information.
pub struct WorkspaceInfo {
    pub id: u64,
    pub idx: usize,
    pub name: Option<String>,
    pub output: Option<String>,
    pub is_focused: bool,
    pub is_active: bool,
}

/// Compositor-agnostic window information.
pub struct WindowInfo {
    pub id: u64,
    pub workspace_id: Option<u64>,
    pub app_id: Option<String>,
    pub is_urgent: bool,
}

/// Request to reorder workspace columns after spawning programs.
pub struct ReorderRequest {
    pub workspace_name: String,
    pub commands: Vec<String>,
    pub existing_window_ids: HashSet<u64>,
}

/// Trait for compositor backends.
///
/// Each method maps to a compositor primitive. Shared orchestration logic
/// (e.g. switch + spawn + reorder) lives as free functions in this module.
pub trait Backend: Send + Sync {
    fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceInfo>>;
    fn list_windows(&self) -> anyhow::Result<Vec<WindowInfo>>;
    fn focus_or_create_workspace(&self, name: &str) -> anyhow::Result<bool>;
    fn spawn_program(&self, command: &[String]) -> anyhow::Result<()>;
    fn close_window(&self, id: u64) -> anyhow::Result<()>;
    fn focus_window(&self, id: u64) -> anyhow::Result<()>;
    fn move_window_to_position(&self, index: usize) -> anyhow::Result<()>;
    fn move_window_to_workspace(&self, name: &str) -> anyhow::Result<()>;
    fn remove_workspace(&self, name: &str) -> anyhow::Result<()>;

    /// Subscribe to compositor events and run cleanup when workspaces may become empty.
    ///
    /// This blocks forever and should be called from a dedicated thread.
    fn run_event_cleanup(&self, prefix: &str);
}

// --- Auto-detection ---

/// Create a backend based on config or environment auto-detection.
pub fn create_backend(config_compositor: Option<&str>) -> Arc<dyn Backend> {
    match config_compositor {
        Some("niri") => Arc::new(niri::NiriBackend),
        Some("hyprland") => Arc::new(hyprland::HyprlandBackend),
        Some(other) => {
            eprintln!("warning: unknown compositor '{other}', trying auto-detect");
            auto_detect()
        }
        None => auto_detect(),
    }
}

fn auto_detect() -> Arc<dyn Backend> {
    if std::env::var_os("NIRI_SOCKET").is_some() {
        Arc::new(niri::NiriBackend)
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Arc::new(hyprland::HyprlandBackend)
    } else {
        eprintln!("warning: no known compositor detected, defaulting to niri");
        Arc::new(niri::NiriBackend)
    }
}

// --- Shared orchestration functions ---

/// Switch to a workspace (creating it if needed) and spawn programs on creation.
///
/// Returns `(created, reorder_request)`.
pub fn switch_workspace(
    backend: &dyn Backend,
    name: &str,
    programs: &[String],
) -> anyhow::Result<(bool, Option<ReorderRequest>)> {
    let created = backend.focus_or_create_workspace(name)?;
    if created {
        return Ok((true, spawn_workspace_programs(backend, name, programs)?));
    }
    Ok((false, None))
}

/// Spawn programs for a newly created workspace.
///
/// If two or more programs are launched, returns a [`ReorderRequest`] that the
/// caller should pass to [`reorder_workspace_columns`].
pub fn spawn_workspace_programs(
    backend: &dyn Backend,
    workspace_name: &str,
    programs: &[String],
) -> anyhow::Result<Option<ReorderRequest>> {
    let existing_ids = if programs.len() >= 2 {
        snapshot_workspace_window_ids(backend, workspace_name)
    } else {
        HashSet::new()
    };

    for cmd_str in programs {
        let parts: Vec<String> = cmd_str.split_whitespace().map(String::from).collect();
        if parts.is_empty() {
            continue;
        }
        backend
            .spawn_program(&parts)
            .with_context(|| format!("failed to spawn '{cmd_str}'"))?;
    }

    if programs.len() >= 2 {
        Ok(Some(ReorderRequest {
            workspace_name: workspace_name.to_string(),
            commands: programs.to_vec(),
            existing_window_ids: existing_ids,
        }))
    } else {
        Ok(None)
    }
}

/// Poll for newly spawned windows on a workspace and reorder columns to match config order.
///
/// Best-effort: logs errors to stderr since the overlay is already closed.
pub fn reorder_workspace_columns(backend: &dyn Backend, request: &ReorderRequest) {
    if let Err(e) = reorder_workspace_columns_inner(backend, request) {
        eprintln!("warning: failed to reorder columns: {e}");
    }
}

fn new_workspace_windows<'a>(
    windows: &'a [WindowInfo],
    ws_id: u64,
    existing_ids: &'a HashSet<u64>,
) -> impl Iterator<Item = &'a WindowInfo> {
    windows
        .iter()
        .filter(move |w| w.workspace_id == Some(ws_id))
        .filter(move |w| !existing_ids.contains(&w.id))
}

fn reorder_workspace_columns_inner(
    backend: &dyn Backend,
    request: &ReorderRequest,
) -> anyhow::Result<()> {
    let expected_count = request.commands.len();

    let workspaces = backend.list_workspaces()?;
    let ws_id = find_workspace_by_name(&workspaces, &request.workspace_name)
        .map(|w| w.id)
        .ok_or_else(|| anyhow::anyhow!("workspace '{}' not found", request.workspace_name))?;

    let poll_interval = Duration::from_millis(200);
    let timeout = Duration::from_secs(5);
    let start = Instant::now();

    // Phase 1: wait for all expected windows to appear
    let new_windows = loop {
        let windows = backend.list_windows()?;
        let new: Vec<&WindowInfo> =
            new_workspace_windows(&windows, ws_id, &request.existing_window_ids).collect();

        if new.len() >= expected_count || start.elapsed() >= timeout {
            let result: Vec<(u64, String)> = new
                .iter()
                .map(|w| (w.id, w.app_id.clone().unwrap_or_default()))
                .collect();
            break result;
        }

        thread::sleep(poll_interval);
    };

    // Phase 2: wait for windows to stabilize
    let stable_target = 3;
    let mut stable_count = 0u32;
    let mut last_ids: HashSet<u64> = new_windows.iter().map(|(id, _)| *id).collect();
    let stable_timeout = Duration::from_secs(8);

    while stable_count < stable_target && start.elapsed() < stable_timeout {
        thread::sleep(poll_interval);
        let windows = backend.list_windows()?;
        let current_ids: HashSet<u64> =
            new_workspace_windows(&windows, ws_id, &request.existing_window_ids)
                .map(|w| w.id)
                .collect();

        if current_ids == last_ids {
            stable_count += 1;
        } else {
            last_ids = current_ids;
            stable_count = 0;
        }
    }

    // Re-fetch the final set of new windows after stabilization
    let windows = backend.list_windows()?;
    let new_windows: Vec<(u64, String)> =
        new_workspace_windows(&windows, ws_id, &request.existing_window_ids)
            .map(|w| (w.id, w.app_id.clone().unwrap_or_default()))
            .collect();

    if new_windows.is_empty() {
        return Ok(());
    }

    // Match each command to a new window by executable name / app_id
    let exe_names: Vec<&str> = request
        .commands
        .iter()
        .map(|c| executable_name(c))
        .collect();

    let mut used_window_ids: HashSet<u64> = HashSet::new();
    let mut ordered_ids: Vec<Option<u64>> = Vec::with_capacity(expected_count);

    for exe in &exe_names {
        let matched = new_windows
            .iter()
            .find(|(id, app_id)| !used_window_ids.contains(id) && app_id_matches(app_id, exe));
        if let Some((id, _)) = matched {
            used_window_ids.insert(*id);
            ordered_ids.push(Some(*id));
        } else {
            ordered_ids.push(None);
        }
    }

    let action_delay = Duration::from_millis(50);

    for (i, window_id) in ordered_ids.iter().enumerate() {
        let Some(id) = window_id else { continue };
        if let Err(e) = backend.focus_window(*id) {
            eprintln!("warning: failed to focus window {id}: {e}");
            continue;
        }
        thread::sleep(action_delay);
        if let Err(e) = backend.move_window_to_position(i + 1) {
            eprintln!("warning: failed to move column to index {}: {e}", i + 1);
        }
        thread::sleep(action_delay);
    }

    Ok(())
}

/// Delete a workspace: close all its windows and remove it.
pub fn delete_workspace(backend: &dyn Backend, name: &str) -> anyhow::Result<()> {
    let workspaces = backend.list_workspaces()?;
    let ws = find_workspace_by_name(&workspaces, name)
        .ok_or_else(|| anyhow::anyhow!("workspace '{name}' not found"))?;
    let ws_id = ws.id;

    let windows = backend.list_windows()?;
    for win in windows.iter().filter(|w| w.workspace_id == Some(ws_id)) {
        backend.close_window(win.id)?;
    }

    backend.remove_workspace(name)
}

/// Remove empty, unfocused dynamic workspaces matching the given prefix.
///
/// Best-effort: logs errors to stderr.
pub fn cleanup_empty_workspaces(backend: &dyn Backend, prefix: &str) {
    if let Err(e) = cleanup_empty_workspaces_inner(backend, prefix) {
        eprintln!("warning: failed to clean up empty workspaces: {e}");
    }
}

fn cleanup_empty_workspaces_inner(backend: &dyn Backend, prefix: &str) -> anyhow::Result<()> {
    let workspaces = backend.list_workspaces()?;
    let windows = backend.list_windows()?;

    let window_ws_ids: HashSet<u64> = windows.iter().filter_map(|w| w.workspace_id).collect();

    for ws in &workspaces {
        let name = match &ws.name {
            Some(n) if n.starts_with(prefix) => n,
            _ => continue,
        };

        if ws.is_focused || ws.is_active || window_ws_ids.contains(&ws.id) {
            continue;
        }

        backend.remove_workspace(name)?;
    }

    Ok(())
}

/// Snapshot all window IDs currently on a named workspace.
pub fn snapshot_workspace_window_ids(backend: &dyn Backend, workspace_name: &str) -> HashSet<u64> {
    let Ok(workspaces) = backend.list_workspaces() else {
        return HashSet::new();
    };
    let ws_id = match find_workspace_by_name(&workspaces, workspace_name) {
        Some(w) => w.id,
        None => return HashSet::new(),
    };
    let Ok(windows) = backend.list_windows() else {
        return HashSet::new();
    };
    windows
        .iter()
        .filter(|w| w.workspace_id == Some(ws_id))
        .map(|w| w.id)
        .collect()
}

/// Run hook commands in a background thread via `sh -c`.
pub fn run_hooks(commands: &[String], env: &[(String, String)]) {
    if commands.is_empty() {
        return;
    }
    let commands: Vec<String> = commands.to_vec();
    let env: Vec<(String, String)> = env.to_vec();
    std::thread::Builder::new()
        .name("hooks".into())
        .spawn(move || {
            for cmd in &commands {
                let result = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::inherit())
                    .spawn();
                match result {
                    Ok(mut child) => {
                        if let Err(e) = child.wait() {
                            eprintln!("warning: hook '{cmd}' failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("warning: failed to spawn hook '{cmd}': {e}"),
                }
            }
        })
        .ok();
}

// --- Event loop helpers ---

/// Retry an event loop forever, reconnecting after failures.
///
/// Used by backend `run_event_cleanup` implementations.
pub(crate) fn retry_event_loop(mut connect_and_run: impl FnMut() -> anyhow::Result<()>) {
    loop {
        if let Err(e) = connect_and_run() {
            eprintln!("warning: event cleanup failed: {e:#}, reconnecting in 5s\u{2026}");
            thread::sleep(Duration::from_secs(5));
        }
    }
}

/// Debounced cleanup state for event-driven workspace cleanup.
///
/// Both backends share the same debounce-then-cleanup pattern: mark pending
/// when a relevant event arrives, flush when enough time has elapsed.
pub(crate) struct DebouncedCleanup {
    last_cleanup: Instant,
    pending: bool,
}

impl DebouncedCleanup {
    const DEBOUNCE: Duration = Duration::from_millis(500);

    pub fn new() -> Self {
        Self {
            last_cleanup: Instant::now(),
            pending: false,
        }
    }

    pub fn mark_pending(&mut self) {
        self.pending = true;
    }

    pub fn flush_if_ready(&mut self, backend: &dyn Backend, prefix: &str) {
        if self.pending && self.last_cleanup.elapsed() >= Self::DEBOUNCE {
            cleanup_empty_workspaces(backend, prefix);
            self.pending = false;
            self.last_cleanup = Instant::now();
        }
    }
}

// --- Shared helpers ---

pub(crate) fn find_workspace_by_name<'a>(
    workspaces: &'a [WorkspaceInfo],
    name: &str,
) -> Option<&'a WorkspaceInfo> {
    workspaces.iter().find(|w| w.name.as_deref() == Some(name))
}

/// Extract the executable name from a command string.
fn executable_name(command: &str) -> &str {
    let first_token = command.split_whitespace().next().unwrap_or(command);
    first_token.rsplit('/').next().unwrap_or(first_token)
}

/// Check whether a window's `app_id` matches an executable name.
fn app_id_matches(app_id: &str, exe: &str) -> bool {
    app_id
        .split('.')
        .any(|segment| segment.eq_ignore_ascii_case(exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{test_window, test_workspace};

    #[test]
    fn executable_name_variants() {
        assert_eq!(executable_name("firefox"), "firefox");
        assert_eq!(executable_name("firefox --private-window"), "firefox");
        assert_eq!(executable_name("/usr/bin/firefox"), "firefox");
        assert_eq!(
            executable_name("/usr/bin/firefox --private-window"),
            "firefox"
        );
    }

    #[test]
    fn app_id_matches_variants() {
        assert!(app_id_matches("org.mozilla.firefox", "firefox"));
        assert!(app_id_matches("org.mozilla.Firefox", "firefox"));
        assert!(app_id_matches("firefox", "firefox"));
        assert!(!app_id_matches("org.mozilla.firefox", "chrome"));
        assert!(!app_id_matches("org.mozilla.firefox", "fire"));
    }

    #[test]
    fn new_workspace_windows_filters_correctly() {
        let windows = vec![
            test_window(1, 10, "firefox"),
            test_window(2, 10, "kitty"),
            test_window(3, 20, "slack"),
            test_window(4, 10, "code"),
        ];
        let existing = HashSet::from([1]);

        let result: Vec<u64> = new_workspace_windows(&windows, 10, &existing)
            .map(|w| w.id)
            .collect();

        assert_eq!(result, vec![2, 4]);
    }

    #[test]
    fn new_workspace_windows_empty() {
        let windows = vec![test_window(1, 20, "firefox"), test_window(2, 30, "kitty")];
        let existing = HashSet::new();

        let result: Vec<u64> = new_workspace_windows(&windows, 10, &existing)
            .map(|w| w.id)
            .collect();

        assert!(result.is_empty());
    }

    #[test]
    fn find_workspace_by_name_variants() {
        let workspaces = vec![
            test_workspace(1, Some("dyn-a"), false),
            test_workspace(2, Some("dyn-b"), true),
            test_workspace(3, None, false),
        ];

        let ws = find_workspace_by_name(&workspaces, "dyn-a");
        assert_eq!(ws.map(|w| w.id), Some(1));

        let ws = find_workspace_by_name(&workspaces, "dyn-z");
        assert!(ws.is_none());

        let ws = find_workspace_by_name(&workspaces, "");
        assert!(ws.is_none());
    }
}
