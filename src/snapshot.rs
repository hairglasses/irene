#![allow(dead_code)]

use std::ffi::CString;
use std::fs::{File, create_dir_all, remove_file, rename};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ColumnSnapshot {
    pub column_index: usize,
    pub lead_window_id: u64,
    pub original_pixel_width: i32,
    pub window_ids: Vec<u64>,
    pub window_heights: Vec<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FontSnapshot {
    #[serde(default)]
    pub original_config_local: Option<String>,
    pub target_pids: Vec<i32>,
    pub applied_font_size: f64,
}

fn default_version() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkspaceSnapshot {
    #[serde(default = "default_version")]
    pub version: u32,
    pub workspace_id: u64,
    #[serde(default)]
    pub output_name: Option<String>,
    #[serde(default)]
    pub monitor_width: Option<i32>,
    #[serde(default)]
    pub monitor_height: Option<i32>,
    #[serde(default)]
    pub timestamp: Option<f64>,
    #[serde(default)]
    pub active_window_id: Option<u64>,
    #[serde(default)]
    pub columns: Vec<ColumnSnapshot>,
    #[serde(default)]
    pub font: Option<FontSnapshot>,

    #[serde(default)]
    pub original_widths: Option<Vec<i32>>,
    #[serde(default)]
    pub compressed_widths: Option<Vec<i32>>,
    #[serde(default)]
    pub font_size_original: Option<f64>,
    #[serde(default)]
    pub font_size_compressed: Option<f64>,
    #[serde(default)]
    pub is_compressed: Option<bool>,
    #[serde(default)]
    pub timestamp_utc: Option<String>,
}

pub fn snapshot_path(workspace_id: u64) -> String {
    format!("/tmp/niri-compress-{workspace_id}.json")
}

pub fn lock_path(workspace_id: u64) -> String {
    format!("/tmp/niri-compress-{workspace_id}.lock")
}

pub fn is_compressed(workspace_id: u64) -> bool {
    Path::new(&snapshot_path(workspace_id)).exists()
}

pub fn is_compressed_with_path(path: &Path) -> bool {
    path.exists()
}

pub fn save_snapshot(snapshot: &WorkspaceSnapshot) -> Result<()> {
    let target = snapshot_path(snapshot.workspace_id);
    save_snapshot_to_path(snapshot, Path::new(&target))
}

pub fn save_snapshot_to_path(snapshot: &WorkspaceSnapshot, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        create_dir_all(parent)?;
    }

    let tmp = target.with_file_name(format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("snapshot"),
        std::process::id()
    ));

    let json = serde_json::to_string_pretty(snapshot)?;
    {
        let mut file = File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }
    rename(&tmp, target)?;
    Ok(())
}

pub fn load_snapshot(workspace_id: u64) -> Option<WorkspaceSnapshot> {
    let path = snapshot_path(workspace_id);
    load_snapshot_from_path(Path::new(&path))
}

pub fn load_snapshot_from_path(path: &Path) -> Option<WorkspaceSnapshot> {
    if !path.exists() {
        return None;
    }
    let data = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return None,
    };
    if data.trim().is_empty() {
        let _ = remove_file(path);
        return None;
    }
    match serde_json::from_str::<WorkspaceSnapshot>(&data) {
        Ok(snap) => Some(snap),
        Err(_) => {
            let _ = remove_file(path);
            None
        }
    }
}

pub fn remove_snapshot(workspace_id: u64) {
    let _ = remove_file(snapshot_path(workspace_id));
}

pub fn remove_snapshot_from_path(path: &Path) {
    let _ = remove_file(path);
}

pub struct SnapshotLock {
    fd: Option<i32>,
}

impl Drop for SnapshotLock {
    fn drop(&mut self) {
        if let Some(fd) = self.fd.take() {
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
                libc::close(fd);
            }
        }
    }
}

pub fn acquire_lock(workspace_id: u64) -> Option<SnapshotLock> {
    let path = lock_path(workspace_id);
    acquire_lock_from_path(Path::new(&path))
}

pub fn acquire_lock_from_path(path: &Path) -> Option<SnapshotLock> {
    let path_str = path.to_str()?;
    let c_path = CString::new(path_str).ok()?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o644) };
    if fd < 0 {
        return None;
    }
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        unsafe {
            libc::close(fd);
        }
        return None;
    }
    Some(SnapshotLock { fd: Some(fd) })
}
