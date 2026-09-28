//! Per-language library models for taint propagation: how data moves
//! through common library calls the analysis cannot see into. A deliberately
//! small, declarative table — string building and containers — until
//! interprocedural summaries replace it.
//!
//! Every call not named here propagates by the default rule of
//! [`crate::taint_flow`]: its result carries the data of its receiver and
//! arguments, and it writes nothing. Concatenation (`+`, `.`, `+=`),
//! f-strings and template literals are IR operations, so they need no
//! entry. Methods are matched by their last name (`sb.append`,
//! `java.lang.StringBuilder::append` → `append`).

/// How one language's library calls move data.
pub struct PropagationRules {
    /// Methods that store their arguments' data in their receiver
    /// (`sb.append(x)`, `list.add(x)`).
    pub receiver_from_args: &'static [&'static str],
    /// Keyed writes: `(method, key argument, value argument)` —
    /// `map.put("k", v)` stores `v` under `map["k"]` when the key is a
    /// constant, else in the whole `map`.
    pub keyed_writes: &'static [(&'static str, usize, usize)],
    /// Keyed reads: `(method, key argument)` — `map.get("k")` reads
    /// `map["k"]` when the key is a constant.
    pub keyed_reads: &'static [(&'static str, usize)],
    /// Functions that write data into an argument:
    /// `(function, destination argument, first source argument)` —
    /// `strcpy(dst, src)`, `sprintf(dst, fmt, …)` (every argument from the
    /// first source on flows into the destination).
    pub argument_writes: &'static [(&'static str, usize, usize)],
    /// Calls whose result carries none of their inputs' data: lengths,
    /// comparisons, predicates.
    pub clean_results: &'static [&'static str],
    /// Calls whose result is their receiver (or first argument) itself,
    /// wrapped, borrowed or copied (`Arc::new(s)`, `x.clone()`,
    /// `m.lock()`): a field read of the result reads that field of it.
    pub projections: &'static [&'static str],
    /// Methods whose result carries their receiver's data alone: an error
    /// mapper's argument builds the error, never the value
    /// (`x.ok_or_else(|| NotFound(id))?` is `x`'s data).
    pub receiver_results: &'static [&'static str],
    /// Position-aware lists: a local list built by `new <type>()` and only
    /// appended to, removed from and read at constant positions in one
    /// straight run of code keeps its elements apart (`add(a); add(b);
    /// remove(0); get(0)` reads `b`).
    pub lists: &'static ListRules,
}

/// The list methods [`PropagationRules::lists`] follows.
pub struct ListRules {
    /// Constructed types (last name, generics dropped) that are lists.
    pub types: &'static [&'static str],
    /// `list.m(x)` appends `x`.
    pub appends: &'static [&'static str],
    /// `list.m(i)` removes the element at constant position `i`.
    pub removes_at: &'static [&'static str],
    /// `list.m()` removes the first element.
    pub removes_first: &'static [&'static str],
    /// `list.m(i)` reads the element at constant position `i`.
    pub reads_at: &'static [&'static str],
    /// `list.m()` reads the first element.
    pub reads_first: &'static [&'static str],
}

static NO_LISTS: ListRules = ListRules {
    types: &[],
    appends: &[],
    removes_at: &[],
    removes_first: &[],
    reads_at: &[],
    reads_first: &[],
};

static JAVA_LISTS: ListRules = ListRules {
    types: &[
        "ArrayList",
        "LinkedList",
        "Vector",
        "ArrayDeque",
        "CopyOnWriteArrayList",
    ],
    appends: &["add", "addLast", "addElement", "offer", "offerLast"],
    removes_at: &["remove"],
    removes_first: &["removeFirst", "poll", "pollFirst", "pop", "remove"],
    reads_at: &["get", "elementAt"],
    reads_first: &["getFirst", "peek", "peekFirst", "element", "firstElement"],
};

impl PropagationRules {
    pub fn for_language(lang: &str) -> &'static PropagationRules {
        match lang {
            "java" => &JAVA,
            "c" | "cpp" => &C,
            "python" => &PYTHON,
            "javascript" | "typescript" | "tsx" | "jsx" => &JS,
            "php" => &PHP,
            "rust" => &RUST,
            _ => &NONE,
        }
    }

    pub fn is_receiver_write(&self, name: &str) -> bool {
        self.receiver_from_args.contains(&name)
    }

    pub fn keyed_write(&self, name: &str) -> Option<(usize, usize)> {
        self.keyed_writes
            .iter()
            .find(|(method, _, _)| *method == name)
            .map(|&(_, key, value)| (key, value))
    }

    pub fn keyed_read(&self, name: &str) -> Option<usize> {
        self.keyed_reads
            .iter()
            .find(|(method, _)| *method == name)
            .map(|&(_, key)| key)
    }

    pub fn argument_write(&self, name: &str) -> Option<(usize, usize)> {
        self.argument_writes
            .iter()
            .find(|(function, _, _)| *function == name)
            .map(|&(_, dst, src)| (dst, src))
    }

    pub fn is_clean_result(&self, name: &str) -> bool {
        self.clean_results.contains(&name)
    }

    pub fn is_projection(&self, name: &str) -> bool {
        self.projections.contains(&name)
    }

    pub fn is_receiver_result(&self, name: &str) -> bool {
        self.receiver_results.contains(&name)
    }
}

static NONE: PropagationRules = PropagationRules {
    receiver_from_args: &[],
    keyed_writes: &[],
    keyed_reads: &[],
    argument_writes: &[],
    clean_results: &[],
    projections: &[],
    receiver_results: &[],
    lists: &NO_LISTS,
};

static JAVA: PropagationRules = PropagationRules {
    receiver_from_args: &[
        "append",
        "insert",
        "add",
        "addAll",
        "addElement",
        "addFirst",
        "addLast",
        "push",
        "offer",
        "putAll",
        "write",
    ],
    keyed_writes: &[
        ("put", 0, 1),
        ("putIfAbsent", 0, 1),
        ("setProperty", 0, 1),
        ("set", 0, 1),
    ],
    keyed_reads: &[("get", 0), ("getProperty", 0), ("getOrDefault", 0)],
    argument_writes: &[("arraycopy", 2, 0), ("getChars", 2, 0)],
    clean_results: &[
        "length",
        "size",
        "isEmpty",
        "equals",
        "equalsIgnoreCase",
        "contains",
        "containsKey",
        "containsValue",
        "startsWith",
        "endsWith",
        "indexOf",
        "lastIndexOf",
        "hashCode",
        "compareTo",
        "compareToIgnoreCase",
        "matches",
        "hasNext",
        "hasMoreElements",
        "hasMoreTokens",
        "exists",
        "isFile",
        "isDirectory",
        "getClass",
        "countTokens",
    ],
    projections: &[],
    receiver_results: &[],
    lists: &JAVA_LISTS,
};

static C: PropagationRules = PropagationRules {
    receiver_from_args: &["append", "push_back", "insert", "assign"],
    keyed_writes: &[],
    keyed_reads: &[],
    argument_writes: &[
        ("strcpy", 0, 1),
        ("strncpy", 0, 1),
        ("strcat", 0, 1),
        ("strncat", 0, 1),
        ("strlcpy", 0, 1),
        ("strlcat", 0, 1),
        ("wcscpy", 0, 1),
        ("wcsncpy", 0, 1),
        ("wcscat", 0, 1),
        ("wcsncat", 0, 1),
        ("memcpy", 0, 1),
        ("memmove", 0, 1),
        ("wmemcpy", 0, 1),
        ("wmemmove", 0, 1),
        ("sprintf", 0, 1),
        ("vsprintf", 0, 1),
        ("swprintf", 0, 2),
        ("snprintf", 0, 2),
        ("vsnprintf", 0, 2),
        ("_snprintf", 0, 2),
        ("_snwprintf", 0, 2),
        ("sscanf", 2, 0),
        ("swscanf", 2, 0),
    ],
    clean_results: &[
        "strlen",
        "wcslen",
        "strnlen",
        "strcmp",
        "strncmp",
        "wcscmp",
        "memcmp",
        "strcasecmp",
        "isdigit",
        "isalpha",
        "isalnum",
        "isspace",
        "size",
        "length",
        "empty",
    ],
    projections: &[],
    receiver_results: &[],
    lists: &NO_LISTS,
};

static PYTHON: PropagationRules = PropagationRules {
    receiver_from_args: &["append", "extend", "insert", "add", "update", "write"],
    keyed_writes: &[("setdefault", 0, 1)],
    keyed_reads: &[("get", 0), ("getlist", 0)],
    argument_writes: &[],
    clean_results: &[
        "len",
        "isinstance",
        "startswith",
        "endswith",
        "isdigit",
        "isnumeric",
        "isalpha",
        "isalnum",
        "exists",
        "isfile",
        "isdir",
    ],
    projections: &[],
    receiver_results: &[],
    lists: &NO_LISTS,
};

static JS: PropagationRules = PropagationRules {
    receiver_from_args: &["push", "unshift", "append", "add", "write"],
    keyed_writes: &[("set", 0, 1), ("setItem", 0, 1)],
    keyed_reads: &[("get", 0), ("getItem", 0)],
    argument_writes: &[],
    clean_results: &[
        "includes",
        "indexOf",
        "lastIndexOf",
        "startsWith",
        "endsWith",
        "test",
        "has",
        "isArray",
        "isNaN",
        "existsSync",
    ],
    projections: &[],
    receiver_results: &[],
    lists: &NO_LISTS,
};

static PHP: PropagationRules = PropagationRules {
    receiver_from_args: &["append", "add", "push"],
    keyed_writes: &[],
    keyed_reads: &[],
    argument_writes: &[("array_push", 0, 1), ("array_unshift", 0, 1)],
    clean_results: &[
        "strlen",
        "count",
        "isset",
        "empty",
        "is_numeric",
        "is_int",
        "ctype_digit",
        "ctype_alpha",
        "ctype_alnum",
        "in_array",
        "array_key_exists",
        "file_exists",
        "is_file",
        "strcmp",
        "preg_match",
    ],
    projections: &[],
    receiver_results: &[],
    lists: &NO_LISTS,
};

/// Rust: `String`/`Vec` building, map and header lookups, predicates.
/// Methods by last name (macros too: `matches!` is `matches`).
static RUST: PropagationRules = PropagationRules {
    receiver_from_args: &[
        "push_str",
        "push",
        "extend",
        "extend_from_slice",
        "append",
        "insert_str",
        "write_all",
        "write_str",
        "push_back",
        "push_front",
    ],
    keyed_writes: &[("insert", 0, 1), ("append_pair", 0, 1)],
    keyed_reads: &[("get", 0), ("get_mut", 0), ("get_all", 0), ("remove", 0)],
    argument_writes: &[],
    clean_results: &[
        "len",
        "is_empty",
        "contains",
        "contains_key",
        "starts_with",
        "ends_with",
        "eq",
        "ne",
        "cmp",
        "partial_cmp",
        "is_some",
        "is_none",
        "is_ok",
        "is_err",
        "exists",
        "try_exists",
        "is_file",
        "is_dir",
        "is_absolute",
        "is_relative",
        "is_match",
        "matches",
        "any",
        "all",
        "count",
        "capacity",
        "is_ascii",
    ],
    projections: &[
        "new",
        "clone",
        "to_owned",
        "as_ref",
        "as_mut",
        "borrow",
        "borrow_mut",
        "deref",
        "lock",
        "unwrap",
        "expect",
        "unwrap_or_default",
        "into_inner",
        "get_ref",
        "Some",
        "Ok",
    ],
    receiver_results: &[
        "ok_or",
        "ok_or_else",
        "map_err",
        "context",
        "with_context",
        "wrap_err",
        "wrap_err_with",
        "inspect_err",
        "expect",
        "expect_err",
    ],
    lists: &NO_LISTS,
};
