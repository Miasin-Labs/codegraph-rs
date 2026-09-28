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

/// Most lines one `use` declaration may span.
const MAX_USE_LINES: usize = 30;

/// Every `use … as _` in `source`: the declaration's line (1-based),
/// whether it is indented (inside a fn or an inline module), and the path
/// (`use a::{b::C as _, D}` gives `a::b::C`).
fn underscore_imports(source: &str) -> Vec<(u32, bool, Vec<String>)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let declaration = trimmed
            .strip_prefix("pub(crate) use ")
            .or_else(|| trimmed.strip_prefix("pub use "))
            .or_else(|| trimmed.strip_prefix("use "));
        let Some(first) = declaration else {
            continue;
        };
        let mut text = first.to_string();
        let mut end = index;
        while !text.contains(';') && end + 1 < lines.len() && end - index < MAX_USE_LINES {
            end += 1;
            text.push(' ');
            text.push_str(lines[end].trim());
        }
        let Some(text) = text.split(';').next() else {
            continue;
        };
        if !text.contains("as _") {
            continue;
        }
        let indented = line.len() != trimmed.len();
        for path in expand_use(text) {
            if let Some(path) = path.strip_suffix(" as _") {
                let segments: Vec<String> = path
                    .split("::")
                    .map(|segment| segment.trim().to_string())
                    .filter(|segment| !segment.is_empty())
                    .collect();
                found.push((index as u32 + 1, indented, segments));
            }
        }
    }
    found
}

/// The paths a use tree names (`a::{b, c::{d as _}}` → `a::b`, `a::c::d as
/// _`), whitespace collapsed.
fn expand_use(tree: &str) -> Vec<String> {
    let tree: String = tree.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(open) = tree.find('{') else {
        return vec![tree.trim().to_string()];
    };
    let prefix = tree[..open].trim().trim_end_matches("::").to_string();
    let Some(close) = tree.rfind('}') else {
        return Vec::new();
    };
    let inner = &tree[open + 1..close];
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&inner[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .flat_map(expand_use)
        .map(|path| {
            if prefix.is_empty() {
                path
            } else {
                format!("{prefix}::{path}")
            }
        })
        .collect()
}

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
        let leaves = context.get_rust_use_leaves(file);
        for found in leaves.iter() {
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
            // An inline module's `use` (indented like a fn's) is one of the
            // file's leaves already.
            if leaves
                .iter()
                .any(|found| found.leaf.path == *path && found.leaf.binding == *binding)
            {
                continue;
            }
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
        // `use std::io::Write as _;` binds no name, so the use leaves leave
        // it out — yet it is exactly how a trait is brought in for its
        // methods.
        if let Some(source) = context.read_file_arc(file) {
            for (line, indented, path) in underscore_imports(&source) {
                let Some((root, rest)) = path.split_first() else {
                    continue;
                };
                let reachable =
                    !matches!(root.as_str(), "crate" | "self" | "super") && cache.knows(root);
                let traced = reachable
                    .then(|| api::trait_at(cache, root, rest))
                    .flatten();
                let Some(Some(canonical)) = traced else {
                    if !reachable || traced.is_none() {
                        // A trait no index describes, by an unknown name.
                        scope.opaque_crates.push((
                            if reachable {
                                root.clone()
                            } else {
                                String::new()
                            },
                            None,
                        ));
                    }
                    continue;
                };
                let in_fn = indented
                    && context
                        .scopes_enclosing_line(file, line)
                        .iter()
                        .any(|node| {
                            matches!(
                                node.kind,
                                crate::types::NodeKind::Function | crate::types::NodeKind::Method
                            )
                        });
                if !indented {
                    scope.imported.insert(canonical);
                } else if in_fn {
                    scope.local.push((line, canonical));
                } else {
                    scope.maybe.insert(canonical);
                }
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

#[cfg(test)]
mod tests {
    use super::underscore_imports;

    #[test]
    fn underscore_imports_are_found_in_every_shape() {
        let source = "use std::io::Write as _;\n\
                      use std::io::{self, Read as _, BufRead};\n\
                      fn f() {\n    use std::os::unix::process::{\n        CommandExt as _,\n    };\n}\n\
                      use std::fmt::Write;\n";
        let found: Vec<(u32, bool, String)> = underscore_imports(source)
            .into_iter()
            .map(|(line, indented, path)| (line, indented, path.join("::")))
            .collect();
        assert_eq!(
            found,
            [
                (1, false, "std::io::Write".to_string()),
                (2, false, "std::io::Read".to_string()),
                (4, true, "std::os::unix::process::CommandExt".to_string()),
            ]
        );
    }
}
