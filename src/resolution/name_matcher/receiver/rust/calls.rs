//! The type a call returns: a tuple struct or variant constructor, an
//! associated fn's or free fn's indexed signature, a smart pointer or
//! cell's constructor seen through to what it holds.

use super::lookup::{RustType, assoc_fn_return, external_path, free_fn_return, resolve_type};
use super::types::{is_deref_wrapper, starts_uppercase};
use super::{Inference, Origin, Site, Value};

/// Std cells and locks whose `new(x)` holds `x` (see `adaptors`).
const CELLS: &[&str] = &["Cell", "Mutex", "OnceCell", "RefCell", "RwLock"];

/// Std traits called as `Trait::f(..)`: the type is whatever `Self` is.
const STD_TRAITS: &[&str] = &[
    "Clone",
    "Default",
    "From",
    "FromIterator",
    "FromStr",
    "Into",
    "TryFrom",
    "TryInto",
];

impl Inference<'_> {
    /// The type a call of `path` returns.
    pub(super) fn call_value(
        &self,
        path: &[&str],
        args: &str,
        site: Site,
        depth: u8,
    ) -> Option<Value> {
        let (&callee, qualifier) = path.split_last()?;
        let Some(&owner) = qualifier.last() else {
            return match callee {
                "Some" => Some(Value::Resolved(RustType::external("Option"))),
                "Ok" | "Err" => Some(Value::Resolved(RustType::external("Result"))),
                _ if starts_uppercase(callee) => Some(self.here(callee)),
                _ => self.free_fn_value(callee),
            };
        };
        if starts_uppercase(callee) {
            // A tuple struct or tuple variant: `Kind::Leaf(..)`, `m::Id(..)`.
            let named = if starts_uppercase(owner) {
                qualifier
            } else {
                path
            };
            return Some(self.here(named.join("::")));
        }
        if !starts_uppercase(owner) {
            return self.free_fn_value(&path.join("::"));
        }
        let owner_ty = match owner {
            "Self" => self.owner.clone()?,
            _ => resolve_type(
                &qualifier.join("::"),
                &self.reference.file_path,
                self.reference,
                self.context,
            ),
        };
        if !owner_ty.is_project_type(self.context) {
            // `Connection::open(p)`: what the dependency declares, when its
            // graph is reachable.
            if let Some(value) = self.foreign_method_value(&owner_ty, callee) {
                return Some(value);
            }
            if is_deref_wrapper(&owner_ty.name) {
                // `Arc::new(Graph::new())` derefs to what it wraps.
                return matches!(callee, "new" | "from" | "clone" | "pin")
                    .then(|| self.expression_value(args, site, depth + 1))
                    .flatten();
            }
            if callee == "new" && CELLS.contains(&owner_ty.name.as_str()) {
                // `RefCell::new(Cache::new())` holds what it wraps.
                return match self.expression_value(args, site, depth + 1) {
                    Some(Value::Written {
                        text,
                        self_ty,
                        file,
                    }) => Some(Value::Written {
                        text: format!("{}<{text}>", owner_ty.name),
                        self_ty,
                        file,
                    }),
                    _ => Some(Value::Resolved(owner_ty)),
                };
            }
            if STD_TRAITS.contains(&owner_ty.name.as_str()) {
                return None;
            }
            // An external type's associated fn: the type itself (`HashMap::new`).
            return Some(Value::Resolved(owner_ty));
        }
        if let Some((text, file)) = assoc_fn_return(&owner_ty, callee, self.reference, self.context)
        {
            return Some(Value::Written {
                text,
                self_ty: Some(owner_ty),
                file: Origin::Project(file),
            });
        }
        // Trait constructors a derive or blanket impl may supply.
        match callee {
            "default" | "clone" | "from" => Some(Value::Resolved(owner_ty)),
            "try_from" | "from_str" => Some(Value::Written {
                text: "Result<Self>".to_string(),
                self_ty: Some(owner_ty),
                file: Origin::Project(self.reference.file_path.clone()),
            }),
            _ => None,
        }
    }

    fn free_fn_value(&self, path: &str) -> Option<Value> {
        let Some((text, file)) = free_fn_return(path, self.reference, self.context) else {
            return self.foreign_fn_value(path);
        };
        Some(Value::Written {
            text,
            self_ty: None,
            file: Origin::Project(file),
        })
    }

    /// `serde_json::from_str(..)`, `tempdir()` after `use tempfile::tempdir`:
    /// the return type a reachable crate outside the project declares.
    fn foreign_fn_value(&self, path: &str) -> Option<Value> {
        let foreign = self.context.foreign_types()?;
        let (krate, rest) = external_path(path, &self.reference.file_path, self.context)?;
        let found = foreign.fn_return(&krate, &rest)?;
        Some(Value::Written {
            text: found.text,
            self_ty: None,
            file: Origin::Foreign {
                krate: found.krate,
                file: found.file,
            },
        })
    }
}
