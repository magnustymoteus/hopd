//! Event handlers hyprhopd has to provide for sctk and for the Hyprland shortcut protocol.
//! Most are empty: hyprhopd only cares about shortcuts, keyboard focus, keys and configures.

use smithay_client_toolkit::{
    compositor::CompositorHandler,
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
        zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
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
        if let hyprland_global_shortcut_v1::Event::Pressed { .. } = event {
            state.shortcut_pressed(*self, qh);
        }
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

// --- Layer surface ---

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor removed our surface (e.g. its output went away): leave the mode.
        self.exit_mode();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.surface_configured();
    }
}

// --- Keyboard ---

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if self.owns_surface(surface) {
            println!("keyboard grabbed");
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.owns_surface(surface) {
            println!("keyboard released");
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
        self.key_pressed(event.keysym);
    }

    // hyprhopd tracks held keys itself, so compositor key repeat is ignored.
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
        event: KeyEvent,
    ) {
        self.key_released(event.keysym);
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
                Err(err) => eprintln!("hyprhopd: cannot use keyboard: {err}"),
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

// --- Required by sctk, unused by hyprhopd so far ---

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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

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

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);

/// Whether `surface` is the one belonging to `layer`.
pub fn is_layer_surface(layer: &LayerSurface, surface: &wl_surface::WlSurface) -> bool {
    layer.wl_surface() == surface
}
