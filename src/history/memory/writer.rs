//! Folds [`SourceEvent`]s into the memory tables, inside the caller's
//! transaction.
//!
//! Idempotent: a call is folded in only the first time its `call_key` is
//! seen (`tool_events.remembered`), prompts are keyed on their native id,
//! outcomes on the call key, and checkpoints are plain upserts — so reading
//! an input unit twice changes nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lookups::{Candidate, PendingLookup};
use rusqlite::{Connection, OptionalExtension, params};

use super::index_probe::IndexProbe;
use crate::history::activity::{Activity, Op, Outcome, OutcomeKind};
use crate::history::event::{RawToolCall, ToolEvent, call_key};
use crate::history::project::ProjectResolver;
use crate::history::redact::redact;
use crate::history::repo::{RepoLocator, absolutize};
use crate::history::sources::{RawPrompt, RawSession, SourceEvent, key_hash};
use crate::history::store::HistoryError;

/// Prompts this close after another of the same session are one prompt.
const PROMPT_BURST_MS: i64 = 2_000;
/// Most other edited files an edit is paired with (co-edits).
const MAX_COEDIT_PARTNERS: i64 = 40;

mod lookups;

/// What a writer did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteStats {
    /// Calls folded into the memory for the first time.
    pub remembered: usize,
    /// Calls already stored by an earlier pass.
    pub already_present: usize,
    /// Calls with at least one masked value.
    pub redacted: usize,
    pub sessions: usize,
    pub prompts: usize,
    pub checkpoints: usize,
}

#[derive(Debug, Clone)]
struct SessionRef {
    id: i64,
    /// The root session (itself for a root): episodes belong to it.
    root: i64,
    repo: Option<i64>,
    cwd: Option<PathBuf>,
}

/// Writes one source's events.
pub(crate) struct MemoryWriter<'c> {
    conn: &'c Connection,
    source: String,
    project: Option<String>,
    projects: ProjectResolver,
    locator: RepoLocator,
    repos: HashMap<PathBuf, i64>,
    repo_roots: HashMap<i64, PathBuf>,
    probes: HashMap<i64, Option<IndexProbe>>,
    sessions: HashMap<String, SessionRef>,
    files: HashMap<(i64, String), i64>,
    idents: HashMap<(i64, String), i64>,
    pending: HashMap<i64, Vec<PendingLookup>>,
    /// Latest prompt time per root session (burst detection).
    last_prompt: HashMap<i64, i64>,
    touched_repos: HashSet<i64>,
    pub(crate) stats: WriteStats,
}

impl<'c> MemoryWriter<'c> {
    pub(crate) fn new(conn: &'c Connection, source: &str, project: Option<String>) -> Self {
        Self {
            conn,
            source: source.to_owned(),
            project,
            projects: ProjectResolver::new(),
            locator: RepoLocator::new(),
            repos: HashMap::new(),
            repo_roots: HashMap::new(),
            probes: HashMap::new(),
            sessions: HashMap::new(),
            files: HashMap::new(),
            idents: HashMap::new(),
            pending: HashMap::new(),
            last_prompt: HashMap::new(),
            touched_repos: HashSet::new(),
            stats: WriteStats::default(),
        }
    }

    /// Repositories this writer added memory to (for the rollups).
    pub(crate) fn touched_repos(&self) -> &HashSet<i64> {
        &self.touched_repos
    }

    pub(crate) fn write(&mut self, event: SourceEvent) -> Result<(), HistoryError> {
        match event {
            SourceEvent::Session(s) => {
                self.session(&s)?;
            }
            SourceEvent::Prompt(p) => self.prompt(&p)?,
            SourceEvent::Call(c) => self.call(*c)?,
            SourceEvent::Checkpoint { key, value } => {
                self.conn.execute(
                    "INSERT OR REPLACE INTO ingest_state (source, key, value) VALUES (?1, ?2, ?3)",
                    params![self.source, key, value],
                )?;
                self.stats.checkpoints += 1;
            }
        }
        Ok(())
    }

    // ── sessions & episodes ─────────────────────────────────────────────

    fn session(&mut self, s: &RawSession) -> Result<SessionRef, HistoryError> {
        let native_key = call_key(&self.source, &s.native_id);
        let parent = match &s.parent {
            Some(p) => Some(self.session_stub(p)?),
            None => None,
        };
        let cwd = s
            .cwd
            .as_deref()
            .map(|c| redact(c).0)
            .and_then(|c| absolutize(&c, None));
        let repo = match &cwd {
            Some(dir) => self.repo_of_dir(dir)?,
            None => None,
        };
        let (id, root_id, repo_id): (i64, Option<i64>, Option<i64>) = self.conn.query_row(
            "INSERT INTO sessions (source, native_key, parent_id, root_id, repo_id, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(native_key) DO UPDATE SET
                 parent_id  = COALESCE(sessions.parent_id, excluded.parent_id),
                 root_id    = COALESCE(sessions.root_id, excluded.root_id),
                 repo_id    = COALESCE(sessions.repo_id, excluded.repo_id),
                 started_at = COALESCE(MIN(sessions.started_at, excluded.started_at),
                                       sessions.started_at, excluded.started_at)
             RETURNING id, root_id, repo_id",
            params![
                self.source,
                native_key,
                parent.as_ref().map(|p| p.id),
                parent.as_ref().map(|p| p.root),
                repo,
                s.started_ms,
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let session = SessionRef {
            id,
            root: root_id.unwrap_or(id),
            repo: repo_id,
            cwd,
        };
        self.stats.sessions += 1;
        self.sessions.insert(s.native_id.clone(), session.clone());
        Ok(session)
    }

    /// A session only known by id (a parent not read yet, or a call whose
    /// session was never announced).
    fn session_stub(&mut self, native_id: &str) -> Result<SessionRef, HistoryError> {
        if let Some(s) = self.sessions.get(native_id) {
            return Ok(s.clone());
        }
        let native_key = call_key(&self.source, native_id);
        let found: Option<(i64, Option<i64>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT id, root_id, repo_id FROM sessions WHERE native_key = ?1",
                params![native_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (id, root, repo) = match found {
            Some((id, root, repo)) => (id, root.unwrap_or(id), repo),
            None => {
                self.conn.execute(
                    "INSERT INTO sessions (source, native_key) VALUES (?1, ?2)",
                    params![self.source, native_key],
                )?;
                let id = self.conn.last_insert_rowid();
                (id, id, None)
            }
        };
        let s = SessionRef {
            id,
            root,
            repo,
            cwd: None,
        };
        self.sessions.insert(native_id.to_owned(), s.clone());
        Ok(s)
    }

    fn prompt(&mut self, p: &RawPrompt) -> Result<(), HistoryError> {
        let session = self.session_stub(&p.session)?;
        let Some(ts) = p.ts_ms else {
            return Ok(());
        };
        let prompt_key = key_hash(&format!("{}\0{}", self.source, p.native_id));
        // A burst of submits (a harness re-queueing, a stuck key) is one
        // prompt: skip any within PROMPT_BURST_MS after another. Depends on
        // timestamps only, so a re-ingest decides the same way.
        let previous = self.last_prompt.insert(session.root, ts);
        if previous.is_some_and(|t| (0..PROMPT_BURST_MS).contains(&(ts - t))) {
            return Ok(());
        }
        let burst: bool = self
            .conn
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM episodes WHERE session_id = ?1
                     AND started_at BETWEEN ?2 AND ?3 AND prompt_key <> ?4)",
            )?
            .query_row(
                params![session.root, ts - PROMPT_BURST_MS, ts, prompt_key],
                |r| r.get(0),
            )?;
        if burst {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO episodes (session_id, prompt_key, repo_id, started_at)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(prompt_key) DO NOTHING",
            params![session.root, prompt_key, session.repo, ts],
        )?;
        self.stats.prompts += 1;
        Ok(())
    }

    /// The episode of `root` a call at `ts` belongs to: the latest one
    /// started by then, else the session's preamble (calls before its
    /// first prompt).
    fn episode_for(
        &mut self,
        root: i64,
        repo: Option<i64>,
        ts: Option<i64>,
    ) -> Result<i64, HistoryError> {
        let found: Option<i64> = match ts {
            Some(ts) => self
                .conn
                .prepare_cached(
                    "SELECT id FROM episodes WHERE session_id = ?1 AND started_at <= ?2
                     ORDER BY started_at DESC LIMIT 1",
                )?
                .query_row(params![root, ts], |r| r.get(0))
                .optional()?,
            None => self
                .conn
                .prepare_cached(
                    "SELECT id FROM episodes WHERE session_id = ?1 ORDER BY started_at DESC LIMIT 1",
                )?
                .query_row(params![root], |r| r.get(0))
                .optional()?,
        };
        if let Some(id) = found {
            return Ok(id);
        }
        let key = key_hash(&format!("{}\0preamble\0{root}", self.source));
        Ok(self.conn.query_row(
            "INSERT INTO episodes (session_id, prompt_key, repo_id, started_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(prompt_key) DO UPDATE SET started_at = MIN(episodes.started_at, excluded.started_at)
             RETURNING id",
            params![root, key, repo, ts.unwrap_or(0)],
            |r| r.get(0),
        )?)
    }

    // ── calls ───────────────────────────────────────────────────────────

    fn call(&mut self, mut raw: RawToolCall) -> Result<(), HistoryError> {
        if self.project.is_some() {
            raw.project.clone_from(&self.project);
        }
        let ev = ToolEvent::from_raw(&self.source, &raw, &mut self.projects);
        self.stats.redacted += usize::from(ev.redacted);
        self.conn
            .prepare_cached(
                "INSERT OR IGNORE INTO tool_events
                 (call_key, source, ts, session, project, tool_kind, primary_cmd, chain, path, redacted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute(params![
                ev.call_key,
                ev.source,
                ev.ts,
                ev.session,
                ev.project,
                ev.tool_kind,
                ev.primary_cmd,
                ev.chain,
                ev.path,
                i64::from(ev.redacted),
            ])?;
        let first_time = self
            .conn
            .prepare_cached(
                "UPDATE tool_events SET remembered = 1 WHERE call_key = ?1 AND remembered = 0",
            )?
            .execute(params![ev.call_key])?;
        if first_time == 0 {
            self.stats.already_present += 1;
            return Ok(());
        }
        self.stats.remembered += 1;

        let session = match raw.session.as_deref() {
            Some(native) => match self.sessions.get(native) {
                Some(s) => s.clone(),
                None => self.session(&RawSession {
                    native_id: native.to_owned(),
                    cwd: raw.cwd.clone(),
                    ..Default::default()
                })?,
            },
            None => self.session(&RawSession {
                native_id: format!("call:{}", raw.native_id),
                cwd: raw.cwd.clone(),
                ..Default::default()
            })?,
        };
        let ts = raw.when_ms();
        let cwd = raw
            .cwd
            .as_deref()
            .map(|c| redact(c).0)
            .and_then(|c| absolutize(&c, None))
            .or_else(|| session.cwd.clone());
        let call_repo = match &cwd {
            Some(dir) => self.repo_of_dir(dir)?,
            None => None,
        }
        .or(session.repo);
        let episode = self.episode_for(session.root, session.repo.or(call_repo), ts)?;
        self.conn
            .prepare_cached(
                "UPDATE episodes SET calls = calls + 1, ended_at = MAX(COALESCE(ended_at, 0), ?2)
                 WHERE id = ?1",
            )?
            .execute(params![episode, ts.unwrap_or(0)])?;
        self.conn
            .prepare_cached(
                "UPDATE sessions SET calls = calls + 1, ended_at = MAX(COALESCE(ended_at, 0), ?2)
                 WHERE id = ?1",
            )?
            .execute(params![session.id, ts.unwrap_or(0)])?;

        let activity = Activity::of(&raw);
        let bytes = raw.result.as_ref().map_or(0, |r| r.output_bytes);
        let candidates = self.touches(episode, &activity, ts, bytes)?;
        if let Some(repo) = call_repo {
            self.lookups(episode, repo, &activity.idents, ts)?;
        }
        self.resolve_lookups(episode, &candidates)?;
        if let Some(outcome) = &activity.outcome {
            self.outcome(&ev.call_key, episode, call_repo, outcome, ts)?;
        }
        if let Some(repo) = call_repo {
            self.touched_repos.insert(repo);
        }
        Ok(())
    }

    /// Record the call's file touches; they are the candidates for where
    /// a pending lookup was found.
    fn touches(
        &mut self,
        episode: i64,
        activity: &Activity,
        ts: Option<i64>,
        bytes: u64,
    ) -> Result<Vec<Candidate>, HistoryError> {
        let mut candidates = Vec::new();
        let single = activity.touches.len() == 1;
        for touch in &activity.touches {
            let Some((root, rel)) = self.locator.locate(&touch.path) else {
                continue;
            };
            let rel = redact(&rel).0;
            let repo = self.repo_id(&root)?;
            let file = self.file_id(repo, &rel)?;
            let fingerprint = self.probe(repo).and_then(|p| p.fingerprint(&rel, ts));
            let touch_bytes = if single && touch.op == Op::Read {
                bytes
            } else {
                0
            };
            let n: i64 = self
                .conn
                .prepare_cached(
                    "INSERT INTO touches (episode_id, file_id, op, n, bytes, first_ts, last_ts, fingerprint)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5, ?5, ?6)
                     ON CONFLICT(episode_id, file_id, op) DO UPDATE SET
                         n = n + 1,
                         bytes = bytes + excluded.bytes,
                         first_ts = COALESCE(MIN(first_ts, excluded.first_ts), first_ts, excluded.first_ts),
                         last_ts = COALESCE(MAX(last_ts, excluded.last_ts), last_ts, excluded.last_ts),
                         fingerprint = CASE WHEN COALESCE(excluded.last_ts, 0) >= COALESCE(last_ts, 0)
                                            THEN excluded.fingerprint ELSE fingerprint END
                     RETURNING n",
                )?
                .query_row(
                    params![episode, file, touch.op.as_str(), touch_bytes as i64, ts, fingerprint],
                    |r| r.get(0),
                )?;
            self.conn
                .prepare_cached(
                    "UPDATE episodes SET repo_id = ?2 WHERE id = ?1 AND repo_id IS NULL",
                )?
                .execute(params![episode, repo])?;
            self.touched_repos.insert(repo);
            if touch.op == Op::Edit && n == 1 {
                self.coedit(episode, repo, file, ts)?;
            }
            candidates.push(Candidate {
                repo,
                file,
                rel,
                line: touch.line,
                op: touch.op,
            });
        }
        // Reads and edits before searches.
        candidates.sort_by_key(|c| c.op == Op::Search);
        Ok(candidates)
    }

    /// Pair a file newly edited in `episode` with the others edited there.
    fn coedit(
        &mut self,
        episode: i64,
        repo: i64,
        file: i64,
        ts: Option<i64>,
    ) -> Result<(), HistoryError> {
        let partners: Vec<i64> = self
            .conn
            .prepare_cached(
                "SELECT t.file_id FROM touches t JOIN files f ON f.id = t.file_id
                 WHERE t.episode_id = ?1 AND t.op = 'e' AND t.file_id <> ?2 AND f.repo_id = ?3
                 LIMIT ?4",
            )?
            .query_map(params![episode, file, repo, MAX_COEDIT_PARTNERS], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<_>>()?;
        for other in partners {
            let (a, b) = if file < other {
                (file, other)
            } else {
                (other, file)
            };
            self.conn
                .prepare_cached(
                    "INSERT INTO coedits (repo_id, file_a, file_b, episodes, last_ts) VALUES (?1, ?2, ?3, 1, ?4)
                     ON CONFLICT(repo_id, file_a, file_b) DO UPDATE SET
                         episodes = episodes + 1,
                         last_ts = COALESCE(MAX(last_ts, excluded.last_ts), last_ts, excluded.last_ts)",
                )?
                .execute(params![repo, a, b, ts])?;
        }
        Ok(())
    }

    fn outcome(
        &mut self,
        call_key: &str,
        episode: i64,
        repo: Option<i64>,
        outcome: &Outcome,
        ts: Option<i64>,
    ) -> Result<(), HistoryError> {
        let template = redact(&outcome.template).0;
        let codes = (!outcome.codes.is_empty()).then(|| outcome.codes.join(","));
        self.conn
            .prepare_cached(
                "INSERT OR IGNORE INTO outcomes
                 (call_key, episode_id, repo_id, kind, template, ok, err_codes, err_sig, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute(params![
                call_key,
                episode,
                repo,
                outcome.kind.as_str(),
                template,
                outcome.ok.map(i64::from),
                codes,
                outcome.signature,
                ts,
            ])?;
        let label = episode_label(outcome.kind, outcome.ok);
        self.conn
            .prepare_cached(
                "UPDATE episodes SET outcome = ?2, outcome_ts = ?3
                 WHERE id = ?1 AND COALESCE(outcome_ts, 0) <= ?3",
            )?
            .execute(params![episode, label, ts.unwrap_or(0)])?;
        Ok(())
    }

    // ── ids & probes ────────────────────────────────────────────────────

    fn repo_of_dir(&mut self, dir: &Path) -> Result<Option<i64>, HistoryError> {
        match self.locator.repo_of_dir(dir) {
            Some(root) => Ok(Some(self.repo_id(&root)?)),
            None => Ok(None),
        }
    }

    fn repo_id(&mut self, root: &Path) -> Result<i64, HistoryError> {
        if let Some(id) = self.repos.get(root) {
            return Ok(*id);
        }
        let text = redact(&root.to_string_lossy()).0;
        let id: i64 = self.conn.query_row(
            "INSERT INTO repos (root) VALUES (?1)
             ON CONFLICT(root) DO UPDATE SET root = excluded.root RETURNING id",
            params![text],
            |r| r.get(0),
        )?;
        self.repos.insert(root.to_path_buf(), id);
        self.repo_roots.insert(id, root.to_path_buf());
        Ok(id)
    }

    fn file_id(&mut self, repo: i64, rel: &str) -> Result<i64, HistoryError> {
        let key = (repo, rel.to_owned());
        if let Some(id) = self.files.get(&key) {
            return Ok(*id);
        }
        let id = upsert_file(self.conn, repo, rel)?;
        self.files.insert(key, id);
        Ok(id)
    }

    fn ident_id(&mut self, repo: i64, name: &str) -> Result<i64, HistoryError> {
        let key = (repo, name.to_owned());
        if let Some(id) = self.idents.get(&key) {
            return Ok(*id);
        }
        let id: i64 = self.conn.query_row(
            "INSERT INTO idents (repo_id, name) VALUES (?1, ?2)
             ON CONFLICT(repo_id, name) DO UPDATE SET name = excluded.name RETURNING id",
            params![repo, name],
            |r| r.get(0),
        )?;
        self.idents.insert(key, id);
        Ok(id)
    }

    fn probe(&mut self, repo: i64) -> Option<&IndexProbe> {
        if !self.probes.contains_key(&repo) {
            let probe = self
                .repo_roots
                .get(&repo)
                .and_then(|root| IndexProbe::open(root));
            self.probes.insert(repo, probe);
        }
        self.probes.get(&repo).and_then(Option::as_ref)
    }
}

/// Insert (or find) a file row.
pub(crate) fn upsert_file(conn: &Connection, repo: i64, rel: &str) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO files (repo_id, path) VALUES (?1, ?2)
         ON CONFLICT(repo_id, path) DO UPDATE SET path = excluded.path RETURNING id",
    )?
    .query_row(params![repo, rel], |r| r.get(0))
}

/// How an identifier the index doesn't know is stored: `#` + 16 hex.
pub(crate) fn hashed_ident(ident: &str) -> String {
    format!("#{}", &key_hash(&format!("ident\0{ident}"))[..16])
}

/// The episode outcome an outcome row implies.
fn episode_label(kind: OutcomeKind, ok: Option<bool>) -> &'static str {
    match (kind, ok) {
        (OutcomeKind::Commit, _) => "commit",
        (OutcomeKind::Test, Some(true)) => "tests-pass",
        (OutcomeKind::Test, Some(false)) => "tests-fail",
        (OutcomeKind::Test, None) => "tests-run",
        (OutcomeKind::Build, Some(true)) => "build-ok",
        (OutcomeKind::Build, Some(false)) => "build-fail",
        (OutcomeKind::Build, None) => "build-run",
    }
}
