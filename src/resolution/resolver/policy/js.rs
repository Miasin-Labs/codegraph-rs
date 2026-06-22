use std::collections::HashSet;
use std::sync::LazyLock;

pub(in crate::resolution::resolver) static JS_BUILT_INS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        HashSet::from([
            "console",
            "window",
            "document",
            "global",
            "process",
            "Promise",
            "Array",
            "Object",
            "String",
            "Number",
            "Boolean",
            "Date",
            "Math",
            "JSON",
            "RegExp",
            "Error",
            "Map",
            "Set",
            "setTimeout",
            "setInterval",
            "clearTimeout",
            "clearInterval",
            "fetch",
            "require",
            "module",
            "exports",
            "__dirname",
            "__filename",
        ])
    });

pub(in crate::resolution::resolver) static REACT_HOOKS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| {
        HashSet::from([
            "useState",
            "useEffect",
            "useContext",
            "useReducer",
            "useCallback",
            "useMemo",
            "useRef",
            "useLayoutEffect",
            "useImperativeHandle",
            "useDebugValue",
        ])
    });
