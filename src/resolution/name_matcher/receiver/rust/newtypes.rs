//! The binding a single-field tuple-struct pattern with a written type
//! introduces: `State(state): State<AppState>` in an axum handler's
//! parameters (or a `let`, or a closure parameter) binds `state` to
//! `AppState`, the one field of `State<AppState>`.
//!
//! The pattern's constructor must be the written type's own name
//! (`W(x): W<T>`), and what that one field holds must be known:
//!
//! - for a project tuple struct, its declaration says: `struct Wrap<T>(pub
//!   T);` holds the type argument written in `T`'s place, `struct
//!   Id(pub Inner);` holds `Inner`, and a field that merely mentions a
//!   parameter (`Vec<T>`) is not typed;
//! - for a type outside the project, only [`NEWTYPE_WRAPPERS`] are known.
//!   Any other external `Foo(x): Foo<T>` stays unknown: its field could be
//!   `Vec<T>` or `Arc<T>`, and guessing `T` would name the wrong type.
//!
//! Nested (`State(AppState { devices, .. })`), referenced (`&W(x)`, or a
//! `&W<T>` written type), and multi-field patterns are not typed.

use super::bindings::word_positions;
use super::lookup::{RustType, resolve_type};
use super::types::{matching_close, matching_paren, split_path, split_top_level, type_args};
use super::variants::without_visibility;
use super::{Inference, Origin, Value};
use crate::resolution::line_index::Lines;

/// A well-known wrapper outside the project whose definition is
/// `pub struct Name<T>(pub T);`, so the one field of `Name<X>` is `X`.
struct NewtypeWrapper {
    name: &'static str,
    /// The crates that define it under this name. A type whose path names
    /// another crate is not this wrapper; one no `use` places (a glob
    /// import) is matched by its name alone.
    crates: &'static [&'static str],
}

/// Extractors and responders of the web frameworks, each checked against
/// its definition:
///
/// - axum 0.7/0.8: `State<S>(pub S)`, `Path<T>(pub T)`, `Query<T>(pub T)`,
///   `Json<T>(pub T)`, `Extension<T>(pub T)`, `Form<T>(pub T)`,
///   `ConnectInfo<T>(pub T)`;
/// - actix-web 4 (`web::…`): `Path<T>`, `Query<T>`, `Json<T>`, `Form<T>`,
///   each wrapping `T` itself. (A pattern only compiles where the field is
///   visible, so whether it is `pub` does not change what it holds.)
///
/// Not `web::Data<T>` (it wraps `Arc<T>`), and nothing else: an unknown
/// wrapper's field is not known.
const NEWTYPE_WRAPPERS: &[NewtypeWrapper] = &[
    NewtypeWrapper {
        name: "State",
        crates: AXUM,
    },
    NewtypeWrapper {
        name: "Extension",
        crates: AXUM,
    },
    NewtypeWrapper {
        name: "ConnectInfo",
        crates: AXUM,
    },
    NewtypeWrapper {
        name: "Path",
        crates: AXUM_OR_ACTIX,
    },
    NewtypeWrapper {
        name: "Query",
        crates: AXUM_OR_ACTIX,
    },
    NewtypeWrapper {
        name: "Json",
        crates: AXUM_OR_ACTIX,
    },
    NewtypeWrapper {
        name: "Form",
        crates: AXUM_OR_ACTIX,
    },
];

const AXUM: &[&str] = &["axum", "axum_core"];
const AXUM_OR_ACTIX: &[&str] = &["axum", "axum_core", "actix_web"];

/// The constructor of the single-field tuple-struct pattern `W(name)` (or
/// `path::W(mut name)`) binding `name`.
pub(super) fn newtype_constructor<'a>(pattern: &'a str, name: &str) -> Option<&'a str> {
    let pattern = pattern.trim();
    let open = pattern.find('(')?;
    let path = pattern[..open].trim();
    let inner = pattern[open..].strip_prefix('(')?.strip_suffix(')')?;
    let last = path.rsplit("::").next()?;
    let is_path = path.split("::").all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    });
    let bound = inner.trim();
    let bound = ["ref ", "mut "].iter().fold(bound, |bound, mode| {
        bound.strip_prefix(mode).map_or(bound, str::trim_start)
    });
    (is_path
        && last.starts_with(|c: char| c.is_ascii_uppercase())
        && !matches!(last, "Some" | "Ok" | "Err")
        && bound == name)
        .then_some(path)
}

impl Inference<'_> {
    /// The type of `x` in `constructor(x): written`, `written` being written
    /// in the referencing file.
    pub(super) fn newtype_field(&self, constructor: &str, written: &str) -> Option<Value> {
        let (path, args) = split_path(written.trim());
        let name = path.rsplit("::").next()?;
        if name.is_empty() || constructor.rsplit("::").next()? != name {
            return None;
        }
        let args = type_args(&args);
        let ty = resolve_type(
            path,
            &self.reference.file_path,
            self.reference,
            self.context,
        );
        if ty.is_project_type(self.context) {
            return self.project_newtype_field(&ty, &args);
        }
        let wrapper = NEWTYPE_WRAPPERS
            .iter()
            .find(|wrapper| wrapper.name == ty.name)?;
        if ty
            .external_crate()
            .is_some_and(|krate| !wrapper.crates.contains(&krate))
        {
            return None;
        }
        match args.as_slice() {
            [inner] => Some(self.here(*inner)),
            _ => None,
        }
    }

    /// The one field of the project tuple struct `ty`, instantiated with
    /// the written type arguments `args`.
    fn project_newtype_field(&self, ty: &RustType, args: &[&str]) -> Option<Value> {
        let nodes = self.context.get_nodes_by_name(&ty.name);
        let declaration = ty.struct_declaration(&nodes, self.reference)?;
        let source = self.context.read_file_arc(&declaration.file_path)?;
        let index = (declaration.start_line as usize).checked_sub(1)?;
        let lines = Lines::of(&source);
        let line = (index < lines.len()).then(|| lines.raw_text(index..index + 1))?;
        let (params, field) = tuple_struct_shape(line, &ty.name)?;
        if let Some(position) = params.iter().position(|param| *param == field) {
            return (args.len() == params.len()).then(|| self.here(args[position]));
        }
        if params
            .iter()
            .any(|param| word_positions(field, param).next().is_some())
        {
            return None;
        }
        Some(Value::Written {
            text: field.to_string(),
            self_ty: Some(ty.clone()),
            file: Origin::Project(declaration.file_path.clone()),
        })
    }
}

/// `struct name<A, 'a, B: Bound>(pub Field);` declared on `line`: its
/// generic parameters' names (lifetimes dropped) and its one field's type.
/// `None` unless the struct is a tuple struct with exactly one field.
fn tuple_struct_shape<'a>(line: &'a str, name: &str) -> Option<(Vec<&'a str>, &'a str)> {
    let rest = word_positions(line, "struct").find_map(|at| {
        let rest = line[at + "struct".len()..].trim_start();
        let rest = rest.strip_prefix(name)?;
        (!rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')).then_some(rest)
    })?;
    let rest = rest.trim_start();
    let (params, rest) = match rest.strip_prefix('<') {
        Some(_) => {
            let close = matching_close(rest)?;
            let params = split_top_level(&rest[1..close], b',')
                .into_iter()
                .filter(|param| !param.is_empty() && !param.starts_with('\''))
                .map(|param| {
                    let param = param.strip_prefix("const ").map_or(param, str::trim_start);
                    param.split([':', '=', ' ']).next().unwrap_or(param).trim()
                })
                .collect();
            (params, rest[close + 1..].trim_start())
        }
        None => (Vec::new(), rest),
    };
    if !rest.starts_with('(') {
        return None;
    }
    let close = matching_paren(rest)?;
    match split_top_level(&rest[1..close], b',')
        .into_iter()
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>()
        .as_slice()
    {
        [field] => {
            let field = without_visibility(field)?;
            (!field.is_empty()).then_some((params, field))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{newtype_constructor, tuple_struct_shape};

    #[test]
    fn reads_single_field_constructor_patterns() {
        assert_eq!(newtype_constructor("State(state)", "state"), Some("State"));
        assert_eq!(
            newtype_constructor("web::Json(mut body)", "body"),
            Some("web::Json")
        );
        assert_eq!(newtype_constructor("Pair(a, b)", "b"), None);
        assert_eq!(newtype_constructor("Some(x)", "x"), None);
        assert_eq!(
            newtype_constructor("State(AppState { devices, .. })", "devices"),
            None
        );
        assert_eq!(newtype_constructor("&State(state)", "state"), None);
        assert_eq!(newtype_constructor("state", "state"), None);
    }

    #[test]
    fn reads_tuple_struct_declarations() {
        assert_eq!(
            tuple_struct_shape("pub struct Wrap<T>(pub T);", "Wrap"),
            Some((vec!["T"], "T"))
        );
        assert_eq!(
            tuple_struct_shape("struct Id<'a, K: Hash + 'a>(pub(crate) &'a K);", "Id"),
            Some((vec!["K"], "&'a K"))
        );
        assert_eq!(
            tuple_struct_shape("pub struct Inner(Cache);", "Inner"),
            Some((vec![], "Cache"))
        );
        assert_eq!(
            tuple_struct_shape("pub struct Pair<A, B>(pub A, pub B);", "Pair"),
            None
        );
        assert_eq!(
            tuple_struct_shape("pub struct Wrapper { x: u8 }", "Wrapper"),
            None
        );
        assert_eq!(
            tuple_struct_shape("pub struct WrapMore<T>(T);", "Wrap"),
            None
        );
    }
}
