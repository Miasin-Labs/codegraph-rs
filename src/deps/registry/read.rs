//! Registry reads.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use super::{
    PackageRow,
    PendingShard,
    ProjectDependency,
    ProjectRow,
    Registry,
    source_from_columns,
};
use crate::deps::error::DepsResult;
use crate::deps::model::{DepKey, Ecosystem, ShardState};

/// Which projects' shards a build considers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingScope<'a> {
    /// Dependencies of the project at this canonical root.
    Project(&'a str),
    /// Every recorded project's dependencies.
    All,
}

/// How many versions are in one state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateCount {
    pub ecosystem: Ecosystem,
    pub state: ShardState,
    pub count: u64,
    pub shard_bytes: u64,
}

/// Registry totals for `codegraph deps status`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryStatus {
    pub projects: u64,
    pub versions: u64,
    pub usages: u64,
    pub shard_bytes: u64,
    pub by_state: Vec<StateCount>,
}

const PACKAGE_COLUMNS: &str =
    "p.id, p.ecosystem, p.name, p.version, p.source_kind, p.source_url, p.source_rev,
     p.state, p.shard_bytes, p.files, p.nodes, p.edges, p.build_ms, p.built_at, p.extractor_version,
     p.partial_reasons, p.error, p.built_from, p.first_seen, p.last_used,
     (SELECT COUNT(DISTINCT u.project_id) FROM usages u WHERE u.package_id = p.id)";

fn package_from_row(row: &Row<'_>) -> rusqlite::Result<PackageRow> {
    let ecosystem: String = row.get(1)?;
    let state: String = row.get(7)?;
    Ok(PackageRow {
        id: row.get(0)?,
        ecosystem: ecosystem.parse().unwrap_or(Ecosystem::Crates),
        name: row.get(2)?,
        version: row.get(3)?,
        source: source_from_columns(&row.get::<_, String>(4)?, row.get(5)?, row.get(6)?),
        state: ShardState::parse(&state).unwrap_or(ShardState::Missing),
        shard_bytes: row.get::<_, i64>(8)?.max(0) as u64,
        files: row.get::<_, Option<i64>>(9)?.map(|v| v.max(0) as u64),
        nodes: row.get::<_, Option<i64>>(10)?.map(|v| v.max(0) as u64),
        edges: row.get::<_, Option<i64>>(11)?.map(|v| v.max(0) as u64),
        build_ms: row.get::<_, Option<i64>>(12)?.map(|v| v.max(0) as u64),
        built_at: row.get(13)?,
        extractor_version: row.get(14)?,
        partial_reasons: row.get(15)?,
        error: row.get(16)?,
        built_from: row.get(17)?,
        first_seen: row.get(18)?,
        last_used: row.get(19)?,
        users: row.get::<_, i64>(20)?.max(0) as u64,
    })
}

impl Registry {
    /// Every dependency the project at `root` records, direct ones first.
    pub fn dependencies_of(&self, root: &str) -> DepsResult<Vec<ProjectDependency>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.ecosystem, p.name, p.version, p.source_kind, p.source_url, p.source_rev, p.state,
                    u.direct, u.lockfile, u.source_dir, u.path
             FROM usages u
             JOIN projects pr ON pr.id = u.project_id
             JOIN packages p ON p.id = u.package_id
             WHERE pr.root = ?1
             ORDER BY p.ecosystem, COALESCE(u.direct, 0) DESC, p.name, p.version",
        )?;
        let rows = stmt.query_map([root], |row| {
            let ecosystem: String = row.get(0)?;
            let kind: String = row.get(3)?;
            let state: String = row.get(6)?;
            let path: Option<String> = row.get(10)?;
            let mut source = source_from_columns(&kind, row.get(4)?, row.get(5)?);
            if let crate::deps::model::DepSource::Path { path: p } = &mut source {
                *p = path;
            }
            Ok(ProjectDependency {
                key: DepKey::new(
                    ecosystem.parse().unwrap_or(Ecosystem::Crates),
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ),
                source,
                state: ShardState::parse(&state).unwrap_or(ShardState::Missing),
                direct: row.get::<_, Option<i64>>(7)?.map(|d| d != 0),
                lockfile: row.get(8)?,
                source_dir: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Shards to (re)build: never built, interrupted, built by an older
    /// extractor or a schema this build cannot read, or failed under an older extractor — and whose
    /// source some project has. `every` lists every buildable version
    /// regardless of state (a forced rebuild). Direct dependencies first,
    /// then the most shared.
    pub fn pending(&self, scope: PendingScope<'_>, every: bool) -> DepsResult<Vec<PendingShard>> {
        let project_root = match scope {
            PendingScope::Project(root) => Some(root),
            PendingScope::All => None,
        };
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.ecosystem, p.name, p.version, p.source_kind, p.source_url, p.source_rev, p.state,
                    MAX(CASE WHEN pr.root = ?3 THEN COALESCE(u.direct, 0) ELSE 0 END) AS direct,
                    COUNT(DISTINCT u.project_id) AS users
             FROM packages p
             JOIN usages u ON u.package_id = p.id
             JOIN projects pr ON pr.id = u.project_id
             WHERE p.source_kind != 'path'
               AND u.source_dir IS NOT NULL
               AND (?4 OR p.state IN ('missing', 'building')
                    OR (p.state IN ('ready', 'partial')
                        AND (COALESCE(p.extractor_version, 0) < ?1 OR COALESCE(p.schema_version, 0) < ?5 OR COALESCE(p.schema_version, 0) > ?2))
                    OR (p.state IN ('failed', 'unavailable') AND p.error IS NOT NULL
                        AND COALESCE(p.extractor_version, 0) < ?1))
               AND (?3 IS NULL OR p.id IN (
                    SELECT u2.package_id FROM usages u2 JOIN projects pr2 ON pr2.id = u2.project_id
                    WHERE pr2.root = ?3))
             GROUP BY p.id
             ORDER BY direct DESC, users DESC, p.ecosystem, p.name, p.version",
        )?;
        let rows = stmt
            .query_map(
                params![
                    crate::extraction::EXTRACTION_VERSION,
                    crate::db::CURRENT_SCHEMA_VERSION,
                    project_root,
                    every,
                    crate::db::MIN_READABLE_SCHEMA_VERSION
                ],
                |row| {
                    let ecosystem: String = row.get(1)?;
                    let state: String = row.get(7)?;
                    Ok((
                        row.get::<_, i64>(0)?,
                        PendingShard {
                            key: DepKey::new(
                                ecosystem.parse().unwrap_or(Ecosystem::Crates),
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                            ),
                            source: source_from_columns(
                                &row.get::<_, String>(4)?,
                                row.get(5)?,
                                row.get(6)?,
                            ),
                            state: ShardState::parse(&state).unwrap_or(ShardState::Missing),
                            source_dirs: Vec::new(),
                            direct: row.get::<_, i64>(8)? != 0,
                            users: row.get::<_, i64>(9)?.max(0) as u64,
                        },
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut dirs = self.conn.prepare_cached(
            "SELECT DISTINCT u.source_dir FROM usages u JOIN projects pr ON pr.id = u.project_id
             WHERE u.package_id = ?1 AND u.source_dir IS NOT NULL
             ORDER BY (pr.root = ?2) DESC, u.source_dir",
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, mut pending) in rows {
            pending.source_dirs = dirs
                .query_map(params![id, project_root], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(PathBuf::from)
                .collect();
            out.push(pending);
        }
        Ok(out)
    }

    /// One version by key.
    pub fn package(&self, key: &DepKey) -> DepsResult<Option<PackageRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {PACKAGE_COLUMNS} FROM packages p WHERE p.ecosystem = ?1 AND p.name = ?2 AND p.version = ?3"
                ),
                params![key.ecosystem.as_str(), key.name, key.version],
                package_from_row,
            )
            .optional()?)
    }

    /// Every recorded version named `name` (optionally in one ecosystem).
    pub fn packages_named(
        &self,
        ecosystem: Option<Ecosystem>,
        name: &str,
    ) -> DepsResult<Vec<PackageRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PACKAGE_COLUMNS} FROM packages p
             WHERE p.name = ?1 AND (?2 IS NULL OR p.ecosystem = ?2)
             ORDER BY p.ecosystem, p.version"
        ))?;
        let rows = stmt.query_map(
            params![name, ecosystem.map(Ecosystem::as_str)],
            package_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every version whose state says a shard directory exists.
    pub fn shards(&self) -> DepsResult<Vec<PackageRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PACKAGE_COLUMNS} FROM packages p WHERE p.state IN ('ready', 'partial')
             ORDER BY p.last_used, p.ecosystem, p.name, p.version"
        ))?;
        let rows = stmt.query_map([], package_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Canonical roots of the projects that use `key`.
    pub fn users_of(&self, key: &DepKey) -> DepsResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT pr.root FROM usages u
             JOIN projects pr ON pr.id = u.project_id
             JOIN packages p ON p.id = u.package_id
             WHERE p.ecosystem = ?1 AND p.name = ?2 AND p.version = ?3
             ORDER BY pr.root",
        )?;
        let rows = stmt.query_map(
            params![key.ecosystem.as_str(), key.name, key.version],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// Every recorded project.
    pub fn projects(&self) -> DepsResult<Vec<ProjectRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT pr.root, pr.lockfiles, pr.recorded_at,
                    (SELECT COUNT(*) FROM usages u WHERE u.project_id = pr.id)
             FROM projects pr ORDER BY pr.root",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectRow {
                root: r.get(0)?,
                lockfiles: r.get::<_, i64>(1)?.max(0) as u64,
                recorded_at: r.get(2)?,
                dependencies: r.get::<_, i64>(3)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Totals by ecosystem and state.
    pub fn status(&self) -> DepsResult<RegistryStatus> {
        let count = |sql: &str| -> DepsResult<u64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))?.max(0) as u64)
        };
        let mut status = RegistryStatus {
            projects: count("SELECT COUNT(*) FROM projects")?,
            versions: count("SELECT COUNT(*) FROM packages")?,
            usages: count("SELECT COUNT(*) FROM usages")?,
            shard_bytes: count(
                "SELECT COALESCE(SUM(shard_bytes), 0) FROM packages WHERE state IN ('ready', 'partial')",
            )?,
            by_state: Vec::new(),
        };
        let mut stmt = self.conn.prepare(
            "SELECT ecosystem, state, COUNT(*), COALESCE(SUM(shard_bytes), 0) FROM packages
             GROUP BY ecosystem, state ORDER BY ecosystem, state",
        )?;
        let mut grouped: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (eco, state, n, bytes) = row?;
            grouped.insert((eco, state), (n.max(0) as u64, bytes.max(0) as u64));
        }
        for ((eco, state), (count, bytes)) in grouped {
            let (Ok(ecosystem), Some(state)) =
                (eco.parse::<Ecosystem>(), ShardState::parse(&state))
            else {
                continue;
            };
            status.by_state.push(StateCount {
                ecosystem,
                state,
                count,
                shard_bytes: bytes,
            });
        }
        Ok(status)
    }
}
