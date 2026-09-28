//! The CodeQL pipeline as one detached, bounded run.
//!
//! Per language: `codeql database create` (skipped while the database's key
//! — CodeQL version, language, build mode, the language's indexed files by
//! size and mtime, the build manifests — is unchanged), then `codeql
//! database analyze` into SARIF (skipped while its key — the database's
//! plus the suites — is unchanged). What is still missing runs as one
//! generated POSIX shell script (`.codegraph/codeql/run/run.sh`) started in
//! its own process group, so it outlives a caller that stops waiting; a
//! watchdog inside the script kills the whole group when the budget
//! (`CODEGRAPH_CODEQL_BUDGET_MS`, 60 min) is spent. A later call picks up a
//! run still going instead of starting another, as `diagnostics` does.
//! Outputs are written to `.tmp` names and renamed into place with their
//! key, so an interrupted run never leaves a database or SARIF file that
//! looks current.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::languages::{BuildMode, CodeqlLanguage};
use super::locate::CodeqlTool;
use crate::directory::get_codegraph_dir;
use crate::utils::is_process_alive;

/// Bump when the layout or the meaning of a key changes.
const CACHE_VERSION: u32 = 1;

/// A failed run for the same plan is reported, not retried, this soon
/// after it ended (asking twice in a row must not rebuild twice).
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Default wall-clock budget of one run.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(60 * 60);

/// Files outside the index that change what an extractor sees.
const BUILD_FILES: &[&str] = &[
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "tsconfig.json",
    "go.mod",
    "go.sum",
    "requirements.txt",
    "pyproject.toml",
    "setup.py",
    "Gemfile",
    "Gemfile.lock",
    "Cargo.toml",
    "Cargo.lock",
    "CMakeLists.txt",
    "Makefile",
    "compile_commands.json",
];

/// Where everything lives: `.codegraph/codeql/`.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: get_codegraph_dir(root).join("codeql"),
        }
    }
    pub fn db(&self, lang: &str) -> PathBuf {
        self.dir.join("db").join(lang)
    }
    fn db_key(&self, lang: &str) -> PathBuf {
        self.dir.join("db").join(format!("{lang}.key"))
    }
    pub fn sarif(&self, lang: &str) -> PathBuf {
        self.dir.join("sarif").join(format!("{lang}.sarif"))
    }
    fn sarif_key(&self, lang: &str) -> PathBuf {
        self.dir.join("sarif").join(format!("{lang}.key"))
    }
    fn run_dir(&self) -> PathBuf {
        self.dir.join("run")
    }
    fn state(&self) -> PathBuf {
        self.run_dir().join("state.json")
    }
    fn script(&self) -> PathBuf {
        self.run_dir().join("run.sh")
    }
    fn config(&self) -> PathBuf {
        self.dir.join("codescanning-config.yml")
    }
    fn step_file(&self, lang: &str, step: &str, ext: &str) -> PathBuf {
        self.run_dir().join(format!("{lang}.{step}.{ext}"))
    }
    fn timeout_marker(&self) -> PathBuf {
        self.run_dir().join("timeout")
    }
}

/// What one language needs.
#[derive(Debug, Clone)]
pub struct LangPlan {
    pub lang: &'static CodeqlLanguage,
    pub build_mode: BuildMode,
    /// Suite/pack/query arguments for `database analyze`.
    pub suites: Vec<String>,
    pub db_key: String,
    pub analysis_key: String,
    /// The language's indexed files (what the database key covers).
    pub files: Vec<String>,
}

impl LangPlan {
    pub fn new(
        root: &Path,
        tool: &CodeqlTool,
        lang: &'static CodeqlLanguage,
        build_mode: BuildMode,
        files: &[String],
        suites: Vec<String>,
    ) -> Self {
        let db_key = db_key(root, tool, lang, build_mode, files);
        let mut hasher = Sha256::new();
        hasher.update(db_key.as_bytes());
        for suite in &suites {
            hasher.update([0]);
            hasher.update(suite.as_bytes());
        }
        Self {
            lang,
            build_mode,
            suites,
            db_key,
            analysis_key: format!("{:x}", hasher.finalize()),
            files: files.to_vec(),
        }
    }

    fn needs_create(&self, layout: &Layout) -> bool {
        read_key(&layout.db_key(self.lang.id)).as_deref() != Some(self.db_key.as_str())
            || !layout.db(self.lang.id).is_dir()
    }

    fn needs_analyze(&self, layout: &Layout) -> bool {
        read_key(&layout.sarif_key(self.lang.id)).as_deref() != Some(self.analysis_key.as_str())
            || !layout.sarif(self.lang.id).is_file()
    }
}

fn read_key(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn mtime_ns(meta: &fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default()
        .as_nanos()
}

/// The database key: CodeQL version, language and build mode, and the
/// language's files and the build manifests by path, size and mtime.
fn db_key(
    root: &Path,
    tool: &CodeqlTool,
    lang: &CodeqlLanguage,
    build_mode: BuildMode,
    files: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_VERSION.to_le_bytes());
    for part in [tool.version.as_str(), lang.id, build_mode.as_str()] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let mut paths: Vec<&str> = files.iter().map(String::as_str).collect();
    paths.extend(BUILD_FILES);
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        let Ok(meta) = fs::metadata(root.join(path)) else {
            continue;
        };
        let mtime = mtime_ns(&meta);
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(meta.len().to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// A run's state file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunState {
    pid: u32,
    started_ms: u64,
    finished_ms: Option<u64>,
    /// The plans it was started for (hash of their keys).
    plan: String,
    budget_ms: u64,
}

/// How a language came out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    /// Its SARIF is current (from this run or an earlier one).
    Complete,
    Running,
    Failed,
}

/// One language's outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LangOutcome {
    pub language: String,
    pub build_mode: BuildMode,
    pub state: StepState,
    /// Both the database and the SARIF were current before this call.
    pub cached: bool,
    pub suites: Vec<String>,
    pub database: String,
    /// Bytes on disk of the database directory.
    pub database_bytes: u64,
    /// Seconds `database create` took (when it ran in the last run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_secs: Option<u64>,
    /// Seconds `database analyze` took (when it ran in the last run).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyze_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip)]
    pub sarif: Option<PathBuf>,
}

/// The whole call's outcome.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub running: bool,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    /// A source changed after the run started: its results may be out of
    /// date (the next call runs again).
    pub stale: bool,
    pub languages: Vec<LangOutcome>,
}

/// Children this process started, reaped with `try_wait` (a finished but
/// unreaped child still looks alive to a pid probe).
static CHILDREN: LazyLock<Mutex<HashMap<PathBuf, Child>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn budget() -> Duration {
    std::env::var("CODEGRAPH_CODEQL_BUDGET_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map_or(DEFAULT_BUDGET, Duration::from_millis)
}

fn plan_hash(plans: &[LangPlan]) -> String {
    let mut hasher = Sha256::new();
    for plan in plans {
        hasher.update(plan.analysis_key.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

/// Bring every plan's SARIF up to date — reuse, pick up a run in progress,
/// or start one — waiting at most `wait`.
pub fn ensure(
    root: &Path,
    tool: &CodeqlTool,
    plans: &[LangPlan],
    wait: Duration,
) -> Result<RunOutcome, String> {
    let layout = Layout::new(root);
    let deadline = Instant::now() + wait;
    let key = layout.state();
    let plan = plan_hash(plans);
    let cached: Vec<bool> = plans
        .iter()
        .map(|p| !p.needs_create(&layout) && !p.needs_analyze(&layout))
        .collect();
    if cached.iter().all(|&c| c) {
        return Ok(outcome(&layout, plans, &cached, None, false));
    }

    let mut state = load(&key);
    // A run in progress (ours, or another process's): wait for it.
    if let Some(running) = state.as_ref().filter(|s| s.finished_ms.is_none()) {
        match wait_for(&key, running, deadline) {
            Some(()) => {
                let mut done = running.clone();
                done.finished_ms = Some(now_ms());
                store(&key, &done);
                state = Some(done);
            }
            None => {
                let mut out = outcome(&layout, plans, &cached, Some(running), true);
                out.running = true;
                return Ok(out);
            }
        }
    }
    let satisfied = plans
        .iter()
        .all(|p| !p.needs_create(&layout) && !p.needs_analyze(&layout));
    // The last run was for this very plan and did not produce it: report
    // its failure rather than retry at once.
    let recent_failure = state.as_ref().is_some_and(|s| {
        s.plan == plan
            && s.finished_ms
                .is_some_and(|done| now_ms().saturating_sub(done) < RETRY_AFTER.as_millis() as u64)
    });
    if satisfied || recent_failure {
        let out = outcome(&layout, plans, &cached, state.as_ref(), false);
        return Ok(finalize(out, plans, root, tool, state.as_ref()));
    }

    let started = start(root, tool, &layout, plans, &plan)?;
    match wait_for(&key, &started, deadline) {
        Some(()) => {
            let mut done = started;
            done.finished_ms = Some(now_ms());
            store(&key, &done);
            let out = outcome(&layout, plans, &cached, Some(&done), false);
            Ok(finalize(out, plans, root, tool, Some(&done)))
        }
        None => {
            let mut out = outcome(&layout, plans, &cached, Some(&started), true);
            out.running = true;
            Ok(out)
        }
    }
}

/// Mark `stale` when a language's files changed during the run (its
/// results may be out of date; the next call's keys differ, so it runs
/// again).
fn finalize(
    mut out: RunOutcome,
    plans: &[LangPlan],
    root: &Path,
    tool: &CodeqlTool,
    state: Option<&RunState>,
) -> RunOutcome {
    if state.is_some() {
        out.stale = plans
            .iter()
            .any(|plan| db_key(root, tool, plan.lang, plan.build_mode, &plan.files) != plan.db_key);
    }
    out
}

fn load(path: &Path) -> Option<RunState> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn store(path: &Path, state: &RunState) {
    let _ = fs::write(path, serde_json::to_vec(state).unwrap_or_default());
}

/// Wait for the run of `state` until `deadline`: `Some` when it ended.
fn wait_for(key: &Path, state: &RunState, deadline: Instant) -> Option<()> {
    loop {
        let owned = {
            let mut children = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
            match children.get_mut(key) {
                Some(child) => match child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        children.remove(key);
                        return Some(());
                    }
                    Ok(None) => true,
                },
                None => false,
            }
        };
        if !owned && !is_process_alive(state.pid) {
            return Some(());
        }
        // The script's watchdog enforces the budget; a run past it by a
        // margin lost its watchdog, so its group is ended here.
        let over = now_ms().saturating_sub(state.started_ms) > state.budget_ms + 60_000;
        if over {
            kill_group(state.pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn kill_group(pid: u32) {
    if pid > 1 {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `'…'` for sh, with `'` spliced as `'\''`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn path_quote(path: &Path) -> String {
    sh_quote(&path.to_string_lossy())
}

/// Paths CodeQL skips: codegraph's own directory (the databases live
/// there) and the usual build output and dependency trees.
const CONFIG: &str = "# Written by `codegraph analyze codeql`.\n\
name: codegraph\n\
paths-ignore:\n  - .codegraph\n  - '**/node_modules'\n  - target\n  - '**/.git'\n";

/// The script for the plans' missing steps.
pub fn script(root: &Path, tool: &CodeqlTool, layout: &Layout, plans: &[LangPlan]) -> String {
    let budget_s = budget().as_secs().max(1);
    let threads = std::env::var("CODEGRAPH_CODEQL_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let ram = std::env::var("CODEGRAPH_CODEQL_RAM")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|mb| format!(" --ram={mb}"))
        .unwrap_or_default();
    let run = path_quote(&layout.run_dir());
    let codeql = path_quote(&tool.exe);
    let mut out = String::new();
    out.push_str("#!/bin/sh\n# Written by `codegraph analyze codeql`; rewritten on every run.\n");
    out.push_str(&format!("cd {} || exit 1\n", path_quote(root)));
    out.push_str(&format!("R={run}\n"));
    out.push_str(&format!(
        "( sleep {budget_s} & echo $! > \"$R/sleep.pid\"; wait $!; \
         echo 'budget of {budget_s}s spent (CODEGRAPH_CODEQL_BUDGET_MS)' > \"$R/timeout\"; \
         kill -TERM 0 ) &\nWATCHDOG=$!\n"
    ));
    out.push_str(
        "step() {\n  name=$1; shift\n  date +%s > \"$R/$name.start\"\n  \"$@\" > \"$R/$name.log\" \
         2>&1\n  rc=$?\n  date +%s > \"$R/$name.end\"\n  echo $rc > \"$R/$name.exit\"\n  return \
         $rc\n}\n",
    );
    for plan in plans {
        let id = plan.lang.id;
        let db = layout.db(id);
        let db_tmp = PathBuf::from(format!("{}.tmp", db.display()));
        let db_key = layout.db_key(id);
        let sarif = layout.sarif(id);
        let sarif_tmp = PathBuf::from(format!("{}.tmp", sarif.display()));
        if plan.needs_create(layout) {
            out.push_str(&format!(
                "# {id}: database (build mode {mode})\nrm -rf {tmp}\n\
                 if step {id}.create {codeql} database create {tmp} --language={id} \
                 --build-mode={mode} --source-root={src} --codescanning-config={cfg} \
                 --threads={threads}{ram} --overwrite; then\n  rm -rf {db} && mv {tmp} {db} && \
                 printf '%s' {key} > {keyfile}\nfi\n",
                mode = plan.build_mode.as_str(),
                tmp = path_quote(&db_tmp),
                src = path_quote(root),
                cfg = path_quote(&layout.config()),
                db = path_quote(&db),
                key = sh_quote(&plan.db_key),
                keyfile = path_quote(&db_key),
            ));
        }
        let suites: Vec<String> = plan.suites.iter().map(|s| sh_quote(s)).collect();
        out.push_str(&format!(
            "# {id}: analysis\nif [ \"$(cat {keyfile} 2>/dev/null)\" = {key} ]; then\n  \
             rm -f {tmp}\n  if step {id}.analyze {codeql} database analyze {db} {suites} \
             --format=sarif-latest --output={tmp} --threads={threads}{ram}; then\n    mv {tmp} \
             {sarif} && printf '%s' {akey} > {akeyfile}\n  fi\nfi\n",
            keyfile = path_quote(&db_key),
            key = sh_quote(&plan.db_key),
            tmp = path_quote(&sarif_tmp),
            db = path_quote(&db),
            suites = suites.join(" "),
            sarif = path_quote(&sarif),
            akey = sh_quote(&plan.analysis_key),
            akeyfile = path_quote(&layout.sarif_key(id)),
        ));
    }
    out.push_str(
        "kill $WATCHDOG 2>/dev/null\nkill \"$(cat \"$R/sleep.pid\")\" 2>/dev/null\nexit 0\n",
    );
    out
}

/// Write the script and start it detached.
fn start(
    root: &Path,
    tool: &CodeqlTool,
    layout: &Layout,
    plans: &[LangPlan],
    plan: &str,
) -> Result<RunState, String> {
    if !cfg!(unix) {
        return Err("the CodeQL adapter runs its pipeline with /bin/sh (Unix only)".to_string());
    }
    let run_dir = layout.run_dir();
    // The last run's step files would read as this run's.
    let _ = fs::remove_dir_all(&run_dir);
    for dir in [
        run_dir.clone(),
        layout.dir.join("db"),
        layout.dir.join("sarif"),
    ] {
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    fs::write(layout.config(), CONFIG).map_err(|e| e.to_string())?;
    let text = script(root, tool, layout, plans);
    fs::write(layout.script(), text).map_err(|e| e.to_string())?;
    let log = fs::File::create(run_dir.join("run.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new("sh");
    command
        .arg(layout.script())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("could not start the CodeQL run: {e}"))?;
    let state = RunState {
        pid: child.id(),
        started_ms: now_ms(),
        finished_ms: None,
        plan: plan.to_string(),
        budget_ms: budget().as_millis() as u64,
    };
    store(&layout.state(), &state);
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(layout.state(), child);
    Ok(state)
}

fn read_secs(path: &Path) -> Option<u64> {
    read_key(path)?.parse().ok()
}

fn tail(path: &Path, lines: usize) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.contains("Picked up _JAVA_OPTIONS"))
        .collect();
    let start = kept.len().saturating_sub(lines);
    Some(kept[start..].join("\n")).filter(|t| !t.is_empty())
}

fn dir_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(fs::Metadata::is_file)
        .map(|meta| meta.len())
        .sum()
}

/// Each plan's state as the files on disk say.
fn outcome(
    layout: &Layout,
    plans: &[LangPlan],
    cached: &[bool],
    state: Option<&RunState>,
    running: bool,
) -> RunOutcome {
    let timed_out = tail(&layout.timeout_marker(), 1);
    let languages = plans
        .iter()
        .zip(cached)
        .map(|(plan, &was_cached)| {
            let id = plan.lang.id;
            let current = !plan.needs_create(layout) && !plan.needs_analyze(layout);
            let secs = |step: &str| {
                let start = read_secs(&layout.step_file(id, step, "start"))?;
                let end = read_secs(&layout.step_file(id, step, "end"))?;
                Some(end.saturating_sub(start))
            };
            let (state, failure) = if current {
                (StepState::Complete, None)
            } else if running {
                (StepState::Running, None)
            } else {
                (
                    StepState::Failed,
                    Some(failure(layout, plan, timed_out.as_deref())),
                )
            };
            LangOutcome {
                language: id.to_string(),
                build_mode: plan.build_mode,
                state,
                cached: was_cached,
                suites: plan.suites.clone(),
                database: layout.db(id).to_string_lossy().into_owned(),
                database_bytes: dir_bytes(&layout.db(id)),
                create_secs: (!was_cached).then(|| secs("create")).flatten(),
                analyze_secs: (!was_cached).then(|| secs("analyze")).flatten(),
                failure,
                sarif: current.then(|| layout.sarif(id)),
            }
        })
        .collect();
    RunOutcome {
        running,
        started_ms: state.map(|s| s.started_ms),
        finished_ms: state.and_then(|s| s.finished_ms),
        stale: false,
        languages,
    }
}

/// Why a language has no current SARIF: the budget, the failing step's
/// exit code and log tail, or a step that never ran.
fn failure(layout: &Layout, plan: &LangPlan, timed_out: Option<&str>) -> String {
    let id = plan.lang.id;
    if let Some(why) = timed_out {
        return format!("stopped: {why}");
    }
    for step in ["create", "analyze"] {
        let Some(code) = read_key(&layout.step_file(id, step, "exit")) else {
            continue;
        };
        if code != "0" {
            let log = tail(&layout.step_file(id, step, "log"), 12).unwrap_or_default();
            return format!("`codeql database {step}` exited {code}:\n{log}");
        }
    }
    if plan.needs_create(layout) {
        "the database was not created (the run ended before it)".to_string()
    } else {
        "the analysis did not produce SARIF (the run ended before it)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::codeql::languages::by_id;

    fn tool() -> CodeqlTool {
        CodeqlTool {
            exe: PathBuf::from("/opt/code ql/codeql"),
            dist: PathBuf::from("/opt/code ql"),
            version: "2.27.1".into(),
        }
    }

    #[test]
    fn quoting_survives_quotes_and_spaces() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn keys_follow_the_sources_the_version_and_the_suites() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("A.java"), "class A {}").unwrap();
        let files = vec!["A.java".to_string()];
        let java = by_id("java").unwrap();
        let plan = |tool: &CodeqlTool, suites: &[&str]| {
            LangPlan::new(
                root,
                tool,
                java,
                BuildMode::None,
                &files,
                suites.iter().map(|s| s.to_string()).collect(),
            )
        };
        let a = plan(&tool(), &["s1"]);
        let b = plan(&tool(), &["s2"]);
        assert_eq!(a.db_key, b.db_key, "suites do not rebuild the database");
        assert_ne!(a.analysis_key, b.analysis_key);
        let mut newer = tool();
        newer.version = "2.28.0".into();
        assert_ne!(plan(&newer, &["s1"]).db_key, a.db_key);
        fs::write(root.join("A.java"), "class A { int x; }").unwrap();
        assert_ne!(plan(&tool(), &["s1"]).db_key, a.db_key, "an edit rebuilds");
    }

    #[test]
    fn the_script_builds_only_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let layout = Layout::new(root);
        let java = by_id("java").unwrap();
        let plan = LangPlan::new(
            root,
            &tool(),
            java,
            BuildMode::None,
            &[],
            vec!["codeql/java-queries:codeql-suites/java-security-and-quality.qls".into()],
        );
        let text = script(root, &tool(), &layout, std::slice::from_ref(&plan));
        assert!(text.contains("database create"), "{text}");
        assert!(text.contains("--build-mode=none"), "{text}");
        assert!(
            text.contains("'/opt/code ql/codeql' database analyze"),
            "{text}"
        );
        assert!(
            text.contains("kill -TERM 0"),
            "the watchdog ends the group: {text}"
        );

        // With a current database, only the analysis runs.
        fs::create_dir_all(layout.db("java")).unwrap();
        fs::write(layout.db_key("java"), &plan.db_key).unwrap();
        let text = script(root, &tool(), &layout, std::slice::from_ref(&plan));
        assert!(!text.contains("database create"), "{text}");
        assert!(text.contains("database analyze"), "{text}");
    }
}
