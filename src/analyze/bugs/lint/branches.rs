//! `identical-branches`: both sides of an `if`/`else` or a ternary are the
//! same code, or a `match`/`switch` arm runs another arm's body — usually a
//! copy-paste whose edit was never made.
//!
//! Arms sharing a body are common on purpose (`A | B` spelled out, a table
//! mapping several cases to one value), so an arm is reported only as a
//! proven slip: its pattern differs from the first arm's by one word, the
//! body names the first arm's word and never its own, and the name the body
//! should have used exists — `Provisioned => Status::PROVISIONING` beside
//! `Provisioning => …` with `Status::PROVISIONED` defined (a symbol of the
//! index or a word of the file; a qualified `Op::Minus` must be spelled in
//! the file). Catch-all arms (`_`, `default`), empty and diverging bodies
//! (`unreachable!()`, `throw`), bodies using a variable their pattern binds
//! and Go type switches are skipped. Ternaries whose two results are the same
//! literal (`c ? 0 : 0`) are placeholders and skipped too.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::rules::{Alt, ArmBody, DIVERGING_MACROS};
use super::syntax::{
    is_pure,
    named_children,
    path_roots,
    pick,
    position,
    same_tokens,
    split_words,
    text,
    tokens,
    walk,
};
use super::{Ctx, IDENTICAL_ARMS, IDENTICAL_BRANCHES};

pub(super) fn check_if(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let Some(shape) = rules.ifs.iter().find(|s| s.kind == node.kind()) else {
        return;
    };
    let is_if = |n: Node<'_>| rules.ifs.iter().any(|s| s.kind == n.kind());
    let (then_body, else_body) = match shape.alternative {
        Alt::Direct => {
            let (Some(then_body), Some(else_body)) = (
                node.child_by_field_name(shape.consequence),
                node.child_by_field_name("alternative"),
            ) else {
                return;
            };
            (then_body, else_body)
        }
        Alt::Wrapped => {
            let (Some(then_body), Some(clause)) = (
                node.child_by_field_name(shape.consequence),
                node.child_by_field_name("alternative"),
            ) else {
                return;
            };
            let Some(else_body) = named_children(clause, rules).into_iter().next() else {
                return;
            };
            (then_body, else_body)
        }
        Alt::Clauses { elif, else_clause } => {
            let mut cursor = node.walk();
            let alternatives: Vec<Node<'_>> = node
                .children_by_field_name("alternative", &mut cursor)
                .collect();
            let Some((last, before)) = alternatives.split_last() else {
                return;
            };
            if last.kind() != else_clause {
                return;
            }
            let then_body = match before.last() {
                Some(clause) if clause.kind() == elif => {
                    clause.child_by_field_name(shape.consequence)
                }
                Some(_) => None,
                None => node.child_by_field_name(shape.consequence),
            };
            let (Some(then_body), Some(else_body)) = (then_body, last.child_by_field_name("body"))
            else {
                return;
            };
            (then_body, else_body)
        }
    };
    if is_if(else_body)
        || !same_tokens(then_body, else_body, rules, ctx.source)
        || narrows_type(node.child_by_field_name(shape.condition), ctx)
    {
        return;
    }
    if significant(&tokens(then_body, rules, ctx.source)).is_empty() {
        return;
    }
    let (else_line, _) = position(else_body);
    let message = format!(
        "both branches of `if {}` run the same code (`{}`)",
        node.child_by_field_name(shape.condition)
            .map(|c| ctx.snippet(c))
            .unwrap_or_default(),
        ctx.snippet(then_body)
    );
    ctx.report(
        "identical-branches",
        node,
        IDENTICAL_BRANCHES,
        message,
        vec![(else_line, "the else branch".to_string())],
    );
}

pub(super) fn check_ternary(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let Some(shape) = rules.ternaries.iter().find(|t| t.kind == node.kind()) else {
        return;
    };
    let (Some(then_value), Some(else_value)) = (
        pick(node, shape.consequence, rules),
        pick(node, shape.alternative, rules),
    ) else {
        return;
    };
    if then_value.id() == else_value.id()
        || !same_tokens(then_value, else_value, rules, ctx.source)
        || narrows_type(pick(node, shape.condition, rules), ctx)
    {
        return;
    }
    if is_pure(then_value, rules, ctx.source)
        && path_roots(then_value, rules, ctx.source).is_empty()
    {
        // `c ? 0 : 0`: a placeholder for values yet to differ.
        return;
    }
    let message = format!(
        "both results of the conditional are `{}`",
        ctx.snippet(then_value)
    );
    ctx.report(
        "identical-branches",
        node,
        IDENTICAL_BRANCHES,
        message,
        Vec::new(),
    );
}

pub(super) fn check_switch(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let rules = ctx.rules;
    let source = ctx.source;
    let Some(shape) = rules.switches.iter().find(|s| s.kind == node.kind()) else {
        return;
    };
    if rules.typed_switches.contains(&shape.kind) {
        return;
    }
    let container = match shape.arms_in {
        Some(field) => match node.child_by_field_name(field) {
            Some(container) => container,
            None => return,
        },
        None => node,
    };
    struct ArmInfo<'t, 's> {
        arm: Node<'t>,
        body: Vec<Node<'t>>,
        key: Vec<&'s str>,
        pattern: Vec<&'s str>,
    }
    let mut arms: Vec<ArmInfo<'_, '_>> = Vec::new();
    // How often each word appears in the switch's patterns.
    let mut pattern_words: HashMap<&str, usize> = HashMap::new();
    for arm in named_children(container, rules) {
        let Some(arm_shape) = rules.arms.iter().find(|a| a.kind == arm.kind()) else {
            continue;
        };
        let body = arm_body(arm, arm_shape.body, ctx);
        let body_ids: HashSet<usize> = body.iter().map(|n| n.id()).collect();
        let pattern: Vec<&str> = named_children(arm, rules)
            .into_iter()
            .filter(|part| !body_ids.contains(&part.id()))
            .flat_map(|part| tokens(part, rules, source))
            .collect();
        for word in pattern.iter().flat_map(|t| split_words(t)) {
            *pattern_words.entry(word).or_default() += 1;
        }
        let body_tokens: Vec<&str> = body
            .iter()
            .flat_map(|part| tokens(*part, rules, source))
            .collect();
        let key = significant(&body_tokens);
        if is_trivial(&key) || is_catch_all(&pattern) {
            // `_ =>`/`default:` sharing an explicit arm's body is a fallback.
            continue;
        }
        // Variables the pattern binds (lowercase names): a body that uses
        // one works on this arm's payload.
        let bindings: HashSet<&str> = pattern
            .iter()
            .flat_map(|t| split_words(t))
            .filter(|w| w.starts_with(|c: char| c.is_lowercase()))
            .collect();
        if key
            .iter()
            .flat_map(|t| split_words(t))
            .any(|w| bindings.contains(w))
        {
            continue;
        }
        arms.push(ArmInfo {
            arm,
            body,
            key,
            pattern,
        });
    }

    // Arms by body: the first arm with each body, and its pattern words.
    let mut seen: HashMap<&[&str], (Node<'_>, HashSet<String>)> = HashMap::new();
    let mut found = Vec::new();
    for info in &arms {
        let pattern_segments = segments(&info.pattern);
        match seen.get(info.key.as_slice()) {
            Some((first, first_segments)) => {
                let slip = copy_slip(
                    &info.body,
                    first_segments,
                    &pattern_segments,
                    &pattern_words,
                    ctx,
                );
                if let Some(slip) = slip {
                    found.push((info.arm, *first, slip));
                }
            }
            None => {
                seen.insert(info.key.as_slice(), (info.arm, pattern_segments));
            }
        }
    }
    for (arm, first, (written, meant)) in found {
        let (first_line, _) = position(first);
        let message = format!(
            "this arm runs the same code as the arm at line {first_line}, down to `{written}`, \
             which names that arm's case — `{meant}` exists and names this one's: `{}`",
            ctx.snippet(arm)
        );
        ctx.report(
            "identical-branches",
            arm,
            IDENTICAL_ARMS,
            message,
            vec![(first_line, "the arm this body was written for".to_string())],
        );
    }
}

/// A copy-paste slip names the first arm's case in the body and not its
/// own, and the name it should have used exists: `Provisioned =>
/// Status::PROVISIONING` beside `Provisioning => Status::PROVISIONING`, with
/// `Status::PROVISIONED` defined. Arms that share a body on purpose
/// (`"scan" => Backward` beside `"backward" => Backward`) have no such
/// sibling name. Returns (the name written, the sibling that exists).
fn copy_slip(
    body: &[Node<'_>],
    first_segments: &HashSet<String>,
    own_segments: &HashSet<String>,
    pattern_words: &HashMap<&str, usize>,
    ctx: &mut Ctx<'_>,
) -> Option<(String, String)> {
    let only_first: Vec<&String> = first_segments.difference(own_segments).collect();
    let only_own: Vec<&String> = own_segments.difference(first_segments).collect();
    if only_first.len() != 1 || only_own.len() != 1 {
        // A slip swaps one word; patterns apart in many (URL routes, type
        // lists) share a body for reasons of their own.
        return None;
    }
    let rules = ctx.rules;
    let source = ctx.source;
    // Each name, with the path that qualifies it (`Op::` in `Op::Plus`).
    let mut names: Vec<(&str, &str)> = Vec::new();
    for part in body {
        walk(*part, |n| {
            if rules.comments.contains(&n.kind()) {
                return false;
            }
            if n.child_count() == 0
                && (n.kind().ends_with("identifier") || rules.idents.contains(&n.kind()))
            {
                let qualifier = n
                    .parent()
                    .filter(|p| p.start_byte() < n.start_byte())
                    .and_then(|p| source.get(p.start_byte()..n.start_byte()))
                    .filter(|q| q.ends_with("::") || q.ends_with('.'))
                    .unwrap_or("");
                names.push((text(n, source), qualifier));
            }
            true
        });
    }
    let spans_of: Vec<(&str, &str, Vec<(usize, usize)>)> = names
        .iter()
        .map(|(name, qualifier)| (*name, *qualifier, segment_spans(name)))
        .collect();
    // The body must not name this arm's own case anywhere.
    let names_own = spans_of.iter().any(|(name, _, spans)| {
        spans.iter().any(|&(a, b)| {
            only_own
                .iter()
                .any(|own| name[a..b].eq_ignore_ascii_case(own))
        })
    });
    if names_own {
        return None;
    }
    for (name, qualifier, spans) in &spans_of {
        for &(a, b) in spans {
            let segment = &name[a..b];
            if !only_first.iter().any(|f| segment.eq_ignore_ascii_case(f)) {
                continue;
            }
            let whole = a == 0 && b == name.len();
            if whole && segment.chars().all(|c| !c.is_uppercase()) {
                // A lone lowercase word (`map`, `int64`): a keyword or a
                // type, not a case name.
                continue;
            }
            for own in &only_own {
                let sibling = format!("{}{}{}", &name[..a], restyle(own, segment), &name[b..]);
                if whole && !qualifier.is_empty() {
                    // `Op::Plus` → `Op::Minus`: a bare `Minus` exists in
                    // any enum; the qualified name must be spelled here.
                    let qualified = format!("{qualifier}{sibling}");
                    if source.contains(&qualified) {
                        return Some((format!("{qualifier}{name}"), qualified));
                    }
                    continue;
                }
                // It must exist beyond this switch's own patterns (`Minus`
                // in `Kind::Minus =>` proves nothing about `Op::Minus`).
                let in_patterns = pattern_words.get(sibling.as_str()).copied().unwrap_or(0);
                if ctx.known(&sibling, in_patterns) {
                    return Some(((*name).to_string(), sibling));
                }
            }
        }
    }
    None
}

/// `word` in the letter case of `like` (`PROVISIONING` → upper, `Plus` →
/// capitalized, else lower).
fn restyle(word: &str, like: &str) -> String {
    if like.chars().all(|c| !c.is_lowercase()) {
        word.to_uppercase()
    } else if like.starts_with(|c: char| c.is_uppercase()) {
        let mut chars = word.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        word.to_string()
    }
}

/// `typeof x === "string" ? x.length : x.length`: TypeScript checks each
/// branch against a different type, so the same text is two programs.
fn narrows_type(condition: Option<Node<'_>>, ctx: &Ctx<'_>) -> bool {
    condition.is_some_and(|c| {
        tokens(c, ctx.rules, ctx.source)
            .iter()
            .any(|t| matches!(*t, "typeof" | "instanceof"))
    })
}

/// `_`, `default`, `case _`: the catch-all arm.
fn is_catch_all(pattern: &[&str]) -> bool {
    let words: Vec<&str> = pattern
        .iter()
        .copied()
        .filter(|t| !matches!(*t, ":" | "=>" | "->" | "case"))
        .collect();
    matches!(words.as_slice(), [] | ["_"] | ["default"])
}

/// Byte ranges of a name's parts of three letters or more, split at `_`,
/// `$` and lower→upper case changes (`push_web3Type` → `push`, `web3`,
/// `Type`).
fn segment_spans(word: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_lower = false;
    let close = |from: usize, to: usize, out: &mut Vec<(usize, usize)>| {
        if word[from..to].chars().count() >= 3 {
            out.push((from, to));
        }
    };
    for (i, c) in word.char_indices() {
        if c == '_' || c == '$' {
            close(start, i, &mut out);
            start = i + c.len_utf8();
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            close(start, i, &mut out);
            start = i;
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
    }
    close(start, word.len(), &mut out);
    out
}

/// Lowercased name parts of every word in `tokens`.
fn segments(tokens: &[&str]) -> HashSet<String> {
    tokens
        .iter()
        .flat_map(|t| split_words(t))
        .flat_map(|word| {
            segment_spans(word)
                .into_iter()
                .map(move |(a, b)| word[a..b].to_lowercase())
        })
        .collect()
}

fn arm_body<'t>(arm: Node<'t>, how: ArmBody, ctx: &Ctx<'_>) -> Vec<Node<'t>> {
    let rules = ctx.rules;
    match how {
        ArmBody::Field(name) => {
            let mut cursor = arm.walk();
            arm.children_by_field_name(name, &mut cursor).collect()
        }
        ArmBody::Except(kind) => named_children(arm, rules)
            .into_iter()
            .filter(|child| child.kind() != kind)
            .collect(),
        ArmBody::Child(kind) => named_children(arm, rules)
            .into_iter()
            .filter(|child| child.kind() == kind)
            .collect(),
    }
}

/// Tokens that carry meaning: punctuation that only groups is dropped.
fn significant<'s>(tokens: &[&'s str]) -> Vec<&'s str> {
    tokens
        .iter()
        .copied()
        .filter(|t| !matches!(*t, "{" | "}" | "(" | ")" | ";" | "," | "pass"))
        .collect()
}

/// Bodies that say nothing: empty, a diverging stub.
fn is_trivial(significant: &[&str]) -> bool {
    match significant {
        [] | ["break"] | ["continue"] | ["return"] => true,
        [first, "!", ..] if DIVERGING_MACROS.contains(first) => true,
        ["throw" | "raise", ..] => true,
        _ => false,
    }
}
