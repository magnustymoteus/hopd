//! A layer surface that can fade in and out. Instead of redrawing, hyprhopd asks Hyprland
//! to scale the surface's opacity (wp_alpha_modifier_v1), once per frame callback.

use std::time::Instant;

use smithay_client_toolkit::{
    compositor::FrameCallbackData,
    reexports::protocols::wp::alpha_modifier::v1::client::{
        wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
        wp_alpha_modifier_v1::WpAlphaModifierV1,
    },
    shell::{
        WaylandSurface,
        wlr_layer::{KeyboardInteractivity, LayerSurface},
    },
    shm::slot::Buffer,
};
use wayland_client::{QueueHandle, protocol::wl_surface::WlSurface};

use crate::State;
use crate::config::Animation;
use crate::handlers::AlphaModifierData;

struct Fade {
    animation: Animation,
    from: f64,
    to: f64,
    /// Unset until the first frame is on screen, so waiting for Hyprland's configure
    /// doesn't eat into the fade.
    start: Option<Instant>,
}

impl Fade {
    /// The opacity at `now`, and whether the fade has finished.
    fn at(&self, now: Instant) -> (f64, bool) {
        let Some(start) = self.start else {
            return (self.from, false);
        };
        let progress = (now - start).as_secs_f64() / self.animation.duration.as_secs_f64();
        if progress >= 1.0 {
            return (self.to, true);
        }
        let eased = self.animation.curve.at(progress);
        (self.from + (self.to - self.from) * eased, false)
    }
}

pub struct Overlay {
    /// Dropping the overlay drops this, which removes the surface from the screen.
    pub layer: LayerSurface,
    /// Absent when Hyprland lacks wp_alpha_modifier_v1; fades are skipped then.
    alpha: Option<WpAlphaModifierSurfaceV1>,
    /// The buffer on screen, kept alive while Hyprland may still read it.
    buffer: Option<Buffer>,
    fade: Option<Fade>,
    /// The opacity last sent to Hyprland.
    opacity: f64,
}

impl Overlay {
    /// Wraps a layer surface that hasn't shown anything yet. With a non-zero `fade_in`, it
    /// starts invisible and fades in from its first frame.
    pub fn new(
        layer: LayerSurface,
        alpha_modifier: Option<&WpAlphaModifierV1>,
        fade_in: Animation,
        qh: &QueueHandle<State>,
    ) -> Overlay {
        let alpha = alpha_modifier
            .map(|manager| manager.get_surface(layer.wl_surface(), qh, AlphaModifierData));
        let mut overlay = Overlay {
            layer,
            alpha,
            buffer: None,
            fade: None,
            opacity: 1.0,
        };
        if !fade_in.duration.is_zero() && overlay.alpha.is_some() {
            overlay.set_opacity(0.0);
            overlay.fade = Some(Fade {
                animation: fade_in,
                from: 0.0,
                to: 1.0,
                start: None,
            });
        }
        overlay
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    /// Puts `buffer` on screen. The fade-in, if there is one, starts with the first.
    pub fn show(
        &mut self,
        buffer: Buffer,
        (width, height): (u32, u32),
        qh: &QueueHandle<State>,
    ) -> Result<(), String> {
        let surface = self.layer.wl_surface();
        buffer.attach_to(surface).map_err(|err| err.to_string())?;
        surface.damage_buffer(0, 0, width as i32, height as i32);
        if let Some(fade) = self.fade.as_mut()
            && fade.start.is_none()
        {
            fade.start = Some(Instant::now());
            surface.frame(qh, FrameCallbackData(surface.clone()));
        }
        self.layer.commit();
        self.buffer = Some(buffer);
        Ok(())
    }

    /// Releases the keyboard at once and starts fading out from the current opacity.
    /// Returns false if there's nothing to fade, in which case the caller drops it.
    pub fn fade_out(&mut self, animation: Animation, qh: &QueueHandle<State>) -> bool {
        if animation.duration.is_zero() || self.alpha.is_none() || self.buffer.is_none() {
            return false;
        }
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        // A fade-in still running already has a frame callback on the way.
        if self.fade.is_none() {
            let surface = self.layer.wl_surface();
            surface.frame(qh, FrameCallbackData(surface.clone()));
        }
        self.fade = Some(Fade {
            animation,
            from: self.opacity,
            to: 0.0,
            start: Some(Instant::now()),
        });
        self.layer.commit();
        true
    }

    /// Advances the fade; called on each frame callback. Returns whether it's still
    /// running.
    pub fn frame(&mut self, qh: &QueueHandle<State>) -> bool {
        let Some(fade) = &self.fade else {
            return false;
        };
        let (opacity, done) = fade.at(Instant::now());
        self.set_opacity(opacity);
        let surface = self.layer.wl_surface();
        if done {
            self.fade = None;
        } else {
            surface.frame(qh, FrameCallbackData(surface.clone()));
        }
        // Damage makes sure Hyprland repaints, and so sends the next frame callback,
        // even though the pixels haven't changed.
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.layer.commit();
        !done
    }

    /// Takes effect on the next commit.
    fn set_opacity(&mut self, opacity: f64) {
        let opacity = opacity.clamp(0.0, 1.0);
        if let Some(alpha) = &self.alpha {
            // The multiplier runs from 0 (invisible) to u32::MAX (unchanged).
            alpha.set_multiplier((opacity * f64::from(u32::MAX)) as u32);
        }
        self.opacity = opacity;
    }
}

impl Drop for Overlay {
    // wayland-client doesn't destroy protocol objects when they're dropped.
    fn drop(&mut self) {
        if let Some(alpha) = self.alpha.take() {
            alpha.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::easing::Curve;
    use std::time::Duration;

    fn fade(from: f64, to: f64) -> (Fade, Instant) {
        let start = Instant::now();
        let fade = Fade {
            animation: Animation {
                duration: Duration::from_millis(100),
                curve: Curve::LINEAR,
            },
            from,
            to,
            start: Some(start),
        };
        (fade, start)
    }

    #[test]
    fn fades_move_from_start_to_end_value() {
        let (fade, start) = fade(0.2, 1.0);
        assert_eq!(fade.at(start), (0.2, false));
        let (halfway, done) = fade.at(start + Duration::from_millis(50));
        assert!((halfway - 0.6).abs() < 1e-6 && !done, "{halfway}");
        assert_eq!(fade.at(start + Duration::from_millis(150)), (1.0, true));
    }

    #[test]
    fn unstarted_fades_hold_their_start_value() {
        let (mut fade, start) = fade(0.0, 1.0);
        fade.start = None;
        assert_eq!(fade.at(start + Duration::from_secs(5)), (0.0, false));
    }
}
