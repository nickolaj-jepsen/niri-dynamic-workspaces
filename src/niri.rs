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

/// Focus an existing workspace by id (no creation).
///
/// niri answers `Handled` even when no workspace has that id.
pub fn focus_workspace_by_id(id: u64) -> anyhow::Result<()> {
    send_action(Action::FocusWorkspace {
        reference: WorkspaceReferenceArg::Id(id),
    })
}

/// Whether two workspace names match the way niri matches them: ASCII case-insensitively.
pub fn same_workspace_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn workspace_id_by_name_with(client: &mut impl NiriClient, name: &str) -> anyhow::Result<u64> {
    list_workspaces_with(client)?
        .iter()
        .find(|w| {
            w.name
                .as_deref()
                .is_some_and(|n| same_workspace_name(n, name))
        })
        .map(|w| w.id)
        .with_context(|| format!("workspace '{name}' not found"))
}

/// Id of the existing workspace named `name`, matched like niri matches names.
///
/// # Errors
/// When no workspace has that name.
pub fn workspace_id_by_name(name: &str) -> anyhow::Result<u64> {
    workspace_id_by_name_with(&mut SocketClient, name)
}

/// The focused workspace's active window: what `window_id: None` would move.
///
/// Layout-based, so it stays valid while the overlay holds keyboard focus.
fn focused_window_id(workspaces: &[Workspace]) -> anyhow::Result<u64> {
    workspaces
        .iter()
        .find(|w| w.is_focused)
        .and_then(|w| w.active_window_id)
        .context("no focused window to move")
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
        w.name
            .as_deref()
            .and_then(|n| crate::config::parse_dynamic_name(n, prefix))
            .is_some_and(|(key, _)| key == ch)
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

/// Find the trailing empty workspace on the focused output.
///
/// Target it by id: focus can move between IPC calls.
fn trailing_empty_workspace(workspaces: &[Workspace]) -> anyhow::Result<&Workspace> {
    let focused_output = workspaces
        .iter()
        .find(|w| w.is_focused)
        .and_then(|w| w.output.clone());

    workspaces
        .iter()
        .filter(|w| w.output == focused_output)
        .max_by_key(|w| w.idx)
        .filter(|w| w.name.is_none() && w.active_window_id.is_none())
        .ok_or_else(|| anyhow::anyhow!("no empty workspace available on the focused output"))
}

fn set_workspace_name_by_id(
    client: &mut impl NiriClient,
    id: u64,
    full_name: &str,
) -> anyhow::Result<()> {
    send_action_with(
        client,
        Action::SetWorkspaceName {
            name: full_name.to_string(),
            workspace: Some(WorkspaceReferenceArg::Id(id)),
        },
    )
}

/// Focus the workspace for `ch`, or create it as `full_name` when none exists.
///
/// Existing workspaces are found by prefix+char, so titled names match.
/// Returns the created workspace's id, or `None` when an existing one was focused.
fn focus_or_create_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
    full_name: &str,
) -> anyhow::Result<Option<u64>> {
    let workspaces = list_workspaces_with(client)?;

    if let Some(existing_name) = find_workspace_name(&workspaces, prefix, ch) {
        send_action_with(
            client,
            Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(existing_name),
            },
        )?;
        return Ok(None);
    }

    // Focus before naming: the cleanup daemon unsets names of empty
    // unfocused dyn workspaces.
    let target_id = trailing_empty_workspace(&workspaces)?.id;
    send_action_with(
        client,
        Action::FocusWorkspace {
            reference: WorkspaceReferenceArg::Id(target_id),
        },
    )?;
    set_workspace_name_by_id(client, target_id, full_name)?;

    Ok(Some(target_id))
}

/// Switch to the workspace for `ch`; when none exists, create it as
/// `full_name` and spawn `commands` (argument vectors) there.
///
/// Returns the created workspace's id (`None` when an existing one was
/// focused) and, when two or more programs spawned, a [`ReorderRequest`] for
/// [`reorder_workspace_columns`].
pub fn switch_workspace(
    prefix: &str,
    ch: char,
    full_name: &str,
    commands: &[Vec<String>],
) -> anyhow::Result<(Option<u64>, Option<ReorderRequest>)> {
    switch_workspace_with(&mut SocketClient, prefix, ch, full_name, commands)
}

fn switch_workspace_with(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
    full_name: &str,
    commands: &[Vec<String>],
) -> anyhow::Result<(Option<u64>, Option<ReorderRequest>)> {
    let Some(ws_id) = focus_or_create_impl(client, prefix, ch, full_name)? else {
        return Ok((None, None));
    };
    Ok((Some(ws_id), spawn_programs_with(client, ws_id, commands)?))
}

/// Spawn the non-empty `commands` in order for the new workspace `workspace_id`.
///
/// Returns a [`ReorderRequest`] when two or more programs spawned.
fn spawn_programs_with(
    client: &mut impl NiriClient,
    workspace_id: u64,
    commands: &[Vec<String>],
) -> anyhow::Result<Option<ReorderRequest>> {
    let commands: Vec<Vec<String>> = commands.iter().filter(|c| !c.is_empty()).cloned().collect();
    for command in &commands {
        spawn_with(client, command).with_context(|| format!("failed to spawn '{}'", command[0]))?;
    }
    Ok((commands.len() >= 2).then_some(ReorderRequest {
        workspace_id,
        commands,
    }))
}

/// Have niri spawn `command`, an argument vector (no shell involved).
///
/// The window opens on whichever workspace is focused when it maps.
pub(crate) fn spawn_with(client: &mut impl NiriClient, command: &[String]) -> anyhow::Result<()> {
    send_action_with(
        client,
        Action::Spawn {
            command: command.to_vec(),
        },
    )
}

/// Programs spawned on a new workspace, whose columns should follow `commands`.
#[derive(Debug)]
pub struct ReorderRequest {
    workspace_id: u64,
    /// Non-empty argument vectors, in the desired column order.
    commands: Vec<Vec<String>>,
}

/// Poll budgets for [`reorder_workspace_columns`], counted in window listings.
struct ReorderTiming {
    poll_interval: Duration,
    /// Pause after each focus or move so niri applies it before the next.
    action_delay: Duration,
    /// Listings spent waiting for every program's window to appear.
    appear_polls: u32,
    /// Listings in all, including those waiting for the windows to settle.
    total_polls: u32,
}

/// 5 s for the windows to appear, 8 s in all.
const REORDER_TIMING: ReorderTiming = ReorderTiming {
    poll_interval: Duration::from_millis(200),
    action_delay: Duration::from_millis(50),
    appear_polls: 25,
    total_polls: 40,
};

/// Unchanged listings in a row that count as settled: apps like VS Code
/// remap and resize during startup.
const STABLE_POLLS: u32 = 3;

/// A command word without any leading path.
fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Check whether a window's `app_id` matches an executable name.
/// Splits the `app_id` on `.` and checks if any segment equals the executable (case-insensitive).
fn app_id_matches(app_id: &str, exe: &str) -> bool {
    app_id
        .split('.')
        .any(|segment| segment.eq_ignore_ascii_case(exe))
}

/// Whether a command word names the window's app: its basename is a segment
/// of the app id (`firefox`, `org.mozilla.firefox`) or the whole of it
/// (`com.slack.Slack`).
fn word_matches(app_id: &str, word: &str) -> bool {
    // `code ~/dev/` has an empty basename, which would match an empty app id.
    let name = basename(word);
    !name.is_empty() && (app_id_matches(app_id, name) || app_id.eq_ignore_ascii_case(name))
}

/// Whether a word after the first names the window's app, as with the
/// program a wrapper starts: `flatpak run com.slack.Slack`, `uwsm app --
/// kitty`, `env FOO=1 foot`, `sh -c '...; exec foot'`.
fn later_word_matches(command: &[String], app_id: &str) -> bool {
    command
        .iter()
        .skip(1)
        .flat_map(|arg| arg.split_whitespace())
        .any(|word| word_matches(app_id, word))
}

/// Wait for the programs' windows on the new workspace, then move their
/// columns into command order unless the user has left the workspace.
///
/// Blocks for up to 8 s. Best-effort: logs errors to stderr since the overlay
/// is already closed.
pub fn reorder_workspace_columns(request: &ReorderRequest) {
    if let Err(e) = reorder_columns_impl(&mut SocketClient, request, &REORDER_TIMING) {
        eprintln!("warning: failed to reorder columns: {e}");
    }
}

fn new_workspace_windows(windows: &[Window], ws_id: u64) -> impl Iterator<Item = &Window> {
    windows
        .iter()
        .filter(move |w| w.workspace_id == Some(ws_id))
}

fn reorder_columns_impl(
    client: &mut impl NiriClient,
    request: &ReorderRequest,
    timing: &ReorderTiming,
) -> anyhow::Result<()> {
    let windows = settled_windows(client, request.workspace_id, request.commands.len(), timing)?;
    if windows.is_empty() {
        return Ok(());
    }
    let ordered = match_windows(&request.commands, &windows);
    for (command, slot) in request.commands.iter().zip(&ordered) {
        if slot.is_none() {
            eprintln!(
                "warning: no window matched `{}`, leaving its column in place",
                shell_words::join(command)
            );
        }
    }
    apply_column_order(client, request.workspace_id, &ordered, timing.action_delay)
}

fn focused_workspace_id(client: &mut impl NiriClient) -> anyhow::Result<Option<u64>> {
    Ok(list_workspaces_with(client)?
        .iter()
        .find(|w| w.is_focused)
        .map(|w| w.id))
}

fn focused_window_with(client: &mut impl NiriClient) -> anyhow::Result<Option<Window>> {
    match client.send(Request::FocusedWindow)? {
        Response::FocusedWindow(window) => Ok(window),
        other => bail!("unexpected response: {other:?}"),
    }
}

/// Move the column of each `ordered` window to its 1-based slot on
/// workspace `ws_id`, then give focus back to the window that had it.
///
/// Stops as soon as the user is on another workspace: niri can only move the
/// focused column, and focusing a window would pull them back. Skips windows
/// that are gone, floating, or already in their slot.
fn apply_column_order(
    client: &mut impl NiriClient,
    ws_id: u64,
    ordered: &[Option<u64>],
    action_delay: Duration,
) -> anyhow::Result<()> {
    let restore = focused_window_with(client)?
        .filter(|w| w.workspace_id == Some(ws_id))
        .map(|w| w.id);
    let mut focus_moved = false;

    for (i, id) in ordered.iter().enumerate() {
        let Some(id) = *id else { continue };
        // Narrows the race with a user switching away, but cannot close it.
        if focused_workspace_id(client)? != Some(ws_id) {
            return Ok(());
        }
        // Earlier moves shift columns, so read the current layout.
        let column = list_windows_with(client)?
            .iter()
            .find(|w| w.id == id && w.workspace_id == Some(ws_id))
            .and_then(|w| w.layout.pos_in_scrolling_layout)
            .map(|(column, _)| column);
        if column.is_none_or(|column| column == i + 1) {
            continue;
        }

        if let Err(e) = send_action_with(client, Action::FocusWindow { id }) {
            eprintln!("warning: failed to focus window {id}: {e}");
            continue;
        }
        focus_moved = true;
        thread::sleep(action_delay);
        // niri clamps the index to the column count.
        if let Err(e) = send_action_with(client, Action::MoveColumnToIndex { index: i + 1 }) {
            eprintln!("warning: failed to move column to index {}: {e}", i + 1);
        }
        thread::sleep(action_delay);
    }

    if let Some(id) = restore.filter(|_| focus_moved) {
        if focused_workspace_id(client)? == Some(ws_id) {
            send_action_with(client, Action::FocusWindow { id })?;
        }
    }
    Ok(())
}

/// The windows on workspace `ws_id` once `expected` of them appeared (or the
/// appear budget ran out) and the set then stayed unchanged for
/// [`STABLE_POLLS`] listings (or the total budget ran out).
fn settled_windows(
    client: &mut impl NiriClient,
    ws_id: u64,
    expected: usize,
    timing: &ReorderTiming,
) -> anyhow::Result<Vec<Window>> {
    let ids = |windows: &[Window]| -> HashSet<u64> {
        new_workspace_windows(windows, ws_id)
            .map(|w| w.id)
            .collect()
    };

    let mut polls = 0;
    let mut windows = loop {
        let windows = list_windows_with(client)?;
        polls += 1;
        if new_workspace_windows(&windows, ws_id).count() >= expected
            || polls >= timing.appear_polls
        {
            break windows;
        }
        thread::sleep(timing.poll_interval);
    };

    let mut last_ids = ids(&windows);
    let mut stable = 0;
    while stable < STABLE_POLLS && polls < timing.total_polls {
        thread::sleep(timing.poll_interval);
        windows = list_windows_with(client)?;
        polls += 1;
        let current_ids = ids(&windows);
        if current_ids == last_ids {
            stable += 1;
        } else {
            last_ids = current_ids;
            stable = 0;
        }
    }

    Ok(new_workspace_windows(&windows, ws_id).cloned().collect())
}

/// Pair each command with a distinct window by app id, in command order;
/// `None` where no window matches.
///
/// Every executable is matched before any later word, so a direct `kitty`
/// keeps its window from a `uwsm app -- kitty`. Windows without an app id
/// never match.
fn match_windows(commands: &[Vec<String>], windows: &[Window]) -> Vec<Option<u64>> {
    let mut unclaimed: Vec<(u64, &str)> = windows
        .iter()
        .filter_map(|w| Some((w.id, w.app_id.as_deref()?)))
        .collect();
    let mut claim = |matches: &dyn Fn(&str) -> bool| {
        let i = unclaimed.iter().position(|&(_, app_id)| matches(app_id))?;
        Some(unclaimed.remove(i).0)
    };

    let mut slots: Vec<Option<u64>> = commands
        .iter()
        .map(|command| {
            let program = command.first()?;
            claim(&|app_id| word_matches(app_id, program))
        })
        .collect();
    for (slot, command) in slots.iter_mut().zip(commands) {
        if slot.is_none() {
            *slot = claim(&|app_id| later_word_matches(command, app_id));
        }
    }
    slots
}

/// Move a window to an existing workspace by id; `None` moves the focused window.
pub fn move_window_to_workspace_by_id(id: u64, window_id: Option<u64>) -> anyhow::Result<()> {
    send_action(Action::MoveWindowToWorkspace {
        window_id,
        reference: WorkspaceReferenceArg::Id(id),
        focus: true,
    })
}

/// Move a window to a workspace, creating it as `full_name` if it doesn't
/// exist; `None` moves the focused window.
///
/// Returns whether a new workspace was named.
///
/// # Errors
/// When `window_id` is `None` and no window is focused; nothing is created then.
pub fn move_window_to_workspace(
    prefix: &str,
    ch: char,
    full_name: &str,
    window_id: Option<u64>,
) -> anyhow::Result<bool> {
    move_window_impl(&mut SocketClient, prefix, ch, full_name, window_id)
}

fn move_window_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    ch: char,
    full_name: &str,
    window_id: Option<u64>,
) -> anyhow::Result<bool> {
    let workspaces = list_workspaces_with(client)?;
    let window_id = match window_id {
        Some(id) => id,
        // Before naming: niri takes a windowless move as a no-op, leaving an empty named workspace.
        None => focused_window_id(&workspaces)?,
    };

    let existing = find_workspace_name(&workspaces, prefix, ch);
    let created = existing.is_none();
    let reference = if let Some(existing) = existing {
        WorkspaceReferenceArg::Name(existing)
    } else {
        // Naming by id needs no focus change, so the focused window — the one
        // the user intends to move — stays focused throughout. Moving by id
        // keeps working even if a concurrent cleanup unsets the fresh name.
        let target_id = trailing_empty_workspace(&workspaces)?.id;
        set_workspace_name_by_id(client, target_id, full_name)?;
        WorkspaceReferenceArg::Id(target_id)
    };

    send_action_with(
        client,
        Action::MoveWindowToWorkspace {
            window_id: Some(window_id),
            reference,
            focus: true,
        },
    )?;
    Ok(created)
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
    cleanup_empty_workspaces_impl(&mut SocketClient, prefix, Duration::from_millis(500))
}

/// Collect prefix-matching workspaces that are empty, unfocused, and inactive.
fn removable_workspaces(
    client: &mut impl NiriClient,
    prefix: &str,
) -> anyhow::Result<Vec<(u64, String)>> {
    let workspaces = list_workspaces_with(client)?;
    let windows = list_windows_with(client)?;

    let window_ws_ids: HashSet<u64> = windows.iter().filter_map(|w| w.workspace_id).collect();

    Ok(workspaces
        .iter()
        .filter_map(|ws| {
            let name = ws.name.as_ref()?;
            if crate::config::parse_dynamic_name(name, prefix).is_none()
                || ws.is_focused
                || ws.is_active
                || window_ws_ids.contains(&ws.id)
            {
                return None;
            }
            Some((ws.id, name.clone()))
        })
        .collect())
}

fn cleanup_empty_workspaces_impl(
    client: &mut impl NiriClient,
    prefix: &str,
    confirm_delay: Duration,
) -> anyhow::Result<()> {
    // Two passes: a workspace mid-creation is briefly named but still empty
    // and unfocused, so only unset names that qualify again after a delay.
    let candidates: HashSet<u64> = removable_workspaces(client, prefix)?
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }

    thread::sleep(confirm_delay);

    for (id, name) in removable_workspaces(client, prefix)? {
        if !candidates.contains(&id) {
            continue;
        }
        send_action_with(
            client,
            Action::UnsetWorkspaceName {
                reference: Some(WorkspaceReferenceArg::Name(name)),
            },
        )?;
    }

    Ok(())
}

/// Subscribe to niri's event stream and run cleanup when workspaces may become empty.
///
/// `prefix_source` is consulted before each cleanup pass; it returns the
/// current workspace prefix, or `None` to skip cleanup (auto-delete disabled).
///
/// Reconnects automatically if the socket drops (e.g. niri restarts).
pub fn run_event_cleanup(mut prefix_source: impl FnMut() -> Option<String>) {
    loop {
        if let Err(e) = event_cleanup_loop(&mut prefix_source) {
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

/// Trailing-edge debounce: due once events have been quiet for `quiet`, or
/// `max_wait` after the first pending event so a busy stream cannot starve it.
pub(crate) struct Debouncer {
    quiet: Duration,
    max_wait: Duration,
    /// `(first, last)` event times since the last [`Debouncer::clear`].
    pending: Option<(Instant, Instant)>,
}

impl Debouncer {
    pub(crate) const fn new(quiet: Duration, max_wait: Duration) -> Self {
        Self {
            quiet,
            max_wait,
            pending: None,
        }
    }

    pub(crate) fn on_event(&mut self, now: Instant) {
        let first = self.pending.map_or(now, |(first, _)| first);
        self.pending = Some((first, now));
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        self.pending.is_some_and(|(first, last)| {
            now.duration_since(last) >= self.quiet || now.duration_since(first) >= self.max_wait
        })
    }

    pub(crate) fn clear(&mut self) {
        self.pending = None;
    }
}

/// Whether an event can leave a dynamic workspace empty and unfocused.
fn triggers_cleanup(event: &Event) -> bool {
    matches!(
        event,
        Event::WindowOpenedOrChanged { .. }
            | Event::WindowClosed { .. }
            | Event::WindowsChanged { .. }
            | Event::WorkspaceActivated { .. }
            | Event::WorkspacesChanged { .. }
    )
}

fn is_read_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Read the next line of a niri event stream and parse it as an [`Event`].
///
/// Returns `Ok(None)` when the read times out or the line does not parse
/// (e.g. a variant from a newer niri). On a timeout the partial line stays in
/// `buf` for the next call; `buf` holds bytes, so a UTF-8 sequence cut by the
/// timeout survives. `buf` must be empty on the first call.
///
/// # Errors
/// When the stream closes or a read fails with anything but a timeout.
fn read_event(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> anyhow::Result<Option<Event>> {
    match reader.read_until(b'\n', buf) {
        Ok(0) => bail!("niri event stream closed"),
        Ok(_) => {
            let event = serde_json::from_slice(buf).ok();
            buf.clear();
            Ok(event)
        }
        Err(e) if is_read_timeout(&e) => Ok(None),
        Err(e) => Err(e).context("failed to read from niri socket"),
    }
}

fn event_cleanup_loop(prefix_source: &mut impl FnMut() -> Option<String>) -> anyhow::Result<()> {
    let mut reader = connect_event_stream()?;
    // The timeout lets a pending cleanup fire when no further event arrives.
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(250)))
        .context("failed to set read timeout")?;

    let mut debouncer = Debouncer::new(Duration::from_millis(500), Duration::from_secs(2));
    let mut buf = Vec::new();

    loop {
        if read_event(&mut reader, &mut buf)?.is_some_and(|event| triggers_cleanup(&event)) {
            debouncer.on_event(Instant::now());
        }

        if debouncer.due(Instant::now()) {
            debouncer.clear();
            if let Some(prefix) = prefix_source() {
                cleanup_empty_workspaces(&prefix);
            }
        }
    }
}

/// Overlay-relevant compositor changes, coarsened from the niri event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Focus may have moved (workspace activation or window focus change).
    FocusChanged,
    /// Workspaces or windows changed; overlay cards may be stale.
    Structural,
}

fn classify_event(event: &Event) -> Option<OverlayEvent> {
    match event {
        Event::WorkspacesChanged { .. }
        | Event::WorkspaceUrgencyChanged { .. }
        | Event::WindowsChanged { .. }
        | Event::WindowOpenedOrChanged { .. }
        | Event::WindowClosed { .. }
        | Event::WindowUrgencyChanged { .. } => Some(OverlayEvent::Structural),
        Event::WorkspaceActivated { .. } | Event::WindowFocusChanged { .. } => {
            Some(OverlayEvent::FocusChanged)
        }
        _ => None,
    }
}

/// Forward overlay-relevant niri events to `on_event` while `alive` is true.
///
/// Intended for a dedicated thread. Uses a socket read timeout so the loop
/// notices `alive` flipping even when no events arrive, and reconnects if
/// the socket drops.
pub fn run_overlay_event_stream(
    alive: &std::sync::atomic::AtomicBool,
    mut on_event: impl FnMut(OverlayEvent),
) {
    while alive.load(std::sync::atomic::Ordering::Relaxed) {
        match overlay_event_loop(alive, &mut on_event) {
            Ok(()) => break,
            Err(e) => {
                eprintln!(
                    "warning: overlay event stream failed: {e:#}, reconnecting in 1s\u{2026}"
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn overlay_event_loop(
    alive: &std::sync::atomic::AtomicBool,
    on_event: &mut impl FnMut(OverlayEvent),
) -> anyhow::Result<()> {
    let reader = &mut connect_event_stream()?;
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .context("failed to set read timeout")?;

    let mut buf = Vec::new();
    while alive.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(event) = read_event(reader, &mut buf)?
            .as_ref()
            .and_then(classify_event)
        {
            on_event(event);
        }
    }
    Ok(())
}

/// Runs its arguments in order, each in its own `sh -c`, so a hook that
/// exits or fails to parse cannot stop the ones after it.
const HOOK_RUNNER: &str = r#"for hook in "$@"; do sh -c "$hook"; done"#;

/// The argument vector that runs `commands` through [`HOOK_RUNNER`] with
/// `env` set, or `None` when there are no commands.
///
/// niri's `Spawn` takes no environment, so `env` sets it. Every name in
/// `env` must be a valid variable name, so that `env` never reads an entry
/// as an option.
fn hook_spawn_command(commands: &[String], env: &[(String, String)]) -> Option<Vec<String>> {
    if commands.is_empty() {
        return None;
    }
    let mut argv = vec!["env".to_string()];
    argv.extend(env.iter().map(|(name, value)| format!("{name}={value}")));
    argv.extend(["sh", "-c", HOOK_RUNNER, "niri-dynamic-workspaces"].map(String::from));
    argv.extend(commands.iter().cloned());
    Some(argv)
}

fn run_hooks_with(client: &mut impl NiriClient, commands: &[String], env: &[(String, String)]) {
    let Some(command) = hook_spawn_command(commands, env) else {
        return;
    };
    // The workspace action already happened; a hook launch failure must not fail it.
    if let Err(e) = spawn_with(client, &command) {
        eprintln!("warning: failed to launch hooks: {e:#}");
    }
}

/// Have niri run hook `commands` in the background, one after another, each
/// through `sh -c` with `env` set.
///
/// Like `programs`, hooks run in niri's environment and outlive this process;
/// their output is discarded. A launch failure is logged to stderr. No-op if
/// `commands` is empty.
pub fn run_hooks(commands: &[String], env: &[(String, String)]) {
    run_hooks_with(&mut SocketClient, commands, env);
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
    use crate::test_helpers::{test_tiled_window, test_window, test_workspace};

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

        assert_eq!(created, None);
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
    fn focus_or_create_focuses_trailing_empty_by_id_then_names() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
        ]);

        let created = focus_or_create_impl(&mut client, "dyn-", 'a', "dyn-a").unwrap();

        assert_eq!(created, Some(2));
        assert_eq!(client.sent.len(), 3);
        // Focus precedes naming so cleanup never sees it named but unfocused.
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Id(2),
            })
        ));
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::SetWorkspaceName {
                name,
                workspace: Some(WorkspaceReferenceArg::Id(2)),
            }) if name == "dyn-a"
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

    fn commands(argvs: &[&[&str]]) -> Vec<Vec<String>> {
        argvs
            .iter()
            .map(|argv| argv.iter().map(|word| (*word).to_string()).collect())
            .collect()
    }

    fn is_spawn(request: &Request) -> bool {
        matches!(request, Request::Action(Action::Spawn { .. }))
    }

    #[test]
    fn switch_workspace_creates_then_spawns_in_order() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
            Response::Handled,
            Response::Handled,
        ]);

        let (created, reorder) = switch_workspace_with(
            &mut client,
            "dyn-",
            'a',
            "dyn-a",
            &commands(&[&["foot"], &["kitty"]]),
        )
        .unwrap();

        assert_eq!(created, Some(2));
        let reorder = reorder.unwrap();
        assert_eq!(reorder.workspace_id, 2);
        assert_eq!(reorder.commands.len(), 2);
        assert_eq!(client.sent.len(), 5);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Id(2),
            })
        ));
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::SetWorkspaceName {
                workspace: Some(WorkspaceReferenceArg::Id(2)),
                ..
            })
        ));
        assert!(matches!(
            &client.sent[3],
            Request::Action(Action::Spawn { command }) if command == &["foot"]
        ));
        assert!(matches!(
            &client.sent[4],
            Request::Action(Action::Spawn { command }) if command == &["kitty"]
        ));
    }

    #[test]
    fn switch_workspace_existing_spawns_nothing() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![test_workspace(1, Some("dyn-a"), false)]),
            Response::Handled,
        ]);

        let (created, reorder) = switch_workspace_with(
            &mut client,
            "dyn-",
            'a',
            "dyn-a",
            &commands(&[&["foot"], &["kitty"]]),
        )
        .unwrap();

        assert_eq!(created, None);
        assert!(reorder.is_none());
        assert_eq!(client.sent.len(), 2);
        assert!(!client.sent.iter().any(is_spawn));
    }

    #[test]
    fn switch_workspace_one_program_needs_no_reorder() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
            Response::Handled,
        ]);

        let (created, reorder) = switch_workspace_with(
            &mut client,
            "dyn-",
            'a',
            "dyn-a",
            &commands(&[&[], &["foot"]]),
        )
        .unwrap();

        assert_eq!(created, Some(2));
        assert!(reorder.is_none());
        assert_eq!(client.sent.iter().filter(|r| is_spawn(r)).count(), 1);
    }

    /// Appear within 3 listings, 6 in all, and no waiting.
    const TEST_TIMING: ReorderTiming = ReorderTiming {
        poll_interval: Duration::ZERO,
        action_delay: Duration::ZERO,
        appear_polls: 3,
        total_polls: 6,
    };

    /// Reorder `argvs` spawned on workspace 2 against `script`, which it must use up.
    fn reorder(argvs: &[&[&str]], script: Vec<Response>) -> MockClient {
        let mut client = MockClient::new(script);
        let request = ReorderRequest {
            workspace_id: 2,
            commands: commands(argvs),
        };
        reorder_columns_impl(&mut client, &request, &TEST_TIMING).unwrap();
        assert!(
            client.responses.is_empty(),
            "unused: {:?}",
            client.responses
        );
        client
    }

    #[derive(Debug, PartialEq)]
    enum Step {
        Focus(u64),
        Move(usize),
    }

    /// The window focuses and column moves sent, in order.
    fn reorder_steps(sent: &[Request]) -> Vec<Step> {
        sent.iter()
            .filter_map(|request| match request {
                Request::Action(Action::FocusWindow { id }) => Some(Step::Focus(*id)),
                Request::Action(Action::MoveColumnToIndex { index }) => Some(Step::Move(*index)),
                _ => None,
            })
            .collect()
    }

    /// Window listings before the reorder starts acting.
    fn settle_listings(sent: &[Request]) -> usize {
        sent.iter()
            .take_while(|r| !matches!(r, Request::FocusedWindow))
            .filter(|r| matches!(r, Request::Windows))
            .count()
    }

    fn repeat<T: Clone>(item: &T, n: usize) -> impl Iterator<Item = T> + '_ {
        std::iter::repeat_n(item, n).cloned()
    }

    /// Workspace 1 and the new workspace 2, with `focused` focused.
    fn focus_on(focused: u64) -> Response {
        Response::Workspaces(vec![
            test_workspace(1, Some("dyn-b"), focused == 1),
            test_workspace(2, Some("dyn-c"), focused == 2),
        ])
    }

    /// kitty 11 left of foot 10 on workspace 2, plus any `others`.
    fn kitty_then_foot(others: &[Window]) -> Response {
        let mut windows = vec![
            test_tiled_window(11, 2, "kitty", 1),
            test_tiled_window(10, 2, "foot", 2),
        ];
        windows.extend_from_slice(others);
        Response::Windows(windows)
    }

    /// [`kitty_then_foot`] once foot 10 moved to the first column.
    fn foot_then_kitty(others: &[Window]) -> Response {
        let mut windows = vec![
            test_tiled_window(10, 2, "foot", 1),
            test_tiled_window(11, 2, "kitty", 2),
        ];
        windows.extend_from_slice(others);
        Response::Windows(windows)
    }

    #[test]
    fn reorder_moves_windows_into_command_order() {
        let firefox = [test_tiled_window(12, 1, "firefox", 1)];
        let before = kitty_then_foot(&firefox);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&before, 4)
                .chain([
                    Response::FocusedWindow(Some(test_tiled_window(11, 2, "kitty", 1))),
                    focus_on(2),
                    before.clone(),
                    Response::Handled,
                    Response::Handled,
                    focus_on(2),
                    foot_then_kitty(&firefox),
                    focus_on(2),
                    Response::Handled,
                ])
                .collect(),
        );

        assert_eq!(client.sent.len(), 13);
        // Moving foot puts kitty in its slot; kitty then gets its focus back.
        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1), Step::Focus(11)]
        );
    }

    #[test]
    fn reorder_waits_for_a_late_window() {
        let both = kitty_then_foot(&[]);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            std::iter::once(Response::Windows(vec![test_tiled_window(10, 2, "foot", 1)]))
                .chain(repeat(&both, 4))
                .chain([
                    Response::FocusedWindow(None),
                    focus_on(2),
                    both.clone(),
                    Response::Handled,
                    Response::Handled,
                    focus_on(2),
                    foot_then_kitty(&[]),
                ])
                .collect(),
        );

        assert_eq!(settle_listings(&client.sent), 5);
        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1)]
        );
    }

    #[test]
    fn reorder_settles_for_the_windows_that_appeared() {
        let foot = Response::Windows(vec![test_tiled_window(10, 2, "foot", 1)]);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&foot, 6)
                .chain([Response::FocusedWindow(None), focus_on(2), foot.clone()])
                .collect(),
        );

        // The appear budget runs out after 3 listings, then 3 more settle.
        assert_eq!(settle_listings(&client.sent), 6);
        assert_eq!(reorder_steps(&client.sent), []);
    }

    #[test]
    fn reorder_restarts_stability_when_windows_change() {
        let firefox = [test_tiled_window(12, 2, "firefox", 3)];
        let three = kitty_then_foot(&firefox);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&kitty_then_foot(&[]), 2)
                .chain(repeat(&three, 4))
                .chain([
                    Response::FocusedWindow(None),
                    focus_on(2),
                    three.clone(),
                    Response::Handled,
                    Response::Handled,
                    focus_on(2),
                    foot_then_kitty(&firefox),
                ])
                .collect(),
        );

        // Without the reset, the first three listings after the appearance would settle it.
        assert_eq!(settle_listings(&client.sent), 6);
        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1)]
        );
    }

    #[test]
    fn reorder_gives_duplicate_programs_distinct_windows() {
        let windows = Response::Windows(vec![
            test_tiled_window(10, 2, "foot", 2),
            test_tiled_window(11, 2, "foot", 1),
        ]);
        let client = reorder(
            &[&["foot"], &["foot"]],
            repeat(&windows, 4)
                .chain([
                    Response::FocusedWindow(None),
                    focus_on(2),
                    windows.clone(),
                    Response::Handled,
                    Response::Handled,
                    focus_on(2),
                    Response::Windows(vec![
                        test_tiled_window(10, 2, "foot", 1),
                        test_tiled_window(11, 2, "foot", 2),
                    ]),
                ])
                .collect(),
        );

        // Had both matched 10, it would be moved again to slot 2.
        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1)]
        );
    }

    #[test]
    fn reorder_does_nothing_after_user_left() {
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&kitty_then_foot(&[]), 4)
                .chain([
                    Response::FocusedWindow(Some(test_tiled_window(20, 1, "firefox", 1))),
                    focus_on(1),
                ])
                .collect(),
        );

        assert_eq!(reorder_steps(&client.sent), []);
    }

    #[test]
    fn reorder_stops_when_user_leaves_midway() {
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&kitty_then_foot(&[]), 4)
                .chain([
                    Response::FocusedWindow(Some(test_tiled_window(11, 2, "kitty", 1))),
                    focus_on(2),
                    kitty_then_foot(&[]),
                    Response::Handled,
                    Response::Handled,
                    focus_on(1),
                ])
                .collect(),
        );

        // No focus given back either: that would pull the user back.
        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1)]
        );
    }

    #[test]
    fn reorder_skips_windows_already_in_place() {
        let windows = foot_then_kitty(&[]);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&windows, 4)
                .chain([
                    Response::FocusedWindow(Some(test_tiled_window(11, 2, "kitty", 2))),
                    focus_on(2),
                    windows.clone(),
                    focus_on(2),
                    windows.clone(),
                ])
                .collect(),
        );

        assert_eq!(reorder_steps(&client.sent), []);
    }

    #[test]
    fn reorder_skips_floating_windows() {
        let mut floating = test_window(10, 2, "foot");
        floating.is_floating = true;
        let windows = Response::Windows(vec![test_tiled_window(11, 2, "kitty", 1), floating]);
        let client = reorder(
            &[&["kitty"], &["foot"]],
            repeat(&windows, 4)
                .chain([
                    Response::FocusedWindow(None),
                    focus_on(2),
                    windows.clone(),
                    focus_on(2),
                    windows.clone(),
                ])
                .collect(),
        );

        assert_eq!(reorder_steps(&client.sent), []);
    }

    #[test]
    fn reorder_refocuses_the_users_window() {
        let firefox = [test_tiled_window(12, 2, "firefox", 3)];
        let before = kitty_then_foot(&firefox);
        let client = reorder(
            &[&["foot"], &["kitty"]],
            repeat(&before, 4)
                .chain([
                    Response::FocusedWindow(Some(firefox[0].clone())),
                    focus_on(2),
                    before.clone(),
                    Response::Handled,
                    Response::Handled,
                    focus_on(2),
                    foot_then_kitty(&firefox),
                    focus_on(2),
                    Response::Handled,
                ])
                .collect(),
        );

        assert_eq!(
            reorder_steps(&client.sent),
            [Step::Focus(10), Step::Move(1), Step::Focus(12)]
        );
    }

    #[test]
    fn move_window_to_existing_workspace_moves_directly() {
        let mut focused = test_workspace(2, Some("browser"), true);
        focused.active_window_id = Some(100);
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![
                test_workspace(1, Some("dyn-a My Project"), false),
                focused,
            ]),
            Response::Handled,
        ]);

        let created = move_window_impl(&mut client, "dyn-", 'a', "dyn-a", None).unwrap();

        assert!(!created);
        assert_eq!(client.sent.len(), 2);
        assert!(matches!(
            &client.sent[1],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(100),
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

        let created = move_window_impl(&mut client, "dyn-", 'a', "dyn-a", None).unwrap();

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
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(100),
                reference: WorkspaceReferenceArg::Id(2),
                focus: true,
            })
        ));
        // The old implementation focused the new workspace and back — no
        // FocusWorkspace action may appear at all.
        assert!(!client
            .sent
            .iter()
            .any(|r| matches!(r, Request::Action(Action::FocusWorkspace { .. }))));
    }

    #[test]
    fn move_window_without_focused_window_errors_before_naming() {
        let mut workspaces = workspaces_with_trailing_empty();
        workspaces[0].active_window_id = None;
        let mut client = MockClient::new(vec![Response::Workspaces(workspaces)]);

        let err = move_window_impl(&mut client, "dyn-", 'x', "dyn-x", None).unwrap_err();

        assert!(err.to_string().contains("no focused window"), "{err}");
        assert_eq!(client.sent.len(), 1);
    }

    #[test]
    fn move_window_moves_given_window() {
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces_with_trailing_empty()),
            Response::Handled,
            Response::Handled,
        ]);

        move_window_impl(&mut client, "dyn-", 'a', "dyn-a", Some(7)).unwrap();

        assert_eq!(client.sent.len(), 3);
        assert!(matches!(
            &client.sent[2],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(7),
                reference: WorkspaceReferenceArg::Id(2),
                focus: true,
            })
        ));
    }

    #[test]
    fn move_window_given_window_needs_no_focused_one() {
        let mut workspaces = workspaces_with_trailing_empty();
        workspaces[0].active_window_id = None;
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces),
            Response::Handled,
            Response::Handled,
        ]);

        move_window_impl(&mut client, "dyn-", 'a', "dyn-a", Some(7)).unwrap();

        assert_eq!(client.sent.len(), 3);
    }

    #[test]
    fn workspace_id_by_name_ignores_ascii_case() {
        let mut client = MockClient::new(vec![Response::Workspaces(vec![
            test_workspace(4, Some("dyn-a"), false),
            test_workspace(5, Some("Mail"), false),
        ])]);

        assert_eq!(workspace_id_by_name_with(&mut client, "mail").unwrap(), 5);
    }

    #[test]
    fn workspace_id_by_name_errors_when_missing() {
        let mut client = MockClient::new(vec![Response::Workspaces(vec![test_workspace(
            1, None, true,
        )])]);

        let err = workspace_id_by_name_with(&mut client, "mail").unwrap_err();

        assert!(err.to_string().contains("'mail' not found"), "{err}");
        assert_eq!(client.sent.len(), 1);
    }

    #[test]
    fn focused_window_id_is_focused_workspaces_active_window() {
        assert_eq!(
            focused_window_id(&workspaces_with_trailing_empty()).unwrap(),
            100
        );

        let mut no_window = workspaces_with_trailing_empty();
        no_window[0].active_window_id = None;
        assert!(focused_window_id(&no_window).is_err());

        let mut unfocused = workspaces_with_trailing_empty();
        unfocused[0].is_focused = false;
        assert!(focused_window_id(&unfocused).is_err());
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

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_string()).collect()
    }

    fn env_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn hook_spawn_command_is_none_without_hooks() {
        assert_eq!(
            hook_spawn_command(&[], &env_pairs(&[("NDW_WORKSPACE_KEY", "a")])),
            None
        );
    }

    #[test]
    fn hook_spawn_command_sets_env_then_runs_hooks_in_order() {
        let env = env_pairs(&[
            ("NDW_WORKSPACE_NAME", "dyn-a My Project"),
            ("NDW_WORKSPACE_KEY", "a"),
        ]);

        let command = hook_spawn_command(&strings(&["h1", "h2"]), &env).unwrap();

        assert_eq!(
            command,
            strings(&[
                "env",
                "NDW_WORKSPACE_NAME=dyn-a My Project",
                "NDW_WORKSPACE_KEY=a",
                "sh",
                "-c",
                HOOK_RUNNER,
                "niri-dynamic-workspaces",
                "h1",
                "h2",
            ])
        );
    }

    #[test]
    fn hook_runner_isolates_failing_hooks() {
        let out = std::env::temp_dir().join(format!("ndw-hook-runner-{}", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let env = env_pairs(&[("NDW_WORKSPACE_KEY", "a"), ("OUT", out.to_str().unwrap())]);
        let hooks = strings(&[
            r#"echo one >>"$OUT""#,
            "exit 3",
            "echo 'unterminated",
            r#"echo "$NDW_WORKSPACE_KEY" >>"$OUT""#,
        ]);
        let argv = hook_spawn_command(&hooks, &env).unwrap();

        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();

        let written = std::fs::read_to_string(&out);
        let _ = std::fs::remove_file(&out);
        assert_eq!(written.unwrap(), "one\na\n");
    }

    #[test]
    fn run_hooks_with_sends_one_spawn_action() {
        let hooks = strings(&["h1", "h2"]);
        let env = env_pairs(&[("NDW_WORKSPACE_KEY", "a")]);
        let mut client = MockClient::new(vec![Response::Handled]);

        run_hooks_with(&mut client, &hooks, &env);

        assert_eq!(client.sent.len(), 1);
        let expected = hook_spawn_command(&hooks, &env).unwrap();
        assert!(matches!(
            &client.sent[0],
            Request::Action(Action::Spawn { command }) if *command == expected
        ));
    }

    #[test]
    fn run_hooks_with_skips_ipc_without_hooks() {
        let mut client = MockClient::new(vec![]);

        run_hooks_with(&mut client, &[], &env_pairs(&[("NDW_WORKSPACE_KEY", "a")]));

        assert!(client.sent.is_empty());
    }

    #[test]
    fn run_hooks_with_survives_ipc_failure() {
        let mut client = MockClient::new(vec![]);

        run_hooks_with(&mut client, &strings(&["h1"]), &[]);

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

        let workspaces = vec![occupied, focused, active, empty, non_dyn];
        let windows = vec![test_window(100, 1, "firefox")];
        let mut client = MockClient::new(vec![
            Response::Workspaces(workspaces.clone()),
            Response::Windows(windows.clone()),
            Response::Workspaces(workspaces),
            Response::Windows(windows),
            Response::Handled,
        ]);

        cleanup_empty_workspaces_impl(&mut client, "dyn-", Duration::ZERO).unwrap();

        assert_eq!(client.sent.len(), 5);
        assert!(matches!(
            &client.sent[4],
            Request::Action(Action::UnsetWorkspaceName {
                reference: Some(WorkspaceReferenceArg::Name(n)),
            }) if n == "dyn-d"
        ));
    }

    #[test]
    fn cleanup_skips_second_pass_when_no_candidates() {
        let mut occupied = test_workspace(1, Some("dyn-a"), false);
        occupied.active_window_id = Some(100);
        let mut client = MockClient::new(vec![
            Response::Workspaces(vec![occupied]),
            Response::Windows(vec![test_window(100, 1, "firefox")]),
        ]);

        cleanup_empty_workspaces_impl(&mut client, "dyn-", Duration::ZERO).unwrap();

        assert_eq!(client.sent.len(), 2);
    }

    #[test]
    fn cleanup_spares_workspaces_that_change_between_passes() {
        // dyn-a is a candidate in pass 1 but focused by pass 2 (mid-creation
        // race); dyn-b only qualifies in pass 2 (named between passes).
        let pass1 = vec![test_workspace(1, Some("dyn-a"), false)];
        let mut focused_now = test_workspace(1, Some("dyn-a"), true);
        focused_now.active_window_id = Some(100);
        let pass2 = vec![focused_now, test_workspace(2, Some("dyn-b"), false)];

        let mut client = MockClient::new(vec![
            Response::Workspaces(pass1),
            Response::Windows(vec![]),
            Response::Workspaces(pass2),
            Response::Windows(vec![test_window(100, 1, "kitty")]),
        ]);

        cleanup_empty_workspaces_impl(&mut client, "dyn-", Duration::ZERO).unwrap();

        assert_eq!(client.sent.len(), 4);
        assert!(!client
            .sent
            .iter()
            .any(|r| matches!(r, Request::Action(Action::UnsetWorkspaceName { .. }))));
    }

    #[test]
    fn basename_strips_leading_path() {
        assert_eq!(basename("firefox"), "firefox");
        assert_eq!(basename("/usr/bin/firefox"), "firefox");
        assert_eq!(basename("/opt/my apps/firefox"), "firefox");
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
    fn word_matches_segments_and_whole_ids() {
        assert!(word_matches("org.mozilla.firefox", "/usr/bin/firefox"));
        assert!(word_matches("com.slack.Slack", "com.slack.slack"));
        // A flatpak's exported launcher.
        assert!(word_matches(
            "com.slack.Slack",
            "/var/lib/flatpak/exports/bin/com.slack.Slack"
        ));
        assert!(!word_matches("", "/home/me/dev/"));
        assert!(!word_matches("org.mozilla.firefox", "--"));
    }

    #[test]
    fn match_windows_finds_wrapped_programs() {
        let windows = vec![
            test_window(14, 2, "Alacritty"),
            test_window(13, 2, "org.mozilla.firefox"),
            test_window(12, 2, "foot"),
            test_window(11, 2, "kitty"),
            test_window(10, 2, "com.slack.Slack"),
        ];

        let slots = match_windows(
            &commands(&[
                &["flatpak", "run", "com.slack.Slack"],
                &["uwsm", "app", "--", "kitty"],
                &["env", "FOO=1", "foot"],
                &["app2unit", "firefox"],
                &["sh", "-c", "sleep 1; exec alacritty"],
            ]),
            &windows,
        );

        assert_eq!(slots, [Some(10), Some(11), Some(12), Some(13), Some(14)]);
    }

    #[test]
    fn match_windows_prefers_executable_matches() {
        let windows = vec![test_window(10, 2, "kitty"), test_window(11, 2, "kitty")];

        let slots = match_windows(
            &commands(&[&["uwsm", "app", "--", "kitty"], &["kitty"]]),
            &windows,
        );

        assert_eq!(slots, [Some(11), Some(10)]);
    }

    #[test]
    fn match_windows_ignores_empty_basenames_and_missing_app_ids() {
        let mut no_app_id = test_window(10, 2, "");
        no_app_id.app_id = None;
        let windows = vec![no_app_id, test_window(11, 2, "")];

        let slots = match_windows(&commands(&[&["code", "/home/me/dev/"]]), &windows);

        assert_eq!(slots, [None]);
    }

    #[test]
    fn match_windows_leaves_renamed_binaries_unmatched() {
        // A known limit: the executable shares no word with the app id.
        let windows = vec![test_window(10, 2, "org.gnome.TextEditor")];

        let slots = match_windows(&commands(&[&["gnome-text-editor"]]), &windows);

        assert_eq!(slots, [None]);
    }

    #[test]
    fn new_workspace_windows_filters_correctly() {
        let windows = vec![
            test_window(1, 10, "firefox"),
            test_window(2, 10, "kitty"),
            test_window(3, 20, "slack"),
            test_window(4, 10, "code"),
        ];

        let result: Vec<u64> = new_workspace_windows(&windows, 10).map(|w| w.id).collect();

        assert_eq!(result, vec![1, 2, 4]);
    }

    #[test]
    fn new_workspace_windows_empty() {
        let windows = vec![test_window(1, 20, "firefox"), test_window(2, 30, "kitty")];

        let result: Vec<u64> = new_workspace_windows(&windows, 10).map(|w| w.id).collect();

        assert!(result.is_empty());
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

    #[test]
    fn find_workspace_by_char_ignores_lookalike_names() {
        let workspaces = vec![test_workspace(1, Some("dyn-alpha"), false)];
        assert!(find_workspace_by_char(&workspaces, "dyn-", 'a').is_none());
    }

    fn test_debouncer() -> Debouncer {
        Debouncer::new(Duration::from_millis(500), Duration::from_secs(2))
    }

    #[test]
    fn debouncer_idle_is_never_due() {
        let debouncer = test_debouncer();
        assert!(!debouncer.due(Instant::now() + Duration::from_secs(60)));
    }

    #[test]
    fn debouncer_fires_after_quiet_period_without_further_events() {
        let mut debouncer = test_debouncer();
        let t0 = Instant::now();
        debouncer.on_event(t0);

        assert!(!debouncer.due(t0 + Duration::from_millis(499)));
        assert!(debouncer.due(t0 + Duration::from_millis(500)));
    }

    #[test]
    fn debouncer_events_extend_the_quiet_period() {
        let mut debouncer = test_debouncer();
        let t0 = Instant::now();
        debouncer.on_event(t0);
        debouncer.on_event(t0 + Duration::from_millis(400));

        assert!(!debouncer.due(t0 + Duration::from_millis(600)));
        assert!(debouncer.due(t0 + Duration::from_millis(900)));
    }

    #[test]
    fn debouncer_busy_stream_fires_at_max_wait() {
        let mut debouncer = test_debouncer();
        let t0 = Instant::now();
        for i in 0..=20 {
            debouncer.on_event(t0 + Duration::from_millis(i * 100));
        }

        assert!(debouncer.due(t0 + Duration::from_millis(2000)));
    }

    #[test]
    fn debouncer_clear_resets_pending() {
        let mut debouncer = test_debouncer();
        let t0 = Instant::now();
        debouncer.on_event(t0);
        debouncer.clear();

        assert!(!debouncer.due(t0 + Duration::from_secs(10)));
    }

    /// A reader with a short timeout, and the end niri would write to.
    fn event_stream_pair() -> (BufReader<UnixStream>, UnixStream) {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        (BufReader::new(reader), writer)
    }

    fn event_line(event: &Event) -> Vec<u8> {
        let mut line = serde_json::to_vec(event).unwrap();
        line.push(b'\n');
        line
    }

    #[test]
    fn read_event_keeps_a_line_split_mid_codepoint() {
        let (mut reader, mut writer) = event_stream_pair();
        let workspaces = vec![test_workspace(1, Some("dyn-a Caf\u{e9}"), true)];
        let line = event_line(&Event::WorkspacesChanged {
            workspaces: workspaces.clone(),
        });
        // Just after the lead byte of the two-byte 'é'.
        let split = line.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut buf = Vec::new();

        writer.write_all(&line[..split]).unwrap();
        assert!(read_event(&mut reader, &mut buf).unwrap().is_none());

        writer.write_all(&line[split..]).unwrap();
        let event = read_event(&mut reader, &mut buf).unwrap();
        assert!(
            matches!(event, Some(Event::WorkspacesChanged { workspaces: got }) if got == workspaces)
        );

        drop(writer);
        assert!(read_event(&mut reader, &mut buf).is_err());
    }

    #[test]
    fn read_event_skips_unparseable_lines() {
        let (mut reader, mut writer) = event_stream_pair();
        writer.write_all(b"{\"NotAnEvent\":{}}\n").unwrap();
        writer
            .write_all(&event_line(&Event::WorkspaceActivated {
                id: 1,
                focused: true,
            }))
            .unwrap();
        let mut buf = Vec::new();

        assert!(read_event(&mut reader, &mut buf).unwrap().is_none());
        assert!(matches!(
            read_event(&mut reader, &mut buf).unwrap(),
            Some(Event::WorkspaceActivated { id: 1, .. })
        ));
    }
}
