//! Session state the compositor has nowhere to keep: each dynamic workspace's
//! configured local working directory.
//!
//! The workspace's processes do not answer this either: the original terminal
//! may have exited and a shell inside it may have changed directory. So the
//! directory is recorded when the workspace is created, keyed by its workspace
//! key, and read back by `cwd`. One small file per key keeps concurrent
//! writers (the daemon and a forwarded CLI, say) from racing on one document.
//!
//! Each entry also stores the workspace's full name and the compositor socket
//! that recorded it, so a key reused with another title, or by a later Niri
//! run, never matches an older entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Entry {
    /// The compositor session that recorded it: Niri's socket changes with
    /// every run, so entries from an earlier session never match.
    session: String,
    /// Full workspace name, title included.
    name: String,
    cwd: String,
}

/// Identifies this compositor run. Its socket path carries the compositor's
/// pid, so it differs between runs.
fn session() -> String {
    std::env::var(niri_ipc::socket::SOCKET_PATH_ENV).unwrap_or_default()
}

/// The directory holding one entry file per workspace key.
fn dir() -> Option<PathBuf> {
    dirs::state_dir().map(|dir| dir.join("niri-dynamic-workspaces").join("cwd"))
}

fn entry_path(dir: &Path, key: char) -> PathBuf {
    dir.join(format!("{key}.json"))
}

fn remove(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other.with_context(|| format!("remove {}", path.display())),
    }
}

/// Record the local working directory of the workspace `name`, whose key is
/// `key`; `None` clears any earlier one, so a recreated workspace never
/// inherits the directory of the one it replaced.
pub fn remember(key: char, name: &str, cwd: Option<&str>) -> anyhow::Result<()> {
    let dir = dir().context("could not determine the state directory")?;
    remember_in(&dir, &session(), key, name, cwd)
}

/// Keep a renamed workspace's entry, so `cwd` still finds its directory.
pub fn retitle(key: char, old: &str, new: &str) {
    let Some(dir) = dir() else {
        return;
    };
    let Ok(data) = fs::read_to_string(entry_path(&dir, key)) else {
        return;
    };
    let Ok(mut entry) = serde_json::from_str::<Entry>(&data) else {
        return;
    };
    // The key may belong to another workspace by now.
    if !entry.name.eq_ignore_ascii_case(old) {
        return;
    }
    entry.name = new.to_string();
    if let Err(e) = write_in(&dir, key, &entry) {
        eprintln!("warning: {e:#}");
    }
}

/// Forget the workspace `name`, whose key is `key`, once it no longer exists.
///
/// Only its own entry goes: a workspace created on the same key meanwhile
/// keeps its directory.
pub fn forget(key: char, name: &str) {
    if let Some(dir) = dir() {
        forget_in(&dir, key, name);
    }
}

fn forget_in(dir: &Path, key: char, name: &str) {
    let path = entry_path(dir, key);
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(entry) = serde_json::from_str::<Entry>(&data) {
            if !entry.name.eq_ignore_ascii_case(name) {
                return;
            }
        }
    }
    if let Err(e) = remove(&path) {
        eprintln!("warning: {e:#}");
    }
}

/// The directory recorded for the workspace `name` with key `key`, if an
/// entry matches it and the directory is still there.
pub fn lookup(key: char, name: &str) -> Option<PathBuf> {
    lookup_in(&dir()?, &session(), key, name)
}

fn remember_in(
    dir: &Path,
    session: &str,
    key: char,
    name: &str,
    cwd: Option<&str>,
) -> anyhow::Result<()> {
    let Some(cwd) = cwd else {
        return remove(&entry_path(dir, key));
    };
    let entry = Entry {
        session: session.to_string(),
        name: name.to_string(),
        cwd: cwd.to_string(),
    };
    write_in(dir, key, &entry)
}

fn write_in(dir: &Path, key: char, entry: &Entry) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = entry_path(dir, key);
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let content = serde_json::to_vec(entry)?;
    let result = fs::write(&temp, content).and_then(|()| fs::rename(&temp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("write {}", path.display()))
}

fn lookup_in(dir: &Path, session: &str, key: char, name: &str) -> Option<PathBuf> {
    let data = fs::read_to_string(entry_path(dir, key)).ok()?;
    let entry: Entry = serde_json::from_str(&data).ok()?;
    if entry.session != session {
        return None;
    }
    // niri matches names case-insensitively, and a rename may only change case.
    if !entry.name.eq_ignore_ascii_case(name) {
        return None;
    }
    let cwd = PathBuf::from(entry.cwd);
    (cwd.is_absolute() && cwd.is_dir()).then_some(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "/run/user/1000/niri.test.sock";

    /// A private directory per test: they run in parallel.
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ndw-state-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn workspace_dir(dir: &Path) -> PathBuf {
        let path = dir.join("a project");
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn remember_and_lookup_roundtrip() {
        let dir = temp_dir("roundtrip");
        let cwd = workspace_dir(&dir);
        remember_in(
            &dir,
            SESSION,
            'a',
            "dyn-a a project",
            Some(cwd.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a a project"), Some(cwd));
        // The title is part of the identity.
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lookup_requires_the_directory_to_exist() {
        let dir = temp_dir("gone");
        let cwd = workspace_dir(&dir);
        remember_in(&dir, SESSION, 'a', "dyn-a", Some(cwd.to_str().unwrap())).unwrap();
        fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lookup_without_entry_is_none() {
        let dir = temp_dir("empty");
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
    }

    #[test]
    fn remember_without_cwd_clears_a_reused_key() {
        let dir = temp_dir("clears");
        let cwd = workspace_dir(&dir);
        remember_in(&dir, SESSION, 'a', "dyn-a", Some(cwd.to_str().unwrap())).unwrap();
        remember_in(&dir, SESSION, 'a', "dyn-a", None).unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        assert!(!entry_path(&dir, 'a').exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn forget_removes_only_the_named_workspace() {
        let dir = temp_dir("forget");
        let cwd = workspace_dir(&dir);
        remember_in(&dir, SESSION, 'a', "dyn-a", Some(cwd.to_str().unwrap())).unwrap();
        // A later workspace on the same key keeps its entry.
        forget_in(&dir, 'a', "dyn-a Other");
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), Some(cwd.clone()));
        forget_in(&dir, 'a', "dyn-a");
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        // Forgetting a key without one is not an error.
        forget_in(&dir, 'b', "dyn-b");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_entry_from_another_session_does_not_match() {
        let dir = temp_dir("session");
        let cwd = workspace_dir(&dir);
        remember_in(
            &dir,
            "/run/user/1000/niri.old.sock",
            'a',
            "dyn-a",
            Some(cwd.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lookup_matches_the_name_case_insensitively() {
        let dir = temp_dir("case");
        let cwd = workspace_dir(&dir);
        remember_in(&dir, SESSION, 'a', "dyn-a", Some(cwd.to_str().unwrap())).unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "DYN-A"), Some(cwd));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn retitle_leaves_another_workspaces_entry_alone() {
        let dir = temp_dir("retitle-other");
        let cwd = workspace_dir(&dir);
        remember_in(&dir, SESSION, 'a', "dyn-a", Some(cwd.to_str().unwrap())).unwrap();
        retitle('a', "dyn-b", "dyn-b Renamed");
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), Some(cwd));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relative_paths_are_ignored() {
        let dir = temp_dir("relative");
        remember_in(&dir, SESSION, 'a', "dyn-a", Some("relative/dir")).unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_corrupt_entry_reads_as_no_directory() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(entry_path(&dir, 'a'), "not json").unwrap();
        assert_eq!(lookup_in(&dir, SESSION, 'a', "dyn-a"), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
