//! Concurrency / control-plane lint over the tree-sitter AST.
//!
//! Finds the bug *class* behind the JFC interrupt-stuck family: a value that a
//! downstream awaiter must receive is delivered **best-effort** (`try_send`)
//! and its failure silently dropped, so under channel backpressure the message
//! is lost while the operation still reports complete. Three shapes, in
//! increasing specificity:
//!
//! * [`ConcurrencyRule::LossySend`] — a `try_send` whose `Result` is discarded
//!   (`let _ = …`, a bare `…;` statement, or `… .ok();`). The base signal.
//! * [`ConcurrencyRule::SplitStateTransition`] — pending state is **removed**
//!   (`pop`/`remove`/`take`) and then re-delivered best-effort: if the send
//!   fails the state is gone and never re-enqueued (the MCP-elicitation
//!   pop-before-`try_send` bug).
//! * [`ConcurrencyRule::LossyThenCommitted`] — a discarded `try_send`
//!   co-occurs with a **guaranteed completion send** (`send_critical(AllComplete)`):
//!   the payload can drop while the turn is still committed as done (the
//!   `AskUserQuestion` answer-vs-`AllComplete` bug).
//!
//! Classification of which calls are lossy / guaranteed / removing lives in
//! [`crate::concurrency_rules`]. The walker is deliberately conservative —
//! anything that binds, matches, `?`-propagates, returns, or passes the result
//! as an argument is treated as handled — because a lint that fires on handled
//! code gets turned off.

use tree_sitter::{Node, Parser};

use crate::concurrency_rules::ConcurrencyRules;

/// Which concurrency hazard a finding represents. Ordered base → most specific;
/// a single lossy-send site is reported under exactly one rule (the most
/// specific that applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyRule {
    /// Best-effort send whose result is discarded.
    LossySend,
    /// State removed then re-sent best-effort (lost on send failure).
    SplitStateTransition,
    /// Lossy send co-occurring with a guaranteed completion send.
    LossyThenCommitted,
}

impl ConcurrencyRule {
    /// Stable machine identifier (for MCP output / DSL filters).
    pub fn id(self) -> &'static str {
        match self {
            ConcurrencyRule::LossySend => "lossy_send",
            ConcurrencyRule::SplitStateTransition => "split_state_transition",
            ConcurrencyRule::LossyThenCommitted => "lossy_then_committed",
        }
    }
}

/// One concurrency-lint hit. `line`/`col` are 1-indexed, matching editor and
/// `file:line` conventions used elsewhere in the crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrencyFinding {
    pub rule: ConcurrencyRule,
    pub line: usize,
    pub col: usize,
    /// Method name at the flagged call site (e.g. `try_send`).
    pub method: String,
    /// Enclosing function name, when the site is inside one.
    pub function: Option<String>,
    /// Human-readable explanation, ready to render in a report.
    pub message: String,
}

/// How a relevant call site is classified while scanning a function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallClass {
    LossyDiscarded,
    LossyHandled,
    Guaranteed,
    Removal,
}

/// A relevant call collected during the walk, scoped to its enclosing function.
struct CallEvent {
    line: usize,
    col: usize,
    method: String,
    class: CallClass,
    /// Lowercased verbatim call text — used to spot a completion marker in a
    /// guaranteed send's callee/arguments.
    text_lower: String,
}

/// All relevant calls within one function (or the module root).
struct FnScope {
    name: Option<String>,
    events: Vec<CallEvent>,
}

/// Analyze `source` of language `lang` and return concurrency-lint findings.
///
/// Returns empty for languages without a concurrency walker yet (only Rust is
/// wired in v1) — callers treat that as "no lint available", not an error.
pub fn analyze_source(lang: &str, source: &str) -> Vec<ConcurrencyFinding> {
    let Some(rules) = ConcurrencyRules::for_language(lang) else {
        return Vec::new();
    };
    // Only the Rust grammar + node-kind mapping is wired for the walker in v1.
    // The rules table already carries TS/Go classifications for when their
    // walkers land; until then, don't pretend to analyze them.
    if lang != "rust" {
        return Vec::new();
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let src = source.as_bytes();

    // Scope 0 is the module root; function_items push their own scopes.
    let mut scopes: Vec<FnScope> = vec![FnScope {
        name: None,
        events: Vec::new(),
    }];
    walk(tree.root_node(), src, rules, &mut scopes, 0);

    let mut findings = Vec::new();
    for scope in &scopes {
        // A guaranteed send whose text reads like a terminal completion turns
        // every discarded lossy send in the same function into the committed
        // variant (the AllComplete shape).
        let has_completion_commit = scope.events.iter().any(|e| {
            e.class == CallClass::Guaranteed && rules.looks_like_completion(&e.text_lower)
        });
        let removal_lines: Vec<usize> = scope
            .events
            .iter()
            .filter(|e| e.class == CallClass::Removal)
            .map(|e| e.line)
            .collect();

        for ev in scope
            .events
            .iter()
            .filter(|e| e.class == CallClass::LossyDiscarded)
        {
            let (rule, message) = if has_completion_commit {
                (
                    ConcurrencyRule::LossyThenCommitted,
                    format!(
                        "best-effort `{}` co-occurs with a guaranteed completion send; \
                         the payload can be dropped under backpressure while the operation \
                         is still committed as complete",
                        ev.method
                    ),
                )
            } else if let Some(&rl) = removal_lines.iter().find(|&&rl| rl <= ev.line) {
                (
                    ConcurrencyRule::SplitStateTransition,
                    format!(
                        "pending state removed at line {rl} then re-delivered best-effort via \
                         `{}`; the state is lost (never re-enqueued) if the send fails",
                        ev.method
                    ),
                )
            } else {
                (
                    ConcurrencyRule::LossySend,
                    format!(
                        "best-effort `{}` whose result is discarded; the message is silently \
                         dropped if the channel is full or closed",
                        ev.method
                    ),
                )
            };
            findings.push(ConcurrencyFinding {
                rule,
                line: ev.line,
                col: ev.col,
                method: ev.method.clone(),
                function: scope.name.clone(),
                message,
            });
        }
    }
    findings.sort_by(|a, b| a.line.cmp(&b.line).then(a.col.cmp(&b.col)));
    findings
}

/// Recursive descent, stack-guarded per the crate invariant (deep AST nesting
/// must not overflow a worker thread's fixed stack). Tracks the enclosing
/// function scope by index into `scopes`.
fn walk(node: Node, src: &[u8], rules: &ConcurrencyRules, scopes: &mut Vec<FnScope>, cur: usize) {
    crate::ensure_sufficient_stack(|| walk_inner(node, src, rules, scopes, cur))
}

fn walk_inner(
    node: Node,
    src: &[u8],
    rules: &ConcurrencyRules,
    scopes: &mut Vec<FnScope>,
    cur: usize,
) {
    let mut cur = cur;
    if node.kind() == "function_item" {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .map(str::to_owned);
        scopes.push(FnScope {
            name,
            events: Vec::new(),
        });
        cur = scopes.len() - 1;
    }

    if node.kind() == "call_expression"
        && let Some(event) = classify_call(node, src, rules)
    {
        scopes[cur].events.push(event);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, rules, scopes, cur);
    }
}

/// Classify a `call_expression` against the rules, or `None` if it's not a
/// send / removal we track.
fn classify_call(call: Node, src: &[u8], rules: &ConcurrencyRules) -> Option<CallEvent> {
    let method = called_name(call, src)?;
    let class = if rules.is_lossy_send(&method) {
        if result_is_discarded(call, src) {
            CallClass::LossyDiscarded
        } else {
            CallClass::LossyHandled
        }
    } else if rules.is_guaranteed_send(&method) {
        CallClass::Guaranteed
    } else if rules.is_state_removal(&method) {
        CallClass::Removal
    } else {
        return None;
    };
    let pos = call.start_position();
    Some(CallEvent {
        line: pos.row + 1,
        col: pos.column + 1,
        method,
        class,
        text_lower: call.utf8_text(src).unwrap_or("").to_ascii_lowercase(),
    })
}

/// The called method/function name for a `call_expression`: the field name of a
/// `recv.method(..)` method call, or the final segment of a path call.
fn called_name(call: Node, src: &[u8]) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "field_expression" => func
            .child_by_field_name("field")
            .and_then(|f| f.utf8_text(src).ok())
            .map(str::to_owned),
        "identifier" => func.utf8_text(src).ok().map(str::to_owned),
        "scoped_identifier" => func
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .or_else(|| func.utf8_text(src).ok().and_then(|t| t.rsplit("::").next()))
            .map(str::to_owned),
        _ => None,
    }
}

/// Whether the `Result` of `call` is discarded rather than handled. Conservative:
/// only a bare expression statement, a `let _ = …` wildcard binding, or a chain
/// that ends in one of those counts as discarded. Binding, `?`, match, return,
/// or passing as an argument all count as handled.
fn result_is_discarded(call: Node, src: &[u8]) -> bool {
    let mut node = call;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            // `let _ = …;` discards; `let x = …;` handles. tree-sitter-rust
            // names the `_` pattern `wildcard_pattern`; the text check is a
            // grammar-version-agnostic backstop.
            "let_declaration" => {
                let Some(pat) = parent.child_by_field_name("pattern") else {
                    return false;
                };
                return pat.kind() == "wildcard_pattern"
                    || pat.utf8_text(src).map(|t| t.trim() == "_").unwrap_or(false);
            }
            // bare `expr;` — the #[must_use] Result is dropped.
            "expression_statement" => return true,
            // `?` propagates the error to the caller — handled.
            "try_expression" => return false,
            // pass-through chaining (`.ok()`, `.await`, parens): keep climbing
            // to the statement that ultimately consumes or drops the value.
            "field_expression"
            | "call_expression"
            | "await_expression"
            | "parenthesized_expression" => {
                node = parent;
            }
            // anything else (arguments, assignment, return, match, if-let
            // condition, binary op, macro, tuple, …) consumes the value.
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_for_findings(src: &str) -> Vec<ConcurrencyFinding> {
        analyze_source("rust", src)
    }

    #[test]
    fn flags_discarded_try_send_keepalive() {
        // The stream-keepalive shape: best-effort send, result dropped.
        let src = r#"
            fn keepalive(tx: &Sender) {
                let _ = tx.try_send(Event::Keepalive);
            }
        "#;
        let f = rules_for_findings(src);
        assert_eq!(f.len(), 1, "expected one finding, got {f:#?}");
        assert_eq!(f[0].rule, ConcurrencyRule::LossySend);
        assert_eq!(f[0].method, "try_send");
        assert_eq!(f[0].function.as_deref(), Some("keepalive"));
    }

    #[test]
    fn flags_bare_statement_try_send() {
        let src = r#"
            fn ping(tx: &Sender) {
                tx.try_send(Event::Ping);
            }
        "#;
        let f = rules_for_findings(src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, ConcurrencyRule::LossySend);
    }

    #[test]
    fn flags_pop_before_send_as_split_transition() {
        // The MCP-elicitation bug: pop the pending request, then best-effort
        // re-deliver it. On failure the request is lost.
        let src = r#"
            fn deliver(pending: &mut Map, tx: &Sender) {
                let req = pending.remove(&id);
                let _ = tx.try_send(req);
            }
        "#;
        let f = rules_for_findings(src);
        assert_eq!(f.len(), 1, "got {f:#?}");
        assert_eq!(f[0].rule, ConcurrencyRule::SplitStateTransition);
        assert!(f[0].message.contains("removed at line"));
    }

    #[test]
    fn flags_lossy_with_completion_commit() {
        // The AskUserQuestion bug: deliver the answer best-effort, then commit
        // AllComplete with a guaranteed send.
        let src = r#"
            fn answer(tx: &Sender, ans: Answer) {
                let _ = tx.try_send(ToolEvent::Result(ans));
                tx.send_critical(EngineEvent::AllComplete);
            }
        "#;
        let f = rules_for_findings(src);
        assert_eq!(f.len(), 1, "got {f:#?}");
        assert_eq!(f[0].rule, ConcurrencyRule::LossyThenCommitted);
    }

    #[test]
    fn does_not_flag_propagated_result() {
        let src = r#"
            fn safe(tx: &Sender) -> Result<(), Error> {
                tx.try_send(x)?;
                Ok(())
            }
        "#;
        assert!(rules_for_findings(src).is_empty());
    }

    #[test]
    fn does_not_flag_handled_result() {
        let src = r#"
            fn handled(tx: &Sender) {
                if let Err(e) = tx.try_send(x) {
                    log(e);
                }
                let r = tx.try_send(y);
                drop(r);
            }
        "#;
        assert!(
            rules_for_findings(src).is_empty(),
            "if-let and bound results must not be flagged"
        );
    }

    #[test]
    fn ignores_non_concurrency_code() {
        let src = r#"
            fn add(a: i32, b: i32) -> i32 {
                let c = a + b;
                c
            }
        "#;
        assert!(rules_for_findings(src).is_empty());
    }

    #[test]
    fn unknown_language_returns_empty() {
        assert!(analyze_source("haskell", "anything").is_empty());
    }
}
