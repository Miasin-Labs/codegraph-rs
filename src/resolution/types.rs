//! Reference Resolution Types
//!
//! Types for the reference resolution system.
//! Ported from `src/resolution/types.ts`.
//!
//! Deviation from the TS file layout (documented in
//! `notes/resolution-types.md`): the data types that the TS
//! `ResolutionContext` referenced from sibling modules via type-only
//! imports — `AliasMap`/`AliasPattern` (path-aliases.ts), `GoModule`
//! (go-module.ts) and `WorkspacePackages` (workspace-packages.ts) — are
//! DEFINED here so this contract compiles standalone while those modules
//! are still being ported. The sibling modules should `pub use
//! super::types::{...}` for them and implement only their loader
//! functions against these shapes.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::{EdgeKind, Language, Metadata, Node, NodeKind};

// =============================================================================
// Unresolved / resolved references
// =============================================================================

/// An unresolved reference from extraction.
///
/// Unlike [`crate::types::UnresolvedReference`] (the persisted DB row,
/// where `file_path`/`language` are optional denormalizations), the
/// resolution pipeline's working type requires both — the resolver fills
/// them in from the source node before strategies run (see TS
/// `ReferenceResolver.resolveReferences`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedRef {
    /// ID of the source node containing the reference
    pub from_node_id: String,
    /// The name being referenced
    pub reference_name: String,
    /// Type of reference
    pub reference_kind: EdgeKind,
    /// Line where reference occurs
    pub line: u32,
    /// Column where reference occurs
    pub column: u32,
    /// File path where reference occurs
    pub file_path: String,
    /// Language of the source file
    pub language: Language,
    /// Possible qualified names it might resolve to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// How a reference was resolved (TS union
/// `'exact-match' | 'import' | 'qualified-name' | 'framework' | 'fuzzy' | 'instance-method' | 'file-path'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolvedBy {
    ExactMatch,
    Import,
    QualifiedName,
    Framework,
    Fuzzy,
    InstanceMethod,
    FilePath,
}

/// Runtime-iterable list of all resolution methods.
pub const RESOLVED_BY_METHODS: [ResolvedBy; 7] = [
    ResolvedBy::ExactMatch,
    ResolvedBy::Import,
    ResolvedBy::QualifiedName,
    ResolvedBy::Framework,
    ResolvedBy::Fuzzy,
    ResolvedBy::InstanceMethod,
    ResolvedBy::FilePath,
];

impl ResolvedBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolvedBy::ExactMatch => "exact-match",
            ResolvedBy::Import => "import",
            ResolvedBy::QualifiedName => "qualified-name",
            ResolvedBy::Framework => "framework",
            ResolvedBy::Fuzzy => "fuzzy",
            ResolvedBy::InstanceMethod => "instance-method",
            ResolvedBy::FilePath => "file-path",
        }
    }
}

impl fmt::Display for ResolvedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ResolvedBy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RESOLVED_BY_METHODS
            .iter()
            .find(|m| m.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown resolution method: {s}"))
    }
}

/// A resolved reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRef {
    /// Original unresolved reference
    pub original: UnresolvedRef,
    /// ID of the target node
    pub target_node_id: String,
    /// Confidence score (0-1)
    pub confidence: f64,
    /// How it was resolved
    pub resolved_by: ResolvedBy,
}

/// Statistics for a resolution attempt (TS inline `stats` object).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionStats {
    pub total: usize,
    pub resolved: usize,
    pub unresolved: usize,
    /// Count per resolution method, keyed by [`ResolvedBy::as_str`] values.
    pub by_method: HashMap<String, usize>,
}

/// Result of resolution attempt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionResult {
    /// Successfully resolved references
    pub resolved: Vec<ResolvedRef>,
    /// References that couldn't be resolved
    pub unresolved: Vec<UnresolvedRef>,
    /// Statistics
    pub stats: ResolutionStats,
}

// =============================================================================
// Sibling-module data types (defined here so the contract compiles standalone;
// path_aliases.rs / go_module.rs / workspace_packages.rs re-export these and
// implement the loaders)
// =============================================================================

/// A single alias pattern from `compilerOptions.paths`.
/// (TS: `AliasPattern` in `path-aliases.ts`.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasPattern {
    /// The literal prefix before `*` (or the whole pattern if no `*`).
    pub prefix: String,
    /// The literal suffix after `*` (almost always empty).
    pub suffix: String,
    /// Whether the pattern contains a `*` wildcard.
    pub has_wildcard: bool,
    /// Replacement templates. When `has_wildcard` is true, `*` in the
    /// replacement is filled with the captured wildcard portion of the
    /// import path. Stored relative to [`AliasMap::base_url`].
    /// tsconfig allows multiple targets per alias (priority order).
    pub replacements: Vec<String>,
}

/// Project import-path aliases from `tsconfig.json` / `jsconfig.json`.
/// (TS: `AliasMap` in `path-aliases.ts`.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AliasMap {
    /// Absolute path. The directory `compilerOptions.paths` is rooted at.
    pub base_url: PathBuf,
    /// Patterns ordered by specificity: longer prefix first, then literal-
    /// before-wildcard, so the resolver tries the most-specific match.
    pub patterns: Vec<AliasPattern>,
}

/// One Go module root discovered from a `go.mod` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoModuleRoot {
    /// The module path declared in `go.mod`, e.g. `github.com/example/myproject`
    pub module_path: String,
    /// Absolute path to the directory containing the `go.mod` file.
    pub root_dir: PathBuf,
}

impl GoModuleRoot {
    pub fn import_suffix<'a>(&self, import_path: &'a str) -> Option<&'a str> {
        if import_path == self.module_path {
            return Some("");
        }
        import_path
            .strip_prefix(&self.module_path)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

/// Go module info from `go.mod`.
/// (TS: `GoModule` in `go-module.ts`.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoModule {
    /// The module path declared in `go.mod`, e.g. `github.com/example/myproject`
    pub module_path: String,
    /// Absolute path to the directory containing the `go.mod` file.
    pub root_dir: PathBuf,
    /// All discovered module roots, ordered for longest module-prefix match.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub module_roots: Vec<GoModuleRoot>,
}

impl GoModule {
    pub fn primary_root(&self) -> GoModuleRoot {
        GoModuleRoot {
            module_path: self.module_path.clone(),
            root_dir: self.root_dir.clone(),
        }
    }

    pub fn matching_root(&self, import_path: &str) -> Option<GoModuleRoot> {
        for module_root in &self.module_roots {
            if module_root.import_suffix(import_path).is_some() {
                return Some(module_root.clone());
            }
        }
        let primary = self.primary_root();
        if primary.import_suffix(import_path).is_some() {
            Some(primary)
        } else {
            None
        }
    }
}

/// Monorepo workspace member packages.
/// (TS: `WorkspacePackages` in `workspace-packages.ts`.)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePackages {
    /// Member package `name` → directory relative to projectRoot (posix).
    /// First declaration wins — the loader checks for an existing key
    /// before inserting, so a `HashMap` preserves the TS `Map` semantics
    /// (lookups are by exact name; the longest-name-match consumer in
    /// `resolveWorkspaceImport` is order-independent).
    pub by_name: HashMap<String, String>,
    /// Bare package name → manifest-declared entry file. HarmonyOS ohpm
    /// modules commonly use `oh-package.json5` with `main: "Index.ets"`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub entry_by_name: HashMap<String, String>,
}

// =============================================================================
// Resolution context
// =============================================================================

/// Context for resolution - provides access to the graph.
///
/// TS interface → Rust trait (object-safe; held as `&dyn ResolutionContext`
/// by framework resolvers). The TS-optional methods
/// (`getProjectAliases?` … `getCppIncludeDirs?`) become default trait
/// methods returning the same value the TS call sites observe when the
/// method is absent (`None` / empty `Vec`), so test fixtures and external
/// context implementations compile without overriding them; the production
/// resolver overrides all of them.
///
/// Error handling: the TS methods are infallible at the type level
/// (exceptions notwithstanding); implementations backed by
/// `db::QueryBuilder` should log-and-swallow query errors to empty
/// results rather than panic.
pub trait ResolutionContext {
    /// Get all nodes in a file
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node>;
    /// Get all nodes by name
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node>;
    /// Get all nodes by qualified name
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node>;
    /// Get all nodes of a kind
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node>;
    /// Check if a file exists
    fn file_exists(&self, file_path: &str) -> bool;
    /// Read file content
    fn read_file(&self, file_path: &str) -> Option<String>;
    /// Get project root
    fn get_project_root(&self) -> &str;
    /// Get all files
    fn get_all_files(&self) -> Vec<String>;
    /// Get nodes by lowercase name (O(1) lookup for fuzzy matching)
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node>;
    /// Get cached import mappings for a file
    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping>;
    /// Project import-path aliases (tsconfig/jsconfig `paths`). Returns
    /// `None` when the project doesn't define any. Cached per resolver
    /// instance — safe to call from any resolver code path. Defaulted so
    /// existing test fixtures and external context implementations
    /// compile without modification; production resolver implements it.
    fn get_project_aliases(&self) -> Option<&AliasMap> {
        None
    }
    /// Go module info from `go.mod` at the project root. Returns `None`
    /// when the project has no `go.mod` (non-Go projects, pre-modules
    /// Go code, or projects whose modules live in subdirectories). Used
    /// by the Go branch of import resolution to distinguish in-module
    /// cross-package imports from third-party packages.
    fn get_go_module(&self) -> Option<&GoModule> {
        None
    }
    /// Monorepo workspace member packages, keyed by declared package name.
    /// Returns `None` for single-package repos (no `workspaces` field).
    /// Lets the resolver treat `@scope/ui/sub` as a local import into the
    /// member's directory instead of an external npm package (#629).
    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        None
    }
    /// Re-exports declared by a file (`export { x } from './other'`,
    /// `export * from './other'`). Empty vec when the file has none.
    /// Defaulted so older callers compile; the import resolver follows
    /// re-export chains when this is provided.
    fn get_re_exports(&self, _file_path: &str, _language: Language) -> Vec<ReExport> {
        Vec::new()
    }
    /// List immediate subdirectories of `relative_path` (relative to the
    /// project root). Returns an empty vec when the path doesn't exist
    /// or isn't a directory. Used by framework resolvers that need to
    /// walk build-system metadata (e.g. Cargo workspace globs). Defaulted
    /// so external context implementations and test fixtures compile
    /// without modification.
    fn list_directories(&self, _relative_path: &str) -> Vec<String> {
        Vec::new()
    }
    /// C/C++ include search directories (relative to project root),
    /// extracted from compile_commands.json or discovered by heuristic.
    /// Used by resolve_cpp_include_path to search -I directories when
    /// relative resolution fails. Defaulted so existing callers compile.
    fn get_cpp_include_dirs(&self) -> Vec<String> {
        Vec::new()
    }
}

// =============================================================================
// Framework resolvers
// =============================================================================

/// Result of framework-specific file extraction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameworkExtractionResult {
    /// Framework-specific nodes (e.g. routes)
    pub nodes: Vec<Node>,
    /// Framework-specific unresolved references (e.g. route -> handler)
    pub references: Vec<UnresolvedRef>,
}

/// Framework-specific resolver.
///
/// TS interface → Rust trait (object-safe; held as
/// `Box<dyn FrameworkResolver>` by the resolver registry). The TS-optional
/// members map as follows:
/// - `languages?` → [`FrameworkResolver::languages`] defaulting to `None`
///   (= applies to all languages).
/// - `claimsReference?` → default `false` (mirrors the TS
///   `resolver.claimsReference?.(name)` call site where an absent method
///   is falsy).
/// - `extract?` / `postExtract?` → default `None`. `None` means "this
///   framework does not implement the hook" (TS: method absent), which the
///   orchestrator may use to skip per-file work; `Some(empty)` means the
///   hook ran and found nothing.
///
/// All methods take `&self`: every TS framework resolver is stateless
/// (verified — no `this.` mutation anywhere under `frameworks/`).
/// Implementations that want a cache should use interior mutability
/// (`RefCell`/`Cell`).
pub trait FrameworkResolver: Send + Sync {
    /// Framework name
    fn name(&self) -> &str;
    /// Languages this framework applies to. If `None`, applies to all languages.
    fn languages(&self) -> Option<&[Language]> {
        None
    }
    /// Detect if project uses this framework (project-level, called once at startup)
    fn detect(&self, context: &dyn ResolutionContext) -> bool;
    /// Resolve a reference using framework-specific patterns
    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef>;
    /// Opt a reference NAME through the resolver's name-exists pre-filter, even when
    /// no node is named that. Needed for dynamic dispatch where the call target is
    /// an attribute/descriptor, not a declared symbol (e.g. Django's
    /// `self._iterable_class(...)`, React effect callbacks). Returning true lets the
    /// ref reach `resolve()` instead of being dropped for having no name match.
    fn claims_reference(&self, _name: &str) -> bool {
        false
    }
    /// Extract framework-specific nodes and references from a file.
    ///
    /// Returns route nodes, middleware nodes, etc., plus unresolved references
    /// that link those nodes to handlers (view classes, controller methods,
    /// included modules). Unresolved references flow into the normal resolution
    /// pipeline; the framework's own `resolve()` is one of the strategies tried.
    ///
    /// `None` = hook not implemented (TS: `extract` absent).
    fn extract(&self, _file_path: &str, _content: &str) -> Option<FrameworkExtractionResult> {
        None
    }
    /// Cross-file finalization pass, called once after all per-file extraction
    /// completes (and again on every incremental sync). Used by frameworks where
    /// a symbol's final representation depends on a sibling file the per-file
    /// `extract()` never saw — e.g. NestJS's `RouterModule.register([...])`
    /// sets route prefixes for controllers declared elsewhere.
    ///
    /// Implementations return route/etc. nodes with mutated fields (typically
    /// `name`); the orchestrator persists each via `update_node`. The node `id`
    /// MUST be preserved so existing edges (route → handler, etc.) stay intact;
    /// `qualified_name` SHOULD be preserved so the pass stays idempotent — a
    /// second run can recover the original in-file form from `qualified_name`.
    ///
    /// `None` = hook not implemented (TS: `postExtract` absent).
    fn post_extract(&self, _context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        None
    }
}

// =============================================================================
// Imports / re-exports
// =============================================================================

/// Import mapping from a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportMapping {
    /// Local name used in the file
    pub local_name: String,
    /// Original exported name (may differ due to aliasing)
    pub exported_name: String,
    /// Source module/path
    pub source: String,
    /// Whether it's a default import
    pub is_default: bool,
    /// Whether it's a namespace import (import * as X)
    pub is_namespace: bool,
    /// Resolved file path (if local)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
}

/// Re-export from a file: `export { x } from './other'` or
/// `export * from './other'`. Used by the resolver to chase
/// symbols through barrel files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReExport {
    #[serde(rename_all = "camelCase")]
    Named {
        /// Name as exported by THIS file.
        exported_name: String,
        /// Name in the upstream module (differs when renamed: `as`).
        original_name: String,
        /// Module specifier of the upstream module.
        source: String,
    },
    Wildcard {
        /// Module specifier of the upstream module.
        source: String,
    },
}

impl ReExport {
    /// Module specifier of the upstream module (common to both variants).
    pub fn source(&self) -> &str {
        match self {
            ReExport::Named { source, .. } => source,
            ReExport::Wildcard { source } => source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_by_round_trips_ts_strings() {
        for m in RESOLVED_BY_METHODS {
            assert_eq!(m.as_str().parse::<ResolvedBy>().unwrap(), m);
        }
        assert_eq!(
            "exact-match".parse::<ResolvedBy>().unwrap(),
            ResolvedBy::ExactMatch
        );
        assert_eq!(
            "instance-method".parse::<ResolvedBy>().unwrap(),
            ResolvedBy::InstanceMethod
        );
        assert!("nope".parse::<ResolvedBy>().is_err());
        // serde wire value matches the TS union string
        assert_eq!(
            serde_json::to_value(ResolvedBy::QualifiedName).unwrap(),
            serde_json::json!("qualified-name")
        );
    }

    #[test]
    fn unresolved_ref_serializes_camel_case_and_omits_absent_candidates() {
        let r = UnresolvedRef {
            from_node_id: "n1".into(),
            reference_name: "foo".into(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 7,
            file_path: "src/a.ts".into(),
            language: Language::Typescript,
            candidates: None,
            metadata: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["fromNodeId"], "n1");
        assert_eq!(v["referenceName"], "foo");
        assert_eq!(v["referenceKind"], "calls");
        assert_eq!(v["filePath"], "src/a.ts");
        assert_eq!(v["language"], "typescript");
        assert!(v.get("candidates").is_none());
    }

    #[test]
    fn re_export_serializes_with_kind_tag() {
        let named = ReExport::Named {
            exported_name: "X".into(),
            original_name: "Y".into(),
            source: "./other".into(),
        };
        let v = serde_json::to_value(&named).unwrap();
        assert_eq!(v["kind"], "named");
        assert_eq!(v["exportedName"], "X");
        assert_eq!(v["originalName"], "Y");
        assert_eq!(v["source"], "./other");

        let wild = ReExport::Wildcard {
            source: "./barrel".into(),
        };
        let v = serde_json::to_value(&wild).unwrap();
        assert_eq!(v["kind"], "wildcard");
        assert_eq!(v["source"], "./barrel");
        assert_eq!(wild.source(), "./barrel");
    }

    #[test]
    fn resolution_context_defaults_match_ts_absent_optionals() {
        struct Fixture;
        impl ResolutionContext for Fixture {
            fn get_nodes_in_file(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_qualified_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_kind(&self, _: NodeKind) -> Vec<Node> {
                Vec::new()
            }
            fn file_exists(&self, _: &str) -> bool {
                false
            }
            fn read_file(&self, _: &str) -> Option<String> {
                None
            }
            fn get_project_root(&self) -> &str {
                "/tmp/p"
            }
            fn get_all_files(&self) -> Vec<String> {
                Vec::new()
            }
            fn get_nodes_by_lower_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
                Vec::new()
            }
        }
        // Object safety + defaults
        let ctx: &dyn ResolutionContext = &Fixture;
        assert!(ctx.get_project_aliases().is_none());
        assert!(ctx.get_go_module().is_none());
        assert!(ctx.get_workspace_packages().is_none());
        assert!(ctx.get_re_exports("a.ts", Language::Typescript).is_empty());
        assert!(ctx.list_directories("src").is_empty());
        assert!(ctx.get_cpp_include_dirs().is_empty());
    }

    #[test]
    fn framework_resolver_defaults_match_ts_absent_optionals() {
        struct Fx;
        impl FrameworkResolver for Fx {
            fn name(&self) -> &str {
                "fx"
            }
            fn detect(&self, _: &dyn ResolutionContext) -> bool {
                false
            }
            fn resolve(&self, _: &UnresolvedRef, _: &dyn ResolutionContext) -> Option<ResolvedRef> {
                None
            }
        }
        let r: Box<dyn FrameworkResolver> = Box::new(Fx);
        assert_eq!(r.name(), "fx");
        assert!(r.languages().is_none());
        assert!(!r.claims_reference("anything"));
        assert!(r.extract("a.ts", "content").is_none());
        struct Ctx;
        impl ResolutionContext for Ctx {
            fn get_nodes_in_file(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_qualified_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_nodes_by_kind(&self, _: NodeKind) -> Vec<Node> {
                Vec::new()
            }
            fn file_exists(&self, _: &str) -> bool {
                false
            }
            fn read_file(&self, _: &str) -> Option<String> {
                None
            }
            fn get_project_root(&self) -> &str {
                ""
            }
            fn get_all_files(&self) -> Vec<String> {
                Vec::new()
            }
            fn get_nodes_by_lower_name(&self, _: &str) -> Vec<Node> {
                Vec::new()
            }
            fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
                Vec::new()
            }
        }
        assert!(r.post_extract(&Ctx).is_none());
    }
}
