//! `codegraph analyze fuzz-run`: a bounded `cargo +nightly fuzz run`, its
//! outcome read back from libFuzzer's output and mapped to the indexed
//! function that failed.
//!
//! Bounded twice: libFuzzer gets `-max_total_time`, and the process group
//! is killed at a wall-clock deadline (build and run each have one), so a
//! wedged build or a harness that ignores the time limit still ends.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;

/// What to run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub target: String,
    /// Fuzzing time (libFuzzer `-max_total_time`), 1..=3600.
    pub seconds: u64,
    /// The cargo-fuzz directory (holds `Cargo.toml` and `fuzz_targets/`).
    pub fuzz_dir: PathBuf,
    /// Run this one input instead of fuzzing (reproduce a crash).
    pub input: Option<PathBuf>,
    /// Wall-clock limit for the build.
    pub build_timeout: Duration,
    /// Sanitizer (`address`, `none`…); cargo-fuzz's default when `None`.
    pub sanitizer: Option<String>,
}

/// How the run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// A crash: panic, sanitizer report, or fatal signal.
    Crash,
    /// An input ran longer than libFuzzer's per-input timeout.
    Timeout,
    OutOfMemory,
    Leak,
    /// Fuzzed for the whole time without failing.
    NoCrash,
    BuildFailed,
    /// Killed at the wall-clock deadline.
    Killed,
    /// Ended without anything recognizable (see the log).
    Error,
}

/// A source location from the report (panic site or stack frame).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub file: String,
    pub line: u32,
}

/// What the output says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedRun {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproducer: Option<String>,
    /// `panic`, a sanitizer error (`heap-buffer-overflow`,
    /// `stack-overflow`…), `deadly-signal`, `timeout`, `out-of-memory`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The panic message, or the sanitizer's summary line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The panic location first, then stack frames in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<Frame>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<u64>,
}

static PANIC_NEW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at ([^\s:][^:]*):(\d+):\d+:\s*$").expect("valid regex"));
static PANIC_OLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at '(.*)', ([^:]+):(\d+):\d+").expect("valid regex"));
static SANITIZER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ERROR: (?:Address|Memory|Thread|Leak)Sanitizer: ([A-Za-z0-9_-]+)")
        .expect("valid regex")
});
static FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*#\d+ 0x[0-9a-f]+ in (\S+) (/[^:\s]+|[^:\s]+\.rs):(\d+)").expect("valid regex")
});
static ARTIFACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Test unit written to (\S+)").expect("valid regex"));
static DONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Done (\d+) runs in \d+ second").expect("valid regex"));
static STATUS_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^#(\d+)\s+(?:DONE|pulse|NEW|REDUCE|INITED)").expect("valid regex")
});

/// Read libFuzzer/cargo-fuzz output.
pub fn parse_output(output: &str) -> ParsedRun {
    let mut parsed = ParsedRun::default();
    let lines: Vec<&str> = output.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if parsed.kind.is_none() || parsed.kind.as_deref() == Some("deadly-signal") {
            if let Some(caps) = PANIC_NEW.captures(line) {
                parsed.kind = Some("panic".to_string());
                parsed.message = lines.get(index + 1).map(|m| m.trim().to_string());
                parsed.frames.insert(
                    0,
                    Frame {
                        symbol: None,
                        file: caps[1].to_string(),
                        line: caps[2].parse().unwrap_or(0),
                    },
                );
                continue;
            }
            if let Some(caps) = PANIC_OLD.captures(line) {
                parsed.kind = Some("panic".to_string());
                parsed.message = Some(caps[1].to_string());
                parsed.frames.insert(
                    0,
                    Frame {
                        symbol: None,
                        file: caps[2].to_string(),
                        line: caps[3].parse().unwrap_or(0),
                    },
                );
                continue;
            }
        }
        if let Some(caps) = SANITIZER.captures(line) {
            if parsed.kind.as_deref() != Some("panic") {
                parsed.kind = Some(caps[1].to_string());
                parsed.message = Some(
                    line.trim()
                        .trim_start_matches(|c| c == '=' || char::is_numeric(c))
                        .trim()
                        .to_string(),
                );
            }
            continue;
        }
        if line.contains("ERROR: libFuzzer: deadly signal") && parsed.kind.is_none() {
            parsed.kind = Some("deadly-signal".to_string());
        } else if line.contains("ERROR: libFuzzer: timeout") {
            parsed.kind = Some("timeout".to_string());
            parsed.message = Some(line.trim().to_string());
        } else if line.contains("ERROR: libFuzzer: out-of-memory") {
            parsed.kind = Some("out-of-memory".to_string());
            parsed.message = Some(line.trim().to_string());
        } else if line.contains("ERROR: libFuzzer: fuzz target exited") && parsed.kind.is_none() {
            parsed.kind = Some("exit".to_string());
        }
        if let Some(caps) = FRAME.captures(line).filter(|_| parsed.frames.len() < 64) {
            let frame = Frame {
                symbol: Some(caps[1].to_string()),
                file: caps[2].to_string(),
                line: caps[3].parse().unwrap_or(0),
            };
            if !parsed.frames.contains(&frame) {
                parsed.frames.push(frame);
            }
        }
        // The first one: cargo-fuzz later echoes inputs (and their Debug
        // form) that can quote this line again.
        if let Some(caps) = ARTIFACT
            .captures(line)
            .filter(|_| parsed.reproducer.is_none())
        {
            parsed.reproducer = Some(caps[1].to_string());
        }
        if let Some(caps) = DONE.captures(line) {
            parsed.runs = caps[1].parse().ok();
        } else if let Some(caps) = STATUS_LINE.captures(line) {
            parsed.runs = caps[1].parse().ok().max(parsed.runs);
        }
    }
    parsed
}

/// The status `parsed` (from a process that exited with `success`) means.
pub fn status_of(parsed: &ParsedRun, success: bool) -> RunStatus {
    let artifact_kind = parsed
        .reproducer
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    match parsed.kind.as_deref() {
        Some("timeout") => RunStatus::Timeout,
        Some("out-of-memory") => RunStatus::OutOfMemory,
        Some(kind) if kind.contains("leak") => RunStatus::Leak,
        Some(_) => RunStatus::Crash,
        None if artifact_kind.starts_with("timeout-") => RunStatus::Timeout,
        None if artifact_kind.starts_with("oom-") => RunStatus::OutOfMemory,
        None if artifact_kind.starts_with("leak-") => RunStatus::Leak,
        None if artifact_kind.starts_with("crash-") => RunStatus::Crash,
        None if success => RunStatus::NoCrash,
        None => RunStatus::Error,
    }
}

/// How to get cargo-fuzz, for when it is missing.
pub const INSTALL_HINT: &str = "cargo-fuzz is not installed (or no nightly toolchain): \
    `cargo install cargo-fuzz` and `rustup toolchain install nightly`";

/// `cargo +nightly fuzz --version` works.
pub fn cargo_fuzz_available() -> bool {
    Command::new("cargo")
        .args(["+nightly", "fuzz", "--version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The finished (or killed) process.
pub(crate) struct Finished {
    pub(crate) success: bool,
    /// Killed at the deadline.
    pub(crate) killed: bool,
    /// Killed because its output passed the cap.
    pub(crate) capped: bool,
    /// The output (for a large log: its start and its end, where the
    /// verdict is).
    pub(crate) output: String,
}

/// Run `command` with its output in `log`, killing its whole process group
/// at `deadline`.
fn run_bounded(command: Command, log: &Path, deadline: Duration) -> Result<Finished, String> {
    run_bounded_capped(command, log, deadline, u64::MAX)
}

/// How much of a log is read back: the first bytes, and the last ones.
const LOG_HEAD: u64 = 64 * 1024;
const LOG_TAIL: u64 = 1024 * 1024;

/// [`run_bounded`], also killing the process group once `log` holds more
/// than `max_output` bytes (a test that prints forever must not fill the
/// disk). Only the head and tail of a large log are read back.
pub(crate) fn run_bounded_capped(
    mut command: Command,
    log: &Path,
    deadline: Duration,
    max_output: u64,
) -> Result<Finished, String> {
    let file =
        std::fs::File::create(log).map_err(|e| format!("cannot create {}: {e}", log.display()))?;
    let err_file = file
        .try_clone()
        .map_err(|e| format!("cannot open {}: {e}", log.display()))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    let started = Instant::now();
    let over_cap = || std::fs::metadata(log).is_ok_and(|m| m.len() > max_output);
    let (success, killed, capped) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.success(), false, false),
            Ok(None) if started.elapsed() >= deadline || over_cap() => {
                let capped = started.elapsed() < deadline;
                kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                break (false, !capped, capped);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("waiting for cargo: {e}")),
        }
    };
    let output = read_head_tail(log).map_err(|e| format!("cannot read {}: {e}", log.display()))?;
    Ok(Finished {
        success,
        killed,
        capped,
        output,
    })
}

/// `log` whole when small, else its first [`LOG_HEAD`] and last
/// [`LOG_TAIL`] bytes around an elision line.
fn read_head_tail(log: &Path) -> std::io::Result<String> {
    use std::io::{Seek as _, SeekFrom};
    let mut file = std::fs::File::open(log)?;
    let len = file.metadata()?.len();
    if len <= LOG_HEAD + LOG_TAIL {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut head = vec![0; LOG_HEAD as usize];
    file.read_exact(&mut head)?;
    file.seek(SeekFrom::Start(len - LOG_TAIL))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    Ok(format!(
        "{}\n… {} bytes left out …\n{}",
        String::from_utf8_lossy(&head),
        len - LOG_HEAD - LOG_TAIL,
        String::from_utf8_lossy(&tail)
    ))
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// The outcome of [`run`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcome {
    pub status: RunStatus,
    pub target: String,
    pub command: String,
    pub elapsed_secs: f64,
    #[serde(flatten)]
    pub parsed: ParsedRun,
    /// The full output.
    pub log: String,
    /// The last lines of a failed build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_error: Option<String>,
}

/// Code (not comments) still calls `todo!(`.
fn has_todo(source: &str) -> bool {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .any(|code| code.contains("todo!("))
}

/// The compiler's errors from a failed build (each `error` line and the
/// few after it), else the output's last lines.
pub fn build_errors(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut picked: Vec<&str> = Vec::new();
    let mut keep = 0;
    for line in &lines {
        if line.starts_with("error") {
            keep = 8;
        }
        if keep > 0 {
            picked.push(line);
            keep -= 1;
        }
        if picked.len() >= 60 {
            break;
        }
    }
    if picked.is_empty() {
        picked = lines[lines.len().saturating_sub(30)..].to_vec();
    }
    picked.join("\n")
}

/// Build, then fuzz (or replay `input`), each under its deadline.
pub fn run(options: &RunOptions) -> Result<RunOutcome, String> {
    if !cargo_fuzz_available() {
        return Err(INSTALL_HINT.to_string());
    }
    let fuzz_dir = &options.fuzz_dir;
    if !fuzz_dir.join("Cargo.toml").is_file() {
        return Err(format!(
            "no cargo-fuzz project at {} (generate one with `codegraph analyze fuzz-harness`)",
            fuzz_dir.display()
        ));
    }
    // A generated harness with TODOs panics in its own `todo!()`, which
    // would read as a crash of the code under test.
    let harness = fuzz_dir
        .join("fuzz_targets")
        .join(format!("{}.rs", options.target));
    if let Ok(source) = std::fs::read_to_string(&harness) {
        if has_todo(&source) {
            return Err(format!(
                "{} still has todo!() placeholders: fill them in first (see its TODO comments)",
                harness.display()
            ));
        }
    }
    let workdir = fuzz_dir.parent().unwrap_or(fuzz_dir);
    let artifacts = fuzz_dir.join("artifacts").join(&options.target);
    std::fs::create_dir_all(&artifacts)
        .map_err(|e| format!("cannot create {}: {e}", artifacts.display()))?;
    let base = |subcommand: &str| {
        let mut command = Command::new("cargo");
        command
            .current_dir(workdir)
            .args(["+nightly", "fuzz", subcommand, "--fuzz-dir"])
            .arg(fuzz_dir)
            .arg("--debug-assertions");
        if let Some(sanitizer) = &options.sanitizer {
            command.args(["--sanitizer", sanitizer]);
        }
        command.arg(&options.target);
        command
    };
    let started = Instant::now();

    // Logs live apart from the artifacts: cargo-fuzz reports every file
    // there as a failing input.
    let logs = fuzz_dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("cannot create {}: {e}", logs.display()))?;
    let build_log = logs.join(format!("{}-build.log", options.target));
    let build = run_bounded(base("build"), &build_log, options.build_timeout)?;
    if build.killed || !build.success {
        return Ok(RunOutcome {
            status: if build.killed {
                RunStatus::Killed
            } else {
                RunStatus::BuildFailed
            },
            target: options.target.clone(),
            command: format!("cargo +nightly fuzz build {}", options.target),
            elapsed_secs: started.elapsed().as_secs_f64(),
            parsed: ParsedRun::default(),
            log: build_log.to_string_lossy().to_string(),
            build_error: Some(build_errors(&build.output)),
        });
    }

    let seconds = options.seconds.clamp(1, 3600);
    let mut command = base("run");
    if let Some(input) = &options.input {
        command.arg(input);
    }
    command
        .arg("--")
        .arg(format!("-artifact_prefix={}/", artifacts.display()));
    if options.input.is_some() {
        command.arg("-runs=1");
    } else {
        command
            .arg(format!("-max_total_time={seconds}"))
            .arg("-timeout=10")
            .arg("-rss_limit_mb=2048")
            .arg("-max_len=65536");
    }
    let rendered = format!(
        "cargo +nightly fuzz run --debug-assertions {}{} -- {}",
        options.target,
        options
            .input
            .as_ref()
            .map(|p| format!(" {}", p.display()))
            .unwrap_or_default(),
        if options.input.is_some() {
            "-runs=1".to_string()
        } else {
            format!("-max_total_time={seconds} -timeout=10 -rss_limit_mb=2048")
        }
    );
    let run_log = logs.join(format!("{}-run.log", options.target));
    // libFuzzer stops itself at max_total_time; the margin covers startup,
    // writing the crash and symbolizing the stack.
    let deadline = Duration::from_secs(seconds + 90);
    let finished = run_bounded(command, &run_log, deadline)?;
    let parsed = parse_output(&finished.output);
    let status = if finished.killed && parsed.kind.is_none() {
        RunStatus::Killed
    } else {
        status_of(&parsed, finished.success)
    };
    Ok(RunOutcome {
        status,
        target: options.target.clone(),
        command: rendered,
        elapsed_secs: started.elapsed().as_secs_f64(),
        parsed,
        log: run_log.to_string_lossy().to_string(),
        build_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANIC_RUN: &str = "\
INFO: Running with entropic power schedule (0xFF, 100).
#2\tINITED cov: 20 ft: 21 corp: 1/1b exec/s: 0 rss: 30Mb
#1045\tNEW    cov: 88 ft: 100 corp: 7/40b lim: 14 exec/s: 0 rss: 31Mb
thread '<unnamed>' panicked at /work/blurhash/src/lib.rs:180:22:
index out of bounds: the len is 83 but the index is 83
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
==12345== ERROR: libFuzzer: deadly signal
    #0 0x55d5c1 in __sanitizer_print_stack_trace /rustc/llvm/asan_stack.cpp:87:3
    #7 0x55e0a2 in blurhash::components::h9a8b7c /work/blurhash/src/lib.rs:180:22
    #8 0x55e1f0 in blurhash::decode::h1234 /work/blurhash/src/lib.rs:125:30
    #9 0x55e2aa in decode::_::__libfuzzer_sys_run /work/blurhash/fuzz/fuzz_targets/decode.rs:9:13
artifact_prefix='/work/blurhash/fuzz/artifacts/decode/'; Test unit written to /work/blurhash/fuzz/artifacts/decode/crash-0f3a
Base64: IyMj
";

    #[test]
    fn a_panic_names_its_site_frames_and_reproducer() {
        let parsed = parse_output(PANIC_RUN);
        assert_eq!(parsed.kind.as_deref(), Some("panic"));
        assert_eq!(
            parsed.message.as_deref(),
            Some("index out of bounds: the len is 83 but the index is 83")
        );
        assert_eq!(
            parsed.frames[0],
            Frame {
                symbol: None,
                file: "/work/blurhash/src/lib.rs".into(),
                line: 180
            }
        );
        assert!(
            parsed
                .frames
                .iter()
                .any(|f| f.symbol.as_deref() == Some("blurhash::decode::h1234"))
        );
        assert_eq!(
            parsed.reproducer.as_deref(),
            Some("/work/blurhash/fuzz/artifacts/decode/crash-0f3a")
        );
        assert_eq!(parsed.runs, Some(1045));
        assert_eq!(status_of(&parsed, false), RunStatus::Crash);
    }

    #[test]
    fn sanitizer_reports_and_clean_runs() {
        let asan = "==77==ERROR: AddressSanitizer: stack-overflow on address 0x7ffc\n\
                    #0 0x1 in yaml_rust::scanner::Scanner::fetch_more_tokens /w/src/scanner.rs:600:9\n\
                    Test unit written to ./artifacts/crash-1\n";
        let parsed = parse_output(asan);
        assert_eq!(parsed.kind.as_deref(), Some("stack-overflow"));
        assert_eq!(parsed.frames[0].file, "/w/src/scanner.rs");
        assert_eq!(status_of(&parsed, false), RunStatus::Crash);

        let clean = "#524288\tpulse  cov: 90 ft: 200\nDone 1000000 runs in 61 second(s)\n";
        let parsed = parse_output(clean);
        assert_eq!(parsed.runs, Some(1_000_000));
        assert_eq!(status_of(&parsed, true), RunStatus::NoCrash);

        let timeout = "ALARM: working on the last Unit for 11 seconds\n\
                       ==9== ERROR: libFuzzer: timeout after 11 seconds\n\
                       Test unit written to ./timeout-abc\n";
        assert_eq!(status_of(&parse_output(timeout), false), RunStatus::Timeout);
        let oom = "==9== ERROR: libFuzzer: out-of-memory (malloc(4294967296))\n";
        assert_eq!(status_of(&parse_output(oom), false), RunStatus::OutOfMemory);
        assert_eq!(status_of(&ParsedRun::default(), false), RunStatus::Error);
    }

    #[test]
    fn todo_placeholders_block_a_run() {
        assert!(has_todo("    let x = todo!(\"build `x`\");\n"));
        assert!(!has_todo(
            "// TODO: build `x` — was todo!()\nlet _ = f(data);\n"
        ));
    }

    #[test]
    fn build_errors_keep_the_compiler_errors() {
        let output = "   Compiling demo v0.1.0\nerror[E0308]: mismatched types\n --> src/a.rs:3:9\n\
                      Error: failed to build fuzz script: ASAN_OPTIONS=...\nStack backtrace:\n   0: x\n";
        let excerpt = build_errors(output);
        assert!(excerpt.starts_with("error[E0308]: mismatched types\n --> src/a.rs:3:9"));
        assert!(!excerpt.contains("Compiling"));
        assert_eq!(build_errors("one\ntwo"), "one\ntwo");
    }

    #[test]
    fn old_panic_format_is_read_too() {
        let parsed = parse_output(
            "thread 'main' panicked at 'attempt to subtract with overflow', src/a.rs:3:9\n",
        );
        assert_eq!(
            parsed.message.as_deref(),
            Some("attempt to subtract with overflow")
        );
        assert_eq!(parsed.frames[0].file, "src/a.rs");
        assert_eq!(parsed.frames[0].line, 3);
    }
}
