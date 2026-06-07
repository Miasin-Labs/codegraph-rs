//! Reference Resolution Orchestrator
//!
//! Coordinates all reference resolution strategies.
//! Ported from `src/resolution/index.ts` (the `ReferenceResolver` class +
//! `createResolver`; the `export * from './types'` re-export lives in
//! `resolution/mod.rs`).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::callback_synthesizer::synthesize_callback_edges;
use super::frameworks::detect_frameworks;
use super::go_module::load_go_module;
use super::import_resolver::{
    extract_import_mappings,
    extract_re_exports,
    load_cpp_include_dirs,
    resolve_jvm_import,
    resolve_via_import,
};
use super::lru_cache::LRUCache;
use super::path_aliases::load_project_aliases;
use super::types::{
    AliasMap,
    FrameworkResolver,
    GoModule,
    ImportMapping,
    ReExport,
    ResolutionContext,
    ResolutionResult,
    ResolutionStats,
    ResolvedRef,
    UnresolvedRef,
    WorkspacePackages,
};
use super::workspace_packages::load_workspace_packages;
use crate::db::{QueryBuilder, ResolvedRefKey};
use crate::error::{Result, log_debug, log_warn};
use crate::types::{Edge, EdgeKind, Language, Metadata, Node, NodeKind, UnresolvedReference};

/// Cache size limits. Each per-resolver cache is bounded so memory
/// stays flat on large codebases (20k+ files). Sizes were chosen to
/// cover the working set for typical resolution batches without
/// exceeding a few hundred MB worst-case. Override via the env var
/// `CODEGRAPH_RESOLVER_CACHE_SIZE` (single integer applied to all
/// caches) when tuning for very large or very small projects.
const DEFAULT_CACHE_LIMIT: usize = 5_000;

/// Mirrors JS `Number.parseInt(raw, 10)`: skip leading whitespace, allow an
/// optional sign, parse the leading run of decimal digits, NaN otherwise.
fn parse_int_prefix(raw: &str) -> Option<i64> {
    let s = raw.trim_start();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|n| sign * n)
}

fn resolve_cache_limit() -> usize {
    let raw = match std::env::var("CODEGRAPH_RESOLVER_CACHE_SIZE") {
        Ok(v) => v,
        Err(_) => return DEFAULT_CACHE_LIMIT,
    };
    if raw.is_empty() {
        // JS `if (!raw)` — empty string is falsy.
        return DEFAULT_CACHE_LIMIT;
    }
    match parse_int_prefix(&raw) {
        Some(parsed) if parsed > 0 => parsed as usize,
        _ => DEFAULT_CACHE_LIMIT,
    }
}

// Pre-built sets for O(1) built-in lookups (allocated once, shared across all instances)
static JS_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

static REACT_HOOKS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

static PYTHON_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

static PYTHON_BUILT_IN_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

static PYTHON_BUILT_IN_METHODS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
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

static GO_STDLIB_PACKAGES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "fmt",
        "os",
        "io",
        "net",
        "http",
        "log",
        "math",
        "sort",
        "sync",
        "time",
        "path",
        "bytes",
        "strings",
        "strconv",
        "errors",
        "context",
        "json",
        "xml",
        "csv",
        "html",
        "template",
        "regexp",
        "reflect",
        "runtime",
        "testing",
        "flag",
        "bufio",
        "crypto",
        "encoding",
        "filepath",
        "hash",
        "mime",
        "rand",
        "signal",
        "sql",
        "syscall",
        "unicode",
        "unsafe",
        "atomic",
        "binary",
        "debug",
        "exec",
        "heap",
        "ring",
        "scanner",
        "tar",
        "zip",
        "gzip",
        "zlib",
        "tls",
        "url",
        "user",
        "pprof",
        "trace",
        "ast",
        "build",
        "parser",
        "printer",
        "token",
        "types",
        "cgo",
        "plugin",
        "race",
        "ioutil",
        // Kubernetes-common stdlib aliases
        "utilruntime",
        "utilwait",
        "utilnet",
    ])
});

static GO_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "make",
        "new",
        "len",
        "cap",
        "append",
        "copy",
        "delete",
        "close",
        "panic",
        "recover",
        "print",
        "println",
        "complex",
        "real",
        "imag",
        "error",
        "nil",
        "true",
        "false",
        "iota",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "uintptr",
        "float32",
        "float64",
        "complex64",
        "complex128",
        "string",
        "bool",
        "byte",
        "rune",
        "any",
    ])
});

const PASCAL_UNIT_PREFIXES: [&str; 15] = [
    "System.",
    "Winapi.",
    "Vcl.",
    "Fmx.",
    "Data.",
    "Datasnap.",
    "Soap.",
    "Xml.",
    "Web.",
    "REST.",
    "FireDAC.",
    "IBX.",
    "IdHTTP",
    "IdTCP",
    "IdSSL",
];

static PASCAL_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "System",
        "SysUtils",
        "Classes",
        "Types",
        "Variants",
        "StrUtils",
        "Math",
        "DateUtils",
        "IOUtils",
        "Generics.Collections",
        "Generics.Defaults",
        "Rtti",
        "TypInfo",
        "SyncObjs",
        "RegularExpressions",
        "SysInit",
        "Windows",
        "Messages",
        "Graphics",
        "Controls",
        "Forms",
        "Dialogs",
        "StdCtrls",
        "ExtCtrls",
        "ComCtrls",
        "Menus",
        "ActnList",
        "WriteLn",
        "Write",
        "ReadLn",
        "Read",
        "Inc",
        "Dec",
        "Ord",
        "Chr",
        "Length",
        "SetLength",
        "High",
        "Low",
        "Assigned",
        "FreeAndNil",
        "Format",
        "IntToStr",
        "StrToInt",
        "FloatToStr",
        "StrToFloat",
        "Trim",
        "UpperCase",
        "LowerCase",
        "Pos",
        "Copy",
        "Delete",
        "Insert",
        "Now",
        "Date",
        "Time",
        "DateToStr",
        "StrToDate",
        "Raise",
        "Exit",
        "Break",
        "Continue",
        "Abort",
        "True",
        "False",
        "nil",
        "Self",
        "Result",
        "Create",
        "Destroy",
        "Free",
        "TObject",
        "TComponent",
        "TPersistent",
        "TInterfacedObject",
        "TList",
        "TStringList",
        "TStrings",
        "TStream",
        "TMemoryStream",
        "TFileStream",
        "Exception",
        "EAbort",
        "EConvertError",
        "EAccessViolation",
        "IInterface",
        "IUnknown",
    ])
});

static C_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // Standard C library functions
        "printf",
        "fprintf",
        "sprintf",
        "snprintf",
        "scanf",
        "fscanf",
        "sscanf",
        "malloc",
        "calloc",
        "realloc",
        "free",
        "memcpy",
        "memmove",
        "memset",
        "memcmp",
        "memchr",
        "strlen",
        "strcpy",
        "strncpy",
        "strcat",
        "strncat",
        "strcmp",
        "strncmp",
        "strstr",
        "strchr",
        "strrchr",
        "strtok",
        "strdup",
        "fopen",
        "fclose",
        "fread",
        "fwrite",
        "fgets",
        "fputs",
        "fputc",
        "fgetc",
        "feof",
        "ferror",
        "fflush",
        "fseek",
        "ftell",
        "rewind",
        "exit",
        "abort",
        "atexit",
        "atoi",
        "atol",
        "atof",
        "strtol",
        "strtoul",
        "strtod",
        "qsort",
        "bsearch",
        "abs",
        "labs",
        "rand",
        "srand",
        "sin",
        "cos",
        "tan",
        "sqrt",
        "pow",
        "log",
        "log10",
        "exp",
        "ceil",
        "floor",
        "fabs",
        "time",
        "clock",
        "difftime",
        "mktime",
        "localtime",
        "gmtime",
        "strftime",
        "asctime",
        "assert",
        "errno",
        "perror",
        "remove",
        "rename",
        "tmpfile",
        "tmpnam",
        "getenv",
        "system",
        "signal",
        "raise",
        "setjmp",
        "longjmp",
        "va_start",
        "va_end",
        "va_arg",
        "va_copy",
        "NULL",
        "EOF",
        "BUFSIZ",
        "FILENAME_MAX",
        "RAND_MAX",
        "EXIT_SUCCESS",
        "EXIT_FAILURE",
        "size_t",
        "ptrdiff_t",
        "wchar_t",
        "intptr_t",
        "uintptr_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "FILE",
        // POSIX additions commonly seen
        "stat",
        "lstat",
        "fstat",
        "open",
        "close",
        "read",
        "write",
        "pipe",
        "fork",
        "exec",
        "waitpid",
        "getpid",
        "getppid",
        "kill",
        "sleep",
        "usleep",
        "pthread_create",
        "pthread_join",
        "pthread_mutex_lock",
        "pthread_mutex_unlock",
        "dlopen",
        "dlsym",
        "dlclose",
    ])
});

static CPP_BUILT_INS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // iostream objects (often used without std:: prefix via using)
        "cout",
        "cin",
        "cerr",
        "clog",
        "endl",
        "flush",
        "ws",
        "std", // the namespace itself when used as std::something
        // Common C++ keywords that leak as references
        "nullptr",
        "true",
        "false",
        "this",
        "sizeof",
        "alignof",
        "typeid",
        "static_cast",
        "dynamic_cast",
        "reinterpret_cast",
        "const_cast",
        "make_unique",
        "make_shared",
        "make_pair",
        "move",
        "forward",
        "swap",
    ])
});

/// JS `charAt(0).toUpperCase() + slice(1)` (scalar-based; differs from
/// UTF-16 slicing only for astral-plane first chars).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// =============================================================================
// Resolution context (the TS object literal returned by createContext())
// =============================================================================

/// The production [`ResolutionContext`] implementation, backed by a
/// [`QueryBuilder`] + the project filesystem, with LRU-bounded caches.
///
/// In TS this was an object literal closing over the `ReferenceResolver`'s
/// private cache fields; in Rust the caches live HERE (interior mutability)
/// and the resolver delegates to them, which is observably identical.
pub struct ResolverContext {
    project_root: String,
    queries: QueryBuilder,
    // All per-resolver caches are LRU-bounded. Previously these were
    // unbounded Maps that grew with every distinct lookup and OOM'd on
    // codebases with 20k+ files (see issue: unbounded cache growth).
    node_cache: RefCell<LRUCache<String, Vec<Node>>>, // per-file node cache
    file_cache: RefCell<LRUCache<String, Option<String>>>, // per-file content cache
    import_mapping_cache: RefCell<LRUCache<String, Vec<ImportMapping>>>,
    re_export_cache: RefCell<LRUCache<String, Vec<ReExport>>>,
    name_cache: RefCell<LRUCache<String, Vec<Node>>>, // name → nodes cache
    lower_name_cache: RefCell<LRUCache<String, Vec<Node>>>, // lower(name) → nodes cache
    qualified_name_cache: RefCell<LRUCache<String, Vec<Node>>>, // qualified_name → nodes cache
    known_names: RefCell<Option<HashSet<String>>>, // all known symbol names for fast pre-filtering
    known_files: RefCell<Option<HashSet<String>>>,
    /// Memoized `get_all_files` result (frameworks call it repeatedly).
    files_list: RefCell<Option<std::sync::Arc<Vec<String>>>>,
    caches_warmed: Cell<bool>,
    // tsconfig/jsconfig path-alias map. `OnceCell` empty = not yet computed,
    // `Some(None)` = computed and absent. Treated as immutable for the
    // resolver's lifetime; callers re-create the resolver if config changes.
    project_aliases: OnceCell<Option<AliasMap>>,
    // go.mod module path. Same lazy/immutable convention as projectAliases.
    go_module: OnceCell<Option<GoModule>>,
    // Monorepo workspace member packages. Same lazy/immutable convention.
    workspace_packages: OnceCell<Option<WorkspacePackages>>,
}

/// `/\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?)$/i` — is this file in the JS/TS family?
fn is_js_family_path(file_path: &str) -> bool {
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\.(?:d\.ts|[cm]?tsx?|[cm]?jsx?)$").expect("valid js-family regex")
    });
    RE.is_match(file_path)
}

impl ResolverContext {
    fn new(project_root: String, queries: QueryBuilder) -> Self {
        let limit = resolve_cache_limit();
        // The content cache is heavier (full file text), so we give it a
        // smaller budget than the metadata caches.
        let content_limit = std::cmp::max(64, limit / 5);
        ResolverContext {
            project_root,
            queries,
            node_cache: RefCell::new(LRUCache::new(limit)),
            file_cache: RefCell::new(LRUCache::new(content_limit)),
            import_mapping_cache: RefCell::new(LRUCache::new(limit)),
            re_export_cache: RefCell::new(LRUCache::new(limit)),
            name_cache: RefCell::new(LRUCache::new(limit)),
            lower_name_cache: RefCell::new(LRUCache::new(limit)),
            qualified_name_cache: RefCell::new(LRUCache::new(limit)),
            known_names: RefCell::new(None),
            known_files: RefCell::new(None),
            files_list: RefCell::new(None),
            caches_warmed: Cell::new(false),
            project_aliases: OnceCell::new(),
            go_module: OnceCell::new(),
            workspace_packages: OnceCell::new(),
        }
    }

    fn clear_caches(&self) {
        self.node_cache.borrow_mut().clear();
        self.file_cache.borrow_mut().clear();
        self.import_mapping_cache.borrow_mut().clear();
        self.re_export_cache.borrow_mut().clear();
        self.name_cache.borrow_mut().clear();
        self.lower_name_cache.borrow_mut().clear();
        self.qualified_name_cache.borrow_mut().clear();
        *self.known_names.borrow_mut() = None;
        *self.known_files.borrow_mut() = None;
        *self.files_list.borrow_mut() = None;
        self.caches_warmed.set(false);
    }

    /// `this.knownNames?.has(name)` — false when the cache isn't warmed.
    fn known_has(&self, name: &str) -> bool {
        self.known_names
            .borrow()
            .as_ref()
            .is_some_and(|s| s.contains(name))
    }
}

impl ResolutionContext for ResolverContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        let key = file_path.to_string();
        let has = self.node_cache.borrow().has(&key);
        if !has {
            let nodes = self
                .queries
                .get_nodes_by_file(file_path)
                .unwrap_or_else(|e| {
                    log_warn(
                        "Failed to load nodes for file during resolution",
                        Some(&serde_json::json!({ "filePath": file_path, "error": e.to_string() })),
                    );
                    Vec::new()
                });
            self.node_cache.borrow_mut().set(key.clone(), nodes);
        }
        self.node_cache
            .borrow_mut()
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        let key = name.to_string();
        if let Some(cached) = self.name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self.queries.get_nodes_by_name(name).unwrap_or_else(|e| {
            log_warn(
                "Failed to load nodes by name during resolution",
                Some(&serde_json::json!({ "name": name, "error": e.to_string() })),
            );
            Vec::new()
        });
        self.name_cache.borrow_mut().set(key, result.clone());
        result
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        let key = qualified_name.to_string();
        if let Some(cached) = self.qualified_name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self
            .queries
            .get_nodes_by_qualified_name_exact(qualified_name)
            .unwrap_or_else(|e| {
                log_warn(
                    "Failed to load nodes by qualified name during resolution",
                    Some(&serde_json::json!({
                        "qualifiedName": qualified_name,
                        "error": e.to_string()
                    })),
                );
                Vec::new()
            });
        self.qualified_name_cache
            .borrow_mut()
            .set(key, result.clone());
        result
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.queries.get_nodes_by_kind(kind).unwrap_or_else(|e| {
            log_warn(
                "Failed to load nodes by kind during resolution",
                Some(&serde_json::json!({ "kind": kind.as_str(), "error": e.to_string() })),
            );
            Vec::new()
        })
    }

    fn file_exists(&self, file_path: &str) -> bool {
        // Check pre-built known files set first (O(1))
        if let Some(known) = self.known_files.borrow().as_ref() {
            let normalized = file_path.replace('\\', "/");
            if known.contains(file_path) || known.contains(&normalized) {
                return true;
            }
        }
        // Fall back to filesystem for files not yet indexed
        Path::new(&self.project_root).join(file_path).exists()
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        let key = file_path.to_string();
        if self.file_cache.borrow().has(&key) {
            return self.file_cache.borrow_mut().get(&key).cloned().flatten();
        }

        let full_path = Path::new(&self.project_root).join(file_path);
        match fs::read(&full_path) {
            Ok(bytes) => {
                // Node's readFileSync(..., 'utf-8') lossy-decodes invalid
                // sequences rather than failing.
                let content = String::from_utf8_lossy(&bytes).into_owned();
                self.file_cache.borrow_mut().set(key, Some(content.clone()));
                Some(content)
            }
            Err(error) => {
                log_debug(
                    "Failed to read file for resolution",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string()
                    })),
                );
                self.file_cache.borrow_mut().set(key, None);
                None
            }
        }
    }

    fn get_project_root(&self) -> &str {
        &self.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        // Memoized: every framework's `detect()` (and several post-extract
        // passes) calls this, and an uncached full table scan of 70k+ paths
        // per call made `CodeGraph::open` take ~25s on llvm-sized indexes.
        if let Some(cached) = self.files_list.borrow().as_ref() {
            return cached.as_ref().clone();
        }
        let files = self.queries.get_all_file_paths().unwrap_or_else(|e| {
            log_warn(
                "Failed to load file paths during resolution",
                Some(&serde_json::json!({ "error": e.to_string() })),
            );
            Vec::new()
        });
        *self.files_list.borrow_mut() = Some(std::sync::Arc::new(files.clone()));
        files
    }

    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        let target: PathBuf = if relative_path == "." || relative_path.is_empty() {
            PathBuf::from(&self.project_root)
        } else {
            Path::new(&self.project_root).join(relative_path)
        };
        match fs::read_dir(&target) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(error) => {
                log_debug(
                    "Failed to list directory for resolution",
                    Some(&serde_json::json!({
                        "relativePath": relative_path,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            }
        }
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        let key = lower_name.to_string();
        if let Some(cached) = self.lower_name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self
            .queries
            .get_nodes_by_lower_name(lower_name)
            .unwrap_or_else(|e| {
                log_warn(
                    "Failed to load nodes by lower name during resolution",
                    Some(&serde_json::json!({ "lowerName": lower_name, "error": e.to_string() })),
                );
                Vec::new()
            });
        self.lower_name_cache.borrow_mut().set(key, result.clone());
        result
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        let cache_key = file_path.to_string();
        // TS `if (cached) return cached;` — a cached EMPTY array is truthy,
        // so any cached value (incl. empty) is returned.
        if let Some(cached) = self.import_mapping_cache.borrow_mut().get(&cache_key) {
            return cached.clone();
        }

        let content = self.read_file(file_path);
        let content = match content {
            // TS `if (!content)` — both null and '' are falsy.
            Some(c) if !c.is_empty() => c,
            _ => {
                self.import_mapping_cache
                    .borrow_mut()
                    .set(cache_key, Vec::new());
                return Vec::new();
            }
        };

        let mappings = extract_import_mappings(file_path, &content, language);
        self.import_mapping_cache
            .borrow_mut()
            .set(cache_key, mappings.clone());
        mappings
    }

    fn get_project_aliases(&self) -> Option<&AliasMap> {
        self.project_aliases
            .get_or_init(|| load_project_aliases(&self.project_root))
            .as_ref()
    }

    fn get_go_module(&self) -> Option<&GoModule> {
        self.go_module
            .get_or_init(|| load_go_module(&self.project_root))
            .as_ref()
    }

    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages
            .get_or_init(|| load_workspace_packages(&self.project_root))
            .as_ref()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let key = file_path.to_string();
        // TS `if (cached) return cached;` — empty array is truthy.
        if let Some(cached) = self.re_export_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let content = self.read_file(file_path);
        let content = match content {
            Some(c) if !c.is_empty() => c,
            _ => {
                self.re_export_cache.borrow_mut().set(key, Vec::new());
                return Vec::new();
            }
        };
        // Re-exports are a JS/TS-only construct, and what matters is the
        // BARREL file's own language — not the consuming reference's. A
        // `.svelte`/`.vue` consumer threads its own language down the
        // re-export chase, which would make extractReExports() bail on a
        // `.ts` index barrel and silently break the chain (#629). Re-key
        // the parse on the barrel's extension so the chase works no matter
        // what kind of file imports through it.
        let parse_language = if is_js_family_path(file_path) {
            Language::Typescript
        } else {
            language
        };
        let re_exports = extract_re_exports(&content, parse_language);
        self.re_export_cache
            .borrow_mut()
            .set(key, re_exports.clone());
        re_exports
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        load_cpp_include_dirs(&self.project_root)
    }
}

// =============================================================================
// Reference Resolver
// =============================================================================

/// Reference Resolver
///
/// Orchestrates reference resolution using multiple strategies.
pub struct ReferenceResolver {
    context: ResolverContext,
    frameworks: Vec<Box<dyn FrameworkResolver>>,
}

impl ReferenceResolver {
    pub fn new(project_root: impl Into<String>, queries: QueryBuilder) -> Self {
        ReferenceResolver {
            context: ResolverContext::new(project_root.into(), queries),
            frameworks: Vec::new(),
        }
    }

    /// Initialize the resolver (detect frameworks, etc.)
    pub fn initialize(&mut self) {
        self.frameworks = detect_frameworks(&self.context);
        self.clear_caches();
    }

    /// The production resolution context (exposed for wiring — the callback
    /// synthesizer and tests use it; TS kept it private but passed it to the
    /// same collaborators).
    pub fn context(&self) -> &dyn ResolutionContext {
        &self.context
    }

    /// Run each framework resolver's cross-file finalization pass and persist
    /// the returned node updates. Idempotent — safe to call after every indexAll
    /// and every incremental sync. Returns the number of nodes updated.
    ///
    /// Caches are cleared before/after so the post-extract pass sees fresh DB
    /// state and downstream queries see the updated names.
    pub fn run_post_extract(&self) -> usize {
        let mut updated = 0usize;
        self.clear_caches();
        for fw in &self.frameworks {
            let Some(nodes) = fw.post_extract(&self.context) else {
                continue; // TS `if (!fw.postExtract) continue;`
            };
            for node in &nodes {
                match self.context.queries.update_node(node) {
                    Ok(()) => updated += 1,
                    Err(err) => {
                        // TS: try/catch around the whole per-framework loop —
                        // an updateNode failure aborts the rest of this
                        // framework's updates and moves on.
                        log_debug(
                            &format!("Framework '{}' postExtract failed", fw.name()),
                            Some(&serde_json::json!({ "error": err.to_string() })),
                        );
                        break;
                    }
                }
            }
        }
        if updated > 0 {
            self.clear_caches();
        }
        updated
    }

    /// Pre-build lightweight caches for resolution.
    /// Node lookups are now handled by indexed SQLite queries instead of
    /// loading all nodes into memory (which caused OOM on large codebases).
    /// We cache the set of known symbol names for fast pre-filtering.
    pub fn warm_caches(&self) {
        if self.context.caches_warmed.get() {
            return;
        }

        // Only cache the set of known file paths (lightweight string set).
        // On a query error the pre-filter stays disabled (None) rather than
        // becoming an empty set that would wrongly filter everything.
        match self.context.queries.get_all_file_paths() {
            Ok(paths) => {
                *self.context.known_files.borrow_mut() = Some(paths.into_iter().collect());
            }
            Err(e) => {
                log_warn(
                    "Failed to warm known-files cache",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                *self.context.known_files.borrow_mut() = None;
            }
        }

        // Cache all distinct symbol names for fast pre-filtering (just strings, not full nodes)
        match self.context.queries.get_all_node_names() {
            Ok(names) => {
                *self.context.known_names.borrow_mut() = Some(names.into_iter().collect());
            }
            Err(e) => {
                log_warn(
                    "Failed to warm known-names cache",
                    Some(&serde_json::json!({ "error": e.to_string() })),
                );
                *self.context.known_names.borrow_mut() = None;
            }
        }

        self.context.caches_warmed.set(true);
    }

    /// Clear internal caches
    pub fn clear_caches(&self) {
        self.context.clear_caches();
    }

    /// Resolve all unresolved references
    pub fn resolve_all(
        &self,
        unresolved_refs: &[UnresolvedReference],
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> ResolutionResult {
        // Pre-load all nodes into memory for fast lookups
        self.warm_caches();

        let mut resolved: Vec<ResolvedRef> = Vec::new();
        let mut unresolved: Vec<UnresolvedRef> = Vec::new();
        let mut by_method: HashMap<String, usize> = HashMap::new();

        // Convert to our internal format, using denormalized fields when available
        let refs: Vec<UnresolvedRef> = unresolved_refs
            .iter()
            .map(|r| UnresolvedRef {
                from_node_id: r.from_node_id.clone(),
                reference_name: r.reference_name.clone(),
                reference_kind: r.reference_kind,
                line: r.line,
                column: r.column,
                // TS `ref.filePath || …` / `ref.language || …` (empty string
                // is falsy; 'unknown' is truthy and kept).
                file_path: match &r.file_path {
                    Some(p) if !p.is_empty() => p.clone(),
                    _ => self.get_file_path_from_node_id(&r.from_node_id),
                },
                language: match r.language {
                    Some(l) => l,
                    None => self.get_language_from_node_id(&r.from_node_id),
                },
                candidates: None,
            })
            .collect();

        let total = refs.len();
        let mut last_reported_percent: i64 = -1;

        // GPU batch pre-filter: probe every reference name (and its
        // qualified-name parts) against the known-names table in one kernel
        // launch. Verdicts are exact mirrors of `has_any_possible_match`;
        // 0x80 marks names the kernel defers to the CPU (non-ASCII
        // capitalization). On machines without CUDA this is a no-op.
        #[cfg(feature = "gpu")]
        #[allow(clippy::type_complexity)]
        let (gpu_hints, gpu_ranked, gpu_s12, gpu_fuzzy): (
            Option<Vec<u8>>,
            Option<HashMap<usize, Option<Node>>>,
            Option<HashMap<usize, Option<(Node, bool)>>>,
            Option<HashMap<usize, Option<(Node, bool)>>>,
        ) = {
            let joiner = {
                let guard = self.context.known_names.borrow();
                guard.as_ref().and_then(|known| {
                    let names: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
                    super::gpu::GpuNameJoiner::new(&names)
                })
            };
            match joiner {
                None => (None, None, None, None),
                Some(joiner) => {
                    let ref_names: Vec<&str> =
                        refs.iter().map(|r| r.reference_name.as_str()).collect();
                    let hints = joiner.probe_batch(&ref_names);
                    // Tier-2: precompute find_best_match winners for every
                    // multi-candidate exact-name group in one kernel launch.
                    // Refs the tier-1 verdict already rules out never reach
                    // strategy 3, so don't pay to rank them.
                    let ranked = self.gpu_rank_exact_name(&joiner, &refs, hints.as_deref());
                    let s12 = self.gpu_match_s12(&joiner, &refs, hints.as_deref());
                    let fuzzy = self.gpu_fuzzy(&joiner, &refs, hints.as_deref());
                    (hints, ranked, s12, fuzzy)
                }
            }
        };
        #[cfg(not(feature = "gpu"))]
        #[allow(clippy::type_complexity)]
        let (gpu_hints, gpu_ranked, gpu_s12, gpu_fuzzy): (
            Option<Vec<u8>>,
            Option<HashMap<usize, Option<Node>>>,
            Option<HashMap<usize, Option<(Node, bool)>>>,
            Option<HashMap<usize, Option<(Node, bool)>>>,
        ) = (None, None, None, None);

        for (i, r) in refs.iter().enumerate() {
            let hint = gpu_hints.as_ref().map(|h| h[i]).and_then(|f| match f {
                1 => Some(true),
                0 => Some(false),
                _ => None, // kernel deferred — CPU decides
            });
            let ranked_hint = gpu_ranked
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|winner| winner.as_ref());
            let s12_hint = gpu_s12
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|w| w.as_ref().map(|(n, s1)| (n, *s1)));
            let fuzzy_hint = gpu_fuzzy
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|w| w.as_ref().map(|(n, x)| (n, *x)));
            let result = self.resolve_one_hinted(r, hint, ranked_hint, s12_hint, fuzzy_hint);

            if let Some(result) = result {
                *by_method
                    .entry(result.resolved_by.as_str().to_string())
                    .or_insert(0) += 1;
                resolved.push(result);
            } else {
                unresolved.push(r.clone());
            }

            // Report progress every 1% to avoid too many updates
            if let Some(cb) = on_progress.as_deref_mut() {
                let current_percent = ((i as f64 / total as f64) * 100.0).floor() as i64;
                if current_percent > last_reported_percent {
                    last_reported_percent = current_percent;
                    cb(i + 1, total);
                }
            }
        }

        // Final progress report
        if total > 0 {
            if let Some(cb) = on_progress {
                cb(total, total);
            }
        }

        ResolutionResult {
            stats: ResolutionStats {
                total,
                resolved: resolved.len(),
                unresolved: unresolved.len(),
                by_method,
            },
            resolved,
            unresolved,
        }
    }

    /// Check if a reference name has any possible match in the codebase.
    /// Uses the pre-built knownNames set to skip expensive resolution
    /// for names that definitely don't exist as symbols.
    fn has_any_possible_match(&self, name: &str) -> bool {
        let guard = self.context.known_names.borrow();
        let Some(known) = guard.as_ref() else {
            return true; // no pre-filter available
        };

        // Direct name match
        if known.contains(name) {
            return true;
        }

        // For qualified names like "obj.method" or "Class::method", check the parts
        if let Some(dot_idx) = name.find('.') {
            if dot_idx > 0 {
                let receiver = &name[..dot_idx];
                let member = &name[dot_idx + 1..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
                // Also check capitalized receiver (instance-method resolution)
                let capitalized = capitalize_first(receiver);
                if known.contains(&capitalized) {
                    return true;
                }
                // JVM FQN: `com.example.foo.Bar` — the only useful segment is the
                // last one (`Bar`); the earlier check finds `example.foo.Bar` which
                // never matches a node name.
                let last_dot = name.rfind('.').unwrap_or(0);
                if last_dot > dot_idx {
                    let tail = &name[last_dot + 1..];
                    if !tail.is_empty() && known.contains(tail) {
                        return true;
                    }
                }
            }
        }
        if let Some(colon_idx) = name.find("::") {
            if colon_idx > 0 {
                let receiver = &name[..colon_idx];
                let member = &name[colon_idx + 2..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
            }
        }

        // For path-like references (e.g., "snippets/drawer-menu.liquid"), check the filename
        if let Some(slash_idx) = name.rfind('/') {
            if slash_idx > 0 {
                let file_name = &name[slash_idx + 1..];
                if known.contains(file_name) {
                    return true;
                }
            }
        }

        false
    }

    /// Does `ref.referenceName` match an import declared in its containing
    /// file? Used as a pre-filter escape so re-export chain resolution
    /// still gets a chance when the name has no project-wide declaration.
    fn matches_any_import(&self, r: &UnresolvedRef) -> bool {
        let imports = self.context.get_import_mappings(&r.file_path, r.language);
        if imports.is_empty() {
            return false;
        }
        for imp in &imports {
            if imp.local_name == r.reference_name
                || r.reference_name
                    .starts_with(&format!("{}.", imp.local_name))
            {
                return true;
            }
        }
        false
    }

    /// Resolve a single reference
    /// Batch-precompute `match_fuzzy` uniqueness verdicts on the GPU.
    #[cfg(feature = "gpu")]
    #[allow(clippy::type_complexity)]
    fn gpu_fuzzy(
        &self,
        joiner: &super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<(Node, bool)>>> {
        fn kind_class(k: NodeKind) -> u8 {
            match k {
                NodeKind::Function => 1,
                NodeKind::Method => 2,
                NodeKind::Class => 3,
                _ => 0,
            }
        }
        let mut lang_ids: HashMap<Language, u8> = HashMap::new();
        let mut intern_lang = |l: Language| -> u8 {
            let next = lang_ids.len() as u8;
            *lang_ids.entry(l).or_insert(next)
        };
        let mut groups: HashMap<String, i32> = HashMap::new();
        let mut cand_starts: Vec<u32> = vec![0];
        let (mut cand_lang, mut cand_kind) = (Vec::new(), Vec::new());
        let mut group_nodes: Vec<Vec<Node>> = Vec::new();
        let (mut ref_group, mut ref_lang) = (
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
        );
        for (idx, r) in refs.iter().enumerate() {
            if prefilter.is_some_and(|f| f[idx] == 0) {
                ref_group.push(-1);
                ref_lang.push(0);
                continue;
            }
            let lower = r.reference_name.to_lowercase();
            let g = *groups.entry(lower.clone()).or_insert_with(|| {
                let candidates = self.context.get_nodes_by_lower_name(&lower);
                if candidates.is_empty() {
                    -1
                } else {
                    for c in &candidates {
                        cand_lang.push(intern_lang(c.language));
                        cand_kind.push(kind_class(c.kind));
                    }
                    cand_starts.push(cand_lang.len() as u32);
                    group_nodes.push(candidates);
                    (group_nodes.len() - 1) as i32
                }
            });
            ref_group.push(g);
            ref_lang.push(intern_lang(r.language));
        }
        if group_nodes.is_empty() {
            return Some(HashMap::new());
        }
        let (best, cross) =
            joiner.fuzzy_unique(&ref_group, &ref_lang, &cand_starts, &cand_lang, &cand_kind)?;
        let mut out = HashMap::new();
        for (i, &g) in ref_group.iter().enumerate() {
            if g < 0 {
                continue;
            }
            let winner = if best[i] < 0 {
                None
            } else {
                let local = (best[i] as u32 - cand_starts[g as usize]) as usize;
                Some((group_nodes[g as usize][local].clone(), cross[i] != 0))
            };
            out.insert(i, winner);
        }
        Some(out)
    }

    /// Batch-precompute `match_method_call` strategy-1/2 winners on the GPU.
    /// Mirrors the CPU block exactly: per dot/colon reference, class
    /// candidates from `get_nodes_by_name(receiver)` (strategy 1) then
    /// `get_nodes_by_name(Capitalized)` (strategy 2), filtered to
    /// Class|Struct|Interface of the same language IN ORDER; the kernel scans
    /// each candidate file's methods in `get_nodes_in_file` order for the
    /// first name+containment match. Returns ref-index → winner
    /// (`None` = strategies 1+2 provably find nothing).
    #[cfg(feature = "gpu")]
    #[allow(clippy::type_complexity)]
    fn gpu_match_s12(
        &self,
        joiner: &super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<(Node, bool)>>> {
        use super::name_matcher::{capitalize_first_shared, split_method_call};

        fn fnv(s: &str) -> u64 {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &b in s.as_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }

        // Per-file method tables, built once per distinct candidate file in
        // get_nodes_in_file order (= CPU scan order).
        let mut file_ids: HashMap<String, u32> = HashMap::new();
        let mut file_starts: Vec<u32> = vec![0];
        let (mut m_hash, mut m_qn_off, mut m_qn_len) = (Vec::new(), Vec::new(), Vec::new());
        let mut qn_buf: Vec<u8> = Vec::new();
        let mut method_nodes: Vec<Node> = Vec::new();

        let mut ref_cand_starts: Vec<u32> = vec![0];
        let (mut cls_file, mut cls_name_off, mut cls_name_len) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut name_buf: Vec<u8> = Vec::new();
        let mut ref_method_hash: Vec<u64> = Vec::new();
        let mut ref_idx_map: Vec<usize> = Vec::new();
        let mut s1_boundary: Vec<u32> = Vec::new();

        for (idx, r) in refs.iter().enumerate() {
            if prefilter.is_some_and(|f| f[idx] == 0) {
                continue;
            }
            let Some((obj, method)) = split_method_call(&r.reference_name) else {
                continue;
            };
            let mut push_classes = |name: &str,
                                    cls_file: &mut Vec<u32>,
                                    cls_name_off: &mut Vec<u32>,
                                    cls_name_len: &mut Vec<u32>,
                                    name_buf: &mut Vec<u8>| {
                for c in self.context.get_nodes_by_name(name) {
                    if !(c.kind == NodeKind::Class
                        || c.kind == NodeKind::Struct
                        || c.kind == NodeKind::Interface)
                        || c.language != r.language
                    {
                        continue;
                    }
                    let fid = match file_ids.get(&c.file_path) {
                        Some(&id) => id,
                        None => {
                            let id = file_ids.len() as u32;
                            file_ids.insert(c.file_path.clone(), id);
                            for n in self.context.get_nodes_in_file(&c.file_path) {
                                if n.kind == NodeKind::Method {
                                    m_hash.push(fnv(&n.name));
                                    m_qn_off.push(qn_buf.len() as u32);
                                    m_qn_len.push(n.qualified_name.len() as u32);
                                    qn_buf.extend_from_slice(n.qualified_name.as_bytes());
                                    method_nodes.push(n);
                                }
                            }
                            file_starts.push(m_hash.len() as u32);
                            id
                        }
                    };
                    cls_file.push(fid);
                    cls_name_off.push(name_buf.len() as u32);
                    cls_name_len.push(c.name.len() as u32);
                    name_buf.extend_from_slice(c.name.as_bytes());
                }
            };
            push_classes(
                obj,
                &mut cls_file,
                &mut cls_name_off,
                &mut cls_name_len,
                &mut name_buf,
            );
            let boundary = cls_file.len() as u32;
            let cap = capitalize_first_shared(obj);
            if cap != obj {
                push_classes(
                    &cap,
                    &mut cls_file,
                    &mut cls_name_off,
                    &mut cls_name_len,
                    &mut name_buf,
                );
            }
            if cls_file.len() as u32 == *ref_cand_starts.last().unwrap() {
                // no candidates at all — CPU loops would find nothing
                continue;
            }
            ref_cand_starts.push(cls_file.len() as u32);
            ref_method_hash.push(fnv(method));
            ref_idx_map.push(idx);
            s1_boundary.push(boundary);
        }
        if ref_method_hash.is_empty() {
            return Some(HashMap::new());
        }

        let (best_m, best_c) = joiner.match_class_methods(
            &ref_cand_starts,
            &ref_method_hash,
            &cls_file,
            &cls_name_off,
            &cls_name_len,
            &name_buf,
            &file_starts,
            &m_hash,
            &m_qn_off,
            &m_qn_len,
            &qn_buf,
        )?;

        let mut out: HashMap<usize, Option<(Node, bool)>> = HashMap::new();
        for (k, &orig_idx) in ref_idx_map.iter().enumerate() {
            let winner = if best_m[k] < 0 {
                None
            } else {
                let via_s1 = (best_c[k] as u32) < s1_boundary[k];
                Some((method_nodes[best_m[k] as usize].clone(), via_s1))
            };
            out.insert(orig_idx, winner);
        }
        Some(out)
    }

    /// Batch-precompute `find_best_match` winners on the GPU for every
    /// reference whose exact-name candidate set has 2+ entries (the only
    /// case where `match_by_exact_name` ranks). Candidates are flattened in
    /// `get_nodes_by_name` order, so the kernel's strict-`>` scan reproduces
    /// the CPU tie-break exactly. Returns ref-index → winner (None = no
    /// candidate beat the CPU's -1.0 selection floor).
    #[cfg(feature = "gpu")]
    fn gpu_rank_exact_name(
        &self,
        joiner: &super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<Node>>> {
        fn kind_class(k: NodeKind) -> u8 {
            match k {
                NodeKind::Function => 1,
                NodeKind::Method => 2,
                NodeKind::Class => 3,
                NodeKind::Struct => 4,
                NodeKind::Interface => 5,
                _ => 0,
            }
        }
        fn ref_kind_class(k: EdgeKind) -> u8 {
            match k {
                EdgeKind::Calls => 1,
                EdgeKind::Instantiates => 2,
                EdgeKind::Decorates => 3,
                _ => 0,
            }
        }
        // FNV-1a over a dir segment, chained with the previous prefix hash —
        // equal cumulative hashes <=> equal leading segment lists.
        fn chain_hash(prev: u64, seg: &str) -> u64 {
            let mut h = prev ^ 0xcbf2_9ce4_8422_2325;
            for &b in seg.as_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        }

        let mut file_ids: HashMap<String, u32> = HashMap::new();
        let mut dir_starts: Vec<u32> = vec![0];
        let mut dir_hashes: Vec<u64> = Vec::new();
        let mut intern_file =
            |path: &str, dir_starts: &mut Vec<u32>, dir_hashes: &mut Vec<u64>| -> u32 {
                if let Some(&id) = file_ids.get(path) {
                    return id;
                }
                let id = file_ids.len() as u32;
                file_ids.insert(path.to_string(), id);
                let mut segs: Vec<&str> = path.split('/').collect();
                segs.pop(); // drop the filename — proximity compares directories
                let mut h = 0u64;
                for seg in segs {
                    h = chain_hash(h, seg);
                    dir_hashes.push(h);
                }
                dir_starts.push(dir_hashes.len() as u32);
                id
            };

        let mut lang_ids: HashMap<Language, u8> = HashMap::new();
        let mut intern_lang = |l: Language| -> u8 {
            let next = lang_ids.len() as u8;
            *lang_ids.entry(l).or_insert(next)
        };

        // Group refs by name; fetch each name's candidates once (context
        // cache backs this). Only multi-candidate names go to the GPU.
        let mut groups: HashMap<&str, i32> = HashMap::new();
        let mut cand_starts: Vec<u32> = vec![0];
        let (mut cand_file, mut cand_lang, mut cand_kind, mut cand_exp, mut cand_line) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut group_nodes: Vec<Vec<Node>> = Vec::new();
        let mut ref_group: Vec<i32> = Vec::with_capacity(refs.len());
        let (mut ref_file, mut ref_lang, mut ref_kind, mut ref_line) = (
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
        );
        for (idx, r) in refs.iter().enumerate() {
            // Tier-1 said "no possible match" — resolve_one exits before
            // strategy 3, so ranking would be pure waste.
            if prefilter.is_some_and(|f| f[idx] == 0) {
                ref_group.push(-1);
                ref_file.push(0);
                ref_lang.push(0);
                ref_kind.push(0);
                ref_line.push(0);
                continue;
            }
            let g = *groups.entry(r.reference_name.as_str()).or_insert_with(|| {
                let candidates = self.context.get_nodes_by_name(&r.reference_name);
                if candidates.len() < 2 {
                    -1
                } else {
                    for c in &candidates {
                        cand_file.push(intern_file(&c.file_path, &mut dir_starts, &mut dir_hashes));
                        cand_lang.push(intern_lang(c.language));
                        cand_kind.push(kind_class(c.kind));
                        cand_exp.push(u8::from(c.is_exported == Some(true)));
                        cand_line.push(c.start_line);
                    }
                    cand_starts.push(cand_file.len() as u32);
                    group_nodes.push(candidates);
                    (group_nodes.len() - 1) as i32
                }
            });
            ref_group.push(g);
            ref_file.push(intern_file(&r.file_path, &mut dir_starts, &mut dir_hashes));
            ref_lang.push(intern_lang(r.language));
            ref_kind.push(ref_kind_class(r.reference_kind));
            ref_line.push(r.line);
        }
        if group_nodes.is_empty() {
            return Some(HashMap::new());
        }

        let best = joiner.score_batch(
            &ref_group,
            &ref_file,
            &ref_lang,
            &ref_kind,
            &ref_line,
            &cand_starts,
            &cand_file,
            &cand_lang,
            &cand_kind,
            &cand_exp,
            &cand_line,
            &dir_starts,
            &dir_hashes,
        )?;

        let mut out: HashMap<usize, Option<Node>> = HashMap::new();
        for (i, (&g, &b)) in ref_group.iter().zip(best.iter()).enumerate() {
            if g < 0 {
                continue;
            }
            let winner = if b < 0 {
                None
            } else {
                let local = (b as u32 - cand_starts[g as usize]) as usize;
                Some(group_nodes[g as usize][local].clone())
            };
            out.insert(i, winner);
        }
        Some(out)
    }

    pub fn resolve_one(&self, r: &UnresolvedRef) -> Option<ResolvedRef> {
        self.resolve_one_hinted(r, None, None, None, None)
    }

    /// `resolve_one` with an optional precomputed `has_any_possible_match`
    /// verdict. The GPU batch pre-filter (feature `gpu`) probes every
    /// reference name in one kernel launch and feeds the verdicts through
    /// here; `None` falls back to the CPU check (also used for the rare
    /// names whose capitalization semantics the kernel defers).
    pub fn resolve_one_hinted(
        &self,
        r: &UnresolvedRef,
        known_hint: Option<bool>,
        ranked: Option<Option<&Node>>,
        s12: Option<Option<(&Node, bool)>>,
        fuzzy: Option<Option<(&Node, bool)>>,
    ) -> Option<ResolvedRef> {
        // Skip built-in/external references
        if self.is_built_in_or_external(r) {
            return None;
        }

        // Fast pre-filter: skip if no symbol with this name exists anywhere
        // AND the name doesn't match a local import. The import escape is
        // necessary because re-export rename chains (`import { login }
        // from './barrel'` where the barrel has `export { signIn as login }
        // from './auth'`) intentionally call a name that has no
        // declaration anywhere — only the renamed upstream symbol does.
        if !known_hint.unwrap_or_else(|| self.has_any_possible_match(&r.reference_name))
            && !self.matches_any_import(r)
            && !self
                .frameworks
                .iter()
                .any(|f| f.claims_reference(&r.reference_name))
        {
            return None;
        }

        // JVM FQN imports skip framework/name-matcher: `import com.example.Bar`
        // resolves directly through the qualifiedName index, which is unambiguous
        // even when several `Bar` classes exist in different packages.
        let jvm_import = resolve_jvm_import(r, &self.context);
        if jvm_import.is_some() {
            return jvm_import;
        }

        let mut candidates: Vec<ResolvedRef> = Vec::new();

        // Strategy 1: Try framework-specific resolution
        for framework in &self.frameworks {
            if let Some(result) = framework.resolve(r, &self.context) {
                if result.confidence >= 0.9 {
                    return Some(result); // High confidence, return immediately
                }
                candidates.push(result);
            }
        }

        // Strategy 2: Try import-based resolution
        if let Some(import_result) = resolve_via_import(r, &self.context) {
            if import_result.confidence >= 0.9 {
                return Some(import_result);
            }
            candidates.push(import_result);
        }

        // Strategy 3: Try name matching
        if let Some(name_result) =
            super::name_matcher::match_reference_full_hints(r, &self.context, ranked, s12, fuzzy)
        {
            candidates.push(name_result);
        }

        // Return highest confidence candidate (first wins ties — strict `>`)
        candidates.into_iter().reduce(|best, curr| {
            if curr.confidence > best.confidence {
                curr
            } else {
                best
            }
        })
    }

    /// Create edges from resolved references
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge> {
        resolved
            .iter()
            .map(|r| {
                let mut kind = r.original.reference_kind;

                // Promote "extends" to "implements" when a class/struct targets an interface
                if kind == EdgeKind::Extends {
                    if let Some(target_node) = self.get_node_by_id(&r.target_node_id) {
                        if target_node.kind == NodeKind::Interface
                            || target_node.kind == NodeKind::Protocol
                        {
                            if let Some(source_node) = self.get_node_by_id(&r.original.from_node_id)
                            {
                                if source_node.kind != NodeKind::Interface
                                    && source_node.kind != NodeKind::Protocol
                                {
                                    kind = EdgeKind::Implements;
                                }
                            }
                        }
                    }
                }

                // Promote "calls" to "instantiates" when the resolved target is a
                // class/struct. Languages without a `new` keyword (Python, Ruby)
                // express instantiation as `Foo()` — extraction can't tell that
                // apart from a function call without symbol info, but resolution
                // can: if `Foo` resolves to a class, the call IS an instantiation.
                if kind == EdgeKind::Calls {
                    if let Some(target_node) = self.get_node_by_id(&r.target_node_id) {
                        if target_node.kind == NodeKind::Class
                            || target_node.kind == NodeKind::Struct
                        {
                            kind = EdgeKind::Instantiates;
                        }
                    }
                }

                let mut metadata = Metadata::new();
                metadata.insert("confidence".to_string(), serde_json::json!(r.confidence));
                metadata.insert(
                    "resolvedBy".to_string(),
                    serde_json::Value::String(r.resolved_by.as_str().to_string()),
                );

                Edge {
                    source: r.original.from_node_id.clone(),
                    target: r.target_node_id.clone(),
                    kind,
                    line: Some(r.original.line),
                    column: Some(r.original.column),
                    metadata: Some(metadata),
                    provenance: None,
                }
            })
            .collect()
    }

    /// Resolve and persist edges to database
    pub fn resolve_and_persist(
        &self,
        unresolved_refs: &[UnresolvedReference],
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        let result = self.resolve_all(unresolved_refs, on_progress);

        // Create edges from resolved references
        let edges = self.create_edges(&result.resolved);

        // Insert edges into database
        if !edges.is_empty() {
            self.context.queries.insert_edges(&edges)?;
        }

        // Clean up resolved refs from unresolved_refs table so metrics are accurate
        if !result.resolved.is_empty() {
            let keys: Vec<ResolvedRefKey> = result
                .resolved
                .iter()
                .map(|r| ResolvedRefKey {
                    from_node_id: r.original.from_node_id.clone(),
                    reference_name: r.original.reference_name.clone(),
                    reference_kind: r.original.reference_kind.as_str().to_string(),
                })
                .collect();
            self.context
                .queries
                .delete_specific_resolved_references(&keys)?;
        }

        Ok(result)
    }

    /// Resolve and persist in batches to keep memory bounded.
    /// Processes unresolved references in chunks, persisting edges and cleaning
    /// up resolved refs after each batch to avoid accumulating large arrays.
    ///
    /// `batch_size: None` uses the TS default of 5000.
    pub fn resolve_and_persist_batched(
        &self,
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
        batch_size: Option<usize>,
    ) -> Result<ResolutionResult> {
        let batch_size = batch_size.unwrap_or(5000);
        self.warm_caches();

        let total = self.context.queries.get_unresolved_references_count()? as usize;
        let mut processed = 0usize;
        let mut aggregate_stats = ResolutionStats::default();

        // Process in stable row-id order. Resolved rows are deleted as we go, while
        // unresolved rows remain so future target-side syncs can repair them.
        let mut last_seen_ref_id: i64 = 0;
        loop {
            let batch_page = self
                .context
                .queries
                .get_unresolved_references_batch_after_id(last_seen_ref_id, batch_size)?;
            let batch = batch_page.refs;
            if batch.is_empty() {
                break;
            }
            last_seen_ref_id = batch_page.last_id;

            let result = self.resolve_all(&batch, None);

            // Persist edges immediately
            let edges = self.create_edges(&result.resolved);
            if !edges.is_empty() {
                self.context.queries.insert_edges(&edges)?;
            }

            // Clean up resolved refs so they don't appear in the next batch
            if !result.resolved.is_empty() {
                let keys: Vec<ResolvedRefKey> = result
                    .resolved
                    .iter()
                    .map(|r| ResolvedRefKey {
                        from_node_id: r.original.from_node_id.clone(),
                        reference_name: r.original.reference_name.clone(),
                        reference_kind: r.original.reference_kind.as_str().to_string(),
                    })
                    .collect();
                self.context
                    .queries
                    .delete_specific_resolved_references(&keys)?;
            }

            // Aggregate stats
            aggregate_stats.total += result.stats.total;
            aggregate_stats.resolved += result.stats.resolved;
            aggregate_stats.unresolved += result.stats.unresolved;
            for (method, count) in result.stats.by_method {
                *aggregate_stats.by_method.entry(method).or_insert(0) += count;
            }

            processed += batch.len();
            if let Some(cb) = on_progress.as_deref_mut() {
                cb(processed, total);
            }

            // (TS yielded to the event loop here so progress UI could render
            // between batches — no event loop natively.)
        }

        // Dynamic-edge synthesis: now that all base `calls` edges are persisted,
        // synthesize observer/callback dispatch edges (dispatcher → registered
        // callbacks) that static parsing leaves out. Best-effort — never fail the
        // index on it. See docs/design/callback-edge-synthesis.md.
        match synthesize_callback_edges(&self.context.queries, &self.context) {
            Ok(n) => {
                aggregate_stats
                    .by_method
                    .insert("callback-synthesis".to_string(), n);
            }
            Err(_) => {
                // synthesis is additive and optional; ignore failures
            }
        }

        Ok(ResolutionResult {
            resolved: Vec::new(),
            unresolved: Vec::new(),
            stats: aggregate_stats,
        })
    }

    /// Get detected frameworks
    pub fn get_detected_frameworks(&self) -> Vec<String> {
        self.frameworks
            .iter()
            .map(|f| f.name().to_string())
            .collect()
    }

    /// Check if reference is to a built-in or external symbol
    fn is_built_in_or_external(&self, r: &UnresolvedRef) -> bool {
        let name = r.reference_name.as_str();
        let is_js_ts = matches!(
            r.language,
            Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
        );

        // JavaScript/TypeScript built-ins
        if is_js_ts && JS_BUILT_INS.contains(name) {
            return true;
        }

        // Common JS/TS library calls (console.log, Math.floor, JSON.parse)
        if is_js_ts
            && (name.starts_with("console.")
                || name.starts_with("Math.")
                || name.starts_with("JSON."))
        {
            return true;
        }

        // React hooks from React itself
        if is_js_ts && REACT_HOOKS.contains(name) {
            return true;
        }

        // Python built-ins (bare calls only — dotted calls like console.print are method calls)
        if r.language == Language::Python && PYTHON_BUILT_INS.contains(name) {
            return true;
        }

        // Python built-in method calls (e.g., list.extend, dict.update)
        if r.language == Language::Python {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let receiver = &name[..dot_idx];
                    let method = &name[dot_idx + 1..];
                    // Filter calls on built-in types (list.append, dict.update, etc.)
                    if PYTHON_BUILT_IN_TYPES.contains(receiver) {
                        return true;
                    }
                    // Filter built-in methods on non-class receivers
                    // (e.g., items.append where items is a local list variable)
                    // But allow if the capitalized receiver matches a known codebase class
                    if PYTHON_BUILT_IN_METHODS.contains(method) {
                        let capitalized = capitalize_first(receiver);
                        if !self.context.known_has(&capitalized) {
                            return true;
                        }
                    }
                }
            }
            // A bare name colliding with a builtin method (index, get, update, count…)
            // is only a builtin when NOTHING in the codebase declares it. A declared
            // symbol with that exact name — e.g. a Flask/FastAPI view `def index()` or
            // `def get()` — is a real reference target. Mirrors the knownNames guard on
            // the dotted branch above; without it, every handler named after a builtin
            // method silently loses its route→handler edge.
            if PYTHON_BUILT_IN_METHODS.contains(name) && !self.context.known_has(name) {
                return true;
            }
        }

        // Go standard library packages — refs like "fmt.Println", "http.ListenAndServe", etc.
        if r.language == Language::Go {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let pkg = &name[..dot_idx];
                    if GO_STDLIB_PACKAGES.contains(pkg) {
                        return true;
                    }
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }

        // Pascal/Delphi built-ins and standard library units
        if r.language == Language::Pascal {
            if PASCAL_UNIT_PREFIXES.iter().any(|p| name.starts_with(p)) {
                return true;
            }
            if PASCAL_BUILT_INS.contains(name) {
                return true;
            }
        }

        // C/C++ standard library symbols (printf, malloc, std::vector, etc.).
        // Names that collide with user-defined symbols are NOT filtered —
        // C and C++ projects routinely shadow stdlib names (custom allocators
        // define `malloc`/`free`, stream wrappers define `read`/`write`/`open`,
        // containers define `move`/`swap`, logging libs wrap `printf`). Killing
        // those resolutions makes the graph wrong, not cleaner. We only filter
        // when there's no user node with this name — then name-matching would
        // produce zero edges anyway and the filter just short-circuits work.
        if r.language == Language::C || r.language == Language::Cpp {
            // C++ std:: namespace prefix — safe to filter unconditionally,
            // since `std::foo` is never a user-defined qualified name in
            // tree-sitter output.
            if name.starts_with("std::") {
                return true;
            }
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }

        false
    }

    fn get_node_by_id(&self, node_id: &str) -> Option<Node> {
        self.context.queries.get_node_by_id(node_id).ok().flatten()
    }

    /// Get file path from node ID
    fn get_file_path_from_node_id(&self, node_id: &str) -> String {
        self.get_node_by_id(node_id)
            .map(|n| n.file_path)
            .unwrap_or_default()
    }

    /// Get language from node ID
    fn get_language_from_node_id(&self, node_id: &str) -> Language {
        self.get_node_by_id(node_id)
            .map(|n| n.language)
            .unwrap_or(Language::Unknown)
    }
}

/// Create a reference resolver instance
pub fn create_resolver(
    project_root: impl Into<String>,
    queries: QueryBuilder,
) -> ReferenceResolver {
    let mut resolver = ReferenceResolver::new(project_root, queries);
    resolver.initialize();
    resolver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_prefix_mirrors_js_parse_int() {
        assert_eq!(parse_int_prefix("123"), Some(123));
        assert_eq!(parse_int_prefix("  42  "), Some(42));
        assert_eq!(parse_int_prefix("123abc"), Some(123));
        assert_eq!(parse_int_prefix("-7"), Some(-7));
        assert_eq!(parse_int_prefix("+9"), Some(9));
        assert_eq!(parse_int_prefix("abc"), None);
        assert_eq!(parse_int_prefix(""), None);
        assert_eq!(parse_int_prefix("0"), Some(0));
    }

    #[test]
    fn capitalize_first_matches_js() {
        assert_eq!(capitalize_first("recorder"), "Recorder");
        assert_eq!(capitalize_first("Recorder"), "Recorder");
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("a"), "A");
    }

    #[test]
    fn js_family_regex_matches_ts_pattern() {
        for p in [
            "a.ts",
            "a.tsx",
            "a.js",
            "a.jsx",
            "a.mts",
            "a.cts",
            "a.mjs",
            "a.cjs",
            "a.d.ts",
            "DIR/B.TSX",
        ] {
            assert!(is_js_family_path(p), "{p} should be JS-family");
        }
        for p in ["a.svelte", "a.vue", "a.py", "a.tsx.bak", "ats"] {
            assert!(!is_js_family_path(p), "{p} should NOT be JS-family");
        }
    }

    #[test]
    fn built_in_sets_have_ts_cardinalities() {
        // Pin the exact TS set sizes so a missed/duplicated entry is loud.
        assert_eq!(JS_BUILT_INS.len(), 28);
        assert_eq!(REACT_HOOKS.len(), 10);
        assert_eq!(PYTHON_BUILT_INS.len(), 23);
        assert_eq!(PYTHON_BUILT_IN_TYPES.len(), 13);
        assert_eq!(PYTHON_BUILT_IN_METHODS.len(), 45);
        assert_eq!(GO_STDLIB_PACKAGES.len(), 67);
        assert_eq!(GO_BUILT_INS.len(), 40);
        assert_eq!(PASCAL_UNIT_PREFIXES.len(), 15);
        assert_eq!(PASCAL_BUILT_INS.len(), 87);
        assert_eq!(C_BUILT_INS.len(), 137);
        assert_eq!(CPP_BUILT_INS.len(), 25);
    }
}
