//! Whether a bare call names a local of the enclosing fn.
//!
//! `stop()` with a parameter `stop: &dyn Fn() -> bool`, or `probe(x)` after
//! `let probe = |s| …;`, runs that local closure or fn pointer whatever the
//! project defines under the same name. The scan walks up from the call to
//! the enclosing fn's first line reading the bindings [`nearest_binding`]
//! recognises, and counts one only while its scope is still open at the
//! call:
//!
//! - `let x = …;` (and `static`/`const`) binds from the `;` ending the
//!   statement to the end of the enclosing block;
//! - `for x in … {`, `if let … {`, `while let … {` bind inside the block
//!   they open;
//! - a closure's `|x|` and a match arm's `x =>` bind in the body that
//!   follows, braced or not, until a bracket, `,`, or `;` ends it.
//!
//! Parameters of the enclosing fn are always in scope.

use super::bindings::{binds, nearest_binding};
use super::types::{is_ident_byte, signature_params};
use super::{enclosing_fn, prefix};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// How many lines above the call are searched for a binding.
const MAX_SCAN_LINES: usize = 2_000;

/// `name`, called bare at `reference`, is a parameter or an in-scope local
/// binding of the enclosing fn.
pub(in crate::resolution::name_matcher) fn is_local_at_call(
    name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(scope) = caller_fn(reference, context) else {
        return false;
    };
    let is_param = scope.signature.as_deref().is_some_and(|signature| {
        signature_params(signature)
            .iter()
            .any(|(pattern, _)| binds(pattern, name))
    });
    if is_param {
        return true;
    }
    let Some(source) = context.read_file_arc(&reference.file_path) else {
        return false;
    };
    let call_line = (reference.line as usize).saturating_sub(1);
    let first_line = (scope.start_line as usize)
        .saturating_sub(1)
        .max(call_line.saturating_sub(MAX_SCAN_LINES));
    if first_line > call_line {
        return false;
    }
    let lines: Vec<&str> = source
        .split('\n')
        .skip(first_line)
        .take(call_line - first_line + 1)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if lines.len() != call_line - first_line + 1 {
        return false;
    }
    let column = reference.column as usize;
    let last = lines.len() - 1;
    (0..=last).rev().any(|index| {
        let line = if index == last {
            prefix(lines[index], column)
        } else {
            lines[index]
        };
        if !line.contains(name) {
            return false;
        }
        let next_lines = if index == last {
            &[][..]
        } else {
            &lines[index + 1..]
        };
        nearest_binding(line, next_lines, name).is_some_and(|(position, _)| {
            // A binding written in a line comment binds nothing.
            !line[..position].contains("//") && in_scope_at_call(&lines, index, position, column)
        })
    })
}

/// The innermost Rust fn or method the call is in: the reference's caller
/// when it is one, else the fn whose lines hold the call.
pub(super) fn caller_fn(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    context
        .get_node_by_id(&reference.from_node_id)
        .filter(|node| {
            node.language == Language::Rust
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= reference.line
                && node.end_line.max(node.start_line) >= reference.line
        })
        .or_else(|| enclosing_fn(reference, context))
}

/// The binding starting at byte `position` of `lines[index]` is still in
/// scope at byte `column` of the last line.
fn in_scope_at_call(lines: &[&str], index: usize, position: usize, column: usize) -> bool {
    let last = lines.len() - 1;
    let line = lines[index];
    let form = Form::at(&line[..position], &line[position..]);
    let mut text = String::new();
    if index == last {
        text.push_str(prefix(line, column).get(position..).unwrap_or_default());
    } else {
        text.push_str(&line[position..]);
        for between in &lines[index + 1..last] {
            text.push('\n');
            text.push_str(between);
        }
        text.push('\n');
        text.push_str(prefix(lines[last], column));
    }
    form.in_scope_after(&text)
}

/// How far a binding's scope reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `let`/`static`/`const`: after the statement, to the end of the block.
    Statement,
    /// `for`/`if let`/`while let`: inside the block the header opens.
    Block,
    /// A closure or match arm: the body that follows, braced or not.
    Body,
}

impl Form {
    /// The form of the binding written at `at`, after `before` on its line.
    fn at(before: &str, at: &str) -> Form {
        if at.starts_with("let") {
            let before = before.trim_end();
            let conditional = ["if", "while", "&&", "||"]
                .iter()
                .any(|word| before.ends_with(word));
            return if conditional {
                Form::Block
            } else {
                Form::Statement
            };
        }
        if at.starts_with("static") || at.starts_with("const") {
            return Form::Statement;
        }
        if at.starts_with("for") {
            return Form::Block;
        }
        Form::Body
    }

    /// The binding whose form starts `text` is in scope at its end.
    fn in_scope_after(self, text: &str) -> bool {
        let mut depth = 0usize;
        let mut opened = false;
        let mut finished = false;
        for byte in code_bytes(text) {
            match byte {
                b'(' | b'[' | b'{' => {
                    opened |= byte == b'{' && depth == 0;
                    depth += 1;
                }
                b')' | b']' | b'}' => {
                    let Some(outer) = depth.checked_sub(1) else {
                        // The block holding the binding closed.
                        return false;
                    };
                    depth = outer;
                    if depth == 0 && opened && self != Form::Statement {
                        return false;
                    }
                }
                b';' if depth == 0 => match self {
                    Form::Statement => finished = true,
                    Form::Block | Form::Body if !opened => return false,
                    _ => {}
                },
                b',' if depth == 0 && self == Form::Body && !opened => return false,
                _ => {}
            }
        }
        match self {
            Form::Statement => finished,
            Form::Block => opened,
            Form::Body => true,
        }
    }
}

/// The bytes of `text` that are code: string, char, and comment contents
/// (a `"{}"` format string, a `'{'`) are skipped.
fn code_bytes(text: &str) -> impl Iterator<Item = u8> + '_ {
    let bytes = text.as_bytes();
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < bytes.len() {
            let byte = bytes[index];
            let next = bytes.get(index + 1).copied();
            match byte {
                b'/' if next == Some(b'/') => index = line_end(bytes, index),
                b'/' if next == Some(b'*') => {
                    index = block_comment_end(bytes, index);
                }
                b'"' => index = string_end(bytes, index + 1),
                b'r' if raw_string_start(bytes, index) => {
                    index = raw_string_end(bytes, index + 1);
                }
                b'\'' => index = char_literal_end(bytes, index),
                _ => {
                    index += 1;
                    return Some(byte);
                }
            }
        }
        None
    })
}

/// The index of the newline ending the line `start` is on.
fn line_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| start + offset)
}

/// The index past a `/* … */` comment starting at `start` (nesting counted).
fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut index = start;
    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'/', b'*') => {
                depth += 1;
                index += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

/// The index past the `"` closing a string whose contents start at `index`.
fn string_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// `r"` or `r#…"` at `index`, starting a token or after a `b` that does
/// (`br"…"`), not ending an identifier (`bar"`).
fn raw_string_start(bytes: &[u8], index: usize) -> bool {
    let token_start = |at: usize| {
        at.checked_sub(1)
            .is_none_or(|before| !is_ident_byte(bytes[before]))
    };
    let starts =
        token_start(index) || (index >= 1 && bytes[index - 1] == b'b' && token_start(index - 1));
    if !starts {
        return false;
    }
    let hashes = bytes[index + 1..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    bytes.get(index + 1 + hashes) == Some(&b'"')
}

/// The index past a raw string whose `#`s start at `index`.
fn raw_string_end(bytes: &[u8], index: usize) -> usize {
    let hashes = bytes[index..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    let mut at = index + hashes + 1;
    while at < bytes.len() {
        if bytes[at] == b'"'
            && bytes[at + 1..]
                .iter()
                .take(hashes)
                .filter(|byte| **byte == b'#')
                .count()
                == hashes
        {
            return at + 1 + hashes;
        }
        at += 1;
    }
    bytes.len()
}

/// The index past a char literal at `start` (`'x'`, `'\n'`, `'\''`, `'é'`);
/// a lifetime or label (`'a`, `'outer:`) is skipped as its quote alone.
fn char_literal_end(bytes: &[u8], start: usize) -> usize {
    match bytes.get(start + 1) {
        Some(b'\\') => {
            // Past the escaped char, so `'\''` ends at its second quote.
            let from = (start + 3).min(bytes.len());
            bytes[from..]
                .iter()
                .position(|byte| *byte == b'\'')
                .map_or(bytes.len(), |offset| from + offset + 1)
        }
        Some(_) => {
            // One char, possibly several bytes, then the closing quote.
            let width = bytes[start + 1..]
                .iter()
                .skip(1)
                .take_while(|byte| (**byte & 0xC0) == 0x80)
                .count()
                + 1;
            if bytes.get(start + 1 + width) == Some(&b'\'') {
                start + 2 + width
            } else {
                start + 1
            }
        }
        None => start + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{Form, code_bytes};

    fn code(text: &str) -> String {
        code_bytes(text).map(char::from).collect()
    }

    #[test]
    fn code_bytes_skip_literals_and_comments() {
        assert_eq!(code(r#"f("{}", '{', x) // }"#), "f(, , x) ");
        assert_eq!(code("a /* { */ b"), "a  b");
        assert_eq!(code(r##"g(r#"{"#, 'a, '\'')"##), "g(, a, )");
        assert_eq!(code("h('é')"), "h()");
    }

    #[test]
    fn statement_bindings_start_after_their_semicolon() {
        assert!(Form::Statement.in_scope_after("let stop = |x| x > 1;\n    "));
        // The call is in the binding's own initializer.
        assert!(!Form::Statement.in_scope_after("let paths = "));
        // The block holding the binding closed before the call.
        assert!(!Form::Statement.in_scope_after("let stop = 1;\n    }\n    "));
        assert!(Form::Statement.in_scope_after("let Some(stop) = f() else { return; };\n"));
    }

    #[test]
    fn block_bindings_live_in_their_block() {
        assert!(Form::Block.in_scope_after("for stop in stops {\n        "));
        // `for stop in stop()`: the pattern is not in scope in its iterator.
        assert!(!Form::Block.in_scope_after("for stop in "));
        assert!(!Form::Block.in_scope_after("for stop in stops {\n    }\n    "));
        assert!(!Form::Block.in_scope_after("if let Some(stop) = s { a } else { "));
    }

    #[test]
    fn closure_and_arm_bindings_live_in_their_body() {
        assert!(Form::Body.in_scope_after("|stop| "));
        assert!(Form::Body.in_scope_after("|stop| {\n        "));
        assert!(!Form::Body.in_scope_after("|stop| stop.len());\n    "));
        assert!(!Form::Body.in_scope_after("=> stop.len(),\n        None => "));
        assert!(Form::Body.in_scope_after("=>\n            "));
    }
}
