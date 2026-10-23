mod handlers;
mod protocol;

use std::process::ExitCode;

use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    reexports::{calloop::EventLoop, calloop_wayland_source::WaylandSource},
    registry::RegistryState,
    seat::{SeatState, keyboard::Keysym},
    shell::{
        WaylandSurface,
        wlr_layer::{KeyboardInteractivity, Layer, LayerShell, LayerSurface},
    },
    shm::{
        Shm,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard::WlKeyboard, wl_shm, wl_surface::WlSurface},
};

use handlers::{ManagerData, is_layer_surface};
use protocol::global_shortcuts::hyprland_global_shortcut_v1::HyprlandGlobalShortcutV1;
use protocol::global_shortcuts::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1;

/// The app_id Hyprland binds refer to, as in `hyprhopd:normal`.
const APP_ID: &str = "hyprhopd";

/// The layer-shell namespace, which Hyprland layer rules can match on.
const LAYER_NAMESPACE: &str = "hyprhopd";

/// Each shortcut hyprhopd registers with Hyprland.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Normal,
    Pane,
}

impl Action {
    const ALL: [Action; 2] = [Action::Normal, Action::Pane];

    fn id(self) -> &'static str {
        match self {
            Action::Normal => "normal",
            Action::Pane => "pane",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Action::Normal => "Enter normal mode (keyboard-driven pointer)",
            Action::Pane => "Enter pane mode (hints in the focused pane)",
        }
    }
}

/// A mode that is currently active and holding the keyboard.
struct ActiveMode {
    /// The invisible surface that has exclusive keyboard focus. Dropping it destroys it,
    /// which hands the keyboard back to whatever had it before.
    layer: LayerSurface,
    /// Kept alive while attached to the surface.
    buffer: Option<Buffer>,
    /// Set once Escape has been pressed while the mode was active, so a release left over
    /// from before (or a press that leaks through) doesn't end the mode.
    escape_down: bool,
}

/// Everything the event handlers can read or change.
pub struct State {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    keyboard: Option<WlKeyboard>,
    // Dropping a proxy in wayland-client doesn't destroy it, so this isn't needed for events
    // to arrive; it's kept so hyprhopd can unregister them explicitly later.
    _shortcuts: Vec<HyprlandGlobalShortcutV1>,
    mode: Option<ActiveMode>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hyprhopd: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("cannot connect to a Wayland compositor: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();

    let manager: HyprlandGlobalShortcutsManagerV1 = globals.bind(&qh, 1..=1, ManagerData).map_err(
        |_| "compositor does not support hyprland_global_shortcuts_manager_v1 (is this Hyprland?)",
    )?;
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|_| "compositor does not support wl_compositor")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| "compositor does not support zwlr_layer_shell_v1")?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| "compositor does not support wl_shm")?;
    // Room for one 1x1 ARGB pixel.
    let pool = SlotPool::new(4, &shm)?;

    let shortcuts = Action::ALL
        .iter()
        .map(|&action| {
            manager.register_shortcut(
                action.id().into(),
                APP_ID.into(),
                action.description().into(),
                String::new(),
                &qh,
                action,
            )
        })
        .collect();

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        keyboard: None,
        _shortcuts: shortcuts,
        mode: None,
    };

    // Flush the registrations and surface any protocol error right away.
    queue.roundtrip(&mut state)?;
    println!("hyprhopd: registered {APP_ID}:normal and {APP_ID}:pane, waiting for shortcuts");

    // calloop lets Wayland events and (later) timers share one loop.
    let mut event_loop = EventLoop::<State>::try_new()?;
    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| format!("cannot add Wayland to the event loop: {e}"))?;
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

impl State {
    fn shortcut_pressed(&mut self, action: Action, qh: &QueueHandle<Self>) {
        match action {
            Action::Normal if self.mode.is_some() => {}
            Action::Normal => self.enter_normal_mode(qh),
            Action::Pane => println!("pane mode isn't implemented yet"),
        }
    }

    fn enter_normal_mode(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor.create_surface(qh);

        // An empty input region makes the surface click-through.
        match Region::new(&self.compositor) {
            Ok(region) => surface.set_input_region(Some(region.wl_region())),
            Err(err) => eprintln!("hyprhopd: cannot make the surface click-through: {err}"),
        }

        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some(LAYER_NAMESPACE),
            None,
        );
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(1, 1);
        // The first commit has no buffer; Hyprland answers with a configure event, and only
        // then may hyprhopd attach a buffer (see surface_configured).
        layer.commit();

        self.mode = Some(ActiveMode {
            layer,
            buffer: None,
            escape_down: false,
        });
        println!("normal mode on");
    }

    fn surface_configured(&mut self) {
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        if mode.buffer.is_some() {
            return;
        }

        // A surface is only shown once it has a buffer, so attach one transparent pixel.
        let (buffer, canvas) = match self.pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888) {
            Ok(created) => created,
            Err(err) => {
                eprintln!("hyprhopd: cannot create a buffer: {err}");
                self.exit_mode();
                return;
            }
        };
        canvas.fill(0);

        let surface = mode.layer.wl_surface();
        if let Err(err) = buffer.attach_to(surface) {
            eprintln!("hyprhopd: cannot attach the buffer: {err}");
            self.exit_mode();
            return;
        }
        surface.damage_buffer(0, 0, 1, 1);
        mode.layer.commit();
        mode.buffer = Some(buffer);
    }

    fn key_pressed(&mut self, keysym: Keysym) {
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        println!("{} pressed", key_name(keysym));
        if keysym == Keysym::Escape {
            mode.escape_down = true;
        }
    }

    fn key_released(&mut self, keysym: Keysym) {
        let Some(mode) = self.mode.as_ref() else {
            return;
        };
        println!("{} released", key_name(keysym));
        // Leaving on release rather than press means the app that gets focus back doesn't
        // see Escape still held down.
        if keysym == Keysym::Escape && mode.escape_down {
            self.exit_mode();
        }
    }

    fn exit_mode(&mut self) {
        if self.mode.take().is_some() {
            println!("normal mode off");
        }
    }

    fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.mode
            .as_ref()
            .is_some_and(|mode| is_layer_surface(&mode.layer, surface))
    }
}

fn key_name(keysym: Keysym) -> String {
    match keysym.name() {
        Some(name) => name.trim_start_matches("XK_").to_string(),
        None => format!("{:#x}", keysym.raw()),
    }
}
