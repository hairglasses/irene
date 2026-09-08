#![allow(dead_code)]

use std::fs::{File, create_dir_all, read_to_string, rename};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use niri_ipc::Window;

pub fn calculate_font_size(num_cols: usize, max_rows: usize) -> f64 {
    let c = num_cols.max(1) as f64;
    let r = max_rows.max(1) as f64;
    let col_penalty = (c - 2.0).max(0.0) * 0.7;
    let row_penalty = (r - 2.0).max(0.0) * 0.5;
    let size = 12.0 - col_penalty - row_penalty;
    size.clamp(8.5, 12.0)
}

pub fn get_ghostty_pids(windows: &[Window]) -> Vec<i32> {
    let mut pids = Vec::new();
    for w in windows {
        let is_ghostty = w.app_id.as_deref().is_some_and(|app| {
            app.eq_ignore_ascii_case("com.mitchellh.ghostty")
                || app.to_ascii_lowercase().contains("ghostty")
        });
        if is_ghostty
            && let Some(pid) = w.pid
            && pid > 0
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

pub fn get_ghostty_pids_for_workspace(windows: &[Window], workspace_id: u64) -> Vec<i32> {
    let mut pids = Vec::new();
    for w in windows {
        if w.workspace_id != Some(workspace_id) || w.is_floating {
            continue;
        }
        let is_ghostty = w.app_id.as_deref().is_some_and(|app| {
            app.eq_ignore_ascii_case("com.mitchellh.ghostty")
                || app.to_ascii_lowercase().contains("ghostty")
        });
        if is_ghostty
            && let Some(pid) = w.pid
            && pid > 0
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

pub fn signal_ghostty_pids(pids: &[i32]) {
    for &pid in pids {
        if pid > 0 {
            unsafe {
                libc::kill(pid, libc::SIGUSR2);
            }
        }
    }
}

pub fn apply_ghostty_font_size(font_size: f64, pids: &[i32]) -> Result<()> {
    let home = std::env::var("HOME")?;
    let path = format!("{home}/.config/ghostty/config.local");
    apply_ghostty_font_size_with_path(font_size, pids, Path::new(&path))
}

pub fn apply_ghostty_font_size_with_path(
    font_size: f64,
    pids: &[i32],
    config_path: &Path,
) -> Result<()> {
    let content = if config_path.exists() {
        read_to_string(config_path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut lines: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().starts_with("font-size"))
        .map(|s| s.to_string())
        .collect();
    lines.push(format!("font-size = {font_size:.1}"));
    let new_content = lines.join("\n") + "\n";

    if let Some(parent) = config_path.parent() {
        create_dir_all(parent)?;
    }

    let tmp_path = config_path.with_file_name(format!(".config.local.{}.tmp", std::process::id()));

    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(new_content.as_bytes())?;
        file.sync_all()?;
    }
    rename(&tmp_path, config_path)?;

    signal_ghostty_pids(pids);
    Ok(())
}

pub fn restore_ghostty_font_size(original_config: Option<&str>, pids: &[i32]) -> Result<()> {
    let home = std::env::var("HOME")?;
    let path = format!("{home}/.config/ghostty/config.local");
    restore_ghostty_font_size_with_path(original_config, pids, Path::new(&path))
}

pub fn restore_ghostty_font_size_with_path(
    original_config: Option<&str>,
    pids: &[i32],
    config_path: &Path,
) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        create_dir_all(parent)?;
    }

    let tmp_path = config_path.with_file_name(format!(".config.local.{}.tmp", std::process::id()));

    if let Some(orig) = original_config {
        let mut file = File::create(&tmp_path)?;
        file.write_all(orig.as_bytes())?;
        file.sync_all()?;
        rename(&tmp_path, config_path)?;
    } else if config_path.exists() {
        let content = read_to_string(config_path).unwrap_or_default();
        let lines: Vec<&str> = content
            .lines()
            .filter(|l| !l.trim().starts_with("font-size"))
            .collect();
        let mut file = File::create(&tmp_path)?;
        let data = lines.join("\n") + if lines.is_empty() { "" } else { "\n" };
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        rename(&tmp_path, config_path)?;
    }

    signal_ghostty_pids(pids);
    Ok(())
}
