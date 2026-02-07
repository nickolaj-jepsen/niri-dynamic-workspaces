use niri_ipc::{Timestamp, Window, WindowLayout, Workspace};

pub fn test_window(id: u64, workspace_id: u64, app_id: &str) -> Window {
    Window {
        id,
        title: None,
        app_id: Some(app_id.to_string()),
        pid: None,
        workspace_id: Some(workspace_id),
        is_focused: false,
        is_floating: false,
        is_urgent: false,
        layout: WindowLayout {
            pos_in_scrolling_layout: None,
            tile_size: (0.0, 0.0),
            window_size: (0, 0),
            tile_pos_in_workspace_view: None,
            window_offset_in_tile: (0.0, 0.0),
        },
        focus_timestamp: Some(Timestamp { secs: 0, nanos: 0 }),
    }
}

pub fn test_workspace(id: u64, name: Option<&str>, is_focused: bool) -> Workspace {
    Workspace {
        id,
        idx: 1,
        name: name.map(String::from),
        output: Some("DP-1".to_string()),
        is_urgent: false,
        is_active: false,
        is_focused,
        active_window_id: None,
    }
}
