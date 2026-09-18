//! [`ActivityReader`]: a read-only, deadline-bounded view of the history
//! store, answering per atlas project — when agents last worked there,
//! how many sessions, the recent episodes, hot files and open failures.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;

use super::scope::HistoryScope;
use super::{DAY_MS, newest, open_history};
use crate::atlas::{Atlas, Project};
use crate::history::deadline::Deadline;
use crate::history::memory::{EpisodeRow, FailureRow, IndexProbe, Queries};
use crate::history::store::HistoryError;
use crate::history::time::now_ms;

/// Sessions are counted over this window.
pub(super) const SESSIONS_WINDOW_MS: i64 = 30 * DAY_MS;
/// Hot files are ranked over this window.
const HOT_WINDOW_MS: i64 = 30 * DAY_MS;
/// Failures (not fixed since) are looked for over this window — the
/// session digest's.
pub(super) const FAILURE_WINDOW_MS: i64 = 14 * DAY_MS;
const EPISODES_SHOWN: usize = 3;
const HOT_SHOWN: usize = 6;
const FAILURES_SHOWN: usize = 3;

/// When agents last worked in a project, and how often lately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySummary {
    /// Latest recorded agent call in the project, or touch of its files
    /// (epoch ms).
    pub last_activity_ms: Option<i64>,
    /// Agent sessions (sub-agents fold into theirs) that worked in the
    /// project in the last 30 days.
    #[serde(rename = "sessions30d")]
    pub sessions_30d: i64,
}

/// A file agents kept coming back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HotFile {
    /// Repo-relative path.
    pub path: String,
    /// Episodes (prompts) that read or edited it in the last 30 days.
    pub episodes: i64,
}

/// A project's recent agent activity (`codegraph projects show`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectActivity {
    #[serde(flatten)]
    pub summary: ActivitySummary,
    /// The latest episodes, newest first.
    pub episodes: Vec<EpisodeRow>,
    pub hot_files: Vec<HotFile>,
    /// Build/test failures of the last 14 days no later run fixed.
    pub open_failures: Vec<FailureRow>,
}

/// Read-only history, every query past its deadline interrupted.
pub struct ActivityReader {
    pub(super) conn: Connection,
    pub(super) deadline: Deadline,
    pub(super) now: i64,
}

impl ActivityReader {
    /// Open the history store at `db` read-only, creating nothing. `None`
    /// when there is no store yet, or it is older than this build reads.
    pub fn open(db: &Path, deadline: Duration) -> Result<Option<Self>, HistoryError> {
        let Some(conn) = open_history(db)? else {
            return Ok(None);
        };
        let deadline = Deadline::arm(&conn, deadline);
        Ok(Some(Self {
            conn,
            deadline,
            now: now_ms(),
        }))
    }

    /// The deadline passed: answers from here on would be partial.
    pub fn expired(&self) -> bool {
        self.deadline.expired()
    }

    /// Where `project`'s activity lives in the store.
    pub fn scope(
        &self,
        atlas: &Atlas,
        project: &Project,
    ) -> Result<Option<HistoryScope>, HistoryError> {
        HistoryScope::resolve(&self.conn, atlas, project)
    }

    /// Queries on one history repository.
    pub(super) fn queries<'a>(
        &'a self,
        repo: i64,
        since: i64,
        limit: usize,
        probe: Option<&'a IndexProbe>,
    ) -> Queries<'a> {
        Queries {
            conn: &self.conn,
            repo,
            probe,
            since,
            limit: limit as i64,
            now: self.now,
        }
    }

    /// Last activity and 30-day sessions of a scope.
    pub fn summary(&self, scope: &HistoryScope) -> Result<ActivitySummary, HistoryError> {
        let mut out = ActivitySummary::default();
        for repo in scope.ids() {
            let q = self.queries(repo, self.now - SESSIONS_WINDOW_MS, 1, None);
            out.last_activity_ms = out.last_activity_ms.max(q.last_activity(&scope.prefix)?);
            out.sessions_30d += q.sessions(&scope.prefix)?;
        }
        Ok(out)
    }

    /// The latest episodes of a scope (touching its prefix, when nested).
    pub(super) fn episodes(
        &self,
        scope: &HistoryScope,
        since: i64,
        limit: usize,
        probe: Option<&IndexProbe>,
    ) -> Result<Vec<EpisodeRow>, HistoryError> {
        let prefix = (!scope.prefix.is_empty()).then_some(scope.prefix.as_str());
        let mut rows = Vec::new();
        for repo in scope.ids() {
            rows.extend(self.queries(repo, since, limit, probe).episodes(prefix)?);
        }
        Ok(newest(rows, |e| e.started_ms, limit))
    }

    /// Failures of a scope's repositories since `since`, newest first
    /// (`open_only`: those no later run fixed).
    pub(super) fn failures(
        &self,
        scope: &HistoryScope,
        since: i64,
        open_only: bool,
        limit: usize,
    ) -> Result<Vec<FailureRow>, HistoryError> {
        let mut rows = Vec::new();
        for repo in scope.ids() {
            let q = self.queries(repo, since, limit, None);
            rows.extend(q.failures(open_only)?);
        }
        Ok(newest(rows, |f| f.ts_ms, limit))
    }

    /// Recent episodes, hot files and open failures of a scope. Files are
    /// marked unchanged-since against the main checkout's index when the
    /// scope is a whole checkout.
    pub fn activity(&self, scope: &HistoryScope) -> Result<ProjectActivity, HistoryError> {
        let summary = self.summary(scope)?;
        let probe = match (scope.prefix.is_empty(), scope.roots().next()) {
            (true, Some(root)) => IndexProbe::open(root),
            _ => None,
        };
        let episodes = self.episodes(scope, 0, EPISODES_SHOWN, probe.as_ref())?;
        let mut hot: Vec<HotFile> = Vec::new();
        for repo in scope.ids() {
            let q = self.queries(repo, self.now - HOT_WINDOW_MS, HOT_SHOWN, None);
            for (path, n) in q.hot_files(&scope.prefix, HOT_SHOWN as i64)? {
                match hot.iter_mut().find(|h| h.path == path) {
                    Some(h) => h.episodes += n,
                    None => hot.push(HotFile { path, episodes: n }),
                }
            }
        }
        hot.sort_by(|a, b| b.episodes.cmp(&a.episodes).then(a.path.cmp(&b.path)));
        hot.truncate(HOT_SHOWN);
        let open_failures =
            self.failures(scope, self.now - FAILURE_WINDOW_MS, true, FAILURES_SHOWN)?;
        Ok(ProjectActivity {
            summary,
            episodes,
            hot_files: hot,
            open_failures,
        })
    }
}
