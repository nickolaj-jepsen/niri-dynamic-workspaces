//! Workspace action choreography shared by the CLI and the overlay UI:
//! niri IPC call → on-create/on-delete hooks. Column reordering and delete
//! completion run in the background while the application is held.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Context as _;
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
/// `programs` are command strings; `{{name}}` placeholders are filled from
/// `hook_info.variables`. A malformed command fails before anything is created.
/// A new workspace with programs is spared from cleanup for
/// [`niri::SPAWN_GRACE`].
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
    let commands = programs
        .iter()
        .map(|program| {
            config::build_argv(program, &hook_info.variables)
                .with_context(|| format!("failed to parse command '{program}'"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (created, reorder) =
        niri::switch_workspace(&config.workspace_prefix, ch, ws_name, &commands)?;
    if let Some(request) = reorder {
        spawn_reorder(app, request);
    }
    if let Some(id) = created {
        // It stays empty until a program maps a window, which the daemon's
        // cleanup would take for abandoned once the user switches away.
        if commands.iter().any(|command| !command.is_empty()) {
            niri::spare_workspace(id, Instant::now() + niri::SPAWN_GRACE);
        }
        run_create_hooks(config, ch, ws_name, hook_info);
    }
    Ok(())
}

/// Run the global on-create hooks, then those of `hook_info`'s template.
fn run_create_hooks(config: &ResolvedConfig, ch: char, ws_name: &str, hook_info: &HookInfo) {
    let hooks = config::collect_create_hooks(config, hook_info.template_name.as_deref());
    let env = config::build_hook_env(
        ws_name,
        ch,
        hook_info.template_name.as_deref(),
        &hook_info.variables,
    );
    niri::run_hooks(&hooks, &env);
}

/// Delete the workspace for `ch`: ask its windows to close, then, on a
/// background thread once they have, unset its name and run the on-delete
/// hooks with that name.
///
/// Errors from finding the workspace or asking its windows to close are
/// returned directly. `on_done` gets the background outcome on the main
/// thread, while the application is still held: a window still open after
/// a few seconds fails it, and the workspace keeps its name without running
/// hooks.
pub fn delete_workspace(
    app: &gtk4::Application,
    config: &ResolvedConfig,
    ch: char,
    on_done: impl FnOnce(anyhow::Result<()>) + 'static,
) -> anyhow::Result<()> {
    let pending = niri::begin_delete(&config.workspace_prefix, ch)?;
    let on_delete = config.hooks.on_delete.clone();
    let finish = move || {
        if niri::finish_delete(&pending)? {
            let env = config::build_hook_env(&pending.name, ch, None, &HashMap::new());
            niri::run_hooks(&on_delete, &env);
        }
        Ok(())
    };
    // Held like spawn_reorder: a CLI caller waits for the outcome.
    let guard = app.hold();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(finish)
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("workspace delete panicked")));
        on_done(result);
        drop(guard);
    });
    Ok(())
}

/// Move a window (`None`: the focused one) to a workspace, creating it if
/// needed, and run the global on-create hooks when it was created.
///
/// No programs are spawned: the moved window is the workspace's content.
/// Errors, creating nothing, when `window_id` is `None` and no window is focused.
pub fn move_window(
    config: &ResolvedConfig,
    ch: char,
    ws_name: &str,
    window_id: Option<u64>,
) -> anyhow::Result<()> {
    if niri::move_window_to_workspace(&config.workspace_prefix, ch, ws_name, window_id)? {
        run_create_hooks(config, ch, ws_name, &HookInfo::default());
    }
    Ok(())
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
