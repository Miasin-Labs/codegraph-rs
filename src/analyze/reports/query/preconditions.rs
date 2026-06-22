use super::*;

/// Does the parsed query contain the `preconditions` pipe operator
/// anywhere in its expression tree?
fn expr_contains_preconditions(expr: &Expr) -> bool {
    // Recursion guard — depth follows the nested query expression tree.
    crate::ensure_sufficient_stack(|| expr_contains_preconditions_inner(expr))
}

fn expr_contains_preconditions_inner(expr: &Expr) -> bool {
    let ops_have = |ops: &[DslOp]| ops.iter().any(|op| matches!(op, DslOp::Preconditions));
    match expr {
        Expr::Pipe(ops) => ops_have(ops),
        Expr::PipeFrom { base, ops } => expr_contains_preconditions(base) || ops_have(ops),
        Expr::SetOp { left, right, .. } => {
            expr_contains_preconditions(left) || expr_contains_preconditions(right)
        }
        Expr::PathQuery(pq) => {
            expr_contains_preconditions(&pq.from) || expr_contains_preconditions(&pq.to)
        }
        Expr::DominatorsOf(inner) | Expr::DominatesOf(inner) | Expr::TraitImplsOf(inner) => {
            expr_contains_preconditions(inner)
        }
        Expr::MultiPath { sources, to, .. } => {
            sources.iter().any(expr_contains_preconditions) || expr_contains_preconditions(to)
        }
        Expr::Entrypoints(_) => false,
    }
}

/// Mirror `run_query_expr`'s parse dispatch (the aggregation grammar first,
/// then the extended expression grammar) to decide whether `query` asks for
/// `preconditions`. Aggregations return scalars — there are no node rows to
/// enrich, so they report `false`.
pub(super) fn query_requests_preconditions(query: &str) -> bool {
    match parse_aggregate(query) {
        Ok(AggExpr::Plain(expr)) => expr_contains_preconditions(&expr),
        Ok(_) => false,
        Err(_) => parse_expr(query)
            .map(|expr| expr_contains_preconditions(&expr))
            .unwrap_or(false),
    }
}

/// Byte offset of (1-based `line`, 0-based `col`) within `source`, clamped
/// into the line (and the file) so a stale column can never index past the
/// parsed text. `None` when the line is 0 (the bridge's "unknown" value) or
/// beyond the file.
fn byte_pos_of(source: &str, line: u32, col: u32) -> Option<usize> {
    let line_idx = line.checked_sub(1)? as usize;
    let mut offset = 0usize;
    for (i, l) in source.split_inclusive('\n').enumerate() {
        if i == line_idx {
            let within = (col as usize).min(l.len().saturating_sub(1));
            return Some((offset + within).min(source.len().saturating_sub(1)));
        }
        offset += l.len();
    }
    None
}

/// Render one engine predicate for display: bare `if` conditions get their
/// keyword back; `match`/`while`/`for`/`loop` texts already carry theirs.
fn predicate_text(p: &Predicate) -> String {
    if p.kind == "if_expression" {
        format!("if {}", p.text)
    } else {
        p.text.clone()
    }
}

/// Build the source-level preconditions section for a `… | preconditions`
/// query result: for every Calls/UnresolvedCall edge between two result
/// nodes, re-read the call site's source file under `workspace_root` and
/// extract the enclosing branch conditions (engine entry point:
/// `predicates::extract_predicates`, which needs source + a byte position —
/// real on v5 indexes). Extraction gaps are counted and surfaced in the
/// section note instead of being silently dropped.
pub(super) fn build_preconditions_section(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    result_nodes: &[ANodeId],
) -> PreconditionsSection {
    let in_set: HashSet<&ANodeId> = result_nodes.iter().collect();
    let mut guards: Vec<PreconditionGuard> = Vec::new();
    let mut missing_byte_anchor = 0usize;
    let mut non_rust = 0usize;
    let mut unreadable = 0usize;
    let mut stale_position = 0usize;
    let mut unconditional = 0usize;
    let mut sources: HashMap<PathBuf, Option<String>> = HashMap::new();

    for id in result_nodes {
        let Some(caller) = graph.get_node(id) else {
            continue;
        };
        if is_placeholder(caller) {
            continue;
        }
        for (target, edge) in graph.get_edges_from(id) {
            if !matches!(edge.kind, AEdgeKind::Calls | AEdgeKind::UnresolvedCall(_)) {
                continue;
            }
            if !in_set.contains(target) {
                continue;
            }
            let callee = graph
                .get_node(target)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "?".to_string());
            // v5 honesty gate: source-level anchoring rides the index's byte
            // offsets. The bridge degrades pre-v5 rows to `0..0` — extracting
            // at a guessed position would be a fabricated answer.
            let caller_range = &caller.span.byte_range;
            if caller_range.start == 0 && caller_range.end == 0 {
                missing_byte_anchor += 1;
                continue;
            }
            let file = &edge.source_span.file;
            if file.extension().and_then(|e| e.to_str()) != Some("rs") {
                non_rust += 1;
                continue;
            }
            let cached = sources
                .entry(file.clone())
                .or_insert_with(|| std::fs::read_to_string(workspace_root.join(file)).ok());
            let Some(source) = cached.as_ref() else {
                unreadable += 1;
                continue;
            };
            let Some(byte_pos) = byte_pos_of(
                source,
                edge.source_span.start_line,
                edge.source_span.start_col,
            ) else {
                stale_position += 1;
                continue;
            };
            let preds = extract_predicates(source, byte_pos);
            if preds.is_empty() {
                unconditional += 1;
                continue;
            }
            // The engine returns innermost-first; report evaluation order.
            let conditions: Vec<String> = preds.iter().rev().map(predicate_text).collect();
            guards.push(PreconditionGuard {
                caller: symbol_ref(caller),
                callee,
                file: file.display().to_string(),
                line: edge.source_span.start_line,
                conditions,
            });
        }
    }
    guards.sort_by(|a, b| {
        (&a.file, a.line, &a.caller.name, &a.callee).cmp(&(
            &b.file,
            b.line,
            &b.caller.name,
            &b.callee,
        ))
    });

    let mut note = if guards.is_empty() {
        "No source-level guarding conditions were found on the call sites between the result \
         nodes."
            .to_string()
    } else {
        "Conditions are listed outermost first (evaluation order), extracted from the on-disk \
         sources at each call site — they reflect the working tree as of this run."
            .to_string()
    };
    if unconditional > 0 {
        note.push_str(&format!(
            " {unconditional} call site(s) have no enclosing branch — those calls are \
             unconditional."
        ));
    }
    if missing_byte_anchor > 0 {
        note.push_str(&format!(
            " {missing_byte_anchor} call site(s) could not be anchored: the index carries no \
             byte offsets there (indexed before schema v5) — re-index (\"codegraph index\") to \
             enable source-level precondition extraction."
        ));
    }
    if non_rust > 0 {
        note.push_str(&format!(
            " Source-level predicate extraction currently covers Rust; {non_rust} call site(s) \
             in other languages were skipped."
        ));
    }
    if unreadable > 0 {
        note.push_str(&format!(
            " {unreadable} call-site file(s) could not be read under the project root — the \
             index may be stale (re-run \"codegraph sync\")."
        ));
    }
    if stale_position > 0 {
        note.push_str(&format!(
            " {stale_position} call-site position(s) lie outside the current file contents — \
             the file changed since indexing (re-run \"codegraph sync\")."
        ));
    }

    PreconditionsSection {
        guarded_call_count: guards.len(),
        guards,
        note,
    }
}
