//! What a type written in a dependency names: a path followed through the
//! file's `use` declarations, and the type an alias or a `Deref` impl
//! stands for — the hops Rust's method resolution takes.

use std::rc::Rc;

use super::lookup::{defines_type, lookup_path};
use crate::resolution::external::open::{ForeignGraph, GraphCache};
use crate::resolution::name_matcher::UseBinding;
use crate::resolution::name_matcher::external::module_path;
use crate::resolution::types::ResolutionContext;
use crate::types::{EdgeKind, NodeKind};

/// How many `use` hops a type path written in a dependency follows.
const MAX_USE_HOPS: u8 = 4;

/// The type an alias at `owner_path` stands for (`pub type StableDiGraph<N,
/// E, Ix> = StableGraph<N, E, Directed, Ix>;`), as a crate and path.
pub(super) fn aliased_type(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner_path: &[String],
) -> Option<(String, Vec<String>)> {
    let alias = lookup_path(cache, &graph.krate, owner_path, EdgeKind::References)?;
    if alias.node.kind != NodeKind::TypeAlias {
        return None;
    }
    let source = alias.graph.context.read_file_arc(&alias.node.file_path)?;
    let start = (alias.node.start_line as usize).checked_sub(1)?;
    let span = (alias.node.end_line.max(alias.node.start_line) as usize).saturating_sub(start);
    let text = source
        .split('\n')
        .skip(start)
        .take(span.max(1))
        .collect::<Vec<_>>()
        .join(" ");
    let declared = text.find(&format!("type {}", alias.node.name))?;
    let target = top_level_assignment(&text[declared..])?;
    type_path(
        cache,
        &alias.graph.krate,
        &alias.node.file_path,
        type_name(target)?,
    )
}

/// What `impl Deref for Owner { type Target = X; … }` derefs to.
pub(super) fn deref_target(
    cache: &GraphCache<'_>,
    graph: &Rc<ForeignGraph>,
    owner_path: &[String],
) -> Option<(String, Vec<String>)> {
    let owner = owner_path.last()?;
    let deref = graph
        .context
        .get_nodes_by_name("deref")
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::Method
                && node.qualified_name == format!("{owner}::deref")
                && graph.in_crate(&node.file_path)
        })?;
    let source = graph.context.read_file_arc(&deref.file_path)?;
    let lines: Vec<&str> = source.split('\n').collect();
    let at = (deref.start_line as usize).checked_sub(1)?.min(lines.len());
    // The impl block's `type Target = …;`, between its header and the fn.
    for line in lines[..at].iter().rev().take(40) {
        let trimmed = line.trim();
        if trimmed.starts_with("impl") || trimmed.starts_with("unsafe impl") {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix("type Target") {
            let target = rest
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .trim_end_matches(';');
            return type_path(cache, &graph.krate, &deref.file_path, type_name(target)?);
        }
    }
    None
}

/// The text right of the `=` that is not inside `<…>` (`type R<T, E = X> =
/// …;` has one in its generics).
fn top_level_assignment(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b';' | b'{' => return None,
            b'=' if depth == 0 => {
                let rest = &text[index + 1..];
                return Some(rest.split(';').next()?.trim());
            }
            _ => {}
        }
    }
    None
}

/// The path of a written type, generic arguments and references dropped
/// (`StableGraph<N, E>` → `StableGraph`); `None` for tuples, slices and
/// the like.
fn type_name(written: &str) -> Option<&str> {
    let text = written.trim().trim_start_matches('&').trim_start();
    let text = text.strip_prefix("mut ").unwrap_or(text);
    let path = text.split('<').next()?.trim();
    (!path.is_empty()
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':'))
    .then_some(path)
}

/// What the type path `path`, written in `file` of the crate code calls
/// `krate`, names: the crate that defines it and the type's path inside
/// that crate (`use` declarations followed; a bare name is its file's
/// module's). `None` for std types and for a crate that defines no such
/// type.
pub(crate) fn type_path(
    cache: &GraphCache<'_>,
    krate: &str,
    file: &str,
    path: &str,
) -> Option<(String, Vec<String>)> {
    let graph = cache.get(krate)?;
    let mut segments: Vec<String> = path
        .split("::")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let uses = graph.context.get_rust_use_leaves(file);
    for _ in 0..MAX_USE_HOPS {
        let (root, rest) = segments.split_first()?;
        let binding = UseBinding::Name(root.clone());
        let Some(leaf) = uses
            .iter()
            .map(|found| &found.leaf)
            .find(|leaf| leaf.binding == binding)
        else {
            break;
        };
        let full: Vec<String> = leaf.path.iter().chain(rest).cloned().collect();
        if full == segments {
            break;
        }
        segments = full;
    }
    let name = segments.last()?.clone();
    let root = segments.first()?.as_str();
    if segments.len() > 1 && matches!(root, "std" | "core" | "alloc") {
        return None;
    }
    let here = module_path(file);
    let (owner, inside) = if segments.len() == 1 {
        // Written bare: a type of the file's own module.
        let mut inside = here;
        inside.push(name.clone());
        (krate.to_string(), inside)
    } else if matches!(root, "crate" | "$crate") || root == graph.krate {
        (krate.to_string(), segments[1..].to_vec())
    } else if matches!(root, "self" | "super") {
        let mut inside = here;
        let mut rest = segments.as_slice();
        while let Some((head, tail)) = rest.split_first() {
            match head.as_str() {
                "self" => {}
                "super" => {
                    inside.pop();
                }
                _ => break,
            }
            rest = tail;
        }
        inside.extend(rest.iter().cloned());
        (krate.to_string(), inside)
    } else if cache.knows(root) {
        (root.to_string(), segments[1..].to_vec())
    } else {
        // A relative module path of this crate (`value::Value`).
        let mut inside = here;
        inside.extend(segments.iter().cloned());
        (krate.to_string(), inside)
    };
    let home = cache.get(&owner)?;
    defines_type(cache, &home, &name).then_some((owner, inside))
}
