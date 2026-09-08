use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use niri_ipc::{Event, Request, Response, socket::Socket};

use crate::utils::{
    Membership, fetch_outputs, fetch_windows, fetch_workspaces, layout_equal_grid_2d,
    layout_workspace, membership, output_dimensions, output_scale, toggle_compression,
};

mod font;
mod layout;
mod snapshot;
mod utils;

#[cfg(test)]
mod tests;

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(long, global = true)]
    equal_grid: bool,

    #[arg(long, global = true)]
    mode: Option<String>,

    #[arg(long, global = true)]
    strut_left: Option<i32>,

    #[arg(long, global = true)]
    strut_right: Option<i32>,

    #[arg(long, global = true)]
    strut_top: Option<i32>,

    #[arg(long, global = true)]
    strut_bottom: Option<i32>,

    #[arg(long, global = true)]
    gap: Option<i32>,

    #[arg(long, global = true)]
    exclude: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    Daemon {
        #[arg(long)]
        equal_grid: bool,

        #[arg(long)]
        mode: Option<String>,
    },

    Compress {
        #[arg(long, default_value_t = true)]
        toggle: bool,

        #[arg(long)]
        workspace: Option<u64>,
    },

    RefreshFocusedWorkspace {
        #[arg(long)]
        equal_grid: bool,

        #[arg(long)]
        mode: Option<String>,
    },

    RefreshAllWorkspaces {
        #[arg(long)]
        equal_grid: bool,

        #[arg(long)]
        mode: Option<String>,
    },

    Status {
        #[arg(long)]
        json: bool,
    },

    Mode {
        mode: Option<String>,
    },
}

fn resolve_layout_config(cli: &Cli) -> layout::LayoutConfig {
    let gap = cli
        .gap
        .or_else(|| std::env::var("IRENE_GAP").ok().and_then(|v| v.parse().ok()))
        .unwrap_or(layout::DEFAULT_GAP);
    let strut_left = cli
        .strut_left
        .or_else(|| {
            std::env::var("IRENE_STRUT_LEFT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(layout::DEFAULT_STRUT_LEFT);
    let strut_right = cli
        .strut_right
        .or_else(|| {
            std::env::var("IRENE_STRUT_RIGHT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(layout::DEFAULT_STRUT_RIGHT);
    let strut_top = cli
        .strut_top
        .or_else(|| {
            std::env::var("IRENE_STRUT_TOP")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(layout::DEFAULT_STRUT_TOP);
    let strut_bottom = cli
        .strut_bottom
        .or_else(|| {
            std::env::var("IRENE_STRUT_BOTTOM")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(layout::DEFAULT_STRUT_BOTTOM);

    layout::LayoutConfig {
        gap,
        strut_left,
        strut_right,
        strut_top,
        strut_bottom,
    }
}

fn print_status(json_format: bool) -> Result<()> {
    let mut socket = Socket::connect()?;
    let outputs = fetch_outputs(&mut socket)?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let windows = fetch_windows(&mut socket)?;
    let base_font = font::get_base_font_size();
    let font_floor = font::get_font_size_floor();

    if json_format {
        let mut ws_list = Vec::new();
        for ws in &workspaces {
            let cols = layout::parse_workspace_columns(&windows, ws.id);
            let win_count = windows
                .iter()
                .filter(|w| w.workspace_id == Some(ws.id) && !w.is_floating)
                .count();
            ws_list.push(serde_json::json!({
                "id": ws.id,
                "name": ws.name,
                "output": ws.output,
                "is_focused": ws.is_focused,
                "is_active": ws.is_active,
                "column_count": cols.len(),
                "window_count": win_count,
                "is_compressed": snapshot::is_compressed(ws.id),
            }));
        }

        let mut out_list = Vec::new();
        for (name, out) in &outputs {
            let (w, h) = if let Some(ref log) = out.logical {
                (log.width as i32, log.height as i32)
            } else {
                (2560, 1440)
            };
            out_list.push(serde_json::json!({
                "name": name,
                "width": w,
                "height": h,
                "scale": out.logical.as_ref().map(|l| l.scale).unwrap_or(1.0),
            }));
        }

        let ghostty_pids = font::get_ghostty_pids(&windows);

        let report = serde_json::json!({
            "status": "ok",
            "outputs": out_list,
            "workspaces": ws_list,
            "font": {
                "base_font_size": base_font,
                "font_size_floor": font_floor,
                "ghostty_instances": ghostty_pids.len(),
            }
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("irene autotiling status:");
        println!("  Ghostty Base Font: {base_font:.1}pt (Floor: {font_floor:.1}pt)");
        println!("  Outputs:");
        for (name, out) in &outputs {
            let (w, h) = if let Some(ref log) = out.logical {
                (log.width as i32, log.height as i32)
            } else {
                (2560, 1440)
            };
            let scale = out.logical.as_ref().map(|l| l.scale).unwrap_or(1.0);
            println!("    - {name}: {w}x{h} (scale {scale})");
        }
        println!("  Workspaces:");
        for ws in &workspaces {
            let cols = layout::parse_workspace_columns(&windows, ws.id);
            let comp = if snapshot::is_compressed(ws.id) {
                " [COMPRESSED]"
            } else {
                ""
            };
            let focus = if ws.is_focused { " (focused)" } else { "" };
            let out = ws.output.as_deref().unwrap_or("none");
            println!(
                "    - WS {} on {}: {} column(s){focus}{comp}",
                ws.id,
                out,
                cols.len()
            );
        }
    }
    Ok(())
}

fn handle_mode(mode: Option<String>) -> Result<()> {
    let mode_path = "/tmp/niri-irene-mode";
    if let Some(m) = mode {
        let normalized = m.to_lowercase();
        if normalized != "equal-grid" && normalized != "master-stack" {
            bail!("invalid mode: '{normalized}'. Valid modes: equal-grid, master-stack");
        }
        std::fs::write(mode_path, format!("{normalized}\n"))?;
        println!("irene autotiling mode set to: {normalized}");
    } else {
        let current = if let Ok(content) = std::fs::read_to_string(mode_path) {
            content.trim().to_string()
        } else {
            "equal-grid".to_string()
        };
        println!("irene autotiling mode: {current}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let global_equal = cli.equal_grid;
    let global_mode = cli.mode.as_deref();
    let config = resolve_layout_config(&cli);
    let exclusions = cli.exclude.clone();

    match cli.command {
        None => {
            let equal_grid = global_mode != Some("master-stack");
            run_daemon(equal_grid, config, exclusions)
        }
        Some(Command::Daemon { equal_grid, mode }) => {
            let is_equal = equal_grid
                || global_equal
                || (mode.as_deref() != Some("master-stack") && global_mode != Some("master-stack"));
            run_daemon(is_equal, config, exclusions)
        }
        Some(Command::Compress { workspace, .. }) => {
            let mut socket = Socket::connect()?;
            let outputs = fetch_outputs(&mut socket)?;
            let workspaces = fetch_workspaces(&mut socket)?;
            let target_ws = if let Some(id) = workspace {
                workspaces.iter().find(|ws| ws.id == id)
            } else {
                workspaces.iter().find(|ws| ws.is_focused)
            };
            let out_name = target_ws.and_then(|ws| ws.output.as_deref());
            let (width, _) = output_dimensions(&outputs, out_name);
            let usable_w =
                layout::calculate_usable_width(width, config.strut_left, config.strut_right);
            let is_comp = toggle_compression(&mut socket, workspace, usable_w, config.gap)?;
            println!(
                "Workspace compression: {}",
                if is_comp { "enabled" } else { "restored" }
            );
            Ok(())
        }
        Some(Command::RefreshFocusedWorkspace { equal_grid, mode }) => {
            let is_equal = equal_grid
                || global_equal
                || (mode.as_deref() != Some("master-stack") && global_mode != Some("master-stack"));
            refresh_focused_workspace(is_equal, config)
        }
        Some(Command::RefreshAllWorkspaces { equal_grid, mode }) => {
            let is_equal = equal_grid
                || global_equal
                || (mode.as_deref() != Some("master-stack") && global_mode != Some("master-stack"));
            refresh_all_workspaces(is_equal, config)
        }
        Some(Command::Status { json }) => print_status(json),
        Some(Command::Mode { mode }) => handle_mode(mode),
    }
}

fn refresh_focused_workspace(equal_grid: bool, config: layout::LayoutConfig) -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let Some(focused) = workspaces.iter().find(|ws| ws.is_focused) else {
        bail!("no focused workspace");
    };
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;

    let (width, height) = output_dimensions(&outputs, focused.output.as_deref());
    let usable_w = layout::calculate_usable_width(width, config.strut_left, config.strut_right);
    let usable_h = layout::calculate_usable_height(height, config.strut_top, config.strut_bottom);

    if equal_grid || snapshot::is_compressed(focused.id) {
        layout_equal_grid_2d(
            &mut socket,
            &windows,
            focused.id,
            usable_w,
            usable_h,
            config.gap,
        )
    } else {
        let scale = output_scale(&outputs, focused.output.as_deref());
        layout_workspace(&mut socket, &windows, focused.id, scale)
    }
}

fn refresh_all_workspaces(equal_grid: bool, config: layout::LayoutConfig) -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;

    for workspace in workspaces {
        let (width, height) = output_dimensions(&outputs, workspace.output.as_deref());
        let usable_w = layout::calculate_usable_width(width, config.strut_left, config.strut_right);
        let usable_h =
            layout::calculate_usable_height(height, config.strut_top, config.strut_bottom);

        if equal_grid || snapshot::is_compressed(workspace.id) {
            layout_equal_grid_2d(
                &mut socket,
                &windows,
                workspace.id,
                usable_w,
                usable_h,
                config.gap,
            )?;
        } else {
            let scale = output_scale(&outputs, workspace.output.as_deref());
            layout_workspace(&mut socket, &windows, workspace.id, scale)?;
        }
    }

    Ok(())
}

fn run_daemon(
    equal_grid: bool,
    config: layout::LayoutConfig,
    _exclusions: Vec<String>,
) -> Result<()> {
    let mut action_socket = Socket::connect()?;
    let mut event_socket = Socket::connect()?;

    let reply = event_socket.send(Request::EventStream)?;
    if !matches!(reply, Ok(Response::Handled)) {
        bail!("niri refused the event stream request");
    }

    let mut tracked: HashMap<u64, Membership> = HashMap::new();
    let mut workspace_outputs: HashMap<u64, Option<String>> = HashMap::new();
    let mut is_overview = false;

    let mut read_event = event_socket.read_events();
    loop {
        let mut dirty: Vec<u64> = Vec::new();

        match read_event()? {
            Event::OverviewOpenedOrClosed { is_open } => {
                is_overview = is_open;
                if !is_overview {
                    dirty.extend(
                        tracked
                            .values()
                            .filter_map(|&(ws, floating)| if floating { None } else { ws }),
                    );
                }
            }
            Event::WorkspaceActivated { id, focused } => {
                if focused {
                    dirty.push(id);
                }
            }
            Event::WorkspacesChanged { workspaces } => {
                let new_outputs: HashMap<u64, Option<String>> = workspaces
                    .iter()
                    .map(|ws| (ws.id, ws.output.clone()))
                    .collect();
                dirty.extend(
                    new_outputs
                        .iter()
                        .filter(|&(id, output)| workspace_outputs.get(id) != Some(output))
                        .map(|(&id, _)| id),
                );
                workspace_outputs = new_outputs;
            }
            Event::ConfigLoaded { .. } => {
                dirty.extend(
                    tracked
                        .values()
                        .filter_map(|&(ws, floating)| if floating { None } else { ws }),
                );
            }
            Event::WindowsChanged { windows } => {
                let new_tracked: HashMap<u64, (Option<u64>, bool)> =
                    windows.iter().map(|w| (w.id, membership(w))).collect();
                for (id, new_mem) in &new_tracked {
                    if let Some(old_mem) = tracked.get(id) {
                        if old_mem != new_mem {
                            if let (Some(ws), false) = new_mem {
                                dirty.push(*ws);
                            }
                            if let (Some(ws), false) = old_mem {
                                dirty.push(*ws);
                            }
                        }
                    } else if let (Some(ws), false) = new_mem {
                        dirty.push(*ws);
                    }
                }
                for (id, old_mem) in &tracked {
                    if !new_tracked.contains_key(id)
                        && let (Some(ws), false) = old_mem
                    {
                        dirty.push(*ws);
                    }
                }
                tracked = new_tracked;
            }
            Event::WindowOpenedOrChanged { window } => {
                let new = membership(&window);
                let old = tracked.insert(window.id, new);
                if old != Some(new) {
                    if let (Some(ws), false) = new {
                        dirty.push(ws);
                    }
                    if let Some((Some(ws), false)) = old {
                        dirty.push(ws);
                    }
                }
            }
            Event::WindowClosed { id } => {
                if let Some((Some(ws), false)) = tracked.remove(&id) {
                    dirty.push(ws);
                }
            }
            _ => {}
        }

        if is_overview {
            continue;
        }

        dirty.sort_unstable();
        dirty.dedup();
        if dirty.is_empty() {
            continue;
        }

        let mode_file_equal = std::fs::read_to_string("/tmp/niri-irene-mode")
            .ok()
            .map(|s| s.trim() != "master-stack");
        let active_equal = mode_file_equal.unwrap_or(equal_grid);

        let windows = fetch_windows(&mut action_socket)?;
        let outputs = fetch_outputs(&mut action_socket)?;
        for workspace_id in dirty {
            let output_name = workspace_outputs
                .get(&workspace_id)
                .and_then(|output| output.as_deref());

            let (width, height) = output_dimensions(&outputs, output_name);
            let usable_w =
                layout::calculate_usable_width(width, config.strut_left, config.strut_right);
            let usable_h =
                layout::calculate_usable_height(height, config.strut_top, config.strut_bottom);

            if active_equal || snapshot::is_compressed(workspace_id) {
                if let Err(err) = layout_equal_grid_2d(
                    &mut action_socket,
                    &windows,
                    workspace_id,
                    usable_w,
                    usable_h,
                    config.gap,
                ) {
                    eprintln!("layout failed for workspace {workspace_id}: {err}");
                }
            } else {
                let scale = output_scale(&outputs, output_name);
                if let Err(err) =
                    layout_workspace(&mut action_socket, &windows, workspace_id, scale)
                {
                    eprintln!("layout failed for workspace {workspace_id}: {err}");
                }
            }
        }
    }
}
