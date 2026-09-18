use std::collections::{BTreeMap, HashMap, HashSet};

use super::cochange::{CoChange, parse_log};
use super::gather::{Direction, Neighbour, Relation};
use super::reason::describe;
use super::score::SeedSymbolSpread;
use super::terms::{overlap, query_terms, segments};
use super::{MAX_ROWS, MIN_ROWS, max_rows};
use crate::mcp::tools::get_explore_output_budget;

#[test]
fn segments_split_identifiers_and_paths_into_lowercase_words() {
    let parts = segments("src/mcp/parseHTTPRequest_handler.rs");
    for expected in [
        "src",
        "mcp",
        "parse",
        "http",
        "request",
        "handler",
        "parsehttprequest",
    ] {
        assert!(
            parts.contains(expected),
            "{expected} missing from {parts:?}"
        );
    }
    // Two-letter runs carry no signal.
    assert!(!parts.contains("rs"));
}

#[test]
fn query_terms_drop_stop_words_and_split_snake_case() {
    let terms = query_terms("how does codegraph_diagnostics place errors");
    assert!(terms.contains("diagnostics"));
    assert!(terms.contains("errors"));
    assert!(!terms.contains("how"));
    assert_eq!(overlap(&terms, &segments("src/diagnostics/mod.rs")), 1);
}

#[test]
fn git_log_records_split_into_file_lists() {
    let output = b"\x1e\n\nsrc/a.rs\nsrc/b.rs\n\x1e\n\nsrc/c.rs\n\x1e\n";
    assert_eq!(
        parse_log(output),
        vec![
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            vec!["src/c.rs".to_string()],
        ]
    );
}

#[test]
fn co_change_counts_shared_commits_and_skips_sweeping_ones() {
    let commit = |files: &[&str]| files.iter().map(|f| f.to_string()).collect::<Vec<_>>();
    let sweeping: Vec<String> = (0..60)
        .map(|i| format!("src/f{i}.rs"))
        .chain(["src/seed.rs".to_string()])
        .collect();
    let commits = vec![
        commit(&["src/seed.rs", "src/near.rs"]),
        commit(&["src/seed.rs", "src/near.rs", "src/other_seed.rs"]),
        commit(&["src/other_seed.rs", "src/far.rs"]),
        commit(&["src/unrelated.rs", "src/near.rs"]),
        commit(&["src/seed.rs", "src/ranked.rs"]),
        sweeping,
    ];
    let seeds = ["src/seed.rs", "src/other_seed.rs"];
    let excluded: HashSet<&str> = ["src/ranked.rs"].into_iter().collect();
    let tally = CoChange::from_commits(&commits, &seeds, &excluded);

    let near = &tally.by_file["src/near.rs"];
    assert_eq!(near.commits, 2, "the unrelated commit does not count");
    assert_eq!(near.seed_rank, 0);
    let far = &tally.by_file["src/far.rs"];
    assert_eq!(far.commits, 1);
    assert_eq!(far.seed_rank, 1);
    assert!(near.weight > far.weight);
    assert!(
        !tally.by_file.contains_key("src/seed.rs"),
        "seeds are never neighbours"
    );
    assert!(
        !tally.by_file.contains_key("src/ranked.rs"),
        "explore's own files are excluded"
    );
    assert!(
        !tally.by_file.contains_key("src/f0.rs"),
        "sweeping commits carry no coupling"
    );
}

fn neighbour(relations: &[((Direction, Relation), &str, usize)]) -> Neighbour {
    let mut by_relation: BTreeMap<(Direction, Relation), BTreeMap<String, usize>> = BTreeMap::new();
    let mut seed_symbols = HashSet::new();
    for (key, symbol, count) in relations {
        by_relation
            .entry(*key)
            .or_default()
            .insert(symbol.to_string(), *count);
        seed_symbols.insert(symbol.to_string());
    }
    Neighbour {
        by_relation,
        seed_symbols,
        ..Neighbour::default()
    }
}

#[test]
fn reasons_name_the_strongest_relation_and_its_seed_symbol() {
    let seeds = ["src/explore/handler.rs", "src/explore/literal.rs"];
    let spread = SeedSymbolSpread::of(&HashMap::new());
    let caller = neighbour(&[
        ((Direction::Inbound, Relation::Calls), "collect_literal", 3),
        ((Direction::Outbound, Relation::Uses), "Budget", 1),
    ]);
    assert_eq!(
        describe(Some(&caller), None, &seeds, false, false, &spread),
        "calls collect_literal +1"
    );
    assert_eq!(
        describe(Some(&caller), None, &seeds, true, false, &spread),
        "tests collect_literal +1"
    );

    let callee = neighbour(&[((Direction::Outbound, Relation::Calls), "handle_explore", 2)]);
    assert_eq!(
        describe(Some(&callee), None, &seeds, false, true, &spread),
        "called by handle_explore"
    );

    let tally = CoChange::from_commits(
        &[vec![
            "src/explore/literal.rs".into(),
            "docs/notes.md".into(),
        ]],
        &seeds,
        &HashSet::new(),
    );
    let entry = &tally.by_file["docs/notes.md"];
    assert_eq!(
        describe(None, Some(entry), &seeds, false, false, &spread),
        "co-changed with literal.rs in 1 commit"
    );
    assert_eq!(
        describe(None, None, &seeds, false, true, &spread),
        "same directory as a result"
    );
}

#[test]
fn reasons_prefer_a_specific_seed_symbol_over_one_every_neighbour_uses() {
    let seeds = ["src/error.rs"];
    let caller = neighbour(&[
        ((Direction::Inbound, Relation::Calls), "unwrap", 4),
        ((Direction::Inbound, Relation::Calls), "render_report", 2),
        ((Direction::Inbound, Relation::Calls), "get", 6),
    ]);
    let mut neighbourhood = HashMap::new();
    neighbourhood.insert("src/report.rs".to_string(), caller.clone());
    for i in 0..20 {
        neighbourhood.insert(
            format!("src/user{i}.rs"),
            neighbour(&[((Direction::Inbound, Relation::Calls), "unwrap", 1)]),
        );
    }
    let spread = SeedSymbolSpread::of(&neighbourhood);
    assert!(spread.specificity("render_report") > spread.specificity("unwrap"));
    assert!(spread.specificity("render_report") > spread.specificity("get"));
    assert_eq!(
        describe(Some(&caller), None, &seeds, false, false, &spread),
        "calls render_report +2"
    );
}

#[test]
fn row_budget_scales_with_the_output_budget() {
    let small = max_rows(get_explore_output_budget(10));
    let large = max_rows(get_explore_output_budget(10_000));
    assert!((MIN_ROWS..=MAX_ROWS).contains(&small));
    assert!((MIN_ROWS..=MAX_ROWS).contains(&large));
    assert!(large >= small);
}
