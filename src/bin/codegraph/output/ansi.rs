// =============================================================================
// ANSI Color Helpers (avoid chalk ESM issues — TS kept raw escapes; so do we)
// =============================================================================

pub(crate) const RESET: &str = "\x1b[0m";
pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const DIM: &str = "\x1b[2m";
pub(crate) const RED: &str = "\x1b[31m";
pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const YELLOW: &str = "\x1b[33m";
pub(crate) const BLUE: &str = "\x1b[34m";
pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const WHITE: &str = "\x1b[37m";
#[allow(dead_code)]
pub(crate) const GRAY: &str = "\x1b[90m";

pub(crate) fn bold(s: &str) -> String {
    format!("{BOLD}{s}{RESET}")
}
pub(crate) fn dim(s: &str) -> String {
    format!("{DIM}{s}{RESET}")
}
pub(crate) fn red(s: &str) -> String {
    format!("{RED}{s}{RESET}")
}
pub(crate) fn green(s: &str) -> String {
    format!("{GREEN}{s}{RESET}")
}
pub(crate) fn yellow(s: &str) -> String {
    format!("{YELLOW}{s}{RESET}")
}
pub(crate) fn blue(s: &str) -> String {
    format!("{BLUE}{s}{RESET}")
}
pub(crate) fn cyan(s: &str) -> String {
    format!("{CYAN}{s}{RESET}")
}
pub(crate) fn white(s: &str) -> String {
    format!("{WHITE}{s}{RESET}")
}
