use super::*;

pub(crate) fn success(message: &str) {
    println!("{} {message}", green(get_glyphs().ok));
}

/// Print error message (TS `error()` — `console.error`, stderr).
pub(crate) fn error_msg(message: &str) {
    eprintln!("{} {message}", red(get_glyphs().err));
}

/// Print info message (TS `info()` — stdout).
pub(crate) fn info(message: &str) {
    println!("{} {message}", blue(get_glyphs().info));
}

/// Print warning message (TS `warn()` — stdout).
pub(crate) fn warn(message: &str) {
    println!("{} {message}", yellow(get_glyphs().warn));
}

// =============================================================================
// @clack/prompts replacements (same flow & wording; plain stdout rendering,
// matching the installer module's clack adaptation — see notes/cli.md)
// =============================================================================

pub(crate) fn clack_intro(msg: &str) {
    println!("{msg}");
}
pub(crate) fn clack_outro(msg: &str) {
    println!("{msg}");
}
pub(crate) fn clack_log_success(msg: &str) {
    println!("{} {msg}", green(get_glyphs().ok));
}
pub(crate) fn clack_log_info(msg: &str) {
    println!("{} {msg}", blue(get_glyphs().info));
}
pub(crate) fn clack_log_warn(msg: &str) {
    println!("{} {msg}", yellow(get_glyphs().warn));
}
pub(crate) fn clack_log_error(msg: &str) {
    println!("{} {msg}", red(get_glyphs().err));
}
pub(crate) fn clack_note(body: &str, title: &str) {
    println!("{title}:");
    for line in body.lines() {
        println!("  {line}");
    }
}
