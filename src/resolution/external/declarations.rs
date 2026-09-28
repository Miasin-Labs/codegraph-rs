//! [`ForeignTypes`] over the reachable graphs: what a dependency declares a
//! method, fn or field to be, and what a type it writes names.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::open::GraphCache;
use super::rust::api::Caller;
use super::rust::lookup::{lookup_field, lookup_method, lookup_path, type_path};
use super::rust::scope::TraitScope;
use crate::resolution::name_matcher::external::{field_declared_type, signature_return};
use crate::resolution::types::ResolutionContext;
use crate::resolution::{ForeignText, ForeignTypes};
use crate::types::{EdgeKind, NodeKind};

/// Declarations of the reachable graphs, counting how many answers chain
/// inference took from them.
pub(crate) struct Declarations<'a> {
    cache: &'a GraphCache<'a>,
    answers: Cell<usize>,
    /// The project the references come from.
    project: &'a dyn ResolutionContext,
    /// The file of the reference being resolved, and its traits in scope
    /// (a method on a chain's type is probed as that file sees it).
    current: RefCell<Option<(String, Rc<TraitScope>)>>,
    /// The line of the reference being resolved.
    line: Cell<u32>,
}

impl<'a> Declarations<'a> {
    pub(crate) fn new(cache: &'a GraphCache<'a>, project: &'a dyn ResolutionContext) -> Self {
        Declarations {
            cache,
            answers: Cell::new(0),
            project,
            current: RefCell::new(None),
            line: Cell::new(0),
        }
    }

    pub(crate) fn cache(&self) -> &'a GraphCache<'a> {
        self.cache
    }

    /// Answers given so far: a count that moved during one inference means
    /// the type came through a dependency's declaration.
    pub(crate) fn answers(&self) -> usize {
        self.answers.get()
    }

    /// Resolve what follows as written in `file` at `line`.
    pub(crate) fn enter(&self, file: &str, line: u32) {
        self.line.set(line);
        if self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|(current, _)| current == file)
        {
            return;
        }
        let known = self.cache.memo.api_scopes.borrow().get(file).cloned();
        let scope = known.unwrap_or_else(|| {
            let scope = Rc::new(TraitScope::of_file(self.cache, self.project, file));
            self.cache
                .memo
                .api_scopes
                .borrow_mut()
                .insert(file.to_string(), Rc::clone(&scope));
            scope
        });
        *self.current.borrow_mut() = Some((file.to_string(), scope));
    }

    /// Run `f` with the current reference's view of traits.
    pub(crate) fn with_caller<T>(&self, f: impl FnOnce(Option<&Caller<'_>>) -> T) -> T {
        let current = self.current.borrow().clone();
        match current {
            Some((file, scope)) => f(Some(&Caller {
                scope: &scope,
                project: self.project,
                file: &file,
                line: self.line.get(),
            })),
            None => f(None),
        }
    }

    fn answered(&self, text: ForeignText) -> Option<ForeignText> {
        self.answers.set(self.answers.get() + 1);
        Some(text)
    }
}

/// `text` (a written type) names a generic parameter: a one-letter type
/// name (`T`, `E`, `K`, `V`, `A`, `T1`).
fn mentions_generic(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| {
            let mut chars = word.chars();
            chars.next().is_some_and(|first| first.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_digit())
        })
}

/// The std types receiver inference types methods of itself, generic
/// arguments carried (its table of wrappers, containers and iterators).
const ADAPTED: &[&str] = &[
    "Option",
    "Result",
    "LockResult",
    "TryLockResult",
    "RefCell",
    "Cell",
    "Mutex",
    "RwLock",
    "OnceCell",
    "OnceLock",
    "Vec",
    "VecDeque",
    "LinkedList",
    "BinaryHeap",
    "HashSet",
    "BTreeSet",
    "HashMap",
    "BTreeMap",
    "Entry",
    "Iter",
    "IterMut",
    "IntoIter",
    "Values",
    "Keys",
    "slice",
    "[T]",
];

/// The methods that table types, generic arguments carried (a map's
/// `entry` is not one: the table writes every map's entry as `HashMap`'s,
/// which the external pass takes for no type at all).
const ADAPTED_METHODS: &[&str] = &[
    "as_deref",
    "as_deref_mut",
    "as_mut",
    "as_mut_slice",
    "as_ref",
    "as_slice",
    "back",
    "back_mut",
    "borrow",
    "borrow_mut",
    "by_ref",
    "cloned",
    "copied",
    "cycle",
    "drain",
    "enumerate",
    "expect",
    "filter",
    "find",
    "first",
    "first_mut",
    "front",
    "front_mut",
    "fuse",
    "get",
    "get_mut",
    "get_mut_or_init",
    "get_or_init",
    "get_or_insert",
    "get_or_insert_default",
    "get_or_insert_with",
    "get_or_try_init",
    "insert",
    "inspect",
    "inspect_err",
    "into_inner",
    "into_iter",
    "into_keys",
    "into_sorted_vec",
    "into_values",
    "iter",
    "iter_mut",
    "keys",
    "last",
    "last_mut",
    "lock",
    "make_contiguous",
    "map_err",
    "max",
    "max_by",
    "max_by_key",
    "min",
    "min_by",
    "min_by_key",
    "next",
    "next_back",
    "nth",
    "nth_back",
    "ok",
    "ok_or",
    "ok_or_else",
    "or",
    "or_default",
    "or_else",
    "or_insert",
    "or_insert_with",
    "or_insert_with_key",
    "peek",
    "peek_mut",
    "peekable",
    "pop",
    "pop_back",
    "pop_first",
    "pop_front",
    "pop_last",
    "read",
    "remove",
    "replace",
    "rev",
    "rfind",
    "skip",
    "skip_while",
    "step_by",
    "swap_remove",
    "take",
    "take_if",
    "take_while",
    "try_borrow",
    "try_borrow_mut",
    "try_lock",
    "try_read",
    "try_write",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "unwrap_unchecked",
    "values",
    "values_mut",
    "write",
    "xor",
];

/// `text` is a generic parameter itself (`T`, `&mut T`).
fn bare_generic(text: &str) -> bool {
    let peeled = text
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();
    mentions_generic(peeled) && !peeled.contains(['<', ':', '('])
}

impl ForeignTypes for Declarations<'_> {
    fn method_return(&self, krate: &str, owner: &[String], method: &str) -> Option<ForeignText> {
        let graph = self.cache.get(krate)?;
        // The method may live in another crate (a re-exported type, an
        // alias, a `Deref` target): its return type is written there.
        let (found_in, node) =
            self.with_caller(|caller| lookup_method(self.cache, &graph, owner, method, caller))?;
        let text = signature_return(node.signature.as_deref()?)?.to_string();
        // A toolchain method's return written with its generic parameters
        // ends a chain — inference substitutes none — where its own table of
        // std wrappers and containers carries on (`Option::as_mut` of
        // `Option<Heap>` is `Option<&mut Heap>`, `Vec<String>::iter()` yields
        // `String`s): leave those to it, and a bare `T` to nobody.
        // `NonNull::cast`'s `NonNull<U>` is still a `NonNull`.
        let adapted = owner
            .last()
            .is_some_and(|owner| ADAPTED.contains(&owner.as_str()))
            && ADAPTED_METHODS.contains(&method);
        if !found_in.crate_dir.is_empty()
            && (bare_generic(&text) || (adapted && mentions_generic(&text)))
        {
            return None;
        }
        self.answered(ForeignText {
            text,
            krate: found_in.krate.clone(),
            file: node.file_path,
        })
    }

    fn fn_return(&self, krate: &str, path: &[String]) -> Option<ForeignText> {
        let found = lookup_path(self.cache, krate, path, EdgeKind::Calls)?;
        let text = match found.node.kind {
            NodeKind::Function | NodeKind::Method => {
                signature_return(found.node.signature.as_deref()?)?.to_string()
            }
            // A tuple struct's constructor makes the struct.
            NodeKind::Struct => found.node.name.clone(),
            _ => return None,
        };
        self.answered(ForeignText {
            text,
            krate: found.graph.krate.clone(),
            file: found.node.file_path,
        })
    }

    fn field_type(&self, krate: &str, owner: &[String], field: &str) -> Option<ForeignText> {
        let graph = self.cache.get(krate)?;
        let node = lookup_field(self.cache, &graph, owner.last()?, field)?;
        let text = field_declared_type(&node, &graph.context)?;
        self.answered(ForeignText {
            text,
            krate: krate.to_string(),
            file: node.file_path,
        })
    }

    fn resolve_type(&self, krate: &str, file: &str, path: &str) -> Option<(String, Vec<String>)> {
        type_path(self.cache, krate, file, path)
    }
}

#[cfg(test)]
mod tests {
    use super::{bare_generic, mentions_generic};

    #[test]
    fn generic_parameters_are_one_letter_names() {
        assert!(mentions_generic("Option<&mut T>"));
        assert!(mentions_generic("Result<T, E>"));
        assert!(mentions_generic("Iter<'_, K, V>"));
        assert!(!mentions_generic("io::Result<File>"));
        assert!(!mentions_generic("&str"));
        assert!(!mentions_generic("Option<Duration>"));
    }

    #[test]
    fn a_bare_generic_parameter_is_no_type() {
        assert!(bare_generic("T"));
        assert!(bare_generic("&mut T"));
        assert!(!bare_generic("Option<&mut T>"));
        assert!(!bare_generic("NonNull<U>"));
        assert!(!bare_generic("File"));
    }
}
