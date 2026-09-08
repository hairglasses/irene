use std::fs::write;

use niri_ipc::{Window, WindowLayout};

use crate::font::{
    apply_ghostty_font_size_with_path, calculate_font_size_with_base,
    detect_ghostty_config_baseline, get_ghostty_pids, get_ghostty_pids_for_workspace,
    restore_ghostty_font_size_with_path,
};
use crate::layout::{
    DEFAULT_GAP, DEFAULT_STRUT_BOTTOM, DEFAULT_STRUT_LEFT, DEFAULT_STRUT_RIGHT, DEFAULT_STRUT_TOP,
    LayoutConfig, calculate_equal_column_widths, calculate_equal_row_heights,
    calculate_usable_height, calculate_usable_width, is_window_excluded, parse_workspace_columns,
};
use crate::snapshot::{
    ColumnSnapshot, FontSnapshot, WorkspaceSnapshot, acquire_lock_from_path, is_compressed,
    is_compressed_with_path, load_snapshot_from_path, lock_path, remove_snapshot,
    remove_snapshot_from_path, save_snapshot_to_path, snapshot_path,
};

#[allow(clippy::too_many_arguments)]
fn make_test_window(
    id: u64,
    workspace_id: u64,
    col: usize,
    row: usize,
    width: f64,
    height: f64,
    app_id: Option<&str>,
    pid: Option<i32>,
) -> Window {
    Window {
        id,
        title: Some(format!("window-{id}")),
        app_id: app_id.map(String::from),
        pid,
        workspace_id: Some(workspace_id),
        is_focused: false,
        is_floating: false,
        is_urgent: false,
        focus_timestamp: None,
        layout: WindowLayout {
            pos_in_scrolling_layout: Some((col, row)),
            tile_size: (width, height),
            window_size: (width as i32, height as i32),
            tile_pos_in_workspace_view: None,
            window_offset_in_tile: (0.0, 0.0),
        },
    }
}

#[test]
fn test_widths_exact_sum_and_delta_across_all_n() {
    let usable_width = 2560;
    let gap = 10;
    for n in 1..=25 {
        let widths = calculate_equal_column_widths(usable_width, gap, n);
        assert_eq!(widths.len(), n);

        let total_consumed: i32 = widths.iter().sum::<i32>() + (n as i32 + 1) * gap;
        assert_eq!(total_consumed, usable_width);

        let max_w = *widths.iter().max().unwrap();
        let min_w = *widths.iter().min().unwrap();
        assert!(
            max_w - min_w <= 1,
            "Delta exceeded 1 for n={n}: {max_w} vs {min_w}"
        );

        for &w in &widths {
            assert!(w > 0);
        }
    }
}

#[test]
fn test_standard_dp1_counts() {
    let widths_3 = calculate_equal_column_widths(2560, 10, 3);
    assert_eq!(widths_3, vec![840, 840, 840]);

    let widths_5 = calculate_equal_column_widths(2560, 10, 5);
    assert_eq!(widths_5, vec![500, 500, 500, 500, 500]);

    let widths_4 = calculate_equal_column_widths(2560, 10, 4);
    assert_eq!(widths_4, vec![628, 628, 627, 627]);

    let widths_8 = calculate_equal_column_widths(2560, 10, 8);
    assert_eq!(widths_8.len(), 8);
    for &w in &widths_8 {
        assert!(w == 308 || w == 309);
    }

    let widths_12 = calculate_equal_column_widths(2560, 10, 12);
    assert_eq!(widths_12.len(), 12);
    for &w in &widths_12 {
        assert!(w == 202 || w == 203);
    }
}

#[test]
fn test_strut_offset_waybar() {
    let usable_width = 2532;
    let gap = 10;

    let widths_3 = calculate_equal_column_widths(usable_width, gap, 3);
    assert_eq!(widths_3, vec![831, 831, 830]);
    assert_eq!(widths_3.iter().sum::<i32>() + 40, 2532);

    let widths_5 = calculate_equal_column_widths(usable_width, gap, 5);
    assert_eq!(widths_5, vec![495, 495, 494, 494, 494]);
    assert_eq!(widths_5.iter().sum::<i32>() + 60, 2532);

    for n in 1..=25 {
        let widths = calculate_equal_column_widths(usable_width, gap, n);
        assert_eq!(widths.len(), n);
        let total = widths.iter().sum::<i32>() + (n as i32 + 1) * gap;
        assert_eq!(total, 2532);
        let max_w = *widths.iter().max().unwrap();
        let min_w = *widths.iter().min().unwrap();
        assert!(max_w - min_w <= 1);
    }
}

#[test]
fn test_zero_and_edge_cases() {
    assert_eq!(
        calculate_equal_column_widths(2560, 10, 0),
        Vec::<i32>::new()
    );
    assert_eq!(
        calculate_equal_column_widths(2560, 0, 4),
        vec![640, 640, 640, 640]
    );
    assert_eq!(calculate_equal_column_widths(10, 50, 2), vec![1, 1]);
}

#[test]
fn test_arbitrary_monitor_resolutions() {
    let resolutions = [(1920, 1080), (2560, 1440), (3840, 2160), (5120, 1440)];
    let gap = 10;
    for &(w_mon, _) in &resolutions {
        for n in 1..=20 {
            let widths = calculate_equal_column_widths(w_mon, gap, n);
            assert_eq!(widths.len(), n);
            let total = widths.iter().sum::<i32>() + (n as i32 + 1) * gap;
            assert!(total <= w_mon);
            let max_w = *widths.iter().max().unwrap();
            let min_w = *widths.iter().min().unwrap();
            assert!(max_w - min_w <= 1);
        }
    }
}

#[test]
fn test_column_parsing_and_5x5_grid_preservation() {
    let mut windows = Vec::new();
    let mut id = 1000;
    for col in 1..=5 {
        for row in 1..=5 {
            windows.push(make_test_window(
                id,
                138,
                col,
                row,
                500.0,
                276.0,
                Some("com.mitchellh.ghostty"),
                Some(id as i32),
            ));
            id += 1;
        }
    }

    let cols = parse_workspace_columns(&windows, 138);
    assert_eq!(cols.len(), 5);

    for (idx, col) in cols.iter().enumerate() {
        assert_eq!(col.column_index, idx + 1);
        assert_eq!(col.window_ids.len(), 5);
        assert_eq!(col.lead_window_id, 1000 + (idx as u64) * 5);
        assert_eq!(col.original_pixel_width, 500);
        assert_eq!(col.window_heights, vec![276, 276, 276, 276, 276]);
    }

    let pids = get_ghostty_pids_for_workspace(&windows, 138);
    assert_eq!(pids.len(), 25);
}

#[test]
fn test_font_scaling_bounds() {
    for c in 1..=10 {
        for r in 1..=10 {
            let size = calculate_font_size_with_base(12.0, c, r);
            assert!(
                size >= 8.5,
                "Font size below 8.5 floor for C={c}, R={r}: {size}"
            );
            assert!(
                size <= 12.0,
                "Font size above 12.0 cap for C={c}, R={r}: {size}"
            );
        }
    }
}

#[test]
fn test_font_scaling_5x5_floor() {
    let size = calculate_font_size_with_base(12.0, 5, 5);
    assert_eq!(size, 8.5);
}

#[test]
fn test_font_scaling_monotonicity() {
    for r in 1..=5 {
        let mut prev = 13.0;
        for c in 1..=5 {
            let size = calculate_font_size_with_base(12.0, c, r);
            assert!(size <= prev);
            prev = size;
        }
    }

    for c in 1..=5 {
        let mut prev = 13.0;
        for r in 1..=5 {
            let size = calculate_font_size_with_base(12.0, c, r);
            assert!(size <= prev);
            prev = size;
        }
    }
}

#[test]
fn test_ghostty_pid_extraction() {
    let windows = vec![
        make_test_window(
            1,
            10,
            1,
            1,
            800.0,
            600.0,
            Some("com.mitchellh.ghostty"),
            Some(1234),
        ),
        make_test_window(2, 10, 1, 2, 800.0, 600.0, Some("ghostty"), Some(5678)),
        make_test_window(3, 10, 2, 1, 800.0, 600.0, Some("firefox"), Some(9999)),
        make_test_window(4, 20, 1, 1, 800.0, 600.0, Some("ghostty"), Some(7777)),
    ];

    let pids_all = get_ghostty_pids(&windows);
    assert_eq!(pids_all.len(), 3);
    assert!(pids_all.contains(&1234));
    assert!(pids_all.contains(&5678));
    assert!(pids_all.contains(&7777));
    assert!(!pids_all.contains(&9999));

    let pids_ws10 = get_ghostty_pids_for_workspace(&windows, 10);
    assert_eq!(pids_ws10.len(), 2);
    assert!(pids_ws10.contains(&1234));
    assert!(pids_ws10.contains(&5678));
    assert!(!pids_ws10.contains(&7777));
}

#[test]
fn test_font_config_local_apply_and_restore() {
    let tmp_dir = std::env::temp_dir().join(format!("niri_test_font_{}", std::process::id()));
    let config_path = tmp_dir.join("config.local");

    let _ = std::fs::remove_dir_all(&tmp_dir);

    apply_ghostty_font_size_with_path(10.5, &[], &config_path).unwrap();
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("font-size = 10.5"));

    apply_ghostty_font_size_with_path(8.5, &[], &config_path).unwrap();
    let content2 = std::fs::read_to_string(&config_path).unwrap();
    assert!(content2.contains("font-size = 8.5"));
    assert_eq!(content2.matches("font-size").count(), 1);

    // Idempotent re-apply should succeed without modifying
    apply_ghostty_font_size_with_path(8.5, &[], &config_path).unwrap();
    let content3 = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(content2, content3);

    restore_ghostty_font_size_with_path(Some("font-family = Terminess\n"), &[], &config_path)
        .unwrap();
    let restored = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(restored, "font-family = Terminess\n");

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_snapshot_roundtrip() {
    let tmp_path = std::env::temp_dir().join(format!("test_snapshot_{}.json", std::process::id()));

    let snap = WorkspaceSnapshot {
        version: 1,
        workspace_id: 138,
        output_name: Some("DP-1".to_string()),
        monitor_width: Some(2560),
        monitor_height: Some(1440),
        timestamp: Some(1725748000.0),
        active_window_id: Some(1001),
        columns: vec![
            ColumnSnapshot {
                column_index: 1,
                lead_window_id: 1000,
                original_pixel_width: 840,
                window_ids: vec![1000, 1001],
                window_heights: vec![705, 705],
            },
            ColumnSnapshot {
                column_index: 2,
                lead_window_id: 1002,
                original_pixel_width: 840,
                window_ids: vec![1002],
                window_heights: vec![1420],
            },
        ],
        font: Some(FontSnapshot {
            original_config_local: Some("test-config".to_string()),
            target_pids: vec![1234, 5678],
            applied_font_size: 11.5,
        }),
        original_widths: Some(vec![840, 840, 840]),
        compressed_widths: Some(vec![500, 500, 500]),
        font_size_original: Some(12.0),
        font_size_compressed: Some(8.5),
        is_compressed: Some(true),
        timestamp_utc: Some("2026-09-08T00:55:00Z".to_string()),
    };

    save_snapshot_to_path(&snap, &tmp_path).unwrap();
    let loaded = load_snapshot_from_path(&tmp_path).expect("failed to load snapshot");
    assert_eq!(loaded, snap);

    remove_snapshot_from_path(&tmp_path);
    assert!(!tmp_path.exists());
}

#[test]
fn test_snapshot_corrupted_payload_self_healing() {
    let tmp_path = std::env::temp_dir().join(format!("test_corrupt_{}.json", std::process::id()));

    // Empty file
    write(&tmp_path, "").unwrap();
    assert!(load_snapshot_from_path(&tmp_path).is_none());
    assert!(
        !tmp_path.exists(),
        "Corrupted empty file should have been unlinked"
    );

    // Truncated JSON
    write(&tmp_path, "{\"version\": 1, \"workspace_id\": ").unwrap();
    assert!(load_snapshot_from_path(&tmp_path).is_none());
    assert!(
        !tmp_path.exists(),
        "Corrupted truncated JSON should have been unlinked"
    );

    // Invalid JSON schema
    write(&tmp_path, "{\"version\": \"not_a_number\"}").unwrap();
    assert!(load_snapshot_from_path(&tmp_path).is_none());
    assert!(
        !tmp_path.exists(),
        "Corrupted schema JSON should have been unlinked"
    );
}

#[test]
fn test_snapshot_advisory_locking() {
    let tmp_lock = std::env::temp_dir().join(format!("test_lock_{}.lock", std::process::id()));

    let lock1 = acquire_lock_from_path(&tmp_lock);
    assert!(lock1.is_some(), "First lock acquisition should succeed");

    let lock2 = acquire_lock_from_path(&tmp_lock);
    assert!(
        lock2.is_none(),
        "Second lock acquisition should fail while lock1 is held"
    );

    drop(lock1);

    let lock3 = acquire_lock_from_path(&tmp_lock);
    assert!(
        lock3.is_some(),
        "Lock acquisition should succeed after previous lock is dropped"
    );

    drop(lock3);
    let _ = std::fs::remove_file(&tmp_lock);
}

#[test]
fn test_interop_m2_snapshot_format() {
    let json_str = r#"{
        "workspace_id": 138,
        "original_widths": [840, 840, 840],
        "compressed_widths": [500, 500, 500],
        "font_size_original": 12.0,
        "font_size_compressed": 8.5,
        "is_compressed": true,
        "timestamp_utc": "2026-09-08T00:55:00Z"
    }"#;

    let snap: WorkspaceSnapshot = serde_json::from_str(json_str).unwrap();
    assert_eq!(snap.workspace_id, 138);
    assert_eq!(snap.version, 1);
    assert_eq!(snap.is_compressed, Some(true));
    assert_eq!(snap.compressed_widths, Some(vec![500, 500, 500]));
    assert_eq!(snap.font_size_compressed, Some(8.5));
}

#[test]
fn test_usable_width_and_struts() {
    assert_eq!(calculate_usable_width(2560, 0, 0), 2560);
    assert_eq!(
        calculate_usable_width(2560, DEFAULT_STRUT_LEFT, DEFAULT_STRUT_RIGHT),
        2532
    );
    assert_eq!(calculate_usable_width(2560, 26, 0), 2534);
    assert_eq!(calculate_usable_width(2560, 3000, 0), 0);
}

#[test]
fn test_equal_row_heights_math() {
    let usable_h = 1440;
    let gap = DEFAULT_GAP;

    let h1 = calculate_equal_row_heights(usable_h, gap, 1);
    assert_eq!(h1, vec![1420]);

    let h2 = calculate_equal_row_heights(usable_h, gap, 2);
    assert_eq!(h2, vec![705, 705]);

    let h3 = calculate_equal_row_heights(usable_h, gap, 3);
    assert_eq!(h3, vec![467, 467, 466]);
    assert_eq!(h3.iter().sum::<i32>() + 4 * gap, usable_h);

    let h5 = calculate_equal_row_heights(usable_h, gap, 5);
    assert_eq!(h5, vec![276, 276, 276, 276, 276]);
    assert_eq!(h5.iter().sum::<i32>() + 6 * gap, usable_h);

    assert_eq!(calculate_equal_row_heights(1440, 10, 0), Vec::<i32>::new());
    assert_eq!(calculate_equal_row_heights(10, 50, 2), vec![1, 1]);
}

#[test]
fn test_snapshot_helpers_and_paths() {
    let ws_id = 99999;
    let path_str = snapshot_path(ws_id);
    assert_eq!(path_str, "/tmp/niri-compress-99999.json");

    let lock_str = lock_path(ws_id);
    assert_eq!(lock_str, "/tmp/niri-compress-99999.lock");

    let p = std::path::Path::new(&path_str);
    remove_snapshot(ws_id);
    assert!(!is_compressed(ws_id));
    assert!(!is_compressed_with_path(p));
}

#[test]
fn test_usable_height_and_struts() {
    assert_eq!(calculate_usable_height(1440, 0, 0), 1440);
    assert_eq!(calculate_usable_height(1440, 30, 0), 1410);
    assert_eq!(calculate_usable_height(1440, 0, 40), 1400);
    assert_eq!(calculate_usable_height(1440, 20, 30), 1390);
    assert_eq!(calculate_usable_height(1440, 2000, 0), 0);
}

#[test]
fn test_layout_config_defaults() {
    let cfg = LayoutConfig::default();
    assert_eq!(cfg.gap, DEFAULT_GAP);
    assert_eq!(cfg.strut_left, DEFAULT_STRUT_LEFT);
    assert_eq!(cfg.strut_right, DEFAULT_STRUT_RIGHT);
    assert_eq!(cfg.strut_top, DEFAULT_STRUT_TOP);
    assert_eq!(cfg.strut_bottom, DEFAULT_STRUT_BOTTOM);
}

#[test]
fn test_is_window_excluded() {
    let floating_win = Window {
        id: 1,
        title: Some("Floating".to_string()),
        app_id: Some("ghostty".to_string()),
        pid: Some(1234),
        workspace_id: Some(1),
        is_focused: false,
        is_floating: true,
        is_urgent: false,
        focus_timestamp: None,
        layout: WindowLayout {
            pos_in_scrolling_layout: None,
            tile_size: (500.0, 500.0),
            window_size: (500, 500),
            tile_pos_in_workspace_view: None,
            window_offset_in_tile: (0.0, 0.0),
        },
    };
    assert!(is_window_excluded(&floating_win, &[]));

    let tiled_mpv = Window {
        id: 2,
        title: Some("Video".to_string()),
        app_id: Some("mpv".to_string()),
        pid: Some(2345),
        workspace_id: Some(1),
        is_focused: false,
        is_floating: false,
        is_urgent: false,
        focus_timestamp: None,
        layout: WindowLayout {
            pos_in_scrolling_layout: Some((0, 0)),
            tile_size: (800.0, 600.0),
            window_size: (800, 600),
            tile_pos_in_workspace_view: None,
            window_offset_in_tile: (0.0, 0.0),
        },
    };
    assert!(!is_window_excluded(&tiled_mpv, &[]));
    assert!(is_window_excluded(&tiled_mpv, &["mpv".to_string()]));
    assert!(is_window_excluded(&tiled_mpv, &["MPV".to_string()]));
    assert!(!is_window_excluded(&tiled_mpv, &["steam".to_string()]));
}

#[test]
fn test_detect_ghostty_config_baseline() {
    let terminess_cfg = r#"
# Ghostty config
font-family = Terminess Nerd Font Mono
background-opacity = 0.85
"#;
    assert_eq!(detect_ghostty_config_baseline(terminess_cfg), 17.0);

    let explicit_cfg = r#"
font-family = Terminess Nerd Font Mono
font-size = 14.5
"#;
    assert_eq!(detect_ghostty_config_baseline(explicit_cfg), 14.5);

    let other_cfg = r#"
font-family = Maple Mono NF CN
font-size = 11.0
"#;
    assert_eq!(detect_ghostty_config_baseline(other_cfg), 11.0);

    let plain_cfg = r#"
# No font settings
background-blur = true
"#;
    assert_eq!(detect_ghostty_config_baseline(plain_cfg), 12.0);
}

#[test]
fn test_equal_row_heights_exact_sum() {
    let usable_h = 1440;
    let gap = 10;
    for m in 1..=10 {
        let heights = calculate_equal_row_heights(usable_h, gap, m);
        assert_eq!(heights.len(), m);
        let total: i32 = heights.iter().sum::<i32>() + (m as i32 + 1) * gap;
        assert_eq!(total, usable_h);
        let max_h = *heights.iter().max().unwrap();
        let min_h = *heights.iter().min().unwrap();
        assert!(max_h - min_h <= 1);
    }
}
