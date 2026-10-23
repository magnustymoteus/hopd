//! Pane mode: labels over the focused window. Typing one jumps the pointer there and
//! hands over to normal mode, for clicking and fine-tuning.

use log::{error, info, warn};
use smithay_client_toolkit::{
    compositor::Region,
    seat::keyboard::Keysym,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
};
use wayland_client::{
    QueueHandle,
    protocol::{wl_shm, wl_surface::WlSurface},
};

use crate::State;
use crate::hints::{Hints, Typed};
use crate::hyprland::{self, Monitor};
use crate::overlay::Overlay;
use crate::render::Area;

/// The layer-shell namespace, which Hyprland layer rules can match on.
pub const LAYER_NAMESPACE: &str = "hyprhopd";

pub struct PaneMode {
    /// The overlay covering the window's monitor.
    overlay: Overlay,
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
        if self.pane.is_some() || self.mode.is_some() {
            return;
        }
        // The previous overlay may still be fading out; it goes away now.
        self.fading_out = None;
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
        // Wayland names outputs the way Hyprland names monitors, like eDP-1.
        let Some(output) = self.output_state.outputs().find(|output| {
            self.output_state
                .info(output)
                .and_then(|info| info.name)
                .is_some_and(|name| name == monitor.name)
        }) else {
            warn!("no Wayland output for monitor {}", monitor.name);
            return;
        };

        let [x, y] = window.at.map(f64::from);
        let [width, height] = window.size.map(f64::from);
        let style = &self.config.pane;
        let hints = Hints::new(width, height, style.spacing, &style.chars);

        let surface = self.compositor.create_surface(qh);
        // An empty input region lets the pointer pass through to the window.
        match Region::new(&self.compositor) {
            Ok(region) => surface.set_input_region(Some(region.wl_region())),
            Err(err) => warn!("cannot make the overlay click-through: {err}"),
        }
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some(LAYER_NAMESPACE),
            Some(&output),
        );
        // Anchored to every edge with size 0: Hyprland sizes it to the whole monitor, and
        // -1 keeps bars' reserved space from shrinking it.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        // The first commit carries no buffer; drawing waits for Hyprland's configure.
        layer.commit();

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
            layout: layout_bounds(&monitors),
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
        self.renderer.draw(
            canvas,
            width,
            height,
            pane.area,
            &pane.hints,
            &self.config.pane,
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
        // Only keys that type exactly one character can be part of a label.
        let mut chars = text.unwrap_or_default().chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return;
        };
        match pane.hints.push(c) {
            Typed::Narrowed => self.draw_pane(),
            Typed::Picked { x, y } => self.finish_pane(x, y),
            Typed::Ignored => {}
        }
    }

    /// Jumps to the picked point, at `(x, y)` inside the window, and starts normal mode.
    fn finish_pane(&mut self, x: f64, y: f64) {
        let Some(pane) = self.pane.take() else {
            return;
        };
        let (left, top, width, height) = pane.layout;
        let target_x = pane.window_at.0 + x - left;
        let target_y = pane.window_at.1 + y - top;
        // The labels fade out while the pointer is already where it's going.
        self.retire(pane.overlay);
        self.virtual_pointer.motion_absolute(
            self.timestamp(),
            target_x.round() as u32,
            target_y.round() as u32,
            width.round() as u32,
            height.round() as u32,
        );
        self.virtual_pointer.frame();
        info!("pane mode off, jumped");
        self.enter_normal_mode();
    }

    pub fn cancel_pane(&mut self) {
        if let Some(pane) = self.pane.take() {
            self.retire(pane.overlay);
            info!("pane mode off");
        }
    }

    /// Fades an overlay out if pane_out asks for it, otherwise removes it at once.
    fn retire(&mut self, mut overlay: Overlay) {
        if overlay.fade_out(self.config.animations.pane_out, &self.qh) {
            self.fading_out = Some(overlay);
        }
    }

    pub fn owns_pane_surface(&self, surface: &WlSurface) -> bool {
        self.pane
            .as_ref()
            .is_some_and(|pane| pane.overlay.surface() == surface)
    }

    /// A frame callback for one of hyprhopd's overlays: time for the next fade step.
    pub fn overlay_frame(&mut self, surface: &WlSurface) {
        if let Some(pane) = self.pane.as_mut()
            && pane.overlay.surface() == surface
        {
            pane.overlay.frame(&self.qh);
        } else if let Some(overlay) = self.fading_out.as_mut()
            && overlay.surface() == surface
            && !overlay.frame(&self.qh)
        {
            self.fading_out = None;
        }
    }

    /// Hyprland removed an overlay, for example because its monitor went away.
    pub fn overlay_closed(&mut self, layer: &LayerSurface) {
        if self
            .pane
            .as_ref()
            .is_some_and(|pane| &pane.overlay.layer == layer)
        {
            self.pane = None;
            info!("pane mode off");
        } else if self
            .fading_out
            .as_ref()
            .is_some_and(|overlay| &overlay.layer == layer)
        {
            self.fading_out = None;
        }
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

/// Top-left corner and size of the box around every monitor, in layout coordinates.
fn layout_bounds(monitors: &[Monitor]) -> (f64, f64, f64, f64) {
    let boxes = monitors.iter().map(|monitor| {
        let (width, height) = monitor.logical_size();
        let (x, y) = (f64::from(monitor.x), f64::from(monitor.y));
        (x, y, x + width, y + height)
    });
    let (left, top, right, bottom) = boxes.fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(l, t, r, b), (x0, y0, x1, y1)| (l.min(x0), t.min(y0), r.max(x1), b.max(y1)),
    );
    (left, top, right - left, bottom - top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i32, y: i32, width: i32, height: i32, scale: f64, transform: i32) -> Monitor {
        Monitor {
            id: 0,
            name: String::new(),
            x,
            y,
            width,
            height,
            scale,
            transform,
        }
    }

    #[test]
    fn layout_covers_every_monitor_in_logical_pixels() {
        let monitors = [
            monitor(0, 0, 1920, 1080, 1.0, 0),
            // A 4K monitor at scale 2, to the right: 1920x1080 logical.
            monitor(1920, -200, 3840, 2160, 2.0, 0),
        ];
        assert_eq!(layout_bounds(&monitors), (0.0, -200.0, 3840.0, 1280.0));
    }

    #[test]
    fn rotated_monitors_swap_width_and_height() {
        let monitors = [monitor(0, 0, 1920, 1080, 1.0, 1)];
        assert_eq!(layout_bounds(&monitors), (0.0, 0.0, 1080.0, 1920.0));
    }
}
