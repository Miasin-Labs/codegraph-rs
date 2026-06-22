//! Project resolution, cache, and worktree mismatch state.

use std::path::Path;
use std::rc::Rc;

use super::ToolHandler;
use crate::codegraph::CodeGraph;
use crate::directory::find_nearest_codegraph_root;
use crate::error::{CodeGraphError, Result};
use crate::sync::worktree::{WorktreeIndexMismatch, detect_worktree_index_mismatch};
use crate::utils::validate_project_path;

impl ToolHandler {
    pub(in crate::mcp::tools) fn get_code_graph(
        &self,
        project_path: Option<&str>,
    ) -> Result<Rc<CodeGraph>> {
        let Some(project_path) = project_path else {
            return match &*self.cg.borrow() {
                Some(cg) => Ok(Rc::clone(cg)),
                None => {
                    let searched =
                        self.default_project_hint
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| {
                                std::env::current_dir()
                                    .map(|p| p.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });
                    Err(CodeGraphError::other(format!(
                        "No CodeGraph project is loaded for this session.\nSearched for a .codegraph/ directory starting from: {searched}\nThe index is likely fine — this is a working-directory detection issue: the MCP client launched the server outside your project and didn't report the workspace root. Fix it either way:\n  • Pass projectPath to the tool call, e.g. projectPath: \"/absolute/path/to/your/project\"\n  • Or add --path to the server's MCP config args: [\"serve\", \"--mcp\", \"--path\", \"/absolute/path/to/your/project\"]"
                    )))
                }
            };
        };

        // Check cache first (using original path as key)
        if let Some(cg) = self.project_cache.borrow().get(project_path) {
            return Ok(Rc::clone(cg));
        }

        // Reject sensitive system directories before opening. Only validate a
        // path that actually exists — a nested or not-yet-created sub-path of
        // a real project must still be allowed to resolve UP to its
        // .codegraph/ root below (issue #238).
        let pp = Path::new(project_path);
        if pp.exists() {
            if let Some(path_error) = validate_project_path(pp) {
                return Err(CodeGraphError::other(path_error));
            }
        }

        // Walk up parent directories to find nearest .codegraph/
        let resolved_root = find_nearest_codegraph_root(pp).ok_or_else(|| {
            CodeGraphError::other(format!(
                "CodeGraph not initialized in {project_path}. Run 'codegraph init' in that project first."
            ))
        })?;

        // If the path resolves to the default project, reuse the already-open
        // default instance rather than opening a SECOND connection to the same
        // DB (issue #238). Deliberately not cached under projectPath — the
        // server owns and closes the default instance.
        if let Some(cg) = &*self.cg.borrow() {
            if cg.get_project_root() == resolved_root.as_path() {
                return Ok(Rc::clone(cg));
            }
        }

        // Check if we already have this resolved root cached (different path,
        // same project)
        let resolved_key = resolved_root.to_string_lossy().to_string();
        if let Some(cg) = self
            .project_cache
            .borrow()
            .get(&resolved_key)
            .map(Rc::clone)
        {
            self.project_cache
                .borrow_mut()
                .insert(project_path.to_string(), Rc::clone(&cg));
            return Ok(cg);
        }

        // Open and cache under both paths
        let cg = Rc::new(CodeGraph::open_sync(&resolved_root)?);
        self.project_cache
            .borrow_mut()
            .insert(resolved_key.clone(), Rc::clone(&cg));
        if project_path != resolved_key {
            self.project_cache
                .borrow_mut()
                .insert(project_path.to_string(), Rc::clone(&cg));
        }
        Ok(cg)
    }

    /// Close all cached project connections.
    pub fn close_all(&self) {
        // The same Rc may be cached under multiple paths; close() is idempotent.
        for cg in self.project_cache.borrow().values() {
            cg.close();
        }
        self.project_cache.borrow_mut().clear();
        self.worktree_mismatch_cache.borrow_mut().clear();
    }

    pub(in crate::mcp::tools) fn worktree_mismatch_for(
        &self,
        project_path: Option<&str>,
    ) -> Option<WorktreeIndexMismatch> {
        let start_path = project_path
            .map(String::from)
            .or_else(|| self.default_project_hint.borrow().clone())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        if let Some(cached) = self.worktree_mismatch_cache.borrow().get(&start_path) {
            return cached.clone();
        }

        let mismatch = match self.get_code_graph(project_path) {
            Ok(cg) => detect_worktree_index_mismatch(Path::new(&start_path), cg.get_project_root()),
            // No resolvable project → nothing to warn.
            Err(_) => None,
        };
        self.worktree_mismatch_cache
            .borrow_mut()
            .insert(start_path, mismatch.clone());
        mismatch
    }
}
