//! Glyph selection for CLI output.
//!
//! On Windows, console output is interpreted via the active output
//! codepage. PowerShell 5.1 and cmd.exe default to OEM codepages
//! (CP437, CP936, ...), so UTF-8 bytes written to the console render
//! as mojibake (see #168). The shimmer worker is hit hardest because
//! it writes raw bytes to stdout (no TTY-aware encoding conversion)
//! to keep animation smooth while the main thread is blocked in
//! SQLite. To stay readable everywhere, we fall back to ASCII glyphs
//! whenever the terminal is not known to handle UTF-8.
//!
//! Detection is intentionally simple:
//!   - `CODEGRAPH_ASCII=1`  -> ASCII (escape hatch for any terminal)
//!   - `CODEGRAPH_UNICODE=1` -> Unicode (opt-in on Windows)
//!   - Windows              -> ASCII by default
//!   - Linux kernel console (`TERM=linux`) -> ASCII
//!   - Everything else      -> Unicode

use std::sync::Mutex;

/// Whether the current terminal is known to handle UTF-8 output.
pub fn supports_unicode() -> bool {
    if std::env::var("CODEGRAPH_ASCII").as_deref() == Ok("1") {
        return false;
    }
    if std::env::var("CODEGRAPH_UNICODE").as_deref() == Ok("1") {
        return true;
    }
    if cfg!(windows) {
        return false;
    }
    std::env::var("TERM").as_deref() != Ok("linux")
}

/// The glyph table used by CLI/progress output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyphs {
    pub ok: &'static str,
    pub err: &'static str,
    pub info: &'static str,
    pub warn: &'static str,
    pub spinner: &'static [&'static str],
    pub bar_filled: &'static str,
    pub bar_empty: &'static str,
    pub rail: &'static str,
    pub phase_done: &'static str,
    pub dash: &'static str,
    pub h_line: &'static str,
    pub tree_branch: &'static str,
    pub tree_last: &'static str,
    pub tree_pipe: &'static str,
}

pub static UNICODE_GLYPHS: Glyphs = Glyphs {
    ok: "✓",
    err: "✗",
    info: "ℹ",
    warn: "⚠",
    spinner: &["·", "✢", "✳", "✶", "✻", "✽"],
    bar_filled: "█",
    bar_empty: "░",
    rail: "│",
    phase_done: "◆",
    dash: "—",
    h_line: "─",
    tree_branch: "├── ",
    tree_last: "└── ",
    tree_pipe: "│   ",
};

pub static ASCII_GLYPHS: Glyphs = Glyphs {
    ok: "[OK]",
    err: "[ERR]",
    info: "[i]",
    warn: "[!]",
    spinner: &[".", "*", "+", "x", "o", "O"],
    bar_filled: "#",
    bar_empty: "-",
    rail: "|",
    phase_done: "*",
    dash: "-",
    h_line: "-",
    tree_branch: "|-- ",
    tree_last: "`-- ",
    tree_pipe: "|   ",
};

static CACHED: Mutex<Option<&'static Glyphs>> = Mutex::new(None);

/// Get the glyph set for the current terminal (cached after first call).
pub fn get_glyphs() -> &'static Glyphs {
    let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
    if cached.is_none() {
        *cached = Some(if supports_unicode() {
            &UNICODE_GLYPHS
        } else {
            &ASCII_GLYPHS
        });
    }
    cached.unwrap()
}

/// Reset the cached glyph set. Test-only; production code should call `get_glyphs()`.
pub fn _reset_glyphs_cache() {
    *CACHED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    //! Glyph fallback / Unicode-support detection.
    //!
    //! Pinned because the matrix is small and the consequence of regression
    //! is highly visible: shimmer-worker output on Windows mojibakes when
    //! UTF-8 glyphs are written raw to stdout (see #168). The detection
    //! + ASCII fallback is the contract that prevents this.
    //!
    //! Note: the TS suite fakes `process.platform`; Rust cannot, so the
    //! platform-specific cases are gated with #[cfg(windows)] /
    //! #[cfg(not(windows))] instead and only run on their real platform.

    use std::sync::Mutex as StdMutex;

    use super::*;

    /// Serializes env-mutating tests (Rust runs tests on parallel threads;
    /// the vitest suite mutates `process.env` serially within one process).
    static TEST_ENV_LOCK: StdMutex<()> = StdMutex::new(());

    /// Mirrors the TS `withEnv` helper: patch env vars, reset the glyph
    /// cache, run, then restore on drop (even on panic).
    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn patch(patch: &[(&'static str, Option<&str>)]) -> Self {
            let saved = patch
                .iter()
                .map(|(k, _)| (*k, std::env::var(k).ok()))
                .collect();
            for (k, v) in patch {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
            _reset_glyphs_cache();
            EnvGuard { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
            _reset_glyphs_cache();
        }
    }

    const ALL_CLEAR: &[(&str, Option<&str>)] = &[
        ("CODEGRAPH_ASCII", None),
        ("CODEGRAPH_UNICODE", None),
        ("TERM", None),
    ];

    // "returns false on Windows by default (mojibake-prone consoles)"
    #[cfg(windows)]
    #[test]
    fn supports_unicode_returns_false_on_windows_by_default() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(ALL_CLEAR);
        assert!(!supports_unicode());
    }

    // "returns true on macOS by default" + "returns true on Linux by default"
    // (one not-windows test; the implementation has no darwin/linux split
    // beyond TERM, which is cleared here).
    #[cfg(not(windows))]
    #[test]
    fn supports_unicode_returns_true_on_unix_by_default() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(ALL_CLEAR);
        assert!(supports_unicode());
    }

    // "returns false on Linux kernel console (TERM=linux)"
    #[cfg(not(windows))]
    #[test]
    fn supports_unicode_returns_false_on_linux_kernel_console() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(&[
            ("CODEGRAPH_ASCII", None),
            ("CODEGRAPH_UNICODE", None),
            ("TERM", Some("linux")),
        ]);
        assert!(!supports_unicode());
    }

    // "respects CODEGRAPH_UNICODE=1 on Windows (opt-in escape hatch)" —
    // the env check precedes the platform check, so this holds everywhere.
    #[test]
    fn supports_unicode_respects_codegraph_unicode_opt_in() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(&[("CODEGRAPH_UNICODE", Some("1")), ("CODEGRAPH_ASCII", None)]);
        assert!(supports_unicode());
    }

    // "respects CODEGRAPH_ASCII=1 on macOS (opt-out escape hatch)"
    #[test]
    fn supports_unicode_respects_codegraph_ascii_opt_out() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(&[("CODEGRAPH_ASCII", Some("1")), ("CODEGRAPH_UNICODE", None)]);
        assert!(!supports_unicode());
    }

    // "CODEGRAPH_ASCII takes precedence over CODEGRAPH_UNICODE"
    #[test]
    fn codegraph_ascii_takes_precedence_over_codegraph_unicode() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(&[
            ("CODEGRAPH_ASCII", Some("1")),
            ("CODEGRAPH_UNICODE", Some("1")),
        ]);
        assert!(!supports_unicode());
    }

    // "returns ASCII glyphs on Windows"
    #[cfg(windows)]
    #[test]
    fn get_glyphs_returns_ascii_glyphs_on_windows() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(ALL_CLEAR);
        let g = get_glyphs();
        assert!(std::ptr::eq(g, &ASCII_GLYPHS));
        assert_eq!(g.ok, "[OK]");
        assert_eq!(g.rail, "|");
        assert_eq!(g.phase_done, "*");
        assert_eq!(g.dash, "-");
    }

    // "returns Unicode glyphs on macOS" (any unix with TERM cleared)
    #[cfg(not(windows))]
    #[test]
    fn get_glyphs_returns_unicode_glyphs_on_unix() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(ALL_CLEAR);
        let g = get_glyphs();
        assert!(std::ptr::eq(g, &UNICODE_GLYPHS));
        assert_eq!(g.ok, "✓");
        assert_eq!(g.rail, "│");
        assert_eq!(g.phase_done, "◆");
        assert_eq!(g.dash, "—");
    }

    // "caches the result so repeated calls return the same object"
    #[test]
    fn get_glyphs_caches_the_result() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::patch(ALL_CLEAR);
        assert!(std::ptr::eq(get_glyphs(), get_glyphs()));
    }

    /// Field-by-field view of a glyph set, for the parity tests below.
    /// (The TS suite compares `Object.keys`; in Rust the struct type
    /// already guarantees identical keys, so we assert on values.)
    fn glyph_entries(g: &Glyphs) -> Vec<(&'static str, String)> {
        vec![
            ("ok", g.ok.to_string()),
            ("err", g.err.to_string()),
            ("info", g.info.to_string()),
            ("warn", g.warn.to_string()),
            ("spinner", g.spinner.join("")),
            ("barFilled", g.bar_filled.to_string()),
            ("barEmpty", g.bar_empty.to_string()),
            ("rail", g.rail.to_string()),
            ("phaseDone", g.phase_done.to_string()),
            ("dash", g.dash.to_string()),
            ("hLine", g.h_line.to_string()),
            ("treeBranch", g.tree_branch.to_string()),
            ("treeLast", g.tree_last.to_string()),
            ("treePipe", g.tree_pipe.to_string()),
        ]
    }

    // "ASCII and Unicode sets cover the same keys" — enforced by the type
    // system; assert both sets fill every field.
    #[test]
    fn ascii_and_unicode_sets_cover_the_same_keys() {
        let ascii = glyph_entries(&ASCII_GLYPHS);
        let unicode = glyph_entries(&UNICODE_GLYPHS);
        assert_eq!(ascii.len(), unicode.len());
        for ((ak, av), (uk, uv)) in ascii.iter().zip(unicode.iter()) {
            assert_eq!(ak, uk);
            assert!(!av.is_empty(), "ASCII_GLYPHS.{ak} is empty");
            assert!(!uv.is_empty(), "UNICODE_GLYPHS.{uk} is empty");
        }
    }

    // "ASCII glyphs are all 7-bit ASCII"
    #[test]
    fn ascii_glyphs_are_all_7_bit_ascii() {
        for (key, value) in glyph_entries(&ASCII_GLYPHS) {
            for ch in value.chars() {
                assert!(
                    (ch as u32) < 128,
                    "ASCII_GLYPHS.{key} contains non-ASCII char U+{:04X}",
                    ch as u32
                );
            }
        }
    }

    // "ASCII spinner has the same frame count as the Unicode spinner"
    #[test]
    fn ascii_spinner_has_same_frame_count_as_unicode_spinner() {
        assert_eq!(ASCII_GLYPHS.spinner.len(), UNICODE_GLYPHS.spinner.len());
    }
}
