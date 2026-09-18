//! What the std (and common crates') wrappers, containers, and adaptors
//! return, as written type text, so a chain can be followed through them:
//!
//! - `RefCell<T>::borrow_mut()` and `Cell<T>::get()` are `T`;
//! - `Mutex<T>::lock()` and `RwLock<T>::read()`/`write()` are a
//!   `LockResult<T>`: `unwrap`/`expect`/`?` open it (std), and any other
//!   method runs on `T` (a `parking_lot` or `tokio` guard comes back bare);
//! - `Option`/`Result` adaptors keep or convert the wrapper (`as_ref`,
//!   `ok`, `ok_or`, `context`);
//! - `Vec<T>::get(..)`/`first()`/`pop()` are `Option<T>`,
//!   `HashMap<K, V>::get(..)` is `Option<V>`, and `entry(..).or_default()`
//!   is `V`;
//! - `iter()`/`values()` are an iterator whose `next()`/`find(..)`/`last()`
//!   are `Option<T>`, as is a signature's `impl Iterator<Item = T>`.
//!
//! The caller asks only about types the project does not define (a project
//! `Cell` runs its own methods). A method this table does not list returns
//! a type it does not know.

use super::types::{peeled, split_path, split_top_level, type_args};

/// The iterator every container's `iter()` is written as here: a std path,
/// so it never names a project type.
const ITER: &str = "std::slice::Iter";
/// A map entry, written under a std path for the same reason.
const ENTRY: &str = "std::collections::hash_map::Entry";

/// What a method call on a wrapper or container returns.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Adapted {
    /// This written type.
    Written(String),
    /// The call runs on the value a lock guards, written so.
    Guarded(String),
}

/// What calling `method` on a value of the external type `written` returns.
pub(super) fn adapt(written: &str, method: &str) -> Option<Adapted> {
    let text = peeled(written);
    if let Some(element) = slice_element(text) {
        return container(&[element], method, Kind::Sequence).map(Adapted::Written);
    }
    let (path, args) = split_path(text);
    let args = type_args(&args);
    let name = path.rsplit("::").next()?;
    let written = |text: String| Some(Adapted::Written(text));
    match name {
        "Option" => option(args.first()?, method).and_then(written),
        "Result" => result(args.first()?, method).and_then(written),
        "LockResult" | "TryLockResult" => Some(Adapted::Guarded(args.first()?.to_string())),
        "RefCell" => ref_cell(args.first()?, method).and_then(written),
        "Cell" => matches!(
            method,
            "get" | "take" | "into_inner" | "replace" | "get_mut"
        )
        .then(|| args.first().map(|inner| inner.to_string()))
        .flatten()
        .and_then(written),
        "Mutex" | "RwLock" | "ReentrantMutex" | "FairMutex" => {
            lock(args.first()?, method).and_then(written)
        }
        "OnceCell" | "OnceLock" => once(args.first()?, method).and_then(written),
        "Vec" | "VecDeque" | "SmallVec" | "ArrayVec" | "ThinVec" | "LinkedList" | "BinaryHeap" => {
            container(&args, method, Kind::Sequence).and_then(written)
        }
        "HashSet" | "BTreeSet" | "IndexSet" | "FxHashSet" | "AHashSet" => {
            container(&args, method, Kind::Set).and_then(written)
        }
        "HashMap" | "BTreeMap" | "IndexMap" | "FxHashMap" | "AHashMap" | "FnvHashMap" => {
            map(&args, method).and_then(written)
        }
        "Entry" if path == ENTRY => entry(&args, method).and_then(written),
        _ => iterator_item(name, &args)
            .and_then(|item| iterator(text, item, method))
            .and_then(written),
    }
}

/// The value a lock result guards: `T` of `LockResult<T>`.
pub(super) fn guarded(written: &str) -> Option<&str> {
    let (path, args) = split_path(peeled(written));
    let name = path.rsplit("::").next()?;
    matches!(name, "LockResult" | "TryLockResult")
        .then(|| type_args(&args).first().copied())
        .flatten()
}

/// What `value[index]` holds for a value of written type `written`: a
/// slice's, `Vec`'s, or `VecDeque`'s element, a map's value.
pub(super) fn indexed(written: &str) -> Option<String> {
    let text = peeled(written);
    if let Some(element) = slice_element(text) {
        return Some(element.to_string());
    }
    let (path, args) = split_path(text);
    let args = type_args(&args);
    let element = match path.rsplit("::").next()? {
        "Vec" | "VecDeque" | "SmallVec" | "ArrayVec" | "ThinVec" => args.first(),
        "HashMap" | "BTreeMap" | "IndexMap" | "FxHashMap" | "AHashMap" | "FnvHashMap" => {
            args.get(1)
        }
        _ => None,
    };
    element.map(|element| element.to_string())
}

/// What iterating a value of written type `written` yields (`for x in v`):
/// a container's element, a map's `(key, value)`, an iterator's item.
pub(super) fn item(written: &str) -> Option<String> {
    let text = peeled(written);
    if let Some(element) = slice_element(text) {
        return Some(element.to_string());
    }
    let (path, args) = split_path(text);
    let args = type_args(&args);
    let name = path.rsplit("::").next()?;
    match name {
        "Vec" | "VecDeque" | "SmallVec" | "ArrayVec" | "ThinVec" | "LinkedList" | "BinaryHeap"
        | "HashSet" | "BTreeSet" | "IndexSet" | "FxHashSet" | "AHashSet" | "Option" | "Result" => {
            args.first().map(|element| element.to_string())
        }
        "HashMap" | "BTreeMap" | "IndexMap" | "FxHashMap" | "AHashMap" | "FnvHashMap" => {
            Some(format!("({}, {})", args.first()?, args.get(1)?))
        }
        _ => iterator_item(name, &args).map(str::to_string),
    }
}

/// `[T]` or `[T; N]`: `T`.
fn slice_element(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    let element = split_top_level(inner, b';').into_iter().next()?;
    (!element.is_empty()).then_some(element)
}

fn option(inner: &str, method: &str) -> Option<String> {
    Some(match method {
        "as_ref" | "as_mut" | "as_deref" | "as_deref_mut" | "cloned" | "copied" | "take" | "or"
        | "or_else" | "xor" | "filter" | "inspect" | "replace" | "take_if" => {
            format!("Option<{inner}>")
        }
        "insert"
        | "get_or_insert"
        | "get_or_insert_with"
        | "get_or_insert_default"
        | "unwrap_unchecked" => inner.to_string(),
        "ok_or" | "ok_or_else" | "context" | "with_context" => format!("Result<{inner}>"),
        "iter" | "iter_mut" | "into_iter" => format!("{ITER}<{inner}>"),
        _ => return None,
    })
}

fn result(inner: &str, method: &str) -> Option<String> {
    Some(match method {
        "as_ref" | "as_mut" | "as_deref" | "as_deref_mut" | "map_err" | "or_else" | "inspect"
        | "inspect_err" | "cloned" | "copied" | "context" | "with_context" | "wrap_err"
        | "wrap_err_with" => format!("Result<{inner}>"),
        "ok" => format!("Option<{inner}>"),
        "unwrap_unchecked" => inner.to_string(),
        "iter" | "iter_mut" | "into_iter" => format!("{ITER}<{inner}>"),
        _ => return None,
    })
}

fn ref_cell(inner: &str, method: &str) -> Option<String> {
    Some(match method {
        "borrow" | "borrow_mut" | "get_mut" | "into_inner" | "take" | "replace" => {
            inner.to_string()
        }
        "try_borrow" | "try_borrow_mut" => format!("Result<{inner}>"),
        _ => return None,
    })
}

fn lock(inner: &str, method: &str) -> Option<String> {
    matches!(
        method,
        "lock"
            | "try_lock"
            | "read"
            | "write"
            | "try_read"
            | "try_write"
            | "blocking_lock"
            | "blocking_read"
            | "blocking_write"
            | "upgradable_read"
            | "lock_owned"
            | "read_owned"
            | "write_owned"
            | "get_mut"
            | "into_inner"
    )
    .then(|| format!("LockResult<{inner}>"))
}

fn once(inner: &str, method: &str) -> Option<String> {
    Some(match method {
        "get" | "get_mut" | "into_inner" | "take" => format!("Option<{inner}>"),
        "get_or_init" | "get_mut_or_init" => inner.to_string(),
        "get_or_try_init" => format!("Result<{inner}>"),
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sequence,
    Set,
}

fn container(args: &[&str], method: &str, kind: Kind) -> Option<String> {
    let element = *args.first()?;
    Some(match method {
        "get" | "get_mut" | "first" | "last" | "first_mut" | "last_mut" | "pop" | "pop_front"
        | "pop_back" | "front" | "back" | "front_mut" | "back_mut" | "peek" | "peek_mut"
        | "pop_first" | "pop_last" | "get_index" | "take" => format!("Option<{element}>"),
        "remove" | "swap_remove" if kind == Kind::Sequence => element.to_string(),
        "iter" | "iter_mut" | "into_iter" | "drain" | "into_sorted_vec" => {
            format!("{ITER}<{element}>")
        }
        "as_slice" | "as_mut_slice" | "make_contiguous" => format!("[{element}]"),
        _ => return None,
    })
}

fn map(args: &[&str], method: &str) -> Option<String> {
    let (key, value) = (*args.first()?, *args.get(1)?);
    Some(match method {
        "get" | "get_mut" | "remove" | "swap_remove" | "shift_remove" => {
            format!("Option<{value}>")
        }
        "values" | "values_mut" | "into_values" => format!("{ITER}<{value}>"),
        "keys" | "into_keys" => format!("{ITER}<{key}>"),
        "iter" | "iter_mut" | "into_iter" | "drain" => format!("{ITER}<({key}, {value})>"),
        "entry" => format!("{ENTRY}<{key}, {value}>"),
        _ => return None,
    })
}

fn entry(args: &[&str], method: &str) -> Option<String> {
    let value = *args.get(1)?;
    Some(match method {
        "or_insert" | "or_insert_with" | "or_default" | "or_insert_with_key" => value.to_string(),
        "and_modify" => format!("{ENTRY}<{}, {value}>", args.first()?),
        _ => return None,
    })
}

/// The item type of an iterator written `name<args>`: the `Item = T` of
/// `impl Iterator<Item = T>`, or the element of a std iterator.
fn iterator_item<'a>(name: &str, args: &[&'a str]) -> Option<&'a str> {
    match name {
        "Iterator" | "DoubleEndedIterator" | "ExactSizeIterator" | "IntoIterator" => args
            .iter()
            .find_map(|arg| arg.strip_prefix("Item")?.trim_start().strip_prefix('='))
            .map(str::trim),
        "Iter" | "IterMut" | "IntoIter" | "Drain" | "Keys" | "Peekable" | "Rev" | "Skip"
        | "Take" | "Fuse" | "StepBy" | "Cloned" | "Copied" => args.first().copied(),
        "Values" | "ValuesMut" | "IntoValues" => args.last().copied(),
        _ => None,
    }
}

/// An iterator adaptor's result: the same iterator, or its next item.
fn iterator(written: &str, item: &str, method: &str) -> Option<String> {
    Some(match method {
        "next" | "last" | "nth" | "find" | "min" | "max" | "min_by" | "max_by" | "min_by_key"
        | "max_by_key" | "next_back" | "nth_back" | "rfind" | "peek" | "peek_mut" => {
            format!("Option<{item}>")
        }
        "filter" | "rev" | "skip" | "take" | "skip_while" | "take_while" | "peekable" | "fuse"
        | "inspect" | "by_ref" | "step_by" | "cycle" | "into_iter" => written.to_string(),
        "cloned" | "copied" => format!("{ITER}<{item}>"),
        "enumerate" => format!("{ITER}<(usize, {item})>"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Adapted, adapt};

    fn written(text: &str, method: &str) -> Option<String> {
        match adapt(text, method)? {
            Adapted::Written(text) => Some(text),
            Adapted::Guarded(_) => None,
        }
    }

    #[test]
    fn wrappers_open_to_what_they_hold() {
        assert_eq!(
            written("Rc<RefCell<Cache>>", "borrow_mut").unwrap(),
            "Cache"
        );
        assert_eq!(
            written("&Arc<Mutex<Graph>>", "lock").unwrap(),
            "LockResult<Graph>"
        );
        assert_eq!(
            adapt("LockResult<Graph>", "get"),
            Some(Adapted::Guarded("Graph".into()))
        );
        assert_eq!(written("Cell<Mode>", "get").unwrap(), "Mode");
        assert_eq!(
            written("OnceLock<Registry>", "get").unwrap(),
            "Option<Registry>"
        );
        assert_eq!(
            written("OnceLock<Registry>", "get_or_init").unwrap(),
            "Registry"
        );
    }

    #[test]
    fn option_and_result_adaptors_keep_or_convert_the_wrapper() {
        assert_eq!(written("Option<Node>", "as_ref").unwrap(), "Option<Node>");
        assert_eq!(written("Option<&Node>", "ok_or").unwrap(), "Result<&Node>");
        assert_eq!(written("Result<Node, E>", "ok").unwrap(), "Option<Node>");
        assert_eq!(written("Option<Node>", "map"), None);
    }

    #[test]
    fn containers_hand_out_their_elements() {
        assert_eq!(written("Vec<Node>", "first").unwrap(), "Option<Node>");
        assert_eq!(written("&[Node]", "last").unwrap(), "Option<Node>");
        assert_eq!(written("Vec<Node>", "remove").unwrap(), "Node");
        assert_eq!(
            written("HashMap<String, Graph>", "get").unwrap(),
            "Option<Graph>"
        );
        let entry = written("BTreeMap<u32, Vec<Edge>>", "entry").unwrap();
        assert_eq!(written(&entry, "or_default").unwrap(), "Vec<Edge>");
        assert_eq!(written("HashSet<Node>", "remove"), None);
    }

    #[test]
    fn iterating_yields_elements_entries_and_items() {
        assert_eq!(super::item("&Vec<Node>").unwrap(), "Node");
        assert_eq!(super::item("&[Edge]").unwrap(), "Edge");
        assert_eq!(
            super::item("BTreeMap<String, Graph>").unwrap(),
            "(String, Graph)"
        );
        let enumerated = written("Vec<Node>", "iter").unwrap();
        let enumerated = written(&enumerated, "enumerate").unwrap();
        assert_eq!(super::item(&enumerated).unwrap(), "(usize, Node)");
        assert_eq!(super::item("String"), None);
    }

    #[test]
    fn iterators_yield_their_items() {
        let values = written("IndexMap<String, Node>", "values").unwrap();
        let filtered = written(&values, "filter").unwrap();
        assert_eq!(written(&filtered, "next").unwrap(), "Option<Node>");
        assert_eq!(
            written("impl Iterator<Item = &Node> + '_", "find").unwrap(),
            "Option<&Node>"
        );
        assert_eq!(written(&values, "map"), None);
    }
}
