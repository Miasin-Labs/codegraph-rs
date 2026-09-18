//! The project's resolution context, seen by the external pass: every
//! lookup is the project's own, and [`ResolutionContext::foreign_types`]
//! answers from the reachable graphs — so receiver inference types chains
//! through dependency return types here, and only here.

use std::sync::Arc;

use super::declarations::Declarations;
use crate::resolution::ForeignTypes;
use crate::resolution::name_matcher::{RustUse, UseLeaf};
use crate::resolution::types::{
    AliasMap,
    GoModule,
    ImportMapping,
    ReExport,
    ResolutionContext,
    WorkspacePackages,
};
use crate::types::{Language, Node, NodeKind};

/// A project context plus the declarations of the graphs it reaches.
pub(crate) struct ExternalContext<'a> {
    pub(crate) project: &'a dyn ResolutionContext,
    pub(crate) declarations: &'a Declarations<'a>,
}

impl ResolutionContext for ExternalContext<'_> {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.project.get_nodes_in_file(file_path)
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.project.get_node_by_id(id)
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.project.get_nodes_by_name(name)
    }

    fn get_nodes_by_name_and_kind(&self, name: &str, kind: NodeKind) -> Vec<Node> {
        self.project.get_nodes_by_name_and_kind(name, kind)
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.project.get_nodes_by_qualified_name(qualified_name)
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.project.get_nodes_by_kind(kind)
    }

    fn file_exists(&self, file_path: &str) -> bool {
        self.project.file_exists(file_path)
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        self.project.read_file(file_path)
    }

    fn read_file_arc(&self, file_path: &str) -> Option<Arc<str>> {
        self.project.read_file_arc(file_path)
    }

    fn get_project_root(&self) -> &str {
        self.project.get_project_root()
    }

    fn get_all_files(&self) -> Vec<String> {
        self.project.get_all_files()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.project.get_nodes_by_lower_name(lower_name)
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        self.project.get_import_mappings(file_path, language)
    }

    fn get_project_aliases(&self) -> Option<&AliasMap> {
        self.project.get_project_aliases()
    }

    fn get_go_module(&self) -> Option<&GoModule> {
        self.project.get_go_module()
    }

    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.project.get_workspace_packages()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        self.project.get_re_exports(file_path, language)
    }

    fn get_rust_use_leaves(&self, file_path: &str) -> Arc<[RustUse]> {
        self.project.get_rust_use_leaves(file_path)
    }

    fn get_rust_fn_local_uses(&self, file_path: &str) -> Arc<[UseLeaf]> {
        self.project.get_rust_fn_local_uses(file_path)
    }

    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        self.project.list_directories(relative_path)
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.project.get_cpp_include_dirs()
    }

    fn is_rust_dependency_method(&self, name: &str) -> bool {
        self.project.is_rust_dependency_method(name)
    }

    fn foreign_types(&self) -> Option<&dyn ForeignTypes> {
        Some(self.declarations)
    }
}
