use std::collections::HashSet;
use std::sync::LazyLock;

pub(in crate::resolution::resolver) static PYTHON_BUILT_INS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        HashSet::from([
            "print",
            "len",
            "range",
            "str",
            "int",
            "float",
            "list",
            "dict",
            "set",
            "tuple",
            "open",
            "input",
            "type",
            "isinstance",
            "hasattr",
            "getattr",
            "setattr",
            "super",
            "self",
            "cls",
            "None",
            "True",
            "False",
        ])
    });

pub(in crate::resolution::resolver) static PYTHON_BUILT_IN_TYPES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        HashSet::from([
            "list",
            "dict",
            "set",
            "tuple",
            "str",
            "int",
            "float",
            "bool",
            "bytes",
            "bytearray",
            "frozenset",
            "object",
            "super",
        ])
    });

pub(in crate::resolution::resolver) static PYTHON_BUILT_IN_METHODS: LazyLock<
    HashSet<&'static str>,
> = LazyLock::new(|| {
    HashSet::from([
        "append",
        "extend",
        "insert",
        "remove",
        "pop",
        "clear",
        "sort",
        "reverse",
        "copy",
        "update",
        "keys",
        "values",
        "items",
        "get",
        "add",
        "discard",
        "union",
        "intersection",
        "difference",
        "split",
        "join",
        "strip",
        "lstrip",
        "rstrip",
        "replace",
        "lower",
        "upper",
        "startswith",
        "endswith",
        "find",
        "index",
        "count",
        "encode",
        "decode",
        "format",
        "isdigit",
        "isalpha",
        "isalnum",
        "read",
        "write",
        "readline",
        "readlines",
        "close",
        "flush",
        "seek",
    ])
});
