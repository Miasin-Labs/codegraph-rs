//! Per-language rules for dataflow extraction.
//!
//! Each language defines which tree-sitter node kinds map to dataflow
//! constructs (parameters, assignments, calls, field accesses, mutations).

/// Language-specific rules for dataflow extraction.
pub struct DataflowRules {
    /// Node kinds representing function definitions.
    pub function_nodes: &'static [&'static str],
    /// Field name for the parameter list of a function.
    pub param_list_field: &'static str,
    /// Node kind for a single parameter identifier.
    pub param_identifier: &'static str,
    /// Field name for the function body.
    pub body_field: &'static str,
    /// Node kind for return statements/expressions.
    pub return_node: &'static str,
    /// Node kinds representing assignment or variable declaration.
    pub assignment_nodes: &'static [&'static str],
    /// Field name for the left-hand side of an assignment.
    pub assign_left_field: &'static str,
    /// Field name for the right-hand side of an assignment.
    pub assign_right_field: &'static str,
    /// Node kinds representing call expressions.
    pub call_nodes: &'static [&'static str],
    /// Field name for the function being called.
    pub call_function_field: &'static str,
    /// Field name for the arguments list of a call.
    pub call_args_field: &'static str,
    /// Node kind for member/field access expressions.
    pub member_node: &'static str,
    /// Field name for the object in a member expression.
    pub member_object_field: &'static str,
    /// Field name for the property/field in a member expression.
    pub member_property_field: &'static str,
    /// Method names that mutate their receiver.
    pub mutating_methods: &'static [&'static str],
    /// Node kind for plain identifiers.
    pub identifier_node: &'static str,
    /// Node kinds representing literal values.
    pub literal_nodes: &'static [&'static str],
    /// Node kind for method call expressions (language-specific).
    /// Empty string if the language uses the same call_nodes for methods.
    pub method_call_node: &'static str,
    /// Field name for the method call receiver object.
    pub method_call_object_field: &'static str,
    /// Field name for the method name in a method call.
    pub method_call_name_field: &'static str,
    /// Field name for the arguments list in a method call.
    pub method_call_args_field: &'static str,
}

impl DataflowRules {
    /// Look up rules for a language by its identifier.
    pub fn for_language(lang: &str) -> Option<&'static DataflowRules> {
        match lang {
            "rust" | "cairo" | "sway" => Some(&RUST_DATAFLOW_RULES),
            "typescript" | "javascript" | "arkts" => Some(&TYPESCRIPT_DATAFLOW_RULES),
            "python" | "vyper" => Some(&PYTHON_DATAFLOW_RULES),
            "go" => Some(&GO_DATAFLOW_RULES),
            "r" => Some(&R_DATAFLOW_RULES),
            "solidity" => Some(&SOLIDITY_DATAFLOW_RULES),
            "move" => Some(&MOVE_DATAFLOW_RULES),
            "fe" => Some(&FE_DATAFLOW_RULES),
            "nix" => Some(&NIX_DATAFLOW_RULES),
            "cfml" | "cfscript" | "cfquery" => Some(&CFSCRIPT_DATAFLOW_RULES),
            "erlang" => Some(&ERLANG_DATAFLOW_RULES),
            // VB.NET and COBOL bodies are unfielded/synthetic, while Terraform
            // has no function construct. `extract_dataflow` requires a body field.
            _ => None,
        }
    }
}

// ─── Rust ────────────────────────────────────────────────────────────────────

static RUST_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_item"],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_expression",
    assignment_nodes: &["let_declaration"],
    assign_left_field: "pattern",
    assign_right_field: "value",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "field_expression",
    member_object_field: "value",
    member_property_field: "field",
    mutating_methods: &[
        "push",
        "pop",
        "insert",
        "remove",
        "clear",
        "sort",
        "retain",
        "extend",
        "drain",
        "truncate",
        "sort_by",
        "sort_unstable",
        "append",
        "reserve",
    ],
    identifier_node: "identifier",
    literal_nodes: &[
        "integer_literal",
        "float_literal",
        "string_literal",
        "raw_string_literal",
        "char_literal",
        "boolean_literal",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── TypeScript ──────────────────────────────────────────────────────────────

static TYPESCRIPT_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &[
        "function_declaration",
        "method_definition",
        "arrow_function",
        "function",
    ],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["variable_declarator", "assignment_expression"],
    assign_left_field: "name",
    assign_right_field: "value",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "member_expression",
    member_object_field: "object",
    member_property_field: "property",
    mutating_methods: &[
        "push", "pop", "shift", "unshift", "splice", "sort", "reverse", "fill",
    ],
    identifier_node: "identifier",
    literal_nodes: &[
        "number",
        "string",
        "true",
        "false",
        "null",
        "undefined",
        "template_string",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── Python ──────────────────────────────────────────────────────────────────

static PYTHON_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_definition"],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["assignment", "augmented_assignment"],
    assign_left_field: "left",
    assign_right_field: "right",
    call_nodes: &["call"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "attribute",
    member_object_field: "object",
    member_property_field: "attribute",
    mutating_methods: &[
        "append", "extend", "insert", "remove", "pop", "clear", "sort", "reverse",
    ],
    identifier_node: "identifier",
    literal_nodes: &["integer", "float", "string", "true", "false", "none"],
    method_call_node: "call",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── Go ──────────────────────────────────────────────────────────────────────

static GO_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_declaration", "method_declaration"],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["short_var_declaration", "assignment_statement"],
    assign_left_field: "left",
    assign_right_field: "right",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "selector_expression",
    member_object_field: "operand",
    member_property_field: "field",
    mutating_methods: &["append", "delete", "Reset", "Write", "Close"],
    identifier_node: "identifier",
    literal_nodes: &[
        "int_literal",
        "float_literal",
        "rune_literal",
        "interpreted_string_literal",
        "raw_string_literal",
        "true",
        "false",
        "nil",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── R ───────────────────────────────────────────────────────────────────────

static R_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_definition"],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    // `return(...)` is an ordinary call in tree-sitter-r.
    return_node: "",
    // Assignments and ordinary binary expressions share `binary_operator`;
    // the table cannot filter its operator field without false assignments.
    assignment_nodes: &[],
    assign_left_field: "lhs",
    assign_right_field: "rhs",
    call_nodes: &["call"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "extract_operator",
    member_object_field: "lhs",
    member_property_field: "rhs",
    // R values are copy-on-modify; common collection functions return a value.
    mutating_methods: &[],
    identifier_node: "identifier",
    literal_nodes: &[
        "complex", "float", "integer", "string", "true", "false", "null", "na", "nan", "inf",
    ],
    method_call_node: "call",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── Solidity ────────────────────────────────────────────────────────────────

static SOLIDITY_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &[
        "function_definition",
        "modifier_definition",
        "constructor_definition",
        "fallback_receive_definition",
    ],
    // Parameters and call arguments are direct children in this grammar.
    param_list_field: "",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["assignment_expression", "augmented_assignment_expression"],
    assign_left_field: "left",
    assign_right_field: "right",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "",
    member_node: "member_expression",
    member_object_field: "object",
    member_property_field: "property",
    mutating_methods: &["push", "pop"],
    identifier_node: "identifier",
    literal_nodes: &[
        "boolean_literal",
        "number_literal",
        "string_literal",
        "hex_string_literal",
        "unicode_string_literal",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "",
};

// ─── Nix ─────────────────────────────────────────────────────────────────────

static NIX_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_expression"],
    // Attribute-set parameters use `formals`; simple parameters use the
    // sibling `universal` field and cannot share this single table field.
    param_list_field: "formals",
    param_identifier: "identifier",
    body_field: "body",
    // A Nix function's body expression is its implicit return value.
    return_node: "",
    assignment_nodes: &["binding"],
    assign_left_field: "attrpath",
    assign_right_field: "expression",
    call_nodes: &["apply_expression"],
    call_function_field: "function",
    call_args_field: "argument",
    member_node: "select_expression",
    member_object_field: "expression",
    member_property_field: "attrpath",
    mutating_methods: &[],
    identifier_node: "variable_expression",
    literal_nodes: &[
        "float_expression",
        "integer_expression",
        "string_expression",
        "indented_string_expression",
        "path_expression",
        "hpath_expression",
        "spath_expression",
        "uri_expression",
    ],
    method_call_node: "apply_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "argument",
};

// ─── CFML / CFScript / CFQuery ───────────────────────────────────────────────

static CFSCRIPT_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &[
        "function_declaration",
        "function_expression",
        "method_definition",
        "arrow_function",
    ],
    param_list_field: "parameters",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["assignment_expression", "augmented_assignment_expression"],
    assign_left_field: "left",
    assign_right_field: "right",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "member_expression",
    member_object_field: "object",
    member_property_field: "property",
    mutating_methods: &[
        "append", "clear", "delete", "insert", "pop", "push", "remove", "sort",
    ],
    identifier_node: "identifier",
    literal_nodes: &[
        "number",
        "string",
        "template_string",
        "true",
        "false",
        "null",
        "undefined",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "arguments",
};

// ─── Erlang ──────────────────────────────────────────────────────────────────

static ERLANG_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_clause", "fun_clause"],
    param_list_field: "args",
    param_identifier: "var",
    body_field: "body",
    // Erlang returns the final expression in a clause implicitly.
    return_node: "",
    assignment_nodes: &["match_expr"],
    assign_left_field: "lhs",
    assign_right_field: "rhs",
    call_nodes: &["call"],
    call_function_field: "expr",
    call_args_field: "args",
    member_node: "remote",
    member_object_field: "module",
    member_property_field: "fun",
    mutating_methods: &[],
    identifier_node: "var",
    literal_nodes: &["atom", "char", "float", "integer", "string"],
    method_call_node: "call",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "args",
};

// ─── Move ────────────────────────────────────────────────────────────────────

static MOVE_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_definition"],
    param_list_field: "parameters",
    param_identifier: "variable_identifier",
    body_field: "body",
    return_node: "return_expression",
    assignment_nodes: &["let_statement", "assign_expression"],
    assign_left_field: "binds",
    assign_right_field: "expr",
    call_nodes: &["call_expression"],
    call_function_field: "",
    call_args_field: "args",
    member_node: "dot_expression",
    member_object_field: "expr",
    member_property_field: "access",
    mutating_methods: &[],
    identifier_node: "name_expression",
    literal_nodes: &[
        "address_literal",
        "bool_literal",
        "byte_string_literal",
        "hex_string_literal",
        "num_literal",
    ],
    method_call_node: "call_expression",
    method_call_object_field: "",
    method_call_name_field: "",
    method_call_args_field: "args",
};

// ─── Fe ──────────────────────────────────────────────────────────────────────

static FE_DATAFLOW_RULES: DataflowRules = DataflowRules {
    function_nodes: &["function_definition", "contract_init", "recv_arm"],
    param_list_field: "",
    param_identifier: "identifier",
    body_field: "body",
    return_node: "return_statement",
    assignment_nodes: &["let_statement"],
    assign_left_field: "name",
    assign_right_field: "value",
    call_nodes: &["call_expression"],
    call_function_field: "function",
    call_args_field: "arguments",
    member_node: "field_expression",
    member_object_field: "value",
    member_property_field: "field",
    mutating_methods: &["push", "pop", "insert", "remove"],
    identifier_node: "identifier",
    literal_nodes: &[
        "boolean_literal",
        "integer_literal",
        "literal",
        "string_literal",
    ],
    method_call_node: "method_call_expression",
    method_call_object_field: "value",
    method_call_name_field: "method",
    method_call_args_field: "arguments",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_web3_languages() {
        assert!(std::ptr::eq(
            DataflowRules::for_language("vyper").unwrap(),
            &PYTHON_DATAFLOW_RULES
        ));
        assert!(std::ptr::eq(
            DataflowRules::for_language("cairo").unwrap(),
            &RUST_DATAFLOW_RULES
        ));
        assert!(std::ptr::eq(
            DataflowRules::for_language("sway").unwrap(),
            &RUST_DATAFLOW_RULES
        ));
        assert!(std::ptr::eq(
            DataflowRules::for_language("move").unwrap(),
            &MOVE_DATAFLOW_RULES
        ));
        assert!(std::ptr::eq(
            DataflowRules::for_language("fe").unwrap(),
            &FE_DATAFLOW_RULES
        ));
    }
}
