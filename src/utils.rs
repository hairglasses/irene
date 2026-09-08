#![allow(dead_code)]

use std::collections::HashMap;

use anyhow::{Result, bail};
use niri_ipc::{Action, Output, Request, Response, SizeChange, Window, Workspace, socket::Socket};

use crate::font::{
    apply_ghostty_font_size, calculate_font_size, get_ghostty_pids_for_workspace,
    restore_ghostty_font_size,
};
pub use crate::layout::DEFAULT_GAP;
use crate::layout::{calculate_equal_column_widths, parse_workspace_columns};
use crate::snapshot::{
    ColumnSnapshot, FontSnapshot, WorkspaceSnapshot, is_compressed, load_snapshot, remove_snapshot,
    save_snapshot,
};

const MASTER_COLUMN_PROPORTION: f64 = 50.0;

pub type Membership = (Option<u64>, bool);

pub fn membership(window: &Window) -> Membership {
    (window.workspace_id, window.is_floating)
}

pub fn send_action(socket: &mut Socket, action: Action) -> Result<()> {
    match socket.send(Request::Action(action))? {
        Ok(_) => Ok(()),
        Err(msg) => bail!("niri rejected action: {msg}"),
    }
}

pub fn fetch_windows(socket: &mut Socket) -> Result<Vec<Window>> {
    match socket.send(Request::Windows)? {
        Ok(Response::Windows(windows)) => Ok(windows),
        Ok(_) => bail!("unexpected response to Windows request"),
        Err(msg) => bail!("niri rejected Windows request: {msg}"),
    }
}

pub fn fetch_outputs(socket: &mut Socket) -> Result<HashMap<String, Output>> {
    match socket.send(Request::Outputs)? {
        Ok(Response::Outputs(outputs)) => Ok(outputs),
        Ok(_) => bail!("unexpected response to Outputs request"),
        Err(msg) => bail!("niri rejected Outputs request: {msg}"),
    }
}

pub fn fetch_workspaces(socket: &mut Socket) -> Result<Vec<Workspace>> {
    match socket.send(Request::Workspaces)? {
        Ok(Response::Workspaces(workspaces)) => Ok(workspaces),
        Ok(_) => bail!("unexpected response to Workspaces request"),
        Err(msg) => bail!("niri rejected Workspaces request: {msg}"),
    }
}

pub fn output_scale(outputs: &HashMap<String, Output>, name: Option<&str>) -> f64 {
    name.and_then(|name| outputs.get(name))
        .and_then(|output| output.logical.as_ref())
        .map_or(1.0, |logical| logical.scale)
}

pub fn output_dimensions(outputs: &HashMap<String, Output>, name: Option<&str>) -> (i32, i32) {
    name.and_then(|name| outputs.get(name))
        .and_then(|output| output.logical.as_ref())
        .map_or((2560, 1440), |logical| {
            (logical.width as i32, logical.height as i32)
        })
}

pub fn layout_equal_grid(
    socket: &mut Socket,
    windows: &[Window],
    workspace_id: u64,
    usable_width: i32,
    gap: i32,
) -> Result<()> {
    let columns = parse_workspace_columns(windows, workspace_id);
    if columns.is_empty() {
        return Ok(());
    }

    let max_rows = columns
        .iter()
        .map(|c| c.window_ids.len())
        .max()
        .unwrap_or(1);
    let font_size = calculate_font_size(columns.len(), max_rows);
    let pids = get_ghostty_pids_for_workspace(windows, workspace_id);

    let widths = if columns.len() == 1 {
        let single_w = (usable_width - 2 * gap).max(1);
        vec![single_w]
    } else {
        calculate_equal_column_widths(usable_width, gap, columns.len())
    };

    for (col, &w) in columns.iter().zip(&widths) {
        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(col.lead_window_id),
                change: SizeChange::SetFixed(w),
            },
        )?;
        for &id in &col.window_ids {
            send_action(socket, Action::ResetWindowHeight { id: Some(id) })?;
        }
    }

    if !pids.is_empty() {
        let _ = apply_ghostty_font_size(font_size, &pids);
    }

    if is_compressed(workspace_id)
        && let Some(mut snap) = load_snapshot(workspace_id)
    {
        snap.columns = columns
            .iter()
            .map(|c| ColumnSnapshot {
                column_index: c.column_index,
                lead_window_id: c.lead_window_id,
                original_pixel_width: c.original_pixel_width,
                window_ids: c.window_ids.clone(),
                window_heights: c.window_heights.clone(),
            })
            .collect();
        if let Some(ref mut font_snap) = snap.font {
            font_snap.applied_font_size = font_size;
            font_snap.target_pids = pids.clone();
        }
        snap.compressed_widths = Some(widths);
        let _ = save_snapshot(&snap);
    }

    Ok(())
}

pub fn layout_workspace(
    socket: &mut Socket,
    windows: &[Window],
    workspace_id: u64,
    _scale: f64,
) -> Result<()> {
    if is_compressed(workspace_id) {
        return layout_equal_grid(socket, windows, workspace_id, 2560, DEFAULT_GAP);
    }

    let mut tiles: Vec<(u64, (usize, usize))> = windows
        .iter()
        .filter(|w| w.workspace_id == Some(workspace_id) && !w.is_floating)
        .filter_map(|w| w.layout.pos_in_scrolling_layout.map(|pos| (w.id, pos)))
        .collect();
    tiles.sort_by_key(|&(_, pos)| pos);

    if tiles.is_empty() {
        return Ok(());
    }

    if tiles.len() == 1 {
        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(tiles[0].0),
                change: SizeChange::SetProportion(100.0),
            },
        )?;
        return send_action(
            socket,
            Action::ResetWindowHeight {
                id: Some(tiles[0].0),
            },
        );
    }

    let mut stacked: Vec<(u64, (usize, usize))> = tiles
        .iter()
        .copied()
        .filter(|&(_, (_, row))| row > 1)
        .collect();
    stacked.sort_by_key(|&(_, (col, row))| (col, std::cmp::Reverse(row)));
    for (id, _) in stacked {
        send_action(socket, Action::ConsumeOrExpelWindowRight { id: Some(id) })?;
    }

    if tiles.len() == 2 {
        for &(id, _) in &tiles {
            send_action(
                socket,
                Action::SetWindowWidth {
                    id: Some(id),
                    change: SizeChange::SetProportion(50.0),
                },
            )?;
            send_action(socket, Action::ResetWindowHeight { id: Some(id) })?;
        }
        return Ok(());
    }

    if tiles.len() == 3 {
        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(tiles[0].0),
                change: SizeChange::SetProportion(50.0),
            },
        )?;
        send_action(
            socket,
            Action::ResetWindowHeight {
                id: Some(tiles[0].0),
            },
        )?;

        let top = tiles[1].0;
        let bottom = tiles[2].0;
        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(top),
                change: SizeChange::SetProportion(50.0),
            },
        )?;
        send_action(
            socket,
            Action::ConsumeOrExpelWindowLeft { id: Some(bottom) },
        )?;
        send_action(socket, Action::ResetWindowHeight { id: Some(top) })?;
        send_action(socket, Action::ResetWindowHeight { id: Some(bottom) })?;
        return Ok(());
    }

    if tiles.len() == 4 {
        let c1_top = tiles[0].0;
        let c1_bottom = tiles[1].0;
        let c2_top = tiles[2].0;
        let c2_bottom = tiles[3].0;

        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(c1_top),
                change: SizeChange::SetProportion(50.0),
            },
        )?;
        send_action(
            socket,
            Action::ConsumeOrExpelWindowLeft {
                id: Some(c1_bottom),
            },
        )?;
        send_action(socket, Action::ResetWindowHeight { id: Some(c1_top) })?;
        send_action(
            socket,
            Action::ResetWindowHeight {
                id: Some(c1_bottom),
            },
        )?;

        send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(c2_top),
                change: SizeChange::SetProportion(50.0),
            },
        )?;
        send_action(
            socket,
            Action::ConsumeOrExpelWindowLeft {
                id: Some(c2_bottom),
            },
        )?;
        send_action(socket, Action::ResetWindowHeight { id: Some(c2_top) })?;
        send_action(
            socket,
            Action::ResetWindowHeight {
                id: Some(c2_bottom),
            },
        )?;
        return Ok(());
    }

    for group in tiles.chunks(3) {
        for &(id, _) in &group[..group.len().min(2)] {
            send_action(
                socket,
                Action::SetWindowWidth {
                    id: Some(id),
                    change: SizeChange::SetProportion(MASTER_COLUMN_PROPORTION),
                },
            )?;
        }
        if let &[_, (top, _), (bottom, _)] = group {
            send_action(
                socket,
                Action::ConsumeOrExpelWindowLeft { id: Some(bottom) },
            )?;
            for id in [top, bottom] {
                send_action(socket, Action::ResetWindowHeight { id: Some(id) })?;
            }
        }
    }

    if let Some(focused) = windows.iter().find(|w| w.is_focused)
        && let Some(idx) = tiles.iter().position(|&(id, _)| id == focused.id)
    {
        let group_start = (idx / 3) * 3;
        let mut ids = vec![tiles[group_start].0];
        if let Some(&(second, _)) = tiles.get(group_start + 1) {
            ids.push(second);
        }
        ids.push(focused.id);
        ids.dedup();
        for id in ids {
            send_action(socket, Action::FocusWindow { id })?;
        }
    }

    Ok(())
}

pub fn toggle_compression(
    socket: &mut Socket,
    workspace_id_opt: Option<u64>,
    usable_width: i32,
    gap: i32,
) -> Result<bool> {
    let workspaces = fetch_workspaces(socket)?;
    let target_ws = if let Some(id) = workspace_id_opt {
        workspaces.into_iter().find(|ws| ws.id == id)
    } else {
        workspaces.into_iter().find(|ws| ws.is_focused)
    };
    let Some(ws) = target_ws else {
        bail!("no matching workspace found");
    };
    let ws_id = ws.id;

    if is_compressed(ws_id) {
        if let Some(snapshot) = load_snapshot(ws_id) {
            for col in &snapshot.columns {
                let _ = send_action(
                    socket,
                    Action::SetWindowWidth {
                        id: Some(col.lead_window_id),
                        change: SizeChange::SetFixed(col.original_pixel_width),
                    },
                );
                for &win_id in &col.window_ids {
                    let _ = send_action(socket, Action::ResetWindowHeight { id: Some(win_id) });
                }
            }
            if let Some(ref font_snap) = snapshot.font {
                let _ = restore_ghostty_font_size(
                    font_snap.original_config_local.as_deref(),
                    &font_snap.target_pids,
                );
            }
        }
        remove_snapshot(ws_id);
        let _ = send_action(socket, Action::CenterVisibleColumns {});
        Ok(false)
    } else {
        let windows = fetch_windows(socket)?;
        let columns = parse_workspace_columns(&windows, ws_id);
        if columns.is_empty() {
            return Ok(false);
        }

        let orig_config = std::env::var("HOME").ok().and_then(|h| {
            std::fs::read_to_string(format!("{h}/.config/ghostty/config.local")).ok()
        });
        let pids = get_ghostty_pids_for_workspace(&windows, ws_id);
        let max_rows = columns
            .iter()
            .map(|c| c.window_ids.len())
            .max()
            .unwrap_or(1);
        let font_size = calculate_font_size(columns.len(), max_rows);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let orig_widths: Vec<i32> = columns.iter().map(|c| c.original_pixel_width).collect();
        let compressed_widths = calculate_equal_column_widths(usable_width, gap, columns.len());

        let snapshot = WorkspaceSnapshot {
            version: 1,
            workspace_id: ws_id,
            output_name: ws.output.clone(),
            monitor_width: Some(usable_width),
            monitor_height: Some(1440),
            timestamp: Some(now),
            active_window_id: ws.active_window_id,
            columns: columns
                .iter()
                .map(|c| ColumnSnapshot {
                    column_index: c.column_index,
                    lead_window_id: c.lead_window_id,
                    original_pixel_width: c.original_pixel_width,
                    window_ids: c.window_ids.clone(),
                    window_heights: c.window_heights.clone(),
                })
                .collect(),
            font: Some(FontSnapshot {
                original_config_local: orig_config,
                target_pids: pids.clone(),
                applied_font_size: font_size,
            }),
            original_widths: Some(orig_widths),
            compressed_widths: Some(compressed_widths),
            font_size_original: None,
            font_size_compressed: Some(font_size),
            is_compressed: Some(true),
            timestamp_utc: None,
        };
        save_snapshot(&snapshot)?;

        layout_equal_grid(socket, &windows, ws_id, usable_width, gap)?;
        let _ = send_action(socket, Action::CenterVisibleColumns {});
        Ok(true)
    }
}
