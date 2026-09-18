//! Atlas writes: registering a project (one short `BEGIN IMMEDIATE`
//! transaction), re-resolving links, pruning vanished projects.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::facts::ProjectFacts;
use super::kinds::{LinkKind, ProjectStatus};
use super::query::{encode_languages, path_text};
use super::store::{Atlas, AtlasError, PruneReport};
use crate::directory::is_initialized;

/// What one registration wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    pub project_id: i64,
    /// Manifest links recorded for the project.
    pub links: usize,
    /// First time the atlas saw this root.
    pub new: bool,
}

impl Atlas {
    /// Upsert `facts` and replace the project's manifest links, then
    /// re-resolve every link — one short transaction.
    pub fn register(&mut self, facts: &ProjectFacts) -> Result<Registered, AtlasError> {
        let tx = self.write_tx()?;
        let registered = upsert(&tx, facts)?;
        relink(&tx)?;
        tx.commit().map_err(AtlasError::from_sqlite)?;
        Ok(registered)
    }

    /// Like [`Self::register`] but leaves link resolution to a later
    /// [`Self::relink`] (a scan registers hundreds, then relinks once).
    pub fn register_deferred(&mut self, facts: &ProjectFacts) -> Result<Registered, AtlasError> {
        let tx = self.write_tx()?;
        let registered = upsert(&tx, facts)?;
        tx.commit().map_err(AtlasError::from_sqlite)?;
        Ok(registered)
    }

    /// Re-resolve every link's `to_project` and rebuild the derived
    /// (`same_remote`, `nested_workspace`) links.
    pub fn relink(&mut self) -> Result<(), AtlasError> {
        let tx = self.write_tx()?;
        relink(&tx)?;
        tx.commit().map_err(AtlasError::from_sqlite)?;
        Ok(())
    }

    /// Mark projects whose root or index vanished `missing`; with `remove`,
    /// delete them (their outgoing links go with them).
    pub fn prune(&mut self, remove: bool) -> Result<PruneReport, AtlasError> {
        let projects = self.projects()?;
        let mut report = PruneReport {
            checked: projects.len(),
            ..PruneReport::default()
        };
        let vanished: Vec<_> = projects
            .into_iter()
            .filter(|p| !is_initialized(&p.root))
            .collect();
        let tx = self.write_tx()?;
        for project in &vanished {
            let root = path_text(&project.root);
            if remove {
                tx.execute("DELETE FROM projects WHERE id = ?1", [project.id])?;
                report.removed.push(root);
            } else if project.status != ProjectStatus::Missing {
                tx.execute(
                    "UPDATE projects SET status = ?2 WHERE id = ?1",
                    params![project.id, ProjectStatus::Missing.as_str()],
                )?;
                report.marked_missing.push(root);
            }
        }
        relink(&tx)?;
        tx.commit().map_err(AtlasError::from_sqlite)?;
        Ok(report)
    }

    /// Take the write lock up front (no read-then-upgrade deadlock); on
    /// contention past the busy timeout the caller gets [`AtlasError::Busy`].
    fn write_tx(&mut self) -> Result<Transaction<'_>, AtlasError> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(AtlasError::from_sqlite)
    }
}

fn upsert(tx: &Transaction<'_>, facts: &ProjectFacts) -> Result<Registered, AtlasError> {
    let now = super::now_ms();
    let root = path_text(&facts.root);
    let existed: Option<i64> = tx
        .query_row("SELECT id FROM projects WHERE root = ?1", [&root], |r| {
            r.get(0)
        })
        .optional()?;
    if let (Some(project_id), ProjectStatus::Missing) = (existed, facts.status) {
        // Gone: keep what it was last seen as (remote, stats, links).
        tx.execute(
            "UPDATE projects SET status = ?2 WHERE id = ?1",
            params![project_id, ProjectStatus::Missing.as_str()],
        )?;
        return Ok(Registered {
            project_id,
            links: 0,
            new: false,
        });
    }
    let index = &facts.index;
    let to_i64 = |n: Option<u64>| n.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
    let project_id: i64 = tx.query_row(
        "INSERT INTO projects (
             root, name, checkout_root, repo_root, is_worktree, remote, branch, head_commit,
             languages, file_count, node_count, edge_count, counts_exact, index_bytes,
             index_schema, extraction_version, engine_version, last_indexed, last_seen,
             registered_at, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                   ?18, ?19, ?19, ?20)
         ON CONFLICT(root) DO UPDATE SET
             name = excluded.name, checkout_root = excluded.checkout_root,
             repo_root = excluded.repo_root, is_worktree = excluded.is_worktree,
             remote = excluded.remote, branch = excluded.branch,
             head_commit = excluded.head_commit,
             languages = COALESCE(excluded.languages, projects.languages),
             file_count = COALESCE(excluded.file_count, projects.file_count),
             node_count = COALESCE(excluded.node_count, projects.node_count),
             edge_count = COALESCE(excluded.edge_count, projects.edge_count),
             counts_exact = excluded.counts_exact,
             index_bytes = COALESCE(excluded.index_bytes, projects.index_bytes),
             index_schema = COALESCE(excluded.index_schema, projects.index_schema),
             extraction_version = COALESCE(excluded.extraction_version, projects.extraction_version),
             engine_version = COALESCE(excluded.engine_version, projects.engine_version),
             last_indexed = COALESCE(excluded.last_indexed, projects.last_indexed),
             last_seen = excluded.last_seen, status = excluded.status
         RETURNING id",
        params![
            root,
            facts.name,
            facts.git.checkout_root.as_deref().map(path_text),
            facts.git.repo_root.as_deref().map(path_text),
            facts.git.is_worktree,
            facts.git.remote,
            facts.git.branch,
            facts.git.head_commit,
            (!index.languages.is_empty()).then(|| encode_languages(&index.languages)),
            to_i64(index.file_count),
            to_i64(index.node_count),
            to_i64(index.edge_count),
            index.counts_exact,
            to_i64(index.index_bytes),
            index.schema,
            index.extraction_version,
            index.engine_version,
            index.last_indexed_ms,
            now,
            facts.status.as_str(),
        ],
        |r| r.get(0),
    )?;

    let manifest_kinds: Vec<&str> = LinkKind::ALL
        .iter()
        .filter(|k| !k.is_derived())
        .map(|k| k.as_str())
        .collect();
    tx.execute(
        &format!(
            "DELETE FROM project_links WHERE from_project = ?1 AND kind IN ({})",
            sql_list(&manifest_kinds)
        ),
        [project_id],
    )?;
    let mut insert = tx.prepare(
        "INSERT OR IGNORE INTO project_links
             (from_project, to_path, kind, evidence_path, evidence_line, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut links = 0;
    for link in &facts.links {
        links += insert.execute(params![
            project_id,
            path_text(&link.target),
            link.kind.as_str(),
            path_text(&link.manifest),
            link.line,
            link.detail,
        ])?;
    }
    Ok(Registered {
        project_id,
        links,
        new: existed.is_none(),
    })
}

/// One project as the linker sees it.
struct Node {
    id: i64,
    root: PathBuf,
    checkout_root: Option<PathBuf>,
    repo_root: Option<PathBuf>,
    is_worktree: bool,
    remote: Option<String>,
}

impl Node {
    fn is_checkout(&self) -> bool {
        self.checkout_root.as_deref() == Some(self.root.as_path())
    }
}

/// Resolve every manifest link to the nearest registered project at or
/// above its target, and rebuild the derived links from the project rows.
fn relink(tx: &Connection) -> Result<(), AtlasError> {
    let nodes = load_nodes(tx)?;
    let by_root: HashMap<&Path, &Node> = nodes.iter().map(|n| (n.root.as_path(), n)).collect();
    let nearest = |path: &Path, strict: bool| -> Option<&Node> {
        path.ancestors()
            .skip(usize::from(strict))
            .find_map(|dir| by_root.get(dir).copied())
    };

    let derived: Vec<&str> = LinkKind::ALL
        .iter()
        .filter(|k| k.is_derived())
        .map(|k| k.as_str())
        .collect();
    tx.execute(
        &format!(
            "DELETE FROM project_links WHERE kind IN ({})",
            sql_list(&derived)
        ),
        [],
    )?;

    // Manifest links: to_project = nearest registered root.
    let mut stmt = tx.prepare("SELECT id, to_path, to_project FROM project_links")?;
    let rows: Vec<(i64, String, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = tx.prepare("UPDATE project_links SET to_project = ?2 WHERE id = ?1")?;
    for (id, to_path, current) in rows {
        let resolved = nearest(Path::new(&to_path), false).map(|n| n.id);
        if resolved != current {
            update.execute(params![id, resolved])?;
        }
    }

    let mut insert = tx.prepare(
        "INSERT OR IGNORE INTO project_links (from_project, to_project, to_path, kind, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    // nested_workspace: the nearest registered ancestor contains this
    // project — unless this is a worktree of that very repository (the
    // worktree relation already groups them).
    for node in &nodes {
        let Some(parent) = nearest(&node.root, true) else {
            continue;
        };
        let worktree_of_parent = node.is_worktree && node.repo_root == parent.repo_root;
        if worktree_of_parent {
            continue;
        }
        let rel = node.root.strip_prefix(&parent.root).map_or_else(
            |_| path_text(&node.root),
            |r| r.to_string_lossy().into_owned(),
        );
        insert.execute(params![
            parent.id,
            node.id,
            path_text(&node.root),
            LinkKind::NestedWorkspace.as_str(),
            rel,
        ])?;
    }
    // same_remote: separate clones (different repositories on disk) of one
    // remote, linked both ways between one representative checkout per
    // repository — its main checkout when registered — so worktrees don't
    // multiply the pairs.
    let mut by_remote: BTreeMap<&str, BTreeMap<&Path, &Node>> = BTreeMap::new();
    for node in nodes.iter().filter(|n| n.is_checkout()) {
        let Some(remote) = &node.remote else { continue };
        let repo = node.repo_root.as_deref().unwrap_or(&node.root);
        let slot = by_remote
            .entry(remote)
            .or_default()
            .entry(repo)
            .or_insert(node);
        let is_main = |n: &Node| n.root.as_path() == repo;
        if is_main(node) || (!is_main(slot) && node.id < slot.id) {
            *slot = node;
        }
    }
    for (remote, repos) in by_remote {
        let group: Vec<&Node> = repos.into_values().collect();
        for a in &group {
            for b in &group {
                if a.id != b.id {
                    insert.execute(params![
                        a.id,
                        b.id,
                        path_text(&b.root),
                        LinkKind::SameRemote.as_str(),
                        remote,
                    ])?;
                }
            }
        }
    }
    Ok(())
}

fn load_nodes(conn: &Connection) -> rusqlite::Result<Vec<Node>> {
    let mut stmt = conn
        .prepare("SELECT id, root, checkout_root, repo_root, is_worktree, remote FROM projects")?;
    let rows = stmt.query_map([], |r| {
        Ok(Node {
            id: r.get(0)?,
            root: PathBuf::from(r.get::<_, String>(1)?),
            checkout_root: r.get::<_, Option<String>>(2)?.map(PathBuf::from),
            repo_root: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
            is_worktree: r.get(4)?,
            remote: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// `'a', 'b'` for a fixed list of known identifiers.
fn sql_list(items: &[&str]) -> String {
    items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ")
}
