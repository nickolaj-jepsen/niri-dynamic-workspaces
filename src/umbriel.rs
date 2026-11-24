//! Experimental Umbriel adapter. Native names are declared in a session-owned
//! include; changing a declaration's name would replace the workspace, not rename it.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use crate::backend::{OverlayEvent, Window, Workspace, WorkspaceSelector};
use crate::config::{parse_dynamic_name, workspace_name, workspace_name_with_title};

const IPC_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);
const HEADER: &str = "# niri-dynamic-workspaces: ";

pub struct Client {
    socket: PathBuf,
    include: Option<PathBuf>,
    ids: Mutex<Ids>,
}

/// UI references are copyable handles; the native string (not its position or
/// hash) is the identity. Use separate keys for windows and workspaces.
#[derive(Default)]
struct Ids(indexmap::IndexSet<(bool, String)>);

impl Ids {
    fn id(&mut self, is_window: bool, id: &str) -> u64 {
        self.0.insert_full((is_window, id.to_owned())).0 as u64
    }

    fn matches(&self, id: u64, is_window: bool, native: &str) -> bool {
        self.0
            .get_index(id as usize)
            .is_some_and(|(w, n)| *w == is_window && n == native)
    }
}

#[derive(Clone, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent Umbriel IPC flags"
)]
struct NativeWorkspace {
    id: String,
    name: String,
    named: bool,
    index: u8,
    output: String,
    focused: bool,
    active: bool,
    urgent: bool,
    focused_window: String,
    window_count: usize,
}

#[derive(Deserialize)]
struct NativeWindow {
    id: String,
    workspace: String,
    pinned: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Rule {
    name: String,
    output: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Managed {
    output: String,
    title: Option<String>,
    /// Local working directory for the workspace's processes, which `cwd`
    /// reports back. Left out when unset, so an include written by a version
    /// without it still loads and rewrites identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

type Rules = BTreeMap<String, Managed>;

#[derive(Serialize)]
struct Include {
    workspace: Vec<Rule>,
}

fn render(rules: &Rules) -> anyhow::Result<String> {
    let workspace = rules
        .iter()
        .map(|(name, entry)| Rule {
            name: name.clone(),
            output: entry.output.clone(),
        })
        .collect();
    let header = format!("{HEADER}{}\n", serde_json::to_string(rules)?);
    let include = Include { workspace };
    // An explicit `workspace = []` would wipe rules from earlier includes.
    if include.workspace.is_empty() {
        Ok(header)
    } else {
        Ok(header + &toml::to_string(&include)?)
    }
}

fn parse_include(data: &str) -> anyhow::Result<Rules> {
    if data.is_empty() {
        return Ok(Rules::new());
    }
    let header = data
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(HEADER))
        .context("include is not an app-owned workspace file")?;
    let rules = serde_json::from_str(header)?;
    ensure!(
        render(&rules)? == data,
        "app-owned workspace include was edited externally"
    );
    Ok(rules)
}

fn replace(path: &Path, content: &str) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = File::options().write(true).create_new(true).open(&temp)?;
    let result = file
        .write_all(content.as_bytes())
        .and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("replace {}", path.display()))
}

impl Client {
    pub fn from_env() -> Option<Self> {
        let socket = std::env::var_os("UMBRIEL_SOCKET")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
                let display = std::env::var("WAYLAND_DISPLAY").ok()?;
                Some(dir.join(format!("umbriel-{display}.sock")))
            })?;
        Some(Self {
            socket,
            include: std::env::var_os("NDW_UMBRIEL_INCLUDE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            ids: Mutex::default(),
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn is_running(&self) -> bool {
        self.workspaces().is_ok()
    }

    fn connect(&self, request: &Value) -> anyhow::Result<BufReader<UnixStream>> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect to Umbriel ({})", self.socket.display()))?;
        stream.set_write_timeout(Some(IPC_TIMEOUT))?;
        stream.set_read_timeout(Some(IPC_TIMEOUT))?;
        serde_json::to_writer(&mut stream, request)?;
        stream.write_all(b"\n")?;
        Ok(BufReader::new(stream))
    }

    fn request<T: DeserializeOwned>(&self, request: &Value) -> anyhow::Result<T> {
        let mut line = String::new();
        self.connect(request)?.read_line(&mut line)?;
        let reply: Value = serde_json::from_str(&line).context("invalid Umbriel IPC reply")?;
        if let Some(message) = reply.get("err") {
            bail!("Umbriel: {}", message.as_str().unwrap_or("unknown error"));
        }
        serde_json::from_value(
            reply
                .get("ok")
                .context("missing Umbriel IPC result")?
                .clone(),
        )
        .context("unexpected Umbriel IPC result")
    }

    fn action(&self, action: &str) -> anyhow::Result<()> {
        self.request(&json!({"cmd":"action", "action":action}))
    }

    fn workspaces(&self) -> anyhow::Result<Vec<NativeWorkspace>> {
        self.request(&json!({"cmd":"workspaces"}))
    }

    pub fn list_workspaces(&self) -> anyhow::Result<Vec<Workspace>> {
        let mut workspaces = self.workspaces()?;
        let titles = self
            .include
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|contents| parse_include(&contents).ok())
            .unwrap_or_default();
        // Keep same-named static targets on the focused output first, like the CLI.
        let output = workspaces
            .iter()
            .find(|w| w.focused)
            .map(|w| w.output.clone());
        workspaces.sort_by(|a, b| {
            (Some(&a.output) != output.as_ref(), &a.output, a.index).cmp(&(
                Some(&b.output) != output.as_ref(),
                &b.output,
                b.index,
            ))
        });
        let mut ids = self.ids.lock().unwrap();
        Ok(workspaces
            .into_iter()
            .map(|w| Workspace {
                id: ids.id(false, &w.id),
                idx: w.index,
                name: w.named.then(|| {
                    titles
                        .get(&w.name)
                        .and_then(|m| m.title.as_deref())
                        .map_or_else(|| w.name.clone(), |t| format!("{} {t}", w.name))
                }),
                output: Some(w.output),
                is_focused: w.focused,
                is_active: w.active,
                is_urgent: w.urgent,
                active_window_id: (!w.focused_window.is_empty())
                    .then(|| ids.id(true, &w.focused_window)),
            })
            .collect())
    }

    pub fn list_windows(&self) -> anyhow::Result<Vec<Window>> {
        let windows: Vec<NativeWindow> = self.request(&json!({"cmd":"windows"}))?;
        let mut ids = self.ids.lock().unwrap();
        Ok(windows
            .into_iter()
            .filter(|w| !w.pinned && !w.workspace.is_empty())
            .map(|w| Window {
                workspace_id: Some(ids.id(false, &w.workspace)),
                is_urgent: false, // Umbriel exposes aggregate urgency on the workspace.
            })
            .collect())
    }

    pub fn workspace_id_by_name(&self, name: &str) -> anyhow::Result<u64> {
        let workspaces = self.workspaces()?;
        let mut matches = workspaces.iter().filter(|w| w.named && w.name == name);
        let first = matches
            .next()
            .with_context(|| format!("workspace '{name}' not found"))?;
        let preferred = workspaces.iter().find(|w| w.focused).map(|w| &w.output);
        let ws = if let Some(second) = matches.next() {
            [first, second]
                .into_iter()
                .chain(matches)
                .find(|w| Some(&w.output) == preferred)
                .with_context(|| format!("workspace '{name}' exists on multiple outputs"))?
        } else {
            first
        };
        Ok(self.ids.lock().unwrap().id(false, &ws.id))
    }

    fn workspace_by_id(&self, id: u64) -> anyhow::Result<NativeWorkspace> {
        self.workspaces()?
            .into_iter()
            .find(|w| self.ids.lock().unwrap().matches(id, false, &w.id))
            .context("workspace is no longer available")
    }

    fn workspace_action(&self, action: &str, ws: &NativeWorkspace) -> anyhow::Result<()> {
        let selector = if ws.named {
            json!(ws.name)
        } else {
            json!(ws.index)
        };
        self.request(
            &json!({"cmd":"action", "action":action, "workspace":selector, "output":ws.output}),
        )
    }

    pub fn focus_workspace_by_id(&self, id: u64) -> anyhow::Result<()> {
        self.workspace_action("workspace-switch", &self.workspace_by_id(id)?)
    }

    fn focused_window(&self, expected: Option<u64>) -> anyhow::Result<NativeWindow> {
        let window: NativeWindow = self.request(&json!({"cmd":"focused-window"}))?;
        if let Some(id) = expected {
            ensure!(
                self.ids.lock().unwrap().matches(id, true, &window.id),
                "focused window changed; reopen the grid before moving it"
            );
        }
        Ok(window)
    }

    pub fn move_window_to_workspace_by_id(
        &self,
        id: u64,
        window: Option<u64>,
        follow: bool,
    ) -> anyhow::Result<()> {
        let ws = self.workspace_by_id(id)?;
        self.focused_window(window)?;
        let action = if follow {
            "window-move-to-workspace"
        } else {
            "window-move-to-workspace-silent"
        };
        self.workspace_action(action, &ws)
    }

    pub fn dynamic_workspace_exists(&self, prefix: &str, ch: char) -> anyhow::Result<bool> {
        Ok(find_named(&self.workspaces()?, &workspace_name(prefix, ch))?.is_some())
    }

    pub fn check_move_to_output(&self, prefix: &str, ch: char) -> anyhow::Result<()> {
        let workspaces = self.workspaces()?;
        if let (Some(ws), Some(current)) = (
            find_named(&workspaces, &workspace_name(prefix, ch))?,
            workspaces.iter().find(|w| w.focused),
        ) {
            ensure!(
                ws.output == current.output,
                "moving workspaces between outputs is not supported on Umbriel yet"
            );
        }
        Ok(())
    }

    fn include(&self) -> anyhow::Result<&Path> {
        let path = self
            .include
            .as_deref()
            .context("set up NDW_UMBRIEL_INCLUDE to manage named Umbriel workspaces")?;
        ensure!(path.is_absolute(), "NDW_UMBRIEL_INCLUDE must be absolute");
        ensure!(
            fs::symlink_metadata(path)?.is_file(),
            "NDW_UMBRIEL_INCLUDE must be a regular file"
        );
        Ok(path)
    }

    fn lock(path: &Path) -> anyhow::Result<File> {
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("ndw-lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    fn apply<T>(
        &self,
        path: &Path,
        old: &str,
        rules: &Rules,
        confirm: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        replace(path, &render(rules)?)?;
        match self.action("config-reload").and_then(|()| confirm()) {
            Ok(result) => Ok(result),
            Err(e) => {
                if let Err(rollback) =
                    replace(path, old).and_then(|()| self.action("config-reload"))
                {
                    bail!("{e:#}; restoring the previous include also failed: {rollback:#}");
                }
                Err(e)
            }
        }
    }

    fn create_or_find(
        &self,
        prefix: &str,
        ch: char,
        title: Option<&str>,
        cwd: Option<&str>,
        focus: bool,
    ) -> anyhow::Result<(NativeWorkspace, bool)> {
        let name = workspace_name(prefix, ch);
        let snapshot = self.workspaces()?;
        if let Some(ws) = find_named(&snapshot, &name)? {
            if focus {
                self.workspace_action("workspace-switch", &ws)?;
            }
            return Ok((ws, false));
        }
        let path = self.include()?;
        let _lock = Self::lock(path)?;
        let snapshot = self.workspaces()?;
        if let Some(ws) = find_named(&snapshot, &name)? {
            if focus {
                self.workspace_action("workspace-switch", &ws)?;
            }
            return Ok((ws, false));
        }
        let output = snapshot
            .iter()
            .find(|w| w.focused)
            .context("no focused Umbriel output")?
            .output
            .clone();
        let old = fs::read_to_string(path)?;
        let mut rules = parse_include(&old)?;
        rules.insert(
            name.clone(),
            Managed {
                output: output.clone(),
                title: title.map(str::to_owned),
                cwd: cwd.map(str::to_owned),
            },
        );
        let ws = self.apply(path, &old, &rules, || {
            let ws = wait_for(|| find_named(&self.workspaces()?, &name).map(|ws| ws.filter(|w| w.output == output)))
                .with_context(|| format!("'{name}' did not appear on '{output}'; check the optional include and dynamic output configuration"))?;
            if focus && !ws.focused {
                self.workspace_action("workspace-switch", &ws)?;
                wait_for(|| Ok(self.workspaces()?.into_iter().find(|w| w.id == ws.id && w.focused)))
                    .with_context(|| format!("Umbriel did not focus '{name}'"))?;
            }
            Ok(ws)
        })?;
        Ok((ws, true))
    }

    pub fn switch_workspace(
        &self,
        prefix: &str,
        ch: char,
        title: Option<&str>,
        commands: &[Vec<String>],
        cwd: Option<&str>,
    ) -> anyhow::Result<bool> {
        let (_, created) = self.create_or_find(prefix, ch, title, cwd, true)?;
        if created {
            for command in commands.iter().filter(|c| !c.is_empty()) {
                self.spawn(command)?;
            }
        }
        Ok(created)
    }

    pub fn spawn(&self, argv: &[String]) -> anyhow::Result<()> {
        let command = format!("exec {}", shell_words::join(argv));
        self.request(&json!({"cmd":"action", "action":"spawn", "command":command}))
    }

    pub fn move_window(
        &self,
        prefix: &str,
        ch: char,
        window: Option<u64>,
        follow: bool,
    ) -> anyhow::Result<bool> {
        let focused = self.focused_window(window)?;
        let (ws, created) = self.create_or_find(prefix, ch, None, None, false)?;
        let workspace = self.ids.lock().unwrap().id(false, &ws.id);
        let window = self.ids.lock().unwrap().id(true, &focused.id);
        self.move_window_to_workspace_by_id(workspace, Some(window), follow)?;
        Ok(created)
    }

    pub fn retitle_workspace(
        &self,
        prefix: &str,
        target: WorkspaceSelector,
        title: Option<&str>,
    ) -> anyhow::Result<char> {
        let path = self.include()?;
        let _lock = Self::lock(path)?;
        let workspaces = self.workspaces()?;
        let name = match target {
            WorkspaceSelector::Key(ch) => workspace_name(prefix, ch),
            WorkspaceSelector::Focused => workspaces
                .iter()
                .find(|w| w.focused && w.named)
                .context("focused workspace is not an app-owned dynamic workspace")?
                .name
                .clone(),
        };
        let (ch, None) = parse_dynamic_name(&name, prefix).context("not a dynamic workspace")?
        else {
            bail!("not an app-owned dynamic workspace");
        };
        ensure!(
            find_named(&workspaces, &name)?.is_some(),
            "workspace '{name}' not found"
        );
        let mut rules = parse_include(&fs::read_to_string(path)?)?;
        rules
            .get_mut(&name)
            .context("workspace is not app-owned")?
            .title = title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        // Only the comment changes; Umbriel keeps the native workspace intact.
        replace(path, &render(&rules)?)?;
        Ok(ch)
    }

    /// The directory recorded in the include for the workspace with key `ch`,
    /// if it has one and it is still there.
    pub fn cwd(&self, prefix: &str, ch: char) -> anyhow::Result<Option<PathBuf>> {
        let name = workspace_name(prefix, ch);
        let rules = parse_include(&fs::read_to_string(self.include()?)?)?;
        Ok(rules
            .get(&name)
            .and_then(|entry| entry.cwd.as_deref())
            .map(PathBuf::from)
            .filter(|cwd| cwd.is_absolute() && cwd.is_dir()))
    }

    pub fn retire_empty(&self, prefix: &str, ch: char) -> anyhow::Result<String> {
        let path = self.include()?;
        let _lock = Self::lock(path)?;
        let name = workspace_name(prefix, ch);
        let old = fs::read_to_string(path)?;
        let mut rules = parse_include(&old)?;
        let entry = rules.remove(&name).context("workspace is not app-owned")?;
        let workspaces = self.workspaces()?;
        let ws = find_named(&workspaces, &name)?.context("workspace not found")?;
        ensure!(
            ws.window_count == 0,
            "Umbriel can only delete empty workspaces"
        );
        self.apply(path, &old, &rules, || {
            wait_for(|| {
                Ok(find_named(&self.workspaces()?, &name)?
                    .is_none()
                    .then_some(()))
            })
            .with_context(|| format!("Umbriel did not retire '{name}'"))
        })?;
        Ok(workspace_name_with_title(
            prefix,
            ch,
            entry.title.as_deref(),
        ))
    }

    pub fn run_overlay_event_stream(
        &self,
        alive: &AtomicBool,
        mut on_event: impl FnMut(OverlayEvent),
    ) {
        while alive.load(Ordering::Relaxed) {
            if let Err(e) = self.overlay_events(alive, &mut on_event) {
                eprintln!("warning: Umbriel overlay event stream: {e:#}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }

    fn overlay_events(
        &self,
        alive: &AtomicBool,
        on_event: &mut impl FnMut(OverlayEvent),
    ) -> anyhow::Result<()> {
        let mut reader =
            self.connect(&json!({"cmd":"subscribe", "events":["workspaces", "windows"]}))?;
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(500)))?;
        let mut line = String::new();
        let mut include = self
            .include
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok());
        while alive.load(Ordering::Relaxed) {
            match reader.read_line(&mut line) {
                Ok(0) => bail!("event stream closed"),
                Ok(_) => {
                    line.clear();
                    on_event(OverlayEvent::Structural);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    // An overlay-only rename from a separate CLI has no IPC event.
                    let current = self
                        .include
                        .as_ref()
                        .and_then(|p| fs::read_to_string(p).ok());
                    if current != include {
                        include = current;
                        on_event(OverlayEvent::Structural);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

fn find_named(
    workspaces: &[NativeWorkspace],
    name: &str,
) -> anyhow::Result<Option<NativeWorkspace>> {
    let mut matches = workspaces.iter().filter(|w| w.named && w.name == name);
    let first = matches.next();
    ensure!(
        matches.next().is_none(),
        "workspace name '{name}' is ambiguous across Umbriel outputs"
    );
    Ok(first.cloned())
}

fn wait_for<T>(mut get: impl FnMut() -> anyhow::Result<Option<T>>) -> anyhow::Result<T> {
    let deadline = Instant::now() + IPC_TIMEOUT;
    loop {
        if let Some(value) = get()? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "timed out waiting for Umbriel");
        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_include_keeps_title_out_of_native_name() {
        let rules = Rules::from([(
            "dyn-b".into(),
            Managed {
                output: "DP-2".into(),
                title: Some("A \"title\"\nwith punctuation".into()),
                cwd: None,
            },
        )]);
        let contents = render(&rules).unwrap();
        let toml: toml::Value = toml::from_str(&contents).unwrap();
        assert_eq!(toml["workspace"][0]["name"].as_str(), Some("dyn-b"));
        assert_eq!(toml["workspace"][0]["output"].as_str(), Some("DP-2"));
        assert_eq!(
            parse_include(&contents).unwrap()["dyn-b"].title,
            rules["dyn-b"].title
        );
        assert!(parse_include(&format!("{contents}\n# external edit")).is_err());
        assert!(parse_include("[[workspace]]\nname='user-owned'").is_err());
        assert!(parse_include(&render(&Rules::new()).unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn generated_include_keeps_the_cwd_across_a_reload() {
        let rules = Rules::from([(
            "dyn-b".into(),
            Managed {
                output: "DP-2".into(),
                title: None,
                cwd: Some("/home/u/my proj".into()),
            },
        )]);
        let contents = render(&rules).unwrap();
        let parsed = parse_include(&contents).unwrap();
        assert_eq!(
            parsed["dyn-b"].cwd.as_deref(),
            Some("/home/u/my proj"),
            "{contents}"
        );
        // Rewriting what was read back is a no-op, so retitling keeps the path.
        assert_eq!(render(&parsed).unwrap(), contents);
    }

    #[test]
    fn an_include_from_before_cwd_still_loads() {
        let old = format!(
            "{HEADER}{}\n[[workspace]]\nname = \"dyn-b\"\noutput = \"DP-2\"\n",
            r#"{"dyn-b":{"output":"DP-2","title":null}}"#
        );
        let rules = parse_include(&old).unwrap();
        assert_eq!(rules["dyn-b"].cwd, None);
        // Rewriting it is a no-op, so a reload keeps every rule valid.
        assert_eq!(render(&rules).unwrap(), old);
    }

    #[test]
    fn a_workspace_without_cwd_renders_without_one() {
        let rules = Rules::from([(
            "dyn-b".into(),
            Managed {
                output: "DP-2".into(),
                title: None,
                cwd: None,
            },
        )]);
        let parsed = parse_include(&render(&rules).unwrap()).unwrap();
        assert_eq!(parsed["dyn-b"].cwd, None);
    }

    #[test]
    fn opaque_handles_distinguish_native_id_and_kind() {
        let mut ids = Ids::default();
        let id = ids.id(false, "DP-1:2");
        assert_eq!(ids.id(false, "DP-1:2"), id);
        assert_ne!(ids.id(true, "DP-1:2"), id);
        assert!(ids.matches(id, false, "DP-1:2"));
        assert!(!ids.matches(id, false, "DP-2:2"));
    }
}
