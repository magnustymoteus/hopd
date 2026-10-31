mod config;
mod easing;
mod handlers;
mod hints;
mod hyprland;
mod instance;
mod logging;
mod motion;
mod overlay;
mod pane;
mod protocol;
mod render;
mod window;

use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use inotify::{Inotify, WatchMask};
use log::{error, info, warn};

use smithay_client_toolkit::{
    compositor::CompositorState,
    output::OutputState,
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        protocols::wp::alpha_modifier::v1::client::wp_alpha_modifier_v1::WpAlphaModifierV1,
        protocols_wlr::virtual_pointer::v1::client::{
            zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
            zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
        },
    },
    registry::RegistryState,
    seat::SeatState,
    shell::wlr_layer::LayerShell,
    shm::{Shm, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{
        wl_keyboard::WlKeyboard,
        wl_pointer::{Axis, AxisSource, ButtonState},
    },
};

use config::{Config, Loaded};
use handlers::{AlphaModifierData, ManagerData, VirtualPointerData};
use motion::{Direction, HeldKeys, ScrollDirection, ScrollKeys, Speed};
use overlay::Overlay;
use pane::PaneMode;
use protocol::global_shortcuts::hyprland_global_shortcut_v1::HyprlandGlobalShortcutV1;
use protocol::global_shortcuts::hyprland_global_shortcuts_manager_v1::HyprlandGlobalShortcutsManagerV1;
use render::Renderer;
use window::WindowMode;

/// The app_id Hyprland binds refer to, as in `hyprhopd:normal`.
const APP_ID: &str = "hyprhopd";

/// Prefix of the submaps hyprhopd installs; see `State::install_submap`.
const SUBMAP_PREFIX: &str = "hyprhopd-";

/// Tick rate used when no monitor reports its refresh rate, in millihertz.
const FALLBACK_REFRESH_MHZ: i32 = 120_000;

/// How long config warnings stay on screen.
const WARNING_DURATION_MS: u32 = 10_000;

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

/// Each global shortcut hyprhopd registers with Hyprland. `Normal`, `Pane` and `Window`
/// are bound by the user in Hyprland's config; the rest are bound by hyprhopd's own submap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Normal,
    Pane,
    Window,
    Exit,
    Move(Direction),
    Scroll(ScrollDirection),
    Button(MouseButton),
}

impl Action {
    const ALL: [Action; 13] = [
        Action::Normal,
        Action::Pane,
        Action::Window,
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
            Action::Window => "window",
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
            Action::Window => "Enter window mode (jump to a visible window)",
            Action::Exit => "Leave the current hyprhopd mode",
            Action::Move(_) => "Move the pointer (normal mode)",
            Action::Scroll(_) => "Scroll (normal mode)",
            Action::Button(_) => "Press a mouse button (normal mode)",
        }
    }

    /// The action whose shortcut id, and config name, is `id`.
    fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|action| action.id() == id)
    }

    /// Whether the action can be bound inside normal mode.
    fn in_normal_mode(self) -> bool {
        !matches!(self, Action::Normal | Action::Pane | Action::Window)
    }
}

/// State of normal mode while it is active.
struct ActiveMode {
    /// The submap this run of the mode entered. A config reload installs a newer one,
    /// which applies the next time the mode starts.
    submap: String,
    /// Speeds as configured when the mode started, for the same reason.
    speed: Speed,
    scroll_speed: Speed,
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
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    /// Shared memory pane and window mode draw their overlays into.
    pool: SlotPool,
    keyboard: Option<WlKeyboard>,
    renderer: Renderer,
    /// For creating Wayland objects outside event handlers, like frame callbacks.
    qh: QueueHandle<State>,
    /// Absent if Hyprland lacks wp_alpha_modifier_v1; fades are skipped then.
    alpha_modifier: Option<WpAlphaModifierV1>,
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
    config: Config,
    config_path: Option<PathBuf>,
    /// Watches the config file's directory; absent if that couldn't be set up.
    config_watch: Option<Inotify>,
    mode: Option<ActiveMode>,
    pane: Option<PaneMode>,
    window: Option<WindowMode>,
    /// Overlays of a pane or window mode that ended, while they fade out.
    fading_out: Vec<Overlay>,
}

fn main() -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    match logging::init() {
        Some(path) => info!("hyprhopd {version}, logging to {}", path.display()),
        None => info!("hyprhopd {version}"),
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Before registering the shortcuts, so two copies never both react to them.
    let _instance = instance::claim()?;
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
    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|_| "compositor does not support wl_compositor")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| "compositor does not support zwlr_layer_shell_v1")?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| "compositor does not support wl_shm")?;
    let alpha_modifier: Option<WpAlphaModifierV1> =
        globals.bind(&qh, 1..=1, AlphaModifierData).ok();
    // Grows on demand; the first overlay's buffer sets the real size.
    let pool = SlotPool::new(4, &shm)?;
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
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        keyboard: None,
        renderer: Renderer::new(),
        qh: qh.clone(),
        alpha_modifier,
        virtual_pointer,
        loop_handle: event_loop.handle(),
        started: Instant::now(),
        _shortcuts: shortcuts,
        submap: None,
        submap_generation: 0,
        event_buffer: String::new(),
        config: Config::default(),
        config_path: config::path(),
        config_watch: None,
        mode: None,
        pane: None,
        window: None,
        fading_out: Vec::new(),
    };

    // Flush the registrations and surface any protocol error right away.
    queue.roundtrip(&mut state)?;

    // A previous hyprhopd may have crashed inside its submap; get the keyboard back.
    if hyprland::current_submap().is_ok_and(|name| name.starts_with(SUBMAP_PREFIX)) {
        info!("leaving a submap left behind by a previous run");
        if let Err(err) = hyprland::reset_submap() {
            error!("cannot reset the submap: {err}");
        }
    }
    state.reload_config();
    state.disable_layer_animation();

    match state.watch_config() {
        Ok(watch) => {
            // calloop owns a duplicate of the fd and only wakes the loop; the events are
            // read through `config_watch`.
            let fd = watch.as_fd().try_clone_to_owned()?;
            state.config_watch = Some(watch);
            event_loop
                .handle()
                .insert_source(
                    Generic::new(fd, Interest::READ, Mode::Level),
                    |_, _, state| Ok(state.read_config_events()),
                )
                .map_err(|e| format!("cannot add the config watch to the event loop: {e}"))?;
        }
        Err(err) => warn!("config changes won't apply until restart: {err}"),
    }

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

    info!("registered {APP_ID}:normal, {APP_ID}:pane and {APP_ID}:window, waiting for shortcuts");

    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| format!("cannot add Wayland to the event loop: {e}"))?;
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

impl State {
    /// Reads the config file again and installs a submap with its binds. Problems are
    /// printed and shown as one Hyprland notification.
    fn reload_config(&mut self) {
        let mut warnings = Vec::new();
        match self.config_path.as_deref().map(config::load) {
            Some(Loaded::Parsed(config, problems)) => {
                warnings = problems;
                // cosmic-text silently falls back for unknown fonts, so check here. Window
                // mode's family is only checked if it's its own, so one typo warns once.
                let pane_family = &config.pane.style.font.family;
                let window_family = &config.window.style.font.family;
                let mut families = vec![("pane", pane_family)];
                if window_family != pane_family {
                    families.push(("window", window_family));
                }
                for (section, family) in families {
                    if let Some(family) = family
                        && !self.renderer.has_family(family)
                    {
                        warnings.push(format!(
                            "{section}:font:family = {family}: no such font installed, using a fallback"
                        ));
                    }
                }
                let animations = &config.animations;
                let fades = [
                    animations.pane_in,
                    animations.pane_out,
                    animations.window_in,
                    animations.window_out,
                ]
                .iter()
                .any(|animation| !animation.duration.is_zero());
                if fades && self.alpha_modifier.is_none() {
                    warnings.push(
                        "animations: Hyprland lacks wp_alpha_modifier_v1, so fades are skipped"
                            .to_string(),
                    );
                }
                self.config = *config;
            }
            Some(Loaded::Missing) => {
                info!("no config file, using the defaults");
                self.config = Config::default();
            }
            // Keep the current config, so a half-typed edit doesn't wipe the binds.
            Some(Loaded::Broken(err)) => {
                self.report(&[format!("{err} (config not applied)")]);
                return;
            }
            None => {
                info!("neither XDG_CONFIG_HOME nor HOME is set, using the defaults")
            }
        }
        warnings.extend(self.install_submap());
        self.report(&warnings);
    }

    /// Turns off Hyprland's own open and close animation for hyprhopd's overlay, so the
    /// only fade is the one in hyprhopd.conf. Like the submap, it's added at runtime and
    /// again after each Hyprland reload.
    fn disable_layer_animation(&self) {
        let lua = hyprland::no_anim_layer_rule_lua("hyprhopd-no-anim", overlay::LAYER_NAMESPACE);
        if let Err(err) = hyprland::eval(&lua) {
            warn!("cannot turn off Hyprland's overlay animation: {err}");
        }
    }

    fn report(&self, warnings: &[String]) {
        if warnings.is_empty() {
            return;
        }
        for warning in warnings {
            warn!("{warning}");
        }
        let message = format!("hyprhopd.conf: {}", warnings.join("; "));
        if let Err(err) = hyprland::notify(&message, WARNING_DURATION_MS) {
            warn!("cannot show a notification: {err}");
        }
    }

    /// Defines a fresh submap with the configured normal-mode binds and returns a warning
    /// for each bind Hyprland rejected. Each install gets a new name instead of replacing
    /// the old submap: redefining appends duplicate binds, and Hyprland's unbind would also
    /// remove the user's own binds for the same keys. Old submaps are never entered again
    /// and vanish on the next Hyprland reload.
    fn install_submap(&mut self) -> Vec<String> {
        self.submap_generation += 1;
        let name = format!(
            "{SUBMAP_PREFIX}{}-{}",
            std::process::id(),
            self.submap_generation
        );
        let mut warnings = Vec::new();
        let mut installed = Vec::new();
        // One request per bind, so Hyprland's error names the bind it rejected.
        for bind in &self.config.normal.binds {
            match install_bind(&name, bind) {
                Ok(()) => installed.push(bind.action),
                Err(err) => warnings.push(format!("normal: {bind}: {err}")),
            }
        }
        // An action whose binds were all rejected gets its default keys back. For exit this
        // is essential: without it, the catch-all would swallow the whole keyboard.
        for action in Action::ALL.into_iter().filter(|a| a.in_normal_mode()) {
            if installed.contains(&action) {
                continue;
            }
            for bind in config::default_binds(action) {
                match install_bind(&name, &bind) {
                    Ok(()) => {
                        installed.push(action);
                        warnings.push(format!(
                            "normal: no bind for {} could be installed, using the default {bind}",
                            action.id()
                        ));
                    }
                    Err(err) => warnings.push(format!("normal: {bind}: {err}")),
                }
            }
        }
        if !installed.contains(&Action::Exit) {
            error!("normal mode is unavailable: no exit bind could be installed");
            self.submap = None;
            return warnings;
        }
        // Last, so it's clear every real bind went in first.
        if let Err(err) = hyprland::eval(&hyprland::catchall_lua(&name)) {
            warnings.push(format!("normal: other keys won't be blocked: {err}"));
        }
        info!("installed submap {name}");
        self.submap = Some(name);
        warnings
    }

    /// Watches the directory rather than the file: editors often save by writing a new
    /// file and renaming it over the old one, which would end a watch on the file itself.
    fn watch_config(&self) -> Result<Inotify, String> {
        let dir = self
            .config_path
            .as_deref()
            .and_then(|path| path.parent())
            .ok_or("no config directory")?;
        let inotify = Inotify::init().map_err(|e| e.to_string())?;
        inotify
            .watches()
            .add(dir, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO)
            .map_err(|e| format!("cannot watch {}: {e}", dir.display()))?;
        Ok(inotify)
    }

    /// Reloads the config once if any pending event concerns the config file.
    fn read_config_events(&mut self) -> PostAction {
        let Some(watch) = self.config_watch.as_mut() else {
            return PostAction::Remove;
        };
        let file_name = self
            .config_path
            .as_deref()
            .and_then(|path| path.file_name());
        let mut buffer = [0u8; 4096];
        let mut changed = false;
        loop {
            match watch.read_events(&mut buffer) {
                Ok(events) => {
                    let mut any = false;
                    for event in events {
                        any = true;
                        changed |= event.name.is_some() && event.name == file_name;
                    }
                    if !any {
                        break;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => {
                    error!("cannot read config changes: {err}");
                    return PostAction::Remove;
                }
            }
        }
        if changed {
            info!("config changed, reloading");
            self.reload_config();
        }
        PostAction::Continue
    }

    /// Reads whatever the event socket has and reacts to complete lines.
    fn read_hyprland_events(&mut self, stream: &UnixStream) -> PostAction {
        let mut chunk = [0u8; 4096];
        loop {
            match (&*stream).read(&mut chunk) {
                Ok(0) => {
                    error!("Hyprland closed its event socket");
                    return PostAction::Remove;
                }
                Ok(n) => self
                    .event_buffer
                    .push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    error!("cannot read Hyprland events: {err}");
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
            "configreloaded" => {
                let warnings = self.install_submap();
                self.report(&warnings);
                self.disable_layer_animation();
            }
            // The submap changed without hyprhopd (another reset, a reload): leave the mode
            // so no button stays pressed and no timer keeps running.
            "submap" if self.mode.as_ref().is_some_and(|mode| mode.submap != data) => {
                self.exit_mode();
            }
            _ if self.pane.is_some() => self.pane_hyprland_event(event, data),
            _ if self.window.is_some() => self.window_hyprland_event(event),
            _ => {}
        }
    }

    fn shortcut_event(&mut self, action: Action, pressed: bool, qh: &QueueHandle<State>) {
        match (action, pressed) {
            (Action::Normal, true) => self.enter_normal_mode(),
            (Action::Pane, true) => self.enter_pane_mode(qh),
            (Action::Window, true) => self.enter_window_mode(qh),
            (Action::Exit, true) => self.exit_mode(),
            (Action::Move(direction), _) => self.set_moving(direction, pressed),
            (Action::Scroll(direction), _) => self.set_scrolling(direction, pressed),
            (Action::Button(button), _) => self.set_button(button, pressed),
            _ => {}
        }
    }

    /// Whether one of hyprhopd's modes is active; only one can be at a time.
    fn busy(&self) -> bool {
        self.mode.is_some() || self.pane.is_some() || self.window.is_some()
    }

    fn enter_normal_mode(&mut self) {
        if self.busy() {
            return;
        }
        let Some(submap) = self.submap.clone() else {
            error!("normal mode is unavailable, its submap isn't installed");
            return;
        };
        if let Err(err) = hyprland::enter_submap(&submap) {
            error!("cannot enter submap {submap}: {err}");
            return;
        }
        self.mode = Some(ActiveMode {
            submap,
            speed: self.config.normal.speed,
            scroll_speed: self.config.normal.scroll_speed,
            held: HeldKeys::default(),
            held_since: None,
            scroll: ScrollKeys::default(),
            scroll_since: None,
            buttons_down: Vec::new(),
            last_tick: Instant::now(),
            timer: None,
        });
        info!(
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
        if hyprland::current_submap().is_ok_and(|name| name == mode.submap)
            && let Err(err) = hyprland::reset_submap()
        {
            error!("cannot leave the submap: {err}");
        }
        info!("normal mode off");
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

    /// Moves the pointer straight to `(x, y)` in layout coordinates. `layout` is the box
    /// around every monitor, which absolute motion is relative to.
    fn jump_pointer(&self, x: f64, y: f64, (left, top, width, height): (f64, f64, f64, f64)) {
        self.virtual_pointer.motion_absolute(
            self.timestamp(),
            (x - left).round() as u32,
            (y - top).round() as u32,
            width.round() as u32,
            height.round() as u32,
        );
        self.virtual_pointer.frame();
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
            Err(err) => error!("cannot start the motion timer: {err}"),
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
            let speed = mode.speed.after(now - held_since);
            let (dx, dy) = mode.held.displacement(speed, elapsed);
            if dx != 0.0 || dy != 0.0 {
                self.virtual_pointer.motion(timestamp, dx, dy);
                self.virtual_pointer.frame();
            }
        }

        if let Some(scroll_since) = mode.scroll_since {
            let speed = mode.scroll_speed.after(now - scroll_since);
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

/// Adds one normal-mode bind to submap `name`, returning Hyprland's reason if it refuses.
fn install_bind(name: &str, bind: &config::Bind) -> Result<(), String> {
    let key = hyprland::key_string(&bind.mods, &bind.key);
    let lua = if bind.action == Action::Exit {
        hyprland::exit_bind_lua(name, &key, APP_ID, bind.action.id())
    } else {
        hyprland::bind_lua(name, &key, APP_ID, bind.action.id())
    };
    hyprland::eval(&lua).map_err(|err| hyprland::bind_error(&err).to_string())
}
