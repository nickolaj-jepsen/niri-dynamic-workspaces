use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Request, Response, Window, Workspace, WorkspaceReferenceArg};

fn send_request(request: Request) -> anyhow::Result<Response> {
    let mut socket = Socket::connect().context("failed to connect to niri")?;
    socket
        .send(request)
        .context("failed to send request")?
        .map_err(|msg| anyhow::anyhow!(msg))
}

fn send_action(action: Action) -> anyhow::Result<()> {
    match send_request(Request::Action(action))? {
        Response::Handled => Ok(()),
        other => bail!("unexpected response: {other:?}"),
    }
}

pub fn list_workspaces() -> anyhow::Result<Vec<Workspace>> {
    match send_request(Request::Workspaces)? {
        Response::Workspaces(mut workspaces) => {
            workspaces.sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
            Ok(workspaces)
        }
        other => bail!("unexpected response: {other:?}"),
    }
}

pub fn list_windows() -> anyhow::Result<Vec<Window>> {
    match send_request(Request::Windows)? {
        Response::Windows(windows) => Ok(windows),
        other => bail!("unexpected response: {other:?}"),
    }
}

/// Focus an existing workspace or create a new one.
///
/// Returns `true` if a new workspace was created, `false` if an existing one was focused.
pub fn focus_or_create_workspace(name: &str) -> anyhow::Result<bool> {
    let workspaces = list_workspaces()?;

    if workspaces.iter().any(|w| w.name.as_deref() == Some(name)) {
        send_action(Action::FocusWorkspace {
            reference: WorkspaceReferenceArg::Name(name.to_string()),
        })?;
        return Ok(false);
    }

    let focused_output = workspaces
        .iter()
        .find(|w| w.is_focused)
        .and_then(|w| w.output.clone());

    let max_idx = workspaces
        .iter()
        .filter(|w| w.output == focused_output)
        .map(|w| w.idx)
        .max()
        .unwrap_or(0);

    send_action(Action::FocusWorkspace {
        reference: WorkspaceReferenceArg::Index(max_idx + 1),
    })?;

    send_action(Action::SetWorkspaceName {
        name: name.to_string(),
        workspace: None,
    })?;

    Ok(true)
}

pub fn spawn_program(command: &[String]) -> anyhow::Result<()> {
    send_action(Action::Spawn {
        command: command.to_vec(),
    })
}

pub struct ReorderRequest {
    pub workspace_name: String,
    pub commands: Vec<String>,
    pub existing_window_ids: HashSet<u64>,
}

/// Extract the executable name from a command string.
/// Takes the first whitespace-delimited token and strips any leading path.
fn executable_name(command: &str) -> &str {
    let first_token = command.split_whitespace().next().unwrap_or(command);
    first_token.rsplit('/').next().unwrap_or(first_token)
}

/// Check whether a window's `app_id` matches an executable name.
/// Splits the `app_id` on `.` and checks if any segment equals the executable (case-insensitive).
fn app_id_matches(app_id: &str, exe: &str) -> bool {
    app_id
        .split('.')
        .any(|segment| segment.eq_ignore_ascii_case(exe))
}

/// Poll for newly spawned windows on a workspace and reorder columns to match config order.
///
/// Best-effort: logs errors to stderr since the overlay is already closed.
pub fn reorder_workspace_columns(request: &ReorderRequest) {
    if let Err(e) = reorder_workspace_columns_inner(request) {
        eprintln!("warning: failed to reorder columns: {e}");
    }
}

fn new_workspace_windows<'a>(
    windows: &'a [Window],
    ws_id: u64,
    existing_ids: &'a HashSet<u64>,
) -> impl Iterator<Item = &'a Window> {
    windows
        .iter()
        .filter(move |w| w.workspace_id == Some(ws_id))
        .filter(move |w| !existing_ids.contains(&w.id))
}

fn reorder_workspace_columns_inner(request: &ReorderRequest) -> anyhow::Result<()> {
    let expected_count = request.commands.len();

    // Find the workspace ID
    let workspaces = list_workspaces()?;
    let ws_id = workspaces
        .iter()
        .find(|w| w.name.as_deref() == Some(&request.workspace_name))
        .map(|w| w.id)
        .ok_or_else(|| anyhow::anyhow!("workspace '{}' not found", request.workspace_name))?;

    // Poll for new windows (200ms interval, 5s timeout)
    let poll_interval = Duration::from_millis(200);
    let timeout = Duration::from_secs(5);
    let start = Instant::now();

    // Phase 1: wait for all expected windows to appear
    let new_windows = loop {
        let windows = list_windows()?;
        let new: Vec<&Window> =
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

    // Phase 2: wait for windows to stabilize (same set of IDs for several cycles)
    // This handles apps like VS Code that remap/resize during startup.
    let stable_target = 3;
    let mut stable_count = 0u32;
    let mut last_ids: HashSet<u64> = new_windows.iter().map(|(id, _)| *id).collect();
    let stable_timeout = Duration::from_secs(8);

    while stable_count < stable_target && start.elapsed() < stable_timeout {
        thread::sleep(poll_interval);
        let windows = list_windows()?;
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
    let windows = list_windows()?;
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

    // Reorder: focus each window and move its column to the target index (1-based)
    for (i, window_id) in ordered_ids.iter().enumerate() {
        let Some(id) = window_id else { continue };
        if let Err(e) = send_action(Action::FocusWindow { id: *id }) {
            eprintln!("warning: failed to focus window {id}: {e}");
            continue;
        }
        thread::sleep(action_delay);
        if let Err(e) = send_action(Action::MoveColumnToIndex { index: i + 1 }) {
            eprintln!("warning: failed to move column to index {}: {e}", i + 1);
        }
        thread::sleep(action_delay);
    }

    Ok(())
}

/// Move the focused window to a named workspace, creating it if it doesn't exist.
///
/// Returns `true` if the workspace was newly created.
pub fn move_window_to_workspace(name: &str) -> anyhow::Result<bool> {
    let workspaces = list_workspaces()?;
    let exists = workspaces.iter().any(|w| w.name.as_deref() == Some(name));

    if !exists {
        let original_ws_id = workspaces
            .iter()
            .find(|w| w.is_focused)
            .map(|w| w.id)
            .ok_or_else(|| anyhow::anyhow!("no focused workspace found"))?;

        // Create the target workspace (this switches focus to it)
        focus_or_create_workspace(name)?;

        // Switch back so the focused window is the one the user intended to move
        send_action(Action::FocusWorkspace {
            reference: WorkspaceReferenceArg::Id(original_ws_id),
        })?;
    }

    send_action(Action::MoveWindowToWorkspace {
        window_id: None,
        reference: WorkspaceReferenceArg::Name(name.to_string()),
        focus: true,
    })?;

    Ok(!exists)
}

pub fn delete_workspace(name: &str) -> anyhow::Result<()> {
    let workspaces = list_workspaces()?;
    let ws = workspaces
        .iter()
        .find(|w| w.name.as_deref() == Some(name))
        .ok_or_else(|| anyhow::anyhow!("workspace '{name}' not found"))?;
    let ws_id = ws.id;

    // Close all windows on this workspace
    let windows = list_windows()?;
    for win in windows.iter().filter(|w| w.workspace_id == Some(ws_id)) {
        send_action(Action::CloseWindow { id: Some(win.id) })?;
    }

    // Unset the workspace name so niri cleans it up
    send_action(Action::UnsetWorkspaceName {
        reference: Some(WorkspaceReferenceArg::Name(name.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(app_id_matches("org.mozilla.Firefox", "firefox")); // case insensitive
        assert!(app_id_matches("firefox", "firefox")); // no dots
        assert!(!app_id_matches("org.mozilla.firefox", "chrome")); // no match
        assert!(!app_id_matches("org.mozilla.firefox", "fire")); // partial segment
    }
}
