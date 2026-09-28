//! Per-language syntax the syntactic templates read (`arm-result-deviance`,
//! `result-discarded`): which tree-sitter node kinds are calls, matches,
//! arms, statements, constants. Looked up by language; a language without a
//! table is skipped by those templates (`missing-companion-call` needs no
//! syntax).

use crate::types::Language;

/// How an arm of a match yields its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::analyze::bugs) enum ArmValue {
    /// The arm's `value` field is an expression (Rust `pat => expr`).
    Expression,
    /// The arm holds statements; its value is what its last statement
    /// returns (a `switch` case, a Python `case`).
    Returned,
}

/// The node kinds and names one language's templates read.
pub(in crate::analyze::bugs) struct Rules {
    /// Function-like nodes whose `body` yields the function's result.
    pub functions: &'static [&'static str],
    /// A `match` must be the function's value (Rust), not just any `switch`
    /// whose cases return (TS/JS/Python).
    pub match_must_be_result: bool,
    pub calls: &'static [&'static str],
    /// Member accesses as `(kind, object field, member field)`.
    pub members: &'static [(&'static str, &'static str, &'static str)],
    /// Paths as `(kind, member field)` (`Type::f`).
    pub paths: &'static [(&'static str, &'static str)],
    /// Generic instantiations `(kind, inner field)` (`f::<T>`).
    pub generics: &'static [(&'static str, &'static str)],
    pub matches: &'static [&'static str],
    /// The scrutinee field of a match.
    pub match_subject: &'static str,
    pub arms: &'static [&'static str],
    /// Arms that are always the default (`default:`).
    pub default_arms: &'static [&'static str],
    /// The field holding an arm's pattern (empty: none).
    pub arm_pattern: &'static str,
    /// The field holding an arm's body (empty: the arm itself holds it).
    pub arm_body: &'static str,
    pub arm_value: ArmValue,
    /// A bare lowercase identifier pattern binds anything (a catch-all arm).
    pub bare_identifier_binds: bool,
    /// `if` expressions whose `consequence`/`alternative` carry the value on,
    /// and the `else` clauses wrapping an alternative.
    pub conditionals: &'static [&'static str],
    pub else_clauses: &'static [&'static str],
    pub blocks: &'static [&'static str],
    /// A block without a tail expression evaluates to unit (Rust).
    pub block_without_tail_is_unit: bool,
    pub returns: &'static [&'static str],
    /// Expression statements; with `statement_needs_semicolon`, only one
    /// ending in `;` discards its value (Rust's block-like tails).
    pub statements: &'static [&'static str],
    pub statement_needs_semicolon: bool,
    /// Nodes whose value is their operand's (`await`, `?`, parentheses).
    pub transparent: &'static [&'static str],
    /// Transparent nodes that also check the value (`f()?` returns early on
    /// failure): a check whose success value is then dropped is neither a
    /// use nor a discard (`resolve(x)?;` is an existence test).
    pub checks: &'static [&'static str],
    /// Bindings as `(kind, pattern field)`: `let _ = f()` discards.
    pub lets: &'static [(&'static str, &'static str)],
    /// Assignments as `(kind, left field)`: `_ = f()` discards.
    pub assignments: &'static [(&'static str, &'static str)],
    /// `(kind, operator)` unary forms that discard: `void f()`.
    pub discarding_unary: &'static [(&'static str, &'static str)],
    /// Pattern-binding conditions (`if let P = f()`) as `(kind, pattern field)`.
    pub let_conditions: &'static [(&'static str, &'static str)],
    pub comments: &'static [&'static str],
    /// Attribute nodes (siblings before the item they annotate), and the
    /// texts marking a test item (`#[test]`, `#[cfg(test)]`,
    /// `#[tokio::test]`): code the beliefs must not learn from.
    pub attributes: &'static [&'static str],
    pub test_markers: &'static [&'static str],
    /// Literal node kinds: a constant value.
    pub literals: &'static [&'static str],
    /// Identifiers and keywords that are constants (`None`, `null`).
    pub constant_words: &'static [&'static str],
    /// Calls with no arguments that build a constant (`Default::default()`).
    pub constant_calls: &'static [&'static str],
    /// Constants that carry nothing but "it worked" (`()`): an arm yielding
    /// one, bare or wrapped (`Ok(())`), reports all a sibling could — the
    /// failures went up through `?`.
    pub unit_values: &'static [&'static str],
    /// Macros that build a constant when empty (`vec![]`).
    pub constant_macros: &'static [&'static str],
    /// Empty collection literals (`[]`, `{}`) as node kinds.
    pub empty_collections: &'static [&'static str],
    /// Single-argument constructors whose value is their argument's
    /// (`Some(x)`, `Ok(x)`, `Box::new(x)`).
    pub value_wrappers: &'static [&'static str],
    /// Methods that pass the receiver's value on (any other method call on it
    /// uses it, `x.unwrap()` included — a panic on failure is a use): `f().ok()` as a statement
    /// still discards `f()`'s.
    pub forwarding_methods: &'static [&'static str],
    /// Forwarding methods that say the failure is ignored on purpose: `f().ok();`
    /// as a statement is an explicit discard, not an unchecked one.
    pub silencing_methods: &'static [&'static str],
    /// Methods that look only at success/failure, dropping the payload.
    pub payload_dropping_methods: &'static [&'static str],
    /// Variants carrying a success payload (`Ok`, `Some`) and the failure
    /// ones (`Err`, `None`): a match or `if let` on a call whose success
    /// patterns bind nothing discards the value.
    pub success_variants: &'static [&'static str],
    pub failure_variants: &'static [&'static str],
    /// Returned from a function without a declared return type, the result
    /// is unit (Rust) rather than unknown.
    pub undeclared_return_is_unit: bool,
    /// Declared return types that carry no value.
    pub unit_returns: &'static [&'static str],
    /// Result types that report success or failure, by their last path
    /// segment (`bool`) or its ending (`Result`: `io::Result`,
    /// `LockResult`): the only results a bare statement discard is reported
    /// for (`result-discarded`).
    pub status_types: &'static [&'static str],
    pub status_suffixes: &'static [&'static str],
    /// Wrappers read through for that (`Promise<boolean>`).
    pub status_transparent: &'static [&'static str],
}

static RUST: Rules = Rules {
    functions: &["function_item", "closure_expression"],
    match_must_be_result: true,
    calls: &["call_expression"],
    members: &[("field_expression", "value", "field")],
    paths: &[("scoped_identifier", "name")],
    generics: &[("generic_function", "function")],
    matches: &["match_expression"],
    match_subject: "value",
    arms: &["match_arm"],
    default_arms: &[],
    arm_pattern: "pattern",
    arm_body: "value",
    arm_value: ArmValue::Expression,
    bare_identifier_binds: true,
    conditionals: &["if_expression"],
    else_clauses: &["else_clause"],
    blocks: &["block", "unsafe_block"],
    block_without_tail_is_unit: true,
    returns: &["return_expression"],
    statements: &["expression_statement"],
    statement_needs_semicolon: true,
    transparent: &[
        "await_expression",
        "try_expression",
        "parenthesized_expression",
    ],
    checks: &["try_expression"],
    lets: &[("let_declaration", "pattern")],
    assignments: &[("assignment_expression", "left")],
    discarding_unary: &[],
    let_conditions: &[("let_condition", "pattern")],
    comments: &["line_comment", "block_comment"],
    attributes: &["attribute_item"],
    test_markers: &[
        "#[test]",
        "cfg(test)",
        "::test]",
        "::test(",
        "#[rstest",
        "#[bench]",
    ],
    literals: &[
        "integer_literal",
        "float_literal",
        "string_literal",
        "raw_string_literal",
        "char_literal",
        "boolean_literal",
        "unit_expression",
    ],
    constant_words: &["None"],
    constant_calls: &[
        "Default::default",
        "Vec::new",
        "String::new",
        "HashMap::new",
        "HashSet::new",
        "BTreeMap::new",
        "BTreeSet::new",
        "VecDeque::new",
    ],
    unit_values: &["()"],
    constant_macros: &["vec"],
    empty_collections: &[],
    value_wrappers: &["Some", "Ok", "Box::new", "Rc::new", "Arc::new"],
    forwarding_methods: &["ok", "map_err", "context", "with_context", "into"],
    silencing_methods: &["ok"],
    payload_dropping_methods: &["is_ok", "is_err", "err"],
    success_variants: &["Ok", "Some"],
    failure_variants: &["Err", "None"],
    undeclared_return_is_unit: true,
    unit_returns: &["()", "!", "&mut Self", "&Self"],
    status_types: &["bool"],
    status_suffixes: &["Result"],
    status_transparent: &[],
};

static TYPESCRIPT: Rules = Rules {
    functions: &[
        "function_declaration",
        "function_expression",
        "arrow_function",
        "method_definition",
        "generator_function_declaration",
    ],
    match_must_be_result: false,
    calls: &["call_expression"],
    members: &[("member_expression", "object", "property")],
    paths: &[],
    generics: &[],
    matches: &["switch_statement"],
    match_subject: "value",
    arms: &["switch_case", "switch_default"],
    default_arms: &["switch_default"],
    arm_pattern: "",
    arm_body: "",
    arm_value: ArmValue::Returned,
    bare_identifier_binds: false,
    conditionals: &[],
    else_clauses: &[],
    blocks: &["statement_block"],
    block_without_tail_is_unit: false,
    returns: &["return_statement"],
    statements: &["expression_statement"],
    statement_needs_semicolon: false,
    transparent: &[
        "await_expression",
        "parenthesized_expression",
        "non_null_expression",
        "as_expression",
        "satisfies_expression",
    ],
    checks: &[],
    lets: &[("variable_declarator", "name")],
    assignments: &[("assignment_expression", "left")],
    discarding_unary: &[("unary_expression", "void")],
    let_conditions: &[],
    comments: &["comment"],
    attributes: &[],
    test_markers: &[],
    literals: &["number", "string", "true", "false", "null", "undefined"],
    constant_words: &["undefined"],
    constant_calls: &[],
    unit_values: &[],
    constant_macros: &[],
    empty_collections: &["array", "object"],
    value_wrappers: &[],
    forwarding_methods: &["catch", "finally"],
    silencing_methods: &[],
    payload_dropping_methods: &[],
    success_variants: &[],
    failure_variants: &[],
    undeclared_return_is_unit: false,
    unit_returns: &[
        "void",
        "Promise<void>",
        "undefined",
        "never",
        "Promise<undefined>",
        "this",
    ],
    status_types: &["boolean"],
    status_suffixes: &["Result"],
    status_transparent: &["Promise"],
};

static PYTHON: Rules = Rules {
    functions: &["function_definition", "lambda"],
    match_must_be_result: false,
    calls: &["call"],
    members: &[("attribute", "object", "attribute")],
    paths: &[],
    generics: &[],
    matches: &["match_statement"],
    match_subject: "subject",
    arms: &["case_clause"],
    default_arms: &[],
    arm_pattern: "",
    arm_body: "consequence",
    arm_value: ArmValue::Returned,
    bare_identifier_binds: true,
    conditionals: &[],
    else_clauses: &[],
    blocks: &["block"],
    block_without_tail_is_unit: false,
    returns: &["return_statement"],
    statements: &["expression_statement"],
    statement_needs_semicolon: false,
    transparent: &["await", "parenthesized_expression"],
    checks: &[],
    lets: &[],
    assignments: &[("assignment", "left")],
    discarding_unary: &[],
    let_conditions: &[],
    comments: &["comment"],
    attributes: &[],
    test_markers: &[],
    literals: &["integer", "float", "string", "true", "false", "none"],
    constant_words: &["None"],
    constant_calls: &["dict", "list", "set", "tuple"],
    unit_values: &[],
    constant_macros: &[],
    empty_collections: &["list", "dictionary", "tuple", "set"],
    value_wrappers: &[],
    forwarding_methods: &[],
    silencing_methods: &[],
    payload_dropping_methods: &[],
    success_variants: &[],
    failure_variants: &[],
    undeclared_return_is_unit: false,
    unit_returns: &["None", "NoReturn", "Never"],
    status_types: &["bool"],
    status_suffixes: &["Result"],
    status_transparent: &[],
};

/// The rules for `language`, if the syntactic templates support it.
pub(in crate::analyze::bugs) fn for_language(language: Language) -> Option<&'static Rules> {
    match language {
        Language::Rust => Some(&RUST),
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => {
            Some(&TYPESCRIPT)
        }
        Language::Python => Some(&PYTHON),
        _ => None,
    }
}
