use super::super::QueryBuilder;
use super::super::rows::visibility_str;
use crate::db::connection::now_ms;
use crate::error::{Result, log_error};
use crate::types::Node;

impl QueryBuilder {
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
              start_byte, end_byte, address, size,
              docstring, signature, return_type, visibility,
              is_exported, is_async, is_static, is_abstract,
              decorators, type_parameters, updated_at
            ) VALUES (
              @id, @kind, @name, @qualifiedName, @filePath, @language,
              @startLine, @endLine, @startColumn, @endColumn,
              @startByte, @endByte, @address, @size,
              @docstring, @signature, @returnType, @visibility,
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
            "@address": node.address.map(|a| a as i64),
            "@size": node.size,
            "@docstring": node.docstring,
            "@signature": node.signature,
            "@returnType": node.return_type,
            "@visibility": node.visibility.map(visibility_str),
            "@isExported": node.is_exported.unwrap_or(false) as i64,
            "@isAsync": node.is_async.unwrap_or(false) as i64,
            "@isStatic": node.is_static.unwrap_or(false) as i64,
            "@isAbstract": node.is_abstract.unwrap_or(false) as i64,
            "@decorators": decorators,
            "@typeParameters": type_parameters,
            "@updatedAt": updated_at,
        })?;
        drop(stmt);
        if !matches!(
            node.kind,
            crate::types::NodeKind::File | crate::types::NodeKind::Import
        ) {
            self.insert_name_segments(&node.name)?;
        }
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
              address = @address,
              size = @size,
              docstring = @docstring,
              signature = @signature,
              return_type = @returnType,
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
            "@address": node.address.map(|a| a as i64),
            "@size": node.size,
            "@docstring": node.docstring,
            "@signature": node.signature,
            "@returnType": node.return_type,
            "@visibility": node.visibility.map(visibility_str),
            "@isExported": node.is_exported.unwrap_or(false) as i64,
            "@isAsync": node.is_async.unwrap_or(false) as i64,
            "@isStatic": node.is_static.unwrap_or(false) as i64,
            "@isAbstract": node.is_abstract.unwrap_or(false) as i64,
            "@decorators": decorators,
            "@typeParameters": type_parameters,
            "@updatedAt": updated_at,
        })?;
        drop(stmt);
        if !matches!(
            node.kind,
            crate::types::NodeKind::File | crate::types::NodeKind::Import
        ) {
            self.insert_name_segments(&node.name)?;
        }
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
}
