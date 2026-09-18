//! Where one line of a Rust fn body binds a name.
//!
//! Only the forms that introduce a local are recognised: `let` (and
//! `static`/`const` items), `if let`/`while let`, `for`, closure parameters,
//! and match-arm patterns. Anything that binds the name without spelling its
//! type (`for x in`, `Some(x) =>`, `|x|`) is reported as opaque, which ends
//! the search: the nearer binding shadows every earlier one, so reading past
//! it would name the wrong local's type.

use std::borrow::Cow;

use super::types::{is_ident_byte, split_top_level, type_colon};

/// How many lines a closure's parameter list may span.
const MAX_PARAM_LINES: usize = 8;

/// How a line binds the name being searched for.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Binding<'a> {
    /// `let x = …`, `let x: T = …`, or `let x: T;`. `init` is the byte
    /// offset in the line where the initializer starts.
    Let {
        annotation: Option<&'a str>,
        init: Option<usize>,
    },
    /// `let Some(x) = …`, `if let Ok(x) = …`: the initializer's type with
    /// `Option`/`Result` unwrapped. `init` is as for [`Binding::Let`].
    Unwrapped { init: usize },
    /// `let (a, x) = …` or `let (a, x): (A, X) = …`: element `position` of
    /// the tuple the annotation or initializer has. `init` is as for
    /// [`Binding::Let`].
    Tuple {
        annotation: Option<&'a str>,
        init: usize,
        position: usize,
    },
    /// `for x in …` (or `for (i, x) in …`, element `position` of the
    /// item): an item of the iterable starting at byte `init`.
    Item {
        init: usize,
        position: Option<usize>,
    },
    /// Field `position` of the tuple variant `variant` (`Backend::Tty(tty)
    /// =>`, `if let Kind::Leaf(x) = …`): the type its declaration writes.
    Variant { variant: &'a str, position: usize },
    /// Untyped parameter `param` of the closure whose `|` is at byte `open`
    /// (`|x|`, or element `position` of `|(k, x)|`): what the method the
    /// closure is passed to hands it.
    Param {
        open: usize,
        param: usize,
        position: Option<usize>,
    },
    /// `|x: T|`.
    Typed(Cow<'a, str>),
    /// A binding whose type the line does not spell.
    Opaque,
}

/// The binding of `name` in `line` nearest the line's end, if any. A
/// closure parameter list left open on `line` continues into `next_lines`.
pub(super) fn binding_in_line<'a>(
    line: &'a str,
    next_lines: &[&str],
    name: &str,
) -> Option<Binding<'a>> {
    nearest_binding(line, next_lines, name).map(|(_, binding)| binding)
}

/// [`binding_in_line`] with the byte offset where the binding form starts:
/// its `let`/`static`/`const`/`for` keyword, the closure's opening `|`, or
/// the match arm's `=>`.
pub(super) fn nearest_binding<'a>(
    line: &'a str,
    next_lines: &[&str],
    name: &str,
) -> Option<(usize, Binding<'a>)> {
    let mut nearest: Option<(usize, Binding<'a>)> = None;
    let mut consider = |position: usize, binding: Binding<'a>| {
        if nearest.as_ref().is_none_or(|(seen, _)| position >= *seen) {
            nearest = Some((position, binding));
        }
    };
    for (position, params) in closure_params(line, next_lines) {
        match closure_binding(params, name) {
            Some(Binding::Param {
                param,
                position: element,
                ..
            }) => consider(
                position,
                Binding::Param {
                    open: position,
                    param,
                    position: element,
                },
            ),
            Some(binding) => consider(position, binding),
            None => {}
        }
    }
    if word_positions(line, name).next().is_some() {
        for keyword in ["let", "static", "const"] {
            for position in word_positions(line, keyword) {
                if let Some(binding) = let_binding(line, position + keyword.len(), name) {
                    consider(position, binding);
                }
            }
        }
        for position in word_positions(line, "for") {
            if let Some(binding) = for_binding(line, position + 3, name) {
                consider(position, binding);
            }
        }
        if let Some((arrow, pattern)) = arm_pattern(line) {
            if binds(pattern, name) {
                let pattern = pattern.trim().trim_start_matches('|').trim_start();
                let binding = variant_position(pattern, name).map_or(
                    Binding::Opaque,
                    |(variant, position)| Binding::Variant { variant, position },
                );
                consider(arrow, binding);
            }
        }
    }
    nearest
}

/// Byte positions where `word` occurs as a whole identifier.
pub(super) fn word_positions<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    text.match_indices(word).filter_map(move |(start, _)| {
        let end = start + word.len();
        let before = start.checked_sub(1).map(|index| bytes[index]);
        let bounded = before.is_none_or(|byte| !is_ident_byte(byte))
            && bytes.get(end).is_none_or(|byte| !is_ident_byte(*byte));
        bounded.then_some(start)
    })
}

/// `let <pattern>[: T] [= init]` (or a `static`/`const` item), the pattern
/// starting after the keyword ending at `keyword_end`.
fn let_binding<'a>(line: &'a str, keyword_end: usize, name: &str) -> Option<Binding<'a>> {
    let after = &line[keyword_end..];
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let start = keyword_end + (after.len() - after.trim_start().len());
    let rest = &line[start..];
    let (head_end, has_init) = match assignment(rest) {
        Some(eq) => (eq, true),
        None => (rest.find(';').unwrap_or(rest.len()), false),
    };
    let head = &rest[..head_end];
    let (pattern, annotation) = match type_colon(head) {
        Some(colon) => (&head[..colon], Some(head[colon + 1..].trim())),
        None => (head, None),
    };
    let pattern = strip_binding_mode(pattern);
    let init = has_init.then_some(start + head_end + 1);
    if pattern == name {
        return Some(Binding::Let {
            annotation: annotation.filter(|text| !text.is_empty()),
            init,
        });
    }
    let unwrapped = ["Some", "Ok"].iter().any(|variant| {
        pattern
            .strip_prefix(variant)
            .and_then(|inner| inner.trim_start().strip_prefix('('))
            .and_then(|inner| inner.strip_suffix(')'))
            .is_some_and(|inner| strip_binding_mode(inner) == name)
    });
    if !unwrapped {
        if let Some((variant, position)) = variant_position(pattern, name) {
            return Some(Binding::Variant { variant, position });
        }
    }
    match init {
        Some(init) if unwrapped && annotation.is_none() => Some(Binding::Unwrapped { init }),
        Some(init) => match tuple_position(pattern, name) {
            Some(position) => Some(Binding::Tuple {
                annotation: annotation.filter(|text| !text.is_empty()),
                init,
                position,
            }),
            None => binds(pattern, name).then_some(Binding::Opaque),
        },
        None => binds(pattern, name).then_some(Binding::Opaque),
    }
}

/// The tuple-variant pattern `Kind::Leaf(a, name)` (or `Leaf(name)`)
/// binding `name` as a plain field: the variant's path and the field's
/// position. `Some(..)`/`Ok(..)` and alternatives (`A(x) | B(x)`) are not.
fn variant_position<'a>(pattern: &'a str, name: &str) -> Option<(&'a str, usize)> {
    let pattern = pattern.split(" if ").next()?.trim();
    let open = pattern.find('(')?;
    let (path, fields) = (pattern[..open].trim(), &pattern[open..]);
    let last = path.rsplit("::").next()?;
    let is_path = path.split("::").all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    });
    if !is_path
        || !last.starts_with(|c: char| c.is_ascii_uppercase())
        || matches!(path, "Some" | "Ok" | "Err")
    {
        return None;
    }
    let inner = fields.strip_prefix('(')?.strip_suffix(')')?;
    let position = tuple_position(&format!("({inner})"), name)?;
    Some((path, position))
}

/// Where `name` is bound in the flat tuple pattern `(a, mut name, _)`.
fn tuple_position(pattern: &str, name: &str) -> Option<usize> {
    let inner = pattern.strip_prefix('(')?.strip_suffix(')')?;
    split_top_level(inner, b',')
        .iter()
        .position(|element| strip_binding_mode(element) == name)
}

/// `mut x`, `ref x`, `ref mut x` -> `x`.
fn strip_binding_mode(pattern: &str) -> &str {
    let mut pattern = pattern.trim();
    for mode in ["ref ", "mut "] {
        pattern = pattern.strip_prefix(mode).map_or(pattern, str::trim_start);
    }
    pattern
}

/// The `=` of an assignment in `text` (not `==`, `=>`, `<=`, `>=`, `!=`).
pub(super) fn assignment(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find(|&index| {
        bytes[index] == b'='
            && !matches!(bytes.get(index + 1), Some(b'=' | b'>'))
            && !matches!(
                index.checked_sub(1).map(|before| bytes[before]),
                Some(b'=' | b'<' | b'>' | b'!')
            )
    })
}

/// `for <pattern> in <iterable>` binding `name`, the `for` keyword ending at
/// `keyword_end`: an item of the iterable when the pattern is `name` or a
/// flat tuple holding it (`for (i, name) in …`), else opaque.
fn for_binding<'a>(line: &'a str, keyword_end: usize, name: &str) -> Option<Binding<'a>> {
    let after_for = &line[keyword_end..];
    if !after_for.starts_with(char::is_whitespace) {
        return None;
    }
    let end = word_positions(after_for, "in").next()?;
    let pattern = &after_for[..end];
    if !binds(pattern, name) {
        return None;
    }
    let init = keyword_end + end + 2;
    let pattern = strip_binding_mode(pattern);
    if pattern == name {
        return Some(Binding::Item {
            init,
            position: None,
        });
    }
    Some(match tuple_position(pattern, name) {
        Some(position) => Binding::Item {
            init,
            position: Some(position),
        },
        None => Binding::Opaque,
    })
}

/// The parameter lists of closures opened on this line, with their offsets.
fn closure_params<'a>(line: &'a str, next_lines: &[&str]) -> Vec<(usize, Cow<'a, str>)> {
    let bytes = line.as_bytes();
    let mut params = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'|' || !opens_closure(&line[..index]) {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'|') {
            index += 2;
            continue;
        }
        let Some(length) = line[index + 1..].find('|') else {
            if let Some(spanning) = spanning_params(&line[index + 1..], next_lines) {
                params.push((index, Cow::Owned(spanning)));
            }
            break;
        };
        params.push((index, Cow::Borrowed(&line[index + 1..index + 1 + length])));
        index += length + 2;
    }
    params
}

/// A parameter list continued past its line: `|a: &A,` then `b: &B| …`.
fn spanning_params(start: &str, next_lines: &[&str]) -> Option<String> {
    let mut params = start.to_string();
    for line in next_lines.iter().take(MAX_PARAM_LINES) {
        params.push('\n');
        match line.find('|') {
            Some(end) => {
                params.push_str(&line[..end]);
                return Some(params);
            }
            None => params.push_str(line),
        }
    }
    None
}

/// A `|` opens a closure when it starts an expression: after an opening
/// bracket, a separator, an assignment, or `move`/`return`.
fn opens_closure(before: &str) -> bool {
    let before = before.trim_end();
    let before = before.strip_suffix("move").map_or(before, str::trim_end);
    before.is_empty()
        || before.ends_with("return")
        || before.ends_with(['(', ',', '=', '{', '[', ';', ':'])
}

fn closure_binding<'a>(params: Cow<'a, str>, name: &str) -> Option<Binding<'a>> {
    match params {
        Cow::Borrowed(params) => param_binding(params, name),
        Cow::Owned(params) => param_binding(&params, name).map(|binding| match binding {
            Binding::Typed(annotation) => Binding::Typed(Cow::Owned(annotation.into_owned())),
            Binding::Param {
                open,
                param,
                position,
            } => Binding::Param {
                open,
                param,
                position,
            },
            _ => Binding::Opaque,
        }),
    }
}

/// The binding of `name` in one closure's parameter list; an untyped one
/// is [`Binding::Param`] with `open` left for the caller to fill in.
fn param_binding<'a>(params: &'a str, name: &str) -> Option<Binding<'a>> {
    split_top_level(params, b',')
        .into_iter()
        .enumerate()
        .find_map(|(index, param)| {
            let (pattern, annotation) = match type_colon(param) {
                Some(colon) => (&param[..colon], Some(param[colon + 1..].trim())),
                None => (param, None),
            };
            let pattern = strip_binding_mode(pattern.trim().trim_start_matches('&'));
            let untyped = |position| Binding::Param {
                open: 0,
                param: index,
                position,
            };
            if pattern == name {
                return Some(annotation.map_or(untyped(None), |annotation| {
                    Binding::Typed(Cow::Borrowed(annotation))
                }));
            }
            if annotation.is_none() {
                if let Some(position) = tuple_position(pattern, name) {
                    return Some(untyped(Some(position)));
                }
            }
            binds(pattern, name).then_some(Binding::Opaque)
        })
}

/// The pattern of a match arm written on this line (`Some(x) if … =>`),
/// with the offset of its `=>`.
fn arm_pattern(line: &str) -> Option<(usize, &str)> {
    let arrow = line.find("=>")?;
    let pattern = &line[..arrow];
    // `match v { Some(x) => …` on one line: the arm starts after the brace.
    let arm_start = word_positions(pattern, "match")
        .last()
        .and_then(|position| {
            pattern[position..]
                .find('{')
                .map(|brace| position + brace + 1)
        })
        .unwrap_or(0);
    Some((arrow, &pattern[arm_start..]))
}

/// `name` occurs in `pattern` as a binding: not a path segment, a field
/// name (`Foo { name: v }`), a call, a macro, or a method receiver.
pub(super) fn binds(pattern: &str, name: &str) -> bool {
    word_positions(pattern, name).any(|start| {
        let before = pattern[..start].trim_end();
        let after = pattern[start + name.len()..].trim_start();
        !before.ends_with(['.', ':']) && !after.starts_with(['(', '.', '!', ':', '{'])
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{Binding, binding_in_line};

    fn binding<'a>(line: &'a str, name: &str) -> Option<Binding<'a>> {
        binding_in_line(line, &[], name)
    }

    #[test]
    fn reads_let_bindings() {
        assert_eq!(
            binding("    let mut graph: Graph<u8> = build();", "graph"),
            Some(Binding::Let {
                annotation: Some("Graph<u8>"),
                init: Some(30)
            })
        );
        assert_eq!(
            binding("let x = a == b;", "x"),
            Some(Binding::Let {
                annotation: None,
                init: Some(7)
            })
        );
        assert_eq!(
            binding("    static RE: LazyLock<Regex> =", "RE"),
            Some(Binding::Let {
                annotation: Some("LazyLock<Regex>"),
                init: Some(32)
            })
        );
        assert_eq!(binding("const fn x() -> u8 {", "x"), None);
        assert_eq!(
            binding("let (a, mut x) = pair;", "x"),
            Some(Binding::Tuple {
                annotation: None,
                init: 16,
                position: 1
            })
        );
        assert_eq!(
            binding("let (a, x): (A, X) = pair;", "x"),
            Some(Binding::Tuple {
                annotation: Some("(A, X)"),
                init: 20,
                position: 1
            })
        );
        assert_eq!(
            binding("let (a, (b, x)) = pair;", "x"),
            Some(Binding::Opaque)
        );
        assert_eq!(binding("let y = x.len();", "x"), None);
    }

    #[test]
    fn reads_unwrapping_bindings() {
        assert_eq!(
            binding("    let Some(target) = find(id) else {", "target"),
            Some(Binding::Unwrapped { init: 22 })
        );
        assert_eq!(
            binding("if let Ok(ref mut file) = open(p) {", "file"),
            Some(Binding::Unwrapped { init: 25 })
        );
        assert_eq!(
            binding("if let Some((a, x)) = map.get(k) {", "x"),
            Some(Binding::Opaque)
        );
    }

    #[test]
    fn reads_pattern_bindings() {
        assert_eq!(
            binding("for (i, x) in xs.iter() {", "x"),
            Some(Binding::Item {
                init: 13,
                position: Some(1)
            })
        );
        assert_eq!(
            binding("    for x in &self.nodes {", "x"),
            Some(Binding::Item {
                init: 12,
                position: None
            })
        );
        assert_eq!(
            binding("for Pair(a, x) in xs {", "x"),
            Some(Binding::Opaque)
        );
        assert_eq!(binding("impl Tr for X {", "X"), None);
        assert_eq!(
            binding("xs.iter().map(|x: &Node| x.len())", "x"),
            Some(Binding::Typed(Cow::Borrowed("&Node")))
        );
        assert_eq!(
            binding("xs.iter().map(|x| x.len())", "x"),
            Some(Binding::Param {
                open: 14,
                param: 0,
                position: None
            })
        );
        assert_eq!(
            binding("m.iter().max_by(|(ka, a), (kb, b)| a.cmp(b))", "kb"),
            Some(Binding::Param {
                open: 16,
                param: 1,
                position: Some(0)
            })
        );
        assert_eq!(binding("if a || x.is_empty() || b {", "x"), None);
        assert_eq!(
            binding("    Some(x) => x.len(),", "x"),
            Some(Binding::Opaque)
        );
        assert_eq!(binding("    Kind::A if x.ok() => 1,", "x"), None);
        assert_eq!(
            binding(
                "    Backend::Headless(headless) => headless.render(),",
                "headless"
            ),
            Some(Binding::Variant {
                variant: "Backend::Headless",
                position: 0
            })
        );
        assert_eq!(
            binding("if let Kind::Pair(a, ref x) = value {", "x"),
            Some(Binding::Variant {
                variant: "Kind::Pair",
                position: 1
            })
        );
        assert_eq!(
            binding("    A(x) | B(x) => x.len(),", "x"),
            Some(Binding::Opaque)
        );
        assert_eq!(binding("    Foo { x: v } => v,", "x"), None);
    }

    #[test]
    fn reads_closure_params_spanning_lines() {
        let next = ["        b: &Graph,", "        (c, d): (u8, u8)| -> bool {"];
        assert_eq!(
            binding_in_line("    let edge = |a: &Graph,", &next, "b"),
            Some(Binding::Typed(Cow::Owned("&Graph".into())))
        );
        assert_eq!(
            binding_in_line("    let edge = |a: &Graph,", &next, "d"),
            Some(Binding::Opaque)
        );
    }
}
