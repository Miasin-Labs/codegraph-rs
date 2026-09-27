//! Risk sites and declaration facts read from each file's syntax tree, in
//! one walk per file, attributed to the innermost indexed function.
//!
//! What counts as a site is per language ([`SiteRules::for_language`]); the
//! walker only reads the table. Declaration facts the index does not keep —
//! generic bounds, the trait of an `impl … for`, a restricted visibility
//! like `pub(crate)` — come from the same walk.

use std::collections::HashMap;

use tree_sitter::Node;

use super::model::SiteCounts;
use crate::analyze::bugs::{FnSpan, Project};
use crate::ensure_sufficient_stack;
use crate::types::Language;

/// Which syntax is a risk site, and where declarations keep what the index
/// leaves out.
pub struct SiteRules {
    /// Function or method declarations (with a `name` field).
    pub function_kinds: &'static [&'static str],
    pub unsafe_kinds: &'static [&'static str],
    pub index_kinds: &'static [&'static str],
    /// Macros that panic by design (`panic!`, `unreachable!`, `assert!`).
    pub panic_macros: &'static [&'static str],
    pub macro_kind: &'static str,
    /// Methods that panic on the unexpected case (`unwrap`, `expect`).
    pub panic_methods: &'static [&'static str],
    pub call_kind: &'static str,
    pub arithmetic_kinds: &'static [&'static str],
    /// Operators of `arithmetic_kinds` that can overflow.
    pub overflow_ops: &'static [&'static str],
    pub loop_kinds: &'static [&'static str],
    /// A block of methods (`impl_item`) and its trait field (`trait`).
    pub container_kind: &'static str,
    pub container_trait_field: &'static str,
    /// Children of a declaration holding generic parameters and bounds.
    pub generic_kinds: &'static [&'static str],
    pub visibility_kind: &'static str,
    /// Modifier children (`unsafe`, `async`, `const`).
    pub modifier_kind: &'static str,
}

const RUST: SiteRules = SiteRules {
    function_kinds: &["function_item"],
    unsafe_kinds: &["unsafe_block"],
    index_kinds: &["index_expression"],
    panic_macros: &[
        "panic",
        "unreachable",
        "assert",
        "assert_eq",
        "assert_ne",
        "todo",
        "unimplemented",
    ],
    macro_kind: "macro_invocation",
    panic_methods: &["unwrap", "expect", "unwrap_unchecked"],
    call_kind: "call_expression",
    arithmetic_kinds: &["binary_expression", "compound_assignment_expr"],
    overflow_ops: &["+", "-", "*", "<<", "+=", "-=", "*=", "<<="],
    loop_kinds: &["loop_expression", "while_expression"],
    container_kind: "impl_item",
    container_trait_field: "trait",
    generic_kinds: &["type_parameters", "where_clause"],
    visibility_kind: "visibility_modifier",
    modifier_kind: "function_modifiers",
};

impl SiteRules {
    pub fn for_language(language: Language) -> Option<&'static SiteRules> {
        match language {
            Language::Rust => Some(&RUST),
            _ => None,
        }
    }
}

/// What the walk learned about one function.
#[derive(Debug, Clone, Default)]
pub struct FnSyntax {
    pub sites: SiteCounts,
    /// Generic parameters and `where` bounds, as written
    /// (`<R: Read>`, `where R: BufRead`).
    pub generics: String,
    /// The trait of the enclosing `impl Trait for Type`, as written.
    pub impl_trait: Option<String>,
    /// The visibility as written (`pub`, `pub(crate)`), empty if none.
    pub visibility: String,
    /// Modifiers as written (`unsafe`, `async`, `const`…).
    pub modifiers: String,
}

/// Walk every file holding one of `wanted` functions (by id) once, and
/// return what it holds per function id. Files of languages without rules
/// are skipped.
pub fn collect(project: &mut Project, files: &[String]) -> HashMap<String, FnSyntax> {
    let mut out = HashMap::new();
    for file in files {
        let spans: Vec<FnSpan> = project.functions_in(file).to_vec();
        if spans.is_empty() {
            continue;
        }
        let Some(parsed) = project.parsed(file) else {
            continue;
        };
        let Some(rules) = SiteRules::for_language(parsed.language) else {
            continue;
        };
        let mut by_line: HashMap<(u32, &str), usize> = HashMap::new();
        for (index, span) in spans.iter().enumerate() {
            by_line
                .entry((span.start_line, &span.name))
                .or_insert(index);
        }
        let mut per_span: Vec<FnSyntax> = vec![FnSyntax::default(); spans.len()];
        let walker = Walker {
            rules,
            source: parsed.source.as_bytes(),
            by_line: &by_line,
        };
        walker.walk(parsed.tree.root_node(), None, None, &mut per_span);
        for (span, syntax) in spans.iter().zip(per_span) {
            out.insert(span.id.clone(), syntax);
        }
    }
    out
}

struct Walker<'a> {
    rules: &'static SiteRules,
    source: &'a [u8],
    by_line: &'a HashMap<(u32, &'a str), usize>,
}

impl<'a> Walker<'a> {
    fn text(&self, node: Node<'_>) -> &'a str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn walk(
        &self,
        node: Node<'_>,
        current: Option<usize>,
        impl_trait: Option<&'a str>,
        out: &mut [FnSyntax],
    ) {
        ensure_sufficient_stack(|| {
            let kind = node.kind();
            let mut current = current;
            let mut impl_trait = impl_trait;
            if self.rules.function_kinds.contains(&kind) {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| self.text(n))
                    .unwrap_or("");
                let line = node.start_position().row as u32 + 1;
                if let Some(&index) = self.by_line.get(&(line, name)) {
                    current = Some(index);
                    self.declaration(node, impl_trait, &mut out[index]);
                }
                // Nested functions start their own impl context.
                impl_trait = None;
            } else if kind == self.rules.container_kind {
                impl_trait = node
                    .child_by_field_name(self.rules.container_trait_field)
                    .map(|n| self.text(n));
            }
            if let Some(index) = current {
                self.count(node, &mut out[index].sites);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.walk(child, current, impl_trait, out);
            }
        });
    }

    fn declaration(&self, node: Node<'_>, impl_trait: Option<&str>, syntax: &mut FnSyntax) {
        syntax.impl_trait = impl_trait.map(str::to_string);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();
            if kind == self.rules.visibility_kind {
                syntax.visibility = self.text(child).to_string();
            } else if kind == self.rules.modifier_kind {
                syntax.modifiers = self.text(child).to_string();
            } else if self.rules.generic_kinds.contains(&kind) {
                if !syntax.generics.is_empty() {
                    syntax.generics.push(' ');
                }
                syntax.generics.push_str(self.text(child));
            }
        }
    }

    fn count(&self, node: Node<'_>, sites: &mut SiteCounts) {
        let rules = self.rules;
        let kind = node.kind();
        if rules.unsafe_kinds.contains(&kind) {
            sites.unsafe_blocks += 1;
        } else if rules.index_kinds.contains(&kind) {
            sites.index += 1;
        } else if rules.loop_kinds.contains(&kind) {
            sites.loops += 1;
        } else if kind == rules.macro_kind {
            let name = node
                .child_by_field_name("macro")
                .map(|n| self.text(n))
                .unwrap_or("");
            if rules.panic_macros.contains(&name) {
                sites.panics += 1;
            }
        } else if kind == rules.call_kind {
            let method = node
                .child_by_field_name("function")
                .and_then(|f| f.child_by_field_name("field"))
                .map(|n| self.text(n))
                .unwrap_or("");
            if rules.panic_methods.contains(&method) {
                sites.panics += 1;
            }
        } else if rules.arithmetic_kinds.contains(&kind) {
            let op = node
                .child_by_field_name("operator")
                .map(|n| self.text(n))
                .unwrap_or("");
            if rules.overflow_ops.contains(&op) {
                sites.arithmetic += 1;
            }
        }
    }
}
