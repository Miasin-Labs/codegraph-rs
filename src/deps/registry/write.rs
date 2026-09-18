//! Registry writes: recording a project's dependencies, and shard state
//! transitions from the builder and `gc`.

use rusqlite::{OptionalExtension, params};

use super::{LocatedDep, RecordSummary, Registry, source_columns};
use crate::deps::error::DepsResult;
use crate::deps::model::{DepKey, DepSource, ShardState};
use crate::deps::shard::ShardMeta;

impl Registry {
    /// Replace everything recorded about the project at `root` (its
    /// canonical checkout root) with `deps`, in one transaction. Every
    /// version it uses is marked used now (`last_used`, for `gc`).
    pub fn record_project(
        &mut self,
        root: &str,
        fingerprint: &str,
        lockfiles: usize,
        deps: &[LocatedDep],
        now_ms: i64,
    ) -> DepsResult<RecordSummary> {
        let tx = self.conn.transaction()?;
        let project_id: i64 = tx.query_row(
            "INSERT INTO projects (root, lock_fingerprint, lockfiles, recorded_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root) DO UPDATE SET lock_fingerprint = excluded.lock_fingerprint,
                 lockfiles = excluded.lockfiles, recorded_at = excluded.recorded_at
             RETURNING id",
            params![root, fingerprint, lockfiles as i64, now_ms],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM usages WHERE project_id = ?1", [project_id])?;

        let mut summary = RecordSummary::default();
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO packages (ecosystem, name, version, source_kind, source_url, source_rev,
                                       state, first_seen, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'missing', ?7, ?7)
                 ON CONFLICT(ecosystem, name, version) DO UPDATE SET last_used = excluded.last_used
                 RETURNING id",
            )?;
            let mut usage = tx.prepare_cached(
                "INSERT INTO usages (project_id, package_id, lockfile, direct, source_dir, path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, package_id, lockfile) DO UPDATE SET
                     direct = COALESCE(excluded.direct, usages.direct),
                     source_dir = COALESCE(excluded.source_dir, usages.source_dir)",
            )?;
            for located in deps {
                let dep = &located.dep;
                let (kind, url, rev) = source_columns(&dep.source);
                let package_id: i64 = upsert.query_row(
                    params![
                        dep.key.ecosystem.as_str(),
                        dep.key.name,
                        dep.key.version,
                        kind,
                        url,
                        rev,
                        now_ms
                    ],
                    |r| r.get(0),
                )?;
                let path = match &dep.source {
                    DepSource::Path { path } => path.as_deref(),
                    _ => None,
                };
                let source_dir = located
                    .source_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                usage.execute(params![
                    project_id,
                    package_id,
                    dep.lockfile,
                    dep.direct.map(i64::from),
                    source_dir,
                    path
                ])?;
                summary.dependencies += 1;
                match (&dep.source, &located.source_dir) {
                    (DepSource::Path { .. }, _) => summary.path += 1,
                    (_, Some(_)) => summary.located += 1,
                    (_, None) => summary.unavailable += 1,
                }
            }
        }
        // A version with no located source anywhere (or a local path) has
        // nothing to build; one some project has the source for does.
        tx.execute(
            "UPDATE packages SET state = CASE
                 WHEN source_kind != 'path' AND EXISTS (
                     SELECT 1 FROM usages u WHERE u.package_id = packages.id AND u.source_dir IS NOT NULL)
                 THEN 'missing' ELSE 'unavailable' END
             WHERE state IN ('missing', 'unavailable') AND error IS NULL
               AND id IN (SELECT package_id FROM usages WHERE project_id = ?1)",
            [project_id],
        )?;
        tx.commit()?;
        Ok(summary)
    }

    /// The lockfile fingerprint recorded for `root`.
    pub fn project_fingerprint(&self, root: &str) -> DepsResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT lock_fingerprint FROM projects WHERE root = ?1",
                [root],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Forget the project at `root` (and its usage rows).
    pub fn delete_project(&self, root: &str) -> DepsResult<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM projects WHERE root = ?1", [root])?
            > 0)
    }

    pub fn mark_building(&self, key: &DepKey) -> DepsResult<()> {
        self.set_state(key, ShardState::Building, None)
    }

    /// A build failed: remember why, and with which extractor (a newer one
    /// retries it).
    pub fn mark_failed(&self, key: &DepKey, error: &str) -> DepsResult<()> {
        self.conn.execute(
            "UPDATE packages SET state = 'failed', error = ?4, extractor_version = ?5
             WHERE ecosystem = ?1 AND name = ?2 AND version = ?3",
            params![
                key.ecosystem.as_str(),
                key.name,
                key.version,
                error,
                crate::extraction::EXTRACTION_VERSION
            ],
        )?;
        Ok(())
    }

    /// The source has nothing to index: `unavailable`, with the reason and
    /// the extractor that decided it (a newer one looks again).
    pub fn mark_no_sources(&self, key: &DepKey) -> DepsResult<()> {
        self.conn.execute(
            "UPDATE packages SET state = 'unavailable', error = 'no library sources', extractor_version = ?4
             WHERE ecosystem = ?1 AND name = ?2 AND version = ?3",
            params![
                key.ecosystem.as_str(),
                key.name,
                key.version,
                crate::extraction::EXTRACTION_VERSION
            ],
        )?;
        Ok(())
    }

    /// Back to `missing` (shard removed, or a build that never started);
    /// `unavailable` when no project has the source.
    pub fn mark_missing(&self, key: &DepKey) -> DepsResult<()> {
        self.conn.execute(
            "UPDATE packages SET shard_bytes = 0, files = NULL, nodes = NULL, edges = NULL,
                 build_ms = NULL, built_at = NULL, partial_reasons = NULL, error = NULL,
                 state = CASE WHEN source_kind != 'path' AND EXISTS (
                     SELECT 1 FROM usages u WHERE u.package_id = packages.id AND u.source_dir IS NOT NULL)
                 THEN 'missing' ELSE 'unavailable' END
             WHERE ecosystem = ?1 AND name = ?2 AND version = ?3",
            params![key.ecosystem.as_str(), key.name, key.version],
        )?;
        Ok(())
    }

    /// A shard was published (or confirmed) as `meta` describes.
    pub fn mark_built(&self, meta: &ShardMeta) -> DepsResult<()> {
        let key = meta.key();
        let reasons = (!meta.partial_reasons.is_empty())
            .then(|| serde_json::to_string(&meta.partial_reasons))
            .transpose()?;
        self.conn.execute(
            "UPDATE packages SET state = ?4, shard_bytes = ?5, files = ?6, nodes = ?7, edges = ?8,
                 build_ms = ?9, built_at = ?10, extractor_version = ?11, schema_version = ?12,
                 partial_reasons = ?13, error = NULL, built_from = ?14
             WHERE ecosystem = ?1 AND name = ?2 AND version = ?3",
            params![
                key.ecosystem.as_str(),
                key.name,
                key.version,
                meta.state.as_str(),
                meta.db_bytes as i64,
                meta.counts.indexed_files as i64,
                meta.counts.nodes as i64,
                meta.counts.edges as i64,
                meta.build_ms as i64,
                meta.built_at_ms,
                meta.extractor_version,
                meta.schema_version,
                reasons,
                meta.source_dir,
            ],
        )?;
        Ok(())
    }

    fn set_state(&self, key: &DepKey, state: ShardState, error: Option<&str>) -> DepsResult<()> {
        self.conn.execute(
            "UPDATE packages SET state = ?4, error = ?5 WHERE ecosystem = ?1 AND name = ?2 AND version = ?3",
            params![key.ecosystem.as_str(), key.name, key.version, state.as_str(), error],
        )?;
        Ok(())
    }

    /// Delete versions no project uses and that have no shard.
    pub fn delete_unused_packages(&self) -> DepsResult<usize> {
        Ok(self.conn.execute(
            "DELETE FROM packages WHERE state NOT IN ('ready', 'partial')
               AND NOT EXISTS (SELECT 1 FROM usages u WHERE u.package_id = packages.id)",
            [],
        )?)
    }
}
