use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

use super::{Backend, WindowInfo, WorkspaceInfo};

pub struct HyprlandBackend;

/// Max moves to reach leftmost column position (exceeds any realistic layout).
const MAX_POSITION_MOVES: usize = 20;

// --- JSON deserialization structs for hyprctl output ---

#[derive(Deserialize)]
struct HyprWorkspace {
    id: i64,
    name: String,
    monitor: String,
}

#[derive(Deserialize)]
struct HyprMonitor {
    focused: bool,
    #[serde(rename = "activeWorkspace")]
    active_workspace: HyprMonitorWorkspace,
}

#[derive(Deserialize)]
struct HyprMonitorWorkspace {
    id: i64,
}

#[derive(Deserialize)]
struct HyprClient {
    address: String,
    workspace: HyprClientWorkspace,
    class: String,
    urgent: bool,
}

#[derive(Deserialize)]
struct HyprClientWorkspace {
    id: i64,
}

// --- Helper functions ---

fn hyprctl(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .context("failed to run hyprctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("hyprctl {args:?} failed: {stderr}");
    }
    String::from_utf8(output.stdout).context("hyprctl output is not valid UTF-8")
}

fn hyprctl_dispatch(dispatch_args: &str) -> anyhow::Result<()> {
    let mut args = vec!["dispatch", "--"];
    args.extend(dispatch_args.split_whitespace());
    hyprctl(&args)?;
    Ok(())
}

/// Parse a hex address string like "0x55a1b2c3" into a u64.
fn parse_address(addr: &str) -> u64 {
    let hex = addr.strip_prefix("0x").unwrap_or(addr);
    u64::from_str_radix(hex, 16).unwrap_or(0)
}

/// Format a u64 as a hex address for hyprctl.
fn format_address(id: u64) -> String {
    format!("0x{id:x}")
}

fn socket2_path() -> anyhow::Result<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE not set")?;
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    Ok(PathBuf::from(runtime_dir)
        .join("hypr")
        .join(sig)
        .join(".socket2.sock"))
}

impl Backend for HyprlandBackend {
    fn list_workspaces(&self) -> anyhow::Result<Vec<WorkspaceInfo>> {
        let ws_json = hyprctl(&["workspaces", "-j"])?;
        let workspaces: Vec<HyprWorkspace> =
            serde_json::from_str(&ws_json).context("failed to parse hyprctl workspaces")?;

        let mon_json = hyprctl(&["monitors", "-j"])?;
        let monitors: Vec<HyprMonitor> =
            serde_json::from_str(&mon_json).context("failed to parse hyprctl monitors")?;

        // Determine which workspace is focused (active on the focused monitor)
        let focused_ws_id = monitors
            .iter()
            .find(|m| m.focused)
            .map(|m| m.active_workspace.id);

        // Active workspace IDs (active on any monitor)
        let active_ws_ids: std::collections::HashSet<i64> =
            monitors.iter().map(|m| m.active_workspace.id).collect();

        let mut infos: Vec<WorkspaceInfo> = workspaces
            .into_iter()
            .enumerate()
            .map(|(idx, ws)| {
                let is_focused = Some(ws.id) == focused_ws_id;
                let is_active = !is_focused && active_ws_ids.contains(&ws.id);
                WorkspaceInfo {
                    #[expect(
                        clippy::cast_sign_loss,
                        reason = "Hyprland workspace IDs are positive in practice"
                    )]
                    id: ws.id as u64,
                    idx: idx + 1,
                    name: Some(ws.name),
                    output: Some(ws.monitor),
                    is_focused,
                    is_active,
                }
            })
            .collect();

        infos.sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
        Ok(infos)
    }

    fn list_windows(&self) -> anyhow::Result<Vec<WindowInfo>> {
        let json = hyprctl(&["clients", "-j"])?;
        let clients: Vec<HyprClient> =
            serde_json::from_str(&json).context("failed to parse hyprctl clients")?;

        Ok(clients
            .into_iter()
            .map(|c| {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "Hyprland workspace IDs are positive in practice"
                )]
                let workspace_id = (c.workspace.id > 0).then_some(c.workspace.id as u64);
                WindowInfo {
                    id: parse_address(&c.address),
                    workspace_id,
                    app_id: (!c.class.is_empty()).then_some(c.class),
                    is_urgent: c.urgent,
                }
            })
            .collect())
    }

    fn focus_or_create_workspace(&self, name: &str) -> anyhow::Result<bool> {
        // Check if workspace already exists
        let workspaces = self.list_workspaces()?;
        let exists = workspaces.iter().any(|w| w.name.as_deref() == Some(name));

        // In Hyprland, `workspace name:X` auto-creates if needed
        hyprctl_dispatch(&format!("workspace name:{name}"))?;

        Ok(!exists)
    }

    fn spawn_program(&self, command: &[String]) -> anyhow::Result<()> {
        let cmd = command.join(" ");
        hyprctl_dispatch(&format!("exec {cmd}"))
    }

    fn close_window(&self, id: u64) -> anyhow::Result<()> {
        hyprctl_dispatch(&format!("closewindow address:{}", format_address(id)))
    }

    fn focus_window(&self, id: u64) -> anyhow::Result<()> {
        hyprctl_dispatch(&format!("focuswindow address:{}", format_address(id)))
    }

    fn move_window_to_position(&self, index: usize) -> anyhow::Result<()> {
        // Hyprland lacks a direct "move to column index" command, so we
        // move all the way left first, then move right to the target.
        for _ in 0..MAX_POSITION_MOVES {
            hyprctl_dispatch("movewindoworgroup l")?;
        }
        for _ in 0..index.saturating_sub(1) {
            hyprctl_dispatch("movewindoworgroup r")?;
        }
        Ok(())
    }

    fn move_window_to_workspace(&self, name: &str) -> anyhow::Result<()> {
        hyprctl_dispatch(&format!("movetoworkspace name:{name}"))
    }

    fn remove_workspace(&self, name: &str) -> anyhow::Result<()> {
        // In Hyprland, empty workspaces auto-delete. If it still exists, try to
        // destroy it explicitly (available in newer Hyprland versions).
        let workspaces = self.list_workspaces()?;
        if workspaces.iter().any(|w| w.name.as_deref() == Some(name)) {
            // destroyworkspace is best-effort — ignore errors
            let _ = hyprctl(&["dispatch", "destroyworkspace", &format!("name:{name}")]);
        }
        Ok(())
    }

    fn run_event_cleanup(&self, prefix: &str) {
        super::retry_event_loop(|| event_cleanup_loop(self, prefix));
    }
}

fn event_cleanup_loop(backend: &HyprlandBackend, prefix: &str) -> anyhow::Result<()> {
    let path = socket2_path()?;
    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "failed to connect to Hyprland socket2 at {}",
            path.display()
        )
    })?;
    let reader = BufReader::new(stream);
    let mut cleanup = super::DebouncedCleanup::new();

    for line in reader.lines() {
        let line = line.context("failed to read from Hyprland socket2")?;

        // Hyprland socket2 events are formatted as "EVENT>>DATA"
        let event_type = line.split(">>").next().unwrap_or("");

        match event_type {
            "workspace" | "openwindow" | "closewindow" | "movewindow" | "destroyworkspace" => {
                cleanup.mark_pending();
            }
            _ => {}
        }

        cleanup.flush_if_ready(backend, prefix);
    }

    bail!("Hyprland event stream closed");
}
