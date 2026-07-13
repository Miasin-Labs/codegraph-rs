//! Private regression tests for MCP tool support modules.

#[cfg(test)]
mod mcp_projection;

#[cfg(test)]
mod tests {
    use crate::mcp::tools::admin::glob_to_regex;
    use crate::mcp::tools::format::{
        extract_symbol_tokens,
        get_explore_budget,
        get_explore_output_budget,
        last_qualifier_part,
        number_source_lines,
        to_locale_string,
    };
    use crate::mcp::tools::registry::tools;
    use crate::mcp::tools::schema::{ToolContent, ToolResult};

    #[test]
    fn explore_budget_tiers_match_ts() {
        assert_eq!(get_explore_budget(0), 1);
        assert_eq!(get_explore_budget(499), 1);
        assert_eq!(get_explore_budget(500), 2);
        assert_eq!(get_explore_budget(4999), 2);
        assert_eq!(get_explore_budget(5000), 3);
        assert_eq!(get_explore_budget(14999), 3);
        assert_eq!(get_explore_budget(15000), 4);
        assert_eq!(get_explore_budget(24999), 4);
        assert_eq!(get_explore_budget(25000), 5);
        assert_eq!(get_explore_budget(u64::MAX), 5);
    }

    #[test]
    fn output_budget_max_chars_per_file_is_monotonic_across_tiers() {
        // The invariant that motivated the doc: a larger tier must never get a
        // smaller max_chars_per_file than a smaller tier.
        let tiers = [0u64, 149, 150, 499, 500, 4999, 5000, 14999, 15000, 30000];
        let mut prev = 0usize;
        for t in tiers {
            let b = get_explore_output_budget(t);
            assert!(
                b.max_chars_per_file >= prev,
                "max_chars_per_file regressed at tier {t}: {} < {prev}",
                b.max_chars_per_file
            );
            prev = b.max_chars_per_file;
        }
    }

    #[test]
    fn output_budget_tier_values_are_digit_for_digit() {
        let t0 = get_explore_output_budget(100);
        assert_eq!(
            (
                t0.max_output_chars,
                t0.default_max_files,
                t0.max_chars_per_file,
                t0.gap_threshold
            ),
            (13000, 4, 3800, 7)
        );
        assert!(t0.exclude_low_value_files);
        let t1 = get_explore_output_budget(300);
        assert_eq!(
            (
                t1.max_output_chars,
                t1.default_max_files,
                t1.max_chars_per_file,
                t1.gap_threshold
            ),
            (18000, 5, 3800, 8)
        );
        let t2 = get_explore_output_budget(1000);
        assert_eq!(
            (
                t2.max_output_chars,
                t2.default_max_files,
                t2.max_chars_per_file,
                t2.gap_threshold
            ),
            (24000, 8, 6500, 12)
        );
        let t3 = get_explore_output_budget(10000);
        assert_eq!(
            (
                t3.max_output_chars,
                t3.default_max_files,
                t3.max_chars_per_file,
                t3.gap_threshold
            ),
            (24000, 8, 7000, 15)
        );
        let t4 = get_explore_output_budget(30000);
        assert_eq!(t3.max_output_chars, t4.max_output_chars);
        assert_eq!(t4.max_chars_per_file, 7000);
        assert!(!t4.exclude_low_value_files);
    }

    #[test]
    fn glob_to_regex_matches_like_ts() {
        let re = glob_to_regex("*.tsx").unwrap();
        assert!(re.is_match("src/App.tsx"));
        assert!(!re.is_match("src/App.ts"));
        let re = glob_to_regex("**/*.test.ts").unwrap();
        assert!(re.is_match("src/deep/x.test.ts"));
        let re = glob_to_regex("src/*.ts").unwrap();
        assert!(re.is_match("src/a.ts"));
        assert!(!re.is_match("src/sub/a.ts"));
    }

    #[test]
    fn locale_string_groups_thousands() {
        assert_eq!(to_locale_string(0), "0");
        assert_eq!(to_locale_string(999), "999");
        assert_eq!(to_locale_string(1000), "1,000");
        assert_eq!(to_locale_string(1234567), "1,234,567");
    }

    #[test]
    fn last_qualifier_part_handles_separators() {
        assert_eq!(last_qualifier_part("a.b.c"), "c");
        assert_eq!(last_qualifier_part("a::b::c"), "c");
        assert_eq!(last_qualifier_part("a/b"), "b");
        assert_eq!(last_qualifier_part("plain"), "plain");
    }

    #[test]
    fn number_source_lines_is_cat_n_style() {
        assert_eq!(number_source_lines("a\nb", 5), "5\ta\n6\tb");
    }

    #[test]
    fn symbol_token_extraction_strips_extensions_and_dedupes() {
        let toks =
            extract_symbol_tokens("AuthService loginUser session-manager Create.cs loginUser");
        assert!(toks.contains(&"AuthService".to_string()));
        assert!(toks.contains(&"loginUser".to_string()));
        assert!(toks.contains(&"Create".to_string()));
        // hyphenated term fails the identifier regex
        assert!(!toks.iter().any(|t| t.contains('-')));
        // deduped
        assert_eq!(toks.iter().filter(|t| *t == "loginUser").count(), 1);
    }

    #[test]
    fn tool_definition_json_is_wire_compatible_with_ts() {
        // codegraph_search serialized: camelCase inputSchema, properties in TS
        // literal order, per-property keys in (type, description, enum?,
        // default?) order, required present.
        let defs = tools();
        assert_eq!(defs.len(), 13);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "codegraph_search",
                "codegraph_callers",
                "codegraph_callees",
                "codegraph_impact",
                "codegraph_node",
                "codegraph_explore",
                "codegraph_status",
                "codegraph_files",
                "codegraph_vuln",
                "codegraph_verify_roles",
                "codegraph_arch",
                "codegraph_xref",
                "codegraph_paths"
            ]
        );
        let json = serde_json::to_string(&defs[0]).unwrap();
        // Top-level key order + camelCase inputSchema.
        let name_i = json.find("\"name\"").unwrap();
        let desc_i = json.find("\"description\"").unwrap();
        let schema_i = json.find("\"inputSchema\"").unwrap();
        let output_schema_i = json.find("\"outputSchema\"").unwrap();
        assert!(name_i < desc_i && desc_i < schema_i && schema_i < output_schema_i);
        // Property order: query, kind, limit, projectPath.
        let q = json.find("\"query\"").unwrap();
        let k = json.find("\"kind\"").unwrap();
        let l = json.find("\"limit\"").unwrap();
        let p = json.find("\"projectPath\"").unwrap();
        assert!(q < k && k < l && l < p);
        // kind carries its enum; limit its default.
        assert!(json.contains("\"enum\":[\"function\",\"method\",\"class\",\"interface\",\"type\",\"variable\",\"route\",\"component\"]"));
        assert!(json.contains("\"default\":10"));
        assert!(json.contains("\"required\":[\"query\"]"));
        assert!(json.contains("\"kind\":{\"const\":\"search\"}"));
        // status has no `required` key at all (TS omits it).
        let status = serde_json::to_value(&defs[6]).unwrap();
        assert!(status["inputSchema"].get("required").is_none());
        assert_eq!(
            status["outputSchema"]["oneOf"][0]["properties"]["kind"]["const"],
            "status"
        );
        assert_eq!(
            status["outputSchema"]["oneOf"][1]["properties"]["kind"]["const"],
            "error"
        );
    }

    #[test]
    fn tool_result_json_omits_is_error_on_success() {
        let ok = ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: "hi".into(),
            }],
            structured_content: None,
            meta: None,
            is_error: None,
        };
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"content":[{"type":"text","text":"hi"}]}"#
        );
        let err = ToolResult {
            content: vec![ToolContent {
                content_type: "text".into(),
                text: "x".into(),
            }],
            structured_content: None,
            meta: None,
            is_error: Some(true),
        };
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"content":[{"type":"text","text":"x"}],"isError":true}"#
        );
    }
}
