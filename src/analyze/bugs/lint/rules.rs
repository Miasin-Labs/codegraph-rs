//! Per-language node-kind tables for the lint rules.
//!
//! The rules in this module's siblings never branch on a language: they ask
//! the table what a node *is* (a condition-controlled loop, a store, a
//! comparison, a function scope). Add a language by adding a table here.

use crate::types::Language;

/// How to find one part of a node.
#[derive(Debug, Clone, Copy)]
pub(super) enum Pick {
    /// The child under this field name.
    Field(&'static str),
    /// The n-th named child, comments skipped.
    Nth(usize),
    /// The first named child whose kind is not one of these (Go's `for cond
    /// {}` keeps its condition in an unnamed field).
    FirstExcept(&'static [&'static str]),
}

/// A loop that runs while a condition holds.
#[derive(Debug, Clone, Copy)]
pub(super) struct CondLoop {
    pub kind: &'static str,
    pub condition: Pick,
    pub body: &'static str,
}

/// How an `if` reaches its `else` body.
#[derive(Debug, Clone, Copy)]
pub(super) enum Alt {
    /// The `alternative` field is the else body (or a nested `if`).
    Direct,
    /// The `alternative` field wraps it in an else clause (`else_clause`).
    Wrapped,
    /// Python: `alternative` repeats — `elif_clause`s then an `else_clause`.
    Clauses {
        elif: &'static str,
        else_clause: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct IfShape {
    pub kind: &'static str,
    pub condition: &'static str,
    pub consequence: &'static str,
    pub alternative: Alt,
}

/// `c ? a : b` and its spellings.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ternary {
    pub kind: &'static str,
    pub condition: Pick,
    pub consequence: Pick,
    pub alternative: Pick,
}

/// Where an arm keeps its body.
#[derive(Debug, Clone, Copy)]
pub(super) enum ArmBody {
    /// Every child under this field (JS `switch_case` repeats `body`).
    Field(&'static str),
    /// Every named child except those of this kind (Java labels).
    Except(&'static str),
    /// The child of this kind (Go `statement_list`).
    Child(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Arm {
    pub kind: &'static str,
    pub body: ArmBody,
}

/// A `match`/`switch`: its arms are the named children of `arms_in` (or of
/// the node itself) whose kind is an arm kind.
#[derive(Debug, Clone, Copy)]
pub(super) struct Switch {
    pub kind: &'static str,
    pub arms_in: Option<&'static str>,
}

/// A binary operation.
#[derive(Debug, Clone, Copy)]
pub(super) struct Binary {
    pub kind: &'static str,
    pub left: Pick,
    pub operator: &'static str,
    pub right: Pick,
}

/// A statement that stores into plain variables.
#[derive(Debug, Clone, Copy)]
pub(super) enum StoreShape {
    /// `wrapper` holding one `kind` node whose `left` is the target and
    /// `right` the value (`x = a;`). `wrapper` empty: the node itself.
    /// `operator` non-empty: the operator field must be `=` (Go, Java).
    Assign {
        wrapper: &'static str,
        kind: &'static str,
        left: &'static str,
        right: &'static str,
        operator: bool,
    },
    /// A declaration with one declarator (`let x = a;`, `var x = a`).
    /// `declarator` empty: the node itself carries `name`/`value`.
    /// `shadows`: declaring again makes a new variable (Rust `let`).
    Declare {
        kind: &'static str,
        declarator: &'static str,
        name: &'static str,
        value: &'static str,
        shadows: bool,
    },
}

/// A node that writes the variables in one of its parts.
#[derive(Debug, Clone, Copy)]
pub(super) struct Mutation {
    pub kind: &'static str,
    /// Field holding the target; empty = every child.
    pub target: &'static str,
    /// Only when a child of this kind is present (`&mut x`, Go `&x`).
    pub marker: Option<&'static str>,
}

/// A method call comparing its receiver with its argument
/// (`a.equals(b)`): `self-comparison` when both are the same.
#[derive(Debug, Clone, Copy)]
pub(super) struct EqMethod {
    pub kind: &'static str,
    pub object: &'static str,
    pub name: &'static str,
    pub arguments: &'static str,
    pub names: &'static [&'static str],
}

/// A named bound of an integer type (`Integer.MIN_VALUE`, `INT_MAX`) and
/// the declared types whose every value lies within it: `x < INT_MIN` for
/// an `int x` never holds.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limit {
    pub name: &'static str,
    pub is_max: bool,
    pub types: &'static [&'static str],
}

/// A declaration naming a variable's type: `(kind, type field, declarator
/// field)`. The declarator is the name itself or a wrapper holding it under
/// `name`/`declarator` (`variable_declarator`, `init_declarator`).
pub(super) type TypedDeclaration = (&'static str, &'static str, &'static str);

const fn m(kind: &'static str, target: &'static str) -> Mutation {
    Mutation {
        kind,
        target,
        marker: None,
    }
}

pub(super) struct Rules {
    /// Variables: identifier leaves (and `self`/`this`).
    pub idents: &'static [&'static str],
    /// Receiver leaves (`self`, `this`).
    pub receivers: &'static [&'static str],
    /// Field accesses: (kind, object field). The member name is not a read
    /// of a variable.
    pub fields: &'static [(&'static str, &'static str)],
    /// Literal kinds (pure, no variable).
    pub literals: &'static [&'static str],
    /// Boolean literal spellings.
    pub bool_true: &'static [&'static str],
    pub bool_false: &'static [&'static str],
    /// Paths that name constants (`Self::MAX`, `Ordering::Less`).
    pub constant_paths: &'static [&'static str],
    /// Pure when every named child is: parens, unary, binary, casts,
    /// subscripts.
    pub transparent: &'static [&'static str],
    /// Operator tokens that make a transparent node impure (`<-`, `delete`).
    pub impure_ops: &'static [&'static str],
    /// Parentheses, stripped before looking at a condition's shape.
    pub parens: &'static [&'static str],
    pub comments: &'static [&'static str],

    pub cond_loops: &'static [CondLoop],
    /// Every loop kind (a dead store never spans one).
    pub loops: &'static [&'static str],
    pub ifs: &'static [IfShape],
    /// Other `condition` holders checked for constant conditions (`elif`).
    pub conditions: &'static [(&'static str, &'static str)],
    pub ternaries: &'static [Ternary],
    pub switches: &'static [Switch],
    pub arms: &'static [Arm],
    /// Switches whose arms bind a differently typed value (Go type switch).
    pub typed_switches: &'static [&'static str],
    pub binaries: &'static [Binary],

    /// Function scopes, closures included.
    pub functions: &'static [&'static str],
    /// Fields of a function node that hold its parameters.
    pub param_fields: &'static [&'static str],
    /// Local declarations: (kind, field naming the declared variables).
    pub declarations: &'static [(&'static str, &'static str)],
    /// `global x` / `nonlocal x`: not locals.
    pub non_locals: &'static [&'static str],
    /// Rust: every plain identifier is a local, so no declaration is needed.
    pub idents_are_locals: bool,
    pub mutations: &'static [Mutation],
    /// Leave the enclosing loop or function: return, break, throw, `?`,
    /// yield, await, goto.
    pub exits: &'static [&'static str],
    /// `continue` (a labelled one leaves the loop).
    pub continues: &'static [&'static str],
    /// Labelled statements / jumps: a dead store never spans one.
    pub jumps: &'static [&'static str],
    pub calls: &'static [&'static str],
    pub macro_kind: Option<&'static str>,
    /// Anything goes (`unsafe {}`): loops holding one are skipped.
    pub opaque: &'static [&'static str],
    /// Attributes between statements (`#[cfg]`): stores never span one.
    pub attributes: &'static [&'static str],
    /// Statement lists.
    pub blocks: &'static [&'static str],
    pub stores: &'static [StoreShape],
    /// Exceptions: a block inside one of these may leave mid-way into a
    /// handler that reads the variable.
    pub tries: &'static [&'static str],

    /// Statements that evaluate an expression and drop its value: a
    /// comparison there (`a == b;`) is `comparison-discarded`. Empty where
    /// the language rejects it (Java, Go).
    pub expr_statements: &'static [&'static str],
    /// Assignments that may stand as an `if`/`while` condition (`if (x =
    /// 5)`): `assignment-in-condition`. Empty where the language rejects it.
    pub cond_assignments: &'static [&'static str],
    /// An assignment in doubled parentheses (`if ((p = next()))`) is the
    /// language's "I mean it" idiom (C compilers, ESLint's except-parens).
    pub double_parens_mean_it: bool,
    pub eq_methods: &'static [EqMethod],
    pub limits: &'static [Limit],
    pub typed_declarations: &'static [TypedDeclaration],
}

static RUST: Rules = Rules {
    idents: &["identifier", "self"],
    receivers: &["self"],
    fields: &[("field_expression", "value")],
    literals: &[
        "integer_literal",
        "float_literal",
        "boolean_literal",
        "string_literal",
        "raw_string_literal",
        "char_literal",
        "unit_expression",
    ],
    bool_true: &["true"],
    bool_false: &["false"],
    constant_paths: &["scoped_identifier"],
    transparent: &[
        "parenthesized_expression",
        "unary_expression",
        "binary_expression",
        "type_cast_expression",
        "reference_expression",
        "index_expression",
        "tuple_expression",
        "array_expression",
    ],
    impure_ops: &[],
    parens: &["parenthesized_expression"],
    comments: &["line_comment", "block_comment"],
    cond_loops: &[CondLoop {
        kind: "while_expression",
        condition: Pick::Field("condition"),
        body: "body",
    }],
    loops: &["while_expression", "loop_expression", "for_expression"],
    ifs: &[IfShape {
        kind: "if_expression",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Wrapped,
    }],
    conditions: &[],
    ternaries: &[],
    switches: &[Switch {
        kind: "match_expression",
        arms_in: Some("body"),
    }],
    arms: &[Arm {
        kind: "match_arm",
        body: ArmBody::Field("value"),
    }],
    typed_switches: &[],
    binaries: &[Binary {
        kind: "binary_expression",
        left: Pick::Field("left"),
        operator: "operator",
        right: Pick::Field("right"),
    }],
    functions: &[
        "function_item",
        "closure_expression",
        "async_block",
        "const_block",
    ],
    param_fields: &["parameters"],
    declarations: &[],
    non_locals: &[],
    idents_are_locals: true,
    mutations: &[
        m("assignment_expression", "left"),
        m("compound_assignment_expr", "left"),
        m("let_declaration", "pattern"),
        m("let_condition", "pattern"),
        m("for_expression", "pattern"),
        Mutation {
            kind: "reference_expression",
            target: "",
            marker: Some("mutable_specifier"),
        },
    ],
    exits: &[
        "return_expression",
        "break_expression",
        "try_expression",
        "await_expression",
        "yield_expression",
    ],
    continues: &["continue_expression"],
    jumps: &["label", "break_expression", "continue_expression"],
    calls: &["call_expression"],
    macro_kind: Some("macro_invocation"),
    opaque: &["unsafe_block"],
    attributes: &["attribute_item", "inner_attribute_item"],
    blocks: &["block"],
    stores: &[
        StoreShape::Assign {
            wrapper: "expression_statement",
            kind: "assignment_expression",
            left: "left",
            right: "right",
            operator: false,
        },
        StoreShape::Assign {
            wrapper: "",
            kind: "assignment_expression",
            left: "left",
            right: "right",
            operator: false,
        },
        StoreShape::Declare {
            kind: "let_declaration",
            declarator: "",
            name: "pattern",
            value: "value",
            shadows: true,
        },
    ],
    tries: &[],
    expr_statements: &["expression_statement"],
    cond_assignments: &[],
    double_parens_mean_it: false,
    eq_methods: &[],
    limits: &[],
    typed_declarations: &[],
};

static JS_FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "generator_function_declaration",
    "generator_function",
    "arrow_function",
    "method_definition",
];

static JS_TS: Rules = Rules {
    idents: &["identifier", "this"],
    receivers: &["this"],
    fields: &[("member_expression", "object")],
    literals: &[
        "number",
        "string",
        "true",
        "false",
        "null",
        "undefined",
        "regex",
    ],
    bool_true: &["true"],
    bool_false: &["false"],
    constant_paths: &[],
    transparent: &[
        "parenthesized_expression",
        "unary_expression",
        "binary_expression",
        "subscript_expression",
        "non_null_expression",
        "as_expression",
        "satisfies_expression",
    ],
    impure_ops: &["delete", "void"],
    parens: &["parenthesized_expression"],
    comments: &["comment", "html_comment"],
    cond_loops: &[
        CondLoop {
            kind: "while_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
        CondLoop {
            kind: "do_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
    ],
    loops: &[
        "while_statement",
        "do_statement",
        "for_statement",
        "for_in_statement",
    ],
    ifs: &[IfShape {
        kind: "if_statement",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Wrapped,
    }],
    conditions: &[],
    ternaries: &[Ternary {
        kind: "ternary_expression",
        condition: Pick::Field("condition"),
        consequence: Pick::Field("consequence"),
        alternative: Pick::Field("alternative"),
    }],
    switches: &[Switch {
        kind: "switch_statement",
        arms_in: Some("body"),
    }],
    arms: &[
        Arm {
            kind: "switch_case",
            body: ArmBody::Field("body"),
        },
        Arm {
            kind: "switch_default",
            body: ArmBody::Field("body"),
        },
    ],
    typed_switches: &[],
    binaries: &[Binary {
        kind: "binary_expression",
        left: Pick::Field("left"),
        operator: "operator",
        right: Pick::Field("right"),
    }],
    functions: JS_FUNCTIONS,
    param_fields: &["parameters", "parameter"],
    declarations: &[
        ("variable_declarator", "name"),
        ("catch_clause", "parameter"),
        ("for_in_statement", "left"),
    ],
    non_locals: &[],
    idents_are_locals: false,
    mutations: &[
        m("assignment_expression", "left"),
        m("augmented_assignment_expression", "left"),
        m("update_expression", "argument"),
        m("variable_declarator", "name"),
        m("for_in_statement", "left"),
    ],
    exits: &[
        "return_statement",
        "break_statement",
        "throw_statement",
        "yield_expression",
        "await_expression",
    ],
    continues: &["continue_statement"],
    jumps: &["labeled_statement", "break_statement", "continue_statement"],
    calls: &["call_expression", "new_expression"],
    macro_kind: None,
    opaque: &["with_statement"],
    attributes: &[],
    blocks: &["statement_block"],
    stores: &[
        StoreShape::Assign {
            wrapper: "expression_statement",
            kind: "assignment_expression",
            left: "left",
            right: "right",
            operator: false,
        },
        StoreShape::Declare {
            kind: "lexical_declaration",
            declarator: "variable_declarator",
            name: "name",
            value: "value",
            shadows: false,
        },
        StoreShape::Declare {
            kind: "variable_declaration",
            declarator: "variable_declarator",
            name: "name",
            value: "value",
            shadows: false,
        },
    ],
    tries: &["try_statement"],
    expr_statements: &["expression_statement"],
    cond_assignments: &["assignment_expression"],
    double_parens_mean_it: true,
    eq_methods: &[],
    limits: &[],
    typed_declarations: &[],
};

static PYTHON: Rules = Rules {
    idents: &["identifier"],
    receivers: &[],
    fields: &[("attribute", "object")],
    literals: &[
        "integer",
        "float",
        "string",
        "concatenated_string",
        "true",
        "false",
        "none",
    ],
    bool_true: &["True"],
    bool_false: &["False"],
    constant_paths: &[],
    transparent: &[
        "parenthesized_expression",
        "not_operator",
        "boolean_operator",
        "comparison_operator",
        "binary_operator",
        "unary_operator",
        "subscript",
        "tuple",
    ],
    impure_ops: &[],
    parens: &["parenthesized_expression"],
    comments: &["comment"],
    cond_loops: &[CondLoop {
        kind: "while_statement",
        condition: Pick::Field("condition"),
        body: "body",
    }],
    loops: &["while_statement", "for_statement"],
    ifs: &[IfShape {
        kind: "if_statement",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Clauses {
            elif: "elif_clause",
            else_clause: "else_clause",
        },
    }],
    conditions: &[("elif_clause", "condition")],
    ternaries: &[Ternary {
        kind: "conditional_expression",
        condition: Pick::Nth(1),
        consequence: Pick::Nth(0),
        alternative: Pick::Nth(2),
    }],
    switches: &[Switch {
        kind: "match_statement",
        arms_in: Some("body"),
    }],
    arms: &[Arm {
        kind: "case_clause",
        body: ArmBody::Field("consequence"),
    }],
    typed_switches: &[],
    binaries: &[
        Binary {
            kind: "boolean_operator",
            left: Pick::Field("left"),
            operator: "operator",
            right: Pick::Field("right"),
        },
        Binary {
            kind: "binary_operator",
            left: Pick::Field("left"),
            operator: "operator",
            right: Pick::Field("right"),
        },
        Binary {
            kind: "comparison_operator",
            left: Pick::Nth(0),
            operator: "operators",
            right: Pick::Nth(1),
        },
    ],
    functions: &["function_definition", "lambda"],
    param_fields: &["parameters"],
    declarations: &[
        ("assignment", "left"),
        ("augmented_assignment", "left"),
        ("for_statement", "left"),
        ("named_expression", "name"),
        ("as_pattern", "alias"),
    ],
    non_locals: &["global_statement", "nonlocal_statement"],
    idents_are_locals: false,
    mutations: &[
        m("assignment", "left"),
        m("augmented_assignment", "left"),
        m("for_statement", "left"),
        m("named_expression", "name"),
        m("delete_statement", ""),
        m("as_pattern", "alias"),
        m("global_statement", ""),
        m("nonlocal_statement", ""),
    ],
    exits: &[
        "return_statement",
        "break_statement",
        "raise_statement",
        "yield",
        "await",
    ],
    continues: &["continue_statement"],
    jumps: &["break_statement", "continue_statement"],
    calls: &["call"],
    macro_kind: None,
    opaque: &["exec_statement"],
    attributes: &[],
    blocks: &["block"],
    stores: &[StoreShape::Assign {
        wrapper: "expression_statement",
        kind: "assignment",
        left: "left",
        right: "right",
        operator: false,
    }],
    tries: &["try_statement", "with_statement"],
    expr_statements: &["expression_statement"],
    // `:=` is the walrus, deliberate by construction.
    cond_assignments: &[],
    double_parens_mean_it: false,
    eq_methods: &[],
    limits: &[],
    typed_declarations: &[],
};

static GO: Rules = Rules {
    idents: &["identifier"],
    receivers: &[],
    fields: &[("selector_expression", "operand")],
    literals: &[
        "int_literal",
        "float_literal",
        "imaginary_literal",
        "rune_literal",
        "interpreted_string_literal",
        "raw_string_literal",
        "true",
        "false",
        "nil",
        "iota",
    ],
    bool_true: &["true"],
    bool_false: &["false"],
    constant_paths: &[],
    transparent: &[
        "parenthesized_expression",
        "unary_expression",
        "binary_expression",
        "index_expression",
    ],
    impure_ops: &["<-"],
    parens: &["parenthesized_expression"],
    comments: &["comment"],
    cond_loops: &[CondLoop {
        kind: "for_statement",
        condition: Pick::FirstExcept(&["block", "for_clause", "range_clause", "comment"]),
        body: "body",
    }],
    loops: &["for_statement"],
    ifs: &[IfShape {
        kind: "if_statement",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Direct,
    }],
    conditions: &[],
    ternaries: &[],
    switches: &[
        Switch {
            kind: "expression_switch_statement",
            arms_in: None,
        },
        Switch {
            kind: "type_switch_statement",
            arms_in: None,
        },
    ],
    arms: &[
        Arm {
            kind: "expression_case",
            body: ArmBody::Child("statement_list"),
        },
        Arm {
            kind: "default_case",
            body: ArmBody::Child("statement_list"),
        },
        Arm {
            kind: "type_case",
            body: ArmBody::Child("statement_list"),
        },
    ],
    typed_switches: &["type_switch_statement"],
    binaries: &[Binary {
        kind: "binary_expression",
        left: Pick::Field("left"),
        operator: "operator",
        right: Pick::Field("right"),
    }],
    functions: &["function_declaration", "method_declaration", "func_literal"],
    param_fields: &["parameters", "result", "receiver"],
    declarations: &[
        ("short_var_declaration", "left"),
        ("var_spec", "name"),
        ("const_spec", "name"),
        ("range_clause", "left"),
        ("receive_statement", "left"),
    ],
    non_locals: &[],
    idents_are_locals: false,
    mutations: &[
        m("assignment_statement", "left"),
        m("short_var_declaration", "left"),
        m("inc_statement", ""),
        m("dec_statement", ""),
        m("var_spec", "name"),
        m("range_clause", "left"),
        m("receive_statement", "left"),
        Mutation {
            kind: "unary_expression",
            target: "",
            marker: Some("&"),
        },
    ],
    exits: &["return_statement", "break_statement", "goto_statement"],
    continues: &["continue_statement"],
    jumps: &[
        "labeled_statement",
        "goto_statement",
        "break_statement",
        "continue_statement",
        "fallthrough_statement",
    ],
    calls: &["call_expression"],
    macro_kind: None,
    opaque: &[],
    attributes: &[],
    blocks: &["statement_list"],
    stores: &[
        StoreShape::Assign {
            wrapper: "",
            kind: "assignment_statement",
            left: "left",
            right: "right",
            operator: true,
        },
        StoreShape::Assign {
            wrapper: "",
            kind: "short_var_declaration",
            left: "left",
            right: "right",
            operator: false,
        },
    ],
    tries: &[],
    // Go rejects both: an unused comparison and an assignment condition.
    expr_statements: &[],
    cond_assignments: &[],
    double_parens_mean_it: false,
    eq_methods: &[],
    limits: &[],
    typed_declarations: &[],
};

static JAVA: Rules = Rules {
    idents: &["identifier", "this"],
    receivers: &["this"],
    fields: &[("field_access", "object")],
    literals: &[
        "decimal_integer_literal",
        "hex_integer_literal",
        "octal_integer_literal",
        "binary_integer_literal",
        "decimal_floating_point_literal",
        "hex_floating_point_literal",
        "true",
        "false",
        "null_literal",
        "string_literal",
        "character_literal",
    ],
    bool_true: &["true"],
    bool_false: &["false"],
    constant_paths: &[],
    transparent: &[
        "parenthesized_expression",
        "unary_expression",
        "binary_expression",
        "array_access",
        "cast_expression",
        "instanceof_expression",
    ],
    impure_ops: &[],
    parens: &["parenthesized_expression"],
    comments: &["line_comment", "block_comment"],
    cond_loops: &[
        CondLoop {
            kind: "while_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
        CondLoop {
            kind: "do_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
    ],
    loops: &[
        "while_statement",
        "do_statement",
        "for_statement",
        "enhanced_for_statement",
    ],
    ifs: &[IfShape {
        kind: "if_statement",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Direct,
    }],
    conditions: &[],
    ternaries: &[Ternary {
        kind: "ternary_expression",
        condition: Pick::Field("condition"),
        consequence: Pick::Field("consequence"),
        alternative: Pick::Field("alternative"),
    }],
    switches: &[
        Switch {
            kind: "switch_expression",
            arms_in: Some("body"),
        },
        Switch {
            kind: "switch_statement",
            arms_in: Some("body"),
        },
    ],
    arms: &[
        Arm {
            kind: "switch_block_statement_group",
            body: ArmBody::Except("switch_label"),
        },
        Arm {
            kind: "switch_rule",
            body: ArmBody::Except("switch_label"),
        },
    ],
    typed_switches: &[],
    binaries: &[Binary {
        kind: "binary_expression",
        left: Pick::Field("left"),
        operator: "operator",
        right: Pick::Field("right"),
    }],
    functions: &[
        "method_declaration",
        "constructor_declaration",
        "compact_constructor_declaration",
        "lambda_expression",
    ],
    param_fields: &["parameters"],
    declarations: &[
        ("variable_declarator", "name"),
        ("enhanced_for_statement", "name"),
        ("catch_formal_parameter", "name"),
        ("resource", "name"),
    ],
    non_locals: &[],
    idents_are_locals: false,
    mutations: &[
        m("assignment_expression", "left"),
        m("update_expression", ""),
        m("variable_declarator", "name"),
        m("enhanced_for_statement", "name"),
    ],
    exits: &[
        "return_statement",
        "break_statement",
        "throw_statement",
        "yield_statement",
    ],
    continues: &["continue_statement"],
    jumps: &["labeled_statement", "break_statement", "continue_statement"],
    calls: &["method_invocation", "object_creation_expression"],
    macro_kind: None,
    opaque: &["synchronized_statement"],
    attributes: &[],
    blocks: &["block"],
    stores: &[
        StoreShape::Assign {
            wrapper: "expression_statement",
            kind: "assignment_expression",
            left: "left",
            right: "right",
            operator: true,
        },
        StoreShape::Declare {
            kind: "local_variable_declaration",
            declarator: "variable_declarator",
            name: "name",
            value: "value",
            shadows: false,
        },
    ],
    tries: &["try_statement", "try_with_resources_statement"],
    // `a == b;` is not a Java statement.
    expr_statements: &[],
    cond_assignments: &["assignment_expression"],
    // Java conditions are `boolean`: `if ((b = c))` is as wrong as `if (b =
    // c)`.
    double_parens_mean_it: false,
    eq_methods: &[EqMethod {
        kind: "method_invocation",
        object: "object",
        name: "name",
        arguments: "arguments",
        names: &[
            "equals",
            "equalsIgnoreCase",
            "compareTo",
            "compareToIgnoreCase",
            "contentEquals",
        ],
    }],
    limits: &[
        Limit {
            name: "Integer.MIN_VALUE",
            is_max: false,
            types: &["int", "short", "byte", "char"],
        },
        Limit {
            name: "Integer.MAX_VALUE",
            is_max: true,
            types: &["int", "short", "byte", "char"],
        },
        Limit {
            name: "Long.MIN_VALUE",
            is_max: false,
            types: &["long", "int", "short", "byte", "char"],
        },
        Limit {
            name: "Long.MAX_VALUE",
            is_max: true,
            types: &["long", "int", "short", "byte", "char"],
        },
        Limit {
            name: "Short.MIN_VALUE",
            is_max: false,
            types: &["short", "byte"],
        },
        Limit {
            name: "Short.MAX_VALUE",
            is_max: true,
            types: &["short", "byte"],
        },
        Limit {
            name: "Byte.MIN_VALUE",
            is_max: false,
            types: &["byte"],
        },
        Limit {
            name: "Byte.MAX_VALUE",
            is_max: true,
            types: &["byte"],
        },
    ],
    typed_declarations: &[
        ("local_variable_declaration", "type", "declarator"),
        ("formal_parameter", "type", "name"),
    ],
};

const C_INT: &[&str] = &[
    "int",
    "signed",
    "signed int",
    "short",
    "short int",
    "signed char",
];
const C_LONG: &[&str] = &[
    "long",
    "long int",
    "int",
    "signed",
    "signed int",
    "short",
    "short int",
    "signed char",
];
const C_LLONG: &[&str] = &[
    "long long",
    "long long int",
    "long",
    "long int",
    "int",
    "signed",
    "signed int",
    "short",
    "short int",
    "signed char",
];
const C_SHORT: &[&str] = &["short", "short int", "signed char"];

const C_LIMITS: &[Limit] = &[
    Limit {
        name: "INT_MIN",
        is_max: false,
        types: C_INT,
    },
    Limit {
        name: "INT_MAX",
        is_max: true,
        types: C_INT,
    },
    Limit {
        name: "LONG_MIN",
        is_max: false,
        types: C_LONG,
    },
    Limit {
        name: "LONG_MAX",
        is_max: true,
        types: C_LONG,
    },
    Limit {
        name: "LLONG_MIN",
        is_max: false,
        types: C_LLONG,
    },
    Limit {
        name: "LLONG_MAX",
        is_max: true,
        types: C_LLONG,
    },
    Limit {
        name: "SHRT_MIN",
        is_max: false,
        types: C_SHORT,
    },
    Limit {
        name: "SHRT_MAX",
        is_max: true,
        types: C_SHORT,
    },
];

/// C, and C++ below: the same statements; C++ wraps `if`/`while`
/// conditions in a `condition_clause`, adds lambdas, `throw`, `try` and
/// range-`for`. `switch` arms are left out (a `case` label is an arbitrary
/// expression and bodies fall through).
const C_RULES: Rules = Rules {
    idents: &["identifier"],
    receivers: &[],
    fields: &[("field_expression", "argument")],
    literals: &[
        "number_literal",
        "string_literal",
        "concatenated_string",
        "char_literal",
        "true",
        "false",
        "null",
    ],
    bool_true: &["true"],
    bool_false: &["false"],
    constant_paths: &[],
    transparent: &[
        "parenthesized_expression",
        "unary_expression",
        "binary_expression",
        "subscript_expression",
        "cast_expression",
        "pointer_expression",
    ],
    impure_ops: &[],
    parens: &["parenthesized_expression"],
    comments: &["comment"],
    cond_loops: &[
        CondLoop {
            kind: "while_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
        CondLoop {
            kind: "do_statement",
            condition: Pick::Field("condition"),
            body: "body",
        },
    ],
    loops: &["while_statement", "do_statement", "for_statement"],
    ifs: &[IfShape {
        kind: "if_statement",
        condition: "condition",
        consequence: "consequence",
        alternative: Alt::Wrapped,
    }],
    conditions: &[],
    ternaries: &[Ternary {
        kind: "conditional_expression",
        condition: Pick::Field("condition"),
        consequence: Pick::Field("consequence"),
        alternative: Pick::Field("alternative"),
    }],
    switches: &[],
    arms: &[],
    typed_switches: &[],
    binaries: &[Binary {
        kind: "binary_expression",
        left: Pick::Field("left"),
        operator: "operator",
        right: Pick::Field("right"),
    }],
    functions: &["function_definition"],
    param_fields: &["declarator"],
    declarations: &[
        ("init_declarator", "declarator"),
        ("declaration", "declarator"),
    ],
    non_locals: &[],
    idents_are_locals: false,
    mutations: &[
        m("assignment_expression", "left"),
        m("update_expression", "argument"),
        m("init_declarator", "declarator"),
        Mutation {
            kind: "pointer_expression",
            target: "",
            marker: Some("&"),
        },
    ],
    exits: &["return_statement", "break_statement", "goto_statement"],
    continues: &["continue_statement"],
    jumps: &[
        "labeled_statement",
        "goto_statement",
        "break_statement",
        "continue_statement",
    ],
    calls: &["call_expression"],
    macro_kind: None,
    // Code that differs per configuration, and inline assembly.
    opaque: &[
        "preproc_if",
        "preproc_ifdef",
        "gnu_asm_expression",
        "seh_try_statement",
    ],
    attributes: &[],
    blocks: &["compound_statement"],
    stores: &[
        StoreShape::Assign {
            wrapper: "expression_statement",
            kind: "assignment_expression",
            left: "left",
            right: "right",
            operator: true,
        },
        StoreShape::Declare {
            kind: "declaration",
            declarator: "init_declarator",
            name: "declarator",
            value: "value",
            shadows: false,
        },
    ],
    tries: &["seh_try_statement"],
    expr_statements: &["expression_statement"],
    cond_assignments: &["assignment_expression"],
    double_parens_mean_it: true,
    eq_methods: &[],
    limits: C_LIMITS,
    typed_declarations: &[
        ("declaration", "type", "declarator"),
        ("parameter_declaration", "type", "declarator"),
    ],
};

static CPP: Rules = Rules {
    idents: &["identifier", "this"],
    receivers: &["this"],
    literals: &[
        "number_literal",
        "string_literal",
        "raw_string_literal",
        "concatenated_string",
        "char_literal",
        "true",
        "false",
        "null",
        "nullptr",
    ],
    parens: &["parenthesized_expression", "condition_clause"],
    loops: &[
        "while_statement",
        "do_statement",
        "for_statement",
        "for_range_loop",
    ],
    functions: &["function_definition", "lambda_expression"],
    exits: &[
        "return_statement",
        "break_statement",
        "goto_statement",
        "throw_statement",
        "co_return_statement",
        "co_yield_statement",
        "co_await_expression",
    ],
    tries: &["try_statement", "seh_try_statement"],
    ..C_RULES
};

static C: Rules = C_RULES;

/// The table for `language`, if the lint rules cover it.
pub(super) fn for_language(language: Language) -> Option<&'static Rules> {
    match language {
        Language::Rust => Some(&RUST),
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => Some(&JS_TS),
        Language::Python => Some(&PYTHON),
        Language::Go => Some(&GO),
        Language::Java => Some(&JAVA),
        Language::C => Some(&C),
        Language::Cpp => Some(&CPP),
        _ => None,
    }
}

/// Calls that end the program or unwind: a loop holding one can end.
pub(super) const EXIT_CALLS: &[&str] = &[
    "exit", "_exit", "abort", "panic", "Exit", "Fatal", "Fatalf", "Fatalln", "Panic", "Panicf",
    "Panicln", "die", "quit", "FailNow", "Skip", "Skipf", "Goexit",
];

/// Rust macros that cannot leave a loop or change a variable they don't
/// name. Any other macro may hide a `return`/`break`/`?`.
pub(super) const QUIET_MACROS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "format_args",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "vec",
    "dbg",
    "concat",
    "stringify",
    "matches",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
];

/// The languages' own empty/failure/boolean values: a match arm yielding
/// one (`X::None => None`) passes a value on, it does not name its case.
pub(super) const PASSTHROUGH_VALUES: &[&str] = &[
    "None",
    "Some",
    "Ok",
    "Err",
    "null",
    "nil",
    "undefined",
    "NULL",
    "nullptr",
    "true",
    "false",
    "True",
    "False",
    "Default",
];

/// Rust macros whose arm body only reports that the arm should not run.
pub(super) const DIVERGING_MACROS: &[&str] =
    &["panic", "unreachable", "todo", "unimplemented", "bail"];
