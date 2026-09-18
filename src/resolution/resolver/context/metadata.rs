use std::path::Path;

use super::ResolverContext;
use crate::resolution::go_module::load_go_module;
use crate::resolution::import_resolver::load_cpp_include_dirs;
use crate::resolution::path_aliases::load_project_aliases;
use crate::resolution::rust_deps;
use crate::resolution::types::{AliasMap, GoModule, WorkspacePackages};
use crate::resolution::workspace_packages::load_workspace_packages;

impl ResolverContext {
    pub(super) fn project_aliases(&self) -> Option<&AliasMap> {
        self.project_aliases
            .get_or_init(|| load_project_aliases(&self.project_root))
            .as_ref()
    }

    pub(super) fn go_module(&self) -> Option<&GoModule> {
        self.go_module
            .get_or_init(|| load_go_module(&self.project_root))
            .as_ref()
    }

    pub(super) fn workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages
            .get_or_init(|| load_workspace_packages(&self.project_root))
            .as_ref()
    }

    pub(super) fn cpp_include_dirs(&self) -> Vec<String> {
        load_cpp_include_dirs(&self.project_root)
    }

    /// Read on first need after each (re)initialization, so artifacts an
    /// index run just built are seen.
    pub(super) fn rust_dependency_method(&self, name: &str) -> bool {
        let mut api = self.rust_dependency_api.borrow_mut();
        api.get_or_insert_with(|| rust_deps::load(Path::new(&self.project_root)))
            .as_ref()
            .is_some_and(|api| api.has_method(name))
    }
}
