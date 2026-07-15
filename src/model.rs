use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub host: HostInfo,
    pub displays: Vec<DisplaySnapshot>,
    pub windows: Vec<WindowSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisplaySnapshot {
    pub id: String,
    pub numeric_id: u32,
    pub name: Option<String>,
    pub frame: Frame,
    pub scale_factor: f64,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowSnapshot {
    pub window_id: u32,
    pub app_name: String,
    pub process_name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    pub title: Option<String>,
    pub frame: Frame,
    pub display_id: Option<String>,
    pub display_frame: Option<Frame>,
    pub display_relative_frame: Option<RelativeFrame>,
    pub z_order: Option<u32>,
    pub fullscreen: bool,
    pub minimized: bool,
    #[serde(default = "default_window_enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_tabs: Vec<BrowserTab>,
}

pub fn default_window_enabled() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserTab {
    pub title: Option<String>,
    pub url: String,
    pub active: bool,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelativeFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotListEntry {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub path: String,
    pub display_count: usize,
    pub window_count: usize,
}

impl Frame {
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub fn area(self) -> f64 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    pub fn intersects(self, other: Frame) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    pub fn intersection_area(self, other: Frame) -> f64 {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        let width = (right - left).max(0.0);
        let height = (bottom - top).max(0.0);
        width * height
    }

    pub fn relative_to(self, display: Frame) -> RelativeFrame {
        RelativeFrame {
            x: (self.x - display.x) / display.width.max(1.0),
            y: (self.y - display.y) / display.height.max(1.0),
            width: self.width / display.width.max(1.0),
            height: self.height / display.height.max(1.0),
        }
    }
}

impl RelativeFrame {
    pub fn to_frame(self, display: Frame) -> Frame {
        Frame {
            x: display.x + self.x * display.width,
            y: display.y + self.y * display.height,
            width: self.width * display.width,
            height: self.height * display.height,
        }
    }
}
