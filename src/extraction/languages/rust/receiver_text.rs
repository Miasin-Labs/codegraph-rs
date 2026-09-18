//! The receiver of a Rust method call, written down for resolution.
//!
//! A method call on anything but a plain identifier or `self`
//! (`self.cache.borrow_mut().clear()`, `Rule::new(a, b).neg()`) is named by
//! its bare method; resolution infers the receiver's type link by link from
//! this compact text instead of re-reading the file. The text is Rust
//! expression syntax with the noise taken out:
//!
//! - whitespace and comments are gone (`self.a\n  .b()` is `self.a.b()`);
//! - a method call's arguments are `()` when there are none and `(..)`
//!   otherwise; a path call keeps a lone argument it can write
//!   (`Arc::new(Graph::new())`, `Some(x)`) and elides the rest;
//! - an index is `[..]`, a struct literal `Foo{..}`, a macro `name!(..)`,
//!   a string literal `""`.
//!
//! Anything whose type the text cannot carry (a closure, a block, `a + b`,
//! a range) leaves the receiver unrecorded, as does one longer than
//! [`MAX_RECEIVER_BYTES`].

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;

/// Receivers longer than this are not recorded.
pub(crate) const MAX_RECEIVER_BYTES: usize = 200;
/// How many links deep a receiver is written.
const MAX_DEPTH: usize = 24;

/// `receiver` (the value of a method call's `field_expression`) as compact
/// text, or `None` when it cannot be written down.
pub(crate) fn receiver_text(receiver: SyntaxNode<'_>, source: &str) -> Option<String> {
    let mut out = String::new();
    write_expression(receiver, source, &mut out, 0)?;
    (out.len() <= MAX_RECEIVER_BYTES).then_some(out)
}

fn write_expression(
    node: SyntaxNode<'_>,
    source: &str,
    out: &mut String,
    depth: usize,
) -> Option<()> {
    if depth > MAX_DEPTH || out.len() > MAX_RECEIVER_BYTES {
        return None;
    }
    crate::ensure_sufficient_stack(|| write_expression_inner(node, source, out, depth))
}

fn write_expression_inner(
    node: SyntaxNode<'_>,
    source: &str,
    out: &mut String,
    depth: usize,
) -> Option<()> {
    let next = depth + 1;
    match node.kind() {
        "self" | "identifier" | "super" | "crate" => out.push_str(get_node_text(node, source)),
        "scoped_identifier" => out.push_str(&compact(get_node_text(node, source))),
        "integer_literal" | "float_literal" | "boolean_literal" => {
            out.push_str(get_node_text(node, source));
        }
        "string_literal" | "raw_string_literal" => out.push_str("\"\""),
        "char_literal" => out.push_str("'c'"),
        "field_expression" => {
            write_expression(get_child_by_field(node, "value")?, source, out, next)?;
            out.push('.');
            out.push_str(get_node_text(get_child_by_field(node, "field")?, source));
        }
        "call_expression" => write_call(node, source, out, next)?,
        "try_expression" => {
            write_expression(only_operand(node)?, source, out, next)?;
            out.push('?');
        }
        "await_expression" => {
            write_expression(only_operand(node)?, source, out, next)?;
            out.push_str(".await");
        }
        "reference_expression" => {
            let value = get_child_by_field(node, "value")?;
            let mutable = operands(node).any(|child| child.kind() == "mutable_specifier");
            out.push_str(if mutable { "&mut " } else { "&" });
            write_expression(value, source, out, next)?;
        }
        "unary_expression" => {
            if !get_node_text(node, source).starts_with('*') {
                return None;
            }
            out.push('*');
            write_expression(only_operand(node)?, source, out, next)?;
        }
        "parenthesized_expression" => {
            out.push('(');
            write_expression(only_operand(node)?, source, out, next)?;
            out.push(')');
        }
        "index_expression" => {
            write_expression(operands(node).next()?, source, out, next)?;
            out.push_str("[..]");
        }
        "type_cast_expression" => {
            write_expression(get_child_by_field(node, "value")?, source, out, next)?;
            out.push_str(" as ");
            out.push_str(&compact(get_node_text(
                get_child_by_field(node, "type")?,
                source,
            )));
        }
        "struct_expression" => {
            out.push_str(&compact(get_node_text(
                get_child_by_field(node, "name")?,
                source,
            )));
            out.push_str("{..}");
        }
        "macro_invocation" => {
            out.push_str(&compact(get_node_text(
                get_child_by_field(node, "macro")?,
                source,
            )));
            out.push_str("!(..)");
        }
        "array_expression" => out.push_str("[..]"),
        _ => return None,
    }
    Some(())
}

/// `f(..)`, `Type::f(x)`, `recv.m(..)`, `recv.m::<T>()`.
fn write_call(node: SyntaxNode<'_>, source: &str, out: &mut String, depth: usize) -> Option<()> {
    let function = get_child_by_field(node, "function")?;
    let arguments = get_child_by_field(node, "arguments")?;
    let args: Vec<SyntaxNode<'_>> = operands(arguments).collect();
    let (callee, turbofish) = match function.kind() {
        "generic_function" => (
            get_child_by_field(function, "function")?,
            Some(get_child_by_field(function, "type_arguments")?),
        ),
        _ => (function, None),
    };
    match callee.kind() {
        "field_expression" => {
            write_expression(get_child_by_field(callee, "value")?, source, out, depth)?;
            out.push('.');
            out.push_str(get_node_text(get_child_by_field(callee, "field")?, source));
            if let Some(turbofish) = turbofish {
                out.push_str("::");
                out.push_str(&compact(get_node_text(turbofish, source)));
            }
            out.push_str(if args.is_empty() { "()" } else { "(..)" });
        }
        "identifier" | "scoped_identifier" => {
            out.push_str(&compact(get_node_text(function, source)));
            out.push('(');
            match args.as_slice() {
                [] => {}
                [only] => {
                    let mark = out.len();
                    if write_expression(*only, source, out, depth).is_none() {
                        out.truncate(mark);
                        out.push_str("..");
                    }
                }
                _ => out.push_str(".."),
            }
            out.push(')');
        }
        _ => return None,
    }
    Some(())
}

/// The named children of `node` that are not comments.
fn operands<'t>(node: SyntaxNode<'t>) -> impl Iterator<Item = SyntaxNode<'t>> {
    (0..node.named_child_count() as u32)
        .filter_map(move |index| node.named_child(index))
        .filter(|child| {
            !matches!(
                child.kind(),
                "line_comment" | "block_comment" | "attribute_item"
            )
        })
}

/// The one expression of `(e)`, `e?`, `e.await`, `*e`.
fn only_operand(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut operands = operands(node);
    let operand = operands.next()?;
    operands.next().is_none().then_some(operand)
}

/// `text` with whitespace runs collapsed to one space, and none around `::`,
/// `<`, `>`, and `,`.
fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space {
            let joins = |c: char| matches!(c, ':' | '<' | '>' | ',' | '(' | ')');
            if !out.is_empty() && !out.ends_with(joins) && !joins(ch) {
                out.push(' ');
            }
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::receiver_text;
    use crate::extraction::grammars::create_parser;
    use crate::types::Language;

    /// The recorded receiver of the last method call in `body`.
    fn receiver_of(body: &str) -> Option<String> {
        let source = format!("fn f() {{ {body}; }}");
        let mut parser = create_parser(Language::Rust).unwrap();
        let tree = parser.parse(&source, None).unwrap();
        let mut stack = vec![tree.root_node()];
        let mut last = None;
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression" {
                let function = node.child_by_field_name("function").unwrap();
                if function.kind() == "field_expression" && last.is_none() {
                    last = Some(function.child_by_field_name("value").unwrap());
                }
            }
            for index in (0..node.named_child_count() as u32).rev() {
                stack.push(node.named_child(index).unwrap());
            }
        }
        receiver_text(last?, &source)
    }

    #[test]
    fn writes_chains_compactly() {
        let cases = [
            (
                "self.cache.borrow_mut().clear()",
                Some("self.cache.borrow_mut()"),
            ),
            (
                "self.map\n  .lock()\n  .unwrap() // guard\n  .get(k)",
                Some("self.map.lock().unwrap()"),
            ),
            ("Rule::new(a, b).neg(c)", Some("Rule::new(..)")),
            (
                "Arc::new(Graph::new()).load()",
                Some("Arc::new(Graph::new())"),
            ),
            ("x.finish()?.as_u64()", Some("x.finish()?")),
            ("load(path).await.run()", Some("load(path).await")),
            ("(&mut *guard).tick()", Some("(&mut *guard)")),
            ("v[0].len()", Some("v[..]")),
            (
                "names.iter().collect::<Vec<_>>().len()",
                Some("names.iter().collect::<Vec<_>>()"),
            ),
            ("Config { a: 1 }.build()", Some("Config{..}")),
            ("format!(\"{x}\").len()", Some("format!(..)")),
            ("\"text\".len()", Some("\"\"")),
            ("self.0.finish()", Some("self.0")),
            ("(a + b).abs()", None),
            ("(|x| x)(1).run()", None),
        ];
        for (body, expected) in cases {
            assert_eq!(receiver_of(body).as_deref(), expected, "{body}");
        }
    }

    #[test]
    fn long_receivers_are_not_recorded() {
        let long = format!("{}.m()", ["a()"; 80].join("."));
        assert_eq!(receiver_of(&long), None);
    }
}
