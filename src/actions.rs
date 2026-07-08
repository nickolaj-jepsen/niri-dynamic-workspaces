//! Workspace action choreography shared by the CLI and the overlay UI:
//! niri IPC call → on-create/on-delete hooks → column reordering.

use std::collections::HashMap;

use gtk4::gio;
use gtk4::prelude::*;

use crate::config::{self, ResolvedConfig};
use crate::niri;

/// Template context passed to on-create hooks.
#[derive(Clone, Default)]
pub struct HookInfo {
    pub template_name: Option<String>,
    pub variables: HashMap<String, String>,
}

/// Switch to a workspace (creating it if needed), spawn its programs, and run
/// on-create hooks when a new workspace was made.
///
/// Column reordering (needed when 2+ programs spawn) runs on a background
/// thread; see [`spawn_reorder`].
pub fn switch_workspace(
    app: &gtk4::Application,
    config: &ResolvedConfig,
    ch: char,
    ws_name: &str,
    programs: &[String],
    hook_info: &HookInfo,
) -> anyhow::Result<()> {
    let (created, reorder) =
        niri::switch_workspace(&config.workspace_prefix, ch, ws_name, programs)?;
    if let Some(request) = reorder {
        spawn_reorder(app, request);
    }
    if created {
        let hooks = config::collect_create_hooks(config, hook_info.template_name.as_deref());
        let env = config::build_hook_env(
            ws_name,
            ch,
            hook_info.template_name.as_deref(),
            &hook_info.variables,
        );
        niri::run_hooks(&hooks, &env);
    }
    Ok(())
}

/// Delete a workspace and run on-delete hooks on success.
pub fn delete_workspace(config: &ResolvedConfig, ch: char, ws_name: &str) -> anyhow::Result<()> {
    niri::delete_workspace(&config.workspace_prefix, ch)?;
    let env = config::build_hook_env(ws_name, ch, None, &HashMap::new());
    niri::run_hooks(&config.hooks.on_delete, &env);
    Ok(())
}

/// Move the focused window to a workspace, creating it if needed.
pub fn move_window(config: &ResolvedConfig, ch: char, ws_name: &str) -> anyhow::Result<()> {
    niri::move_window_to_workspace(&config.workspace_prefix, ch, ws_name)
}

/// Run column reordering on a blocking thread, holding the application alive
/// until it settles.
///
/// The hold guard prevents two failure modes: without it, a non-daemon
/// process exits when the overlay closes (killing the reorder mid-poll), and
/// running the reorder inline would block the daemon's main loop for the
/// duration of the window polling.
fn spawn_reorder(app: &gtk4::Application, request: niri::ReorderRequest) {
    let guard = app.hold();
    glib::spawn_future_local(async move {
        let _ = gio::spawn_blocking(move || niri::reorder_workspace_columns(&request)).await;
        drop(guard);
    });
}
