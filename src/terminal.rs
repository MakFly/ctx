use std::env;
use std::io::{self, IsTerminal};

pub fn stdout_enabled() -> bool {
    color_allowed() && io::stdout().is_terminal()
}

pub fn stderr_enabled() -> bool {
    color_allowed() && io::stderr().is_terminal()
}

pub fn stdout(text: impl AsRef<str>, code: u8) -> String {
    paint(text.as_ref(), code, stdout_enabled())
}

pub fn stderr(text: impl AsRef<str>, code: u8) -> String {
    paint(text.as_ref(), code, stderr_enabled())
}

fn color_allowed() -> bool {
    env::var_os("NO_COLOR").is_none()
}

fn paint(text: &str, code: u8, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}
