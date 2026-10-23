mod handlers;
mod motion;
mod protocol;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    reexports::{
        calloop::{
            EventLoop, LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        protocols_wlr::virtual_pointer::v1::client::{
            zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
            zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        },
    },
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

use handlers::{ManagerData, VirtualPointerData, is_layer_surface};
use motion::{Direction, HeldKeys, Speed};
use protocol::global_shortcuts::hyprland_global_shortcut_v1::HyprlandGlobalShortcutV1;
use protocol::global_shortcuts::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1;

/// The app_id Hyprland binds refer to, as in `hyprhopd:normal`.
const APP_ID: &str = "hyprhopd";

/// The layer-shell namespace, which Hyprland layer rules can match on.
const LAYER_NAMESPACE: &str = "hyprhopd";

/// Pointer speed in normal mode. Hard-coded until the config file exists.
const SPEED: Speed = Speed {
    initial: 220.0,
    acceleration: 700.0,
    max: 1600.0,
};

/// Tick rate used when no monitor reports its refresh rate, in millihertz.
const FALLBACK_REFRESH_MHZ: i32 = 120_000;

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
    held: HeldKeys,
    /// When the current run of held movement keys started; acceleration counts from here.
    held_since: Option<Instant>,
    last_tick: Instant,
    /// The repeating motion timer, present only while movement keys are held.
    timer: Option<RegistrationToken>,
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
    virtual_pointer: ZwlrVirtualPointerV1,
    loop_handle: LoopHandle<'static, State>,
    /// Reference point for the millisecond timestamps virtual-pointer events carry.
    started: Instant,
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
    let pointer_manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, VirtualPointerData)
        .map_err(|_| "compositor does not support zwlr_virtual_pointer_manager_v1")?;
    // No seat given: Hyprland attaches it to the default seat.
    let virtual_pointer = pointer_manager.create_virtual_pointer(None, &qh, VirtualPointerData);

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

    // calloop lets Wayland events and the motion timer share one loop.
    let mut event_loop = EventLoop::<State>::try_new()?;

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        keyboard: None,
        virtual_pointer,
        loop_handle: event_loop.handle(),
        started: Instant::now(),
        _shortcuts: shortcuts,
        mode: None,
    };

    // Flush the registrations and surface any protocol error right away.
    queue.roundtrip(&mut state)?;
    println!("hyprhopd: registered {APP_ID}:normal and {APP_ID}:pane, waiting for shortcuts");

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
            held: HeldKeys::default(),
            held_since: None,
            last_tick: Instant::now(),
            timer: None,
        });
        println!(
            "normal mode on (motion at {:.0} Hz)",
            1.0 / self.tick_interval().as_secs_f64()
        );
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
        if let Some(direction) = direction(keysym) {
            mode.held.set(direction, true);
            mode.held_since.get_or_insert_with(Instant::now);
            self.start_motion();
        }
    }

    fn key_released(&mut self, keysym: Keysym) {
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        println!("{} released", key_name(keysym));
        if let Some(direction) = direction(keysym) {
            mode.held.set(direction, false);
            if !mode.held.any() {
                // Acceleration starts over on the next press.
                mode.held_since = None;
                self.stop_motion();
            }
        }
        // Leaving on release rather than press means the app that gets focus back doesn't
        // see Escape still held down.
        if keysym == Keysym::Escape && self.mode.as_ref().is_some_and(|mode| mode.escape_down) {
            self.exit_mode();
        }
    }

    fn exit_mode(&mut self) {
        self.stop_motion();
        if self.mode.take().is_some() {
            println!("normal mode off");
        }
    }

    /// Starts the repeating motion timer, unless it's already running.
    fn start_motion(&mut self) {
        let interval = self.tick_interval();
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        if mode.timer.is_some() {
            return;
        }
        mode.last_tick = Instant::now();
        let timer = Timer::from_duration(interval);
        match self
            .loop_handle
            .insert_source(timer, |_, _, state| state.motion_tick())
        {
            Ok(token) => mode.timer = Some(token),
            Err(err) => eprintln!("hyprhopd: cannot start the motion timer: {err}"),
        }
    }

    fn stop_motion(&mut self) {
        if let Some(token) = self.mode.as_mut().and_then(|mode| mode.timer.take()) {
            self.loop_handle.remove(token);
        }
    }

    /// Moves the pointer by however far it should have travelled since the last tick.
    fn motion_tick(&mut self) -> TimeoutAction {
        let interval = self.tick_interval();
        let timestamp = self.timestamp();
        let Some(mode) = self.mode.as_mut() else {
            return TimeoutAction::Drop;
        };
        let Some(held_since) = mode.held_since else {
            mode.timer = None;
            return TimeoutAction::Drop;
        };

        let now = Instant::now();
        let elapsed = now - mode.last_tick;
        mode.last_tick = now;

        let speed = SPEED.after(now - held_since);
        let (dx, dy) = mode.held.displacement(speed, elapsed);
        if dx != 0.0 || dy != 0.0 {
            self.virtual_pointer.motion(timestamp, dx, dy);
            self.virtual_pointer.frame();
        }
        TimeoutAction::ToDuration(interval)
    }

    /// One tick per frame of the fastest connected monitor.
    fn tick_interval(&self) -> Duration {
        let refresh_mhz = self
            .output_state
            .outputs()
            .filter_map(|output| self.output_state.info(&output))
            .flat_map(|info| info.modes)
            .filter(|mode| mode.current && mode.refresh_rate > 0)
            .map(|mode| mode.refresh_rate)
            .max()
            .unwrap_or(FALLBACK_REFRESH_MHZ);
        Duration::from_secs_f64(1000.0 / f64::from(refresh_mhz))
    }

    /// Milliseconds since hyprhopd started, wrapping like Wayland timestamps do.
    fn timestamp(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.mode
            .as_ref()
            .is_some_and(|mode| is_layer_surface(&mode.layer, surface))
    }
}

/// The movement direction a key stands for. Hard-coded to hjkl until the config exists.
fn direction(keysym: Keysym) -> Option<Direction> {
    match keysym {
        Keysym::h | Keysym::H => Some(Direction::Left),
        Keysym::j | Keysym::J => Some(Direction::Down),
        Keysym::k | Keysym::K => Some(Direction::Up),
        Keysym::l | Keysym::L => Some(Direction::Right),
        _ => None,
    }
}

fn key_name(keysym: Keysym) -> String {
    match keysym.name() {
        Some(name) => name.trim_start_matches("XK_").to_string(),
        None => format!("{:#x}", keysym.raw()),
    }
}
