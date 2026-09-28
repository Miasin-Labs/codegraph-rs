//! Running the real Miri: `cargo +nightly miri test`, bounded.
//!
//! Miri is built on `rustc_private` and pinned to one nightly, so it is a
//! tool to run, never a library to link. Each test binary is built once
//! (`--no-run`, under the build deadline) and each test then runs alone
//! (`-- --exact <path>`, under the run deadline): UB aborts the whole
//! interpreted binary, so one test per process is what attributes a report
//! to its test. Both go through the fuzz runner's bounded spawn — the
//! process group is killed at the deadline, and the log is capped.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::diagnostic::{Category, ParsedOutput, parse_output};
use super::select::TestTarget;
use crate::analyze::fuzz::run::{build_errors, run_bounded_capped};

/// Which aliasing model Miri checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Borrows {
    /// Miri's default.
    #[default]
    Stacked,
    /// `-Zmiri-tree-borrows`.
    Tree,
    /// `-Zmiri-disable-stacked-borrows`: no aliasing model.
    None,
}

/// How Miri runs.
#[derive(Debug, Clone)]
pub struct MiriOptions {
    pub borrows: Borrows,
    /// `-Zmiri-strict-provenance` (the default): integer-to-pointer casts
    /// are reported as unsupported instead of guessed.
    pub strict_provenance: bool,
    /// `-Zmiri-disable-isolation`: let the program see the host (files,
    /// clock, env). Off unless asked.
    pub disable_isolation: bool,
    /// `-Zmiri-ignore-leaks`.
    pub ignore_leaks: bool,
    /// More `MIRIFLAGS`, appended.
    pub extra_flags: Vec<String>,
    /// Let cargo use the network (default: `--offline`).
    pub online: bool,
    /// `CARGO_TARGET_DIR` (default: cargo's).
    pub target_dir: Option<PathBuf>,
    /// Wall clock for building a test binary under Miri.
    pub build_timeout: Duration,
    /// Wall clock for one test.
    pub run_timeout: Duration,
    /// A run whose log grows past this is killed.
    pub max_output: u64,
}

impl Default for MiriOptions {
    fn default() -> Self {
        Self {
            borrows: Borrows::Stacked,
            strict_provenance: true,
            disable_isolation: false,
            ignore_leaks: false,
            extra_flags: Vec::new(),
            online: false,
            target_dir: None,
            build_timeout: Duration::from_secs(900),
            run_timeout: Duration::from_secs(120),
            max_output: 64 * 1024 * 1024,
        }
    }
}

impl MiriOptions {
    /// The `MIRIFLAGS` value for these options.
    pub fn miriflags(&self) -> String {
        let mut flags: Vec<String> = Vec::new();
        if self.strict_provenance {
            flags.push("-Zmiri-strict-provenance".into());
        }
        match self.borrows {
            Borrows::Stacked => {}
            Borrows::Tree => flags.push("-Zmiri-tree-borrows".into()),
            Borrows::None => flags.push("-Zmiri-disable-stacked-borrows".into()),
        }
        if self.disable_isolation {
            flags.push("-Zmiri-disable-isolation".into());
        }
        if self.ignore_leaks {
            flags.push("-Zmiri-ignore-leaks".into());
        }
        flags.extend(self.extra_flags.iter().cloned());
        flags.join(" ")
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// Miri reported Undefined Behavior.
    Ub,
    /// Miri reported a memory leak.
    Leak,
    /// Miri cannot run this code (FFI, inline assembly, an unsupported or
    /// isolated operation, its own resource limit): nothing is proven
    /// either way.
    Unsupported,
    /// The test passed under Miri: no UB on this execution.
    Clean,
    /// The test failed (a panic, a failed assert) without UB.
    TestFailed,
    /// The program aborted (`abnormal termination`).
    Aborted,
    Deadlock,
    /// The test binary did not build (see the reason).
    BuildFailed,
    /// Killed at the wall-clock deadline.
    Timeout,
    /// Killed because its output passed the cap.
    OutputCapped,
    /// libtest skipped it (`#[ignore]`, `cfg_attr(miri, ignore)`).
    Ignored,
    /// No test by that name ran (compiled out under Miri?).
    NoTest,
    /// Ended without anything recognizable (see the log).
    Error,
}

impl RunStatus {
    /// The run proves nothing about the code either way.
    pub fn is_inconclusive(self) -> bool {
        !matches!(self, Self::Ub | Self::Leak | Self::Clean | Self::TestFailed)
    }
}

/// The status of a finished run.
pub fn status_of(parsed: &ParsedOutput, success: bool, killed: bool, capped: bool) -> RunStatus {
    if let Some(diagnostic) = parsed.primary() {
        return match diagnostic.category {
            Category::UndefinedBehavior => RunStatus::Ub,
            Category::MemoryLeak => RunStatus::Leak,
            Category::Unsupported | Category::ResourceExhaustion => RunStatus::Unsupported,
            Category::Abort => RunStatus::Aborted,
            Category::Deadlock => RunStatus::Deadlock,
            Category::CompileError => RunStatus::BuildFailed,
        };
    }
    if killed {
        return RunStatus::Timeout;
    }
    if capped {
        return RunStatus::OutputCapped;
    }
    if parsed.failed > 0 || !parsed.panics.is_empty() {
        return RunStatus::TestFailed;
    }
    if success {
        return if parsed.passed > 0 {
            RunStatus::Clean
        } else if parsed.ignored > 0 {
            RunStatus::Ignored
        } else {
            RunStatus::NoTest
        };
    }
    RunStatus::Error
}

/// How to get Miri, for when it is missing.
pub const INSTALL_HINT: &str = "Miri is not installed (or no nightly toolchain): \
    `rustup toolchain install nightly --component miri rust-src`";

/// `cargo +nightly miri --version` works.
pub fn miri_available() -> bool {
    Command::new("cargo")
        .args(["+nightly", "miri", "--version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One test binary of one package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Binary {
    /// The package's `Cargo.toml`.
    pub manifest: PathBuf,
    pub target: TestTarget,
}

/// What one invocation produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub status: RunStatus,
    /// The command, as a person would type it.
    pub command: String,
    pub elapsed_secs: f64,
    #[serde(skip)]
    pub parsed: ParsedOutput,
    /// The log file.
    pub log: String,
    /// Why it did not run or prove anything, in a line or a few: Miri's
    /// unsupported message, the build error, the panic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

fn base_command(binary: &Binary, options: &MiriOptions) -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["+nightly", "miri", "test"])
        .arg("--manifest-path")
        .arg(&binary.manifest)
        .args(binary.target.cargo_args())
        .env("MIRIFLAGS", options.miriflags())
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("RUSTFLAGS");
    if !options.online {
        command.arg("--offline").env("CARGO_NET_OFFLINE", "true");
    }
    if let Some(dir) = &options.target_dir {
        command.env("CARGO_TARGET_DIR", dir);
    }
    if let Some(dir) = binary.manifest.parent() {
        command.current_dir(dir);
    }
    command
}

fn render(binary: &Binary, options: &MiriOptions, tail: &str) -> String {
    format!(
        "MIRIFLAGS=\"{}\" cargo +nightly miri test --manifest-path {} {}{}{}",
        options.miriflags(),
        binary.manifest.display(),
        binary.target.cargo_args().join(" "),
        if options.online { "" } else { " --offline" },
        tail
    )
}

/// Build `binary` under Miri (`--no-run`).
pub fn build(binary: &Binary, options: &MiriOptions, log: &Path) -> Result<Invocation, String> {
    let started = Instant::now();
    let mut command = base_command(binary, options);
    command.arg("--no-run");
    let finished = run_bounded_capped(command, log, options.build_timeout, options.max_output)?;
    let parsed = parse_output(&finished.output);
    let status = if finished.success {
        RunStatus::Clean
    } else if finished.killed {
        RunStatus::Timeout
    } else if parsed.primary().is_some() {
        // A const evaluated at build time can itself be UB.
        status_of(&parsed, false, false, false)
    } else {
        RunStatus::BuildFailed
    };
    let reason = (status != RunStatus::Clean).then(|| build_reason(&finished.output, status));
    Ok(Invocation {
        status,
        command: render(binary, options, " --no-run"),
        elapsed_secs: started.elapsed().as_secs_f64(),
        parsed,
        log: log.to_string_lossy().to_string(),
        reason,
    })
}

/// Why a build failed, in a few lines: missing dependencies offline are
/// said plainly, anything else is the compiler's errors.
pub fn build_reason(output: &str, status: RunStatus) -> String {
    if status == RunStatus::Timeout {
        return "the build did not finish before the deadline".into();
    }
    let offline = [
        "no matching package named",
        "failed to download",
        "attempting to make an HTTP request, but --offline was specified",
        "you're using offline mode",
        "failed to get `",
        "failed to load source for dependency",
    ];
    if offline.iter().any(|needle| output.contains(needle)) {
        return "a dependency is not in the local cargo cache (the build runs offline; \
                pass --online to fetch)"
            .into();
    }
    build_errors(output)
}

/// rustc refused the code (`error[E0432]`, a deny-by-default lint such as
/// `invalid_reference_casting`, `aborting due to` errors).
pub fn is_compile_error(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.starts_with("error[E") || line.starts_with("error: aborting due to"))
        || output.contains("error: could not compile")
}

/// Run one test (`-- --exact <path>`), or every test of the binary when
/// `test` is `None`.
pub fn run_test(
    binary: &Binary,
    test: Option<&str>,
    options: &MiriOptions,
    log: &Path,
) -> Result<Invocation, String> {
    let started = Instant::now();
    let mut command = base_command(binary, options);
    let mut tail = String::new();
    if let Some(test) = test {
        command.args(["--", "--exact", test]);
        tail = format!(" -- --exact {test}");
    }
    let finished = run_bounded_capped(command, log, options.run_timeout, options.max_output)?;
    let parsed = parse_output(&finished.output);
    let mut status = status_of(&parsed, finished.success, finished.killed, finished.capped);
    // cargo-miri compiles the crate under test only when its runner starts
    // (`--no-run` builds the dependencies): a compile error shows up here.
    if status == RunStatus::Error && !parsed.ran_tests && is_compile_error(&finished.output) {
        status = RunStatus::BuildFailed;
    }
    let reason = match status {
        RunStatus::Unsupported | RunStatus::Aborted | RunStatus::Deadlock => {
            parsed.primary().map(|d| {
                let hint = d
                    .help
                    .iter()
                    .find(|h| h.contains("MIRIFLAGS") || h.contains("isolation"))
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default();
                format!("{}{hint}", d.message)
            })
        }
        RunStatus::TestFailed => parsed
            .panics
            .first()
            .map(|p| format!("panicked: {}", p.message)),
        RunStatus::Timeout => Some(format!(
            "killed after {}s (Miri interprets: a test that takes seconds natively can take \
             hours; raise --seconds or pick a smaller test)",
            options.run_timeout.as_secs()
        )),
        RunStatus::OutputCapped => Some("killed: its output passed the cap".into()),
        RunStatus::NoTest => {
            Some("no test by that name ran under Miri (compiled out with cfg(miri)?)".into())
        }
        RunStatus::Ignored => Some("libtest ignored it".into()),
        RunStatus::BuildFailed => Some(build_reason(&finished.output, status)),
        RunStatus::Error => Some(build_errors(&finished.output)),
        RunStatus::Ub | RunStatus::Leak | RunStatus::Clean => None,
    };
    Ok(Invocation {
        status,
        command: render(binary, options, &tail),
        elapsed_secs: started.elapsed().as_secs_f64(),
        parsed,
        log: log.to_string_lossy().to_string(),
        reason,
    })
}
