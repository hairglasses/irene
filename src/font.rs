#![allow(dead_code)]

use std::fs::{File, create_dir_all, read_to_string, rename};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use niri_ipc::Window;

pub fn detect_ghostty_config_baseline(content: &str) -> f64 {
    let mut explicit_size = None;
    let mut has_terminess = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("font-size") {
            let val_str = rest.trim_start_matches([' ', '=']).trim();
            if let Ok(val) = val_str.parse::<f64>()
                && (6.0..=32.0).contains(&val)
            {
                explicit_size = Some(val);
            }
        } else if let Some(rest) = trimmed.strip_prefix("font-family") {
            let val_str = rest.trim_start_matches([' ', '=']).trim();
            if val_str.to_ascii_lowercase().contains("terminess") {
                has_terminess = true;
            }
        }
    }

    if let Some(sz) = explicit_size {
        sz
    } else if has_terminess {
        17.0
    } else {
        12.0
    }
}

pub fn get_base_font_size() -> f64 {
    if let Ok(home) = std::env::var("HOME") {
        let base_path = format!("{home}/.config/ghostty/base-font-size");
        if let Ok(content) = read_to_string(&base_path)
            && let Ok(val) = content.trim().parse::<f64>()
            && (6.0..=32.0).contains(&val)
        {
            return val;
        }

        let config_path = format!("{home}/.config/ghostty/config");
        if let Ok(content) = read_to_string(&config_path) {
            return detect_ghostty_config_baseline(&content);
        }
    }
    12.0
}

pub fn get_font_size_floor() -> f64 {
    if let Ok(home) = std::env::var("HOME") {
        let path = format!("{home}/.config/ghostty/font-size-floor");
        if let Ok(content) = read_to_string(&path)
            && let Ok(val) = content.trim().parse::<f64>()
            && (6.0..=32.0).contains(&val)
        {
            return val;
        }
    }
    8.5
}

pub fn calculate_font_size_with_base(base: f64, num_cols: usize, max_rows: usize) -> f64 {
    let c = num_cols.max(1) as f64;
    let r = max_rows.max(1) as f64;
    let col_penalty = (c - 2.0).max(0.0) * 0.7;
    let row_penalty = (r - 2.0).max(0.0) * 0.5;
    let size = base - col_penalty - row_penalty;
    let min_floor = (base - 3.5).max(6.0);
    size.clamp(min_floor, base)
}

pub fn calculate_font_size(num_cols: usize, max_rows: usize) -> f64 {
    let base = get_base_font_size();
    let floor = get_font_size_floor();
    let raw = calculate_font_size_with_base(base, num_cols, max_rows);
    raw.max(floor)
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
        let base = get_base_font_size();
        let content = read_to_string(config_path).unwrap_or_default();
        let mut lines: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().starts_with("font-size"))
            .map(|s| s.to_string())
            .collect();
        lines.push(format!("font-size = {base:.1}"));
        let mut file = File::create(&tmp_path)?;
        let data = lines.join("\n") + "\n";
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        rename(&tmp_path, config_path)?;
    }

    signal_ghostty_pids(pids);
    Ok(())
}
