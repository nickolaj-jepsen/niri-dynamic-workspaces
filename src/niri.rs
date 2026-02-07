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

pub fn focus_or_create_workspace(name: &str) -> anyhow::Result<()> {
    let workspaces = list_workspaces()?;

    if workspaces.iter().any(|w| w.name.as_deref() == Some(name)) {
        return send_action(Action::FocusWorkspace {
            reference: WorkspaceReferenceArg::Name(name.to_string()),
        });
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
    })
}

pub fn delete_workspace(name: &str) -> anyhow::Result<()> {
    let workspaces = list_workspaces()?;
    let ws = workspaces
        .iter()
        .find(|w| w.name.as_deref() == Some(name))
        .ok_or_else(|| anyhow::anyhow!("workspace '{name}' not found"))?;
    let ws_id = ws.id;

    // Pick a workspace to move windows to: the focused one, or any other on the same output
    let target_id = workspaces
        .iter()
        .find(|w| w.is_focused && w.id != ws_id)
        .or_else(|| workspaces.iter().find(|w| w.id != ws_id))
        .map(|w| w.id)
        .ok_or_else(|| anyhow::anyhow!("no other workspace to move windows to"))?;

    // Move all windows off this workspace
    let windows = list_windows()?;
    for win in windows.iter().filter(|w| w.workspace_id == Some(ws_id)) {
        send_action(Action::MoveWindowToWorkspace {
            window_id: Some(win.id),
            reference: WorkspaceReferenceArg::Id(target_id),
            focus: false,
        })?;
    }

    // Unset the workspace name so niri cleans it up
    send_action(Action::UnsetWorkspaceName {
        reference: Some(WorkspaceReferenceArg::Name(name.to_string())),
    })
}
