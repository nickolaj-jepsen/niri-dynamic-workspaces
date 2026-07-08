use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Event, Request, Response, Window, Workspace, WorkspaceReferenceArg};

/// Transport for niri IPC requests.
///
/// Implemented by [`SocketClient`] for the real compositor socket; tests use
/// a scripted mock to assert the exact request sequence.
pub trait NiriClient {
    fn send(&mut self, request: Request) -> anyhow::Result<Response>;
}

/// [`NiriClient`] that opens a fresh connection to the niri socket per request.
pub struct SocketClient;

impl NiriClient for SocketClient {
    fn send(&mut self, request: Request) -> anyhow::Result<Response> {
        let mut socket = Socket::connect().context("failed to connect to niri")?;
        socket
            .send(request)
            .context("failed to send request")?
            .map_err(|msg| anyhow::anyhow!(msg))
    }
}

fn send_action_with(client: &mut impl NiriClient, action: Action) -> anyhow::Result<()> {
    match client.send(Request::Action(action))? {
        Response::Handled => Ok(()),
        other => bail!("unexpected response: {other:?}"),
    }
}

fn send_action(action: Action) -> anyhow::Result<()> {
    send_action_with(&mut SocketClient, action)
}

/// Focus an existing workspace by name (no creation).
pub fn focus_workspace_by_name(name: &str) -> anyhow::Result<()> {
    send_action(Action::FocusWorkspace {
        reference: WorkspaceReferenceArg::Name(name.to_string()),
    })
}

fn list_workspaces_with(client: &mut impl NiriClient) -> anyhow::Result<Vec<Workspace>> {
    match client.send(Request::Workspaces)? {
        Response::Workspaces(mut workspaces) => {
            workspaces.sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
            Ok(workspaces)
        }
        other => bail!("unexpected response: {other:?}"),
    }
}

pub fn list_workspaces() -> anyhow::Result<Vec<Workspace>> {
    list_workspaces_with(&mut SocketClient)
}

fn find_workspace_by_name<'a>(workspaces: &'a [Workspace], name: &str) -> Option<&'a Workspace> {
    workspaces.iter().find(|w| w.name.as_deref() == Some(name))
}

/// Find a workspace by prefix and key character.
///
/// Matches workspaces whose name starts with `{prefix}{ch}` followed by end-of-string
/// or a space (for titled workspaces like `"dyn-a My Project"`).
fn find_workspace_by_char<'a>(
    workspaces: &'a [Workspace],
    prefix: &str,
    ch: char,
) -> Option<&'a Workspace> {
    workspaces.iter().find(|w| {
        w.name.as_ref().is_some_and(|n| {
            n.strip_prefix(prefix).and_then(|rest| rest.chars().next()) == Some(ch)
        })
    })
}

/// Get the full name of an existing workspace matched by prefix+char.
fn find_workspace_name(workspaces: &[Workspace], prefix: &str, ch: char) -> Option<String> {
    find_workspace_by_char(workspaces, prefix, ch).and_then(|w| w.name.clone())
}

fn list_windows_with(client: &mut impl NiriClient) -> anyhow::Result<Vec<Window>> {
    match client.send(Request::Windows)? {
        Response::Windows(windows) => Ok(windows),
        other => bail!("unexpected response: {other:?}"),
    }
}

pub fn list_windows() -> anyhow::Result<Vec<Window>> {
    list_windows_with(&mut SocketClient)
}

/// Focus an existing workspace or create a new one.
///
/// Finds existing workspaces by prefix+char (to handle titled names).
/// When creating, uses `full_name` for the workspace name.
/// Returns `true` if a new workspace was created, `false` if an existing one was focused.
pub fn focus_or_create_workspace(prefix: &str, ch: char, full_name: &str) -> anyhow::Result<bool> {
    focus_or_create_impl(&mut SocketClient, prefix, ch, full_name)
}

/// Name the trailing empty workspace on the focused output.
///
/// Targets the workspace by id so the result doesn't depend on what happens
/// to be focused when the request lands (naming the focused workspace was
/// racy: focus can move between IPC calls).
fn name_trailing_empty_workspace(
    client: &mut impl NiriClient,
    workspaces: &[Workspace],
    full_name: &str,
) -> anyhow::Result<()> {
    let focused_output = workspaces
        .iter()
        .find(|w| w.is_focused)
        .and_then(|w| w.output.clone());

    let target = workspaces
        .iter()
        .filter(|w| w.output == focused_output)
        .max_by_key(|w| w.idx)
        .filter(|w| w.name.is_none() && w.active_window_id.is_none())
        .ok_or_else(|| anyhow::anyhow!("no empty workspace available on the focused output"))?;

    send_action_with(
        client,
        Action::SetWorkspaceName {
            name: full_name.to_string(),
            workspace: Some(WorkspaceReferenceArg::Id(target.id)),
        },
    )
}

fn focus_or_create_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
    full_name: &str,
) -> anyhow::Result<bool> {
    let workspaces = list_workspaces_with(client)?;

    if let Some(existing_name) = find_workspace_name(&workspaces, prefix, ch) {
        send_action_with(
            client,
            Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(existing_name),
            },
        )?;
        return Ok(false);
    }

    name_trailing_empty_workspace(client, &workspaces, full_name)?;

    send_action_with(
        client,
        Action::FocusWorkspace {
            reference: WorkspaceReferenceArg::Name(full_name.to_string()),
        },
    )?;

    Ok(true)
}

/// Switch to a workspace (creating it if needed) and spawn programs on creation.
///
/// Combines [`focus_or_create_workspace`] with [`spawn_workspace_programs`].
/// Returns `(created, reorder_request)` where `created` indicates whether a new
/// workspace was made, and `reorder_request` is present when multiple programs
/// were spawned.
pub fn switch_workspace(
    prefix: &str,
    ch: char,
    full_name: &str,
    programs: &[String],
) -> anyhow::Result<(bool, Option<ReorderRequest>)> {
    let created = focus_or_create_workspace(prefix, ch, full_name)?;
    if created {
        return Ok((true, spawn_workspace_programs(full_name, programs)?));
    }
    Ok((false, None))
}

/// Spawn programs for a newly created workspace.
///
/// Splits each command string with shell quoting rules (no shell is invoked)
/// and spawns via niri IPC.
/// If two or more programs are launched, returns a [`ReorderRequest`] that the
/// caller should pass to [`reorder_workspace_columns`] (either synchronously
/// or in a background thread).
pub fn spawn_workspace_programs(
    workspace_name: &str,
    programs: &[String],
) -> anyhow::Result<Option<ReorderRequest>> {
    let needs_reorder = programs.len() >= 2;
    let existing_ids = if needs_reorder {
        snapshot_workspace_window_ids(workspace_name)
    } else {
        HashSet::new()
    };

    for cmd_str in programs {
        let parts = shell_words::split(cmd_str)
            .with_context(|| format!("failed to parse command '{cmd_str}'"))?;
        if parts.is_empty() {
            continue;
        }
        spawn_program(&parts).with_context(|| format!("failed to spawn '{cmd_str}'"))?;
    }

    if needs_reorder {
        Ok(Some(ReorderRequest {
            workspace_name: workspace_name.to_string(),
            commands: programs.to_vec(),
            existing_window_ids: existing_ids,
        }))
    } else {
        Ok(None)
    }
}

pub fn spawn_program(command: &[String]) -> anyhow::Result<()> {
    send_action(Action::Spawn {
        command: command.to_vec(),
    })
}

#[derive(Debug)]
pub struct ReorderRequest {
    pub workspace_name: String,
    pub commands: Vec<String>,
    pub existing_window_ids: HashSet<u64>,
}

/// Extract the executable name from a command string.
///
/// Takes the first shell-word (falling back to whitespace splitting when the
/// command doesn't parse) and strips any leading path.
fn executable_name(command: &str) -> String {
    let first_token = shell_words::split(command)
        .ok()
        .and_then(|words| words.into_iter().next())
        .unwrap_or_else(|| {
            command
                .split_whitespace()
                .next()
                .unwrap_or(command)
                .to_string()
        });
    first_token
        .rsplit('/')
        .next()
        .unwrap_or(&first_token)
        .to_string()
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
    let ws_id = find_workspace_by_name(&workspaces, &request.workspace_name)
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
    let exe_names: Vec<String> = request
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

/// Move the focused window to an existing workspace by name.
pub fn move_window_to_workspace_by_name(name: &str) -> anyhow::Result<()> {
    send_action(Action::MoveWindowToWorkspace {
        window_id: None,
        reference: WorkspaceReferenceArg::Name(name.to_string()),
        focus: true,
    })
}

/// Move the focused window to a workspace, creating it if it doesn't exist.
pub fn move_window_to_workspace(prefix: &str, ch: char, full_name: &str) -> anyhow::Result<()> {
    move_window_impl(&mut SocketClient, prefix, ch, full_name)
}

fn move_window_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
    full_name: &str,
) -> anyhow::Result<()> {
    let workspaces = list_workspaces_with(client)?;

    let ws_name = if let Some(existing) = find_workspace_name(&workspaces, prefix, ch) {
        existing
    } else {
        // Naming by id needs no focus change, so the focused window — the one
        // the user intends to move — stays focused throughout.
        name_trailing_empty_workspace(client, &workspaces, full_name)?;
        full_name.to_string()
    };

    send_action_with(
        client,
        Action::MoveWindowToWorkspace {
            window_id: None,
            reference: WorkspaceReferenceArg::Name(ws_name),
            focus: true,
        },
    )
}

/// Remove empty, unfocused dynamic workspaces matching the given prefix.
///
/// Best-effort: logs errors to stderr since this runs in the background daemon.
pub fn cleanup_empty_workspaces(prefix: &str) {
    if let Err(e) = cleanup_empty_workspaces_inner(prefix) {
        eprintln!("warning: failed to clean up empty workspaces: {e}");
    }
}

fn cleanup_empty_workspaces_inner(prefix: &str) -> anyhow::Result<()> {
    cleanup_empty_workspaces_impl(&mut SocketClient, prefix)
}

fn cleanup_empty_workspaces_impl(client: &mut impl NiriClient, prefix: &str) -> anyhow::Result<()> {
    let workspaces = list_workspaces_with(client)?;
    let windows = list_windows_with(client)?;

    let window_ws_ids: HashSet<u64> = windows.iter().filter_map(|w| w.workspace_id).collect();

    for ws in &workspaces {
        let name = match &ws.name {
            Some(n) if n.starts_with(prefix) => n,
            _ => continue,
        };

        if ws.is_focused || ws.is_active || window_ws_ids.contains(&ws.id) {
            continue;
        }

        send_action_with(
            client,
            Action::UnsetWorkspaceName {
                reference: Some(WorkspaceReferenceArg::Name(name.clone())),
            },
        )?;
    }

    Ok(())
}

/// Subscribe to niri's event stream and run cleanup when workspaces may become empty.
///
/// Reconnects automatically if the socket drops (e.g. niri restarts).
pub fn run_event_cleanup(prefix: &str) {
    loop {
        if let Err(e) = event_cleanup_loop(prefix) {
            eprintln!("warning: event cleanup failed: {e:#}, reconnecting in 5s\u{2026}");
            thread::sleep(Duration::from_secs(5));
        }
    }
}

/// Connect to niri and subscribe to the event stream.
///
/// Returns a buffered reader over the socket, ready to read events line-by-line.
fn connect_event_stream() -> anyhow::Result<BufReader<UnixStream>> {
    let socket_path =
        std::env::var_os(niri_ipc::socket::SOCKET_PATH_ENV).context("NIRI_SOCKET not set")?;
    let stream = UnixStream::connect(socket_path).context("failed to connect to niri")?;
    let mut reader = BufReader::new(stream);

    let mut buf = serde_json::to_string(&Request::EventStream).unwrap();
    buf.push('\n');
    reader.get_mut().write_all(buf.as_bytes())?;

    buf.clear();
    reader.read_line(&mut buf)?;
    let reply: Result<Response, String> =
        serde_json::from_str(&buf).context("failed to parse response")?;
    reply.map_err(|msg| anyhow::anyhow!(msg))?;

    Ok(reader)
}

fn event_cleanup_loop(prefix: &str) -> anyhow::Result<()> {
    let mut reader = connect_event_stream()?;

    // Read events line-by-line (no shutdown — avoids half-close issues with newer niri)
    let debounce = Duration::from_millis(500);
    let mut last_cleanup = Instant::now();
    let mut cleanup_pending = false;
    let mut buf = String::new();

    loop {
        buf.clear();
        let n = reader
            .read_line(&mut buf)
            .context("failed to read from niri socket")?;
        if n == 0 {
            bail!("niri event stream closed");
        }

        // Skip events that don't deserialize (e.g. new variants from a newer niri)
        let Ok(event) = serde_json::from_str::<Event>(&buf) else {
            continue;
        };

        match &event {
            Event::WindowOpenedOrChanged { .. }
            | Event::WindowClosed { .. }
            | Event::WindowsChanged { .. }
            | Event::WorkspaceActivated { .. }
            | Event::WorkspacesChanged { .. } => {
                cleanup_pending = true;
            }
            _ => {}
        }

        if cleanup_pending && last_cleanup.elapsed() >= debounce {
            cleanup_empty_workspaces(prefix);
            cleanup_pending = false;
            last_cleanup = Instant::now();
        }
    }
}

/// Snapshot all window IDs currently on a named workspace.
///
/// Returns an empty set if the workspace doesn't exist or IPC fails.
pub fn snapshot_workspace_window_ids(workspace_name: &str) -> HashSet<u64> {
    let Ok(workspaces) = list_workspaces() else {
        return HashSet::new();
    };
    let ws_id = match find_workspace_by_name(&workspaces, workspace_name) {
        Some(w) => w.id,
        None => return HashSet::new(),
    };
    let Ok(windows) = list_windows() else {
        return HashSet::new();
    };
    windows
        .iter()
        .filter(|w| w.workspace_id == Some(ws_id))
        .map(|w| w.id)
        .collect()
}

/// Run hook commands in a background thread via `sh -c`.
///
/// Each command runs sequentially with the given environment variables set.
/// Errors are logged to stderr. No-op if `commands` is empty.
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

pub fn delete_workspace(prefix: &str, ch: char) -> anyhow::Result<()> {
    delete_workspace_impl(&mut SocketClient, prefix, ch)
}

fn delete_workspace_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
) -> anyhow::Result<()> {
    let workspaces = list_workspaces_with(client)?;
    let ws = find_workspace_by_char(&workspaces, prefix, ch)
        .ok_or_else(|| anyhow::anyhow!("workspace '{prefix}{ch}' not found"))?;
    let ws_id = ws.id;
    let ws_name = ws
        .name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("workspace has no name"))?;

    // Close all windows on this workspace
    let windows = list_windows_with(client)?;
    for win in windows.iter().filter(|w| w.workspace_id == Some(ws_id)) {
        send_action_with(client, Action::CloseWindow { id: Some(win.id) })?;
    }

    // Unset the workspace name so niri cleans it up
    send_action_with(
        client,
        Action::UnsetWorkspaceName {
            reference: Some(WorkspaceReferenceArg::Name(ws_name)),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::test_helpers::{test_window, test_workspace};

    /// Scripted [`NiriClient`] that records every request it receives.
    struct MockClient {
        responses: VecDeque<Response>,
        sent: Vec<Request>,
    }

    impl MockClient {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: responses.into(),
                sent: Vec::new(),
            }
        }
    }

    impl NiriClient for MockClient {
        fn send(&mut self, request: Request) -> anyhow::Result<Response> {
            self.sent.push(request);
            self.responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no scripted response left"))
        }
    }

    #[test]
    fn focus_or_create_focuses_existing_by_full_name() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![test_workspace(1, Some("dyn-a My Project"), false)]),
            Response::Handled,
        ]);

        let created = focus_or_create_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap();

        assert!(!created);
        assert_eq!(client.sent.len(), 2);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(n),
            }) if n == "dyn-a My Project"
        ));
    }

    /// Focused workspace (id 1, idx 1) plus the trailing empty unnamed
    /// workspace (id 2, idx 2) that niri keeps on every output.
    fn workspaces_with_trailing_empty() -> Vec<Workspace> {
        let mut focused = test_workspace(1, Some("browser"), true);
        focused.active_window_id = Some(100);
        let mut empty = test_workspace(2, None, false);
        empty.idx = 2;
        vec![focused, empty]
    }

    #[test]
    fn focus_or_create_names_trailing_empty_by_id_then_focuses() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
        ]);

        let created = focus_or_create_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap();

        assert!(created);
        assert_eq!(client.sent.len(), 3);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::SetWorkspaceName {
                name,
                workspace: Some(WorkspaceReferenceArg::Id(2)),
            }) if name == "dyn-a"
        ));
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(n),
            }) if n == "dyn-a"
        ));
    }

    #[test]
    fn focus_or_create_errors_when_trailing_workspace_not_empty() {
        // Highest-idx workspace on the output is named/occupied — unexpected
        // state, so creation must fail instead of renaming it.
        let mut ws = test_workspace(1, Some("browser"), true);
        ws.active_window_id = Some(100);
        let mut client = MockClient::new(vec![Response::Workspaces(vec![ws])]);

        let err = focus_or_create_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap_err();

        assert!(err.to_string().contains("no empty workspace"));
        assert_eq!(client.sent.len(), 1);
    }

    #[test]
    fn move_window_to_existing_workspace_moves_directly() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![test_workspace(1, Some("dyn-a My Project"), false)]),
            Response::Handled,
        ]);

        move_window_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap();

        assert_eq!(client.sent.len(), 2);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: None,
                reference: WorkspaceReferenceArg::Name(n),
                focus: true,
            }) if n == "dyn-a My Project"
        ));
    }

    #[test]
    fn move_window_creates_target_without_focus_change() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
        ]);

        move_window_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap();

        assert_eq!(client.sent.len(), 3);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::SetWorkspaceName {
                name,
                workspace: Some(WorkspaceReferenceArg::Id(2)),
            }) if name == "dyn-a"
        ));
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: None,
                reference: WorkspaceReferenceArg::Name(n),
                focus: true,
            }) if n == "dyn-a"
        ));
        // The old implementation focused the new workspace and back — no
        // FocusWorkspace action may appear at all.
        assert!(!client
            .sent
            .iter()
            .any(|r| matches!(r, Request::Action(Action::FocusWorkspace { .. }))));
    }

    #[test]
    fn delete_workspace_closes_windows_then_unsets_name() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![test_workspace(10, Some("dyn-a"), false)]),
            Response::Windows(vec![
                test_window(1, 10, "firefox"),
                test_window(2, 20, "kitty"),
            ]),
            Response::Handled,
            Response::Handled,
        ]);

        delete_workspace_impl(&mut client, "dyn-", 'a').unwrap();

        assert_eq!(client.sent.len(), 4);
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::CloseWindow { id: Some(1) })
        ));
        assert!(matches!(
            &client.sent[3],
            Request::Action(Action::UnsetWorkspaceName {
                reference: Some(WorkspaceReferenceArg::Name(n)),
            }) if n == "dyn-a"
        ));
    }

    #[test]
    fn delete_workspace_missing_errors_without_actions() {
        let mut client = MockClient::new(vec![Response::Workspaces(vec![])]);

        let err = delete_workspace_impl(&mut client, "dyn-", 'a').unwrap_err();

        assert!(err.to_string().contains("not found"));
        assert_eq!(client.sent.len(), 1);
    }

    #[test]
    fn cleanup_skips_focused_active_and_occupied() {
        let mut occupied = test_workspace(1, Some("dyn-a"), false);
        occupied.active_window_id = Some(100);
        let focused = test_workspace(2, Some("dyn-b"), true);
        let mut active = test_workspace(3, Some("dyn-c"), false);
        active.is_active = true;
        let empty = test_workspace(4, Some("dyn-d"), false);
        let non_dyn = test_workspace(5, Some("static"), false);

        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![occupied, focused, active, empty, non_dyn]),
            Response::Windows(vec![test_window(100, 1, "firefox")]),
            Response::Handled,
        ]);

        cleanup_empty_workspaces_impl(&mut client, "dyn-").unwrap();

        assert_eq!(client.sent.len(), 3);
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::UnsetWorkspaceName {
                reference: Some(WorkspaceReferenceArg::Name(n)),
            }) if n == "dyn-d"
        ));
    }

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
    fn executable_name_quoted_paths() {
        assert_eq!(
            executable_name("'/opt/my apps/firefox' --private-window"),
            "firefox"
        );
        assert_eq!(executable_name("code '/home/me/my project'"), "code");
        // Unparseable input falls back to whitespace splitting
        assert_eq!(executable_name("foo 'unclosed"), "foo");
    }

    #[test]
    fn spawn_workspace_programs_rejects_malformed_quoting() {
        let err = spawn_workspace_programs("dyn-a", &["code 'unclosed".to_string()]).unwrap_err();
        assert!(err.to_string().contains("failed to parse"));
    }

    #[test]
    fn app_id_matches_variants() {
        assert!(app_id_matches("org.mozilla.firefox", "firefox"));
        assert!(app_id_matches("org.mozilla.Firefox", "firefox")); // case insensitive
        assert!(app_id_matches("firefox", "firefox")); // no dots
        assert!(!app_id_matches("org.mozilla.firefox", "chrome")); // no match
        assert!(!app_id_matches("org.mozilla.firefox", "fire")); // partial segment
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

        // Found
        let ws = find_workspace_by_name(&workspaces, "dyn-a");
        assert_eq!(ws.map(|w| w.id), Some(1));

        // Not found
        let ws = find_workspace_by_name(&workspaces, "dyn-z");
        assert!(ws.is_none());

        // None-named workspaces are never matched
        let ws = find_workspace_by_name(&workspaces, "");
        assert!(ws.is_none());
    }

    #[test]
    fn find_workspace_by_char_basic() {
        let workspaces = vec![
            test_workspace(1, Some("dyn-a"), false),
            test_workspace(2, Some("dyn-b My Project"), true),
            test_workspace(3, None, false),
        ];

        // Bare name
        let ws = find_workspace_by_char(&workspaces, "dyn-", 'a');
        assert_eq!(ws.map(|w| w.id), Some(1));

        // Titled name
        let ws = find_workspace_by_char(&workspaces, "dyn-", 'b');
        assert_eq!(ws.map(|w| w.id), Some(2));

        // Not found
        let ws = find_workspace_by_char(&workspaces, "dyn-", 'z');
        assert!(ws.is_none());

        // None-named workspaces never match
        let ws = find_workspace_by_char(&workspaces, "dyn-", 'c');
        assert!(ws.is_none());
    }
}
