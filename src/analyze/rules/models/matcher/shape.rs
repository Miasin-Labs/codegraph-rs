//! A call's anatomy, per language: its callee name, receiver, positional
//! and keyword arguments, and — when the callee is a plain path
//! (`a.b.c`, `Type::f`) — that path. Field names per grammar are in
//! [`SHAPES`]; the code reads them and never branches on a language.

use tree_sitter::Node;

/// How one call node kind spells its parts.
struct Shape {
    kind: &'static str,
    /// The field holding the callee expression (`function`), or the type
    /// constructed (`type`, `constructor`) when `constructor`.
    callee: &'static str,
    /// For a callee holding a receiver: (member node kind, object field,
    /// name field) — `field_expression` + `argument` + `field`.
    members: &'static [(&'static str, &'static str, &'static str)],
    /// Scoped path node kinds whose `name` field is the name
    /// (`scoped_identifier`, `qualified_identifier`).
    scoped: &'static [&'static str],
    /// The call names its method directly (Java: `object` + `name` on the
    /// call node itself).
    direct: Option<(&'static str, &'static str)>,
    arguments: &'static str,
    /// Argument node kinds that are `name=value` keywords: (kind, name
    /// field, value field).
    keyword: Option<(&'static str, &'static str, &'static str)>,
    /// Argument node kinds skipped (splats, comments).
    skipped: &'static [&'static str],
    constructor: bool,
}

const SHAPES: &[Shape] = &[
    // Java
    Shape {
        kind: "method_invocation",
        callee: "name",
        members: &[],
        scoped: &[],
        direct: Some(("object", "name")),
        arguments: "arguments",
        keyword: None,
        skipped: &["line_comment", "block_comment"],
        constructor: false,
    },
    Shape {
        kind: "object_creation_expression",
        callee: "type",
        members: &[],
        scoped: &[],
        direct: None,
        arguments: "arguments",
        keyword: None,
        skipped: &["line_comment", "block_comment"],
        constructor: true,
    },
    // C/C++, JavaScript, Rust
    Shape {
        kind: "call_expression",
        callee: "function",
        members: &[
            ("field_expression", "argument", "field"),
            ("field_expression", "value", "field"),
            ("member_expression", "object", "property"),
        ],
        scoped: &["qualified_identifier", "scoped_identifier"],
        direct: None,
        arguments: "arguments",
        keyword: None,
        skipped: &[
            "comment",
            "line_comment",
            "block_comment",
            "spread_element",
            "attribute_item",
        ],
        constructor: false,
    },
    // C++ / JavaScript `new`
    Shape {
        kind: "new_expression",
        callee: "constructor",
        members: &[],
        scoped: &[],
        direct: None,
        arguments: "arguments",
        keyword: None,
        skipped: &["comment", "spread_element"],
        constructor: true,
    },
    // Python
    Shape {
        kind: "call",
        callee: "function",
        members: &[("attribute", "object", "attribute")],
        scoped: &[],
        direct: None,
        arguments: "arguments",
        keyword: Some(("keyword_argument", "name", "value")),
        skipped: &["comment", "list_splat", "dictionary_splat"],
        constructor: false,
    },
];

/// A call's parts.
#[derive(Debug, Clone)]
pub struct CallShape<'t> {
    pub call: Node<'t>,
    /// The callee's last name (a constructed type's simple name).
    pub name: String,
    pub receiver: Option<Node<'t>>,
    pub args: Vec<Node<'t>>,
    pub keywords: Vec<(String, Node<'t>)>,
    pub constructor: bool,
    /// The callee as a plain path of names, as written (`a.b.c`,
    /// `std::env::var`, `java.io.File`), whitespace removed; `None` when
    /// it is not one (a call's result, an index…).
    pub path: Option<String>,
    /// The receiver as a plain path (`request`, `this.stmt`).
    pub receiver_path: Option<String>,
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// Node kinds that make up a plain path of names.
const PATH_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "namespace_identifier",
    "scoped_identifier",
    "scoped_type_identifier",
    "qualified_identifier",
    "field_access",
    "attribute",
    "member_expression",
    "field_expression",
    "this",
    "self",
    "super",
    "crate",
    "generic_type",
    "generic_function",
    "type_arguments",
];

/// `node` as a plain path (`a.b.c`, `x::Y`), generics removed.
pub fn plain_path(node: Node, source: &str) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if !PATH_KINDS.contains(&n.kind()) {
            return None;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in text(node, source).chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 || ch.is_whitespace() => {}
            _ => out.push(ch),
        }
    }
    let out = out.replace("::::", "::").replace("?.", ".");
    (!out.is_empty()).then_some(out)
}

/// Whether `kind` is a call node some language's shape reads.
pub fn is_call(kind: &str) -> bool {
    SHAPES.iter().any(|shape| shape.kind == kind)
}

/// The parts of `call`, if its kind is a call.
pub fn shape<'t>(call: Node<'t>, source: &str) -> Option<CallShape<'t>> {
    let spec = SHAPES.iter().find(|shape| shape.kind == call.kind())?;
    let mut out = CallShape {
        call,
        name: String::new(),
        receiver: None,
        args: Vec::new(),
        keywords: Vec::new(),
        constructor: spec.constructor,
        path: None,
        receiver_path: None,
    };
    if let Some((object_field, name_field)) = spec.direct {
        let name = call.child_by_field_name(name_field)?;
        out.name = text(name, source).to_string();
        out.receiver = call.child_by_field_name(object_field);
        out.receiver_path = out.receiver.and_then(|r| plain_path(r, source));
        out.path = match (&out.receiver, &out.receiver_path) {
            (None, _) => Some(out.name.clone()),
            (Some(_), Some(receiver)) => Some(format!("{receiver}.{}", out.name)),
            _ => None,
        };
    } else {
        let mut callee = call.child_by_field_name(spec.callee)?;
        // `f::<T>(…)`: the function inside.
        while callee.kind() == "generic_function" {
            callee = callee.child_by_field_name("function")?;
        }
        if spec.constructor {
            let path = plain_path(callee, source)?;
            out.name = last_segment(&path).to_string();
            out.path = Some(path);
        } else if let Some((_, object_field, name_field)) =
            spec.members.iter().find(|(kind, object, _)| {
                *kind == callee.kind() && callee.child_by_field_name(object).is_some()
            })
        {
            let name = callee.child_by_field_name(name_field)?;
            out.name = text(name, source).to_string();
            out.receiver = callee.child_by_field_name(object_field);
            out.receiver_path = out.receiver.and_then(|r| plain_path(r, source));
            out.path = plain_path(callee, source);
        } else if spec.scoped.contains(&callee.kind()) {
            let name = callee.child_by_field_name("name")?;
            out.name = text(name, source).to_string();
            out.path = plain_path(callee, source);
        } else if matches!(callee.kind(), "identifier" | "field_identifier") {
            out.name = text(callee, source).to_string();
            out.path = Some(out.name.clone());
        } else {
            return None;
        }
    }
    let Some(arguments) = call.child_by_field_name(spec.arguments) else {
        return Some(out);
    };
    let mut cursor = arguments.walk();
    let children: Vec<Node> =
        if arguments.kind().ends_with("arguments") || arguments.kind() == "argument_list" {
            arguments.named_children(&mut cursor).collect()
        } else {
            // A lone generator/argument node.
            vec![arguments]
        };
    for child in children {
        if spec.skipped.contains(&child.kind()) || child.kind().ends_with("comment") {
            continue;
        }
        if let Some((kind, name_field, value_field)) = spec.keyword {
            if child.kind() == kind {
                if let (Some(name), Some(value)) = (
                    child.child_by_field_name(name_field),
                    child.child_by_field_name(value_field),
                ) {
                    out.keywords.push((text(name, source).to_string(), value));
                }
                continue;
            }
        }
        out.args.push(child);
    }
    Some(out)
}

/// The last name of a path (`a.b.C` → `C`, `x::y` → `y`).
pub fn last_segment(path: &str) -> &str {
    path.rsplit(['.', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}
