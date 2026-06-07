//! Database Queries
//!
//! Prepared statements for CRUD operations on the knowledge graph.
//! Ported from `src/db/queries.ts`.
//!
//! NOTE on shared helpers: like the TS file (which imports `kindBonus`/
//! `nameMatchBonus`/`scorePathRelevance` from `../search/query-utils`,
//! `parseQuery`/`boundedEditDistance` from `../search/query-parser`, and
//! `isGeneratedFile` from `../extraction/generated-detection`), this file
//! pulls those from `crate::search` and
//! `crate::extraction::generated_detection`. `is_low_value_file` and the
//! FTS-operator filter are defined inline in the TS file, so they live
//! here too (see the "Inline helpers" section near the bottom).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::types::Value;
use rusqlite::{Row, params_from_iter};

use crate::db::connection::{Db, now_ms};
use crate::error::{Result, log_error};
use crate::extraction::generated_detection::is_generated_file;
use crate::search::{
    bounded_edit_distance,
    kind_bonus,
    name_match_bonus,
    parse_query,
    score_path_relevance,
};
use crate::types::{
    Edge,
    EdgeKind,
    FileRecord,
    GraphStats,
    Language,
    Node,
    NodeKind,
    Provenance,
    SearchOptions,
    SearchResult,
    UnresolvedReference,
    Visibility,
};
use crate::utils::safe_json_parse;

const SQLITE_PARAM_CHUNK_SIZE: usize = 500;

// Node cache max size (LRU-style)
const MAX_CACHE_SIZE: usize = 1000;

// =============================================================================
// Row → type converters
// =============================================================================

fn conv_err(msg: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, msg.into().into())
}

fn parse_visibility(s: Option<String>) -> Option<Visibility> {
    match s.as_deref() {
        Some("public") => Some(Visibility::Public),
        Some("private") => Some(Visibility::Private),
        Some("protected") => Some(Visibility::Protected),
        Some("internal") => Some(Visibility::Internal),
        _ => None,
    }
}

fn visibility_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Protected => "protected",
        Visibility::Internal => "internal",
    }
}

/// Convert database row to Node object (TS `rowToNode`).
fn node_from_row(row: &Row<'_>) -> rusqlite::Result<Node> {
    let kind_s: String = row.get("kind")?;
    let kind: NodeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let lang_s: String = row.get("language")?;
    // Unknown language strings fall back to `unknown` (TS casts blindly).
    let language: Language = lang_s.parse().unwrap_or(Language::Unknown);
    let visibility: Option<String> = row.get("visibility")?;
    let decorators: Option<String> = row.get("decorators")?;
    let type_parameters: Option<String> = row.get("type_parameters")?;
    Ok(Node {
        id: row.get("id")?,
        kind,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        file_path: row.get("file_path")?,
        language,
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        start_column: row.get("start_column")?,
        end_column: row.get("end_column")?,
        start_byte: row.get("start_byte")?,
        end_byte: row.get("end_byte")?,
        docstring: row.get("docstring")?,
        signature: row.get("signature")?,
        visibility: parse_visibility(visibility),
        is_exported: Some(row.get::<_, i64>("is_exported")? == 1),
        is_async: Some(row.get::<_, i64>("is_async")? == 1),
        is_static: Some(row.get::<_, i64>("is_static")? == 1),
        is_abstract: Some(row.get::<_, i64>("is_abstract")? == 1),
        decorators: decorators.and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        type_parameters: type_parameters
            .and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        updated_at: lenient_i64(row, "updated_at")?,
    })
}

/// Convert database row to Edge object (TS `rowToEdge`).
fn edge_from_row(row: &Row<'_>) -> rusqlite::Result<Edge> {
    let kind_s: String = row.get("kind")?;
    let kind: EdgeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let metadata: Option<String> = row.get("metadata")?;
    let provenance: Option<String> = row.get("provenance")?;
    Ok(Edge {
        source: row.get("source")?,
        target: row.get("target")?,
        kind,
        metadata: metadata
            .and_then(|s| safe_json_parse::<Option<crate::types::Metadata>>(&s, None)),
        line: row.get("line")?,
        column: row.get("col")?,
        provenance: provenance.and_then(|s| s.parse::<Provenance>().ok()),
    })
}

/// Convert database row to UnresolvedReference (TS `rowToUnresolvedReference`).
fn unresolved_from_row(row: &Row<'_>) -> rusqlite::Result<UnresolvedReference> {
    let kind_s: String = row.get("reference_kind")?;
    let reference_kind: EdgeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let candidates: Option<String> = row.get("candidates")?;
    let lang_s: String = row.get("language")?;
    Ok(UnresolvedReference {
        from_node_id: row.get("from_node_id")?,
        reference_name: row.get("reference_name")?,
        reference_kind,
        line: row.get("line")?,
        column: row.get("col")?,
        candidates: candidates.and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        file_path: Some(row.get("file_path")?),
        language: Some(lang_s.parse().unwrap_or(Language::Unknown)),
    })
}

/// Read an INTEGER-ish column leniently, accepting REAL by truncation.
///
/// The TS implementation stores plain JS numbers, and some of them are
/// fractional — most notably `files.modified_at`, which Node fills from
/// `fs.statSync().mtimeMs` (a float). A TS-written database therefore holds
/// REAL where the Rust port expects INTEGER; truncate exactly like a JS
/// reader coerced back through `Math.trunc` would, so the two
/// implementations can share one `.codegraph/codegraph.db`.
fn lenient_i64(row: &Row<'_>, col: &str) -> rusqlite::Result<i64> {
    use rusqlite::types::ValueRef;
    match row.get_ref(col)? {
        ValueRef::Integer(i) => Ok(i),
        ValueRef::Real(f) => Ok(f as i64),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            other.data_type(),
            format!("expected INTEGER or REAL for column {col}").into(),
        )),
    }
}

/// Convert database row to FileRecord object (TS `rowToFileRecord`).
fn file_from_row(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    let lang_s: String = row.get("language")?;
    let errors: Option<String> = row.get("errors")?;
    Ok(FileRecord {
        path: row.get("path")?,
        content_hash: row.get("content_hash")?,
        language: lang_s.parse().unwrap_or(Language::Unknown),
        size: lenient_i64(row, "size")?.max(0) as u64,
        modified_at: lenient_i64(row, "modified_at")?,
        indexed_at: lenient_i64(row, "indexed_at")?,
        node_count: row.get("node_count")?,
        errors: errors
            .and_then(|s| safe_json_parse::<Option<Vec<crate::types::ExtractionError>>>(&s, None)),
    })
}

fn placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
}

// =============================================================================
// Result structs (TS returned anonymous object literals)
// =============================================================================

/// Result of [`QueryBuilder::get_dominant_file`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DominantFile {
    pub file_path: String,
    pub edge_count: u64,
    pub next_edge_count: u64,
}

/// Result of [`QueryBuilder::get_top_route_file`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopRouteFile {
    pub file_path: String,
    pub route_count: u64,
    pub total_routes: u64,
}

/// One URL → handler mapping from [`QueryBuilder::get_routing_manifest`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingManifestEntry {
    pub url: String,
    pub handler: String,
    pub handler_file: String,
    pub handler_line: u32,
    pub handler_kind: String,
}

/// Result of [`QueryBuilder::get_routing_manifest`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingManifest {
    pub entries: Vec<RoutingManifestEntry>,
    pub top_handler_file: Option<String>,
    pub top_handler_file_count: u64,
    pub total_routes: u64,
}

/// Lightweight (nodes, edges) count snapshot.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct NodeEdgeCount {
    pub nodes: u64,
    pub edges: u64,
}

/// A page of unresolved references plus the last stable row id seen.
#[derive(Debug, Clone)]
pub struct UnresolvedBatch {
    pub refs: Vec<UnresolvedReference>,
    pub last_id: i64,
}

/// Key identifying a resolved reference for precise deletion
/// (TS `{ fromNodeId, referenceName, referenceKind }`).
#[derive(Debug, Clone)]
pub struct ResolvedRefKey {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
}

// =============================================================================
// LRU node cache (TS used insertion-ordered Map semantics)
// =============================================================================

struct NodeLru {
    map: HashMap<String, (u64, Node)>,
    order: BTreeMap<u64, String>,
    seq: u64,
    cap: usize,
}

impl NodeLru {
    fn new(cap: usize) -> Self {
        NodeLru {
            map: HashMap::new(),
            order: BTreeMap::new(),
            seq: 0,
            cap,
        }
    }

    /// Get + LRU-touch (TS delete-and-re-add on the Map).
    fn get_touch(&mut self, id: &str) -> Option<Node> {
        let (old_seq, node) = self.map.get(id).map(|(s, n)| (*s, n.clone()))?;
        self.order.remove(&old_seq);
        self.seq += 1;
        self.order.insert(self.seq, id.to_string());
        if let Some(entry) = self.map.get_mut(id) {
            entry.0 = self.seq;
        }
        Some(node)
    }

    /// Add a node to the cache, evicting oldest if needed (TS `cacheNode`).
    fn insert(&mut self, node: Node) {
        if let Some((old_seq, _)) = self.map.remove(&node.id) {
            self.order.remove(&old_seq);
        } else if self.map.len() >= self.cap {
            // Evict oldest (first) entry
            if let Some((&oldest, _)) = self.order.iter().next() {
                if let Some(id) = self.order.remove(&oldest) {
                    self.map.remove(&id);
                }
            }
        }
        self.seq += 1;
        self.order.insert(self.seq, node.id.clone());
        self.map.insert(node.id.clone(), (self.seq, node));
    }

    fn remove(&mut self, id: &str) {
        if let Some((seq, _)) = self.map.remove(id) {
            self.order.remove(&seq);
        }
    }

    /// Invalidate cache for nodes in a file (TS `deleteNodesByFile` loop).
    fn remove_by_file(&mut self, file_path: &str) {
        let ids: Vec<String> = self
            .map
            .iter()
            .filter(|(_, (_, n))| n.file_path == file_path)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.remove(&id);
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

// =============================================================================
// QueryBuilder
// =============================================================================

/// Query builder for the knowledge graph database.
pub struct QueryBuilder {
    db: Db,
    // Node cache for frequently accessed nodes (LRU-style, max 1000 entries)
    node_cache: RefCell<NodeLru>,
}

impl QueryBuilder {
    pub fn new(db: Db) -> Self {
        QueryBuilder {
            db,
            node_cache: RefCell::new(NodeLru::new(MAX_CACHE_SIZE)),
        }
    }

    /// Borrow the underlying shared handle (for callers that need raw SQL).
    pub fn db(&self) -> &Db {
        &self.db
    }

    // =========================================================================
    // Node Operations
    // =========================================================================

    /// Insert a new node.
    pub fn insert_node(&self, node: &Node) -> Result<()> {
        // Validate required fields to prevent SQLite bind errors
        if node.id.is_empty() || node.name.is_empty() || node.file_path.is_empty() {
            log_error(
                "Skipping node with missing required fields:",
                Some(&serde_json::json!({
                    "id": node.id,
                    "kind": node.kind.as_str(),
                    "name": node.name,
                    "filePath": node.file_path,
                    "language": node.language.as_str(),
                })),
            );
            return Ok(());
        }

        // INSERT OR REPLACE may overwrite a node we have cached. Drop the
        // stale entry so the next get_node_by_id sees the new row, not the
        // old one (matches the cache-invalidation pattern used by
        // update_node and delete_node below).
        self.node_cache.borrow_mut().remove(&node.id);

        let mut stmt = self.db.conn().prepare_cached(
            "INSERT OR REPLACE INTO nodes (
              id, kind, name, qualified_name, file_path, language,
              start_line, end_line, start_column, end_column,
              start_byte, end_byte,
              docstring, signature, visibility,
              is_exported, is_async, is_static, is_abstract,
              decorators, type_parameters, updated_at
            ) VALUES (
              @id, @kind, @name, @qualifiedName, @filePath, @language,
              @startLine, @endLine, @startColumn, @endColumn,
              @startByte, @endByte,
              @docstring, @signature, @visibility,
              @isExported, @isAsync, @isStatic, @isAbstract,
              @decorators, @typeParameters, @updatedAt
            )",
        )?;

        let qualified_name: &str = if node.qualified_name.is_empty() {
            &node.name
        } else {
            &node.qualified_name
        };
        let decorators: Option<String> = match &node.decorators {
            Some(d) => Some(serde_json::to_string(d)?),
            None => None,
        };
        let type_parameters: Option<String> = match &node.type_parameters {
            Some(t) => Some(serde_json::to_string(t)?),
            None => None,
        };
        let updated_at = if node.updated_at == 0 {
            now_ms()
        } else {
            node.updated_at
        };

        stmt.execute(rusqlite::named_params! {
            "@id": node.id,
            "@kind": node.kind.as_str(),
            "@name": node.name,
            "@qualifiedName": qualified_name,
            "@filePath": node.file_path,
            "@language": node.language.as_str(),
            "@startLine": node.start_line,
            "@endLine": node.end_line,
            "@startColumn": node.start_column,
            "@endColumn": node.end_column,
            "@startByte": node.start_byte,
            "@endByte": node.end_byte,
            "@docstring": node.docstring,
            "@signature": node.signature,
            "@visibility": node.visibility.map(visibility_str),
            "@isExported": node.is_exported.unwrap_or(false) as i64,
            "@isAsync": node.is_async.unwrap_or(false) as i64,
            "@isStatic": node.is_static.unwrap_or(false) as i64,
            "@isAbstract": node.is_abstract.unwrap_or(false) as i64,
            "@decorators": decorators,
            "@typeParameters": type_parameters,
            "@updatedAt": updated_at,
        })?;
        Ok(())
    }

    /// Insert multiple nodes in a transaction.
    pub fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        self.db.transaction(|| {
            for node in nodes {
                self.insert_node(node)?;
            }
            Ok(())
        })
    }

    /// Update an existing node.
    pub fn update_node(&self, node: &Node) -> Result<()> {
        // Invalidate cache before update
        self.node_cache.borrow_mut().remove(&node.id);

        // Validate required fields
        if node.id.is_empty() || node.name.is_empty() || node.file_path.is_empty() {
            log_error(
                "Skipping node update with missing required fields:",
                Some(&serde_json::json!(node.id)),
            );
            return Ok(());
        }

        let mut stmt = self.db.conn().prepare_cached(
            "UPDATE nodes SET
              kind = @kind,
              name = @name,
              qualified_name = @qualifiedName,
              file_path = @filePath,
              language = @language,
              start_line = @startLine,
              end_line = @endLine,
              start_column = @startColumn,
              end_column = @endColumn,
              start_byte = @startByte,
              end_byte = @endByte,
              docstring = @docstring,
              signature = @signature,
              visibility = @visibility,
              is_exported = @isExported,
              is_async = @isAsync,
              is_static = @isStatic,
              is_abstract = @isAbstract,
              decorators = @decorators,
              type_parameters = @typeParameters,
              updated_at = @updatedAt
            WHERE id = @id",
        )?;

        let qualified_name: &str = if node.qualified_name.is_empty() {
            &node.name
        } else {
            &node.qualified_name
        };
        let decorators: Option<String> = match &node.decorators {
            Some(d) => Some(serde_json::to_string(d)?),
            None => None,
        };
        let type_parameters: Option<String> = match &node.type_parameters {
            Some(t) => Some(serde_json::to_string(t)?),
            None => None,
        };
        let updated_at = if node.updated_at == 0 {
            now_ms()
        } else {
            node.updated_at
        };

        stmt.execute(rusqlite::named_params! {
            "@id": node.id,
            "@kind": node.kind.as_str(),
            "@name": node.name,
            "@qualifiedName": qualified_name,
            "@filePath": node.file_path,
            "@language": node.language.as_str(),
            "@startLine": node.start_line,
            "@endLine": node.end_line,
            "@startColumn": node.start_column,
            "@endColumn": node.end_column,
            "@startByte": node.start_byte,
            "@endByte": node.end_byte,
            "@docstring": node.docstring,
            "@signature": node.signature,
            "@visibility": node.visibility.map(visibility_str),
            "@isExported": node.is_exported.unwrap_or(false) as i64,
            "@isAsync": node.is_async.unwrap_or(false) as i64,
            "@isStatic": node.is_static.unwrap_or(false) as i64,
            "@isAbstract": node.is_abstract.unwrap_or(false) as i64,
            "@decorators": decorators,
            "@typeParameters": type_parameters,
            "@updatedAt": updated_at,
        })?;
        Ok(())
    }

    /// Delete a node by ID.
    pub fn delete_node(&self, id: &str) -> Result<()> {
        // Invalidate cache
        self.node_cache.borrow_mut().remove(id);
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM nodes WHERE id = ?")?;
        stmt.execute([id])?;
        Ok(())
    }

    /// Delete all nodes for a file.
    pub fn delete_nodes_by_file(&self, file_path: &str) -> Result<()> {
        // Invalidate cache for nodes in this file
        self.node_cache.borrow_mut().remove_by_file(file_path);
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM nodes WHERE file_path = ?")?;
        stmt.execute([file_path])?;
        Ok(())
    }

    /// Get a node by ID.
    pub fn get_node_by_id(&self, id: &str) -> Result<Option<Node>> {
        // Check cache first (get + LRU touch)
        if let Some(cached) = self.node_cache.borrow_mut().get_touch(id) {
            return Ok(Some(cached));
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE id = ?")?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => {
                let node = node_from_row(row)?;
                self.cache_node(node.clone());
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// Batch lookup: fetch many nodes by ID in a single SQL round-trip.
    ///
    /// Replaces the N+1 pattern in graph traversal where every edge would
    /// trigger its own `get_node_by_id` call. For a function with 50 callers
    /// this collapses 50 point reads into one IN-list query (~10-50x
    /// faster end-to-end).
    ///
    /// Returns a map keyed by id so callers can preserve their own ordering
    /// (typically the order edges were returned from the graph). Missing IDs
    /// are simply absent from the map.
    ///
    /// Cache-aware: ids already in the LRU cache are served from memory and
    /// the SQL query only touches the misses.
    pub fn get_nodes_by_ids(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }

        // Serve cache hits first; build the miss list for SQL.
        let mut misses: Vec<&String> = Vec::new();
        {
            let mut cache = self.node_cache.borrow_mut();
            for id in ids {
                match cache.get_touch(id) {
                    Some(node) => {
                        out.insert(id.clone(), node);
                    }
                    None => misses.push(id),
                }
            }
        }
        if misses.is_empty() {
            return Ok(out);
        }

        // Chunk under SQLite's parameter limit (default 999; chunk at 500
        // for safety and to keep the query plan simple).
        for chunk in misses.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                node_from_row,
            )?;
            for row in rows {
                let node = row?;
                out.insert(node.id.clone(), node.clone());
                self.cache_node(node);
            }
        }
        Ok(out)
    }

    fn get_existing_node_ids(&self, ids: &[&String]) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        if ids.is_empty() {
            return Ok(out);
        }

        let mut unique_ids: Vec<&String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for id in ids {
            if seen.insert(id.as_str()) {
                unique_ids.push(id);
            }
        }

        for chunk in unique_ids.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT id FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(chunk.iter().map(|s| s.as_str())), |row| {
                    row.get::<_, String>(0)
                })?;
            for row in rows {
                out.insert(row?);
            }
        }
        Ok(out)
    }

    /// Add a node to the cache, evicting oldest if needed.
    fn cache_node(&self, node: Node) {
        self.node_cache.borrow_mut().insert(node);
    }

    /// Clear the node cache.
    pub fn clear_cache(&self) {
        self.node_cache.borrow_mut().clear();
    }

    /// Get all nodes in a file.
    pub fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE file_path = ? ORDER BY start_line")?;
        let rows = stmt.query_map([file_path], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Find the file that holds the densest concentration of the project's
    /// internal call graph — the "core" file. Used by context-builder to
    /// boost ranking of symbols in that file's directory.
    ///
    /// Returns None if no file has a meaningful concentration (e.g. spread
    /// evenly across many files, or empty index).
    ///
    /// "Internal" = source and target are in the same file. Cross-file
    /// edges aren't useful here — they don't tell us which file is the
    /// functional center.
    ///
    /// Excludes test/spec files from candidacy via path-pattern.
    pub fn get_dominant_file(&self) -> Result<Option<DominantFile>> {
        // Pull top 20 candidates; we then filter out test/generated files
        // in code (regex-grade matching that SQL LIKE can't express).
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT n.file_path AS file_path, COUNT(*) AS edge_count
             FROM edges e
             JOIN nodes n ON e.source = n.id
             JOIN nodes m ON e.target = m.id
             WHERE n.file_path = m.file_path
             GROUP BY n.file_path
             ORDER BY edge_count DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
            ))
        })?;
        let mut filtered: Vec<(String, u64)> = Vec::new();
        for row in rows {
            let (file_path, edge_count) = row?;
            if !is_low_value_file(&file_path) {
                filtered.push((file_path, edge_count));
            }
        }
        if filtered.is_empty() || filtered[0].1 < 20 {
            return Ok(None);
        }
        Ok(Some(DominantFile {
            file_path: filtered[0].0.clone(),
            edge_count: filtered[0].1,
            next_edge_count: filtered.get(1).map(|r| r.1).unwrap_or(0),
        }))
    }

    /// Find the file that holds the densest concentration of the project's
    /// `route` nodes (framework-emitted: Express/Gin/Flask/Rails/Drupal/etc.).
    ///
    /// Excludes test/generated files from candidacy. Returns None if there
    /// are fewer than 3 non-test routes total, or if no file holds at least
    /// 30% of them (diffuse routing → no single answer file).
    pub fn get_top_route_file(&self) -> Result<Option<TopRouteFile>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT file_path, COUNT(*) AS cnt
             FROM nodes
             WHERE kind = 'route'
             GROUP BY file_path
             ORDER BY cnt DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
            ))
        })?;
        let mut filtered: Vec<(String, u64)> = Vec::new();
        for row in rows {
            let (file_path, cnt) = row?;
            if !is_low_value_file(&file_path) {
                filtered.push((file_path, cnt));
            }
        }
        if filtered.is_empty() {
            return Ok(None);
        }
        let total_routes: u64 = filtered.iter().map(|r| r.1).sum();
        let top = &filtered[0];
        if total_routes < 3 || top.1 < 3 {
            return Ok(None);
        }
        if (top.1 as f64) / (total_routes as f64) < 0.30 {
            return Ok(None);
        }
        Ok(Some(TopRouteFile {
            file_path: top.0.clone(),
            route_count: top.1,
            total_routes,
        }))
    }

    /// Build a URL → handler manifest from the index. Each route node's
    /// `references` edge points at the function/method that handles the
    /// request. We join them in one pass; the agent gets the canonical
    /// routing answer ("POST /users/login → AuthController#login") without
    /// having to parse the framework's route DSL itself.
    ///
    /// Also returns the file with the most handler endpoints.
    /// `limit` defaults to 40 when `None`.
    pub fn get_routing_manifest(&self, limit: Option<usize>) -> Result<Option<RoutingManifest>> {
        let limit = limit.unwrap_or(40);
        // Edge kind varies across framework resolvers: Spring/Rails/
        // Laravel/Drupal emit `references`, Express emits `calls`. Accept
        // both — the semantic is the same (route → its handler).
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT
               r.name AS url,
               h.name AS handler,
               h.file_path AS handler_file,
               h.start_line AS handler_line,
               h.kind AS handler_kind
             FROM nodes r
             JOIN edges e ON e.source = r.id
             JOIN nodes h ON e.target = h.id
             WHERE r.kind = 'route'
               AND e.kind IN ('references', 'calls')
               AND h.kind IN ('function', 'method', 'class')
             ORDER BY r.file_path, r.start_line
             LIMIT ?",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(RoutingManifestEntry {
                url: row.get(0)?,
                handler: row.get(1)?,
                handler_file: row.get(2)?,
                handler_line: row.get(3)?,
                handler_kind: row.get(4)?,
            })
        })?;
        // Drop test/generated handlers — same hygiene as elsewhere.
        let mut filtered: Vec<RoutingManifestEntry> = Vec::new();
        for row in rows {
            let entry = row?;
            if !is_low_value_file(&entry.handler_file) {
                filtered.push(entry);
            }
        }
        if filtered.len() < 3 {
            return Ok(None);
        }
        // Identify the file holding the most handlers (the "primary handler
        // file"). Insertion-ordered so ties resolve to the first-seen file,
        // matching the TS Map iteration order.
        let mut file_counts: Vec<(String, u64)> = Vec::new();
        for entry in &filtered {
            match file_counts
                .iter_mut()
                .find(|(f, _)| f == &entry.handler_file)
            {
                Some((_, c)) => *c += 1,
                None => file_counts.push((entry.handler_file.clone(), 1)),
            }
        }
        let mut top_handler_file: Option<String> = None;
        let mut top_handler_file_count: u64 = 0;
        for (file, count) in &file_counts {
            if *count > top_handler_file_count {
                top_handler_file = Some(file.clone());
                top_handler_file_count = *count;
            }
        }
        let total_routes = filtered.len() as u64;
        Ok(Some(RoutingManifest {
            entries: filtered,
            top_handler_file,
            top_handler_file_count,
            total_routes,
        }))
    }

    /// Get all nodes of a specific kind.
    pub fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE kind = ?")?;
        let rows = stmt.query_map([kind.as_str()], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Stream every node of a kind one at a time (lazy) instead of
    /// materializing them all like [`Self::get_nodes_by_kind`]. For unbounded
    /// kinds (`function`, `method`) on a symbol-dense project the full vector
    /// is gigabytes; the dynamic-edge synthesizers only scan-and-filter, so
    /// they iterate to keep memory O(1) in the node count rather than
    /// O(nodes) (#610).
    ///
    /// Rust deviation: a generator becomes a visitor callback. Return `true`
    /// from `f` to continue, `false` to stop early. Other queries on the same
    /// connection are safe to run from inside `f` (the cursor stays valid).
    pub fn iterate_nodes_by_kind(
        &self,
        kind: NodeKind,
        mut f: impl FnMut(Node) -> bool,
    ) -> Result<()> {
        // Fresh statement per call (not a cached one): an iterator holds an
        // open cursor, so a shared statement would conflict across
        // overlapping scans.
        let conn = self.db.conn();
        let mut stmt = conn.prepare("SELECT * FROM nodes WHERE kind = ?")?;
        let mut rows = stmt.query([kind.as_str()])?;
        while let Some(row) = rows.next()? {
            let node = node_from_row(row)?;
            if !f(node) {
                break;
            }
        }
        Ok(())
    }

    /// Get all nodes in the database.
    pub fn get_all_nodes(&self) -> Result<Vec<Node>> {
        let mut stmt = self.db.conn().prepare_cached("SELECT * FROM nodes")?;
        let rows = stmt.query_map([], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by exact name match (uses idx_nodes_name index).
    pub fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE name = ?")?;
        let rows = stmt.query_map([name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by exact qualified name match (uses idx_nodes_qualified_name index).
    pub fn get_nodes_by_qualified_name_exact(&self, qualified_name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE qualified_name = ?")?;
        let rows = stmt.query_map([qualified_name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get nodes by lowercase name match (uses idx_nodes_lower_name expression index).
    pub fn get_nodes_by_lower_name(&self, lower_name: &str) -> Result<Vec<Node>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM nodes WHERE lower(name) = ?")?;
        let rows = stmt.query_map([lower_name], node_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Search nodes by name using FTS with fallback to LIKE for better matching.
    ///
    /// Search strategy:
    /// 1. Try FTS5 prefix match (query*) for word-start matching
    /// 2. If no results, try LIKE for substring matching (e.g., "signIn" finds "signInWithGoogle")
    /// 3. Score results based on match quality
    pub fn search_nodes(&self, query: &str, options: &SearchOptions) -> Result<Vec<SearchResult>> {
        let limit = options.limit.unwrap_or(100);
        let offset = options.offset.unwrap_or(0);

        // Parse field-qualified bits out of the raw query (kind:, lang:,
        // path:, name:). Anything not recognised stays in `text` and goes
        // to FTS unchanged. Filters compose with the SearchOptions arg —
        // both are applied (intersection-style).
        let parsed = parse_query(query);
        let merged_kinds = match intersect_filter_axis(options.kinds.as_deref(), &parsed.kinds) {
            Ok(filters) => filters,
            Err(()) => return Ok(Vec::new()),
        };
        let merged_languages =
            match intersect_filter_axis(options.languages.as_deref(), &parsed.languages) {
                Ok(filters) => filters,
                Err(()) => return Ok(Vec::new()),
            };
        let path_filters = &parsed.path_filters;
        let name_filters = &parsed.name_filters;
        // The text portion drives FTS/LIKE; if all the user typed was
        // filters (`kind:function`), we still need *some* candidate set,
        // so synthesise an empty-text path that returns everything matching
        // the filters.
        let text = parsed.text.as_str();
        let kinds = merged_kinds.as_deref();
        let languages = merged_languages.as_deref();
        let exhaustive_candidates = !path_filters.is_empty() || !name_filters.is_empty();
        let fetch_limit = limit + offset;

        // First try FTS5 with prefix matching
        let mut results = if !text.is_empty() {
            self.search_nodes_fts(
                text,
                kinds,
                languages,
                fetch_limit,
                0,
                exhaustive_candidates,
            )
        } else {
            // Over-fetch by 5× when running filter-only (no text). The
            // post-scoring path: + name: filters can be very selective, so
            // a smaller multiplier risks returning fewer than `limit`
            // results despite the DB having plenty of matches.
            self.search_all_by_filters(kinds, languages, fetch_limit * 5, exhaustive_candidates)?
        };

        // If no FTS results, try LIKE-based substring search
        if results.is_empty() && text.chars().count() >= 2 {
            results = self.search_nodes_like(
                text,
                kinds,
                languages,
                fetch_limit,
                0,
                exhaustive_candidates,
            )?;
        }

        // Final fuzzy fallback: scan all known names and keep those within
        // a tight Levenshtein distance. Only fires when both FTS and LIKE
        // returned nothing AND there's a text portion long enough to be
        // worth fuzzing (1-char queries would match too much).
        if results.is_empty() && text.chars().count() >= 3 {
            results = self.search_nodes_fuzzy(
                text,
                kinds,
                languages,
                fetch_limit,
                exhaustive_candidates,
            )?;
        }

        // Supplement: ensure exact name matches are always candidates.
        // BM25 can bury short exact-match names (e.g. "getBean") under hundreds
        // of compound names (e.g. "getBeanDescriptor") in large codebases,
        // pushing them past the FTS fetch limit before post-hoc scoring can
        // help. Use the max BM25 score as the base so the name_match_bonus
        // (exact=30 vs prefix=20) actually differentiates them after rescoring.
        if !results.is_empty() && !text.is_empty() {
            let mut existing_ids: HashSet<String> =
                results.iter().map(|r| r.node.id.clone()).collect();
            let max_fts_score = results
                .iter()
                .map(|r| r.score)
                .fold(f64::NEG_INFINITY, f64::max);
            let terms: Vec<&str> = text
                .split_whitespace()
                .filter(|t| t.chars().count() >= 2)
                .collect();
            for term in terms {
                let mut sql = String::from("SELECT * FROM nodes WHERE name = ? COLLATE NOCASE");
                let mut params: Vec<Value> = vec![Value::Text(term.to_string())];
                push_kind_filter(&mut sql, &mut params, "", kinds);
                push_language_filter(&mut sql, &mut params, "", languages);
                sql.push_str(" LIMIT 20");
                let mut stmt = self.db.conn().prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
                for row in rows {
                    let node = row?;
                    if !existing_ids.contains(&node.id) {
                        existing_ids.insert(node.id.clone());
                        results.push(SearchResult {
                            node,
                            score: max_fts_score,
                            highlights: None,
                        });
                    }
                }
            }
        }

        // Apply multi-signal scoring
        if !results.is_empty() && (!text.is_empty() || !query.is_empty()) {
            let scoring_query = if !text.is_empty() { text } else { query };
            for r in results.iter_mut() {
                r.score = r.score
                    + f64::from(kind_bonus(r.node.kind))
                    + f64::from(score_path_relevance(&r.node.file_path, scoring_query))
                    + f64::from(name_match_bonus(&r.node.name, scoring_query));
            }
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        // Apply path: + name: filters AFTER scoring. Scoring already uses
        // path/name as a soft signal; the explicit filters here are a hard
        // gate. Done last so the FTS limit fetched plenty of candidates to
        // narrow from.
        if !path_filters.is_empty() {
            let lowered: Vec<String> = path_filters.iter().map(|p| p.to_lowercase()).collect();
            results.retain(|r| {
                let fp = r.node.file_path.to_lowercase();
                lowered.iter().any(|p| fp.contains(p))
            });
        }
        if !name_filters.is_empty() {
            let lowered: Vec<String> = name_filters.iter().map(|n| n.to_lowercase()).collect();
            results.retain(|r| {
                let nm = r.node.name.to_lowercase();
                lowered.iter().any(|n| nm.contains(n))
            });
        }

        Ok(results.into_iter().skip(offset).take(limit).collect())
    }

    /// Match-everything path used when the user supplied only field
    /// filters (`kind:function lang:typescript`) with no text. Returns
    /// candidates ordered by name; the caller's filter pass narrows to
    /// what was asked for.
    fn search_all_by_filters(
        &self,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE 1=1");
        let mut params: Vec<Value> = Vec::new();
        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);
        sql.push_str(" ORDER BY name");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }
        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(SearchResult {
                node: row?,
                score: 1.0,
                highlights: None,
            });
        }
        Ok(out)
    }

    /// Fuzzy fallback: when zero FTS/LIKE hits, try an edit-distance
    /// sweep over the distinct symbol-name set. Caps `max_dist` at 2 so
    /// `getUssr` finds `getUser` but `process` doesn't match `prosody`.
    /// Bounded edit distance keeps each comparison cheap; the per-query
    /// scan is O(distinct-name-count) which is far smaller than total
    /// node count on any real codebase.
    fn search_nodes_fuzzy(
        &self,
        text: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let lowered = text.to_lowercase();
        let max_dist = if lowered.chars().count() <= 4 { 1 } else { 2 };

        // Pull the distinct name list once. Even on a 200k-node project the
        // distinct name set is typically O(10k) because most names repeat.
        // The candidate-cap below bounds memory regardless.
        let all_names = self.get_all_node_names()?;
        let mut candidates: Vec<(String, usize)> = Vec::new();
        for name in all_names {
            let dist = bounded_edit_distance(&name.to_lowercase(), &lowered, max_dist);
            if dist <= max_dist {
                candidates.push((name, dist));
            }
        }
        candidates.sort_by_key(|c| c.1);

        // Cap the per-name follow-up queries. Each survivor triggers a
        // separate `SELECT * FROM nodes WHERE name = ?`; without this cap
        // a project with many similar names (`getUser1`, `getUser2`...)
        // could fan out far beyond `limit` queries before the inner-loop
        // limit kicks in.
        let followup_cap = if exhaustive_candidates {
            candidates.len()
        } else {
            (limit * 2).max(50)
        };
        candidates.truncate(followup_cap);

        let mut results: Vec<SearchResult> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (name, dist) in candidates {
            if !exhaustive_candidates && results.len() >= limit {
                break;
            }
            let mut sql = String::from("SELECT * FROM nodes WHERE name = ?");
            let mut params: Vec<Value> = vec![Value::Text(name)];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            push_language_filter(&mut sql, &mut params, "", languages);
            if !exhaustive_candidates {
                sql.push_str(" LIMIT 5");
            }
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
            for row in rows {
                let node = row?;
                if seen.contains(&node.id) {
                    continue;
                }
                seen.insert(node.id.clone());
                // Lower the score for each edit step away from the query so
                // exact-match fallbacks (dist 0) outrank dist-2 typos.
                results.push(SearchResult {
                    node,
                    score: 1.0 / (1.0 + dist as f64),
                    highlights: None,
                });
                if !exhaustive_candidates && results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    /// FTS5 search with prefix matching.
    fn search_nodes_fts(
        &self,
        query: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        offset: usize,
        exhaustive_candidates: bool,
    ) -> Vec<SearchResult> {
        // Add prefix wildcard for better matching (e.g., "auth" matches
        // "AuthService", "authenticate"). Escape special FTS5 characters
        // and add prefix wildcard.
        //
        // `::` is a qualifier separator in Rust/C++/Ruby, not a token char,
        // so treat it as whitespace before the strip step. Otherwise queries
        // like `stage_apply::run` collapse to `stage_applyrun` (the colons
        // are stripped without splitting) and find nothing. See #173.
        let cleaned: String = query
            .replace("::", " ") // Rust/C++/Ruby qualifier separator
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '*' | '(' | ')' | ':' | '^')) // FTS5 special chars
            .collect();
        let fts_query = cleaned
            .split_whitespace()
            .filter(|term| !term.is_empty())
            // Strip FTS5 boolean operators to prevent query manipulation
            .filter(|term| !is_fts_operator(term))
            .map(|term| format!("\"{term}\"*")) // Prefix match each term
            .collect::<Vec<_>>()
            .join(" OR ");

        if fts_query.is_empty() {
            return Vec::new();
        }

        // BM25 column weights: id=0, name=20, qualified_name=5, docstring=1, signature=2
        // Heavy name weight ensures exact/prefix name matches rank above incidental
        // mentions in long docstrings or qualified names of nested symbols.
        // Fetch 5x requested limit so post-hoc rescoring (kind_bonus, path
        // relevance, name_match_bonus) can promote results that BM25 alone
        // undervalues.
        let fts_limit = (limit * 5).max(100);

        let mut sql = String::from(
            "SELECT nodes.*, bm25(nodes_fts, 0, 20, 5, 1, 2) as score
             FROM nodes_fts
             JOIN nodes ON nodes_fts.id = nodes.id
             WHERE nodes_fts MATCH ?",
        );
        let mut params: Vec<Value> = vec![Value::Text(fts_query)];
        push_kind_filter(&mut sql, &mut params, "nodes.", kinds);
        push_language_filter(&mut sql, &mut params, "nodes.", languages);
        sql.push_str(" ORDER BY score");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(Value::Integer(fts_limit as i64));
            params.push(Value::Integer(offset as i64));
        }

        // FTS query failed → return empty (TS try/catch parity)
        let run = || -> Result<Vec<SearchResult>> {
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| {
                let node = node_from_row(row)?;
                let score: f64 = row.get("score")?;
                Ok(SearchResult {
                    node,
                    score: score.abs(), // bm25 returns negative scores
                    highlights: None,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        };
        run().unwrap_or_default()
    }

    /// LIKE-based substring search for cases where FTS doesn't match.
    /// Useful for camelCase matching (e.g., "signIn" finds "signInWithGoogle").
    fn search_nodes_like(
        &self,
        query: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        offset: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            "SELECT nodes.*,
               CASE
                 WHEN name = ? THEN 1.0
                 WHEN name LIKE ? THEN 0.9
                 WHEN name LIKE ? THEN 0.8
                 WHEN qualified_name LIKE ? THEN 0.7
                 ELSE 0.5
               END as score
             FROM nodes
             WHERE (
               name LIKE ? OR
               qualified_name LIKE ? OR
               name LIKE ?
             )",
        );

        // Pattern variants for better matching
        let exact_match = query.to_string();
        let starts_with = format!("{query}%");
        let contains = format!("%{query}%");

        let mut params: Vec<Value> = vec![
            Value::Text(exact_match),         // Exact match score
            Value::Text(starts_with.clone()), // Starts with score
            Value::Text(contains.clone()),    // Contains score
            Value::Text(contains.clone()),    // Qualified name score
            Value::Text(contains.clone()),    // WHERE: name contains
            Value::Text(contains),            // WHERE: qualified_name contains
            Value::Text(starts_with),         // WHERE: name starts with
        ];

        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);

        sql.push_str(" ORDER BY score DESC, length(name) ASC");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(Value::Integer(limit as i64));
            params.push(Value::Integer(offset as i64));
        }

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let node = node_from_row(row)?;
            let score: f64 = row.get("score")?;
            Ok(SearchResult {
                node,
                score,
                highlights: None,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Find nodes by exact name match.
    ///
    /// Used for hybrid search — looks up symbols by exact name or
    /// case-insensitive match. Returns high-confidence matches for known
    /// symbol names extracted from a query.
    pub fn find_nodes_by_exact_name(
        &self,
        names: &[String],
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let kinds = options.kinds.as_deref();
        let languages = options.languages.as_deref();
        let limit = options.limit.unwrap_or(50);

        // Two-pass approach to handle common names (e.g., "run" has 40+ matches):
        // Pass 1: Find which files contain distinctive (rare) symbols from the query.
        // Pass 2: Query each name, boosting results that co-locate with distinctive symbols.

        // Pass 1: Find files containing each queried name, identify distinctive names
        let mut name_to_files: Vec<(String, HashSet<String>)> = Vec::new();
        for name in names {
            let mut sql =
                String::from("SELECT DISTINCT file_path FROM nodes WHERE name COLLATE NOCASE = ?");
            let mut params: Vec<Value> = vec![Value::Text(name.clone())];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            sql.push_str(" LIMIT 100");
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0))?;
            let mut files = HashSet::new();
            for row in rows {
                files.insert(row?);
            }
            name_to_files.push((name.to_lowercase(), files));
        }

        // Distinctive names are those with fewer than 10 file matches
        // (e.g., "scrapeLoop" = 1 file)
        let mut distinctive_files: HashSet<String> = HashSet::new();
        for (_, files) in &name_to_files {
            if !files.is_empty() && files.len() < 10 {
                for f in files {
                    distinctive_files.insert(f.clone());
                }
            }
        }

        // Pass 2: Query each name with per-name limit, scoring by co-location
        let per_name_limit = 8usize.max(limit.div_ceil(names.len()));
        let mut all_results: Vec<SearchResult> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        for name in names {
            let mut sql = String::from(
                "SELECT nodes.*, 1.0 as score
                 FROM nodes
                 WHERE name COLLATE NOCASE = ?",
            );
            let mut params: Vec<Value> = vec![Value::Text(name.clone())];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            push_language_filter(&mut sql, &mut params, "", languages);

            // Fetch enough to find co-located results among common names
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer((per_name_limit * 3).max(50) as i64));

            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| {
                let node = node_from_row(row)?;
                let score: f64 = row.get("score")?;
                Ok((node, score))
            })?;
            let mut name_results: Vec<SearchResult> = Vec::new();
            for row in rows {
                let (node, score) = row?;
                if seen_ids.contains(&node.id) {
                    continue;
                }
                // Boost results in files that also contain distinctive symbols
                let co_location_boost = if distinctive_files.contains(&node.file_path) {
                    20.0
                } else {
                    0.0
                };
                name_results.push(SearchResult {
                    node,
                    score: score + co_location_boost,
                    highlights: None,
                });
            }

            // Sort by score (co-located first), take per-name limit
            name_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for r in name_results.into_iter().take(per_name_limit) {
                seen_ids.insert(r.node.id.clone());
                all_results.push(r);
            }
        }

        // Sort all results by score so co-located results bubble up
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(limit);
        Ok(all_results)
    }

    /// Find nodes whose name contains a substring (LIKE-based).
    /// Useful for CamelCase-part matching where FTS fails because
    /// e.g. "TransportSearchAction" is one FTS token, not matchable by "Search"*.
    ///
    /// Results are ordered by name length (shorter = more likely to be the core type).
    /// `exclude_prefix` excludes prefix matches (handled by FTS-based prefix search).
    pub fn find_nodes_by_name_substring(
        &self,
        substring: &str,
        options: &SearchOptions,
        exclude_prefix: bool,
    ) -> Result<Vec<SearchResult>> {
        let kinds = options.kinds.as_deref();
        let languages = options.languages.as_deref();
        let limit = options.limit.unwrap_or(30);

        let mut sql = String::from(
            "SELECT nodes.*, 1.0 as score
             FROM nodes
             WHERE name LIKE ?",
        );
        let mut params: Vec<Value> = vec![Value::Text(format!("%{substring}%"))];

        // Exclude prefix matches (handled by FTS-based prefix search in Step 2b)
        if exclude_prefix {
            sql.push_str(" AND name NOT LIKE ?");
            params.push(Value::Text(format!("{substring}%")));
        }

        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);

        sql.push_str(" ORDER BY length(name) ASC LIMIT ?");
        params.push(Value::Integer(limit as i64));

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let node = node_from_row(row)?;
            let score: f64 = row.get("score")?;
            Ok(SearchResult {
                node,
                score,
                highlights: None,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // =========================================================================
    // Edge Operations
    // =========================================================================

    /// Insert a new edge.
    pub fn insert_edge(&self, edge: &Edge) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance)
             VALUES (@source, @target, @kind, @metadata, @line, @col, @provenance)",
        )?;
        let metadata: Option<String> = match &edge.metadata {
            Some(m) => Some(serde_json::to_string(m)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@source": edge.source,
            "@target": edge.target,
            "@kind": edge.kind.as_str(),
            "@metadata": metadata,
            "@line": edge.line,
            "@col": edge.column,
            "@provenance": edge.provenance.map(|p| p.as_str()),
        })?;
        Ok(())
    }

    /// Insert multiple edges in a transaction.
    /// Edges whose endpoints don't exist in the DB are skipped — endpoint
    /// existence is validated from the database, not the (possibly stale)
    /// node cache.
    pub fn insert_edges(&self, edges: &[Edge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        self.db.transaction(|| {
            let mut endpoint_ids: Vec<&String> = Vec::new();
            let mut seen: HashSet<&str> = HashSet::new();
            for edge in edges {
                if seen.insert(edge.source.as_str()) {
                    endpoint_ids.push(&edge.source);
                }
                if seen.insert(edge.target.as_str()) {
                    endpoint_ids.push(&edge.target);
                }
            }
            let existing_node_ids = self.get_existing_node_ids(&endpoint_ids)?;

            for edge in edges {
                if !existing_node_ids.contains(&edge.source)
                    || !existing_node_ids.contains(&edge.target)
                {
                    continue;
                }
                self.insert_edge(edge)?;
            }
            Ok(())
        })
    }

    /// Delete all edges from a source node.
    pub fn delete_edges_by_source(&self, source_id: &str) -> Result<()> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM edges WHERE source = ?")?;
        stmt.execute([source_id])?;
        Ok(())
    }

    /// Get outgoing edges from a node.
    pub fn get_outgoing_edges(
        &self,
        source_id: &str,
        kinds: Option<&[EdgeKind]>,
        provenance: Option<&str>,
    ) -> Result<Vec<Edge>> {
        let has_kinds = kinds.map(|k| !k.is_empty()).unwrap_or(false);
        if has_kinds || provenance.is_some() {
            let mut sql = String::from("SELECT * FROM edges WHERE source = ?");
            let mut params: Vec<Value> = vec![Value::Text(source_id.to_string())];

            push_edge_kind_filter(&mut sql, &mut params, kinds);

            if let Some(p) = provenance {
                sql.push_str(" AND provenance = ?");
                params.push(Value::Text(p.to_string()));
            }

            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
            return rows.map(|r| r.map_err(Into::into)).collect();
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM edges WHERE source = ?")?;
        let rows = stmt.query_map([source_id], edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get outgoing edges from multiple source nodes in one query.
    pub fn get_outgoing_edges_for_sources(
        &self,
        source_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(source_ids)?;
        let mut sql =
            String::from("SELECT * FROM edges WHERE source IN (SELECT value FROM json_each(?))");
        let mut params: Vec<Value> = vec![Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get incoming edges to a node.
    pub fn get_incoming_edges(
        &self,
        target_id: &str,
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        let has_kinds = kinds.map(|k| !k.is_empty()).unwrap_or(false);
        if has_kinds {
            let mut sql = String::from("SELECT * FROM edges WHERE target = ?");
            let mut params: Vec<Value> = vec![Value::Text(target_id.to_string())];
            push_edge_kind_filter(&mut sql, &mut params, kinds);
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
            return rows.map(|r| r.map_err(Into::into)).collect();
        }

        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM edges WHERE target = ?")?;
        let rows = stmt.query_map([target_id], edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get incoming edges to multiple target nodes in one query.
    pub fn get_incoming_edges_for_targets(
        &self,
        target_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if target_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(target_ids)?;
        let mut sql =
            String::from("SELECT * FROM edges WHERE target IN (SELECT value FROM json_each(?))");
        let mut params: Vec<Value> = vec![Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Find all edges where both source and target are in the given node set.
    /// Useful for recovering inter-node connectivity after BFS.
    pub fn find_edges_between_nodes(
        &self,
        node_ids: &[String],
        kinds: Option<&[EdgeKind]>,
    ) -> Result<Vec<Edge>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids_json = serde_json::to_string(node_ids)?;
        let mut sql = String::from(
            "SELECT * FROM edges WHERE source IN (SELECT value FROM json_each(?)) AND target IN (SELECT value FROM json_each(?))",
        );
        let mut params: Vec<Value> = vec![Value::Text(ids_json.clone()), Value::Text(ids_json)];
        push_edge_kind_filter(&mut sql, &mut params, kinds);

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), edge_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    // =========================================================================
    // File Operations
    // =========================================================================

    /// Insert or update a file record.
    pub fn upsert_file(&self, file: &FileRecord) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
             VALUES (@path, @contentHash, @language, @size, @modifiedAt, @indexedAt, @nodeCount, @errors)
             ON CONFLICT(path) DO UPDATE SET
               content_hash = @contentHash,
               language = @language,
               size = @size,
               modified_at = @modifiedAt,
               indexed_at = @indexedAt,
               node_count = @nodeCount,
               errors = @errors",
        )?;
        let errors: Option<String> = match &file.errors {
            Some(e) => Some(serde_json::to_string(e)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@path": file.path,
            "@contentHash": file.content_hash,
            "@language": file.language.as_str(),
            "@size": file.size as i64,
            "@modifiedAt": file.modified_at,
            "@indexedAt": file.indexed_at,
            "@nodeCount": file.node_count,
            "@errors": errors,
        })?;
        Ok(())
    }

    /// Delete a file record and its nodes.
    pub fn delete_file(&self, file_path: &str) -> Result<()> {
        self.db.transaction(|| {
            self.delete_nodes_by_file(file_path)?;
            let mut stmt = self
                .db
                .conn()
                .prepare_cached("DELETE FROM files WHERE path = ?")?;
            stmt.execute([file_path])?;
            Ok(())
        })
    }

    /// Get a file record by path.
    pub fn get_file_by_path(&self, file_path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM files WHERE path = ?")?;
        let mut rows = stmt.query([file_path])?;
        match rows.next()? {
            Some(row) => Ok(Some(file_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Get all tracked files.
    pub fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM files ORDER BY path")?;
        let rows = stmt.query_map([], file_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Most recent index timestamp (ms since epoch) across all tracked files, or
    /// None when nothing is indexed yet. One indexed aggregate, no per-row scan. (#329)
    pub fn get_last_indexed_at(&self) -> Result<Option<i64>> {
        let last: Option<i64> =
            self.db
                .conn()
                .query_row("SELECT MAX(indexed_at) AS last FROM files", [], |row| {
                    // Lenient like `lenient_i64`: a TS-written database may
                    // hold REAL timestamps (fractional mtimeMs-style numbers).
                    use rusqlite::types::ValueRef;
                    match row.get_ref(0)? {
                        ValueRef::Null => Ok(None),
                        ValueRef::Integer(i) => Ok(Some(i)),
                        ValueRef::Real(f) => Ok(Some(f as i64)),
                        other => Err(rusqlite::Error::FromSqlConversionFailure(
                            0,
                            other.data_type(),
                            "expected INTEGER or REAL for MAX(indexed_at)".into(),
                        )),
                    }
                })?;
        Ok(last)
    }

    /// Get files that need re-indexing (hash changed).
    pub fn get_stale_files(
        &self,
        current_hashes: &HashMap<String, String>,
    ) -> Result<Vec<FileRecord>> {
        let files = self.get_all_files()?;
        Ok(files
            .into_iter()
            .filter(|f| {
                current_hashes
                    .get(&f.path)
                    .map(|h| !h.is_empty() && h != &f.content_hash)
                    .unwrap_or(false)
            })
            .collect())
    }

    // =========================================================================
    // Unresolved References
    // =========================================================================

    /// Insert an unresolved reference.
    pub fn insert_unresolved_ref(&self, reference: &UnresolvedReference) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language)
             VALUES (@fromNodeId, @referenceName, @referenceKind, @line, @col, @candidates, @filePath, @language)",
        )?;
        let candidates: Option<String> = match &reference.candidates {
            Some(c) => Some(serde_json::to_string(c)?),
            None => None,
        };
        stmt.execute(rusqlite::named_params! {
            "@fromNodeId": reference.from_node_id,
            "@referenceName": reference.reference_name,
            "@referenceKind": reference.reference_kind.as_str(),
            "@line": reference.line,
            "@col": reference.column,
            "@candidates": candidates,
            "@filePath": reference.file_path.as_deref().unwrap_or(""),
            "@language": reference.language.unwrap_or(Language::Unknown).as_str(),
        })?;
        Ok(())
    }

    /// Insert multiple unresolved references in a transaction.
    pub fn insert_unresolved_refs_batch(&self, refs: &[UnresolvedReference]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.db.transaction(|| {
            for reference in refs {
                self.insert_unresolved_ref(reference)?;
            }
            Ok(())
        })
    }

    /// Delete unresolved references from a node.
    pub fn delete_unresolved_by_node(&self, node_id: &str) -> Result<()> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("DELETE FROM unresolved_refs WHERE from_node_id = ?")?;
        stmt.execute([node_id])?;
        Ok(())
    }

    /// Get unresolved references by name (for resolution).
    pub fn get_unresolved_by_name(&self, name: &str) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs WHERE reference_name = ?")?;
        let rows = stmt.query_map([name], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all unresolved references.
    pub fn get_unresolved_references(&self) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs")?;
        let rows = stmt.query_map([], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get the count of unresolved references without loading them into memory.
    pub fn get_unresolved_references_count(&self) -> Result<u64> {
        let count: i64 = self.db.conn().query_row(
            "SELECT COUNT(*) as count FROM unresolved_refs",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Get a batch of unresolved references using LIMIT/OFFSET pagination.
    /// Used to process references in bounded memory chunks.
    pub fn get_unresolved_references_batch(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs LIMIT ? OFFSET ?")?;
        let rows = stmt.query_map([limit as i64, offset as i64], unresolved_from_row)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get unresolved references after a stable row id. This lets full-index
    /// resolution scan each currently-unresolved row once while keeping unresolved
    /// rows for future target-side sync repair.
    pub fn get_unresolved_references_batch_after_id(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<UnresolvedBatch> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT * FROM unresolved_refs WHERE id > ? ORDER BY id LIMIT ?")?;
        let rows = stmt.query_map([after_id, limit as i64], |row| {
            let id: i64 = row.get("id")?;
            let reference = unresolved_from_row(row)?;
            Ok((id, reference))
        })?;
        let mut refs = Vec::new();
        let mut last_id = after_id;
        for row in rows {
            let (id, reference) = row?;
            last_id = id;
            refs.push(reference);
        }
        Ok(UnresolvedBatch { refs, last_id })
    }

    /// Get all tracked file paths (lightweight — no full FileRecord objects).
    pub fn get_all_file_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT path FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get all distinct node names (lightweight — just name strings for pre-filtering).
    pub fn get_all_node_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT DISTINCT name FROM nodes")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    /// Get unresolved references scoped to specific file paths.
    /// Uses the idx_unresolved_file_path index for efficient lookup.
    pub fn get_unresolved_references_by_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<UnresolvedReference>> {
        if file_paths.is_empty() {
            return Ok(Vec::new());
        }

        // Chunk under SQLite's parameter limit: the first sync of a very large
        // repo passes every changed file here, which an unbounded `IN (...)`
        // would bind as one parameter each — exceeding MAX_VARIABLE_NUMBER and
        // aborting with "too many SQL variables". (#540)
        let mut out = Vec::new();
        for chunk in file_paths.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE file_path IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                unresolved_from_row,
            )?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Get unresolved references whose target name may have changed.
    /// Used by incremental sync to repair refs in unchanged files after a new or
    /// modified file introduces a previously-missing symbol.
    pub fn get_unresolved_references_by_names(
        &self,
        names: &[String],
    ) -> Result<Vec<UnresolvedReference>> {
        let mut unique_names: Vec<&String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for name in names {
            if !name.is_empty() && seen.insert(name.as_str()) {
                unique_names.push(name);
            }
        }
        if unique_names.is_empty() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for chunk in unique_names.chunks(SQLITE_PARAM_CHUNK_SIZE) {
            let sql = format!(
                "SELECT * FROM unresolved_refs WHERE reference_name IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(chunk.iter().map(|s| s.as_str())),
                unresolved_from_row,
            )?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// Delete all unresolved references (after resolution).
    pub fn clear_unresolved_references(&self) -> Result<()> {
        self.db.exec("DELETE FROM unresolved_refs")
    }

    /// Delete resolved references by their IDs.
    pub fn delete_resolved_references(&self, from_node_ids: &[String]) -> Result<()> {
        if from_node_ids.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "DELETE FROM unresolved_refs WHERE from_node_id IN ({})",
            placeholders(from_node_ids.len())
        );
        let mut stmt = self.db.conn().prepare(&sql)?;
        stmt.execute(params_from_iter(from_node_ids.iter().map(|s| s.as_str())))?;
        Ok(())
    }

    /// Delete specific resolved references by (from_node_id, reference_name,
    /// reference_kind) tuples. More precise than
    /// [`Self::delete_resolved_references`] — only removes refs that were
    /// actually resolved.
    pub fn delete_specific_resolved_references(&self, refs: &[ResolvedRefKey]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.db.transaction(|| {
            let mut stmt = self.db.conn().prepare_cached(
                "DELETE FROM unresolved_refs WHERE from_node_id = ? AND reference_name = ? AND reference_kind = ?",
            )?;
            for r in refs {
                stmt.execute([
                    r.from_node_id.as_str(),
                    r.reference_name.as_str(),
                    r.reference_kind.as_str(),
                ])?;
            }
            Ok(())
        })
    }

    // =========================================================================
    // Statistics
    // =========================================================================

    /// Lightweight (nodes, edges) count snapshot. Used around an index/sync
    /// run to compute true additions across extraction + resolution +
    /// synthesis — the per-phase counter in the orchestrator only sees
    /// extraction's contribution, which is why the CLI summary under-reported
    /// the edge count (resolution + synthesizer edges were invisible).
    pub fn get_node_and_edge_count(&self) -> Result<NodeEdgeCount> {
        let (nodes, edges): (i64, i64) = self.db.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM nodes) AS nodes, (SELECT COUNT(*) FROM edges) AS edges",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(NodeEdgeCount {
            nodes: nodes.max(0) as u64,
            edges: edges.max(0) as u64,
        })
    }

    /// Get graph statistics.
    pub fn get_stats(&self) -> Result<GraphStats> {
        // Single query for all three aggregate counts
        let (node_count, edge_count, file_count): (i64, i64, i64) = self.db.conn().query_row(
            "SELECT
                   (SELECT COUNT(*) FROM nodes) AS node_count,
                   (SELECT COUNT(*) FROM edges) AS edge_count,
                   (SELECT COUNT(*) FROM files) AS file_count",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let group_count = |sql: &str| -> Result<HashMap<String, u64>> {
            let mut stmt = self.db.conn().prepare_cached(sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (key, count) = row?;
                out.insert(key, count);
            }
            Ok(out)
        };

        let nodes_by_kind = group_count("SELECT kind, COUNT(*) as count FROM nodes GROUP BY kind")?;
        let edges_by_kind = group_count("SELECT kind, COUNT(*) as count FROM edges GROUP BY kind")?;
        let files_by_language =
            group_count("SELECT language, COUNT(*) as count FROM files GROUP BY language")?;

        Ok(GraphStats {
            node_count: node_count.max(0) as u64,
            edge_count: edge_count.max(0) as u64,
            file_count: file_count.max(0) as u64,
            nodes_by_kind,
            edges_by_kind,
            files_by_language,
            db_size_bytes: 0, // Set by caller using DatabaseConnection::get_size()
            last_updated: now_ms(),
        })
    }

    // =========================================================================
    // Project Metadata
    // =========================================================================

    /// Get a metadata value by key.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT value FROM project_metadata WHERE key = ?")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Set a metadata key-value pair (upsert).
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT INTO project_metadata (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )?;
        stmt.execute(rusqlite::params![key, value, now_ms()])?;
        Ok(())
    }

    /// Get all metadata as a key-value map.
    pub fn get_all_metadata(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .db
            .conn()
            .prepare_cached("SELECT key, value FROM project_metadata")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (key, value) = row?;
            out.insert(key, value);
        }
        Ok(out)
    }

    /// Clear all data from the database.
    pub fn clear(&self) -> Result<()> {
        self.node_cache.borrow_mut().clear();
        self.db.transaction(|| {
            self.db.exec("DELETE FROM unresolved_refs")?;
            self.db.exec("DELETE FROM edges")?;
            self.db.exec("DELETE FROM nodes")?;
            self.db.exec("DELETE FROM files")?;
            Ok(())
        })
    }
}

// =============================================================================
// SQL filter helpers
// =============================================================================

fn unique_filters<T: Copy + Eq>(filters: &[T]) -> Vec<T> {
    let mut unique = Vec::new();
    for filter in filters {
        if !unique.contains(filter) {
            unique.push(*filter);
        }
    }
    unique
}

fn intersect_filter_axis<T: Copy + Eq>(
    option_filters: Option<&[T]>,
    query_filters: &[T],
) -> std::result::Result<Option<Vec<T>>, ()> {
    let option_filters = option_filters.filter(|filters| !filters.is_empty());
    let query_filters = if query_filters.is_empty() {
        None
    } else {
        Some(query_filters)
    };

    match (option_filters, query_filters) {
        (None, None) => Ok(None),
        (Some(filters), None) | (None, Some(filters)) => Ok(Some(unique_filters(filters))),
        (Some(options), Some(query)) => {
            let mut intersection = Vec::new();
            for filter in options {
                if query.contains(filter) && !intersection.contains(filter) {
                    intersection.push(*filter);
                }
            }
            if intersection.is_empty() {
                Err(())
            } else {
                Ok(Some(intersection))
            }
        }
    }
}

fn push_kind_filter(
    sql: &mut String,
    params: &mut Vec<Value>,
    col_prefix: &str,
    kinds: Option<&[NodeKind]>,
) {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            sql.push_str(&format!(
                " AND {}kind IN ({})",
                col_prefix,
                placeholders(kinds.len())
            ));
            for k in kinds {
                params.push(Value::Text(k.as_str().to_string()));
            }
        }
    }
}

fn push_language_filter(
    sql: &mut String,
    params: &mut Vec<Value>,
    col_prefix: &str,
    languages: Option<&[Language]>,
) {
    if let Some(languages) = languages {
        if !languages.is_empty() {
            sql.push_str(&format!(
                " AND {}language IN ({})",
                col_prefix,
                placeholders(languages.len())
            ));
            for l in languages {
                params.push(Value::Text(l.as_str().to_string()));
            }
        }
    }
}

fn push_edge_kind_filter(sql: &mut String, params: &mut Vec<Value>, kinds: Option<&[EdgeKind]>) {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
            for k in kinds {
                params.push(Value::Text(k.as_str().to_string()));
            }
        }
    }
}

// =============================================================================
// Inline helpers — defined inline in TS `src/db/queries.ts` as well (they are
// NOT part of the shared search/extraction modules).
// =============================================================================

/// TS `isLowValueFile` patterns, applied to the lowercased path.
static LOW_VALUE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?:^|/)(tests?|__tests?__|spec)/",
        r"_test\.go$",
        r"(?:^|/)test_[^/]+\.py$",
        r"_test\.py$",
        r"_spec\.rb$",
        r"_test\.rb$",
        r"\.(test|spec)\.[jt]sx?$",
        r"(test|spec|tests)\.(java|kt|scala)$",
        r"(tests?|spec)\.cs$",
        r"tests?\.swift$",
        r"_test\.dart$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});

/// Path-only heuristic for files that should not be candidates for
/// "dominant file" detection: test/spec files and tool-generated files.
/// Generated files (`*.pb.go`, `*.pulsar.go`, mock outputs, …) often
/// have huge in-file edge counts that dwarf the real source — etcd's
/// `rpc.pb.go` has 4× the in-file edges of `server.go`.
fn is_low_value_file(file_path: &str) -> bool {
    let lp = file_path.to_lowercase();
    LOW_VALUE_PATTERNS.iter().any(|p| p.is_match(&lp)) || is_generated_file(file_path)
}

static FTS_OPERATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(AND|OR|NOT|NEAR)$").expect("static regex"));

/// Whether a term is an FTS5 boolean operator (stripped to prevent query
/// manipulation) — TS inline `/^(AND|OR|NOT|NEAR)$/i`.
fn is_fts_operator(term: &str) -> bool {
    FTS_OPERATOR_RE.is_match(term)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    //! Coverage for the helpers defined inline in this file. The shared
    //! search/extraction helpers are tested where they live:
    //! `crate::search::query_parser`, `crate::search::query_utils`,
    //! `crate::extraction::generated_detection`.

    use super::{is_fts_operator, is_low_value_file};
    use crate::extraction::generated_detection::is_generated_file;

    #[test]
    fn low_value_and_generated_file_detection() {
        // Test/spec paths
        assert!(is_low_value_file("src/__tests__/foo.ts"));
        assert!(is_low_value_file("pkg/server_test.go"));
        assert!(is_low_value_file("lib/foo.spec.tsx"));
        // Generated protobuf stubs (the etcd rpc.pb.go case from the TS docs)
        assert!(is_low_value_file("api/etcdserverpb/rpc.pb.go"));
        assert!(is_generated_file("gen/types.pb.go"));
        assert!(is_generated_file("client_grpc_pb.js"));
        // Real source survives
        assert!(!is_low_value_file("server/etcdserver/server.go"));
        assert!(!is_generated_file("src/db/queries.rs"));
    }

    #[test]
    fn fts_operator_detection_case_insensitive() {
        assert!(is_fts_operator("AND"));
        assert!(is_fts_operator("or"));
        assert!(is_fts_operator("Near"));
        assert!(!is_fts_operator("Andrew"));
    }
}
