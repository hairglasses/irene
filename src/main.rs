use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use niri_ipc::{Event, Request, Response, socket::Socket};

use crate::utils::{
    DEFAULT_GAP, Membership, fetch_outputs, fetch_windows, fetch_workspaces, layout_equal_grid,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let global_equal = cli.equal_grid;
    let global_mode = cli.mode.as_deref();

    match cli.command {
        None => {
            let equal_grid = global_mode != Some("master-stack");
            run_daemon(equal_grid)
        }
        Some(Command::Daemon { equal_grid, mode }) => {
            let is_equal = equal_grid
                || global_equal
                || (mode.as_deref() != Some("master-stack") && global_mode != Some("master-stack"));
            run_daemon(is_equal)
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
            let usable_w = layout::calculate_usable_width(
                width,
                layout::DEFAULT_STRUT_LEFT,
                layout::DEFAULT_STRUT_RIGHT,
            );
            let is_comp = toggle_compression(&mut socket, workspace, usable_w, DEFAULT_GAP)?;
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
            refresh_focused_workspace(is_equal)
        }
        Some(Command::RefreshAllWorkspaces { equal_grid, mode }) => {
            let is_equal = equal_grid
                || global_equal
                || (mode.as_deref() != Some("master-stack") && global_mode != Some("master-stack"));
            refresh_all_workspaces(is_equal)
        }
    }
}

fn refresh_focused_workspace(equal_grid: bool) -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let Some(focused) = workspaces.iter().find(|ws| ws.is_focused) else {
        bail!("no focused workspace");
    };
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;

    if equal_grid || snapshot::is_compressed(focused.id) {
        let (width, _) = output_dimensions(&outputs, focused.output.as_deref());
        let usable_w = layout::calculate_usable_width(
            width,
            layout::DEFAULT_STRUT_LEFT,
            layout::DEFAULT_STRUT_RIGHT,
        );
        layout_equal_grid(&mut socket, &windows, focused.id, usable_w, DEFAULT_GAP)
    } else {
        let scale = output_scale(&outputs, focused.output.as_deref());
        layout_workspace(&mut socket, &windows, focused.id, scale)
    }
}

fn refresh_all_workspaces(equal_grid: bool) -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;

    for workspace in workspaces {
        if equal_grid || snapshot::is_compressed(workspace.id) {
            let (width, _) = output_dimensions(&outputs, workspace.output.as_deref());
            let usable_w = layout::calculate_usable_width(
                width,
                layout::DEFAULT_STRUT_LEFT,
                layout::DEFAULT_STRUT_RIGHT,
            );
            layout_equal_grid(&mut socket, &windows, workspace.id, usable_w, DEFAULT_GAP)?;
        } else {
            let scale = output_scale(&outputs, workspace.output.as_deref());
            layout_workspace(&mut socket, &windows, workspace.id, scale)?;
        }
    }

    Ok(())
}

fn run_daemon(equal_grid: bool) -> Result<()> {
    let mut action_socket = Socket::connect()?;
    let mut event_socket = Socket::connect()?;

    let reply = event_socket.send(Request::EventStream)?;
    if !matches!(reply, Ok(Response::Handled)) {
        bail!("niri refused the event stream request");
    }

    let mut tracked: HashMap<u64, Membership> = HashMap::new();
    let mut workspace_outputs: HashMap<u64, Option<String>> = HashMap::new();

    let mut read_event = event_socket.read_events();
    loop {
        let mut dirty: Vec<u64> = Vec::new();

        match read_event()? {
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
                tracked = windows.iter().map(|w| (w.id, membership(w))).collect();
                if let Ok(workspaces) = fetch_workspaces(&mut action_socket)
                    && let Some(focused) = workspaces.iter().find(|ws| ws.is_focused)
                {
                    dirty.push(focused.id);
                }
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

        dirty.sort_unstable();
        dirty.dedup();
        if dirty.is_empty() {
            continue;
        }

        let windows = fetch_windows(&mut action_socket)?;
        let outputs = fetch_outputs(&mut action_socket)?;
        for workspace_id in dirty {
            let output_name = workspace_outputs
                .get(&workspace_id)
                .and_then(|output| output.as_deref());

            if equal_grid || snapshot::is_compressed(workspace_id) {
                let (width, _) = output_dimensions(&outputs, output_name);
                let usable_w = layout::calculate_usable_width(
                    width,
                    layout::DEFAULT_STRUT_LEFT,
                    layout::DEFAULT_STRUT_RIGHT,
                );
                if let Err(err) = layout_equal_grid(
                    &mut action_socket,
                    &windows,
                    workspace_id,
                    usable_w,
                    DEFAULT_GAP,
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
