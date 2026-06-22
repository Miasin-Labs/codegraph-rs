use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::engine::ResolutionPolicy;
use super::{
    APEX_BUILT_IN_METHODS,
    APEX_SYSTEM_TYPES,
    BASH_BUILT_INS,
    C_BUILT_INS,
    C_CPP_STDLIB_CALLS,
    CPP_BUILT_INS,
    GO_BUILT_INS,
    GO_STDLIB_PACKAGES,
    JS_BUILT_INS,
    JVM_NAMESPACE_SEGMENTS,
    JVM_STDLIB_EXTERNAL_CALLS,
    JVM_STDLIB_IMPORT_PREFIXES,
    JVM_STDLIB_TYPES,
    PASCAL_BUILT_INS,
    PASCAL_UNIT_PREFIXES,
    PYTHON_BUILT_IN_METHODS,
    PYTHON_BUILT_IN_TYPES,
    PYTHON_BUILT_INS,
    REACT_HOOKS,
    capitalize_first,
    has_any_possible_match_in,
    is_js_family_path,
    is_js_ts_language,
    is_low_value_js_ts_resolution_source,
};
use crate::db::QueryBuilder;
use crate::error::{Result, log_debug};
use crate::resolution::go_module::load_go_module;
use crate::resolution::import_resolver::{
    extract_import_mappings,
    extract_re_exports,
    load_cpp_include_dirs,
};
use crate::resolution::path_aliases::load_project_aliases;
use crate::resolution::types::{
    AliasMap,
    GoModule,
    ImportMapping,
    ReExport,
    ResolutionContext,
    UnresolvedRef,
    WorkspacePackages,
};
use crate::resolution::workspace_packages::load_workspace_packages;
use crate::types::{EdgeKind, Language, Node, NodeKind};

pub(super) struct ResolverSnapshot {
    context: SnapshotContext,
}

type ImportCacheKey = (String, Language);

pub(super) struct SnapshotContext {
    project_root: String,
    nodes_by_id: HashMap<String, Node>,
    nodes_by_file: HashMap<String, Vec<Node>>,
    nodes_by_name: HashMap<String, Vec<Node>>,
    nodes_by_qualified_name: HashMap<String, Vec<Node>>,
    nodes_by_kind: HashMap<NodeKind, Vec<Node>>,
    nodes_by_lower_name: HashMap<String, Vec<Node>>,
    all_files: Vec<String>,
    known_files: HashSet<String>,
    known_names: HashSet<String>,
    project_aliases: Option<AliasMap>,
    go_module: Option<GoModule>,
    workspace_packages: Option<WorkspacePackages>,
    cpp_include_dirs: Vec<String>,
    file_cache: Mutex<HashMap<String, Option<String>>>,
    import_mapping_cache: Mutex<HashMap<ImportCacheKey, Vec<ImportMapping>>>,
    re_export_cache: Mutex<HashMap<ImportCacheKey, Vec<ReExport>>>,
}

impl ResolverSnapshot {
    pub(super) fn build(project_root: &str, queries: &QueryBuilder) -> Result<Self> {
        let nodes = queries.get_all_nodes()?;
        let all_files = queries.get_all_file_paths()?;
        let mut nodes_by_id = HashMap::with_capacity(nodes.len());
        let mut nodes_by_file: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_qualified_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_kind: HashMap<NodeKind, Vec<Node>> = HashMap::new();
        let mut nodes_by_lower_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut known_names = HashSet::new();

        for node in nodes {
            known_names.insert(node.name.clone());
            nodes_by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node.clone());
            nodes_by_name
                .entry(node.name.clone())
                .or_default()
                .push(node.clone());
            nodes_by_qualified_name
                .entry(node.qualified_name.clone())
                .or_default()
                .push(node.clone());
            nodes_by_kind
                .entry(node.kind)
                .or_default()
                .push(node.clone());
            nodes_by_lower_name
                .entry(node.name.to_lowercase())
                .or_default()
                .push(node.clone());
            nodes_by_id.insert(node.id.clone(), node);
        }

        let known_files: HashSet<String> = all_files.iter().cloned().collect();
        let context = SnapshotContext {
            project_root: project_root.to_string(),
            nodes_by_id,
            nodes_by_file,
            nodes_by_name,
            nodes_by_qualified_name,
            nodes_by_kind,
            nodes_by_lower_name,
            all_files,
            known_files,
            known_names,
            project_aliases: load_project_aliases(project_root),
            go_module: load_go_module(project_root),
            workspace_packages: load_workspace_packages(project_root),
            cpp_include_dirs: load_cpp_include_dirs(project_root),
            file_cache: Mutex::new(HashMap::new()),
            import_mapping_cache: Mutex::new(HashMap::new()),
            re_export_cache: Mutex::new(HashMap::new()),
        };

        Ok(Self { context })
    }

    pub(super) fn context(&self) -> &SnapshotContext {
        &self.context
    }
}

impl SnapshotContext {
    pub(super) fn get_node_by_id(&self, node_id: &str) -> Option<&Node> {
        self.nodes_by_id.get(node_id)
    }

    fn known_has(&self, name: &str) -> bool {
        self.known_names.contains(name)
    }
}

impl ResolutionPolicy for SnapshotContext {
    fn is_built_in_or_external(&self, reference: &UnresolvedRef) -> bool {
        let name = reference.reference_name.as_str();
        if is_low_value_js_ts_resolution_source(reference) {
            return true;
        }
        let is_js_ts = is_js_ts_language(reference.language);

        if is_js_ts && JS_BUILT_INS.contains(name) {
            return true;
        }
        if is_js_ts
            && (name.starts_with("console.")
                || name.starts_with("Math.")
                || name.starts_with("JSON."))
        {
            return true;
        }
        if is_js_ts && REACT_HOOKS.contains(name) {
            return true;
        }

        if reference.language == Language::Python && PYTHON_BUILT_INS.contains(name) {
            return true;
        }
        if reference.language == Language::Python {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let receiver = &name[..dot_idx];
                    let method = &name[dot_idx + 1..];
                    if PYTHON_BUILT_IN_TYPES.contains(receiver) {
                        return true;
                    }
                    if PYTHON_BUILT_IN_METHODS.contains(method) {
                        let capitalized = capitalize_first(receiver);
                        if !self.known_has(&capitalized) {
                            return true;
                        }
                    }
                }
            }
            if PYTHON_BUILT_IN_METHODS.contains(name) && !self.known_has(name) {
                return true;
            }
        }

        if reference.language == Language::Go {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let pkg = &name[..dot_idx];
                    if GO_STDLIB_PACKAGES.contains(pkg) {
                        return true;
                    }
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }

        if (reference.language == Language::C || reference.language == Language::Cpp)
            && reference.reference_kind == EdgeKind::Calls
            && C_CPP_STDLIB_CALLS.contains(name)
            && !self.known_has(name)
        {
            return true;
        }
        if (reference.language == Language::Java || reference.language == Language::Kotlin)
            && reference.reference_kind == EdgeKind::Calls
            && JVM_STDLIB_EXTERNAL_CALLS.contains(name)
            && !self.known_has(name)
        {
            return true;
        }
        if (reference.language == Language::Java || reference.language == Language::Kotlin)
            && reference.reference_kind == EdgeKind::Imports
            && JVM_STDLIB_IMPORT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            return true;
        }
        if (reference.language == Language::Java || reference.language == Language::Kotlin)
            && (reference.reference_kind == EdgeKind::References
                || reference.reference_kind == EdgeKind::Instantiates)
            && JVM_STDLIB_TYPES.contains(name)
            && !self.known_has(name)
        {
            return true;
        }
        if (reference.language == Language::Java || reference.language == Language::Kotlin)
            && reference.reference_kind == EdgeKind::References
            && JVM_NAMESPACE_SEGMENTS.contains(name)
        {
            return true;
        }

        if reference.language == Language::Pascal {
            if PASCAL_UNIT_PREFIXES.iter().any(|p| name.starts_with(p)) {
                return true;
            }
            if PASCAL_BUILT_INS.contains(name) {
                return true;
            }
        }
        if reference.language == Language::Bash && BASH_BUILT_INS.contains(name) {
            return true;
        }

        if reference.language == Language::Apex {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let receiver = &name[..dot_idx];
                    let method = &name[dot_idx + 1..];
                    if APEX_SYSTEM_TYPES.contains(receiver.to_lowercase().as_str())
                        && !self.known_has(receiver)
                        && !self.known_has(&capitalize_first(receiver))
                    {
                        return true;
                    }
                    if APEX_BUILT_IN_METHODS.contains(method.to_lowercase().as_str()) {
                        let capitalized = capitalize_first(receiver);
                        if !self.known_has(receiver) && !self.known_has(&capitalized) {
                            return true;
                        }
                    }
                }
            }
        }

        if reference.language == Language::C || reference.language == Language::Cpp {
            if name.starts_with("std::") {
                return true;
            }
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }

        false
    }

    fn has_any_possible_match(&self, name: &str) -> bool {
        has_any_possible_match_in(&self.known_names, name)
    }

    fn has_any_possible_match_ci(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        let probe = |s: &str| !self.get_nodes_by_lower_name(s).is_empty();
        if probe(&lower) {
            return true;
        }
        if let Some(dot_idx) = lower.find('.') {
            if dot_idx > 0 {
                let receiver = &lower[..dot_idx];
                let member = &lower[dot_idx + 1..];
                if probe(receiver) || probe(member) {
                    return true;
                }
                let last_dot = lower.rfind('.').unwrap_or(0);
                if last_dot > dot_idx && probe(&lower[last_dot + 1..]) {
                    return true;
                }
            }
        }
        false
    }

    fn matches_any_import(&self, reference: &UnresolvedRef) -> bool {
        let imports = self.get_import_mappings(&reference.file_path, reference.language);
        if imports.is_empty() {
            return false;
        }
        imports.iter().any(|import| {
            import.local_name == reference.reference_name
                || reference
                    .reference_name
                    .starts_with(&format!("{}.", import.local_name))
        })
    }
}

impl ResolutionContext for SnapshotContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes_by_file
            .get(file_path)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes_by_name.get(name).cloned().unwrap_or_default()
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes_by_qualified_name
            .get(qualified_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes_by_kind.get(&kind).cloned().unwrap_or_default()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        let normalized = file_path.replace('\\', "/");
        self.known_files.contains(file_path)
            || self.known_files.contains(&normalized)
            || Path::new(&self.project_root).join(file_path).exists()
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        {
            let cache = self
                .file_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(file_path) {
                return cached.clone();
            }
        }

        let full_path = Path::new(&self.project_root).join(file_path);
        let content = match fs::read(&full_path) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) => {
                log_debug(
                    "Failed to read file for snapshot resolution",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string()
                    })),
                );
                None
            }
        };
        self.file_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(file_path.to_string(), content.clone());
        content
    }

    fn get_project_root(&self) -> &str {
        &self.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.all_files.clone()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes_by_lower_name
            .get(lower_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        let key = (file_path.to_string(), language);
        {
            let cache = self
                .import_mapping_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let mappings = match self.read_file(file_path) {
            Some(content) if !content.is_empty() => {
                extract_import_mappings(file_path, &content, language)
            }
            _ => Vec::new(),
        };
        self.import_mapping_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, mappings.clone());
        mappings
    }

    fn get_project_aliases(&self) -> Option<&AliasMap> {
        self.project_aliases.as_ref()
    }

    fn get_go_module(&self) -> Option<&GoModule> {
        self.go_module.as_ref()
    }

    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages.as_ref()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let parse_language = if is_js_family_path(file_path) {
            Language::Typescript
        } else {
            language
        };
        let key = (file_path.to_string(), parse_language);
        {
            let cache = self
                .re_export_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let re_exports = match self.read_file(file_path) {
            Some(content) if !content.is_empty() => extract_re_exports(&content, parse_language),
            _ => Vec::new(),
        };
        self.re_export_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, re_exports.clone());
        re_exports
    }

    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        let target: PathBuf = if relative_path == "." || relative_path.is_empty() {
            PathBuf::from(&self.project_root)
        } else {
            Path::new(&self.project_root).join(relative_path)
        };
        match fs::read_dir(&target) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(error) => {
                log_debug(
                    "Failed to list directory for snapshot resolution",
                    Some(&serde_json::json!({
                        "relativePath": relative_path,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            }
        }
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.cpp_include_dirs.clone()
    }
}
