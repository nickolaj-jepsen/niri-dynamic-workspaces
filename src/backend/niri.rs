use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixStream;

use anyhow::{bail, Context};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Event, Request, Response, WorkspaceReferenceArg};

use super::{Backend, WindowInfo, WorkspaceInfo};

pub struct NiriBackend;

impl Backend for NiriBackend {
    fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceInfo>> {
        match send_request(Request::Workspaces)? {
            Response::Workspaces(mut workspaces) => {
                workspaces.sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
                Ok(workspaces.into_iter().map(convert_workspace).collect())
            }
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn list_windows(&self) -> anyhow::Result<Vec<WindowInfo>> {
        match send_request(Request::Windows)? {
            Response::Windows(windows) => Ok(windows.into_iter().map(convert_window).collect()),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn focus_or_create_workspace(&self, name: &str) -> anyhow::Result<bool> {
        let workspaces = self.list_workspaces()?;

        if super::find_workspace_by_name(&workspaces, name).is_some() {
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
            reference: WorkspaceReferenceArg::Index((max_idx + 1) as u8),
        })?;

        send_action(Action::SetWorkspaceName {
            name: name.to_string(),
            workspace: None,
        })?;

        Ok(true)
    }

    fn spawn_program(&self, command: &[String]) -> anyhow::Result<()> {
        send_action(Action::Spawn {
            command: command.to_vec(),
        })
    }

    fn close_window(&self, id: u64) -> anyhow::Result<()> {
        send_action(Action::CloseWindow { id: Some(id) })
    }

    fn focus_window(&self, id: u64) -> anyhow::Result<()> {
        send_action(Action::FocusWindow { id })
    }

    fn move_window_to_position(&self, index: usize) -> anyhow::Result<()> {
        send_action(Action::MoveColumnToIndex { index })
    }

    fn move_window_to_workspace(&self, name: &str) -> anyhow::Result<()> {
        let workspaces = self.list_workspaces()?;

        if super::find_workspace_by_name(&workspaces, name).is_none() {
            let original_ws_id = workspaces
                .iter()
                .find(|w| w.is_focused)
                .map(|w| w.id)
                .ok_or_else(|| anyhow::anyhow!("no focused workspace found"))?;

            self.focus_or_create_workspace(name)?;

            send_action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Id(original_ws_id),
            })?;
        }

        send_action(Action::MoveWindowToWorkspace {
            window_id: None,
            reference: WorkspaceReferenceArg::Name(name.to_string()),
            focus: true,
        })
    }

    fn remove_workspace(&self, name: &str) -> anyhow::Result<()> {
        send_action(Action::UnsetWorkspaceName {
            reference: Some(WorkspaceReferenceArg::Name(name.to_string())),
        })
    }

    fn run_event_cleanup(&self, prefix: &str) {
        super::retry_event_loop(|| event_cleanup_loop(self, prefix));
    }
}

// --- Internal helpers ---

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

fn convert_workspace(ws: niri_ipc::Workspace) -> WorkspaceInfo {
    WorkspaceInfo {
        id: ws.id,
        idx: usize::from(ws.idx),
        name: ws.name,
        output: ws.output,
        is_focused: ws.is_focused,
        is_active: ws.is_active,
    }
}

fn convert_window(w: niri_ipc::Window) -> WindowInfo {
    WindowInfo {
        id: w.id,
        workspace_id: w.workspace_id,
        app_id: w.app_id,
        is_urgent: w.is_urgent,
    }
}

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

fn event_cleanup_loop(backend: &NiriBackend, prefix: &str) -> anyhow::Result<()> {
    let mut reader = connect_event_stream()?;
    let mut cleanup = super::DebouncedCleanup::new();
    let mut buf = String::new();

    loop {
        buf.clear();
        let n = reader
            .read_line(&mut buf)
            .context("failed to read from niri socket")?;
        if n == 0 {
            bail!("niri event stream closed");
        }

        let Ok(event) = serde_json::from_str::<Event>(&buf) else {
            continue;
        };

        match &event {
            Event::WindowOpenedOrChanged { .. }
            | Event::WindowClosed { .. }
            | Event::WindowsChanged { .. }
            | Event::WorkspaceActivated { .. }
            | Event::WorkspacesChanged { .. } => {
                cleanup.mark_pending();
            }
            _ => {}
        }

        cleanup.flush_if_ready(backend, prefix);
    }
}
