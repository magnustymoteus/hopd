//! Event handlers hyprhopd has to provide for sctk and for the Hyprland and wlr protocols.
//! Most are empty: hyprhopd reacts to shortcuts, and in pane mode to its overlay's
//! configure events and keys.

use log::error;
use smithay_client_toolkit::{
    compositor::CompositorHandler,
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        protocols::wp::alpha_modifier::v1::client::{
            wp_alpha_modifier_surface_v1::{self, WpAlphaModifierSurfaceV1},
            wp_alpha_modifier_v1::{self, WpAlphaModifierV1},
        },
        protocols_wlr::virtual_pointer::v1::client::{
            zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
            zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_surface},
};

use crate::protocol::global_shortcuts::hyprland_global_shortcut_v1::{
    self, HyprlandGlobalShortcutV1,
};
use crate::protocol::global_shortcuts::hyprland_global_shortcuts_manager_v1::{
    self, HyprlandGlobalShortcutsManagerV1,
};
use crate::{Action, State};

// --- Hyprland global shortcuts ---

/// User data for the shortcuts manager, which has no events.
pub struct ManagerData;

impl Dispatch2<HyprlandGlobalShortcutsManagerV1, State> for ManagerData {
    fn event(
        &self,
        _: &mut State,
        _: &HyprlandGlobalShortcutsManagerV1,
        _: hyprland_global_shortcuts_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

// Each shortcut carries the Action it stands for as its user data.
impl Dispatch2<HyprlandGlobalShortcutV1, State> for Action {
    fn event(
        &self,
        state: &mut State,
        _: &HyprlandGlobalShortcutV1,
        event: hyprland_global_shortcut_v1::Event,
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let pressed = matches!(event, hyprland_global_shortcut_v1::Event::Pressed { .. });
        state.shortcut_event(*self, pressed, qh);
    }
}

// --- Virtual pointer ---

/// User data for the virtual-pointer manager and pointer; neither has events.
pub struct VirtualPointerData;

impl Dispatch2<ZwlrVirtualPointerManagerV1, State> for VirtualPointerData {
    fn event(
        &self,
        _: &mut State,
        _: &ZwlrVirtualPointerManagerV1,
        _: zwlr_virtual_pointer_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<ZwlrVirtualPointerV1, State> for VirtualPointerData {
    fn event(
        &self,
        _: &mut State,
        _: &ZwlrVirtualPointerV1,
        _: zwlr_virtual_pointer_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

// --- Alpha modifier, for fading overlays ---

/// User data for the alpha-modifier manager and its per-surface objects; neither has
/// events.
pub struct AlphaModifierData;

impl Dispatch2<WpAlphaModifierV1, State> for AlphaModifierData {
    fn event(
        &self,
        _: &mut State,
        _: &WpAlphaModifierV1,
        _: wp_alpha_modifier_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<WpAlphaModifierSurfaceV1, State> for AlphaModifierData {
    fn event(
        &self,
        _: &mut State,
        _: &WpAlphaModifierSurfaceV1,
        _: wp_alpha_modifier_surface_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

// --- Pane mode's overlay ---

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.overlay_closed(layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.pane_configured(configure.new_size);
    }
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        // Something else took the keyboard (a lock screen, say): typing can't continue.
        if self.owns_pane_surface(surface) {
            self.cancel_pane();
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.pane_key(event.keysym, event.utf8.as_deref());
    }

    // Labels are typed one press at a time; a held key shouldn't type it again.
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(err) => error!("cannot use the keyboard: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

// Frame callbacks drive fades; scale changes aren't handled yet.
impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.overlay_frame(surface);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

// --- Outputs, for monitor refresh rates and choosing the overlay's monitor ---

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);
