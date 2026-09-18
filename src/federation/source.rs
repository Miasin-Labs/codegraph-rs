//! A short window of a definition's source, read from its own graph's
//! tree — a dependency's source directory on this machine or a linked
//! project's checkout. Never the network, never outside that root, never
//! a file too large to be source.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Largest file a window is read from.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Verbatim lines `start..=end` of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceWindow {
    /// The file, absolute.
    pub path: PathBuf,
    pub start: u32,
    pub end: u32,
    pub text: String,
    /// The definition runs past `end` (the window's line or character cap).
    pub truncated: bool,
}

/// Lines `start..=end` of `root/file`, at most `max_lines` lines and
/// `max_chars` characters (whole lines only). `None` when the file is not
/// under `root`, is missing or too large, or `start` is past its end.
pub fn read_window(
    root: &Path,
    file: &str,
    start: u32,
    end: u32,
    max_lines: usize,
    max_chars: usize,
) -> Option<SourceWindow> {
    let relative = Path::new(file);
    let inside = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if !inside || start == 0 {
        return None;
    }
    let path = root.join(relative);
    let handle = fs::File::open(&path).ok()?;
    if handle.metadata().ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    handle.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    let wanted = end.max(start) as usize - start as usize + 1;
    let mut lines = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for line in text.lines().skip(start as usize - 1).take(wanted) {
        if lines.len() >= max_lines || used + line.len() + 1 > max_chars {
            truncated = true;
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    Some(SourceWindow {
        end: start + lines.len() as u32 - 1,
        start,
        text: lines.join("\n"),
        truncated,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_whole_lines_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "// head\npub fn a() {\n    one();\n    two();\n}\n",
        )
        .unwrap();
        let window = read_window(dir.path(), "src/lib.rs", 2, 5, 10, 1_000).unwrap();
        assert_eq!(window.text, "pub fn a() {\n    one();\n    two();\n}");
        assert_eq!((window.start, window.end, window.truncated), (2, 5, false));

        let short = read_window(dir.path(), "src/lib.rs", 2, 5, 2, 1_000).unwrap();
        assert_eq!(short.text, "pub fn a() {\n    one();");
        assert_eq!((short.end, short.truncated), (3, true));

        assert!(read_window(dir.path(), "../outside.rs", 1, 1, 10, 100).is_none());
        assert!(read_window(dir.path(), "/etc/hostname", 1, 1, 10, 100).is_none());
        assert!(read_window(dir.path(), "src/lib.rs", 40, 41, 10, 100).is_none());
    }
}
