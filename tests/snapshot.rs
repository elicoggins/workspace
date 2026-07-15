use chrono::{TimeZone, Utc};
use workspace::model::{
    BrowserTab, DisplaySnapshot, Frame, HostInfo, RelativeFrame, WindowSnapshot, WorkspaceSnapshot,
    SNAPSHOT_VERSION,
};

#[test]
fn snapshot_round_trips_as_json() {
    let snapshot = WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        name: "coding".to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 5, 12, 15, 30, 0).unwrap(),
        host: HostInfo {
            hostname: "macbook-pro".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
        },
        displays: vec![DisplaySnapshot {
            id: "cgdisplay-1".to_string(),
            numeric_id: 1,
            name: Some("Built-in Display".to_string()),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
            },
            scale_factor: 2.0,
            is_primary: true,
        }],
        windows: vec![WindowSnapshot {
            window_id: 231,
            app_name: "Visual Studio Code".to_string(),
            process_name: "Code".to_string(),
            bundle_id: Some("com.microsoft.VSCode".to_string()),
            pid: 1234,
            title: Some("api.ts".to_string()),
            frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 1728.0,
                height: 1415.0,
            },
            display_id: Some("cgdisplay-1".to_string()),
            display_frame: Some(Frame {
                x: 0.0,
                y: 0.0,
                width: 2560.0,
                height: 1440.0,
            }),
            display_relative_frame: Some(RelativeFrame {
                x: 0.0,
                y: 0.0,
                width: 0.675,
                height: 0.9826388889,
            }),
            z_order: Some(0),
            fullscreen: false,
            minimized: false,
            enabled: true,
            browser_tabs: vec![
                BrowserTab {
                    title: Some("Rust".to_string()),
                    url: "https://www.rust-lang.org/".to_string(),
                    active: true,
                },
                BrowserTab {
                    title: Some("Docs".to_string()),
                    url: "https://doc.rust-lang.org/".to_string(),
                    active: false,
                },
            ],
        }],
    };

    let json = serde_json::to_string_pretty(&snapshot).unwrap();
    let decoded: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded, snapshot);
}

#[test]
fn old_snapshots_default_windows_to_enabled() {
    let json = r#"
        {
            "version": 1,
            "name": "old",
            "created_at": "2026-05-12T15:30:00Z",
            "host": { "hostname": "host", "os": "macos", "arch": "aarch64" },
            "displays": [],
            "windows": [
                {
                    "window_id": 1,
                    "app_name": "Code",
                    "process_name": "Code",
                    "bundle_id": "com.microsoft.VSCode",
                    "pid": 42,
                    "title": "main",
                    "frame": { "x": 0.0, "y": 0.0, "width": 100.0, "height": 100.0 },
                    "display_id": null,
                    "display_frame": null,
                    "display_relative_frame": null,
                    "z_order": 0,
                    "fullscreen": false,
                    "minimized": false
                }
            ]
        }
        "#;

    let decoded: WorkspaceSnapshot = serde_json::from_str(json).unwrap();

    assert!(decoded.windows[0].enabled);
}

#[test]
fn snapshot_store_rejects_future_schema_versions() {
    use std::fs;
    use workspace::error::WorkspaceError;
    use workspace::storage::SnapshotStore;

    let tmp = tempdir_path("workspace-future-version");
    fs::create_dir_all(&tmp).unwrap();
    let store = SnapshotStore::new(tmp.clone()).unwrap();

    let path = tmp.join("future.json");
    let body = serde_json::json!({
        "version": 999,
        "name": "future",
        "created_at": "2030-01-01T00:00:00Z",
        "host": { "hostname": "h", "os": "macos", "arch": "aarch64" },
        "displays": [],
        "windows": []
    });
    fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();

    let err = store.load("future").unwrap_err();
    match err {
        WorkspaceError::UnsupportedSnapshotVersion {
            name,
            found,
            supported,
        } => {
            assert_eq!(name, "future");
            assert_eq!(found, 999);
            assert!(supported < 999);
        }
        other => panic!("expected UnsupportedSnapshotVersion, got {other:?}"),
    }
}

fn tempdir_path(prefix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}"))
}
