mod handlers;
mod hyprland;
mod motion;
mod protocol;

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    output::OutputState,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        protocols_wlr::virtual_pointer::v1::client::{
            zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
            zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        },
    },
    registry::RegistryState,
};
use wayland_client::{
    Connection,
    globals::registry_queue_init,
    protocol::wl_pointer::{Axis, AxisSource, ButtonState},
};

use handlers::{ManagerData, VirtualPointerData};
use motion::{Direction, HeldKeys, ScrollDirection, ScrollKeys, Speed};
use protocol::global_shortcuts::hyprland_global_shortcut_v1::HyprlandGlobalShortcutV1;
use protocol::global_shortcuts::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1;

/// The app_id Hyprland binds refer to, as in `hyprhopd:normal`.
const APP_ID: &str = "hyprhopd";

/// Prefix of the submaps hyprhopd installs; see `State::install_submap`.
const SUBMAP_PREFIX: &str = "hyprhopd-";

/// Pointer speed in normal mode. Hard-coded until the config file exists.
const SPEED: Speed = Speed {
    initial: 220.0,
    acceleration: 700.0,
    max: 1600.0,
};

/// Scroll speed in scroll units per second (warpd's defaults). Hard-coded until the config
/// file exists.
const SCROLL_SPEED: Speed = Speed {
    initial: 300.0,
    acceleration: 1600.0,
    max: 9000.0,
};

/// Tick rate used when no monitor reports its refresh rate, in millihertz.
const FALLBACK_REFRESH_MHZ: i32 = 120_000;

/// Normal-mode keys, in Hyprland's key names. Hard-coded to warpd's defaults until the
/// config file exists.
const NORMAL_KEYS: [(&str, Action); 9] = [
    ("h", Action::Move(Direction::Left)),
    ("j", Action::Move(Direction::Down)),
    ("k", Action::Move(Direction::Up)),
    ("l", Action::Move(Direction::Right)),
    ("e", Action::Scroll(ScrollDirection::Down)),
    ("r", Action::Scroll(ScrollDirection::Up)),
    ("m", Action::Button(MouseButton::Left)),
    ("comma", Action::Button(MouseButton::Middle)),
    ("period", Action::Button(MouseButton::Right)),
];
const EXIT_KEY: &str = "escape";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// Linux input event codes (BTN_LEFT, BTN_MIDDLE, BTN_RIGHT), which Wayland uses.
    fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0x110,
            MouseButton::Middle => 0x112,
            MouseButton::Right => 0x111,
        }
    }
}

/// Each global shortcut hyprhopd registers with Hyprland. `Normal` and `Pane` are bound by
/// the user in Hyprland's config; the rest are bound by hyprhopd's own submap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Normal,
    Pane,
    Exit,
    Move(Direction),
    Scroll(ScrollDirection),
    Button(MouseButton),
}

impl Action {
    const ALL: [Action; 12] = [
        Action::Normal,
        Action::Pane,
        Action::Exit,
        Action::Move(Direction::Left),
        Action::Move(Direction::Down),
        Action::Move(Direction::Up),
        Action::Move(Direction::Right),
        Action::Scroll(ScrollDirection::Down),
        Action::Scroll(ScrollDirection::Up),
        Action::Button(MouseButton::Left),
        Action::Button(MouseButton::Middle),
        Action::Button(MouseButton::Right),
    ];

    fn id(self) -> &'static str {
        match self {
            Action::Normal => "normal",
            Action::Pane => "pane",
            Action::Exit => "exit",
            Action::Move(Direction::Left) => "left",
            Action::Move(Direction::Down) => "down",
            Action::Move(Direction::Up) => "up",
            Action::Move(Direction::Right) => "right",
            Action::Scroll(ScrollDirection::Down) => "scroll_down",
            Action::Scroll(ScrollDirection::Up) => "scroll_up",
            Action::Button(MouseButton::Left) => "button_left",
            Action::Button(MouseButton::Middle) => "button_middle",
            Action::Button(MouseButton::Right) => "button_right",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Action::Normal => "Enter normal mode (keyboard-driven pointer)",
            Action::Pane => "Enter pane mode (hints in the focused pane)",
            Action::Exit => "Leave the current hyprhopd mode",
            Action::Move(_) => "Move the pointer (normal mode)",
            Action::Scroll(_) => "Scroll (normal mode)",
            Action::Button(_) => "Press a mouse button (normal mode)",
        }
    }
}

/// State of normal mode while it is active.
struct ActiveMode {
    held: HeldKeys,
    /// When the current run of held movement keys started; acceleration counts from here.
    held_since: Option<Instant>,
    scroll: ScrollKeys,
    /// Like `held_since`, for scroll keys, so scroll acceleration is independent of motion.
    scroll_since: Option<Instant>,
    /// Mouse buttons currently pressed through hyprhopd.
    buttons_down: Vec<MouseButton>,
    last_tick: Instant,
    /// The repeating timer, present only while movement or scroll keys are held.
    timer: Option<RegistrationToken>,
}

/// Everything the event handlers can read or change.
pub struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    virtual_pointer: ZwlrVirtualPointerV1,
    loop_handle: LoopHandle<'static, State>,
    /// Reference point for the millisecond timestamps virtual-pointer events carry.
    started: Instant,
    // Dropping a proxy in wayland-client doesn't destroy it, so this isn't needed for events
    // to arrive; it's kept so hyprhopd can unregister them explicitly later.
    _shortcuts: Vec<HyprlandGlobalShortcutV1>,
    /// Name of the most recently installed submap, if installing succeeded.
    submap: Option<String>,
    /// Counts installs, so each one gets a fresh submap name.
    submap_generation: u32,
    /// Unfinished line from the event socket.
    event_buffer: String,
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

    // calloop lets Wayland events, Hyprland events and the motion timer share one loop.
    let mut event_loop = EventLoop::<State>::try_new()?;

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        virtual_pointer,
        loop_handle: event_loop.handle(),
        started: Instant::now(),
        _shortcuts: shortcuts,
        submap: None,
        submap_generation: 0,
        event_buffer: String::new(),
        mode: None,
    };

    // Flush the registrations and surface any protocol error right away.
    queue.roundtrip(&mut state)?;

    // A previous hyprhopd may have crashed inside its submap; get the keyboard back.
    if hyprland::current_submap().is_ok_and(|name| name.starts_with(SUBMAP_PREFIX)) {
        println!("hyprhopd: leaving a submap left behind by a previous run");
        if let Err(err) = hyprland::reset_submap() {
            eprintln!("hyprhopd: cannot reset the submap: {err}");
        }
    }
    state.install_submap();

    let events = hyprland::event_stream()
        .map_err(|e| format!("cannot connect to Hyprland's event socket: {e}"))?;
    events.set_nonblocking(true)?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(events, Interest::READ, Mode::Level),
            |_, stream, state| Ok(state.read_hyprland_events(stream)),
        )
        .map_err(|e| format!("cannot add Hyprland events to the event loop: {e}"))?;

    println!("hyprhopd: registered {APP_ID}:normal and {APP_ID}:pane, waiting for shortcuts");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| format!("cannot add Wayland to the event loop: {e}"))?;
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

impl State {
    /// Defines a fresh submap with hyprhopd's normal-mode keys. Each install gets a new
    /// name instead of replacing the old submap: redefining appends duplicate binds, and
    /// Hyprland's unbind would also remove the user's own binds for the same keys. Old
    /// submaps are never entered again and vanish on the next Hyprland reload.
    fn install_submap(&mut self) {
        self.submap_generation += 1;
        let name = format!(
            "{SUBMAP_PREFIX}{}-{}",
            std::process::id(),
            self.submap_generation
        );
        let binds: Vec<(&str, &str)> = NORMAL_KEYS
            .iter()
            .map(|&(key, action)| (key, action.id()))
            .collect();
        let lua = hyprland::submap_lua(&name, APP_ID, &binds, EXIT_KEY, Action::Exit.id());
        match hyprland::eval(&lua) {
            Ok(()) => {
                println!("hyprhopd: installed submap {name}");
                self.submap = Some(name);
            }
            Err(err) => {
                eprintln!("hyprhopd: cannot install the normal-mode submap: {err}");
                self.submap = None;
            }
        }
    }

    /// Reads whatever the event socket has and reacts to complete lines.
    fn read_hyprland_events(&mut self, stream: &UnixStream) -> PostAction {
        let mut chunk = [0u8; 4096];
        loop {
            match (&*stream).read(&mut chunk) {
                Ok(0) => {
                    eprintln!("hyprhopd: Hyprland closed its event socket");
                    return PostAction::Remove;
                }
                Ok(n) => self
                    .event_buffer
                    .push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    eprintln!("hyprhopd: cannot read Hyprland events: {err}");
                    return PostAction::Remove;
                }
            }
        }
        while let Some(end) = self.event_buffer.find('\n') {
            let line: String = self.event_buffer.drain(..=end).collect();
            if let Some((event, data)) = hyprland::parse_event(line.trim_end()) {
                self.hyprland_event(event, data);
            }
        }
        PostAction::Continue
    }

    fn hyprland_event(&mut self, event: &str, data: &str) {
        match event {
            // A reload wipes every bind added at runtime, including hyprhopd's submap.
            "configreloaded" => self.install_submap(),
            // The submap changed without hyprhopd (another reset, a reload): leave the mode
            // so no button stays pressed and no timer keeps running.
            "submap" if self.mode.is_some() && Some(data) != self.submap.as_deref() => {
                self.exit_mode();
            }
            _ => {}
        }
    }

    fn shortcut_event(&mut self, action: Action, pressed: bool) {
        match (action, pressed) {
            (Action::Normal, true) => self.enter_normal_mode(),
            (Action::Pane, true) => println!("pane mode isn't implemented yet"),
            (Action::Exit, true) => self.exit_mode(),
            (Action::Move(direction), _) => self.set_moving(direction, pressed),
            (Action::Scroll(direction), _) => self.set_scrolling(direction, pressed),
            (Action::Button(button), _) => self.set_button(button, pressed),
            _ => {}
        }
    }

    fn enter_normal_mode(&mut self) {
        if self.mode.is_some() {
            return;
        }
        let Some(submap) = &self.submap else {
            eprintln!("hyprhopd: normal mode is unavailable, its submap isn't installed");
            return;
        };
        if let Err(err) = hyprland::enter_submap(submap) {
            eprintln!("hyprhopd: cannot enter submap {submap}: {err}");
            return;
        }
        self.mode = Some(ActiveMode {
            held: HeldKeys::default(),
            held_since: None,
            scroll: ScrollKeys::default(),
            scroll_since: None,
            buttons_down: Vec::new(),
            last_tick: Instant::now(),
            timer: None,
        });
        println!(
            "normal mode on (motion at {:.0} Hz)",
            1.0 / self.tick_interval().as_secs_f64()
        );
    }

    fn exit_mode(&mut self) {
        self.stop_motion();
        let timestamp = self.timestamp();
        let Some(mode) = self.mode.take() else {
            return;
        };
        // A button still held here would otherwise stay pressed, like a stuck mouse button.
        for &button in &mode.buttons_down {
            self.send_button(timestamp, button, ButtonState::Released);
        }
        if mode.scroll.any() {
            self.virtual_pointer
                .axis_stop(timestamp, Axis::VerticalScroll);
            self.virtual_pointer.frame();
        }
        // The exit key already left the submap; this covers every other way of leaving.
        if hyprland::current_submap().is_ok_and(|name| Some(name) == self.submap)
            && let Err(err) = hyprland::reset_submap()
        {
            eprintln!("hyprhopd: cannot leave the submap: {err}");
        }
        println!("normal mode off");
    }

    fn set_moving(&mut self, direction: Direction, held: bool) {
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        mode.held.set(direction, held);
        if held {
            mode.held_since.get_or_insert_with(Instant::now);
            self.start_motion();
        } else if !mode.held.any() {
            // Acceleration starts over on the next press.
            mode.held_since = None;
            self.stop_motion_if_idle();
        }
    }

    fn set_scrolling(&mut self, direction: ScrollDirection, held: bool) {
        let timestamp = self.timestamp();
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        mode.scroll.set(direction, held);
        if held {
            mode.scroll_since.get_or_insert_with(Instant::now);
            self.start_motion();
        } else if !mode.scroll.any() {
            mode.scroll_since = None;
            self.virtual_pointer
                .axis_stop(timestamp, Axis::VerticalScroll);
            self.virtual_pointer.frame();
            self.stop_motion_if_idle();
        }
    }

    fn set_button(&mut self, button: MouseButton, pressed: bool) {
        let timestamp = self.timestamp();
        let Some(mode) = self.mode.as_mut() else {
            return;
        };
        let index = mode.buttons_down.iter().position(|&down| down == button);
        match (pressed, index) {
            (true, None) => {
                mode.buttons_down.push(button);
                self.send_button(timestamp, button, ButtonState::Pressed);
            }
            (false, Some(index)) => {
                mode.buttons_down.swap_remove(index);
                self.send_button(timestamp, button, ButtonState::Released);
            }
            _ => {}
        }
    }

    fn send_button(&self, timestamp: u32, button: MouseButton, state: ButtonState) {
        self.virtual_pointer.button(timestamp, button.code(), state);
        self.virtual_pointer.frame();
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

    fn stop_motion_if_idle(&mut self) {
        if self
            .mode
            .as_ref()
            .is_some_and(|mode| !mode.held.any() && !mode.scroll.any())
        {
            self.stop_motion();
        }
    }

    fn stop_motion(&mut self) {
        if let Some(token) = self.mode.as_mut().and_then(|mode| mode.timer.take()) {
            self.loop_handle.remove(token);
        }
    }

    /// Moves and scrolls by however far each should have gone since the last tick.
    fn motion_tick(&mut self) -> TimeoutAction {
        let interval = self.tick_interval();
        let timestamp = self.timestamp();
        let Some(mode) = self.mode.as_mut() else {
            return TimeoutAction::Drop;
        };
        if mode.held_since.is_none() && mode.scroll_since.is_none() {
            mode.timer = None;
            return TimeoutAction::Drop;
        }

        let now = Instant::now();
        let elapsed = now - mode.last_tick;
        mode.last_tick = now;

        if let Some(held_since) = mode.held_since {
            let speed = SPEED.after(now - held_since);
            let (dx, dy) = mode.held.displacement(speed, elapsed);
            if dx != 0.0 || dy != 0.0 {
                self.virtual_pointer.motion(timestamp, dx, dy);
                self.virtual_pointer.frame();
            }
        }

        if let Some(scroll_since) = mode.scroll_since {
            let speed = SCROLL_SPEED.after(now - scroll_since);
            let amount = mode.scroll.delta(speed, elapsed);
            if amount != 0.0 {
                // Continuous: smooth values, without the momentum apps add to touchpads.
                self.virtual_pointer.axis_source(AxisSource::Continuous);
                self.virtual_pointer
                    .axis(timestamp, Axis::VerticalScroll, amount);
                self.virtual_pointer.frame();
            }
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
}
