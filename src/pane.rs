//! Pane mode: labels over the focused window. Typing one jumps the pointer there and
//! hands over to normal mode, for clicking and fine-tuning.

use log::{error, info, warn};
use smithay_client_toolkit::{seat::keyboard::Keysym, shell::wlr_layer::LayerSurface};
use wayland_client::{
    QueueHandle,
    protocol::{wl_shm, wl_surface::WlSurface},
};

use crate::State;
use crate::hints::{Hints, Typed};
use crate::hyprland;
use crate::overlay::Overlay;
use crate::render::Area;

pub struct PaneMode {
    /// The overlay covering the window's monitor.
    pub overlay: Overlay,
    hints: Hints,
    /// The target window's address, as events write it.
    window: String,
    /// Where the window is on the overlay.
    area: Area,
    /// The window's top-left corner in layout coordinates.
    window_at: (f64, f64),
    /// Top-left corner and size of the whole monitor layout, which absolute pointer
    /// motion is relative to.
    layout: (f64, f64, f64, f64),
    /// The overlay's size, known once Hyprland has configured it.
    size: Option<(u32, u32)>,
}

impl State {
    pub fn enter_pane_mode(&mut self, qh: &QueueHandle<State>) {
        if self.busy() {
            return;
        }
        // Overlays still fading out go away now.
        self.fading_out.clear();
        let window = match hyprland::active_window() {
            Ok(Some(window)) => window,
            Ok(None) => {
                info!("pane mode: no window is focused");
                return;
            }
            Err(err) => {
                warn!("cannot find the focused window: {err}");
                return;
            }
        };
        let monitors = match hyprland::monitors() {
            Ok(monitors) => monitors,
            Err(err) => {
                warn!("cannot list monitors: {err}");
                return;
            }
        };
        let Some(monitor) = monitors.iter().find(|m| m.id == window.monitor) else {
            warn!("the focused window's monitor is missing");
            return;
        };
        let Some(layer) = self.overlay_layer(&monitor.name, true, qh) else {
            return;
        };

        let [x, y] = window.at.map(f64::from);
        let [width, height] = window.size.map(f64::from);
        let pane = &self.config.pane;
        let hints = Hints::grid(width, height, pane.spacing, &pane.style.chars);

        // So the backdrop's corners match the window's. Without it they're square.
        let radius = hyprland::window_rounding().unwrap_or_else(|err| {
            warn!("cannot read Hyprland's window rounding: {err}");
            0
        });
        let area = Area {
            x: (x - f64::from(monitor.x)) as f32,
            y: (y - f64::from(monitor.y)) as f32,
            width: width as f32,
            height: height as f32,
            radius: radius as f32,
        };

        info!("pane mode on ({} labels)", hints.visible().count());
        let overlay = Overlay::new(
            layer,
            self.alpha_modifier.as_ref(),
            self.config.animations.pane_in,
            qh,
        );
        self.pane = Some(PaneMode {
            overlay,
            hints,
            window: window.event_address().to_string(),
            area,
            window_at: (x, y),
            layout: hyprland::layout_bounds(&monitors),
            size: None,
        });
    }

    pub fn pane_configured(&mut self, (width, height): (u32, u32)) {
        if let Some(pane) = self.pane.as_mut() {
            pane.size = Some((width, height));
            self.draw_pane();
        }
    }

    fn draw_pane(&mut self) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        let Some((width, height)) = pane.size.filter(|&(w, h)| w > 0 && h > 0) else {
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
        // Hint positions are relative to the window.
        self.renderer.draw(
            canvas,
            width,
            height,
            &[pane.area],
            (pane.area.x, pane.area.y),
            &pane.hints,
            &self.config.pane.style,
        );
        if let Err(err) = pane.overlay.show(buffer, (width, height), &self.qh) {
            error!("cannot show the overlay: {err}");
        }
    }

    pub fn pane_key(&mut self, keysym: Keysym, text: Option<&str>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        match keysym {
            Keysym::Escape => return self.cancel_pane(),
            Keysym::BackSpace => {
                pane.hints.pop();
                return self.draw_pane();
            }
            _ => {}
        }
        let Some(c) = single_char(text) else {
            return;
        };
        match pane.hints.push(c) {
            Typed::Narrowed => self.draw_pane(),
            Typed::Picked { x, y, .. } => self.finish_pane(x, y),
            Typed::Ignored => {}
        }
    }

    /// Jumps to the picked point, at `(x, y)` inside the window, and starts normal mode.
    fn finish_pane(&mut self, x: f64, y: f64) {
        let Some(pane) = self.pane.take() else {
            return;
        };
        // The labels fade out while the pointer is already where it's going.
        self.retire(pane.overlay, self.config.animations.pane_out);
        self.jump_pointer(pane.window_at.0 + x, pane.window_at.1 + y, pane.layout);
        info!("pane mode off, jumped");
        self.enter_normal_mode();
    }

    pub fn cancel_pane(&mut self) {
        if let Some(pane) = self.pane.take() {
            self.retire(pane.overlay, self.config.animations.pane_out);
            info!("pane mode off");
        }
    }

    pub fn owns_pane_surface(&self, surface: &WlSurface) -> bool {
        self.pane
            .as_ref()
            .is_some_and(|pane| pane.overlay.surface() == surface)
    }

    pub fn owns_pane_layer(&self, layer: &LayerSurface) -> bool {
        self.pane
            .as_ref()
            .is_some_and(|pane| &pane.overlay.layer == layer)
    }

    /// Cancels pane mode when its window closes, moves or changes shape, since the
    /// labels would then point at the wrong place.
    pub fn pane_hyprland_event(&mut self, event: &str, data: &str) {
        let Some(pane) = self.pane.as_ref() else {
            return;
        };
        // These events start with the window's address, then a comma.
        let is_target = data.split(',').next() == Some(pane.window.as_str());
        let cancel = match event {
            "closewindow" | "movewindowv2" | "changefloatingmode" => is_target,
            "fullscreen" | "workspacev2" | "monitorremoved" => true,
            _ => false,
        };
        if cancel {
            info!("pane mode: the window changed");
            self.cancel_pane();
        }
    }
}

/// The character a key typed, if it typed exactly one: only those can be part of a label.
pub fn single_char(text: Option<&str>) -> Option<char> {
    let mut chars = text.unwrap_or_default().chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}
