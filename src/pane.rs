//! Pane mode: labels over the focused window. Typing one jumps the pointer there and
//! hands over to normal mode, for clicking and fine-tuning.

use smithay_client_toolkit::{
    compositor::Region,
    seat::keyboard::Keysym,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
    shm::slot::Buffer,
};
use wayland_client::{
    QueueHandle,
    protocol::{wl_shm, wl_surface::WlSurface},
};

use crate::State;
use crate::hints::{Hints, Typed};
use crate::hyprland::{self, Monitor};

/// The layer-shell namespace, which Hyprland layer rules can match on.
const LAYER_NAMESPACE: &str = "hyprhopd";

pub struct PaneMode {
    /// The overlay covering the window's monitor. Dropping it removes it from the screen.
    layer: LayerSurface,
    hints: Hints,
    /// The target window's address, as events write it.
    window: String,
    /// The window's top-left corner on the overlay.
    origin: (f64, f64),
    /// The window's top-left corner in layout coordinates.
    window_at: (f64, f64),
    /// Top-left corner and size of the whole monitor layout, which absolute pointer
    /// motion is relative to.
    layout: (f64, f64, f64, f64),
    /// The overlay's size, known once Hyprland has configured it.
    size: Option<(u32, u32)>,
    /// The buffer on screen, kept alive while Hyprland may still read it.
    buffer: Option<Buffer>,
}

impl State {
    pub fn enter_pane_mode(&mut self, qh: &QueueHandle<State>) {
        if self.pane.is_some() || self.mode.is_some() {
            return;
        }
        let window = match hyprland::active_window() {
            Ok(Some(window)) => window,
            Ok(None) => {
                println!("pane mode: no window is focused");
                return;
            }
            Err(err) => {
                eprintln!("hyprhopd: cannot find the focused window: {err}");
                return;
            }
        };
        let monitors = match hyprland::monitors() {
            Ok(monitors) => monitors,
            Err(err) => {
                eprintln!("hyprhopd: cannot list monitors: {err}");
                return;
            }
        };
        let Some(monitor) = monitors.iter().find(|m| m.id == window.monitor) else {
            eprintln!("hyprhopd: the focused window's monitor is missing");
            return;
        };
        // Wayland names outputs the way Hyprland names monitors, like eDP-1.
        let Some(output) = self.output_state.outputs().find(|output| {
            self.output_state
                .info(output)
                .and_then(|info| info.name)
                .is_some_and(|name| name == monitor.name)
        }) else {
            eprintln!("hyprhopd: no Wayland output for monitor {}", monitor.name);
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
            Err(err) => eprintln!("hyprhopd: cannot make the overlay click-through: {err}"),
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

        println!("pane mode on ({} labels)", hints.visible().count());
        self.pane = Some(PaneMode {
            layer,
            hints,
            window: window.event_address().to_string(),
            origin: (x - f64::from(monitor.x), y - f64::from(monitor.y)),
            window_at: (x, y),
            layout: layout_bounds(&monitors),
            size: None,
            buffer: None,
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
                eprintln!("hyprhopd: cannot allocate the overlay: {err}");
                return;
            }
        };
        self.renderer.draw(
            canvas,
            width,
            height,
            pane.origin,
            &pane.hints,
            &self.config.pane,
        );
        let surface = pane.layer.wl_surface();
        if let Err(err) = buffer.attach_to(surface) {
            eprintln!("hyprhopd: cannot show the overlay: {err}");
            return;
        }
        surface.damage_buffer(0, 0, width as i32, height as i32);
        pane.layer.commit();
        pane.buffer = Some(buffer);
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
        drop(pane);
        self.virtual_pointer.motion_absolute(
            self.timestamp(),
            target_x.round() as u32,
            target_y.round() as u32,
            width.round() as u32,
            height.round() as u32,
        );
        self.virtual_pointer.frame();
        println!("pane mode off, jumped");
        self.enter_normal_mode();
    }

    pub fn cancel_pane(&mut self) {
        if self.pane.take().is_some() {
            println!("pane mode off");
        }
    }

    pub fn owns_pane_surface(&self, surface: &WlSurface) -> bool {
        self.pane
            .as_ref()
            .is_some_and(|pane| pane.layer.wl_surface() == surface)
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
            println!("pane mode: the window changed");
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
