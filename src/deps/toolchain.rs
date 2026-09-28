//! The Rust toolchain's own library as a dependency: `std`, `core` and
//! `alloc` from the `rust-src` component, built into one read-only shard
//! per toolchain (`deps/rust/std-<release>+<commit>/`) and recorded for
//! every project with a `Cargo.lock`, like a crate it depends on.
//!
//! The toolchain is the one `rustc` resolves in the project's directory
//! (so a `rust-toolchain.toml` pin is honoured), asked with a deadline;
//! its library sources are `$(rustc --print sysroot)/lib/rustlib/src/rust/
//! library`, present only when `rust-src` is installed — without it there
//! is nothing to record, never an error. `CODEGRAPH_STD=0` turns this off;
//! `CODEGRAPH_RUST_SRC` (a `library` directory) with
//! `CODEGRAPH_RUST_VERSION` names one explicitly instead of asking rustc.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

/// The shard name of the toolchain's library.
pub const STD_NAME: &str = "std";
/// The library crates the shard holds, as code names them.
pub const STD_CRATES: &[&str] = &["std", "core", "alloc"];
/// What the registry records as the dependency's lockfile.
pub const TOOLCHAIN_LOCKFILE: &str = "rust-toolchain";
/// Longest wait for `rustc`.
const RUSTC_DEADLINE: Duration = Duration::from_secs(5);

/// One toolchain's library sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `1.101.0-nightly+d080e7dff1b0`: the release and its commit, so two
    /// nightlies of one release never share a shard.
    pub version: String,
    /// The `library` directory (`std/src/lib.rs` inside it).
    pub library: PathBuf,
}

impl Toolchain {
    pub fn key(&self) -> DepKey {
        DepKey::new(Ecosystem::Rust, STD_NAME, self.version.clone())
    }

    /// The toolchain as a dependency the registry records.
    pub fn as_dependency(&self) -> ResolvedDep {
        ResolvedDep {
            key: self.key(),
            lock_version: self.version.clone(),
            source: DepSource::Registry,
            direct: Some(true),
            lockfile: TOOLCHAIN_LOCKFILE.to_string(),
            install_path: None,
        }
    }
}

/// How [`super::locate::SourceRoots`] finds the toolchain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolchainSource {
    /// Not recorded (tests, explicit roots).
    #[default]
    None,
    /// Ask `rustc` in the project's directory.
    Detect,
    /// This one.
    Fixed(Toolchain),
}

impl ToolchainSource {
    /// The toolchain for the project at `project_root`, if its library
    /// sources are here.
    pub fn resolve(&self, project_root: &Path) -> Option<Toolchain> {
        match self {
            Self::None => None,
            Self::Fixed(toolchain) => Some(toolchain.clone()),
            Self::Detect => detect(project_root),
        }
    }
}

/// `CODEGRAPH_STD=0` (or `false`/`off`) turns the toolchain shard off.
pub fn std_enabled() -> bool {
    !std::env::var("CODEGRAPH_STD").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}

/// The toolchain `rustc` resolves in `project_root`, with its library
/// sources, or `None` (no rustc, no `rust-src`, or too slow to answer).
pub fn detect(project_root: &Path) -> Option<Toolchain> {
    if !std_enabled() {
        return None;
    }
    if let Some(library) = std::env::var_os("CODEGRAPH_RUST_SRC").filter(|v| !v.is_empty()) {
        let library = PathBuf::from(library);
        let version = std::env::var("CODEGRAPH_RUST_VERSION").ok()?;
        return is_library(&library).then(|| Toolchain {
            version: sanitize(&version),
            library,
        });
    }
    let verbose = rustc(project_root, &["-vV"])?;
    let field = |name: &str| {
        verbose
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|v| v.trim().to_string())
    };
    let release = field("release:")?;
    let commit: String = field("commit-hash:")
        .unwrap_or_default()
        .chars()
        .take(12)
        .collect();
    let sysroot = rustc(project_root, &["--print", "sysroot"])?;
    let library = Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !is_library(&library) {
        return None;
    }
    let version = if commit.is_empty() || commit == "unknown" {
        release
    } else {
        format!("{release}+{commit}")
    };
    Some(Toolchain {
        version: sanitize(&version),
        library: std::fs::canonicalize(&library).unwrap_or(library),
    })
}

fn is_library(dir: &Path) -> bool {
    dir.join("std/src/lib.rs").is_file() && dir.join("core/src/lib.rs").is_file()
}

/// `rustc <args>` in `dir`, its stdout, within [`RUSTC_DEADLINE`].
fn rustc(dir: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() >= RUSTC_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// A version as one path segment's worth of text.
fn sanitize(version: &str) -> String {
    version
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_toolchain_is_a_direct_rust_dependency() {
        let toolchain = Toolchain {
            version: "1.99.0+abc".into(),
            library: PathBuf::from("/sysroot/lib/rustlib/src/rust/library"),
        };
        let dep = toolchain.as_dependency();
        assert_eq!(dep.key.ecosystem, Ecosystem::Rust);
        assert_eq!(dep.key.dir_name(), "std-1.99.0+abc");
        assert_eq!(dep.direct, Some(true));
        assert_eq!(
            ToolchainSource::Fixed(toolchain.clone()).resolve(Path::new("/p")),
            Some(toolchain)
        );
        assert_eq!(ToolchainSource::None.resolve(Path::new("/p")), None);
    }

    #[test]
    fn versions_stay_one_segment() {
        assert_eq!(
            sanitize("1.101.0-nightly+d080e7dff1b0"),
            "1.101.0-nightly+d080e7dff1b0"
        );
        assert_eq!(sanitize("1.0 (x/y)"), "1.0__x_y_");
    }
}
