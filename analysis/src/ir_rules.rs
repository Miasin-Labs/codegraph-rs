//! Per-language rules for the rules-driven IR lowering
//! ([`crate::ir::lower_with_rules`]): which tree-sitter node kinds are
//! declarations, assignments, calls, member and element accesses, and in
//! which fields they keep their parts. Control flow (if/loops/switch/try/
//! jumps) comes from [`crate::cfg_rules::CfgRules`], so a language is
//! lowered once both tables know it. Add a language by adding a table here,
//! never by branching in the lowerer.

/// Where a part of a node sits: a named field, or the `i`-th named child
/// (for grammars that leave the node unfielded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Field(&'static str),
    Child(usize),
}

/// A call-shaped node.
#[derive(Debug, Clone, Copy)]
pub struct CallShape {
    pub kind: &'static str,
    /// Field holding the callee expression (`f`, `obj.m`, `pkg.f`), when the
    /// grammar has one; its receiver comes from a member access there.
    pub function: Option<&'static str>,
    /// Field holding the receiver of a method call written apart
    /// (Java `object`, PHP `object`).
    pub object: Option<&'static str>,
    /// Field holding the method name, when written apart from the receiver.
    pub name: Option<&'static str>,
    /// Field holding the argument list (else: the child whose kind is in
    /// [`IrRules::argument_lists`]).
    pub arguments: Option<&'static str>,
    /// An object construction (`new T(…)`): the callee is `new T`.
    pub constructor: bool,
}

/// `object.property`.
#[derive(Debug, Clone, Copy)]
pub struct MemberShape {
    pub kind: &'static str,
    pub object: Slot,
    pub property: Slot,
}

/// `object[index]`.
#[derive(Debug, Clone, Copy)]
pub struct SubscriptShape {
    pub kind: &'static str,
    pub object: Slot,
    pub index: Slot,
}

/// `cond ? a : b`.
#[derive(Debug, Clone, Copy)]
pub struct TernaryShape {
    pub kind: &'static str,
    pub condition: Slot,
    pub consequence: Slot,
    pub alternative: Slot,
}

/// A loop that binds each element of an iterable (`for x in xs`).
#[derive(Debug, Clone, Copy)]
pub struct ForEachShape {
    pub kind: &'static str,
    pub binding: Slot,
    pub iterable: Slot,
}

/// A declaration statement and the declarators it holds.
#[derive(Debug, Clone, Copy)]
pub struct DeclShape {
    pub kind: &'static str,
    /// Child kinds that each declare one name (`variable_declarator`,
    /// `init_declarator`); a child that is itself a name declares it
    /// without a value.
    pub declarators: &'static [&'static str],
    /// Fields of a declarator holding the declared name (searched in
    /// order, descending through nested declarators: `*p`, `a[10]`).
    pub name: &'static [&'static str],
    /// Field of a declarator holding the initial value.
    pub value: &'static str,
}

/// How a language's syntax maps to IR, by node kind.
pub struct IrRules {
    /// Fields holding a function's parameter list.
    pub parameter_lists: &'static [&'static str],
    /// Fields of a parameter node holding its name (descended through
    /// nested declarators); a parameter that is itself an identifier is
    /// its own name.
    pub parameter_name: &'static [&'static str],
    /// Parameter kinds that are the receiver, not a positional parameter.
    pub receiver_parameters: &'static [&'static str],
    /// Nodes read as variables (their text is the name).
    pub identifiers: &'static [&'static str],
    /// Literal constants.
    pub literals: &'static [&'static str],
    /// String kinds that may interpolate values (`f"{x}"`, `` `${x}` ``,
    /// `"$x"`): constant unless a child is a value part.
    pub interpolating_strings: &'static [&'static str],
    /// Children of an interpolating string that are literal text.
    pub string_fragments: &'static [&'static str],
    pub declarations: &'static [DeclShape],
    /// `left = right` / `left op= right` (fields `left`, `right`,
    /// `operator`).
    pub assignments: &'static [&'static str],
    /// `x++` / `--x`: the first named child (or `argument`) is updated.
    pub updates: &'static [&'static str],
    /// Binary operators (fields `left`, `operator`, `right`).
    pub binaries: &'static [&'static str],
    /// Unary operators: the operand is field `operand`/`argument` or the
    /// last named child; the operator is field `operator` or the first
    /// token.
    pub unaries: &'static [&'static str],
    /// Unary kinds that designate storage through a pointer (`*p`, `&x`):
    /// the place of their operand.
    pub address_ops: &'static [&'static str],
    pub calls: &'static [CallShape],
    /// Kinds of argument-list nodes (for calls whose list is unfielded).
    pub argument_lists: &'static [&'static str],
    /// Argument wrappers whose value is a field or their last child
    /// (Python `keyword_argument`, PHP `argument`).
    pub argument_wrappers: &'static [&'static str],
    pub members: &'static [MemberShape],
    pub subscripts: &'static [SubscriptShape],
    pub ternaries: &'static [TernaryShape],
    /// Expressions whose value is their `value` field or last named child
    /// (parentheses, casts, `await`, C++ `condition_clause`).
    pub passthrough: &'static [&'static str],
    /// Collection literals and initializers: their value is built from
    /// every element.
    pub collections: &'static [&'static str],
    pub foreach: &'static [ForEachShape],
    /// Loops that run their body before testing their condition.
    pub do_loops: &'static [&'static str],
    /// Children of a switch arm holding its case values (Java
    /// `switch_label`); else the arm's `value` field holds the one value.
    pub case_labels: &'static [&'static str],
    /// Nodes never lowered (types, annotations, comments): skipped as
    /// statements and constant as expressions.
    pub ignored: &'static [&'static str],
    /// Object-like macro definitions (fields `name`, `value`): one whose
    /// body is a single name or literal is lowered as what it stands for
    /// (`#define ARG data` makes `ARG` read `data`).
    pub macro_definitions: &'static [&'static str],
    /// An expression-oriented language (Rust): blocks, `if` and `match`
    /// have values, patterns bind names, macros take token trees.
    pub expression: Option<&'static ExpressionRules>,
}

/// A binding of a pattern to a value: `let p = v;`, `if let p = v`.
#[derive(Debug, Clone, Copy)]
pub struct BindingShape {
    pub kind: &'static str,
    pub pattern: &'static str,
    pub value: &'static str,
    /// Field of the block run when the pattern does not match
    /// (`let … else { … }`).
    pub otherwise: Option<&'static str>,
}

/// A match arm: its pattern (binding names from the scrutinee), the guard
/// inside the pattern node, and the arm's value.
#[derive(Debug, Clone, Copy)]
pub struct ArmShape {
    pub kind: &'static str,
    pub pattern: &'static str,
    /// Field of the pattern node holding the guard (`p if cond`).
    pub guard: &'static str,
    pub value: &'static str,
}

/// A macro call written with a token tree (`format!("{}", x)`): the
/// callee is the macro's name plus `!`; the arguments are the tree's
/// comma-separated groups.
#[derive(Debug, Clone, Copy)]
pub struct MacroShape {
    pub kind: &'static str,
    /// Field holding the macro's name.
    pub name: &'static str,
    /// Kind of the token tree child.
    pub tokens: &'static str,
    /// Kinds of string literals whose `{name}` placeholders read variables.
    pub format_strings: &'static [&'static str],
}

/// What an expression-oriented language adds to [`IrRules`].
#[derive(Debug)]
pub struct ExpressionRules {
    pub bindings: &'static [BindingShape],
    pub arm: ArmShape,
    pub macros: &'static [MacroShape],
    /// Callee wrappers and the field holding the callee they wrap
    /// (`x.parse::<T>` is `x.parse`).
    pub callee_wrappers: &'static [(&'static str, &'static str)],
    /// The receiver parameter's variable (`self`).
    pub receiver_var: &'static str,
    /// Fields of a pattern that bind nothing (a variant's path, a guard).
    pub pattern_skip_fields: &'static [&'static str],
    /// Statement kinds that are never a block's value (`let`, `x;`).
    pub statements: &'static [&'static str],
    /// `e?`: `e`'s value, or an early return when it is an error — a
    /// branch on `e` itself, so a validator whose result is `?`-checked
    /// guards what follows.
    pub try_kind: &'static str,
    /// Struct literals, built field by field.
    pub structs: StructShape,
}

/// `S { a: x, b, ..base }`: a value whose field `a` holds `x`, `b` holds
/// `b`, and whose other fields come from `base`.
#[derive(Debug, Clone, Copy)]
pub struct StructShape {
    pub kind: &'static str,
    /// Field of the literal holding its initializer list.
    pub body: &'static str,
    /// `a: x`: the initializer kind, its name and value fields.
    pub initializer: &'static str,
    pub name: &'static str,
    pub value: &'static str,
    /// `b` (a name standing for `b: b`).
    pub shorthand: &'static str,
    /// `..base`.
    pub base: &'static str,
}

impl ExpressionRules {
    pub fn binding(&self, kind: &str) -> Option<&BindingShape> {
        self.bindings.iter().find(|shape| shape.kind == kind)
    }

    pub fn macro_call(&self, kind: &str) -> Option<&MacroShape> {
        self.macros.iter().find(|shape| shape.kind == kind)
    }

    pub fn callee_wrapper(&self, kind: &str) -> Option<&'static str> {
        self.callee_wrappers
            .iter()
            .find(|(wrapper, _)| *wrapper == kind)
            .map(|(_, field)| *field)
    }
}

impl IrRules {
    /// Rules for a language by its identifier.
    pub fn for_language(lang: &str) -> Option<&'static IrRules> {
        match lang {
            "java" => Some(&JAVA),
            "c" => Some(&C),
            "cpp" => Some(&CPP),
            "php" => Some(&PHP),
            "python" => Some(&PYTHON),
            "javascript" | "typescript" | "tsx" | "jsx" => Some(&JS),
            "rust" => Some(&RUST),
            _ => None,
        }
    }

    pub fn call(&self, kind: &str) -> Option<&CallShape> {
        self.calls.iter().find(|shape| shape.kind == kind)
    }

    pub fn member(&self, kind: &str) -> Option<&MemberShape> {
        self.members.iter().find(|shape| shape.kind == kind)
    }

    pub fn subscript(&self, kind: &str) -> Option<&SubscriptShape> {
        self.subscripts.iter().find(|shape| shape.kind == kind)
    }

    pub fn ternary(&self, kind: &str) -> Option<&TernaryShape> {
        self.ternaries.iter().find(|shape| shape.kind == kind)
    }

    pub fn foreach(&self, kind: &str) -> Option<&ForEachShape> {
        self.foreach.iter().find(|shape| shape.kind == kind)
    }

    pub fn declaration(&self, kind: &str) -> Option<&DeclShape> {
        self.declarations.iter().find(|shape| shape.kind == kind)
    }
}

// ─── Java ────────────────────────────────────────────────────────────────────

static JAVA: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["name"],
    receiver_parameters: &["receiver_parameter"],
    identifiers: &["identifier", "this", "super"],
    literals: &[
        "decimal_integer_literal",
        "hex_integer_literal",
        "octal_integer_literal",
        "binary_integer_literal",
        "decimal_floating_point_literal",
        "hex_floating_point_literal",
        "string_literal",
        "character_literal",
        "true",
        "false",
        "null_literal",
        "class_literal",
        "text_block",
    ],
    interpolating_strings: &[],
    string_fragments: &[],
    declarations: &[DeclShape {
        kind: "local_variable_declaration",
        declarators: &["variable_declarator"],
        name: &["name"],
        value: "value",
    }],
    assignments: &["assignment_expression"],
    updates: &["update_expression"],
    binaries: &["binary_expression"],
    unaries: &["unary_expression"],
    address_ops: &[],
    calls: &[
        CallShape {
            kind: "method_invocation",
            function: None,
            object: Some("object"),
            name: Some("name"),
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "object_creation_expression",
            function: None,
            object: None,
            name: Some("type"),
            arguments: Some("arguments"),
            constructor: true,
        },
        CallShape {
            kind: "explicit_constructor_invocation",
            function: None,
            object: Some("object"),
            name: Some("constructor"),
            arguments: Some("arguments"),
            constructor: false,
        },
    ],
    argument_lists: &["argument_list"],
    argument_wrappers: &[],
    members: &[MemberShape {
        kind: "field_access",
        object: Slot::Field("object"),
        property: Slot::Field("field"),
    }],
    subscripts: &[SubscriptShape {
        kind: "array_access",
        object: Slot::Field("array"),
        index: Slot::Field("index"),
    }],
    ternaries: &[TernaryShape {
        kind: "ternary_expression",
        condition: Slot::Field("condition"),
        consequence: Slot::Field("consequence"),
        alternative: Slot::Field("alternative"),
    }],
    passthrough: &["parenthesized_expression", "cast_expression"],
    collections: &["array_initializer", "array_creation_expression"],
    foreach: &[ForEachShape {
        kind: "enhanced_for_statement",
        binding: Slot::Field("name"),
        iterable: Slot::Field("value"),
    }],
    do_loops: &["do_statement"],
    case_labels: &["switch_label"],
    ignored: &[
        "comment",
        "line_comment",
        "block_comment",
        "modifiers",
        "marker_annotation",
        "annotation",
        "local_class_declaration",
        "class_declaration",
        "record_declaration",
        "interface_declaration",
        "enum_declaration",
        "lambda_expression",
        "method_reference",
        "empty_statement",
        "assert_statement",
    ],
    macro_definitions: &[],
    expression: None,
};

// ─── C / C++ ─────────────────────────────────────────────────────────────────

const C_LITERALS: &[&str] = &[
    "number_literal",
    "string_literal",
    "char_literal",
    "concatenated_string",
    "true",
    "false",
    "null",
    "nullptr",
    "raw_string_literal",
    "sizeof_expression",
];

const C_DECLARATIONS: &[DeclShape] = &[DeclShape {
    kind: "declaration",
    declarators: &["init_declarator"],
    name: &["declarator"],
    value: "value",
}];

const C_CALL: CallShape = CallShape {
    kind: "call_expression",
    function: Some("function"),
    object: None,
    name: None,
    arguments: Some("arguments"),
    constructor: false,
};

const C_MEMBERS: &[MemberShape] = &[MemberShape {
    kind: "field_expression",
    object: Slot::Field("argument"),
    property: Slot::Field("field"),
}];

const C_SUBSCRIPTS: &[SubscriptShape] = &[SubscriptShape {
    kind: "subscript_expression",
    object: Slot::Field("argument"),
    index: Slot::Field("index"),
}];

const C_TERNARIES: &[TernaryShape] = &[TernaryShape {
    kind: "conditional_expression",
    condition: Slot::Field("condition"),
    consequence: Slot::Field("consequence"),
    alternative: Slot::Field("alternative"),
}];

static C: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["declarator"],
    receiver_parameters: &[],
    identifiers: &["identifier", "field_identifier"],
    literals: C_LITERALS,
    interpolating_strings: &[],
    string_fragments: &[],
    declarations: C_DECLARATIONS,
    assignments: &["assignment_expression"],
    updates: &["update_expression"],
    binaries: &["binary_expression"],
    unaries: &["unary_expression", "pointer_expression"],
    address_ops: &["pointer_expression"],
    calls: &[C_CALL],
    argument_lists: &["argument_list"],
    argument_wrappers: &[],
    members: C_MEMBERS,
    subscripts: C_SUBSCRIPTS,
    ternaries: C_TERNARIES,
    passthrough: &[
        "parenthesized_expression",
        "cast_expression",
        "comma_expression",
    ],
    collections: &["initializer_list", "compound_literal_expression"],
    foreach: &[],
    do_loops: &["do_statement"],
    case_labels: &[],
    ignored: &[
        "comment",
        "type_definition",
        "struct_specifier",
        "preproc_call",
        "preproc_def",
        "preproc_function_def",
        "empty_statement",
        "goto_statement",
        "gnu_asm_expression",
    ],
    macro_definitions: &["preproc_def"],
    expression: None,
};

static CPP: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["declarator"],
    receiver_parameters: &[],
    identifiers: &[
        "identifier",
        "field_identifier",
        "this",
        "qualified_identifier",
    ],
    literals: C_LITERALS,
    interpolating_strings: &[],
    string_fragments: &[],
    declarations: C_DECLARATIONS,
    assignments: &["assignment_expression"],
    updates: &["update_expression"],
    binaries: &["binary_expression"],
    unaries: &["unary_expression", "pointer_expression"],
    address_ops: &["pointer_expression"],
    calls: &[
        C_CALL,
        CallShape {
            kind: "new_expression",
            function: None,
            object: None,
            name: Some("type"),
            arguments: Some("arguments"),
            constructor: true,
        },
    ],
    argument_lists: &["argument_list"],
    argument_wrappers: &[],
    members: C_MEMBERS,
    subscripts: C_SUBSCRIPTS,
    ternaries: C_TERNARIES,
    passthrough: &[
        "parenthesized_expression",
        "cast_expression",
        "comma_expression",
        "condition_clause",
        "static_cast_expression",
        "reinterpret_cast_expression",
    ],
    collections: &["initializer_list", "compound_literal_expression"],
    foreach: &[ForEachShape {
        kind: "for_range_loop",
        binding: Slot::Field("declarator"),
        iterable: Slot::Field("right"),
    }],
    do_loops: &["do_statement"],
    case_labels: &[],
    ignored: &[
        "comment",
        "type_definition",
        "struct_specifier",
        "class_specifier",
        "preproc_call",
        "preproc_def",
        "preproc_function_def",
        "empty_statement",
        "goto_statement",
        "lambda_expression",
        "using_declaration",
        "alias_declaration",
    ],
    macro_definitions: &["preproc_def"],
    expression: None,
};

// ─── PHP ─────────────────────────────────────────────────────────────────────

static PHP: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["name"],
    receiver_parameters: &[],
    identifiers: &["variable_name", "name", "qualified_name"],
    literals: &["integer", "float", "string", "boolean", "null", "nowdoc"],
    interpolating_strings: &["encapsed_string", "heredoc", "shell_command_expression"],
    string_fragments: &[
        "string_content",
        "escape_sequence",
        "heredoc_start",
        "heredoc_end",
        "string_value",
    ],
    declarations: &[],
    assignments: &[
        "assignment_expression",
        "augmented_assignment_expression",
        "reference_assignment_expression",
    ],
    updates: &["update_expression"],
    binaries: &["binary_expression"],
    unaries: &["unary_op_expression"],
    address_ops: &[],
    calls: &[
        CallShape {
            kind: "function_call_expression",
            function: Some("function"),
            object: None,
            name: None,
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "member_call_expression",
            function: None,
            object: Some("object"),
            name: Some("name"),
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "nullsafe_member_call_expression",
            function: None,
            object: Some("object"),
            name: Some("name"),
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "scoped_call_expression",
            function: None,
            object: None,
            name: Some("name"),
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "object_creation_expression",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: true,
        },
        CallShape {
            kind: "print_intrinsic",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: false,
        },
        CallShape {
            kind: "include_expression",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: false,
        },
        CallShape {
            kind: "include_once_expression",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: false,
        },
        CallShape {
            kind: "require_expression",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: false,
        },
        CallShape {
            kind: "require_once_expression",
            function: None,
            object: None,
            name: None,
            arguments: None,
            constructor: false,
        },
    ],
    argument_lists: &["arguments"],
    argument_wrappers: &["argument"],
    members: &[
        MemberShape {
            kind: "member_access_expression",
            object: Slot::Field("object"),
            property: Slot::Field("name"),
        },
        MemberShape {
            kind: "nullsafe_member_access_expression",
            object: Slot::Field("object"),
            property: Slot::Field("name"),
        },
        MemberShape {
            kind: "scoped_property_access_expression",
            object: Slot::Field("scope"),
            property: Slot::Field("name"),
        },
    ],
    subscripts: &[SubscriptShape {
        kind: "subscript_expression",
        object: Slot::Child(0),
        index: Slot::Child(1),
    }],
    ternaries: &[TernaryShape {
        kind: "conditional_expression",
        condition: Slot::Field("condition"),
        consequence: Slot::Field("body"),
        alternative: Slot::Field("alternative"),
    }],
    passthrough: &[
        "parenthesized_expression",
        "cast_expression",
        "error_suppression_expression",
    ],
    collections: &["array_creation_expression", "array_element_initializer"],
    foreach: &[ForEachShape {
        kind: "foreach_statement",
        binding: Slot::Child(1),
        iterable: Slot::Child(0),
    }],
    do_loops: &["do_statement"],
    case_labels: &[],
    ignored: &[
        "comment",
        "text_interpolation",
        "anonymous_function",
        "arrow_function",
        "function_definition",
        "class_declaration",
        "empty_statement",
        "global_declaration",
    ],
    macro_definitions: &[],
    expression: None,
};

// ─── Python ──────────────────────────────────────────────────────────────────

static PYTHON: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["name"],
    receiver_parameters: &[],
    identifiers: &["identifier"],
    literals: &["integer", "float", "true", "false", "none", "ellipsis"],
    interpolating_strings: &["string"],
    string_fragments: &[
        "string_start",
        "string_content",
        "string_end",
        "escape_sequence",
        "escape_interpolation",
    ],
    declarations: &[],
    assignments: &["assignment", "augmented_assignment"],
    updates: &[],
    binaries: &["binary_operator", "boolean_operator", "comparison_operator"],
    unaries: &["unary_operator", "not_operator"],
    address_ops: &[],
    calls: &[CallShape {
        kind: "call",
        function: Some("function"),
        object: None,
        name: None,
        arguments: Some("arguments"),
        constructor: false,
    }],
    argument_lists: &["argument_list"],
    argument_wrappers: &[
        "keyword_argument",
        "list_splat",
        "dictionary_splat",
        "interpolation",
    ],
    members: &[MemberShape {
        kind: "attribute",
        object: Slot::Field("object"),
        property: Slot::Field("attribute"),
    }],
    subscripts: &[SubscriptShape {
        kind: "subscript",
        object: Slot::Field("value"),
        index: Slot::Field("subscript"),
    }],
    ternaries: &[TernaryShape {
        kind: "conditional_expression",
        condition: Slot::Child(1),
        consequence: Slot::Child(0),
        alternative: Slot::Child(2),
    }],
    passthrough: &["parenthesized_expression", "await", "expression_list"],
    collections: &[
        "list",
        "tuple",
        "set",
        "dictionary",
        "pair",
        "concatenated_string",
        "list_comprehension",
        "generator_expression",
    ],
    foreach: &[ForEachShape {
        kind: "for_statement",
        binding: Slot::Field("left"),
        iterable: Slot::Field("right"),
    }],
    do_loops: &[],
    case_labels: &[],
    ignored: &[
        "comment",
        "pass_statement",
        "import_statement",
        "import_from_statement",
        "function_definition",
        "class_definition",
        "decorated_definition",
        "lambda",
        "global_statement",
        "nonlocal_statement",
    ],
    macro_definitions: &[],
    expression: None,
};

// ─── JavaScript / TypeScript ─────────────────────────────────────────────────

static JS: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["pattern", "name", "left"],
    receiver_parameters: &[],
    identifiers: &[
        "identifier",
        "this",
        "shorthand_property_identifier",
        "property_identifier",
    ],
    literals: &[
        "number",
        "string",
        "true",
        "false",
        "null",
        "undefined",
        "regex",
    ],
    interpolating_strings: &["template_string"],
    string_fragments: &["string_fragment", "escape_sequence"],
    declarations: &[
        DeclShape {
            kind: "lexical_declaration",
            declarators: &["variable_declarator"],
            name: &["name"],
            value: "value",
        },
        DeclShape {
            kind: "variable_declaration",
            declarators: &["variable_declarator"],
            name: &["name"],
            value: "value",
        },
    ],
    assignments: &["assignment_expression", "augmented_assignment_expression"],
    updates: &["update_expression"],
    binaries: &["binary_expression"],
    unaries: &["unary_expression"],
    address_ops: &[],
    calls: &[
        CallShape {
            kind: "call_expression",
            function: Some("function"),
            object: None,
            name: None,
            arguments: Some("arguments"),
            constructor: false,
        },
        CallShape {
            kind: "new_expression",
            function: None,
            object: None,
            name: Some("constructor"),
            arguments: Some("arguments"),
            constructor: true,
        },
    ],
    argument_lists: &["arguments"],
    argument_wrappers: &["spread_element", "template_substitution"],
    members: &[MemberShape {
        kind: "member_expression",
        object: Slot::Field("object"),
        property: Slot::Field("property"),
    }],
    subscripts: &[SubscriptShape {
        kind: "subscript_expression",
        object: Slot::Field("object"),
        index: Slot::Field("index"),
    }],
    ternaries: &[TernaryShape {
        kind: "ternary_expression",
        condition: Slot::Field("condition"),
        consequence: Slot::Field("consequence"),
        alternative: Slot::Field("alternative"),
    }],
    passthrough: &[
        "parenthesized_expression",
        "await_expression",
        "as_expression",
        "non_null_expression",
        "satisfies_expression",
        "type_assertion",
    ],
    collections: &["array", "object", "pair", "sequence_expression"],
    foreach: &[ForEachShape {
        kind: "for_in_statement",
        binding: Slot::Field("left"),
        iterable: Slot::Field("right"),
    }],
    do_loops: &["do_statement"],
    case_labels: &[],
    ignored: &[
        "comment",
        "empty_statement",
        "import_statement",
        "function_declaration",
        "generator_function_declaration",
        "class_declaration",
        "function_expression",
        "function",
        "arrow_function",
        "class",
        "type_alias_declaration",
        "interface_declaration",
    ],
    macro_definitions: &[],
    expression: None,
};

// ─── Rust ────────────────────────────────────────────────────────────────────

static RUST_EXPRESSIONS: ExpressionRules = ExpressionRules {
    bindings: &[
        BindingShape {
            kind: "let_declaration",
            pattern: "pattern",
            value: "value",
            otherwise: Some("alternative"),
        },
        BindingShape {
            kind: "let_condition",
            pattern: "pattern",
            value: "value",
            otherwise: None,
        },
    ],
    arm: ArmShape {
        kind: "match_arm",
        pattern: "pattern",
        guard: "condition",
        value: "value",
    },
    macros: &[MacroShape {
        kind: "macro_invocation",
        name: "macro",
        tokens: "token_tree",
        format_strings: &["string_literal", "raw_string_literal"],
    }],
    callee_wrappers: &[("generic_function", "function")],
    receiver_var: "self",
    pattern_skip_fields: &["type", "condition"],
    statements: &["expression_statement", "let_declaration", "empty_statement"],
    try_kind: "try_expression",
    structs: StructShape {
        kind: "struct_expression",
        body: "body",
        initializer: "field_initializer",
        name: "field",
        value: "value",
        shorthand: "shorthand_field_initializer",
        base: "base_field_initializer",
    },
};

static RUST: IrRules = IrRules {
    parameter_lists: &["parameters"],
    parameter_name: &["pattern"],
    receiver_parameters: &["self_parameter"],
    identifiers: &["identifier", "self", "shorthand_field_identifier"],
    literals: &[
        "string_literal",
        "raw_string_literal",
        "char_literal",
        "integer_literal",
        "float_literal",
        "boolean_literal",
        "unit_expression",
        "scoped_identifier",
        "negative_literal",
    ],
    interpolating_strings: &[],
    string_fragments: &[],
    declarations: &[],
    assignments: &["assignment_expression", "compound_assignment_expr"],
    updates: &[],
    binaries: &["binary_expression"],
    unaries: &["unary_expression"],
    address_ops: &[],
    calls: &[CallShape {
        kind: "call_expression",
        function: Some("function"),
        object: None,
        name: None,
        arguments: Some("arguments"),
        constructor: false,
    }],
    argument_lists: &["arguments"],
    argument_wrappers: &[],
    members: &[MemberShape {
        kind: "field_expression",
        object: Slot::Field("value"),
        property: Slot::Field("field"),
    }],
    subscripts: &[SubscriptShape {
        kind: "index_expression",
        object: Slot::Child(0),
        index: Slot::Child(1),
    }],
    ternaries: &[],
    passthrough: &[
        "parenthesized_expression",
        "await_expression",
        "reference_expression",
        "type_cast_expression",
    ],
    collections: &["array_expression", "tuple_expression"],
    foreach: &[ForEachShape {
        kind: "for_expression",
        binding: Slot::Field("pattern"),
        iterable: Slot::Field("value"),
    }],
    do_loops: &[],
    case_labels: &[],
    ignored: &[
        "line_comment",
        "block_comment",
        "attribute_item",
        "inner_attribute_item",
        "use_declaration",
        "function_item",
        "function_signature_item",
        "struct_item",
        "enum_item",
        "union_item",
        "impl_item",
        "trait_item",
        "mod_item",
        "const_item",
        "static_item",
        "type_item",
        "macro_definition",
        "extern_crate_declaration",
        "foreign_mod_item",
        "empty_statement",
        "label",
        "lifetime",
        "type_arguments",
        "mutable_specifier",
    ],
    macro_definitions: &[],
    expression: Some(&RUST_EXPRESSIONS),
};

#[cfg(test)]
mod tests {
    use super::*;

    const COMMENTS: &[&str] = &["comment", "line_comment", "block_comment"];

    #[test]
    fn every_lowered_language_has_control_flow_rules() {
        for lang in [
            "java",
            "c",
            "cpp",
            "php",
            "python",
            "javascript",
            "typescript",
            "rust",
        ] {
            assert!(IrRules::for_language(lang).is_some(), "{lang}");
            assert!(
                crate::cfg_rules::CfgRules::for_language(lang).is_some(),
                "{lang} has IR rules but no CFG rules"
            );
        }
    }

    #[test]
    fn comments_are_ignored_everywhere() {
        for lang in ["java", "c", "cpp", "php", "python", "javascript", "rust"] {
            let rules = IrRules::for_language(lang).unwrap();
            assert!(
                COMMENTS.iter().any(|c| rules.ignored.contains(c)),
                "{lang} does not ignore comments"
            );
        }
    }
}
