use std::collections::HashMap;

use anyhow::{Result, bail};
use niri_ipc::{Action, Event, Output, Request, Response, SizeChange, Window, socket::Socket};

/// Max width for a lone window, in physical (device) pixels. Niri sizes
/// windows in logical pixels, so divide by the output scale before use.
const MAX_WINDOW_WIDTH: f64 = 2400.0;
const COLUMN_PROPORTION: f64 = 50.0;

/// What determines a window's place in the pattern: the workspace it is on
/// and whether it tiles at all. Relayouts happen only when this changes for
/// some window, so our own resize/consume actions never re-trigger us.
type Membership = (Option<u64>, bool);

fn membership(window: &Window) -> Membership {
    (window.workspace_id, window.is_floating)
}

fn send_action(socket: &mut Socket, action: Action) -> Result<()> {
    match socket.send(Request::Action(action))? {
        Ok(_) => Ok(()),
        Err(msg) => bail!("niri rejected action: {msg}"),
    }
}

fn fetch_windows(socket: &mut Socket) -> Result<Vec<Window>> {
    match socket.send(Request::Windows)? {
        Ok(Response::Windows(windows)) => Ok(windows),
        Ok(_) => bail!("unexpected response to Windows request"),
        Err(msg) => bail!("niri rejected Windows request: {msg}"),
    }
}

fn fetch_outputs(socket: &mut Socket) -> Result<HashMap<String, Output>> {
    match socket.send(Request::Outputs)? {
        Ok(Response::Outputs(outputs)) => Ok(outputs),
        Ok(_) => bail!("unexpected response to Outputs request"),
        Err(msg) => bail!("niri rejected Outputs request: {msg}"),
    }
}

/// Apply the repeating [solo][pair] master pattern to one workspace.
/// `scale` is the scale factor of the output the workspace is on.
fn layout_workspace(
    socket: &mut Socket,
    windows: &[Window],
    workspace_id: u64,
    scale: f64,
) -> Result<()> {
    // Tiled windows on the workspace in visual order (column, then row).
    let mut tiles: Vec<(u64, (usize, usize))> = windows
        .iter()
        .filter(|w| w.workspace_id == Some(workspace_id) && !w.is_floating)
        .filter_map(|w| w.layout.pos_in_scrolling_layout.map(|pos| (w.id, pos)))
        .collect();
    tiles.sort_by_key(|&(_, pos)| pos);

    if tiles.len() == 1 {
        return send_action(
            socket,
            Action::SetWindowWidth {
                id: Some(tiles[0].0),
                change: SizeChange::SetFixed((MAX_WINDOW_WIDTH / scale).round() as i32),
            },
        );
    }

    // Flatten: expel every stacked window into its own column. Expelling
    // bottom-first puts each window in a new column immediately to the right
    // of its old one, which preserves the overall window order. Indices are
    // 1-based, so a tile below the top of its column has row > 1.
    let mut stacked: Vec<(u64, (usize, usize))> = tiles
        .iter()
        .copied()
        .filter(|&(_, (_, row))| row > 1)
        .collect();
    stacked.sort_by_key(|&(_, (col, row))| (col, std::cmp::Reverse(row)));
    for (id, _) in stacked {
        send_action(socket, Action::ConsumeOrExpelWindowRight { id: Some(id) })?;
    }

    // Rebuild the repeating [solo][pair] pattern with half-width columns.
    for group in tiles.chunks(3) {
        for &(id, _) in &group[..group.len().min(2)] {
            send_action(
                socket,
                Action::SetWindowWidth {
                    id: Some(id),
                    change: SizeChange::SetProportion(COLUMN_PROPORTION),
                },
            )?;
        }
        if let &[_, (top, _), (bottom, _)] = group {
            send_action(socket, Action::ConsumeOrExpelWindowLeft { id: Some(bottom) })?;
            for id in [top, bottom] {
                send_action(socket, Action::ResetWindowHeight { id: Some(id) })?;
            }
        }
    }

    // Bring the focused window's pattern group fully into view. Opening a
    // window can leave the view scrolled so that the other column of the
    // group hangs off-screen (e.g. a lone 2400px column plus a new 2400px
    // default-width column overflow the output before we shrink them).
    // Walking focus across the group scrolls it into view, since a full
    // group is exactly screen-wide; restoring focus then doesn't scroll.
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

fn main() -> Result<()> {
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
            let scale = workspace_outputs
                .get(&workspace_id)
                .and_then(|output| output.as_deref())
                .and_then(|name| outputs.get(name))
                .and_then(|output| output.logical.as_ref())
                .map_or(1.0, |logical| logical.scale);
            if let Err(err) = layout_workspace(&mut action_socket, &windows, workspace_id, scale) {
                eprintln!("layout failed for workspace {workspace_id}: {err}");
            }
        }
    }
}
