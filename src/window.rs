//! Window mode: a label on every window you can see, on every monitor. Typing one focuses
//! that window and puts the pointer at its centre.

use log::{error, info, warn};
use smithay_client_toolkit::{seat::keyboard::Keysym, shell::wlr_layer::LayerSurface};
use wayland_client::{
    QueueHandle,
    protocol::{wl_shm, wl_surface::WlSurface},
};

use crate::State;
use crate::hints::{Hints, Typed};
use crate::hyprland::{self, Client, Monitor};
use crate::overlay::Overlay;
use crate::pane::single_char;
use crate::render::Area;

pub struct WindowMode {
    /// One per monitor that has labelled windows.
    overlays: Vec<MonitorOverlay>,
    /// One at each window's centre, in layout coordinates.
    hints: Hints,
    /// Each hint's window address, in the same order as the hints.
    targets: Vec<String>,
    /// Top-left corner and size of the whole monitor layout, which absolute pointer
    /// motion is relative to.
    layout: (f64, f64, f64, f64),
}

struct MonitorOverlay {
    overlay: Overlay,
    /// The monitor's top-left corner in layout coordinates.
    origin: (f64, f64),
    /// The labelled windows on this monitor, in overlay coordinates, for the backdrop.
    areas: Vec<Area>,
    /// The overlay's size, known once Hyprland has configured it.
    size: Option<(u32, u32)>,
}

impl WindowMode {
    pub fn overlay_mut(&mut self, surface: &WlSurface) -> Option<&mut Overlay> {
        self.overlays
            .iter_mut()
            .map(|monitor| &mut monitor.overlay)
            .find(|overlay| overlay.surface() == surface)
    }
}

impl State {
    pub fn enter_window_mode(&mut self, qh: &QueueHandle<State>) {
        if self.busy() {
            return;
        }
        // Overlays still fading out go away now.
        self.fading_out.clear();
        let (monitors, clients) = match (hyprland::monitors(), hyprland::clients()) {
            (Ok(monitors), Ok(clients)) => (monitors, clients),
            (Err(err), _) | (_, Err(err)) => {
                warn!("window mode: cannot list monitors and windows: {err}");
                return;
            }
        };
        let windows = visible_windows(&clients, &monitors);
        let Some(&(first, _)) = windows.first() else {
            info!("window mode: no windows to label");
            return;
        };
        // Typing goes to the focused monitor's overlay, or to the first overlay if that
        // monitor has no windows and so no overlay.
        let keyboard = windows
            .iter()
            .find(|(monitor, _)| monitor.focused)
            .map_or(first.id, |(monitor, _)| monitor.id);
        // So the backdrop's corners match the windows'. Without it they're square.
        let radius = hyprland::window_rounding().unwrap_or_else(|err| {
            warn!("cannot read Hyprland's window rounding: {err}");
            0
        });

        let mut overlays = Vec::new();
        for monitor in &monitors {
            let areas: Vec<Area> = windows
                .iter()
                .filter(|(on, _)| on.id == monitor.id)
                .map(|(_, window)| Area {
                    x: (window.at[0] - monitor.x) as f32,
                    y: (window.at[1] - monitor.y) as f32,
                    width: window.size[0] as f32,
                    height: window.size[1] as f32,
                    radius: radius as f32,
                })
                .collect();
            if areas.is_empty() {
                continue;
            }
            // Without every overlay, some labels couldn't be seen; better not to start.
            let Some(layer) = self.overlay_layer(&monitor.name, monitor.id == keyboard, qh) else {
                return;
            };
            overlays.push(MonitorOverlay {
                overlay: Overlay::new(
                    layer,
                    self.alpha_modifier.as_ref(),
                    self.config.animations.window_in,
                    qh,
                ),
                origin: (f64::from(monitor.x), f64::from(monitor.y)),
                areas,
                size: None,
            });
        }

        let centres = windows.iter().map(|(_, window)| centre(window)).collect();
        let hints = Hints::at(centres, &self.config.window.style.chars);
        info!(
            "window mode on ({} windows on {} monitors)",
            windows.len(),
            overlays.len()
        );
        self.window = Some(WindowMode {
            overlays,
            hints,
            targets: windows
                .iter()
                .map(|(_, window)| window.address.clone())
                .collect(),
            layout: hyprland::layout_bounds(&monitors),
        });
    }

    pub fn window_configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        if let Some(index) = window
            .overlays
            .iter()
            .position(|monitor| &monitor.overlay.layer == layer)
        {
            window.overlays[index].size = Some(size);
            self.draw_window(index);
        }
    }

    /// Draws the overlay at `index` in the list.
    fn draw_window(&mut self, index: usize) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        let Some(monitor) = window.overlays.get_mut(index) else {
            return;
        };
        let Some((width, height)) = monitor.size.filter(|&(w, h)| w > 0 && h > 0) else {
            return;
        };
        let (buffer, canvas) = match self.pool.create_buffer(
            width as i32,
            height as i32,
            width as i32 * 4,
            wl_shm::Format::Argb8888,
        ) {
            Ok(created) => created,
            Err(err) => {
                error!("cannot allocate the overlay: {err}");
                return;
            }
        };
        // Hints are in layout coordinates; this overlay starts at its monitor's corner.
        let origin = (-monitor.origin.0 as f32, -monitor.origin.1 as f32);
        self.renderer.draw(
            canvas,
            width,
            height,
            &monitor.areas,
            origin,
            &window.hints,
            &self.config.window.style,
        );
        if let Err(err) = monitor.overlay.show(buffer, (width, height), &self.qh) {
            error!("cannot show the overlay: {err}");
        }
    }

    fn redraw_window(&mut self) {
        let count = self
            .window
            .as_ref()
            .map_or(0, |window| window.overlays.len());
        for index in 0..count {
            self.draw_window(index);
        }
    }

    pub fn window_key(&mut self, keysym: Keysym, text: Option<&str>) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        match keysym {
            Keysym::Escape => return self.cancel_window(),
            Keysym::BackSpace => {
                window.hints.pop();
                return self.redraw_window();
            }
            _ => {}
        }
        let Some(c) = single_char(text) else {
            return;
        };
        match window.hints.push(c) {
            Typed::Narrowed => self.redraw_window(),
            Typed::Picked { index, x, y } => self.finish_window(index, x, y),
            Typed::Ignored => {}
        }
    }

    /// Focuses the picked window, whose centre is at `(x, y)`, and puts the pointer there.
    fn finish_window(&mut self, index: usize, x: f64, y: f64) {
        let Some(window) = self.window.take() else {
            return;
        };
        for monitor in window.overlays {
            self.retire(monitor.overlay, self.config.animations.window_out);
        }
        let address = &window.targets[index];
        if let Err(err) = hyprland::focus_window(address) {
            warn!("cannot focus window {address}: {err}");
        }
        // Done here rather than left to Hyprland, so it happens even with
        // cursor:no_warps, and with follow_mouse the focus then stays put.
        self.jump_pointer(x, y, window.layout);
        info!("window mode off, focused {address}");
    }

    pub fn cancel_window(&mut self) {
        if let Some(window) = self.window.take() {
            for monitor in window.overlays {
                self.retire(monitor.overlay, self.config.animations.window_out);
            }
            info!("window mode off");
        }
    }

    pub fn owns_window_surface(&self, surface: &WlSurface) -> bool {
        self.window.as_ref().is_some_and(|window| {
            window
                .overlays
                .iter()
                .any(|monitor| monitor.overlay.surface() == surface)
        })
    }

    pub fn owns_window_layer(&self, layer: &LayerSurface) -> bool {
        self.window.as_ref().is_some_and(|window| {
            window
                .overlays
                .iter()
                .any(|monitor| &monitor.overlay.layer == layer)
        })
    }

    /// Cancels window mode when any window, workspace or monitor changes, since labels
    /// could then point at the wrong window or at nothing.
    pub fn window_hyprland_event(&mut self, event: &str) {
        // Hyprland sends most events in two versions; one of each is enough here.
        let cancel = matches!(
            event,
            "openwindow"
                | "closewindow"
                | "movewindowv2"
                | "changefloatingmode"
                | "fullscreen"
                | "workspacev2"
                | "activespecial"
                | "moveworkspacev2"
                | "monitoradded"
                | "monitorremoved"
        );
        if cancel && self.window.is_some() {
            info!("window mode: the windows changed");
            self.cancel_window();
        }
    }
}

/// The windows that can be seen right now, each with its monitor, in label order: monitor
/// by monitor from left to right, then top to bottom and left to right on each.
fn visible_windows<'a>(
    clients: &'a [Client],
    monitors: &'a [Monitor],
) -> Vec<(&'a Monitor, &'a Client)> {
    let mut monitors: Vec<&Monitor> = monitors.iter().collect();
    monitors.sort_by_key(|monitor| (monitor.x, monitor.y));
    let mut visible = Vec::new();
    for monitor in monitors {
        // An open special workspace covers the regular one, and has the focus.
        let workspace = match monitor.special_workspace.id {
            0 => monitor.active_workspace.id,
            special => special,
        };
        let mut shown: Vec<&Client> = clients
            .iter()
            .filter(|client| {
                client.mapped
                    && !client.hidden
                    && client.workspace.id == workspace
                    && client.size[0] > 0
                    && client.size[1] > 0
            })
            .collect();
        // A fullscreen or maximized window covers the rest.
        if let Some(&full) = shown.iter().find(|client| client.fullscreen != 0) {
            shown = vec![full];
        }
        shown.sort_by_key(|client| (client.at[1], client.at[0]));
        visible.extend(shown.into_iter().map(|client| (monitor, client)));
    }
    visible
}

fn centre(window: &Client) -> (f64, f64) {
    (
        f64::from(window.at[0]) + f64::from(window.size[0]) / 2.0,
        f64::from(window.at[1]) + f64::from(window.size[1]) / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyprland::WorkspaceRef;
    use crate::hyprland::tests::monitor;

    fn client(address: &str, workspace: i64, at: [i32; 2]) -> Client {
        Client {
            address: address.into(),
            at,
            size: [100, 100],
            workspace: WorkspaceRef { id: workspace },
            mapped: true,
            hidden: false,
            fullscreen: 0,
        }
    }

    fn addresses(visible: &[(&Monitor, &Client)]) -> Vec<String> {
        visible
            .iter()
            .map(|(_, client)| client.address.clone())
            .collect()
    }

    #[test]
    fn only_the_shown_workspace_counts_in_reading_order() {
        let monitors = [monitor(0, (0, 0), (1920, 1080), 1.0, 0)];
        let mut unmapped = client("unmapped", 1, [0, 0]);
        unmapped.mapped = false;
        let mut tab = client("background-tab", 1, [0, 0]);
        tab.hidden = true;
        let clients = [
            client("bottom", 1, [0, 600]),
            client("elsewhere", 2, [0, 0]),
            client("top-right", 1, [900, 0]),
            client("top-left", 1, [0, 0]),
            unmapped,
            tab,
        ];
        let visible = visible_windows(&clients, &monitors);
        assert_eq!(addresses(&visible), ["top-left", "top-right", "bottom"]);
    }

    #[test]
    fn monitors_go_left_to_right() {
        let mut right = monitor(1, (1920, 0), (1920, 1080), 1.0, 0);
        right.active_workspace = WorkspaceRef { id: 2 };
        let monitors = [right, monitor(0, (0, 0), (1920, 1080), 1.0, 0)];
        let clients = [
            client("on-right", 2, [1920, 0]),
            client("on-left", 1, [0, 500]),
        ];
        let visible = visible_windows(&clients, &monitors);
        assert_eq!(addresses(&visible), ["on-left", "on-right"]);
        assert_eq!(visible[1].0.id, 1);
    }

    #[test]
    fn an_open_special_workspace_hides_the_regular_one() {
        let mut shown = monitor(0, (0, 0), (1920, 1080), 1.0, 0);
        shown.special_workspace = WorkspaceRef { id: -98 };
        let clients = [
            client("regular", 1, [0, 0]),
            client("scratchpad", -98, [0, 0]),
        ];
        let visible = visible_windows(&clients, std::slice::from_ref(&shown));
        assert_eq!(addresses(&visible), ["scratchpad"]);
    }

    #[test]
    fn a_fullscreen_window_is_the_only_one() {
        let monitors = [monitor(0, (0, 0), (1920, 1080), 1.0, 0)];
        let mut full = client("fullscreen", 1, [500, 0]);
        full.fullscreen = 2;
        let clients = [client("behind", 1, [0, 0]), full];
        let visible = visible_windows(&clients, &monitors);
        assert_eq!(addresses(&visible), ["fullscreen"]);
    }

    #[test]
    fn labels_go_at_window_centres() {
        let mut window = client("a", 1, [10, 20]);
        window.size = [301, 100];
        assert_eq!(centre(&window), (160.5, 70.0));
    }
}
