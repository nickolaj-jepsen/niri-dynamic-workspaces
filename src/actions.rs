//! Workspace action choreography shared by the CLI and the overlay UI:
//! niri IPC call → on-create/on-delete hooks. Column reordering and delete
//! completion run in the background while the application is held.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Context as _;
use gtk4::gio;
use gtk4::prelude::*;

use crate::config::{self, ResolvedConfig, Select, Template, VariableType};
use crate::niri;

/// Template context passed to on-create hooks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookInfo {
    pub template_name: Option<String>,
    pub variables: HashMap<String, String>,
}

/// How to create a workspace: the arguments of [`switch_workspace`].
#[derive(Debug)]
pub struct CreateRequest {
    pub ws_name: String,
    pub programs: Vec<String>,
    pub hook_info: HookInfo,
}

/// Resolve how the CLI creates the workspace for `ch`: from `template`, or
/// else the key's own template, filled with `vars` (`NAME`, `VALUE` pairs,
/// the last one winning); without either, from the key's programs.
///
/// `title` replaces the template's title, and a blank one leaves the
/// workspace untitled.
///
/// # Errors
/// When `template` does not exist, when `vars` are given without a
/// template, name a variable the template lacks, or miss one it declares.
pub fn resolve_create_request(
    config: &ResolvedConfig,
    ch: char,
    template: Option<&str>,
    vars: &[(String, String)],
    title: Option<&str>,
) -> anyhow::Result<CreateRequest> {
    let title = title.map(str::trim);
    let template = match template {
        Some(name) => Some(find_template(config, name)?),
        None => config.template_for(ch),
    };
    let Some(template) = template else {
        anyhow::ensure!(vars.is_empty(), "--var needs a template");
        return Ok(CreateRequest {
            ws_name: config::workspace_name_with_title(&config.workspace_prefix, ch, title),
            programs: config.programs_for(ch).to_vec(),
            hook_info: HookInfo::default(),
        });
    };
    let name = &template.name;

    let declared: Vec<&str> = template.variables.iter().map(|v| v.name.as_str()).collect();
    let mut values: HashMap<String, String> = HashMap::new();
    for (var, value) in vars {
        let Some(declared) = template.variables.iter().find(|v| v.name == *var) else {
            anyhow::bail!(
                "template '{name}' has no variable '{var}' (it has: {})",
                if declared.is_empty() {
                    "none".to_string()
                } else {
                    declared.join(", ")
                }
            );
        };
        let value = match declared.var_type {
            VariableType::Select(Select::Dirs { .. }) => config::expand_tilde(value),
            _ => value.clone(),
        };
        values.insert(var.clone(), value);
    }
    let missing: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|v| !values.contains_key(*v))
        .collect();
    anyhow::ensure!(
        missing.is_empty(),
        "template '{name}' needs --var for: {}",
        missing.join(", ")
    );

    let title = match title {
        Some(title) => Some(title.to_string()),
        None => {
            config::resolve_workspace_title(template.title.as_deref(), &template.variables, &values)
        }
    };
    Ok(CreateRequest {
        ws_name: config::workspace_name_with_title(&config.workspace_prefix, ch, title.as_deref()),
        programs: template.programs.clone(),
        hook_info: HookInfo {
            template_name: Some(template.name.clone()),
            variables: values,
        },
    })
}

/// The template named `name`; the error lists the configured ones.
fn find_template<'a>(config: &'a ResolvedConfig, name: &str) -> anyhow::Result<&'a Template> {
    if let Some(template) = config.templates.iter().find(|t| t.name == name) {
        return Ok(template);
    }
    let known: Vec<&str> = config.templates.iter().map(|t| t.name.as_str()).collect();
    if known.is_empty() {
        anyhow::bail!("unknown template '{name}': no templates are configured");
    }
    anyhow::bail!("unknown template '{name}' (known: {})", known.join(", "))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Templates for [`resolve_create_request`]: `dev` with a dir variable and
    /// a title, `note` with one text variable, `beta` with none. Key x is
    /// bound to beta and y to note.
    const TEMPLATES: &str = r#"
[workspace.a]
programs = ["firefox"]

[workspace.x]
template = "beta"

[workspace.y]
template = "note"

[template.dev]
programs = ["code {{project}}", "kitty {{branch}}"]
title = "dev: {{project|basename}}"

[template.dev.variables.project]
name = "Project"
type = "dir"
dirs = ["~/dev"]

[template.dev.variables.branch]
name = "Branch"

[template.note]
programs = ["gnome-text-editor {{topic}}"]

[template.note.variables.topic]
name = "Topic"

[template.beta]
programs = ["true"]
title = "BETA"
"#;

    fn request(
        ch: char,
        template: Option<&str>,
        vars: &[(&str, &str)],
        title: Option<&str>,
    ) -> anyhow::Result<CreateRequest> {
        let (config, warnings) = config::resolve_toml(TEMPLATES);
        assert!(warnings.is_empty(), "{warnings:?}");
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        resolve_create_request(&config, ch, template, &vars, title)
    }

    fn error(result: anyhow::Result<CreateRequest>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn create_request_plain_key_uses_key_programs_and_title() {
        let req = request('a', None, &[], None).unwrap();
        assert_eq!(req.ws_name, "dyn-a");
        assert_eq!(req.programs, ["firefox"]);
        assert_eq!(req.hook_info, HookInfo::default());

        let req = request('a', None, &[], Some(" Notes ")).unwrap();
        assert_eq!(req.ws_name, "dyn-a Notes");
        assert!(request('b', None, &[], None).unwrap().programs.is_empty());
    }

    #[test]
    fn create_request_template_resolves_title_programs_and_hook_info() {
        let req = request(
            'p',
            Some("dev"),
            &[
                ("project", "/home/u/old"),
                ("branch", "main"),
                ("project", "/home/u/proj"),
            ],
            None,
        )
        .unwrap();
        assert_eq!(req.ws_name, "dyn-p dev: proj");
        // Placeholders are filled in by switch_workspace.
        assert_eq!(req.programs, ["code {{project}}", "kitty {{branch}}"]);
        assert_eq!(req.hook_info.template_name.as_deref(), Some("dev"));
        assert_eq!(
            req.hook_info.variables,
            HashMap::from([
                ("project".to_string(), "/home/u/proj".to_string()),
                ("branch".to_string(), "main".to_string()),
            ])
        );
    }

    #[test]
    fn create_request_auto_title_from_first_variable() {
        let req = request('n', Some("note"), &[("topic", "groceries")], None).unwrap();
        assert_eq!(req.ws_name, "dyn-n groceries");
    }

    #[test]
    fn create_request_title_override_wins_and_empty_clears() {
        let vars = [("topic", "groceries")];
        let req = request('n', Some("note"), &vars, Some("Shopping")).unwrap();
        assert_eq!(req.ws_name, "dyn-n Shopping");
        let req = request('n', Some("note"), &vars, Some(" ")).unwrap();
        assert_eq!(req.ws_name, "dyn-n");
    }

    #[test]
    fn create_request_template_without_variables_uses_raw_title() {
        let req = request('b', Some("beta"), &[], None).unwrap();
        assert_eq!(req.ws_name, "dyn-b BETA");
        assert_eq!(req.programs, ["true"]);
        assert!(req.hook_info.variables.is_empty());
    }

    #[test]
    fn create_request_unknown_template_lists_known() {
        assert_eq!(
            error(request('a', Some("Dev"), &[], None)),
            "unknown template 'Dev' (known: dev, note, beta)"
        );
        let (config, _) = config::resolve_toml("");
        assert_eq!(
            error(resolve_create_request(&config, 'a', Some("dev"), &[], None)),
            "unknown template 'dev': no templates are configured"
        );
    }

    #[test]
    fn create_request_missing_variable_errors() {
        assert_eq!(
            error(request('p', Some("dev"), &[], None)),
            "template 'dev' needs --var for: project, branch"
        );
        assert_eq!(
            error(request('p', Some("dev"), &[("branch", "main")], None)),
            "template 'dev' needs --var for: project"
        );
    }

    #[test]
    fn create_request_undeclared_variable_errors() {
        assert_eq!(
            error(request('p', Some("note"), &[("project", "x")], None)),
            "template 'note' has no variable 'project' (it has: topic)"
        );
        assert_eq!(
            error(request('b', Some("beta"), &[("x", "y")], None)),
            "template 'beta' has no variable 'x' (it has: none)"
        );
    }

    #[test]
    fn create_request_var_without_template_errors() {
        assert_eq!(
            error(request('a', None, &[("topic", "x")], None)),
            "--var needs a template"
        );
    }

    #[test]
    fn create_request_uses_bound_template() {
        let req = request('x', None, &[], None).unwrap();
        assert_eq!(req.ws_name, "dyn-x BETA");
        assert_eq!(req.programs, ["true"]);
        assert_eq!(req.hook_info.template_name.as_deref(), Some("beta"));
    }

    #[test]
    fn create_request_explicit_template_beats_binding() {
        let req = request('x', Some("note"), &[("topic", "groceries")], None).unwrap();
        assert_eq!(req.ws_name, "dyn-x groceries");
        assert_eq!(req.hook_info.template_name.as_deref(), Some("note"));
    }

    #[test]
    fn create_request_bound_template_with_variables_needs_vars() {
        assert_eq!(
            error(request('y', None, &[], None)),
            "template 'note' needs --var for: topic"
        );
    }

    #[test]
    fn create_request_bound_template_accepts_vars_without_flag() {
        let req = request('y', None, &[("topic", "groceries")], Some("Shop")).unwrap();
        assert_eq!(req.ws_name, "dyn-y Shop");
        assert_eq!(req.hook_info.variables["topic"], "groceries");
    }

    #[test]
    fn create_request_dir_value_expands_tilde() {
        let req = request(
            'p',
            Some("dev"),
            &[("project", "~/dev/app"), ("branch", "~/x")],
            None,
        )
        .unwrap();
        let vars = &req.hook_info.variables;
        assert_eq!(vars["project"], config::expand_tilde("~/dev/app"));
        // Only dir variables name paths.
        assert_eq!(vars["branch"], "~/x");
    }
}
