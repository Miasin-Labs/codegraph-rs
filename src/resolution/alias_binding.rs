//! Alias bindings: a name bound to nothing but another symbol.
//!
//! ```text
//!   export const alias = realImpl;      // p1 - cross-file through an import
//!   const local = realImpl;             // p6 - same file
//!   export const api = { run: impl };   // p5 - property holding a function ref
//!   export { realImpl as alias };       // p2 - a local export clause
//! ```
//!
//! Extraction records the binding itself as a `constant`/`variable` node and
//! stores its initializer in `signature` (`"= realImpl"`), so a call through the
//! alias resolves to the BINDING, not the function. `callers realImpl` then
//! omits every caller that went through the alias and reports a confident zero,
//! while `callers alias` finds them - the edge exists, it just terminates one
//! hop short. A local `export { X as Y }` clause is worse: the exported name
//! matches no declaration at all, so resolution fails outright.
//!
//! A call through an alias IS a call to the aliased function, so these hops are
//! only ever applied to `calls` refs. A `references` edge to the binding is
//! correct as-is - reading the alias as a value is a genuine use of the alias.
//!
//! Ported from `src/resolution/alias-binding.ts` (upstream #1482 / PR #1485).

use std::sync::LazyLock;

use regex::Regex;

use crate::resolution::types::ResolutionContext;
use crate::types::{Node, NodeKind};

/// Kinds that can be a pure alias for another symbol.
fn is_alias_binding_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Constant | NodeKind::Variable | NodeKind::Property
    )
}

/// Kinds an alias may usefully forward a CALL to.
fn is_callable_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Component
    )
}

// NOTE on regex classes: the TS source uses `\\w`/`[\\w$]` without the `u`
// flag, i.e. ASCII `[A-Za-z0-9_]`. The Rust `regex` crate's `\\w` is Unicode,
// so the ASCII classes are spelled out to keep matching byte-for-byte
// identical to the TS original.
/// `= identifier`, optionally with a cast or trailing semicolon, and nothing
/// else. (TS `BARE_ALIAS_RE`.)
static BARE_ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:as\s+[A-Za-z0-9_.<>\[\]]+\s*)?;?$")
        .expect("valid regex")
});

/// Escape regex metacharacters (mirrors the TS `escapeRegExp`).
fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The symbol name an alias binding forwards to, or `None` when the
/// initializer is anything else (a call, a literal, an expression - all
/// genuine definitions).
///
/// `member_name` targets a property of an object-literal initializer
/// (`= { run: impl }` for `api.run()`), including ES shorthand (`= { impl }`).
pub fn alias_target_name(signature: Option<&str>, member_name: Option<&str>) -> Option<String> {
    let signature = signature?;
    let initializer = signature.trim();

    if let Some(member_name) = member_name {
        let key = escape_regex(member_name);
        let explicit = Regex::new(&format!(
            r"[{{,]\s*{key}\s*:\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,}}]"
        ))
        .expect("valid regex");
        if let Some(caps) = explicit.captures(initializer) {
            return Some(caps[1].to_string());
        }
        // `{ impl }` - shorthand binds the property to the same-named symbol.
        let shorthand = Regex::new(&format!(r"[{{,]\s*({key})\s*[,}}]")).expect("valid regex");
        if let Some(caps) = shorthand.captures(initializer) {
            return Some(caps[1].to_string());
        }
        return None;
    }

    BARE_ALIAS_RE
        .captures(initializer)
        .map(|caps| caps[1].to_string())
}

/// The callable an alias binding forwards to.
///
/// Prefers a declaration in the alias's own file: an alias almost always names
/// a local symbol or one it imported, and a same-file hit needs no
/// disambiguation. Cross-file is accepted only when the name is unique in the
/// project, so an ambiguous name yields no hop rather than an invented edge - a
/// wrong edge is worse than a missing one.
pub fn resolve_alias_binding(
    alias_node: &Node,
    member_name: Option<&str>,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    if !is_alias_binding_kind(alias_node.kind) {
        return None;
    }

    let target_name = alias_target_name(alias_node.signature.as_deref(), member_name)?;
    if target_name == alias_node.name {
        return None;
    }

    let candidates: Vec<Node> = context
        .get_nodes_by_name(&target_name)
        .into_iter()
        .filter(|n| is_callable_kind(n.kind))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let mut same_file = candidates
        .iter()
        .filter(|n| n.file_path == alias_node.file_path);
    match (same_file.next(), same_file.next()) {
        // Exactly one same-file callable - the unambiguous local hit.
        (Some(only), None) => return Some(only.clone()),
        // Two or more same-file callables - ambiguous, no hop.
        (Some(_), Some(_)) => return None,
        // No same-file callable - fall through to the cross-file rule.
        (None, _) => {}
    }

    // Cross-file is accepted only when the name is unique project-wide.
    if candidates.len() == 1 {
        Some(candidates.into_iter().next().expect("len == 1"))
    } else {
        None
    }
}

/// Local export clauses: `export { realImpl as alias }` / `export { realImpl }`
/// with no `from` source.
///
/// `extract_re_exports` only models the `export ... from './other'` form, so a
/// local clause leaves the exported name bound to nothing the export index
/// knows - importing `alias` matches no declaration and resolution falls
/// through to the name-matcher, which cannot cross the rename (a false 0
/// callers).
///
/// Type-only specifiers are skipped: they carry no runtime call.
pub fn extract_local_export_aliases(content: &str) -> Vec<LocalExportAlias> {
    // `export { ... }` NOT followed by `from` - the `from` form is a re-export.
    // The Rust `regex` crate has no lookahead, so the negative `(?!\s*from)`
    // is emulated by matching the terminator and checking it is not `from`.
    static CLAUSE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"export\s*\{([^}]*)\}\s*([;\n]|from\b)").expect("valid regex")
    });
    static RENAMED_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([A-Za-z_$][A-Za-z0-9_$]*)\s+as\s+([A-Za-z_$][A-Za-z0-9_$]*)$")
            .expect("valid regex")
    });
    static PLAIN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").expect("valid regex"));
    static TYPE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^type\s").expect("valid regex"));

    let mut out: Vec<LocalExportAlias> = Vec::new();
    for clause in CLAUSE_RE.captures_iter(content) {
        // Skip the re-export form (`export { ... } from '...'`).
        if &clause[2] == "from" {
            continue;
        }
        for raw in clause[1].split(',') {
            let specifier = raw.trim();
            if specifier.is_empty() || TYPE_RE.is_match(specifier) {
                continue;
            }
            if let Some(renamed) = RENAMED_RE.captures(specifier) {
                out.push(LocalExportAlias {
                    local_name: renamed[1].to_string(),
                    exported_name: renamed[2].to_string(),
                });
                continue;
            }
            if PLAIN_RE.is_match(specifier) {
                out.push(LocalExportAlias {
                    local_name: specifier.to_string(),
                    exported_name: specifier.to_string(),
                });
            }
        }
    }
    out
}

/// A name introduced by a local `export { local as exported }` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExportAlias {
    /// The declared symbol the clause forwards to.
    pub local_name: String,
    /// The name the clause exports it under.
    pub exported_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_target_name_reads_a_bare_binding() {
        assert_eq!(
            alias_target_name(Some("= realImpl"), None).as_deref(),
            Some("realImpl")
        );
        assert_eq!(
            alias_target_name(Some("= realImpl;"), None).as_deref(),
            Some("realImpl")
        );
        assert_eq!(
            alias_target_name(Some("= realImpl as SomeType"), None).as_deref(),
            Some("realImpl")
        );
    }

    #[test]
    fn alias_target_name_rejects_genuine_initializers() {
        // A call, a literal, an arrow - all genuine definitions, not aliases.
        assert_eq!(alias_target_name(Some("= realImpl()"), None), None);
        assert_eq!(alias_target_name(Some("= 42"), None), None);
        assert_eq!(alias_target_name(Some("= () => realImpl()"), None), None);
        assert_eq!(alias_target_name(None, None), None);
    }

    #[test]
    fn alias_target_name_reads_object_literal_property() {
        assert_eq!(
            alias_target_name(Some("= { run: realImpl }"), Some("run")).as_deref(),
            Some("realImpl")
        );
        // ES shorthand: `{ realImpl }` binds `realImpl` to the same-named symbol.
        assert_eq!(
            alias_target_name(Some("= { realImpl }"), Some("realImpl")).as_deref(),
            Some("realImpl")
        );
        // A missing member yields nothing.
        assert_eq!(
            alias_target_name(Some("= { run: realImpl }"), Some("walk")),
            None
        );
    }

    #[test]
    fn extract_local_export_aliases_reads_clauses_but_skips_re_exports() {
        let content = concat!(
            "function realImpl() {}\n",
            "export { realImpl as aliasName };\n",
            "export { plain };\n",
            "export { type OnlyType };\n",
            "export { forwarded } from './other';\n",
        );
        let out = extract_local_export_aliases(content);
        assert_eq!(
            out,
            vec![
                LocalExportAlias {
                    local_name: "realImpl".into(),
                    exported_name: "aliasName".into(),
                },
                LocalExportAlias {
                    local_name: "plain".into(),
                    exported_name: "plain".into(),
                },
            ]
        );
    }
}
