//! Start, resume, and read a time-boxed checker run.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Checker, Diagnostic, cargo, parse, tsc};
use crate::directory::get_codegraph_dir;
use crate::utils::is_process_alive;

/// A result younger than this is returned instead of starting another run,
/// so an agent that asks twice in a row does not rebuild twice.
const REUSE_WITHIN: Duration = Duration::from_secs(10);

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// The checker finished; `diagnostics` is complete.
    Complete,
    /// Still running in the background; call again for the result.
    Running,
}

/// The outcome of one `check_or_poll` call.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsRun {
    pub checker: Checker,
    pub status: RunStatus,
    pub started_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    /// The checker's exit code, when this process saw it exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
    /// Tail of stderr when the checker failed without diagnostics (e.g. a
    /// broken Cargo.toml), so the failure is visible rather than "0 errors".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunState {
    pid: u32,
    started_ms: u64,
    finished_ms: Option<u64>,
    exit_code: Option<i32>,
}

/// Children this process started, so they can be reaped with `try_wait`
/// (a finished-but-unreaped child still looks alive to a pid probe).
static CHILDREN: LazyLock<Mutex<HashMap<PathBuf, Child>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

struct Paths {
    state: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

fn paths(root: &Path, checker: Checker) -> Paths {
    let dir = get_codegraph_dir(root).join("diagnostics");
    let name = checker.as_str();
    Paths {
        state: dir.join(format!("{name}.state.json")),
        stdout: dir.join(format!("{name}.out")),
        stderr: dir.join(format!("{name}.err")),
    }
}

fn load(paths: &Paths) -> Option<RunState> {
    serde_json::from_str(&fs::read_to_string(&paths.state).ok()?).ok()
}

fn store(paths: &Paths, state: &RunState) -> std::io::Result<()> {
    fs::write(&paths.state, serde_json::to_vec(state).unwrap_or_default())
}

fn command(root: &Path, checker: Checker) -> Option<Command> {
    let mut command = match checker {
        Checker::Check | Checker::Clippy => cargo::command(checker),
        Checker::Tsc => tsc::command(root)?,
    };
    command.current_dir(root).stdin(Stdio::null());
    // Its own process group: the run must outlive a request that times out.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Some(command)
}

fn finished(root: &Path, checker: Checker, paths: &Paths, state: &RunState) -> DiagnosticsRun {
    let stdout = fs::read_to_string(&paths.stdout).unwrap_or_default();
    let diagnostics = parse(checker, root, &stdout);
    let stderr = fs::read_to_string(&paths.stderr).unwrap_or_default();
    // A run another process started has no exit code here; its stderr still
    // shows a failure (cargo prints `error: …` for a manifest or an offline
    // dependency it cannot resolve).
    let failed = state.exit_code.map_or_else(
        || stderr.lines().any(|line| line.starts_with("error")),
        |code| code != 0,
    );
    let failure = (failed && diagnostics.is_empty()).then(|| {
        let tail: Vec<&str> = stderr.lines().rev().take(15).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    });
    DiagnosticsRun {
        checker,
        status: RunStatus::Complete,
        started_ms: state.started_ms,
        finished_ms: state.finished_ms,
        exit_code: state.exit_code,
        diagnostics,
        failure,
    }
}

fn running(checker: Checker, state: &RunState) -> DiagnosticsRun {
    DiagnosticsRun {
        checker,
        status: RunStatus::Running,
        started_ms: state.started_ms,
        finished_ms: None,
        exit_code: None,
        diagnostics: Vec::new(),
        failure: None,
    }
}

/// Wait for a run this process owns, up to `deadline` or until `stop()`.
fn wait_own(key: &Path, deadline: Instant, stop: &dyn Fn() -> bool) -> Option<Option<i32>> {
    loop {
        let mut children = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
        let child = children.get_mut(key)?;
        match child.try_wait() {
            Ok(Some(status)) => {
                children.remove(key);
                return Some(status.code());
            }
            Ok(None) if Instant::now() < deadline && !stop() => {}
            Ok(None) => return None,
            Err(_) => {
                children.remove(key);
                return Some(None);
            }
        }
        drop(children);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run the checker, or pick up a run already in progress, and wait at most
/// `wait` for it (less if `stop()` turns true, e.g. a cancelled request).
/// Never blocks past the deadline: an unfinished run keeps going detached and
/// the next call returns its result.
pub fn check_or_poll(
    root: &Path,
    checker: Checker,
    wait: Duration,
    stop: &dyn Fn() -> bool,
) -> Result<DiagnosticsRun, String> {
    let paths = paths(root, checker);
    let deadline = Instant::now() + wait;
    let key = paths.state.clone();

    // 1. A run this process started: reap it if done.
    let owned = CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&key);
    if owned {
        let mut state = load(&paths).unwrap_or_default();
        return Ok(match wait_own(&key, deadline, stop) {
            Some(code) => {
                state.finished_ms = Some(now_ms());
                state.exit_code = code;
                let _ = store(&paths, &state);
                finished(root, checker, &paths, &state)
            }
            None => running(checker, &state),
        });
    }

    // 2. A run another process started (e.g. a previous server).
    if let Some(mut state) = load(&paths) {
        if state.finished_ms.is_none() {
            while is_process_alive(state.pid) {
                if Instant::now() >= deadline || stop() {
                    return Ok(running(checker, &state));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            state.finished_ms = Some(now_ms());
            let _ = store(&paths, &state);
            return Ok(finished(root, checker, &paths, &state));
        }
        // 3. A result fresh enough to reuse.
        if state
            .finished_ms
            .is_some_and(|done| now_ms().saturating_sub(done) < REUSE_WITHIN.as_millis() as u64)
        {
            return Ok(finished(root, checker, &paths, &state));
        }
    }

    // 4. Start a new run.
    fs::create_dir_all(paths.state.parent().unwrap_or(root)).map_err(|e| e.to_string())?;
    let stdout = fs::File::create(&paths.stdout).map_err(|e| e.to_string())?;
    let stderr = fs::File::create(&paths.stderr).map_err(|e| e.to_string())?;
    let mut command = command(root, checker).ok_or_else(|| {
        format!(
            "{} is not available for {}",
            checker.as_str(),
            root.display()
        )
    })?;
    let child = command
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", checker.as_str()))?;
    let state = RunState {
        pid: child.id(),
        started_ms: now_ms(),
        finished_ms: None,
        exit_code: None,
    };
    store(&paths, &state).map_err(|e| e.to_string())?;
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone(), child);
    let mut state = state;
    Ok(match wait_own(&key, deadline, stop) {
        Some(code) => {
            state.finished_ms = Some(now_ms());
            state.exit_code = code;
            let _ = store(&paths, &state);
            finished(root, checker, &paths, &state)
        }
        None => running(checker, &state),
    })
}
