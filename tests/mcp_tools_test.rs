//! MCP ToolHandler integration tests.
//!
//! Ports (real files, real SQLite, no mocks — TS suite parity):
//! - `__tests__/explore-output-budget.test.ts` (#185 budget shape + e2e)
//! - `__tests__/adaptive-explore-sizing.test.ts` (sibling skeletonization)
//! - `__tests__/explore-blast-radius.test.ts`
//! - `__tests__/mcp-tool-allowlist.test.ts` (CODEGRAPH_MCP_TOOLS)
//! - `__tests__/mcp-files-path-normalization.test.ts` (#426)
//!
//! Env-var discipline: tests that MUTATE process env (CODEGRAPH_MCP_TOOLS,
//! CODEGRAPH_ADAPTIVE_EXPLORE, CODEGRAPH_EXPLORE_LINENUMS) take the ENV_LOCK
//! write lock; tests that depend on the DEFAULT env take the read lock — same
//! pattern as tests/sync_test.rs.

use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use codegraph::mcp::tools::{
    ToolHandler,
    get_explore_budget,
    get_explore_output_budget,
    get_static_tools,
    tools,
};
use codegraph::{CodeGraph, EdgeKind, IndexOptions, NodeKind};
use serde_json::json;
use tempfile::TempDir;

static ENV_LOCK: RwLock<()> = RwLock::new(());

fn env_read() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().unwrap_or_else(|e| e.into_inner())
}

fn env_write() -> RwLockWriteGuard<'static, ()> {
    ENV_LOCK.write().unwrap_or_else(|e| e.into_inner())
}

/// Sets an env var for the test's duration, restoring the prior value on drop
/// (vitest afterEach parity).
struct EnvVarGuard {
    key: String,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }

    fn unset(key: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::remove_var(key);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Return the `#### <path> ...` section for a file basename, header through
/// the line before the next `###`/`####` header (or end of output).
fn section_for(text: &str, basename: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines
        .iter()
        .position(|l| l.starts_with("#### ") && l.contains(basename))
    else {
        return String::new();
    };
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        if l.starts_with("### ") || l.starts_with("#### ") {
            end = i;
            break;
        }
    }
    lines[start..end].join("\n")
}

// =============================================================================
// getExploreOutputBudget / getExploreBudget — __tests__/explore-output-budget.test.ts
// =============================================================================

#[test]
fn returns_a_strictly_smaller_total_cap_for_small_projects_than_for_huge_ones() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.max_output_chars < huge.max_output_chars);
    assert!(small.default_max_files < huge.default_max_files);
    assert!(small.max_chars_per_file < huge.max_chars_per_file);
}

#[test]
fn caps_total_output_well_under_8000_tokens_on_small_projects() {
    let small = get_explore_output_budget(100);
    assert!(small.max_output_chars <= 20000);
}

#[test]
fn caps_medium_large_projects_at_the_inline_tool_result_ceiling() {
    // A bigger single response gets externalized by the host to a file the
    // agent Reads back — so large repos get MORE CALLS, not a fatter response.
    let large = get_explore_output_budget(10000);
    assert!(large.max_output_chars <= 25000);
    assert!(large.max_output_chars >= 20000);
}

#[test]
fn uses_tier_breakpoints_matching_get_explore_budget() {
    // Very-tiny tier (<150 files) gets a tighter cap than small (150-499).
    let tier0a = get_explore_output_budget(50);
    let tier0b = get_explore_output_budget(149);
    assert_eq!(tier0a.max_output_chars, tier0b.max_output_chars);

    let tier1a = get_explore_output_budget(150);
    let tier1b = get_explore_output_budget(499);
    assert_eq!(tier1a.max_output_chars, tier1b.max_output_chars);
    // The <500 explore-call budget covers both very-tiny and small.
    assert_eq!(get_explore_budget(50), get_explore_budget(499));

    let tier2a = get_explore_output_budget(500);
    let tier2b = get_explore_output_budget(4999);
    assert_eq!(tier2a.max_output_chars, tier2b.max_output_chars);
    assert_eq!(get_explore_budget(500), get_explore_budget(4999));

    let tier3a = get_explore_output_budget(5000);
    let tier3b = get_explore_output_budget(14999);
    assert_eq!(tier3a.max_output_chars, tier3b.max_output_chars);

    // Small tiers step up (13k → 18k → 24k); medium and large SHARE the ~24k
    // inline ceiling — scaling now lives in the CALL budget.
    assert_ne!(tier0a.max_output_chars, tier1a.max_output_chars); // <150 vs <500
    assert_ne!(tier1a.max_output_chars, tier2a.max_output_chars); // <500 vs <5000
    assert_eq!(tier2a.max_output_chars, tier3a.max_output_chars); // <5000 == <15000
    assert!(get_explore_budget(5000) > get_explore_budget(4999)); // calls scale instead
}

#[test]
fn gates_off_meta_text_on_small_projects() {
    let small = get_explore_output_budget(100);
    assert!(!small.include_additional_files);
    assert!(!small.include_completeness_signal);
    assert!(!small.include_budget_note);
}

#[test]
fn keeps_all_meta_text_on_for_projects_that_earn_the_breadth_signal() {
    let medium = get_explore_output_budget(1000);
    assert!(medium.include_additional_files);
    assert!(medium.include_completeness_signal);
    assert!(medium.include_budget_note);
}

#[test]
fn keeps_the_relationships_section_on_for_medium_plus_tiers() {
    // Relationships dropped on <500 tiers; re-enabled at >=500.
    assert!(!get_explore_output_budget(50).include_relationships);
    assert!(get_explore_output_budget(1000).include_relationships);
    assert!(get_explore_output_budget(10000).include_relationships);
    assert!(get_explore_output_budget(30000).include_relationships);
}

#[test]
fn caps_the_per_file_header_symbol_list_more_tightly_on_small_projects() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.max_symbols_in_file_header < huge.max_symbols_in_file_header);
    assert!(small.max_symbols_in_file_header > 0);
}

#[test]
fn uses_a_tighter_clustering_gap_threshold_on_small_projects() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.gap_threshold <= huge.gap_threshold);
}

#[test]
fn handles_the_boundary_file_counts_exactly() {
    // 149 -> very-tiny, 150 -> small
    assert_eq!(
        get_explore_output_budget(149).max_output_chars,
        get_explore_output_budget(50).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(150).max_output_chars,
        get_explore_output_budget(200).max_output_chars
    );
    // 499 -> small, 500 -> medium
    assert_eq!(
        get_explore_output_budget(499).max_output_chars,
        get_explore_output_budget(200).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(500).max_output_chars,
        get_explore_output_budget(1000).max_output_chars
    );
    // 4999 -> medium, 5000 -> large
    assert_eq!(
        get_explore_output_budget(4999).max_output_chars,
        get_explore_output_budget(1000).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(5000).max_output_chars,
        get_explore_output_budget(10000).max_output_chars
    );
    // 14999 -> large, 15000 -> xlarge
    assert_eq!(
        get_explore_output_budget(14999).max_output_chars,
        get_explore_output_budget(10000).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(15000).max_output_chars,
        get_explore_output_budget(30000).max_output_chars
    );
}

// =============================================================================
// codegraph_explore output respects the adaptive budget — e2e (#185)
// =============================================================================

/// `beforeAll` of the e2e budget describe: one fat target file (many stacked
/// methods, the Alamofire Session.swift shape) plus a few small supporting
/// files.
fn budget_fixture(root: &Path) -> CodeGraph {
    let src_dir = root.join("src");
    let mut fat_lines: Vec<String> = vec!["export class Session {".to_string()];
    for i in 0..30 {
        fat_lines.push(format!("  method{i}(arg: string): string {{"));
        fat_lines.push(format!("    return this.helper{i}(arg) + \"{i}\";"));
        fat_lines.push("  }".to_string());
        fat_lines.push(format!("  private helper{i}(arg: string): string {{"));
        fat_lines.push(format!("    return arg.repeat({});", i + 1));
        fat_lines.push("  }".to_string());
    }
    fat_lines.push("}".to_string());
    write(&src_dir.join("session.ts"), &fat_lines.join("\n"));

    for i in 0..5 {
        write(
            &src_dir.join(format!("support{i}.ts")),
            &format!(
                "import {{ Session }} from './session';\nexport function callSession{i}(s: Session) {{ return s.method{i}('hi'); }}\n"
            ),
        );
    }

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn explore(handler: &ToolHandler, query: &str) -> String {
    let res = handler.execute("codegraph_explore", &json!({ "query": query }));
    assert_ne!(res.is_error, Some(true), "explore errored: {}", res.text());
    res.text().to_string()
}

#[test]
fn keeps_total_output_under_the_small_project_cap() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    let small_budget = get_explore_output_budget(100);
    // Allow a small overshoot for the trailing markers — the cap is enforced
    // per-file rather than as an absolute output ceiling.
    assert!(
        text.len() < small_budget.max_output_chars + 500,
        "explore output too large: {} chars",
        text.len()
    );
}

#[test]
fn final_output_never_exceeds_the_absolute_inline_ceiling() {
    // Regression for the >25K leak: flow.text was prepended after budget
    // accounting and the truncation suffix was appended after the ceiling
    // cut, so real sessions saw outputs up to 27,064 chars against the
    // 25,000 inline cap.
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    for f in 0..12 {
        let mut lines: Vec<String> = vec![format!("export class Service{f} {{")];
        for i in 0..40 {
            lines.push(format!("  process{f}x{i}(arg: string): string {{"));
            lines.push(format!(
                "    return this.transform{f}x{i}(arg) + \"suffix-{f}-{i}\";"
            ));
            lines.push("  }".to_string());
            lines.push(format!("  transform{f}x{i}(arg: string): string {{"));
            lines.push(format!("    return arg.repeat({});", i + 1));
            lines.push("  }".to_string());
        }
        lines.push("}".to_string());
        write(&src_dir.join(format!("service{f}.ts")), &lines.join("\n"));
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(
        &handler,
        "Service0 Service1 Service2 Service3 Service4 Service5 process transform",
    );
    assert!(
        text.len() <= 25_000,
        "explore output exceeds absolute inline ceiling: {} chars",
        text.len()
    );
}

#[test]
fn omits_the_meta_text_gated_off_for_small_projects() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("### Additional relevant files"));
    assert!(!text.contains("Complete source code is included above"));
    assert!(!text.contains("Explore budget:"));
}

#[test]
fn still_includes_the_relationships_section_or_source() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    // Either there are relationships, or no edges were significant — both fine.
    let has_relationships = text.contains("### Relationships");
    let source_follows_header = text.find("### Source Code").map(|i| i > 0).unwrap_or(false);
    assert!(has_relationships || source_follows_header);
}

#[test]
fn prefixes_source_lines_with_line_numbers_by_default() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_EXPLORE_LINENUMS");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    // At least one fenced source line should look like `<digits>\t<code>`.
    let re = regex::Regex::new(r"\n\d+\t").unwrap();
    assert!(re.is_match(&text));
}

#[test]
fn omits_line_numbers_when_linenums_env_is_zero() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_EXPLORE_LINENUMS", "0");
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    // The synthetic source has no tab-prefixed numeric lines of its own.
    let re = regex::Regex::new(r"\n\d+\t(?:export|  )").unwrap();
    assert!(!re.is_match(&text));
}

#[test]
fn uses_language_neutral_omission_markers() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    assert!(!text.contains("// ... (gap)"));
    assert!(!text.contains("// ... trimmed"));
}

#[test]
fn does_not_collapse_a_whole_file_class_into_just_its_header() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = budget_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "Session method helper");
    // A method body line (`methodN(arg: string)`) should appear, not just
    // the `export class Session {` opener (envelope filter, #185 follow-up).
    let re = regex::Regex::new(r"method\d+\(arg: string\)").unwrap();
    assert!(re.is_match(&text));
}

// =============================================================================
// Adaptive codegraph_explore sizing — sibling skeletonization
// (__tests__/adaptive-explore-sizing.test.ts)
// =============================================================================

// Stable marker — assert the `· skeleton` tag, not its exact trailing wording.
const SKELETON_MARK: &str = "· skeleton (signatures only";

// Names the spine (dispatch/proceed/handleLogging), the on-spine exemplar,
// the three off-spine siblings, and the distinct step.
const QUERY: &str = "dispatch proceed handleLogging LoggingInterceptor BridgeInterceptor CacheInterceptor RetryInterceptor ResponseFormatter";

// Names AuthInterceptor's `authenticate` and Codec's `encode` (both methods),
// plus the spine tokens so a spine still forms.
fn spare_query() -> String {
    format!("{QUERY} authenticate encode AuthInterceptor Codec JsonCodec")
}

/// The OkHttp-interceptor-chain-in-miniature fixture (see the TS test header
/// for the full design rationale).
fn adaptive_fixture(root: &Path) -> CodeGraph {
    let src = root.join("src");

    // The interchangeable contract — 4+ implementers below => sibling family.
    write(
        &src.join("interceptor.ts"),
        "export interface Interceptor {\n  intercept(request: string): string;\n}\n",
    );

    // The mechanism + the spine: dispatch -> proceed -> handleLogging.
    write(
        &src.join("dispatcher.ts"),
        "import { LoggingInterceptor } from './logging-interceptor';\n\nexport class RequestDispatcher {\n  dispatch(): string {\n    const chain = new InterceptorChain();\n    return chain.proceed();\n  }\n}\n\nexport class InterceptorChain {\n  proceed(): string {\n    const exemplar = new LoggingInterceptor();\n    return exemplar.handleLogging();\n  }\n}\n",
    );

    // On-spine exemplar: must stay FULL even though it is a sibling.
    write(
        &src.join("logging-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class LoggingInterceptor implements Interceptor {\n  handleLogging(): string {\n    const tag = 'LOGGING_BODY_MARKER';\n    return this.intercept(tag);\n  }\n  intercept(request: string): string {\n    return 'logged:' + request;\n  }\n}\n",
    );

    // Off-spine siblings — interchangeable impls of Interceptor => SKELETONIZE.
    write(
        &src.join("bridge-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class BridgeInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'BRIDGE_BODY_MARKER';\n    return 'bridged:' + request + detail;\n  }\n}\n",
    );
    write(
        &src.join("cache-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class CacheInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'CACHE_BODY_MARKER';\n    return 'cached:' + request + detail;\n  }\n}\n",
    );
    write(
        &src.join("retry-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class RetryInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'RETRY_BODY_MARKER';\n    return 'retried:' + request + detail;\n  }\n}\n",
    );

    // A 1:1 interface->impl pair: a DISTINCT step => FULL.
    write(
        &src.join("formatter.ts"),
        "export interface Formatter {\n  format(input: string): string;\n}\n",
    );
    write(
        &src.join("response-formatter.ts"),
        "import { Formatter } from './formatter';\nimport { JsonCodec } from './codec';\n\nexport class ResponseFormatter implements Formatter {\n  format(input: string): string {\n    const detail = 'FORMATTER_BODY_MARKER';\n    // Calls into the Codec family from OFF the dispatch spine.\n    return new JsonCodec().encode(input) + detail;\n  }\n}\n",
    );

    // An off-spine sibling that owns a uniquely-named method `authenticate`
    // (the RealCall fix): a named callable means "show me this" => stays full.
    write(
        &src.join("auth-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class AuthInterceptor implements Interceptor {\n  authenticate(token: string): string {\n    const detail = 'AUTH_BODY_MARKER';\n    return 'auth:' + token + detail;\n  }\n  intercept(request: string): string {\n    return this.authenticate(request);\n  }\n}\n",
    );

    // A base class that DEFINES a >=3-impl supertype AND co-locates its
    // subclasses (Django compiler.py shape).
    write(
        &src.join("codec.ts"),
        "export class Codec {\n  encode(input: string): string {\n    const detail = 'CODEC_BASE_MARKER';\n    return input + detail;\n  }\n}\nexport class JsonCodec extends Codec {\n  encode(input: string): string { return '{' + input + '}'; }\n}\nexport class XmlCodec extends Codec {\n  encode(input: string): string {\n    const detail = 'XML_BODY_MARKER';\n    return '<' + input + detail + '>';\n  }\n}\nexport class YamlCodec extends Codec {\n  encode(input: string): string { return '- ' + input; }\n}\n",
    );

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn explore_with_max_files(handler: &ToolHandler, query: &str, max_files: u32) -> String {
    let res = handler.execute(
        "codegraph_explore",
        &json!({ "query": query, "maxFiles": max_files }),
    );
    assert_ne!(res.is_error, Some(true), "explore errored: {}", res.text());
    res.text().to_string()
}

#[test]
fn fixture_sanity_interceptor_has_3_plus_implementers_formatter_has_fewer() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());

    let find = |name: &str, kind: NodeKind| {
        cg.search_nodes(name, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.name == name && n.kind == kind)
    };

    let interceptor = find("Interceptor", NodeKind::Interface).expect("Interceptor interface");
    let formatter = find("Formatter", NodeKind::Interface).expect("Formatter interface");

    let implementers = |id: &str| {
        cg.get_incoming_edges(id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Implements || e.kind == EdgeKind::Extends)
            .count()
    };

    // The whole gate hinges on this signal — assert the fixture actually
    // produces the >=3 / <3 split.
    assert!(implementers(&interceptor.id) >= 3);
    assert!(implementers(&formatter.id) < 3);
}

#[test]
fn skeletonizes_off_spine_polymorphic_siblings() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    // Precondition: the spine must have formed, or nothing skeletonizes.
    assert!(
        text.contains("## Flow (call path among the symbols you queried)"),
        "no flow spine formed:\n{text}"
    );

    for (file, marker) in [
        ("bridge-interceptor.ts", "BRIDGE_BODY_MARKER"),
        ("cache-interceptor.ts", "CACHE_BODY_MARKER"),
        ("retry-interceptor.ts", "RETRY_BODY_MARKER"),
    ] {
        let section = section_for(&text, file);
        assert!(
            !section.is_empty(),
            "{file} should be present in the explore output"
        );
        assert!(
            section.contains(SKELETON_MARK),
            "{file} should be skeletonized:\n{section}"
        );
        // The signature line survives; the body (with its marker) is elided.
        assert!(section.contains("intercept(request"));
        assert!(
            !section.contains(marker),
            "{file} body marker must NOT survive skeletonization"
        );
    }
}

#[test]
fn keeps_the_on_spine_exemplar_full_even_though_it_is_a_sibling() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    let section = section_for(&text, "logging-interceptor.ts");
    assert!(
        !section.is_empty(),
        "logging-interceptor.ts should be present"
    );
    assert!(
        !section.contains(SKELETON_MARK),
        "on-spine exemplar must NOT be skeletonized:\n{section}"
    );
    // Full source => the body marker is present.
    assert!(section.contains("LOGGING_BODY_MARKER"));
}

#[test]
fn keeps_a_distinct_step_full() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    let section = section_for(&text, "response-formatter.ts");
    assert!(
        !section.is_empty(),
        "response-formatter.ts should be present"
    );
    assert!(
        !section.contains(SKELETON_MARK),
        "a 1:1 interface impl is not a sibling and must stay full"
    );
    assert!(section.contains("FORMATTER_BODY_MARKER"));
}

#[test]
fn adaptive_explore_env_zero_disables_skeletonization() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_ADAPTIVE_EXPLORE", "0");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    assert!(
        !text.contains(SKELETON_MARK),
        "no file should be skeletonized with the flag off"
    );
    // The previously-skeletonized siblings now render their full bodies.
    let section = section_for(&text, "bridge-interceptor.ts");
    assert!(!section.is_empty());
    assert!(section.contains("BRIDGE_BODY_MARKER"));
}

#[test]
fn spares_an_off_spine_sibling_when_the_agent_named_a_callable_in_it() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, &spare_query(), 15);
    assert!(text.contains("## Flow (call path among the symbols you queried)"));

    // auth-interceptor.ts is an off-spine Interceptor sibling — would
    // skeletonize — but the agent named its method `authenticate`, so it
    // stays FULL.
    let auth = section_for(&text, "auth-interceptor.ts");
    assert!(!auth.is_empty(), "auth-interceptor.ts should be present");
    assert!(
        !auth.contains(SKELETON_MARK),
        "a file holding an agent-named callable must NOT be skeletonized"
    );
    assert!(auth.contains("AUTH_BODY_MARKER"));

    // Contrast: bridge-interceptor.ts — same family, named only by TYPE —
    // still skeletonizes.
    let bridge = section_for(&text, "bridge-interceptor.ts");
    assert!(
        bridge.contains(SKELETON_MARK),
        "a sibling named only by type still skeletonizes:\n{bridge}"
    );
    assert!(!bridge.contains("BRIDGE_BODY_MARKER"));
}

#[test]
fn collapses_a_base_plus_subclasses_family_file_to_a_focused_view() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, &spare_query(), 15);

    // codec.ts defines the base Codec (>=3 subclasses extend it) and
    // co-locates the subclasses — a "family" file. It COLLAPSES, but
    // per-symbol: the named base method `Codec.encode` keeps its body while a
    // non-named subclass (XmlCodec) collapses to a signature.
    let codec = section_for(&text, "codec.ts");
    assert!(!codec.is_empty(), "codec.ts should be present");
    assert!(
        codec.contains("· focused"),
        "a named family file collapses to a focused (not full) view:\n{codec}"
    );
    assert!(
        codec.contains("CODEC_BASE_MARKER"),
        "the named base method body is kept (no Read-back)"
    );
    assert!(
        !codec.contains("XML_BODY_MARKER"),
        "a non-named subclass body is elided to a signature"
    );
}

#[test]
fn naming_a_shared_polymorphic_method_does_not_spare_the_siblings() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    // `intercept` is implemented by every interceptor — a polymorphic name,
    // not a unique one. Naming it must NOT keep all five full.
    let text = explore_with_max_files(&handler, &format!("{QUERY} intercept"), 12);

    let bridge = section_for(&text, "bridge-interceptor.ts");
    assert!(
        bridge.contains(SKELETON_MARK),
        "a sibling named only via a shared method is not spared:\n{bridge}"
    );
    assert!(
        !bridge.contains("BRIDGE_BODY_MARKER"),
        "a shared method does not earn a body in a non-supertype leaf"
    );
}

// =============================================================================
// codegraph_explore — blast radius (__tests__/explore-blast-radius.test.ts)
// =============================================================================

fn blast_fixture(root: &Path) -> CodeGraph {
    let src = root.join("src");
    // `target` is depended on by a sibling (caller) and a test file.
    write(
        &src.join("feature.ts"),
        "export function target() { return 1; }\nexport function caller() { return target(); }\n",
    );
    write(
        &src.join("feature.test.ts"),
        "import { target } from './feature';\nexport function checkTarget() { return target(); }\n",
    );
    // A leaf with no dependents — must NOT show up in the blast radius.
    write(
        &src.join("leaf.ts"),
        "export function lonelyLeaf() { return 42; }\n",
    );

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

#[test]
fn lists_dependents_and_covering_tests_for_an_entry_symbol() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = blast_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "target");

    assert!(
        text.contains("### Blast radius"),
        "missing blast radius:\n{text}"
    );
    assert!(text.contains("`target`"));
    assert!(text.contains("caller")); // a caller count is reported
    // It names WHERE (the caller file) — not the caller's source body.
    assert!(text.contains("feature.ts"));
    // Test coverage is surfaced (covering test file, or the warning).
    let tests_re = regex::Regex::new(r"tests:.*feature\.test\.ts").unwrap();
    assert!(
        tests_re.is_match(&text) || text.contains("no covering tests"),
        "test coverage not surfaced:\n{text}"
    );
}

#[test]
fn omits_symbols_that_have_no_dependents_from_the_blast_radius() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = blast_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore(&handler, "lonelyLeaf");
    // lonelyLeaf has zero callers — it must never appear under a blast-radius
    // bullet. (TS: /Blast radius[\s\S]*`lonelyLeaf`/ must not match.)
    if let Some(pos) = text.find("Blast radius") {
        assert!(
            !text[pos..].contains("`lonelyLeaf`"),
            "lonelyLeaf appeared in the blast radius:\n{text}"
        );
    }
}

#[test]
fn callees_do_not_fall_back_to_a_wrong_symbol_for_qualified_misses() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src").join("lib.ts"),
        "export function helper() { return 1; }\nexport function run_index_all() { return helper(); }\n",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    let res = handler.execute(
        "codegraph_callees",
        &json!({ "symbol": "nope.run_index_all", "limit": 10 }),
    );
    assert_ne!(res.is_error, Some(true), "callees errored: {}", res.text());
    let text = res.text();
    assert!(
        text.contains("Symbol \"nope.run_index_all\" not found in the codebase"),
        "unexpected qualified-miss response:\n{text}"
    );
    assert!(
        !text.contains("helper"),
        "qualified miss must not fall back to run_index_all:\n{text}"
    );
}

// =============================================================================
// CODEGRAPH_MCP_TOOLS allowlist (__tests__/mcp-tool-allowlist.test.ts)
// =============================================================================

fn listed_names() -> Vec<String> {
    let mut names: Vec<String> = ToolHandler::new(None)
        .get_tools()
        .into_iter()
        .map(|t| t.name)
        .collect();
    names.sort();
    names
}

#[test]
fn exposes_the_full_tool_surface_when_unset() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
    let all = listed_names();
    assert!(all.contains(&"codegraph_explore".to_string()));
    assert!(!all.contains(&"codegraph_context".to_string()));
    assert!(!all.contains(&"codegraph_trace".to_string()));
    assert!(all.len() >= 8);
}

#[test]
fn filters_list_tools_to_the_allowlisted_short_names() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "explore,search,node");
    assert_eq!(
        listed_names(),
        vec!["codegraph_explore", "codegraph_node", "codegraph_search"]
    );
}

#[test]
fn accepts_fully_qualified_names_and_ignores_whitespace() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", " codegraph_explore , search ");
    assert_eq!(
        listed_names(),
        vec!["codegraph_explore", "codegraph_search"]
    );
}

#[test]
fn treats_an_empty_whitespace_value_as_unset() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "   ");
    assert!(listed_names().len() >= 8);
}

#[test]
fn rejects_a_disabled_tool_on_execute() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "node");
    let res = ToolHandler::new(None).execute("codegraph_explore", &json!({}));
    assert_eq!(res.is_error, Some(true));
    assert!(res.text().contains("disabled via CODEGRAPH_MCP_TOOLS"));
}

#[test]
fn lets_an_allowlisted_tool_past_the_guard() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "search");
    // No CodeGraph attached, so it fails *after* the allowlist guard — the
    // "disabled" message must NOT appear, proving the guard passed it through.
    let res = ToolHandler::new(None).execute("codegraph_search", &json!({ "query": "x" }));
    assert!(!res.text().contains("disabled via CODEGRAPH_MCP_TOOLS"));
}

#[test]
fn static_tools_honor_the_allowlist_too() {
    let _env = env_write();
    {
        let _guard = EnvVarGuard::unset("CODEGRAPH_MCP_TOOLS");
        assert_eq!(get_static_tools().len(), tools().len());
    }
    {
        let _guard = EnvVarGuard::set("CODEGRAPH_MCP_TOOLS", "explore,files");
        let mut names: Vec<String> = get_static_tools().into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(names, vec!["codegraph_explore", "codegraph_files"]);
    }
}

// =============================================================================
// codegraph_files path-filter normalization (#426)
// (__tests__/mcp-files-path-normalization.test.ts)
// =============================================================================

fn files_fixture(root: &Path) -> CodeGraph {
    write(&root.join("src/index.ts"), "export const x = 1;\n");
    write(
        &root.join("src/components/Button.ts"),
        "export const Button = () => 1;\n",
    );
    write(&root.join("tests/a.test.ts"), "export const t = 1;\n");
    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn listed(handler: &ToolHandler, path_filter: Option<&str>) -> String {
    let mut args = serde_json::Map::new();
    if let Some(pf) = path_filter {
        args.insert("path".into(), json!(pf));
    }
    args.insert("format".into(), json!("flat"));
    args.insert("includeMetadata".into(), json!(false));
    let result = handler.execute("codegraph_files", &serde_json::Value::Object(args));
    assert_ne!(
        result.is_error,
        Some(true),
        "codegraph_files errored: {}",
        result.text()
    );
    result.text().to_string()
}

#[test]
fn treats_rootish_path_filters_as_project_root() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));

    // Root-ish filters: every shape an agent might guess for "whole project"
    // must list the same files as no filter at all.
    for rootish in ["/", ".", "./", "", "\\", "//", ".//"] {
        let output = listed(&handler, Some(rootish));
        assert!(
            output.contains("src/index.ts"),
            "path={rootish:?}:\n{output}"
        );
        assert!(
            output.contains("src/components/Button.ts"),
            "path={rootish:?}"
        );
        assert!(output.contains("tests/a.test.ts"), "path={rootish:?}");
    }
}

#[test]
fn matches_a_real_subdirectory_prefix() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_leading_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("/src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_leading_dot_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("./src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn tolerates_a_trailing_slash_on_a_real_subdirectory() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src/"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("tests/a.test.ts"));
}

#[test]
fn normalizes_windows_backslashes() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = files_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let output = listed(&handler, Some("src\\components"));
    assert!(output.contains("src/components/Button.ts"));
    assert!(!output.contains("src/index.ts"));
}

#[test]
fn does_not_match_sibling_directories_that_share_a_prefix() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = Rc::new(files_fixture(dir.path()));
    let handler = ToolHandler::new(Some(Rc::clone(&cg)));

    // Old code matched on raw `startsWith`, so a filter "src" would also
    // return a sibling like "src-utils/...".
    write(
        &dir.path().join("src-utils/helper.ts"),
        "export const h = 1;\n",
    );
    cg.index_all(&IndexOptions::default()).unwrap();

    let output = listed(&handler, Some("src"));
    assert!(output.contains("src/index.ts"));
    assert!(!output.contains("src-utils/helper.ts"));
}
