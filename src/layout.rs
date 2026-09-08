#![allow(dead_code)]

use niri_ipc::Window;

pub const DEFAULT_GAP: i32 = 10;
pub const DEFAULT_STRUT_LEFT: i32 = 28;
pub const DEFAULT_STRUT_RIGHT: i32 = 0;
pub const DEFAULT_STRUT_TOP: i32 = 0;
pub const DEFAULT_STRUT_BOTTOM: i32 = 0;

pub fn calculate_usable_width(monitor_width: i32, strut_left: i32, strut_right: i32) -> i32 {
    (monitor_width - strut_left - strut_right).max(0)
}

pub fn calculate_usable_height(monitor_height: i32, strut_top: i32, strut_bottom: i32) -> i32 {
    (monitor_height - strut_top - strut_bottom).max(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutConfig {
    pub gap: i32,
    pub strut_left: i32,
    pub strut_right: i32,
    pub strut_top: i32,
    pub strut_bottom: i32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            gap: DEFAULT_GAP,
            strut_left: DEFAULT_STRUT_LEFT,
            strut_right: DEFAULT_STRUT_RIGHT,
            strut_top: DEFAULT_STRUT_TOP,
            strut_bottom: DEFAULT_STRUT_BOTTOM,
        }
    }
}

pub fn is_window_excluded(window: &Window, excluded_app_ids: &[String]) -> bool {
    if window.is_floating {
        return true;
    }
    if let Some(ref app) = window.app_id
        && excluded_app_ids
            .iter()
            .any(|ex| app.eq_ignore_ascii_case(ex))
    {
        return true;
    }
    false
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnInfo {
    pub column_index: usize,
    pub lead_window_id: u64,
    pub window_ids: Vec<u64>,
    pub original_pixel_width: i32,
    pub window_heights: Vec<i32>,
}

pub fn calculate_equal_column_widths(usable_width: i32, gap: i32, num_columns: usize) -> Vec<i32> {
    if num_columns == 0 {
        return Vec::new();
    }
    let n = num_columns as i32;
    let total_gaps = (n + 1) * gap;
    let available_width = usable_width - total_gaps;
    if available_width < n {
        return vec![1; num_columns];
    }

    let base_width = available_width / n;
    let remainder = available_width % n;

    let mut widths = Vec::with_capacity(num_columns);
    for i in 0..num_columns {
        if (i as i32) < remainder {
            widths.push(base_width + 1);
        } else {
            widths.push(base_width);
        }
    }
    widths
}

pub fn calculate_equal_row_heights(usable_height: i32, gap: i32, num_rows: usize) -> Vec<i32> {
    if num_rows == 0 {
        return Vec::new();
    }
    let m = num_rows as i32;
    let total_gaps = (m + 1) * gap;
    let available_height = usable_height - total_gaps;
    if available_height < m {
        return vec![1; num_rows];
    }

    let base_height = available_height / m;
    let remainder = available_height % m;

    let mut heights = Vec::with_capacity(num_rows);
    for i in 0..num_rows {
        if (i as i32) < remainder {
            heights.push(base_height + 1);
        } else {
            heights.push(base_height);
        }
    }
    heights
}

pub fn parse_workspace_columns(windows: &[Window], workspace_id: u64) -> Vec<ColumnInfo> {
    parse_workspace_columns_with_exclusions(windows, workspace_id, &[])
}

pub fn parse_workspace_columns_with_exclusions(
    windows: &[Window],
    workspace_id: u64,
    excluded_app_ids: &[String],
) -> Vec<ColumnInfo> {
    let mut cols_map: std::collections::BTreeMap<usize, Vec<&Window>> =
        std::collections::BTreeMap::new();

    for w in windows {
        if w.workspace_id != Some(workspace_id) || is_window_excluded(w, excluded_app_ids) {
            continue;
        }
        if let Some((col_idx, _)) = w.layout.pos_in_scrolling_layout {
            cols_map.entry(col_idx).or_default().push(w);
        }
    }

    let mut columns = Vec::new();
    for (&col_idx, win_list) in &mut cols_map {
        win_list.sort_by_key(|w| {
            w.layout
                .pos_in_scrolling_layout
                .map(|(_, row)| row)
                .unwrap_or(0)
        });
        let lead_win = win_list[0];
        let orig_w = lead_win.layout.tile_size.0.round() as i32;
        let window_ids = win_list.iter().map(|w| w.id).collect();
        let window_heights = win_list
            .iter()
            .map(|w| w.layout.tile_size.1.round() as i32)
            .collect();

        columns.push(ColumnInfo {
            column_index: col_idx,
            lead_window_id: lead_win.id,
            window_ids,
            original_pixel_width: orig_w,
            window_heights,
        });
    }

    columns
}
