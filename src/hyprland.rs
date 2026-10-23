//! Talking to Hyprland over its IPC sockets: commands on `.socket.sock`, events on
//! `.socket2.sock`. This is what `hyprctl` does under the hood.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::Deserialize;

/// The directory holding this Hyprland instance's sockets.
fn socket_dir() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let instance = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");
    match (runtime, instance) {
        (Some(runtime), Some(instance)) => Ok(PathBuf::from(runtime).join("hypr").join(instance)),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR or HYPRLAND_INSTANCE_SIGNATURE is not set (is this Hyprland?)",
        )),
    }
}

/// Sends one command and returns Hyprland's reply. Hyprland closes the connection after
/// replying, so each command uses its own connection.
fn request(command: &str) -> io::Result<String> {
    let mut stream = UnixStream::connect(socket_dir()?.join(".socket.sock"))?;
    stream.write_all(command.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.trim().to_string())
}

/// Runs a command that answers "ok" on success, turning any other reply into an error.
fn expect_ok(command: &str) -> Result<(), String> {
    match request(command) {
        Ok(reply) if reply == "ok" => Ok(()),
        Ok(reply) => Err(reply),
        Err(err) => Err(err.to_string()),
    }
}

/// Executes Lua inside Hyprland, like `hyprctl eval`.
pub fn eval(lua: &str) -> Result<(), String> {
    expect_ok(&format!("eval {lua}"))
}

pub fn enter_submap(name: &str) -> Result<(), String> {
    expect_ok(&format!("dispatch hl.dsp.submap({})", lua_string(name)))
}

pub fn reset_submap() -> Result<(), String> {
    expect_ok("dispatch hl.dsp.submap(\"reset\")")
}

pub fn current_submap() -> Result<String, String> {
    request("submap").map_err(|err| err.to_string())
}

/// A window as `hyprctl activewindow -j` describes it, in layout coordinates.
#[derive(Debug, Deserialize)]
pub struct Window {
    /// Like `0x5617571353a0`. Events name windows without the `0x`.
    pub address: String,
    pub at: [i32; 2],
    pub size: [i32; 2],
    /// The monitor's id.
    pub monitor: i64,
}

impl Window {
    /// The address as events write it.
    pub fn event_address(&self) -> &str {
        self.address.trim_start_matches("0x")
    }
}

/// A monitor as `hyprctl monitors -j` describes it.
#[derive(Debug, Deserialize)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    /// Top-left corner in layout coordinates.
    pub x: i32,
    pub y: i32,
    /// In physical pixels, before scale and rotation.
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    /// 0-7, as in wl_output; odd values are rotated by 90 or 270 degrees.
    pub transform: i32,
}

impl Monitor {
    /// Width and height in layout coordinates.
    pub fn logical_size(&self) -> (f64, f64) {
        let (w, h) = (
            f64::from(self.width) / self.scale,
            f64::from(self.height) / self.scale,
        );
        if self.transform % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// The focused window, if there is one.
pub fn active_window() -> Result<Option<Window>, String> {
    let reply = request("j/activewindow").map_err(|err| err.to_string())?;
    // With nothing focused, Hyprland answers `{}`.
    if reply == "{}" {
        return Ok(None);
    }
    serde_json::from_str(&reply)
        .map(Some)
        .map_err(|err| format!("unexpected activewindow reply: {err}"))
}

pub fn monitors() -> Result<Vec<Monitor>, String> {
    let reply = request("j/monitors").map_err(|err| err.to_string())?;
    serde_json::from_str(&reply).map_err(|err| format!("unexpected monitors reply: {err}"))
}

/// Hyprland's `decoration:rounding`: the radius of window corners, in pixels.
pub fn window_rounding() -> Result<i64, String> {
    #[derive(Deserialize)]
    struct IntOption {
        int: i64,
    }
    let reply = request("j/getoption decoration:rounding").map_err(|err| err.to_string())?;
    serde_json::from_str::<IntOption>(&reply)
        .map(|option| option.int)
        .map_err(|err| format!("unexpected getoption reply: {err}"))
}

/// Connects to the event socket, which streams lines like `configreloaded>>`.
pub fn event_stream() -> io::Result<UnixStream> {
    UnixStream::connect(socket_dir()?.join(".socket2.sock"))
}

/// One event from the event socket, split into its name and data.
pub fn parse_event(line: &str) -> Option<(&str, &str)> {
    line.split_once(">>")
}

/// A Lua string literal for `value`.
fn lua_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Shows an on-screen Hyprland notification with a warning icon for `duration_ms`.
pub fn notify(message: &str, duration_ms: u32) -> Result<(), String> {
    // Arguments: icon (0 = warning), duration, color (0 = default), message.
    expect_ok(&format!("notify 0 {duration_ms} 0 {message}"))
}

/// A Hyprland key string like `SHIFT + CTRL + h`.
pub fn key_string(mods: &[String], key: &str) -> String {
    let mut parts: Vec<&str> = mods.iter().map(String::as_str).collect();
    parts.push(key);
    parts.join(" + ")
}

/// Lua that adds `body`'s binds to submap `name`. Defining a submap that already exists
/// appends to it, so binds can be added one call at a time.
fn in_submap(name: &str, body: &str) -> String {
    format!(
        "hl.define_submap({}, function()\n{body}end)\n",
        lua_string(name)
    )
}

/// Lua that binds `key` in submap `name` to the global shortcut `app_id:shortcut`.
pub fn bind_lua(name: &str, key: &str, app_id: &str, shortcut: &str) -> String {
    in_submap(
        name,
        &format!(
            "  hl.bind({}, hl.dsp.global({}))\n",
            lua_string(key),
            lua_string(&format!("{app_id}:{shortcut}"))
        ),
    )
}

/// Like `bind_lua`, but `key` also leaves the submap. It does that itself, so it still
/// works if hyprhopd has crashed.
pub fn exit_bind_lua(name: &str, key: &str, app_id: &str, shortcut: &str) -> String {
    in_submap(
        name,
        &format!(
            "  hl.bind({}, function()\n    hl.dispatch(hl.dsp.submap(\"reset\"))\n    hl.dispatch(hl.dsp.global({}))\n  end)\n",
            lua_string(key),
            lua_string(&format!("{app_id}:{shortcut}"))
        ),
    )
}

/// Lua that adds layer rule `name`, turning off Hyprland's animations for layer surfaces
/// with exactly this namespace. Adding a rule whose name exists again is harmless.
pub fn no_anim_layer_rule_lua(name: &str, namespace: &str) -> String {
    format!(
        "hl.layer_rule({{ name = {}, match = {{ namespace = {} }}, no_anim = true }})",
        lua_string(name),
        lua_string(&format!("^{namespace}$"))
    )
}

/// Lua that swallows every key submap `name` doesn't bind. Catch-all also fires for bound
/// keys, so it must do nothing.
pub fn catchall_lua(name: &str) -> String {
    in_submap(name, "  hl.bind(\"catchall\", function() end)\n")
}

/// The useful part of Hyprland's reply to a failed bind, like
/// `failed to parse key string: Unknown keysym: "x"`.
pub fn bind_error(reply: &str) -> &str {
    reply
        .split_once("hl.bind: ")
        .map_or(reply, |(_, message)| message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_strings_are_escaped() {
        assert_eq!(lua_string("h"), "\"h\"");
        assert_eq!(lua_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn binds_are_added_to_the_submap() {
        assert_eq!(
            bind_lua("hyprhopd-1", "SHIFT + h", "hyprhopd", "left"),
            "hl.define_submap(\"hyprhopd-1\", function()\n\
             \x20 hl.bind(\"SHIFT + h\", hl.dsp.global(\"hyprhopd:left\"))\n\
             end)\n"
        );
    }

    #[test]
    fn the_exit_bind_resets_the_submap_before_notifying() {
        assert_eq!(
            exit_bind_lua("hyprhopd-1", "escape", "hyprhopd", "exit"),
            "hl.define_submap(\"hyprhopd-1\", function()\n\
             \x20 hl.bind(\"escape\", function()\n\
             \x20   hl.dispatch(hl.dsp.submap(\"reset\"))\n\
             \x20   hl.dispatch(hl.dsp.global(\"hyprhopd:exit\"))\n\
             \x20 end)\n\
             end)\n"
        );
    }

    #[test]
    fn layer_rule_matches_the_namespace_exactly() {
        assert_eq!(
            no_anim_layer_rule_lua("hyprhopd-no-anim", "hyprhopd"),
            "hl.layer_rule({ name = \"hyprhopd-no-anim\", match = { namespace = \"^hyprhopd$\" }, no_anim = true })"
        );
    }

    #[test]
    fn key_strings_join_mods_and_key() {
        assert_eq!(key_string(&[], "h"), "h");
        assert_eq!(
            key_string(&["SHIFT".into(), "CTRL".into()], "comma"),
            "SHIFT + CTRL + comma"
        );
    }

    #[test]
    fn bind_errors_drop_the_lua_location() {
        assert_eq!(
            bind_error(
                "error: =[C]:-1: hl.bind: failed to parse key string: Unknown keysym: \"x\""
            ),
            "failed to parse key string: Unknown keysym: \"x\""
        );
        assert_eq!(bind_error("something else"), "something else");
    }

    #[test]
    fn events_split_on_the_first_separator() {
        assert_eq!(
            parse_event("configreloaded>>"),
            Some(("configreloaded", ""))
        );
        assert_eq!(
            parse_event("submap>>hyprhopd-1"),
            Some(("submap", "hyprhopd-1"))
        );
        assert_eq!(parse_event("garbage"), None);
    }
}
