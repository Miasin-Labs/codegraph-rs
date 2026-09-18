//! Following a chain one link at a time: `recv.field`, `recv.m(..)`,
//! `recv[i]`, on the type the chain has reached so far.
//!
//! - On a project type, a field has its declared type and a method its
//!   declared return type (`Self` meaning that type).
//! - On anything else, only what [`adaptors`](super::adaptors) knows of a
//!   std wrapper, container, or iterator is followed.
//!
//! A method the project type does not itself define (a trait's provided
//! method, a derive), a generic return type (`T`, `Self::Item`), and every
//! other external method end the chain: the type after them is not known,
//! and nothing after them is guessed.

use super::adaptors::{Adapted, adapt, guarded, indexed, item};
use super::expr::Tail;
use super::fields::declared_type;
use super::lookup::{RustType, assoc_fn_return};
use super::types::{Named, named_type, peeled, split_path, split_top_level};
use super::{Inference, Origin, Value};

/// How many lock guards deep one method call is followed.
const MAX_GUARDS: u8 = 2;

impl Inference<'_> {
    /// The type after one link.
    pub(super) fn link_value(&self, value: Value, tail: &Tail<'_>) -> Option<Value> {
        match tail {
            Tail::Same => Some(value),
            Tail::Unwrap => self.unwrap(value),
            Tail::Spelled(text) => Some(self.here(text.clone())),
            Tail::Method { name, .. } => self.method_value(value, name, 0),
            Tail::Field(name) => self.field_value(value, name),
            Tail::Index => match value {
                Value::Written {
                    text,
                    self_ty,
                    file,
                } => Some(Value::Written {
                    text: indexed(&text)?,
                    self_ty,
                    file,
                }),
                Value::Resolved(_) => None,
            },
        }
    }

    /// What `value.method(..)` returns.
    fn method_value(&self, value: Value, method: &str, guards: u8) -> Option<Value> {
        let ty = self.resolve(value.clone())?;
        if ty.is_project_type(self.context) {
            let (text, file) = assoc_fn_return(&ty, method, self.reference, self.context)?;
            return Some(Value::Written {
                text,
                self_ty: Some(ty),
                file: Origin::Project(file),
            });
        }
        if let Some(value) = self.foreign_method_value(&ty, method) {
            return Some(value);
        }
        let Value::Written {
            text,
            self_ty,
            file,
        } = value
        else {
            return None;
        };
        match adapt(&text, method)? {
            Adapted::Written(text) => Some(Value::Written {
                text,
                self_ty,
                file,
            }),
            // `parking_lot::Mutex<T>::lock()` hands back the guard itself.
            Adapted::Guarded(inner) if guards < MAX_GUARDS => {
                let inner = Value::Written {
                    text: inner,
                    self_ty,
                    file,
                };
                self.method_value(inner, method, guards + 1)
            }
            Adapted::Guarded(_) => None,
        }
    }

    /// What `value.field` holds: the field's declared type, on a project
    /// type (through a lock guard, too).
    fn field_value(&self, value: Value, field: &str) -> Option<Value> {
        // `pair.1` on a tuple type, `self.0` on a tuple struct.
        if let Ok(position) = field.parse::<usize>() {
            return self.tuple_element(value.clone(), position).or_else(|| {
                let ty = self.resolve(value)?;
                ty.is_project_type(self.context)
                    .then(|| self.variant_field(&ty.name, position))
                    .flatten()
            });
        }
        let value = match value {
            Value::Written {
                text,
                self_ty,
                file,
            } => Value::Written {
                text: guarded(&text).map_or(text.clone(), str::to_string),
                self_ty,
                file,
            },
            resolved @ Value::Resolved(_) => resolved,
        };
        let ty = self.resolve(value)?;
        if !ty.is_project_type(self.context) {
            return self.foreign_field_value(&ty, field);
        }
        let nodes = self.context.get_nodes_by_name(field);
        let declaration = ty.field(field, &nodes, self.reference)?;
        Some(Value::Written {
            text: declared_type(declaration, self.context)?,
            self_ty: Some(ty),
            file: Origin::Project(declaration.file_path.clone()),
        })
    }

    /// What `ty.method(..)` returns when `ty` is a type of a crate outside
    /// the project whose graph the external pass reaches: the return type
    /// that crate declares. `None` without such a graph (the in-project
    /// pass), or when the crate has no one such method.
    pub(super) fn foreign_method_value(&self, ty: &RustType, method: &str) -> Option<Value> {
        let krate = ty.external_crate()?;
        let found =
            self.context
                .foreign_types()?
                .method_return(krate, ty.external_path(), method)?;
        Some(Value::Written {
            text: found.text,
            self_ty: Some(ty.clone()),
            file: Origin::Foreign {
                krate: found.krate,
                file: found.file,
            },
        })
    }

    /// The declared type of the public field `ty.field` of a crate outside
    /// the project (see [`Self::foreign_method_value`]).
    fn foreign_field_value(&self, ty: &RustType, field: &str) -> Option<Value> {
        let krate = ty.external_crate()?;
        let found = self
            .context
            .foreign_types()?
            .field_type(krate, ty.external_path(), field)?;
        Some(Value::Written {
            text: found.text,
            self_ty: Some(ty.clone()),
            file: Origin::Foreign {
                krate: found.krate,
                file: found.file,
            },
        })
    }

    /// What iterating `value` yields (`for node in &self.nodes`).
    pub(super) fn iterated_value(&self, value: Value) -> Option<Value> {
        let Value::Written {
            text,
            self_ty,
            file,
        } = value
        else {
            return None;
        };
        Some(Value::Written {
            text: item(&text)?,
            self_ty,
            file,
        })
    }

    /// Element `position` of a tuple value (`let (dir, handler) = setup();`).
    pub(super) fn tuple_element(&self, value: Value, position: usize) -> Option<Value> {
        let Value::Written {
            text,
            self_ty,
            file,
        } = value
        else {
            return None;
        };
        let inner = peeled(&text).strip_prefix('(')?.strip_suffix(')')?;
        let element = *split_top_level(inner, b',').get(position)?;
        (!element.is_empty()).then(|| Value::Written {
            text: element.to_string(),
            self_ty,
            file,
        })
    }

    /// The type a chain ends on, or `None` when it names a generic
    /// parameter (`T`) or an associated type (`Self::Item`) rather than a
    /// type: the value's real type is then not known.
    pub(super) fn resolve_link(&self, value: Value) -> Option<RustType> {
        if let Value::Written {
            text,
            self_ty,
            file,
        } = &value
        {
            let named = named_type(text)?;
            let generic = matches!(named, Named::Path(path) if names_generic(path));
            if generic && matches!(named, Named::Path(path) if path.contains("::")) {
                // `Self::Item`, `T::Output`.
                return None;
            }
            let ty = self.resolve_written(named, self_ty.as_ref(), file)?;
            // A one-letter struct (`struct S`) is a type all the same.
            return (!generic || ty.is_project_type(self.context)).then_some(ty);
        }
        self.resolve(value)
    }
}

/// `T`, `K2`, `Self::Item`, `T::Output`: a written path that names a
/// generic parameter or an associated type, not a type of its own.
fn names_generic(path: &str) -> bool {
    let (path, _) = split_path(peeled(path));
    let mut segments = path.split("::");
    let first = segments.next().unwrap_or_default();
    // `T`, `K2`: one capital, maybe a digit (`IO`, `DB` are types).
    let parameter = |segment: &str| match segment.as_bytes() {
        [letter] => letter.is_ascii_uppercase(),
        [letter, digit] => letter.is_ascii_uppercase() && digit.is_ascii_digit(),
        _ => false,
    };
    match segments.next() {
        None => parameter(first),
        Some(_) => first == "Self" || parameter(first),
    }
}

#[cfg(test)]
mod tests {
    use super::names_generic;

    #[test]
    fn generic_parameters_and_associated_types_are_not_types() {
        for path in ["T", "K2", "Self::Item", "T::Output"] {
            assert!(names_generic(path), "{path}");
        }
        for path in ["Graph", "IO", "crate::Graph", "io::Result"] {
            assert!(!names_generic(path), "{path}");
        }
    }
}
