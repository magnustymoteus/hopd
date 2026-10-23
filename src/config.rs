//! hyprhopd.conf: Hyprland-style config (hyprlang), one section per mode.
//!
//! ```text
//! normal {
//!     bind = SHIFT, h, left
//!     speed {
//!         initial = 220
//!     }
//! }
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use hyprlang::{ConfigOptions, ConfigValue, ParseResult};

use crate::motion::{Direction, ScrollDirection, Speed};
use crate::{Action, MouseButton};

/// warpd's normal-mode keys, used for every action the config doesn't bind.
const DEFAULT_BINDS: [(&str, Action); 10] = [
    ("h", Action::Move(Direction::Left)),
    ("j", Action::Move(Direction::Down)),
    ("k", Action::Move(Direction::Up)),
    ("l", Action::Move(Direction::Right)),
    ("e", Action::Scroll(ScrollDirection::Down)),
    ("r", Action::Scroll(ScrollDirection::Up)),
    ("m", Action::Button(MouseButton::Left)),
    ("comma", Action::Button(MouseButton::Middle)),
    ("period", Action::Button(MouseButton::Right)),
    ("escape", Action::Exit),
];

/// Pointer speed in px/s.
const DEFAULT_SPEED: Speed = Speed {
    initial: 220.0,
    acceleration: 700.0,
    max: 1600.0,
};

/// Scroll speed in scroll units per second (warpd's defaults).
const DEFAULT_SCROLL_SPEED: Speed = Speed {
    initial: 300.0,
    acceleration: 1600.0,
    max: 9000.0,
};

/// Options hyprhopd understands, besides `normal:bind`.
const SPEED_OPTIONS: [&str; 2] = ["speed", "scroll_speed"];
const SPEED_FIELDS: [&str; 3] = ["initial", "acceleration", "max"];

#[derive(Debug, Clone, PartialEq)]
pub struct Bind {
    /// Modifier names as written, like `SHIFT`.
    pub mods: Vec<String>,
    /// A Hyprland key name, like `h` or `comma`.
    pub key: String,
    pub action: Action,
}

impl Bind {
    fn default_for(key: &str, action: Action) -> Bind {
        Bind {
            mods: Vec::new(),
            key: key.to_string(),
            action,
        }
    }
}

// Written back the way it appears in the config, for warnings.
impl fmt::Display for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bind = {}, {}, {}",
            self.mods.join(" "),
            self.key,
            self.action.id()
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalConfig {
    pub binds: Vec<Bind>,
    pub speed: Speed,
    pub scroll_speed: Speed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub normal: NormalConfig,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            normal: NormalConfig {
                binds: DEFAULT_BINDS
                    .iter()
                    .map(|&(key, action)| Bind::default_for(key, action))
                    .collect(),
                speed: DEFAULT_SPEED,
                scroll_speed: DEFAULT_SCROLL_SPEED,
            },
        }
    }
}

/// The outcome of reading the config file.
pub enum Loaded {
    /// Parsed, with a warning for each setting that was skipped.
    Parsed(Config, Vec<String>),
    /// No config file; the defaults apply.
    Missing,
    /// The file couldn't be parsed at all (a syntax error, or unreadable).
    Broken(String),
}

/// `$XDG_CONFIG_HOME/hypr/hyprhopd.conf`, or `~/.config/hypr/hyprhopd.conf`.
pub fn path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config_home.join("hypr").join("hyprhopd.conf"))
}

pub fn load(path: &Path) -> Loaded {
    if !path.exists() {
        return Loaded::Missing;
    }
    // parse_file rather than reading the text ourselves, so `source = ...` resolves
    // relative to the config file.
    parse_with(|parser| parser.parse_file(path))
}

#[cfg(test)]
pub fn parse(text: &str) -> Loaded {
    parse_with(|parser| parser.parse(text))
}

fn parse_with(run: impl FnOnce(&mut hyprlang::Config) -> ParseResult<()>) -> Loaded {
    let mut parser = hyprlang::Config::with_options(ConfigOptions {
        throw_all_errors: true,
        ..ConfigOptions::default()
    });
    // Registering `bind` as a handler keeps every bind line instead of only the last.
    parser.register_category_handler_fn("normal", "bind", |_| Ok(()));
    if let Err(err) = run(&mut parser) {
        return Loaded::Broken(err.to_string());
    }

    let mut warnings = Vec::new();
    let mut config = Config::default();

    let lines = parser
        .get_handler_calls("normal:bind")
        .cloned()
        .unwrap_or_default();
    let binds: Vec<Bind> = lines
        .iter()
        .filter_map(|line| match parse_bind(line) {
            Ok(bind) => Some(bind),
            Err(reason) => {
                warnings.push(format!("normal: bind = {line}: {reason}"));
                None
            }
        })
        .collect();
    // A bind for an action replaces that action's defaults; other defaults stay.
    config
        .normal
        .binds
        .retain(|default| !binds.iter().any(|bind| bind.action == default.action));
    config.normal.binds.extend(binds);

    config.normal.speed = read_speed(&parser, "speed", DEFAULT_SPEED, &mut warnings);
    config.normal.scroll_speed =
        read_speed(&parser, "scroll_speed", DEFAULT_SCROLL_SPEED, &mut warnings);

    let mut unknown: Vec<&str> = parser
        .keys()
        .into_iter()
        .filter(|key| !is_known_option(key))
        .collect();
    unknown.sort_unstable();
    for key in unknown {
        warnings.push(format!("unknown option {key}"));
    }

    Loaded::Parsed(config, warnings)
}

/// Parses `MODS, key, action`, as in `SHIFT CTRL, h, left`.
fn parse_bind(value: &str) -> Result<Bind, String> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    let [mods, key, action] = parts[..] else {
        return Err("expected MODS, key, action".to_string());
    };
    if key.is_empty() {
        return Err("missing key".to_string());
    }
    let action = Action::from_id(action)
        .filter(|action| action.in_normal_mode())
        .ok_or_else(|| format!("unknown action \"{action}\""))?;
    Ok(Bind {
        mods: mods.split_whitespace().map(str::to_string).collect(),
        key: key.to_string(),
        action,
    })
}

/// Reads `normal:<name>:initial` and friends, keeping the default for each one that's
/// missing or invalid.
fn read_speed(
    parser: &hyprlang::Config,
    name: &str,
    default: Speed,
    warnings: &mut Vec<String>,
) -> Speed {
    let mut speed = default;
    let fields = [
        ("initial", &mut speed.initial),
        ("acceleration", &mut speed.acceleration),
        ("max", &mut speed.max),
    ];
    for (field, slot) in fields {
        let key = format!("normal:{name}:{field}");
        let Ok(value) = parser.get(&key) else {
            continue;
        };
        let number = match value {
            ConfigValue::Int(n) => *n as f64,
            ConfigValue::Float(n) => *n,
            other => {
                warnings.push(format!("{key} = {other}: expected a number"));
                continue;
            }
        };
        // A zero acceleration is allowed: it gives a constant speed.
        let valid = if field == "acceleration" {
            number >= 0.0
        } else {
            number > 0.0
        };
        if valid {
            *slot = number;
        } else {
            warnings.push(format!("{key} = {number}: must be positive"));
        }
    }
    if speed.max < speed.initial {
        warnings.push(format!(
            "normal:{name}: max ({}) is below initial ({}), using initial as max",
            speed.max, speed.initial
        ));
        speed.max = speed.initial;
    }
    speed
}

fn is_known_option(key: &str) -> bool {
    let mut parts = key.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("normal"), Some(option), Some(field), None) => {
            SPEED_OPTIONS.contains(&option) && SPEED_FIELDS.contains(&field)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> (Config, Vec<String>) {
        match parse(text) {
            Loaded::Parsed(config, warnings) => (config, warnings),
            Loaded::Missing => panic!("parse never reports a missing file"),
            Loaded::Broken(err) => panic!("unexpected syntax error: {err}"),
        }
    }

    fn binds_for(config: &Config, action: Action) -> Vec<String> {
        config
            .normal
            .binds
            .iter()
            .filter(|bind| bind.action == action)
            .map(|bind| bind.to_string())
            .collect()
    }

    #[test]
    fn empty_file_gives_the_defaults() {
        let (config, warnings) = parsed("");
        assert_eq!(config, Config::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_bind_replaces_only_its_actions_defaults() {
        let (config, warnings) = parsed("normal {\n  bind = SHIFT, a, left\n}\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            binds_for(&config, Action::Move(Direction::Left)),
            ["bind = SHIFT, a, left"]
        );
        assert_eq!(
            binds_for(&config, Action::Move(Direction::Right)),
            ["bind = , l, right"]
        );
    }

    #[test]
    fn an_action_can_have_several_keys() {
        let (config, _) = parsed("normal {\n  bind = , a, exit\n  bind = CTRL, c, exit\n}\n");
        assert_eq!(
            binds_for(&config, Action::Exit),
            ["bind = , a, exit", "bind = CTRL, c, exit"]
        );
    }

    #[test]
    fn variables_are_expanded_in_binds() {
        let (config, _) = parsed("$mod = SHIFT CTRL\nnormal {\n  bind = $mod, a, up\n}\n");
        assert_eq!(
            binds_for(&config, Action::Move(Direction::Up)),
            ["bind = SHIFT CTRL, a, up"]
        );
    }

    #[test]
    fn bad_binds_are_skipped_with_a_warning() {
        let (config, warnings) = parsed(
            "normal {\n  bind = , a, fly\n  bind = , normal, normal\n  bind = a, left\n  bind = SHIFT, , up\n}\n",
        );
        assert_eq!(
            warnings,
            [
                "normal: bind = , a, fly: unknown action \"fly\"",
                "normal: bind = , normal, normal: unknown action \"normal\"",
                "normal: bind = a, left: expected MODS, key, action",
                "normal: bind = SHIFT, , up: missing key",
            ]
        );
        // Skipped binds don't replace the defaults.
        assert_eq!(config, Config::default());
    }

    #[test]
    fn speeds_are_read_and_partially_defaulted() {
        let (config, warnings) =
            parsed("normal {\n  speed {\n    initial = 100\n    max = 2500.5\n  }\n}\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            config.normal.speed,
            Speed {
                initial: 100.0,
                acceleration: DEFAULT_SPEED.acceleration,
                max: 2500.5,
            }
        );
        assert_eq!(config.normal.scroll_speed, DEFAULT_SCROLL_SPEED);
    }

    #[test]
    fn bad_speeds_keep_their_defaults() {
        let (config, warnings) = parsed(
            "normal {\n  speed {\n    initial = fast\n    acceleration = -5\n  }\n  scroll_speed {\n    acceleration = 0\n  }\n}\n",
        );
        assert_eq!(
            warnings,
            [
                "normal:speed:initial = fast: expected a number",
                "normal:speed:acceleration = -5: must be positive",
            ]
        );
        assert_eq!(config.normal.speed, DEFAULT_SPEED);
        assert_eq!(config.normal.scroll_speed.acceleration, 0.0);
    }

    #[test]
    fn max_below_initial_is_raised_to_initial() {
        let (config, warnings) = parsed("normal {\n  speed {\n    initial = 3000\n  }\n}\n");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(config.normal.speed.max, 3000.0);
    }

    #[test]
    fn unknown_options_are_reported() {
        let (_, warnings) = parsed("speed = 3\nnormal {\n  sped {\n    max = 1\n  }\n}\n");
        assert_eq!(
            warnings,
            ["unknown option normal:sped:max", "unknown option speed"]
        );
    }

    #[test]
    fn syntax_errors_break_the_whole_file() {
        assert!(matches!(
            parse("normal {\n  bind = , a, left\n  ==== oops\n}\n"),
            Loaded::Broken(_)
        ));
    }
}
