//! Which traits a file has in scope — what decides whether `recv.m()` may
//! call a trait's method (rustc only considers traits in scope: the
//! prelude's, and those the file imports).
//!
//! Built from the project file's `use` declarations, each resolved through
//! the API indexes ([`super::api`]). Anything that might bring a trait in
//! without saying which one — a glob of a project module, an import of an
//! item no index describes, a `use` inside a fn or an inline module — makes
//! the answer "maybe", and a lookup that depends on a "maybe" answers
//! nothing.

use std::collections::HashSet;

use super::api;
use crate::resolution::external::open::GraphCache;
use crate::resolution::name_matcher::UseBinding;
use crate::resolution::types::ResolutionContext;

/// Whether a trait's methods can be called in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InScope {
    Yes,
    Maybe,
    No,
}

/// The traits one file has in scope.
#[derive(Debug, Default)]
pub(crate) struct TraitScope {
    /// Traits (canonical paths) imported by name at the file's top level.
    imported: HashSet<String>,
    /// Traits imported where they may not reach the reference (inside a fn
    /// or an inline module), or through a glob of a module that exports
    /// them.
    maybe: HashSet<String>,
    /// Some import (a glob of a project module, a name no index knows) may
    /// bring in any trait.
    opaque: bool,
    /// Crates a glob or an unresolved named import reads from (with the
    /// names imported by name): their own traits may be in scope.
    opaque_crates: Vec<(String, Option<String>)>,
    /// Traits a `use` inside a fn imports, with the line of that `use`: in
    /// scope in that fn only.
    local: Vec<(u32, String)>,
}

impl TraitScope {
    /// The scope of `file` in the project `context`.
    pub(crate) fn of_file(
        cache: &GraphCache<'_>,
        context: &dyn ResolutionContext,
        file: &str,
    ) -> TraitScope {
        let mut scope = TraitScope::default();
        for found in context.get_rust_use_leaves(file).iter() {
            let nested = !found.inline_modules.is_empty();
            // `mod tests { use super::*; }` re-imports the file's own
            // top-level names, which this scope already holds.
            if nested && found.leaf.binding == UseBinding::Glob && found.leaf.path == ["super"] {
                continue;
            }
            scope.add(cache, &found.leaf.path, &found.leaf.binding, nested);
        }
        for local in context.get_rust_fn_local_uses(file).iter() {
            let (path, binding) = (&local.leaf.path, &local.leaf.binding);
            let named = matches!(binding, UseBinding::Name(_));
            let traced = path.split_first().and_then(|(root, rest)| {
                let reachable =
                    !matches!(root.as_str(), "crate" | "self" | "super") && cache.knows(root);
                reachable
                    .then(|| api::trait_at(cache, root, rest))
                    .flatten()
            });
            match traced {
                Some(Some(canonical)) if named => scope.local.push((local.line, canonical)),
                _ => scope.add(cache, path, binding, true),
            }
        }
        scope
    }

    fn add(&mut self, cache: &GraphCache<'_>, path: &[String], binding: &UseBinding, nested: bool) {
        let Some((root, rest)) = path.split_first() else {
            return;
        };
        let reachable = !matches!(root.as_str(), "crate" | "self" | "super") && cache.knows(root);
        match binding {
            UseBinding::Glob => {
                if !reachable {
                    self.opaque = true;
                    return;
                }
                match api::module_traits(cache, root, rest) {
                    Some(traits) => self.maybe.extend(traits),
                    None => self.opaque_crates.push((root.clone(), None)),
                }
            }
            UseBinding::Name(_) | UseBinding::Module(_) => {
                if !reachable {
                    // A project item: a trait of the project (or one it
                    // re-exports) may be what it names.
                    self.opaque_crates
                        .push((String::new(), path.last().cloned()));
                    return;
                }
                match api::trait_at(cache, root, rest) {
                    Some(Some(canonical)) if nested => {
                        self.maybe.insert(canonical);
                    }
                    Some(Some(canonical)) => {
                        self.imported.insert(canonical);
                    }
                    Some(None) => {}
                    None => self
                        .opaque_crates
                        .push((root.clone(), path.last().cloned())),
                }
            }
        }
    }

    /// Whether the trait `canonical` is in scope (`prelude`: the prelude's
    /// traits; `in_same_fn`: a `use` on that line reaches the reference).
    pub(crate) fn has(
        &self,
        prelude: &HashSet<String>,
        canonical: &str,
        in_same_fn: &dyn Fn(u32) -> bool,
    ) -> InScope {
        let local = || {
            self.local
                .iter()
                .any(|(line, local)| local == canonical && in_same_fn(*line))
        };
        if prelude.contains(canonical) || self.imported.contains(canonical) || local() {
            InScope::Yes
        } else if self.opaque || self.maybe.contains(canonical) {
            InScope::Maybe
        } else {
            InScope::No
        }
    }

    /// Crates (`""`: the project) whose traits an import may bring in, and
    /// the name imported (`None`: a glob).
    pub(crate) fn opaque_imports(&self) -> &[(String, Option<String>)] {
        &self.opaque_crates
    }

    /// A glob of a project module may bring in any trait.
    pub(crate) fn is_opaque(&self) -> bool {
        self.opaque
    }
}
