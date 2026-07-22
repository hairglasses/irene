use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use niri_ipc::{Event, Request, Response, socket::Socket};

use crate::utils::{
    Membership, fetch_outputs, fetch_windows, fetch_workspaces, layout_workspace, membership,
    output_scale,
};

mod utils;

/// Master layout for niri: one window on the left, two stacked on the right,
/// repeating.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Watch niri events and keep workspaces in the pattern (the default).
    Daemon,

    /// Re-layout the focused workspace once.
    RefreshFocusedWorkspace,

    /// Re-layout all workspaces
    RefreshAllWorkspaces,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        None | Some(Command::Daemon) => run_daemon(),
        Some(Command::RefreshFocusedWorkspace) => refresh_focused_workspace(),
        Some(Command::RefreshAllWorkspaces) => refresh_all_workspaces(),
    }
}

fn refresh_focused_workspace() -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let Some(focused) = workspaces.iter().find(|ws| ws.is_focused) else {
        bail!("no focused workspace");
    };
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;
    let scale = output_scale(&outputs, focused.output.as_deref());
    layout_workspace(&mut socket, &windows, focused.id, scale)
}

fn refresh_all_workspaces() -> Result<()> {
    let mut socket = Socket::connect()?;
    let workspaces = fetch_workspaces(&mut socket)?;
    let windows = fetch_windows(&mut socket)?;
    let outputs = fetch_outputs(&mut socket)?;

    for workspace in workspaces {
        let scale = output_scale(&outputs, workspace.output.as_deref());
        layout_workspace(&mut socket, &windows, workspace.id, scale)?;
    }

    Ok(())
}

fn run_daemon() -> Result<()> {
    let mut action_socket = Socket::connect()?;
    let mut event_socket = Socket::connect()?;

    let reply = event_socket.send(Request::EventStream)?;
    if !matches!(reply, Ok(Response::Handled)) {
        bail!("niri refused the event stream request");
    }

    let mut tracked: HashMap<u64, Membership> = HashMap::new();

    // Which output each workspace is on, from WorkspacesChanged events.
    let mut workspace_outputs: HashMap<u64, Option<String>> = HashMap::new();

    let mut read_event = event_socket.read_events();
    loop {
        let mut dirty: Vec<u64> = Vec::new();

        match read_event()? {
            // Also fires when outputs connect or disconnect, since that
            // reassigns workspaces to outputs. A workspace on a new output
            // needs a relayout: the lone-window width in logical pixels
            // depends on the output's scale.
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
            // A config reload can change output scales.
            Event::ConfigLoaded { .. } => {
                dirty.extend(
                    tracked
                        .values()
                        .filter_map(|&(ws, floating)| if floating { None } else { ws }),
                );
            }
            // Sent once when the stream starts: seed state, lay out everything.
            Event::WindowsChanged { windows } => {
                tracked = windows.iter().map(|w| (w.id, membership(w))).collect();
                dirty.extend(
                    tracked
                        .values()
                        .filter_map(|&(ws, floating)| if floating { None } else { ws }),
                );
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
            let scale = output_scale(
                &outputs,
                workspace_outputs
                    .get(&workspace_id)
                    .and_then(|output| output.as_deref()),
            );
            if let Err(err) = layout_workspace(&mut action_socket, &windows, workspace_id, scale) {
                eprintln!("layout failed for workspace {workspace_id}: {err}");
            }
        }
    }
}
