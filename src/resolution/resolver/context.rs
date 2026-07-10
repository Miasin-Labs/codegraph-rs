//! Production filesystem/database-backed resolution context.

mod files;
mod imports;
mod metadata;
mod nodes;

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::sync::LazyLock;

use super::cache::resolve_cache_limit;
use crate::db::QueryBuilder;
use crate::resolution::lru_cache::LRUCache;
use crate::resolution::types::{
    AliasMap,
    GoModule,
    ImportMapping,
    ReExport,
    ResolutionContext,
    UnresolvedRef,
    WorkspacePackages,
};
use crate::types::{Language, Node, NodeKind};

/// The production [`ResolutionContext`] implementation, backed by a
/// [`QueryBuilder`] + the project filesystem, with LRU-bounded caches.
pub struct ResolverContext {
    pub(super) project_root: String,
    pub(super) queries: QueryBuilder,
    pub(super) node_cache: RefCell<LRUCache<String, Vec<Node>>>,
    pub(super) file_cache: RefCell<LRUCache<String, Option<String>>>,
    pub(super) import_mapping_cache: RefCell<LRUCache<String, Vec<ImportMapping>>>,
    pub(super) re_export_cache: RefCell<LRUCache<String, Vec<ReExport>>>,
    pub(super) name_cache: RefCell<LRUCache<String, Vec<Node>>>,
    pub(super) lower_name_cache: RefCell<LRUCache<String, Vec<Node>>>,
    pub(super) qualified_name_cache: RefCell<LRUCache<String, Vec<Node>>>,
    pub(super) known_names: RefCell<Option<HashSet<String>>>,
    pub(super) known_files: RefCell<Option<HashSet<String>>>,
    pub(super) files_list: RefCell<Option<std::sync::Arc<Vec<String>>>>,
    pub(super) caches_warmed: Cell<bool>,
    pub(super) project_aliases: OnceCell<Option<AliasMap>>,
    pub(super) go_module: OnceCell<Option<GoModule>>,
    pub(super) workspace_packages: OnceCell<Option<WorkspacePackages>>,
}

/// JS/TS/ArkTS source files that use ES module import syntax.
pub(super) fn is_js_family_path(file_path: &str) -> bool {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?|ets)$")
            .expect("valid js-family regex")
    });
    RE.is_match(file_path)
}

pub(super) fn is_js_ts_language(language: Language) -> bool {
    matches!(
        language,
        Language::Typescript
            | Language::Javascript
            | Language::Tsx
            | Language::Jsx
            | Language::Arkts
    )
}

pub(super) fn is_low_value_js_ts_resolution_source(reference: &UnresolvedRef) -> bool {
    if !is_js_ts_language(reference.language) || !is_js_family_path(&reference.file_path) {
        return false;
    }

    let path = reference.file_path.replace('\\', "/").to_ascii_lowercase();
    if path.contains("/deobfuscated-bundles/") {
        return true;
    }

    let file_name = reference
        .file_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(reference.file_path.as_str())
        .to_ascii_lowercase();
    file_name.contains(".min.") || file_name.contains(".deob.")
}

impl ResolverContext {
    pub(super) fn new(project_root: String, queries: QueryBuilder) -> Self {
        let limit = resolve_cache_limit();
        let content_limit = std::cmp::max(64, limit / 5);
        ResolverContext {
            project_root,
            queries,
            node_cache: RefCell::new(LRUCache::new(limit)),
            file_cache: RefCell::new(LRUCache::new(content_limit)),
            import_mapping_cache: RefCell::new(LRUCache::new(limit)),
            re_export_cache: RefCell::new(LRUCache::new(limit)),
            name_cache: RefCell::new(LRUCache::new(limit)),
            lower_name_cache: RefCell::new(LRUCache::new(limit)),
            qualified_name_cache: RefCell::new(LRUCache::new(limit)),
            known_names: RefCell::new(None),
            known_files: RefCell::new(None),
            files_list: RefCell::new(None),
            caches_warmed: Cell::new(false),
            project_aliases: OnceCell::new(),
            go_module: OnceCell::new(),
            workspace_packages: OnceCell::new(),
        }
    }

    pub(super) fn clear_caches(&self) {
        self.node_cache.borrow_mut().clear();
        self.file_cache.borrow_mut().clear();
        self.import_mapping_cache.borrow_mut().clear();
        self.re_export_cache.borrow_mut().clear();
        self.name_cache.borrow_mut().clear();
        self.lower_name_cache.borrow_mut().clear();
        self.qualified_name_cache.borrow_mut().clear();
        *self.known_names.borrow_mut() = None;
        *self.known_files.borrow_mut() = None;
        *self.files_list.borrow_mut() = None;
        self.caches_warmed.set(false);
    }

    /// `this.knownNames?.has(name)` — false when the cache isn't warmed.
    pub(super) fn known_has(&self, name: &str) -> bool {
        self.known_names
            .borrow()
            .as_ref()
            .is_some_and(|names| names.contains(name))
    }
}

impl ResolutionContext for ResolverContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.cached_nodes_in_file(file_path)
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.cached_nodes_by_name(name)
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.cached_nodes_by_qualified_name(qualified_name)
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes_by_kind(kind)
    }

    fn file_exists(&self, file_path: &str) -> bool {
        self.indexed_or_disk_file_exists(file_path)
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        self.cached_file_text(file_path)
    }

    fn get_project_root(&self) -> &str {
        &self.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.cached_all_files()
    }

    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        self.directories_in(relative_path)
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.cached_nodes_by_lower_name(lower_name)
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        self.cached_import_mappings(file_path, language)
    }

    fn get_project_aliases(&self) -> Option<&AliasMap> {
        self.project_aliases()
    }

    fn get_go_module(&self) -> Option<&GoModule> {
        self.go_module()
    }

    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        self.cached_re_exports(file_path, language)
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.cpp_include_dirs()
    }
}
