//! hyprhopd.conf: Hyprland-style config (hyprlang), one section per mode.
//!
//! ```text
//! normal {
//!     bind = SHIFT, h, left
//!     speed {
//!         initial = 220
//!     }
//! }
//! pane {
//!     spacing = 60
//!     label {
//!         padding = 1 4
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
const PANE_OPTIONS: [&str; 13] = [
    "pane:spacing",
    "pane:chars",
    "pane:font:family",
    "pane:font:size",
    "pane:font:weight",
    "pane:label:text_color",
    "pane:label:typed_color",
    "pane:label:background",
    "pane:label:padding",
    "pane:label:radius",
    "pane:label:border:width",
    "pane:label:border:color",
    "pane:backdrop:color",
];

/// Smallest allowed `pane:spacing`, in pixels.
const MIN_SPACING: f64 = 10.0;

/// CSS-style weight names, as fontconfig and cosmic-text number them.
const WEIGHTS: [(&str, u16); 10] = [
    ("thin", 100),
    ("extralight", 200),
    ("light", 300),
    ("normal", 400),
    ("regular", 400),
    ("medium", 500),
    ("semibold", 600),
    ("bold", 700),
    ("extrabold", 800),
    ("black", 900),
];

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

/// An sRGB colour with straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    const fn hex(rgba: u32) -> Rgba {
        let [r, g, b, a] = rgba.to_be_bytes();
        Rgba { r, g, b, a }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// A font family name; `None` means the system's monospace font.
    pub family: Option<String>,
    /// In pixels.
    pub size: f32,
    /// 100 (thin) to 900 (black).
    pub weight: u16,
}

/// How each label looks. Its box is laid out like CSS: text, then padding, then the
/// border, with the background under all of it.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelStyle {
    pub text_color: Rgba,
    /// Colour of the letters already typed.
    pub typed_color: Rgba,
    pub background: Rgba,
    /// Vertical and horizontal padding, in pixels.
    pub padding: (f32, f32),
    /// Corner radius in pixels; 0 gives square corners.
    pub radius: f32,
    /// Border width in pixels; 0 means no border.
    pub border_width: f32,
    pub border_color: Rgba,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneConfig {
    /// Distance between neighbouring label centres, in pixels.
    pub spacing: f64,
    /// Characters labels are made of, in the order labels use them.
    pub chars: Vec<char>,
    pub font: Font,
    pub label: LabelStyle,
    /// Dims the target window behind the labels; transparent means off.
    pub backdrop: Rgba,
}

impl Default for PaneConfig {
    fn default() -> PaneConfig {
        PaneConfig {
            spacing: 60.0,
            chars: ('a'..='z').collect(),
            font: Font {
                family: None,
                size: 14.0,
                weight: 700,
            },
            label: LabelStyle {
                text_color: Rgba::hex(0xffffffff),
                typed_color: Rgba::hex(0xf9e2afff),
                background: Rgba::hex(0x1e1e2ecc),
                padding: (1.0, 4.0),
                radius: 0.0,
                border_width: 0.0,
                border_color: Rgba::hex(0xffffff40),
            },
            backdrop: Rgba::hex(0x00000000),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub normal: NormalConfig,
    pub pane: PaneConfig,
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
            pane: PaneConfig::default(),
        }
    }
}

/// warpd's default binds for `action`, used when none of its configured binds install.
pub fn default_binds(action: Action) -> impl Iterator<Item = Bind> {
    DEFAULT_BINDS
        .into_iter()
        .filter(move |&(_, default)| default == action)
        .map(|(key, action)| Bind::default_for(key, action))
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
    config.pane = read_pane(&parser, &mut warnings);

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

fn read_pane(parser: &hyprlang::Config, warnings: &mut Vec<String>) -> PaneConfig {
    let mut pane = PaneConfig::default();
    if let Some(spacing) = number(parser, "pane:spacing", warnings) {
        // Below this, a window would need thousands of labels.
        if spacing >= MIN_SPACING {
            pane.spacing = spacing;
        } else {
            warnings.push(format!(
                "pane:spacing = {spacing}: must be at least {MIN_SPACING}"
            ));
        }
    }
    if let Some(chars) = text(parser, "pane:chars", warnings) {
        match parse_chars(&chars) {
            Ok(chars) => pane.chars = chars,
            Err(reason) => warnings.push(format!("pane:chars = {chars}: {reason}")),
        }
    }
    if let Some(family) = text(parser, "pane:font:family", warnings) {
        pane.font.family = Some(family);
    }
    if let Some(size) = number(parser, "pane:font:size", warnings) {
        if size > 0.0 {
            pane.font.size = size as f32;
        } else {
            warnings.push(format!("pane:font:size = {size}: must be positive"));
        }
    }
    if let Some(weight) = text(parser, "pane:font:weight", warnings) {
        match parse_weight(&weight) {
            Some(weight) => pane.font.weight = weight,
            None => warnings.push(format!(
                "pane:font:weight = {weight}: expected thin, light, normal, medium, bold, black, ... or 100 to 900"
            )),
        }
    }
    let label = &mut pane.label;
    if let Some(padding) = text(parser, "pane:label:padding", warnings) {
        match parse_padding(&padding) {
            Some(padding) => label.padding = padding,
            None => warnings.push(format!(
                "pane:label:padding = {padding}: expected one or two numbers, like 4 or 1 4"
            )),
        }
    }
    for (key, slot) in [
        ("pane:label:radius", &mut label.radius),
        ("pane:label:border:width", &mut label.border_width),
    ] {
        if let Some(value) = number(parser, key, warnings) {
            if value >= 0.0 {
                *slot = value as f32;
            } else {
                warnings.push(format!("{key} = {value}: can't be negative"));
            }
        }
    }
    for (key, slot) in [
        ("pane:label:text_color", &mut label.text_color),
        ("pane:label:typed_color", &mut label.typed_color),
        ("pane:label:background", &mut label.background),
        ("pane:label:border:color", &mut label.border_color),
        ("pane:backdrop:color", &mut pane.backdrop),
    ] {
        if let Some(color) = color(parser, key, warnings) {
            *slot = color;
        }
    }
    pane
}

/// CSS-style padding: `4` for every side, or `1 4` for vertical then horizontal.
fn parse_padding(value: &str) -> Option<(f32, f32)> {
    let numbers: Vec<f32> = value
        .split_whitespace()
        .map(|n| n.parse().ok().filter(|&n: &f32| n >= 0.0))
        .collect::<Option<_>>()?;
    match numbers[..] {
        [all] => Some((all, all)),
        [vertical, horizontal] => Some((vertical, horizontal)),
        _ => None,
    }
}

/// A number option, or `None` (with a warning if it's set to something else).
fn number(parser: &hyprlang::Config, key: &str, warnings: &mut Vec<String>) -> Option<f64> {
    match parser.get(key).ok()? {
        ConfigValue::Int(n) => Some(*n as f64),
        ConfigValue::Float(n) => Some(*n),
        other => {
            warnings.push(format!("{key} = {other}: expected a number"));
            None
        }
    }
}

/// A text option. Numbers count as text too, so `chars = 1234` works.
fn text(parser: &hyprlang::Config, key: &str, warnings: &mut Vec<String>) -> Option<String> {
    match parser.get(key).ok()? {
        ConfigValue::String(s) => Some(s.clone()),
        ConfigValue::Int(n) => Some(n.to_string()),
        ConfigValue::Float(n) => Some(n.to_string()),
        other => {
            warnings.push(format!("{key} = {other}: expected text"));
            None
        }
    }
}

/// A colour option: `rgba(RRGGBBAA)` and `rgba(r, g, b, a)` arrive parsed, `rgb(RRGGBB)`
/// as text.
fn color(parser: &hyprlang::Config, key: &str, warnings: &mut Vec<String>) -> Option<Rgba> {
    let parsed = match parser.get(key).ok()? {
        ConfigValue::Color(c) => Some(Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }),
        ConfigValue::String(s) => parse_rgb(s),
        _ => None,
    };
    if parsed.is_none() {
        let value = parser.get(key).map(|v| v.to_string()).unwrap_or_default();
        warnings.push(format!(
            "{key} = {value}: expected rgba(RRGGBBAA) or rgb(RRGGBB)"
        ));
    }
    parsed
}

/// `rgb(RRGGBB)`, which hyprlang leaves as text.
fn parse_rgb(value: &str) -> Option<Rgba> {
    let hex = value.trim().strip_prefix("rgb(")?.strip_suffix(')')?.trim();
    if hex.len() != 6 {
        return None;
    }
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgba::hex(rgb << 8 | 0xff))
}

fn parse_chars(value: &str) -> Result<Vec<char>, String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.iter().any(|c| c.is_whitespace()) {
        return Err("labels can't contain spaces".to_string());
    }
    if let Some((i, c)) = chars
        .iter()
        .enumerate()
        .find(|&(i, c)| chars[..i].contains(c))
    {
        return Err(format!("'{c}' appears twice (position {})", i + 1));
    }
    if chars.len() < 2 {
        return Err("needs at least two characters".to_string());
    }
    Ok(chars)
}

fn parse_weight(value: &str) -> Option<u16> {
    let value = value.trim().to_lowercase();
    if let Some(&(_, weight)) = WEIGHTS.iter().find(|&&(name, _)| name == value) {
        return Some(weight);
    }
    value
        .parse()
        .ok()
        .filter(|weight| (1..=1000).contains(weight))
}

fn is_known_option(key: &str) -> bool {
    if PANE_OPTIONS.contains(&key) {
        return true;
    }
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
    fn pane_options_are_read() {
        let (config, warnings) = parsed(
            "pane {\n  spacing = 40\n  chars = asdf\n  font {\n    family = Noto Sans\n    size = 18.5\n    weight = semibold\n  }\n  label {\n    text_color = rgb(ff0000)\n    typed_color = rgba(00ff0080)\n    background = rgba(0, 0, 255, 0.5)\n    padding = 2 6\n    radius = 4\n    border {\n      width = 1.5\n      color = rgba(ffffffff)\n    }\n  }\n  backdrop {\n    color = rgba(00000066)\n  }\n}\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let pane = config.pane;
        assert_eq!(pane.spacing, 40.0);
        assert_eq!(pane.chars, ['a', 's', 'd', 'f']);
        assert_eq!(
            pane.font,
            Font {
                family: Some("Noto Sans".into()),
                size: 18.5,
                weight: 600,
            }
        );
        assert_eq!(
            pane.label,
            LabelStyle {
                text_color: Rgba::hex(0xff0000ff),
                typed_color: Rgba::hex(0x00ff0080),
                background: Rgba::hex(0x0000ff80),
                padding: (2.0, 6.0),
                radius: 4.0,
                border_width: 1.5,
                border_color: Rgba::hex(0xffffffff),
            }
        );
        assert_eq!(pane.backdrop, Rgba::hex(0x00000066));
    }

    #[test]
    fn bad_pane_options_keep_their_defaults() {
        let (config, warnings) = parsed(
            "pane {\n  spacing = 0\n  chars = abca\n  font {\n    weight = heavyish\n  }\n  label {\n    text_color = red\n    padding = 1 2 3\n    radius = -1\n  }\n}\n",
        );
        assert_eq!(warnings.len(), 6, "{warnings:?}");
        assert_eq!(config.pane, PaneConfig::default());
    }

    #[test]
    fn padding_takes_one_or_two_numbers() {
        assert_eq!(parse_padding("4"), Some((4.0, 4.0)));
        assert_eq!(parse_padding("1 4.5"), Some((1.0, 4.5)));
        assert_eq!(parse_padding("1 2 3"), None);
        assert_eq!(parse_padding("-1"), None);
        assert_eq!(parse_padding("wide"), None);
    }

    #[test]
    fn old_flat_label_options_are_reported() {
        let (_, warnings) = parsed("pane {\n  background = rgba(000000ff)\n}\n");
        assert_eq!(warnings, ["unknown option pane:background"]);
    }

    #[test]
    fn label_chars_need_two_distinct_non_space_characters() {
        assert_eq!(parse_chars("12"), Ok(vec!['1', '2']));
        assert!(parse_chars("a").is_err());
        assert!(parse_chars("a b").is_err());
        assert!(parse_chars("aba").is_err());
    }

    #[test]
    fn default_binds_exist_for_every_normal_mode_action() {
        for action in Action::ALL.into_iter().filter(|a| a.in_normal_mode()) {
            assert!(default_binds(action).next().is_some(), "{action:?}");
        }
    }

    #[test]
    fn syntax_errors_break_the_whole_file() {
        assert!(matches!(
            parse("normal {\n  bind = , a, left\n  ==== oops\n}\n"),
            Loaded::Broken(_)
        ));
    }
}
