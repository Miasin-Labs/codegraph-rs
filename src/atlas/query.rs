//! The atlas read API — what `codegraph projects` and later phases
//! (cross-shard resolution, cross-project queries, the history join) use.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, Row, params};

use super::kinds::{LinkKind, ProjectStatus};
use super::model::{Evidence, LanguageCount, Project, ProjectLink};
use super::remote::normalize_remote;
use super::store::{Atlas, AtlasError};

pub(super) const PROJECT_COLUMNS: &str = "id, root, name, checkout_root, repo_root, is_worktree, \
     remote, branch, head_commit, languages, file_count, node_count, edge_count, counts_exact, \
     index_bytes, index_schema, extraction_version, engine_version, last_indexed, last_seen, \
     registered_at, status";

const LINK_COLUMNS: &str =
    "id, from_project, to_project, to_path, kind, evidence_path, evidence_line, detail";

impl Atlas {
    /// Every registered project, by name then root.
    pub fn projects(&self) -> Result<Vec<Project>, AtlasError> {
        self.query_projects(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects ORDER BY name, root"),
            [],
        )
    }

    /// The project with this id.
    pub fn project(&self, id: i64) -> Result<Option<Project>, AtlasError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
                [id],
                project_from_row,
            )
            .optional()?)
    }

    /// The project registered at exactly `root` (canonicalized first).
    pub fn project_by_root(&self, root: &Path) -> Result<Option<Project>, AtlasError> {
        let root = super::canonical_root(root);
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE root = ?1"),
                [path_text(&root)],
                project_from_row,
            )
            .optional()?)
    }

    /// The nearest registered project whose root is `path` or one of its
    /// ancestors.
    pub fn project_for_path(&self, path: &Path) -> Result<Option<Project>, AtlasError> {
        let path = super::canonical_root(path);
        for dir in path.ancestors() {
            if let Some(project) = self.project_by_exact_root(dir)? {
                return Ok(Some(project));
            }
        }
        Ok(None)
    }

    /// Projects named `name` (the display name, or a worktree's bare
    /// directory name).
    pub fn projects_named(&self, name: &str) -> Result<Vec<Project>, AtlasError> {
        self.query_projects(
            &format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE name = ?1 OR name LIKE ?2 ESCAPE '\\' \
                 ORDER BY root"
            ),
            params![name, format!("%@{}", escape_like(name))],
        )
    }

    /// Projects whose remote normalizes like `url` (clones and worktrees of
    /// one repository).
    pub fn projects_with_remote(&self, url: &str) -> Result<Vec<Project>, AtlasError> {
        let Some(remote) = normalize_remote(url) else {
            return Ok(Vec::new());
        };
        self.query_projects(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE remote = ?1 ORDER BY root"),
            [remote],
        )
    }

    /// Other checkouts of `project`'s repository: linked worktrees and the
    /// main checkout (same `repo_root`).
    pub fn worktrees_of(&self, project: &Project) -> Result<Vec<Project>, AtlasError> {
        let Some(repo) = &project.repo_root else {
            return Ok(Vec::new());
        };
        let rows = self.query_projects(
            &format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE repo_root = ?1 AND id <> ?2 \
                 ORDER BY root"
            ),
            params![path_text(repo), project.id],
        )?;
        Ok(rows.into_iter().filter(Project::is_checkout).collect())
    }

    /// Separate clones of `project`'s remote (a different repository on
    /// disk).
    pub fn clones_of(&self, project: &Project) -> Result<Vec<Project>, AtlasError> {
        let Some(remote) = &project.remote else {
            return Ok(Vec::new());
        };
        let rows = self.query_projects(
            &format!(
                "SELECT {PROJECT_COLUMNS} FROM projects WHERE remote = ?1 AND id <> ?2 \
                 ORDER BY root"
            ),
            params![remote, project.id],
        )?;
        Ok(rows
            .into_iter()
            .filter(|p| p.is_checkout() && p.repo_root != project.repo_root)
            .collect())
    }

    /// Links declared by (or derived for) `project`.
    pub fn links_from(&self, project: i64) -> Result<Vec<ProjectLink>, AtlasError> {
        self.query_links(
            &format!(
                "SELECT {LINK_COLUMNS} FROM project_links WHERE from_project = ?1 \
                 ORDER BY kind, to_path, evidence_path, evidence_line"
            ),
            [project],
        )
    }

    /// Links from other projects into `project`.
    pub fn links_to(&self, project: i64) -> Result<Vec<ProjectLink>, AtlasError> {
        self.query_links(
            &format!(
                "SELECT {LINK_COLUMNS} FROM project_links \
                 WHERE to_project = ?1 AND from_project <> ?1 \
                 ORDER BY kind, from_project, evidence_path, evidence_line"
            ),
            [project],
        )
    }

    /// Every link.
    pub fn links(&self) -> Result<Vec<ProjectLink>, AtlasError> {
        self.query_links(
            &format!("SELECT {LINK_COLUMNS} FROM project_links ORDER BY from_project, kind, id"),
            [],
        )
    }

    fn project_by_exact_root(&self, root: &Path) -> Result<Option<Project>, AtlasError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE root = ?1"),
                [path_text(root)],
                project_from_row,
            )
            .optional()?)
    }

    fn query_projects<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<Project>, AtlasError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn query_links<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
    ) -> Result<Vec<ProjectLink>, AtlasError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, link_from_row)?;
        // Kinds this build doesn't know (a newer writer's) are skipped.
        Ok(rows
            .filter_map(|row| row.transpose())
            .collect::<rusqlite::Result<_>>()?)
    }
}

pub(super) fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    let path = |i: usize| -> rusqlite::Result<Option<PathBuf>> {
        Ok(row.get::<_, Option<String>>(i)?.map(PathBuf::from))
    };
    let count = |i: usize| -> rusqlite::Result<Option<u64>> {
        Ok(row
            .get::<_, Option<i64>>(i)?
            .map(|n| n.max(0).unsigned_abs()))
    };
    let languages: Option<String> = row.get(9)?;
    Ok(Project {
        id: row.get(0)?,
        root: PathBuf::from(row.get::<_, String>(1)?),
        name: row.get(2)?,
        checkout_root: path(3)?,
        repo_root: path(4)?,
        is_worktree: row.get(5)?,
        remote: row.get(6)?,
        branch: row.get(7)?,
        head_commit: row.get(8)?,
        languages: languages
            .as_deref()
            .map(decode_languages)
            .unwrap_or_default(),
        file_count: count(10)?,
        node_count: count(11)?,
        edge_count: count(12)?,
        counts_exact: row.get(13)?,
        index_bytes: count(14)?,
        index_schema: row.get(15)?,
        extraction_version: row.get(16)?,
        engine_version: row.get(17)?,
        last_indexed_ms: row.get(18)?,
        last_seen_ms: row.get(19)?,
        registered_ms: row.get(20)?,
        status: ProjectStatus::parse(&row.get::<_, String>(21)?),
    })
}

/// A link row, or `None` for a kind this build doesn't know.
fn link_from_row(row: &Row<'_>) -> rusqlite::Result<Option<ProjectLink>> {
    let Some(kind) = LinkKind::parse(&row.get::<_, String>(4)?) else {
        return Ok(None);
    };
    let evidence = row
        .get::<_, Option<String>>(5)?
        .map(|path| -> rusqlite::Result<Evidence> {
            Ok(Evidence {
                path: PathBuf::from(path),
                line: row.get(6)?,
            })
        })
        .transpose()?;
    Ok(Some(ProjectLink {
        id: row.get(0)?,
        from_project: row.get(1)?,
        to_project: row.get(2)?,
        to_path: PathBuf::from(row.get::<_, String>(3)?),
        kind,
        evidence,
        detail: row.get(7)?,
    }))
}

/// `[["rust", 12], ["toml", 1]]` → counts (malformed JSON: none).
fn decode_languages(json: &str) -> Vec<LanguageCount> {
    serde_json::from_str::<Vec<(String, u64)>>(json)
        .map(|pairs| {
            pairs
                .into_iter()
                .map(|(language, files)| LanguageCount { language, files })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn encode_languages(languages: &[LanguageCount]) -> String {
    let pairs: Vec<(&str, u64)> = languages
        .iter()
        .map(|l| (l.language.as_str(), l.files))
        .collect();
    serde_json::to_string(&pairs).unwrap_or_else(|_| "[]".to_owned())
}

/// A path as the atlas stores it.
pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('_', "\\_")
        .replace('%', "\\%")
}
