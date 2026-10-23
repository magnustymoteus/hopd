//! Event handlers hyprhopd has to provide for sctk and for the Hyprland and wlr protocols.
//! Most are empty: hyprhopd only reacts to shortcuts and needs monitor refresh rates.

use smithay_client_toolkit::{
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
        zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use wayland_client::{Connection, QueueHandle, protocol::wl_output};

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
        _: &QueueHandle<State>,
    ) {
        let pressed = matches!(event, hyprland_global_shortcut_v1::Event::Pressed { .. });
        state.shortcut_event(*self, pressed);
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

// --- Outputs, for monitor refresh rates ---

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
    registry_handlers![OutputState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);
