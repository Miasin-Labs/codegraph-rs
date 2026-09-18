//! Function signatures: the receiver and the positional parameters, one
//! entry per argument position.
//!
//! Interprocedural analyses bind the `i`-th argument of a call to
//! `params[i]`, so every lowerer must record exactly one entry per
//! positional parameter — no comments, attributes or `*`/`/` separators —
//! and keep the method receiver out of the list.

use tree_sitter::Node;

use super::Var;
use super::call::is_comment;

/// A function's receiver and positional parameters.
#[derive(Debug, Default)]
pub(super) struct Signature {
    pub(super) receiver: Option<Var>,
    pub(super) params: Vec<Var>,
}

impl Signature {
    fn push(&mut self, binding: &str) {
        let binding = binding.trim();
        if !binding.is_empty() {
            self.params.push(Var::new(binding));
        }
    }
}

fn text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// Rust: `&self` / `&mut self` / `self: Box<Self>` is the receiver; each
/// `parameter` contributes its pattern (`mut x` binds `x`).
pub(super) fn rust(function: Node<'_>, source: &str) -> Signature {
    let mut sig = Signature::default();
    let Some(list) = function.child_by_field_name("parameters") else {
        return sig;
    };
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        match param.kind() {
            "self_parameter" => sig.receiver = Some(Var::new("self")),
            "parameter" => match param.child_by_field_name("pattern") {
                Some(pat) if pat.kind() == "self" => sig.receiver = Some(Var::new("self")),
                Some(pat) => sig.push(text(pat, source)),
                None => sig.push(text(param, source)),
            },
            // `#[attr]` on a parameter is a sibling node; C-variadic `...`
            // binds no name.
            "attribute_item" | "variadic_parameter" => {}
            _ if is_comment(param) => {}
            _ => sig.push(text(param, source)),
        }
    }
    sig
}

/// Python: the first parameter of a method defined in a class body (other
/// than a `@staticmethod`) is the receiver — `self`, or `cls` of a
/// `@classmethod`. Outside a class a leading `self` still is.
pub(super) fn python(function: Node<'_>, source: &str) -> Signature {
    let mut sig = Signature::default();
    let Some(list) = function.child_by_field_name("parameters") else {
        return sig;
    };
    let method = python_is_bound_method(function, source);
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        let Some(binding) = python_binding(param, source) else {
            continue;
        };
        let first = sig.receiver.is_none() && sig.params.is_empty();
        if first && (method || binding == "self") {
            sig.receiver = Some(Var::new(binding));
        } else {
            sig.push(binding);
        }
    }
    sig
}

/// The name a Python parameter binds; `None` for the `*` / `/` markers.
fn python_binding<'a>(param: Node<'a>, source: &'a str) -> Option<&'a str> {
    match param.kind() {
        "keyword_separator" | "positional_separator" => return None,
        _ if is_comment(param) => return None,
        "default_parameter" | "typed_default_parameter" => {
            return param
                .child_by_field_name("name")
                .map(|name| text(name, source));
        }
        _ => {}
    }
    // `x: int`, `*args`, `**kwargs`, `*args: int`: the identifier is the
    // first named child, possibly inside a splat.
    let mut node = param;
    while matches!(
        node.kind(),
        "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern"
    ) {
        match node.named_child(0) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    Some(text(node, source))
}

/// Is `function` defined directly in a class body without `@staticmethod`?
fn python_is_bound_method(function: Node<'_>, source: &str) -> bool {
    let decorated = function
        .parent()
        .filter(|p| p.kind() == "decorated_definition");
    let static_method = decorated.is_some_and(|d| {
        let mut cursor = d.walk();
        d.named_children(&mut cursor)
            .filter(|c| c.kind() == "decorator")
            .any(|c| text(c, source).trim_start_matches('@').trim() == "staticmethod")
    });
    let in_class = decorated
        .unwrap_or(function)
        .parent()
        .and_then(|block| block.parent())
        .is_some_and(|owner| owner.kind() == "class_definition");
    in_class && !static_method
}

/// TypeScript / JavaScript: a non-static method's receiver is `this`; a
/// function may declare it as a leading `this: T` parameter.
pub(super) fn typescript(function: Node<'_>, source: &str) -> Signature {
    let mut sig = Signature::default();
    if function.kind() == "method_definition" && !ts_is_static(function) {
        sig.receiver = Some(Var::new("this"));
    }
    let list = function
        .child_by_field_name("parameters")
        .or_else(|| function.child_by_field_name("parameter"));
    let Some(list) = list else {
        return sig;
    };
    if list.kind() != "formal_parameters" {
        // Arrow function with a single bare parameter: `x => ...`.
        sig.push(text(list, source));
        return sig;
    }
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        if is_comment(param) {
            continue;
        }
        let binding = ts_binding(param, source);
        if binding == "this" {
            sig.receiver = Some(Var::new("this"));
        } else {
            sig.push(binding);
        }
    }
    sig
}

fn ts_is_static(method: Node<'_>) -> bool {
    let mut cursor = method.walk();
    method.children(&mut cursor).any(|c| c.kind() == "static")
}

/// The name a TS/JS parameter binds: `x` for `x`, `x?: T`, `x = 1`,
/// `...x`, `public x: T`; destructuring patterns keep their text (they bind
/// no single name, but still hold a position).
fn ts_binding<'a>(param: Node<'a>, source: &'a str) -> &'a str {
    let mut node = param;
    loop {
        let inner = match node.kind() {
            "required_parameter" | "optional_parameter" => node.child_by_field_name("pattern"),
            "assignment_pattern" => node.child_by_field_name("left"),
            "rest_pattern" => node.named_child(0),
            _ => None,
        };
        match inner {
            Some(inner) => node = inner,
            None => return text(node, source),
        }
    }
}

/// Go: the method receiver (`s` in `func (s *S) M()`), then every name of
/// every parameter declaration — `a, b int` binds two positions.
pub(super) fn go(function: Node<'_>, source: &str) -> Signature {
    let receiver = function
        .child_by_field_name("receiver")
        .and_then(|list| {
            let mut cursor = list.walk();
            let decl = list
                .named_children(&mut cursor)
                .find(|c| c.kind() == "parameter_declaration");
            decl
        })
        .and_then(|decl| decl.child_by_field_name("name"))
        .map(|name| Var::new(text(name, source)));
    let mut sig = Signature {
        receiver,
        params: Vec::new(),
    };
    let Some(list) = function.child_by_field_name("parameters") else {
        return sig;
    };
    let mut cursor = list.walk();
    for decl in list.named_children(&mut cursor) {
        if !matches!(
            decl.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        ) {
            continue;
        }
        let mut names = decl.walk();
        for name in decl.children_by_field_name("name", &mut names) {
            sig.push(text(name, source));
        }
    }
    sig
}

#[cfg(test)]
mod tests;
