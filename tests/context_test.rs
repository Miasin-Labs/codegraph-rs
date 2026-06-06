//! Context Builder Tests
//!
//! Ported from `__tests__/context.test.ts` and `__tests__/context-ranking.test.ts`.
//!
//! The TS suites index real TS source files through the `CodeGraph` facade;
//! the facade/extraction pipeline is a separate port task, so these tests
//! write the same fixture files to disk (the builder reads real files for
//! code blocks) and insert the node/edge shapes extraction + resolution
//! produce for them directly via `QueryBuilder` (real SQLite, no mocks).

use std::fs;
use std::rc::Rc;

use codegraph::context::{ContextBuilder, LOW_CONFIDENCE_MARKER};
use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::graph::GraphTraverser;
use codegraph::search::is_distinctive_identifier;
use codegraph::types::{
    BuildContextOptions,
    Confidence,
    ContextFormat,
    Edge,
    EdgeKind,
    FileRecord,
    FindRelevantContextOptions,
    Language,
    Node,
    NodeKind,
    TaskInput,
};
use tempfile::TempDir;

// =============================================================================
// Fixture (mirrors the sample codebase in __tests__/context.test.ts)
// =============================================================================

const PAYMENT_TS: &str = r#"/**
 * Payment Service
 * Handles payment processing logic.
 */

export interface PaymentResult {
  success: boolean;
  transactionId: string;
  amount: number;
}

export class PaymentService {
  private apiKey: string;

  constructor(apiKey: string) {
    this.apiKey = apiKey;
  }

  /**
   * Process a payment for the given amount
   */
  async processPayment(amount: number): Promise<PaymentResult> {
    // Validate amount
    if (amount <= 0) {
      throw new Error('Invalid amount');
    }

    // Process payment
    const transactionId = this.generateTransactionId();
    return {
      success: true,
      transactionId,
      amount,
    };
  }

  private generateTransactionId(): string {
    return 'txn_' + Math.random().toString(36).substring(2);
  }
}

export function createPaymentService(apiKey: string): PaymentService {
  return new PaymentService(apiKey);
}
"#;

const CHECKOUT_TS: &str = r#"/**
 * Checkout Controller
 * Handles the checkout flow.
 */

import { PaymentService, PaymentResult } from './payment';

export interface CartItem {
  id: string;
  name: string;
  price: number;
  quantity: number;
}

export class CheckoutController {
  private paymentService: PaymentService;

  constructor(paymentService: PaymentService) {
    this.paymentService = paymentService;
  }

  /**
   * Process checkout for the given cart
   */
  async processCheckout(cart: CartItem[]): Promise<PaymentResult> {
    const total = this.calculateTotal(cart);

    if (total === 0) {
      throw new Error('Cart is empty');
    }

    return this.paymentService.processPayment(total);
  }

  /**
   * Calculate the total price of the cart
   */
  calculateTotal(cart: CartItem[]): number {
    return cart.reduce((sum, item) => sum + item.price * item.quantity, 0);
  }
}
"#;

const UTILS_TS: &str = r#"/**
 * Utility functions
 */

export function formatCurrency(amount: number): string {
  return '$' + amount.toFixed(2);
}

export function validateEmail(email: string): boolean {
  return email.includes('@');
}
"#;

struct Fixture {
    dir: TempDir,
    _conn: DatabaseConnection,
    queries: Rc<QueryBuilder>,
}

impl Fixture {
    fn builder(&self) -> ContextBuilder {
        ContextBuilder::new(
            self.dir.path(),
            Rc::clone(&self.queries),
            GraphTraverser::new(Rc::clone(&self.queries)),
        )
    }
}

fn make_file(path: &str, language: Language) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: format!("hash-{path}"),
        language,
        size: 100,
        modified_at: 1_700_000_000_000,
        indexed_at: 1_700_000_000_000,
        node_count: 1,
        errors: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: u32,
    end_line: u32,
    exported: bool,
) -> Node {
    let mut node = Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
    );
    node.is_exported = Some(exported);
    node
}

/// Builds the graph extraction + resolution produce for the sample codebase
/// (payment.ts / checkout.ts / utils.ts), with the real files on disk.
fn build_fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("payment.ts"), PAYMENT_TS).expect("write payment.ts");
    fs::write(src.join("checkout.ts"), CHECKOUT_TS).expect("write checkout.ts");
    fs::write(src.join("utils.ts"), UTILS_TS).expect("write utils.ts");

    let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).expect("init db");
    let queries = Rc::new(QueryBuilder::new(conn.get_db().expect("db handle")));

    for path in ["src/payment.ts", "src/checkout.ts", "src/utils.ts"] {
        queries
            .upsert_file(&make_file(path, Language::Typescript))
            .expect("upsert file");
    }

    let ts = Language::Typescript;
    let nodes = vec![
        // src/payment.ts
        make_node(
            "file:payment",
            NodeKind::File,
            "payment.ts",
            "src/payment.ts",
            "src/payment.ts",
            ts,
            1,
            44,
            false,
        ),
        make_node(
            "iface:PaymentResult",
            NodeKind::Interface,
            "PaymentResult",
            "src/payment.ts::PaymentResult",
            "src/payment.ts",
            ts,
            6,
            10,
            true,
        ),
        make_node(
            "class:PaymentService",
            NodeKind::Class,
            "PaymentService",
            "src/payment.ts::PaymentService",
            "src/payment.ts",
            ts,
            12,
            40,
            true,
        ),
        make_node(
            "method:processPayment",
            NodeKind::Method,
            "processPayment",
            "src/payment.ts::PaymentService.processPayment",
            "src/payment.ts",
            ts,
            22,
            35,
            false,
        ),
        make_node(
            "method:generateTransactionId",
            NodeKind::Method,
            "generateTransactionId",
            "src/payment.ts::PaymentService.generateTransactionId",
            "src/payment.ts",
            ts,
            37,
            39,
            false,
        ),
        make_node(
            "fn:createPaymentService",
            NodeKind::Function,
            "createPaymentService",
            "src/payment.ts::createPaymentService",
            "src/payment.ts",
            ts,
            42,
            44,
            true,
        ),
        // src/checkout.ts
        make_node(
            "file:checkout",
            NodeKind::File,
            "checkout.ts",
            "src/checkout.ts",
            "src/checkout.ts",
            ts,
            1,
            41,
            false,
        ),
        make_node(
            "iface:CartItem",
            NodeKind::Interface,
            "CartItem",
            "src/checkout.ts::CartItem",
            "src/checkout.ts",
            ts,
            8,
            13,
            true,
        ),
        make_node(
            "class:CheckoutController",
            NodeKind::Class,
            "CheckoutController",
            "src/checkout.ts::CheckoutController",
            "src/checkout.ts",
            ts,
            15,
            41,
            true,
        ),
        make_node(
            "method:processCheckout",
            NodeKind::Method,
            "processCheckout",
            "src/checkout.ts::CheckoutController.processCheckout",
            "src/checkout.ts",
            ts,
            25,
            33,
            false,
        ),
        make_node(
            "method:calculateTotal",
            NodeKind::Method,
            "calculateTotal",
            "src/checkout.ts::CheckoutController.calculateTotal",
            "src/checkout.ts",
            ts,
            38,
            40,
            false,
        ),
        // src/utils.ts
        make_node(
            "file:utils",
            NodeKind::File,
            "utils.ts",
            "src/utils.ts",
            "src/utils.ts",
            ts,
            1,
            11,
            false,
        ),
        make_node(
            "fn:formatCurrency",
            NodeKind::Function,
            "formatCurrency",
            "src/utils.ts::formatCurrency",
            "src/utils.ts",
            ts,
            5,
            7,
            true,
        ),
        make_node(
            "fn:validateEmail",
            NodeKind::Function,
            "validateEmail",
            "src/utils.ts::validateEmail",
            "src/utils.ts",
            ts,
            9,
            11,
            true,
        ),
    ];
    queries.insert_nodes(&nodes).expect("insert nodes");

    let edges = vec![
        // Containment
        Edge::new("file:payment", "iface:PaymentResult", EdgeKind::Contains),
        Edge::new("file:payment", "class:PaymentService", EdgeKind::Contains),
        Edge::new(
            "class:PaymentService",
            "method:processPayment",
            EdgeKind::Contains,
        ),
        Edge::new(
            "class:PaymentService",
            "method:generateTransactionId",
            EdgeKind::Contains,
        ),
        Edge::new(
            "file:payment",
            "fn:createPaymentService",
            EdgeKind::Contains,
        ),
        Edge::new("file:checkout", "iface:CartItem", EdgeKind::Contains),
        Edge::new(
            "file:checkout",
            "class:CheckoutController",
            EdgeKind::Contains,
        ),
        Edge::new(
            "class:CheckoutController",
            "method:processCheckout",
            EdgeKind::Contains,
        ),
        Edge::new(
            "class:CheckoutController",
            "method:calculateTotal",
            EdgeKind::Contains,
        ),
        Edge::new("file:utils", "fn:formatCurrency", EdgeKind::Contains),
        Edge::new("file:utils", "fn:validateEmail", EdgeKind::Contains),
        // Imports (resolved to the imported symbols, as ReferenceResolver does)
        Edge::new("file:checkout", "class:PaymentService", EdgeKind::Imports),
        Edge::new("file:checkout", "iface:PaymentResult", EdgeKind::Imports),
        // Calls
        Edge::new(
            "method:processCheckout",
            "method:calculateTotal",
            EdgeKind::Calls,
        ),
        Edge::new(
            "method:processCheckout",
            "method:processPayment",
            EdgeKind::Calls,
        ),
        Edge::new(
            "method:processPayment",
            "method:generateTransactionId",
            EdgeKind::Calls,
        ),
        // Instantiation / returns
        Edge::new(
            "fn:createPaymentService",
            "class:PaymentService",
            EdgeKind::Instantiates,
        ),
    ];
    queries.insert_edges(&edges).expect("insert edges");

    Fixture {
        dir,
        _conn: conn,
        queries,
    }
}

fn markdown_opts() -> BuildContextOptions {
    BuildContextOptions {
        format: Some(ContextFormat::Markdown),
        ..Default::default()
    }
}

fn json_opts() -> BuildContextOptions {
    BuildContextOptions {
        format: Some(ContextFormat::Json),
        ..Default::default()
    }
}

// =============================================================================
// getCode()
// =============================================================================

#[test]
fn get_code_extracts_code_for_a_node() {
    let fx = build_fixture();
    let builder = fx.builder();

    let code = builder
        .get_code("class:PaymentService")
        .expect("get_code")
        .expect("code present");

    assert!(code.contains("class PaymentService"));
    assert!(code.contains("processPayment"));
}

#[test]
fn get_code_returns_none_for_non_existent_node() {
    let fx = build_fixture();
    let builder = fx.builder();

    let code = builder.get_code("non-existent-id").expect("get_code");
    assert!(code.is_none());
}

// =============================================================================
// findRelevantContext()
// =============================================================================

#[test]
fn finds_relevant_nodes_for_a_query() {
    let fx = build_fixture();
    let builder = fx.builder();

    // Use simple query that matches symbol names (FTS5 treats spaces as AND)
    let result = builder
        .find_relevant_context("PaymentService", &FindRelevantContextOptions::default())
        .expect("find");

    assert!(!result.nodes.is_empty());
    // Should find payment-related nodes
    assert!(result.nodes.values().any(|n| {
        let name = n.name.to_lowercase();
        name.contains("payment") || name.contains("checkout")
    }));
}

#[test]
fn includes_edges_in_the_result() {
    let fx = build_fixture();
    let builder = fx.builder();

    let result = builder
        .find_relevant_context(
            "checkout",
            &FindRelevantContextOptions {
                traversal_depth: Some(2),
                ..Default::default()
            },
        )
        .expect("find");

    // Should have some edges from traversal
    assert!(!result.edges.is_empty());
}

#[test]
fn respects_max_nodes_option() {
    let fx = build_fixture();
    let builder = fx.builder();

    let result = builder
        .find_relevant_context(
            "function",
            &FindRelevantContextOptions {
                max_nodes: Some(5),
                ..Default::default()
            },
        )
        .expect("find");

    assert!(result.nodes.len() <= 5);
}

// =============================================================================
// buildContext()
// =============================================================================

#[test]
fn builds_context_with_markdown_format() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Text("Fix checkout error".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Markdown),
                max_code_blocks: Some(3),
                ..Default::default()
            },
        )
        .expect("build");

    // Should contain markdown structure
    assert!(markdown.contains("## Code Context"));
    assert!(markdown.contains("**Query:** Fix checkout error"));
}

#[test]
fn builds_context_with_json_format() {
    let fx = build_fixture();
    let builder = fx.builder();

    let json = builder
        .build_context(
            &TaskInput::Text("payment processing".to_string()),
            &json_opts(),
        )
        .expect("build");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed["query"], "payment processing");
    assert!(parsed["nodes"].is_array());
}

#[test]
fn accepts_object_input_with_title_and_description() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Titled {
                title: "Checkout bug".to_string(),
                description: Some("Cart total calculation is wrong".to_string()),
            },
            &markdown_opts(),
        )
        .expect("build");

    assert!(markdown.contains("Checkout bug: Cart total calculation is wrong"));
}

#[test]
fn includes_code_blocks_when_requested() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Text("PaymentService".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Markdown),
                include_code: Some(true),
                max_code_blocks: Some(2),
                ..Default::default()
            },
        )
        .expect("build");

    // Should contain code blocks
    assert!(markdown.contains("### Code"));
    assert!(markdown.contains("```typescript"));
}

#[test]
fn excludes_code_blocks_when_requested() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Text("payment".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Markdown),
                include_code: Some(false),
                ..Default::default()
            },
        )
        .expect("build");

    // Should not contain code section
    assert!(!markdown.contains("### Code"));
}

#[test]
fn includes_related_symbols_in_compact_format() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Text("checkout".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Markdown),
                max_nodes: Some(10),
                ..Default::default()
            },
        )
        .expect("build");

    // Compact format uses "Related Symbols" instead of verbose "Related Files"
    // and groups symbols by file for compactness
    assert!(markdown.contains("### Entry Points"));
}

#[test]
fn has_compact_output_without_verbose_stats_footer() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(&TaskInput::Text("payment".to_string()), &markdown_opts())
        .expect("build");

    // Compact format should NOT have verbose stats footer
    assert!(!markdown.contains("*Context:"));
    // But should still have query
    assert!(markdown.contains("**Query:**"));
}

// =============================================================================
// Context structure
// =============================================================================

#[test]
fn finds_entry_points_from_search() {
    let fx = build_fixture();
    let builder = fx.builder();

    let json = builder
        .build_context(&TaskInput::Text("PaymentService".to_string()), &json_opts())
        .expect("build");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let entry_points = parsed["entryPoints"].as_array().expect("entryPoints array");
    assert!(!entry_points.is_empty());
}

#[test]
fn traverses_graph_from_entry_points() {
    let fx = build_fixture();
    let builder = fx.builder();

    let json = builder
        .build_context(
            &TaskInput::Text("CheckoutController".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Json),
                traversal_depth: Some(2),
                ..Default::default()
            },
        )
        .expect("build");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let node_names: Vec<&str> = parsed["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();

    // CheckoutController calls PaymentService, so both should be present
    assert!(node_names.iter().any(|name| name.contains("Checkout")));
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn handles_empty_query() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(&TaskInput::Text(String::new()), &markdown_opts())
        .expect("build");

    assert!(markdown.contains("## Code Context"));
}

#[test]
fn handles_query_with_no_matches() {
    let fx = build_fixture();
    let builder = fx.builder();

    let json = builder
        .build_context(
            &TaskInput::Text("xyznonexistent123".to_string()),
            &json_opts(),
        )
        .expect("build");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    // Should return empty or minimal results
    assert!(parsed["nodes"].is_array());
}

#[test]
fn truncates_long_code_blocks() {
    let fx = build_fixture();
    let builder = fx.builder();

    let markdown = builder
        .build_context(
            &TaskInput::Text("PaymentService".to_string()),
            &BuildContextOptions {
                format: Some(ContextFormat::Markdown),
                max_code_block_size: Some(100),
                include_code: Some(true),
                ..Default::default()
            },
        )
        .expect("build");

    // Long code blocks should be truncated
    if markdown.contains("```typescript") {
        assert!(markdown.contains("\n... (truncated) ..."));
    }
}

// =============================================================================
// isDistinctiveIdentifier (from __tests__/context-ranking.test.ts)
// =============================================================================

#[test]
fn treats_plain_dictionary_words_as_non_distinctive() {
    for word in ["flat", "object", "screen", "standing", "capture"] {
        assert!(
            !is_distinctive_identifier(word),
            "{word} should not be distinctive"
        );
    }
}

#[test]
fn treats_leading_capital_only_words_as_non_distinctive() {
    assert!(!is_distinctive_identifier("Screen"));
    assert!(!is_distinctive_identifier("Zustand"));
}

#[test]
fn treats_camel_pascal_snake_acronyms_digits_as_distinctive() {
    assert!(is_distinctive_identifier("setLastEmail"));
    assert!(is_distinctive_identifier("OrgUserStore"));
    assert!(is_distinctive_identifier("user_store"));
    assert!(is_distinctive_identifier("REST"));
    assert!(is_distinctive_identifier("v2"));
}

// =============================================================================
// Context ranking — common-word precision & confidence
// (from __tests__/context-ranking.test.ts)
// =============================================================================

/// The corroborated target: a capture-flow screen whose NAME alone matches
/// three query terms (capture + intro + screen), and which lives under a
/// matching directory. The trap: an unrelated constant literally named FLAT,
/// in a totally different area — "flat" in a prose query exact-matches it.
fn build_ranking_fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");

    let capture_dir = dir.path().join("src/app/capture");
    fs::create_dir_all(&capture_dir).expect("mkdir capture");
    fs::write(
        capture_dir.join("intro.tsx"),
        "export function CaptureIntroScreen() {\n  // Onboarding screen shown before the user selects flat or standing object capture.\n  return null;\n}\n",
    )
    .expect("write intro.tsx");

    let scripts_dir = dir.path().join("scripts/dataset");
    fs::create_dir_all(&scripts_dir).expect("mkdir scripts");
    fs::write(
        scripts_dir.join("download.ts"),
        "export const FLAT = 'freiburg_flat_dataset';\nexport function downloadDataset(name: string): string { return name; }\n",
    )
    .expect("write download.ts");

    let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).expect("init db");
    let queries = Rc::new(QueryBuilder::new(conn.get_db().expect("db handle")));

    queries
        .upsert_file(&make_file("src/app/capture/intro.tsx", Language::Tsx))
        .expect("upsert file");
    queries
        .upsert_file(&make_file(
            "scripts/dataset/download.ts",
            Language::Typescript,
        ))
        .expect("upsert file");

    let nodes = vec![
        make_node(
            "file:intro",
            NodeKind::File,
            "intro.tsx",
            "src/app/capture/intro.tsx",
            "src/app/capture/intro.tsx",
            Language::Tsx,
            1,
            4,
            false,
        ),
        make_node(
            "fn:CaptureIntroScreen",
            NodeKind::Function,
            "CaptureIntroScreen",
            "src/app/capture/intro.tsx::CaptureIntroScreen",
            "src/app/capture/intro.tsx",
            Language::Tsx,
            1,
            4,
            true,
        ),
        make_node(
            "file:download",
            NodeKind::File,
            "download.ts",
            "scripts/dataset/download.ts",
            "scripts/dataset/download.ts",
            Language::Typescript,
            1,
            2,
            false,
        ),
        make_node(
            "const:FLAT",
            NodeKind::Constant,
            "FLAT",
            "scripts/dataset/download.ts::FLAT",
            "scripts/dataset/download.ts",
            Language::Typescript,
            1,
            1,
            true,
        ),
        make_node(
            "fn:downloadDataset",
            NodeKind::Function,
            "downloadDataset",
            "scripts/dataset/download.ts::downloadDataset",
            "scripts/dataset/download.ts",
            Language::Typescript,
            2,
            2,
            true,
        ),
    ];
    queries.insert_nodes(&nodes).expect("insert nodes");

    let edges = vec![
        Edge::new("file:intro", "fn:CaptureIntroScreen", EdgeKind::Contains),
        Edge::new("file:download", "const:FLAT", EdgeKind::Contains),
        Edge::new("file:download", "fn:downloadDataset", EdgeKind::Contains),
    ];
    queries.insert_edges(&edges).expect("insert edges");

    Fixture {
        dir,
        _conn: conn,
        queries,
    }
}

#[test]
fn does_not_let_a_common_word_exact_match_outrank_a_corroborated_symbol() {
    let fx = build_ranking_fixture();
    let builder = fx.builder();

    let sg = builder
        .find_relevant_context(
            "capture intro onboarding screen flat object",
            &FindRelevantContextOptions::default(),
        )
        .expect("find");
    let root_names: Vec<&str> = sg
        .roots
        .iter()
        .filter_map(|id| sg.nodes.get(id).map(|n| n.name.as_str()))
        .collect();

    // The corroborated capture screen surfaces as an entry point...
    assert!(
        root_names.contains(&"CaptureIntroScreen"),
        "roots were {root_names:?}"
    );
    // ...and the trap constant is never the lead result (the bug we fixed).
    assert_ne!(root_names.first().copied(), Some("FLAT"));

    let cap_idx = root_names.iter().position(|n| *n == "CaptureIntroScreen");
    let flat_idx = root_names.iter().position(|n| *n == "FLAT");
    if let (Some(cap), Some(flat)) = (cap_idx, flat_idx) {
        assert!(cap < flat, "CaptureIntroScreen should outrank FLAT");
    }

    // And it's confidently answered (we located a corroborated symbol).
    assert_eq!(sg.confidence, Some(Confidence::High));
}

#[test]
fn flags_low_confidence_and_emits_the_handoff_when_only_common_words_match() {
    let fx = build_ranking_fixture();
    let builder = fx.builder();

    let query = "flat object thing";
    let sg = builder
        .find_relevant_context(query, &FindRelevantContextOptions::default())
        .expect("find");
    assert_eq!(sg.confidence, Some(Confidence::Low));

    let md = builder
        .build_context(&TaskInput::Text(query.to_string()), &markdown_opts())
        .expect("build");
    assert!(md.contains(LOW_CONFIDENCE_MARKER));
    // The handoff routes to the precise tools rather than claiming completeness.
    assert!(md.contains("codegraph_explore"));
}

#[test]
fn does_not_emit_the_handoff_for_a_precise_distinctive_symbol_query() {
    let fx = build_ranking_fixture();
    let builder = fx.builder();

    let sg = builder
        .find_relevant_context("CaptureIntroScreen", &FindRelevantContextOptions::default())
        .expect("find");
    assert_eq!(sg.confidence, Some(Confidence::High));

    let md = builder
        .build_context(
            &TaskInput::Text("CaptureIntroScreen".to_string()),
            &markdown_opts(),
        )
        .expect("build");
    assert!(!md.contains(LOW_CONFIDENCE_MARKER));
}
