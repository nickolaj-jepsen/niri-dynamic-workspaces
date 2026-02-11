use crate::backend::{WindowInfo, WorkspaceInfo};

pub fn test_window(id: u64, workspace_id: u64, app_id: &str) -> WindowInfo {
    WindowInfo {
        id,
        workspace_id: Some(workspace_id),
        app_id: Some(app_id.to_string()),
        is_urgent: false,
    }
}

pub fn test_workspace(id: u64, name: Option<&str>, is_focused: bool) -> WorkspaceInfo {
    WorkspaceInfo {
        id,
        idx: 1,
        name: name.map(String::from),
        output: Some("DP-1".to_string()),
        is_focused,
        is_active: false,
    }
}
