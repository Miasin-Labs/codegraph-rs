//! Per-language rules for control-flow graph construction.
//!
//! Each language defines which tree-sitter node kinds map to control-flow
//! constructs (conditionals, loops, switches, jumps, exceptions). The walker
//! in [`crate::cfg`] is language-agnostic: it asks [`CfgRules::classify`]
//! what a node *is* and never branches on a language id itself.

/// Language-specific rules for CFG construction.
pub struct CfgRules {
    /// Node kinds representing if/elif/conditional expressions.
    pub if_nodes: &'static [&'static str],
    /// Node kinds for chained `elif`-style alternatives that carry their own
    /// condition (Python `elif_clause`, PHP `else_if_clause`).
    pub elif_nodes: &'static [&'static str],
    /// Node kind for else clauses.
    pub else_node: Option<&'static str>,
    /// Transparent statement containers the walker descends into, in order:
    /// blocks, else clauses, `with` bodies, statement lists.
    pub block_nodes: &'static [&'static str],
    /// Node kinds for for-style loops.
    pub for_nodes: &'static [&'static str],
    /// Node kinds for while-style loops.
    pub while_nodes: &'static [&'static str],
    /// Node kind for infinite loops (Rust's `loop`).
    pub loop_node: Option<&'static str>,
    /// Node kinds for switch/match statements.
    pub switch_nodes: &'static [&'static str],
    /// Node kinds for individual case/arm entries.
    pub case_nodes: &'static [&'static str],
    /// Case kinds whose end falls into the next case (C-style `switch`).
    pub fallthrough_cases: &'static [&'static str],
    /// Explicit fall-into-next-case statement (Go `fallthrough`).
    pub fallthrough_node: Option<&'static str>,
    /// Whether an unlabeled `break` inside a switch exits the switch (C, Java,
    /// JS, Go) rather than the enclosing loop (Rust/Python `match`).
    pub break_exits_switch: bool,
    /// How a switch recognises its catch-all arm.
    pub default_case: SwitchDefault,
    /// Node kinds for try blocks.
    pub try_nodes: &'static [&'static str],
    /// Node kind for catch/except clauses.
    pub catch_node: Option<&'static str>,
    /// Node kind for finally blocks.
    pub finally_node: Option<&'static str>,
    /// Node kind for return statements/expressions.
    pub return_node: Option<&'static str>,
    /// Postfix error-propagation operator that returns early (Rust `?`).
    pub try_operator: Option<&'static str>,
    /// Node kind for break statements.
    pub break_node: Option<&'static str>,
    /// Node kind for continue statements.
    pub continue_node: Option<&'static str>,
    /// Node kind for throw/raise/panic expressions.
    pub throw_node: Option<&'static str>,
    /// How jump labels are spelled (`break 'outer`, `continue outer`).
    pub labels: LabelStyle,
    /// Nested function scopes (closures, lambdas, local classes) besides
    /// [`Self::function_nodes`]: a `return` inside one leaves *that* scope,
    /// so the embedded-exit scan never enters them.
    pub nested_scope_nodes: &'static [&'static str],
    /// Field name to extract the function body.
    pub body_field: &'static str,
    /// Node kinds representing function definitions.
    pub function_nodes: &'static [&'static str],
}

/// How a language spells jump labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// No labeled jumps are modelled.
    None,
    /// The label is a leading child of the loop or block it names
    /// (Rust `'outer: loop {}`, `'a: {}`).
    Leading {
        /// Node kind of the label, on the loop and on `break`/`continue`.
        label: &'static str,
    },
    /// A wrapper statement carries the label of the statement it wraps
    /// (JS/TS/Go/Java/C `outer: for (…)`).
    Wrapper {
        /// Node kind of the labeled wrapper statement.
        statement: &'static str,
        /// Node kind of the label, on the wrapper and on `break`/`continue`.
        label: &'static str,
    },
}

impl LabelStyle {
    /// Node kind of a label name, as it appears on `break`/`continue`.
    pub fn label_kind(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Leading { label } | Self::Wrapper { label, .. } => Some(label),
        }
    }
}

/// How a switch recognises its catch-all arm, which decides whether it
/// needs a "no arm matched" (`otherwise`) edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchDefault {
    /// Exhaustive by construction (Rust/Move/Fe `match`, Erlang clauses):
    /// no `otherwise` edge.
    Exhaustive,
    /// An arm whose source text starts with this keyword is the catch-all
    /// (`default:`, Python `case _:`).
    Keyword(&'static str),
}

/// The control-flow role of a syntax node under a language's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Construct {
    /// A transparent container: walk its children in order.
    Block,
    /// A two-way conditional, possibly with elif/else alternatives.
    If,
    /// A loop that tests a condition (or iterator) before each iteration.
    Loop,
    /// A loop without a condition (Rust `loop`, R `repeat`).
    InfiniteLoop,
    /// A multi-way branch (switch/match).
    Switch,
    /// A try/catch/finally statement.
    Try,
    /// A labeled statement wrapper (`outer: for …`).
    Labeled,
    /// `return`.
    Return,
    /// `throw` / `raise` / `revert` / `abort`.
    Throw,
    /// `break`.
    Break,
    /// `continue` / `next`.
    Continue,
    /// Go `fallthrough`.
    Fallthrough,
    /// Anything else: a straight-line statement.
    Plain,
}

impl CfgRules {
    /// Look up rules for a language by its identifier.
    pub fn for_language(lang: &str) -> Option<&'static CfgRules> {
        match lang {
            "rust" => Some(&RUST_CFG_RULES),
            "typescript" | "javascript" | "arkts" => Some(&TYPESCRIPT_CFG_RULES),
            "python" => Some(&PYTHON_CFG_RULES),
            "go" => Some(&GO_CFG_RULES),
            "java" => Some(&JAVA_CFG_RULES),
            "c" => Some(&C_CFG_RULES),
            "cpp" => Some(&CPP_CFG_RULES),
            "php" => Some(&PHP_CFG_RULES),
            "r" => Some(&R_CFG_RULES),
            "solidity" => Some(&SOLIDITY_CFG_RULES),
            "vyper" => Some(&PYTHON_CFG_RULES),
            "move" => Some(&MOVE_CFG_RULES),
            "cairo" | "sway" => Some(&RUST_CFG_RULES),
            "fe" => Some(&FE_CFG_RULES),
            "nix" => Some(&NIX_CFG_RULES),
            "cfml" | "cfscript" | "cfquery" => Some(&CFSCRIPT_CFG_RULES),
            "erlang" => Some(&ERLANG_CFG_RULES),
            // VB.NET and COBOL bodies are unfielded/synthetic, while Terraform
            // has no function construct. `build_cfg` requires a body field.
            _ => None,
        }
    }

    /// Classify a node kind into its control-flow role.
    pub fn classify(&self, kind: &str) -> Construct {
        let is = |slot: Option<&str>| slot == Some(kind);
        if self.block_nodes.contains(&kind) {
            Construct::Block
        } else if self.if_nodes.contains(&kind) {
            Construct::If
        } else if self.for_nodes.contains(&kind) || self.while_nodes.contains(&kind) {
            Construct::Loop
        } else if is(self.loop_node) {
            Construct::InfiniteLoop
        } else if self.switch_nodes.contains(&kind) {
            Construct::Switch
        } else if self.try_nodes.contains(&kind) {
            Construct::Try
        } else if matches!(self.labels, LabelStyle::Wrapper { statement, .. } if statement == kind)
        {
            Construct::Labeled
        } else if is(self.return_node) {
            Construct::Return
        } else if is(self.throw_node) {
            Construct::Throw
        } else if is(self.break_node) {
            Construct::Break
        } else if is(self.continue_node) {
            Construct::Continue
        } else if is(self.fallthrough_node) {
            Construct::Fallthrough
        } else {
            Construct::Plain
        }
    }

    /// Whether `kind` opens a nested function scope (a nested function,
    /// closure, lambda, or local class) whose jumps are not this function's.
    pub fn is_nested_scope(&self, kind: &str) -> bool {
        self.function_nodes.contains(&kind) || self.nested_scope_nodes.contains(&kind)
    }

    /// Whether a switch arm whose source text is `arm_text` is the catch-all.
    pub fn is_default_arm(&self, arm_text: &str) -> bool {
        let SwitchDefault::Keyword(keyword) = self.default_case else {
            return false;
        };
        arm_text
            .trim_start()
            .strip_prefix(keyword)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    }
}

// ─── Rust ────────────────────────────────────────────────────────────────────

static RUST_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_expression"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["block", "else_clause", "unsafe_block"],
    for_nodes: &["for_expression"],
    while_nodes: &["while_expression"],
    loop_node: Some("loop_expression"),
    switch_nodes: &["match_expression"],
    case_nodes: &["match_arm"],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: Some("return_expression"),
    try_operator: Some("try_expression"),
    break_node: Some("break_expression"),
    continue_node: Some("continue_expression"),
    throw_node: None,
    labels: LabelStyle::Leading { label: "label" },
    nested_scope_nodes: &["closure_expression", "async_block", "gen_block"],
    body_field: "body",
    function_nodes: &["function_item"],
};

// ─── TypeScript ──────────────────────────────────────────────────────────────

static TYPESCRIPT_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["statement_block", "else_clause"],
    for_nodes: &["for_statement", "for_in_statement"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_statement"],
    case_nodes: &["switch_case", "switch_default"],
    fallthrough_cases: &["switch_case", "switch_default"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &["try_statement"],
    catch_node: Some("catch_clause"),
    finally_node: Some("finally_clause"),
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("throw_statement"),
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "statement_identifier",
    },
    nested_scope_nodes: &[
        "function_expression",
        "function",
        "generator_function",
        "generator_function_declaration",
        "class",
        "class_declaration",
    ],
    body_field: "body",
    function_nodes: &[
        "function_declaration",
        "method_definition",
        "arrow_function",
    ],
};

// ─── Python ──────────────────────────────────────────────────────────────────

static PYTHON_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &["elif_clause"],
    else_node: Some("else_clause"),
    block_nodes: &["block", "else_clause", "with_statement"],
    for_nodes: &["for_statement"],
    while_nodes: &["while_statement"],
    loop_node: None,
    switch_nodes: &["match_statement"],
    case_nodes: &["case_clause"],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Keyword("case _"),
    try_nodes: &["try_statement"],
    catch_node: Some("except_clause"),
    finally_node: Some("finally_clause"),
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("raise_statement"),
    labels: LabelStyle::None,
    nested_scope_nodes: &["lambda", "class_definition"],
    body_field: "body",
    function_nodes: &["function_definition"],
};

// ─── Go ──────────────────────────────────────────────────────────────────────

static GO_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    // `block` wraps a `statement_list` in tree-sitter-go 0.25.
    block_nodes: &["block", "statement_list"],
    for_nodes: &["for_statement"],
    while_nodes: &[],
    loop_node: None,
    switch_nodes: &[
        "expression_switch_statement",
        "type_switch_statement",
        "select_statement",
    ],
    case_nodes: &[
        "expression_case",
        "type_case",
        "default_case",
        "communication_case",
    ],
    fallthrough_cases: &[],
    fallthrough_node: Some("fallthrough_statement"),
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: None,
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "label_name",
    },
    nested_scope_nodes: &["func_literal"],
    body_field: "body",
    function_nodes: &["function_declaration", "method_declaration"],
};

// ─── Java ────────────────────────────────────────────────────────────────────

static JAVA_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["block", "constructor_body"],
    for_nodes: &["for_statement", "enhanced_for_statement"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_expression"],
    case_nodes: &["switch_block_statement_group", "switch_rule"],
    // `case 1:` groups fall through; `case 1 ->` rules do not.
    fallthrough_cases: &["switch_block_statement_group"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &["try_statement", "try_with_resources_statement"],
    catch_node: Some("catch_clause"),
    finally_node: Some("finally_clause"),
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("throw_statement"),
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "identifier",
    },
    nested_scope_nodes: &["lambda_expression", "class_body"],
    body_field: "body",
    function_nodes: &["method_declaration", "constructor_declaration"],
};

// ─── C ───────────────────────────────────────────────────────────────────────

static C_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["compound_statement", "else_clause"],
    for_nodes: &["for_statement"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_statement"],
    case_nodes: &["case_statement"],
    fallthrough_cases: &["case_statement"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: None,
    // C has no labeled break; the wrapper is still descended (goto labels).
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "statement_identifier",
    },
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &["function_definition"],
};

// ─── C++ ─────────────────────────────────────────────────────────────────────

static CPP_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["compound_statement", "else_clause"],
    for_nodes: &["for_statement", "for_range_loop"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_statement"],
    case_nodes: &["case_statement"],
    fallthrough_cases: &["case_statement"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &["try_statement"],
    catch_node: Some("catch_clause"),
    finally_node: None,
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("throw_statement"),
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "statement_identifier",
    },
    nested_scope_nodes: &["lambda_expression"],
    body_field: "body",
    function_nodes: &["function_definition"],
};

// ─── PHP ─────────────────────────────────────────────────────────────────────

static PHP_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &["else_if_clause"],
    else_node: Some("else_clause"),
    block_nodes: &["compound_statement", "colon_block", "else_clause"],
    for_nodes: &["for_statement", "foreach_statement"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_statement", "match_expression"],
    case_nodes: &[
        "case_statement",
        "default_statement",
        "match_conditional_expression",
        "match_default_expression",
    ],
    fallthrough_cases: &["case_statement", "default_statement"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &["try_statement"],
    catch_node: Some("catch_clause"),
    finally_node: Some("finally_clause"),
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("throw_expression"),
    labels: LabelStyle::None,
    nested_scope_nodes: &["anonymous_function"],
    body_field: "body",
    function_nodes: &[
        "function_definition",
        "method_declaration",
        "arrow_function",
    ],
};

// ─── R ───────────────────────────────────────────────────────────────────────

static R_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: None,
    block_nodes: &["braced_expression"],
    for_nodes: &["for_statement"],
    while_nodes: &["while_statement"],
    loop_node: Some("repeat_statement"),
    switch_nodes: &[],
    case_nodes: &[],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    // `return(...)` is an ordinary call in tree-sitter-r.
    return_node: None,
    try_operator: None,
    break_node: Some("break"),
    continue_node: Some("next"),
    throw_node: None,
    labels: LabelStyle::None,
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &["function_definition"],
};

// ─── Solidity ────────────────────────────────────────────────────────────────

static SOLIDITY_CFG_RULES: CfgRules = CfgRules {
    // Yul control-flow nodes have no named body fields, so the generic CFG
    // walker cannot model them without a Yul-specific adapter.
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: None,
    // tree-sitter-solidity wraps every statement in a visible `statement`.
    block_nodes: &["block_statement", "function_body", "statement"],
    for_nodes: &["for_statement"],
    while_nodes: &["while_statement", "do_while_statement"],
    loop_node: None,
    switch_nodes: &[],
    case_nodes: &[],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &["try_statement"],
    catch_node: Some("catch_clause"),
    finally_node: None,
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("revert_statement"),
    labels: LabelStyle::None,
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &[
        "function_definition",
        "modifier_definition",
        "constructor_definition",
        "fallback_receive_definition",
    ],
};

// ─── Move ───────────────────────────────────────────────────────────────────

static MOVE_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_expression"],
    elif_nodes: &[],
    else_node: None,
    // `block_item` is Move's `expr;` statement wrapper.
    block_nodes: &["block", "block_item"],
    for_nodes: &[],
    while_nodes: &["while_expression"],
    loop_node: Some("loop_expression"),
    switch_nodes: &["match_expression"],
    case_nodes: &["match_arm"],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: Some("return_expression"),
    try_operator: None,
    break_node: Some("break_expression"),
    continue_node: Some("continue_expression"),
    throw_node: Some("abort_expression"),
    labels: LabelStyle::None,
    nested_scope_nodes: &["lambda_expression"],
    body_field: "body",
    function_nodes: &[
        "function_definition",
        "macro_function_definition",
        "usual_spec_function",
    ],
};

// ─── Fe ─────────────────────────────────────────────────────────────────────

static FE_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_expression"],
    elif_nodes: &[],
    else_node: None,
    block_nodes: &["block"],
    for_nodes: &["for_statement"],
    while_nodes: &["while_statement"],
    loop_node: None,
    switch_nodes: &["match_expression"],
    case_nodes: &["match_arm"],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: None,
    labels: LabelStyle::None,
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &["function_definition", "contract_init", "recv_arm"],
};

// ─── Nix ─────────────────────────────────────────────────────────────────────

static NIX_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_expression"],
    elif_nodes: &[],
    else_node: None,
    // `let … in e`, `with s; e`, `assert c; e` evaluate their parts in order.
    block_nodes: &["let_expression", "with_expression", "assert_expression"],
    for_nodes: &[],
    while_nodes: &[],
    loop_node: None,
    switch_nodes: &[],
    case_nodes: &[],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &[],
    catch_node: None,
    finally_node: None,
    return_node: None,
    try_operator: None,
    break_node: None,
    continue_node: None,
    throw_node: None,
    labels: LabelStyle::None,
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &["function_expression"],
};

// ─── CFML / CFScript / CFQuery ───────────────────────────────────────────────

static CFSCRIPT_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &["if_statement"],
    elif_nodes: &[],
    else_node: Some("else_clause"),
    block_nodes: &["statement_block", "else_clause"],
    for_nodes: &["for_statement", "for_in_statement"],
    while_nodes: &["while_statement", "do_statement"],
    loop_node: None,
    switch_nodes: &["switch_statement"],
    case_nodes: &["switch_case", "switch_default"],
    fallthrough_cases: &["switch_case", "switch_default"],
    fallthrough_node: None,
    break_exits_switch: true,
    default_case: SwitchDefault::Keyword("default"),
    try_nodes: &["try_statement"],
    catch_node: Some("catch_clause"),
    finally_node: Some("finally_clause"),
    return_node: Some("return_statement"),
    try_operator: None,
    break_node: Some("break_statement"),
    continue_node: Some("continue_statement"),
    throw_node: Some("throw_statement"),
    labels: LabelStyle::Wrapper {
        statement: "labeled_statement",
        label: "statement_identifier",
    },
    nested_scope_nodes: &[],
    body_field: "body",
    function_nodes: &[
        "function_declaration",
        "function_expression",
        "method_definition",
        "arrow_function",
    ],
};

// ─── Erlang ──────────────────────────────────────────────────────────────────

static ERLANG_CFG_RULES: CfgRules = CfgRules {
    if_nodes: &[],
    elif_nodes: &[],
    else_node: None,
    block_nodes: &["clause_body", "block_expr"],
    for_nodes: &[],
    while_nodes: &[],
    loop_node: None,
    // Erlang `if` is a multi-clause selection, not a binary if/else node.
    switch_nodes: &["if_expr", "case_expr", "receive_expr", "maybe_expr"],
    case_nodes: &["if_clause", "cr_clause", "receive_after"],
    fallthrough_cases: &[],
    fallthrough_node: None,
    break_exits_switch: false,
    // A clause set with no match raises; there is no silent fallthrough.
    default_case: SwitchDefault::Exhaustive,
    try_nodes: &["try_expr"],
    catch_node: Some("catch_clause"),
    finally_node: Some("try_after"),
    return_node: None,
    try_operator: None,
    break_node: None,
    continue_node: None,
    throw_node: None,
    labels: LabelStyle::None,
    nested_scope_nodes: &[],
    body_field: "body",
    // `fun_decl` is a wrapper without a body field; its clauses carry bodies.
    function_nodes: &["function_clause", "fun_clause"],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web3_languages_have_cfg_rules() {
        for language in ["vyper", "move", "cairo", "sway", "fe"] {
            assert!(
                CfgRules::for_language(language).is_some(),
                "missing CFG rules for {language}"
            );
        }
    }

    #[test]
    fn classify_maps_kinds_to_constructs() {
        let rust = CfgRules::for_language("rust").unwrap();
        assert_eq!(rust.classify("block"), Construct::Block);
        assert_eq!(rust.classify("loop_expression"), Construct::InfiniteLoop);
        assert_eq!(rust.classify("let_declaration"), Construct::Plain);
        let ts = CfgRules::for_language("typescript").unwrap();
        assert_eq!(ts.classify("labeled_statement"), Construct::Labeled);
        assert_eq!(ts.classify("throw_statement"), Construct::Throw);
        let go = CfgRules::for_language("go").unwrap();
        assert_eq!(go.classify("fallthrough_statement"), Construct::Fallthrough);
    }

    #[test]
    fn default_arm_needs_a_whole_keyword() {
        let c = CfgRules::for_language("c").unwrap();
        assert!(c.is_default_arm("default: b(); break;"));
        assert!(c.is_default_arm("  default : b();"));
        assert!(!c.is_default_arm("case defaultValue: b();"));
        let py = CfgRules::for_language("python").unwrap();
        assert!(py.is_default_arm("case _:\n    pass"));
        assert!(!py.is_default_arm("case _x if x:"));
        let rust = CfgRules::for_language("rust").unwrap();
        assert!(!rust.is_default_arm("_ => 0"), "Rust match is exhaustive");
    }
}
