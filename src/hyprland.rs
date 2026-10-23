//! Talking to Hyprland over its IPC sockets: commands on `.socket.sock`, events on
//! `.socket2.sock`. This is what `hyprctl` does under the hood.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

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

/// Lua that defines submap `name`: each key in `binds` triggers the global shortcut
/// `app_id:shortcut`, `exit_key` leaves the submap and triggers `app_id:exit_shortcut`, and
/// every other key is swallowed.
pub fn submap_lua(
    name: &str,
    app_id: &str,
    binds: &[(&str, &str)],
    exit_key: &str,
    exit_shortcut: &str,
) -> String {
    let global = |shortcut: &str| lua_string(&format!("{app_id}:{shortcut}"));
    let mut lua = format!("hl.define_submap({}, function()\n", lua_string(name));
    for (key, shortcut) in binds {
        lua.push_str(&format!(
            "  hl.bind({}, hl.dsp.global({}))\n",
            lua_string(key),
            global(shortcut)
        ));
    }
    // The exit key resets the submap itself, so it still works if hyprhopd has crashed.
    lua.push_str(&format!(
        "  hl.bind({}, function()\n    hl.dispatch(hl.dsp.submap(\"reset\"))\n    hl.dispatch(hl.dsp.global({}))\n  end)\n",
        lua_string(exit_key),
        global(exit_shortcut)
    ));
    // Catch-all also fires for bound keys, so it must do nothing.
    lua.push_str("  hl.bind(\"catchall\", function() end)\nend)\n");
    lua
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
    fn submap_binds_each_key_then_exit_then_catchall() {
        let lua = submap_lua(
            "hyprhopd-1",
            "hyprhopd",
            &[("h", "left"), ("comma", "button_middle")],
            "escape",
            "exit",
        );
        let expected = "hl.define_submap(\"hyprhopd-1\", function()\n\
             \x20 hl.bind(\"h\", hl.dsp.global(\"hyprhopd:left\"))\n\
             \x20 hl.bind(\"comma\", hl.dsp.global(\"hyprhopd:button_middle\"))\n\
             \x20 hl.bind(\"escape\", function()\n\
             \x20   hl.dispatch(hl.dsp.submap(\"reset\"))\n\
             \x20   hl.dispatch(hl.dsp.global(\"hyprhopd:exit\"))\n\
             \x20 end)\n\
             \x20 hl.bind(\"catchall\", function() end)\n\
             end)\n";
        assert_eq!(lua, expected);
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
