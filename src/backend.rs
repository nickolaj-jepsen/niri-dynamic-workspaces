//! Compositor-neutral operations used by the overlay and CLI.
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, OnceLock};

use anyhow::{bail, Context};

use crate::config;
use crate::state;
use crate::{niri, umbriel};

/// The workspace a command acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceSelector {
    /// The dynamic workspace with this key.
    Key(char),
    /// The focused workspace, which must be dynamic.
    Focused,
}

/// Overlay-relevant compositor changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Focus may have moved (workspace activation or window focus change).
    FocusChanged,
    /// Workspaces or windows changed; overlay cards may be stale.
    Structural,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Choice {
    #[default]
    Auto,
    Niri,
    Umbriel,
}

static UMBRIEL: OnceLock<umbriel::Client> = OnceLock::new();

/// Resolve against the invoking environment, before GTK can forward commands.
/// Only Umbriel needs a per-socket application ID; keep Niri's existing ID.
pub fn select(choice: Choice, app_id: &mut String) -> anyhow::Result<()> {
    if choice == Choice::Niri {
        return Ok(());
    }
    let client = umbriel::Client::from_env();
    if choice == Choice::Auto {
        let umbriel = client.as_ref().is_some_and(umbriel::Client::is_running);
        if !umbriel {
            return Ok(()); // Keep Niri's diagnostics when its socket is missing.
        }
        if niri::list_workspaces().is_ok() {
            bail!("both Niri and Umbriel IPC are reachable; select --compositor explicitly");
        }
    }
    let client = client.context("set UMBRIEL_SOCKET or run inside an Umbriel session")?;
    let mut hash = DefaultHasher::new();
    client.socket().hash(&mut hash);
    let _ = write!(app_id, ".Umbriel{:x}", hash.finish());
    assert!(UMBRIEL.set(client).is_ok(), "compositor already selected");
    Ok(())
}

pub fn umbriel() -> Option<&'static umbriel::Client> {
    UMBRIEL.get()
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub id: u64,
    pub idx: u8,
    pub name: Option<String>,
    pub output: Option<String>,
    pub is_focused: bool,
    pub is_active: bool,
    pub is_urgent: bool,
    pub active_window_id: Option<u64>,
}

impl From<niri_ipc::Workspace> for Workspace {
    fn from(ws: niri_ipc::Workspace) -> Self {
        Self {
            id: ws.id,
            idx: ws.idx,
            name: ws.name,
            output: ws.output,
            is_focused: ws.is_focused,
            is_active: ws.is_active,
            is_urgent: ws.is_urgent,
            active_window_id: ws.active_window_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Window {
    pub workspace_id: Option<u64>,
    pub is_urgent: bool,
}

impl From<niri_ipc::Window> for Window {
    fn from(w: niri_ipc::Window) -> Self {
        Self {
            workspace_id: w.workspace_id,
            is_urgent: w.is_urgent,
        }
    }
}

pub fn list_workspaces() -> anyhow::Result<Vec<Workspace>> {
    umbriel().map_or_else(
        || {
            Ok(niri::list_workspaces()?
                .into_iter()
                .map(Workspace::from)
                .collect())
        },
        umbriel::Client::list_workspaces,
    )
}

pub fn list_windows() -> anyhow::Result<Vec<Window>> {
    umbriel().map_or_else(
        || {
            Ok(niri::list_windows()?
                .into_iter()
                .map(Window::from)
                .collect())
        },
        umbriel::Client::list_windows,
    )
}

pub fn same_workspace_name(a: &str, b: &str) -> bool {
    if umbriel().is_some() {
        a == b
    } else {
        niri::same_workspace_name(a, b)
    }
}

pub fn workspace_id_by_name(name: &str) -> anyhow::Result<u64> {
    umbriel().map_or_else(
        || niri::workspace_id_by_name(name),
        |u| u.workspace_id_by_name(name),
    )
}

pub fn focus_workspace_by_id(id: u64) -> anyhow::Result<()> {
    umbriel().map_or_else(
        || niri::focus_workspace_by_id(id),
        |u| u.focus_workspace_by_id(id),
    )
}

pub fn move_window_to_workspace_by_id(
    id: u64,
    window: Option<u64>,
    follow: bool,
) -> anyhow::Result<()> {
    umbriel().map_or_else(
        || niri::move_window_to_workspace_by_id(id, window, follow),
        |u| u.move_window_to_workspace_by_id(id, window, follow),
    )
}

pub fn dynamic_workspace_exists(prefix: &str, ch: char) -> anyhow::Result<bool> {
    umbriel().map_or_else(
        || niri::dynamic_workspace_exists(prefix, ch),
        |u| u.dynamic_workspace_exists(prefix, ch),
    )
}

pub fn move_workspace_to_focused_output(prefix: &str, ch: char) -> anyhow::Result<()> {
    umbriel().map_or_else(
        || niri::move_workspace_to_focused_output(prefix, ch),
        |u| u.check_move_to_output(prefix, ch),
    )
}

pub fn retitle_workspace(
    prefix: &str,
    target: WorkspaceSelector,
    title: Option<&str>,
) -> anyhow::Result<char> {
    if let Some(client) = umbriel() {
        return client.retitle_workspace(prefix, target, title);
    }
    // The recorded directory belongs to the workspace, not its title.
    let before = retitled_name(prefix, target)?;
    let ch = niri::retitle_workspace(prefix, target, title)?;
    if let Some(old) = before {
        let new = config::workspace_name_with_title(
            prefix,
            ch,
            title.map(str::trim).filter(|t| !t.is_empty()),
        );
        state::retitle(ch, &old, &new);
    }
    Ok(ch)
}

/// The current full name of the workspace `target`, before renaming it.
fn retitled_name(prefix: &str, target: WorkspaceSelector) -> anyhow::Result<Option<String>> {
    let workspaces = niri::list_workspaces()?;
    Ok(match target {
        WorkspaceSelector::Key(ch) => workspaces
            .iter()
            .find(|w| {
                w.name
                    .as_deref()
                    .and_then(|n| config::parse_dynamic_name(n, prefix))
                    .is_some_and(|(key, _)| key == ch)
            })
            .and_then(|w| w.name.clone()),
        WorkspaceSelector::Focused => workspaces
            .iter()
            .find(|w| w.is_focused)
            .and_then(|w| w.name.clone()),
    })
}

/// The local working directory recorded for a dynamic workspace: the one with
/// key `ch`, or the focused one without it. `None` when it has none, and an
/// error when there is no such workspace or it is not dynamic.
pub fn workspace_cwd(prefix: &str, ch: Option<char>) -> anyhow::Result<Option<PathBuf>> {
    let name = list_workspaces()?
        .into_iter()
        .find(|ws| match ch {
            Some(ch) => ws.name.as_deref().is_some_and(|name| {
                config::parse_dynamic_name(name, prefix).is_some_and(|(key, _)| key == ch)
            }),
            None => ws.is_focused,
        })
        .with_context(|| match ch {
            Some(ch) => format!("workspace '{prefix}{ch}' not found"),
            None => "no focused workspace".to_string(),
        })?
        .name
        .with_context(|| match ch {
            Some(ch) => format!("workspace '{prefix}{ch}' is not a dynamic workspace"),
            None => "the focused workspace is not a dynamic workspace".to_string(),
        })?;
    let Some((ch, _)) = config::parse_dynamic_name(&name, prefix) else {
        bail!("workspace '{name}' is not a dynamic workspace");
    };
    if let Some(client) = umbriel() {
        return client.cwd(prefix, ch);
    }
    Ok(state::lookup(ch, &name))
}

/// Record the local working directory of a workspace that was just created,
/// clearing any earlier entry of the same name.
pub fn remember_cwd(key: char, name: &str, cwd: Option<&str>) -> anyhow::Result<()> {
    // The Umbriel backend keeps it in its include's rules, written on creation.
    if umbriel().is_some() {
        return Ok(());
    }
    state::remember(key, name, cwd)
}

/// Forget the recorded directory of the workspace `name`, whose key is `key`,
/// once it no longer exists.
pub fn forget_cwd(key: char, name: &str) {
    if umbriel().is_none() {
        state::forget(key, name);
    }
}

pub fn set_overlay_origin(id: Option<u64>) {
    if umbriel().is_none() {
        niri::set_overlay_origin(id);
    }
}

pub fn run_overlay_event_stream(alive: &AtomicBool, callback: impl FnMut(OverlayEvent)) {
    if let Some(client) = umbriel() {
        client.run_overlay_event_stream(alive, callback);
    } else {
        niri::run_overlay_event_stream(alive, callback);
    }
}

pub fn run_hooks(hooks: &[String], env: &[(String, String)]) {
    if let Some(client) = umbriel() {
        if let Some(command) = niri::hook_spawn_command(hooks, env) {
            if let Err(e) = client.spawn(&command) {
                eprintln!("warning: failed to launch hooks: {e:#}");
            }
        }
    } else {
        niri::run_hooks(hooks, env);
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::{Window, Workspace};

    pub fn test_workspace(id: u64, name: Option<&str>, focused: bool) -> Workspace {
        crate::test_helpers::test_workspace(id, name, focused).into()
    }

    pub fn test_window(id: u64, workspace_id: u64, app_id: &str) -> Window {
        crate::test_helpers::test_window(id, workspace_id, app_id).into()
    }
}
