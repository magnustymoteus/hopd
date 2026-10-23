//! Logging to the terminal and to a log file. Hyprland sends the output of programs it
//! starts to /dev/null, so without the file an autostarted hyprhopd's messages are lost.

use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use log::{error, warn};
use simplelog::{
    ColorChoice, CombinedLogger, Config, ConfigBuilder, LevelFilter, SharedLogger, TermLogger,
    TerminalMode, WriteLogger,
};

/// Sets up logging and returns the log file's path, or None when only the terminal gets
/// the messages. Must be called once, before anything logs.
pub fn init() -> Option<PathBuf> {
    let config = config();
    // termcolor's Auto only looks at TERM, so colour would also reach pipes and files.
    let color = if std::io::stderr().is_terminal() {
        ColorChoice::Auto
    } else {
        ColorChoice::Never
    };
    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![TermLogger::new(
        LevelFilter::Info,
        config.clone(),
        TerminalMode::Stderr,
        color,
    )];
    let file = match log_path(env::var_os("XDG_STATE_HOME"), env::var_os("HOME")) {
        Some(path) => match open(&path) {
            Ok(file) => Ok((path, file)),
            Err(err) => Err(format!(
                "cannot write the log file {}: {err}",
                path.display()
            )),
        },
        None => Err("neither XDG_STATE_HOME nor HOME is set".to_string()),
    };
    let (path, problem) = match file {
        Ok((path, file)) => {
            loggers.push(WriteLogger::new(LevelFilter::Info, config, file));
            (Some(path), None)
        }
        Err(problem) => (None, Some(problem)),
    };
    // Only fails if a logger is already set, which can't happen when called once.
    let _ = CombinedLogger::init(loggers);
    // A panic's message would otherwise go to stderr, which may be /dev/null.
    std::panic::set_hook(Box::new(|info| error!("crashed: {info}")));
    if let Some(problem) = problem {
        warn!("{problem}, logging to the terminal only");
    }
    path
}

/// Where the log goes: $XDG_STATE_HOME/hyprhopd/hyprhopd.log, with ~/.local/state as the
/// fallback for XDG_STATE_HOME.
fn log_path(state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let state_home = state_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local").join("state")))?;
    Some(state_home.join("hyprhopd").join("hyprhopd.log"))
}

/// Opens the log file empty, so each run starts a fresh log.
fn open(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    File::create(path)
}

fn config() -> Config {
    let mut builder = ConfigBuilder::new();
    // Falls back to UTC if the local offset can't be determined.
    let _ = builder.set_time_offset_to_local();
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_under_xdg_state_home() {
        assert_eq!(
            log_path(Some("/state".into()), Some("/home/me".into())),
            Some(PathBuf::from("/state/hyprhopd/hyprhopd.log"))
        );
    }

    #[test]
    fn falls_back_to_local_state_in_home() {
        let expected = Some(PathBuf::from("/home/me/.local/state/hyprhopd/hyprhopd.log"));
        assert_eq!(log_path(None, Some("/home/me".into())), expected);
        // An empty XDG_STATE_HOME counts as unset.
        assert_eq!(log_path(Some("".into()), Some("/home/me".into())), expected);
    }

    #[test]
    fn no_path_without_any_directory() {
        assert_eq!(log_path(None, None), None);
    }
}
