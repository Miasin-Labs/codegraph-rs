//! IDA/Hex-Rays pseudo-C extractor.
//!
//! Detects and extracts C-like files produced by IDA/Hex-Rays dump scripts.
//! These are often close to C, but include invalid C identifiers (e.g.
//! `.mysql_init`) and very large one-function outputs that are better handled
//! with a lightweight pass than with tree-sitter.
//!
//! Ported from `src/extraction/ida-c-extractor.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use cpp_demangle::{DemangleOptions, Symbol};
use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Metadata,
    Node,
    NodeKind,
    Severity,
    UnresolvedReference,
};
use crate::utils::sha256_hex;

const IDA_DUMP_EXTENSIONS: &[&str] = &[".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".hxx"];
const IDA_SAMPLE_BYTES: usize = 16 * 1024;
const MAX_IDA_LOCAL_VARIABLES: usize = 2000;
/// Per-function cap on distinct string-literal nodes (avoids blowup on
/// resource-table / format-heavy functions).
const MAX_IDA_STRINGS: usize = 200;

static CONTROL_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "if", "for", "while", "switch", "return", "sizeof", "alignof", "case", "do",
    ]
    .into_iter()
    .collect()
});

static IDA_TYPE_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "__int8",
        "__int16",
        "__int32",
        "__int64",
        "__int128",
        "__m64",
        "__m128",
        "__m128i",
        "__m128d",
        "__m256",
        "__m256i",
        "__m256d",
        "__m512",
        "__m512i",
        "__m512d",
        "__fastcall",
        "__cdecl",
        "__stdcall",
        "__thiscall",
        "__usercall",
        "__noreturn",
        "_BYTE",
        "_WORD",
        "_DWORD",
        "_QWORD",
        "_OWORD",
        "_UNKNOWN",
        // IDA boolean / tbyte primitives. `_BOOL*` does not start with `__`,
        // so without listing it explicitly `is_builtin_type_name` returns
        // false and it leaks as a spurious TypeAlias (232 files in the
        // reference corpus; same class as Ghidra's `undefined*`).
        "_BOOL",
        "_BOOL1",
        "_BOOL2",
        "_BOOL4",
        "_BOOL8",
        "_TBYTE",
        "bool",
        "char",
        "double",
        "float",
        "int",
        "long",
        "short",
        "signed",
        "unsigned",
        "void",
        "size_t",
        "ssize_t",
        "ptrdiff_t",
        "intptr_t",
        "uintptr_t",
        "DWORD",
        "QWORD",
        "WORD",
        "BYTE",
    ]
    .into_iter()
    .collect()
});

static IDA_CALL_MACROS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "BYTE1",
        "BYTE2",
        "BYTE3",
        "BYTE4",
        "BYTE5",
        "BYTE6",
        "BYTE7",
        "COERCE_DOUBLE",
        "COERCE_FLOAT",
        "COERCE_LONG_DOUBLE",
        "COERCE_UNSIGNED_INT",
        "COERCE_UNSIGNED_INT64",
        "DWORD1",
        "DWORD2",
        "HIDWORD",
        "HIWORD",
        "HIBYTE",
        "JUMPOUT",
        "LOBYTE",
        "LODWORD",
        "LOWORD",
        "SHIDWORD",
        "SHIWORD",
        "SHIBYTE",
        "SLOBYTE",
        "SLODWORD",
        "SLOWORD",
        "WORD1",
        "WORD2",
        "WORD3",
        "__CFADD__",
        "__CFSUB__",
        "__OFADD__",
        "__OFSUB__",
        "__PAIR16__",
        "__PAIR32__",
        "__PAIR64__",
        "__ROL1__",
        "__ROL2__",
        "__ROL4__",
        "__ROL8__",
        "__ROR1__",
        "__ROR2__",
        "__ROR4__",
        "__ROR8__",
    ]
    .into_iter()
    .collect()
});

/// IDA/Hex-Rays compiler intrinsics that PRINT like calls but lower to a
/// single instruction — they have no symbol and must never become `Calls`
/// references. The enumerated `IDA_CALL_MACROS` denylist cannot keep up with
/// these open-ended families, which generate ~18k bogus, never-resolvable
/// call edges across the reference corpus (8.7k `_mm_*`, 6.2k
/// `__readfsqword`, ~3k `_Interlocked*`). Every branch is anchored to an
/// intrinsic-only namespace, so no real call is dropped:
/// - `_mm*_…`           SSE/AVX vector intrinsics
/// - `__{read,write}{f,g}s{byte,word,dword,qword}`  segment/canary access
/// - `_Interlocked*`    MS interlocked atomics
/// - `_byteswap_*`, `_BitScan*`, `_bittest*`, `_mul128`/`_umul128`
/// - `_ReadStatusReg`, `__break`/`__debugbreak`, ARM barrier/exclusive
///   access intrinsics
/// - `__attribute__` / `__declspec` syntax artifacts that can appear in
///   decompiled signatures
/// - `S?(BYTE|WORD|DWORD)<n>`   bit/byte slice macros (incl. signed/high idx)
/// - `__S?PAIR<n>__`, `__RO[LR]<n>__`, `COERCE_*`   pair/rotate/type-pun macros
static IDA_INTRINSIC_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:_mm[0-9]*_[a-z]|__(?:read|write)[fg]s(?:byte|word|dword|qword)$|_Interlocked|_byteswap_|_BitScan|_bittest(?:64)?$|_u?mul128$|_ReadStatusReg$|__(?:break|debugbreak|und|dmb|ldrex|strex|ldaxr|stlxr|rev16|clz|lzcnt|attribute__|declspec)$|S?(?:BYTE|WORD|DWORD)[0-9]+$|__S?PAIR(?:8|16|32|64|128)__$|__RO[LR][0-9]+__$|COERCE_)",
    )
    .expect("valid regex")
});

static TYPE_QUALIFIER_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "class",
        "const",
        "enum",
        "extern",
        "far",
        "near",
        "register",
        "static",
        "struct",
        "union",
        "volatile",
        "__hidden",
        "__ptr64",
        "__restrict",
        "__unaligned",
    ]
    .into_iter()
    .collect()
});

static IDA_NAME_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)^\.?sub_[0-9a-f]+(?:__[0-9a-f]{8,16})?\.c$").expect("valid regex"),
        Regex::new(
            r"(?i)^\.?(?:j_|nullsub_|locret_|loc_|off_|qword_|dword_|word_|byte_|stru_|unk_|asc_|a)[0-9a-f_]*.*\.c$",
        )
        .expect("valid regex"),
        Regex::new(r"^\.?_[A-Z0-9].*\.c$").expect("valid regex"),
        Regex::new(r"^\.[^.].*\.c$").expect("valid regex"),
    ]
});

// --- isIdaGeneratedC content probes ---
static ADDRESS_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*//\s*Address:\s*0x[0-9a-f]+").expect("valid regex"));
static DISASSEMBLY_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*//\s*Disassembly:").expect("valid regex"));
static RESOLVED_TARGET_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^\s*//\s*Resolved target:").expect("valid regex"));
static IDA_INT_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b__(?:int(?:8|16|32|64|128)|fastcall|usercall|noreturn)\b").expect("valid regex")
});
static IDA_WORD_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:_QWORD|_DWORD|_WORD|_BYTE|_OWORD|_BOOL[1248]?|_TBYTE|BYREF)\b")
        .expect("valid regex")
});
static STACK_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[[re]?[sb]p[+-][0-9A-F]+h\]").expect("valid regex"));
static LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bLABEL_[0-9]+\b").expect("valid regex"));
static SUB_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsub_[0-9A-Fa-f]+\s*\(").expect("valid regex"));

// --- extraction regexes ---
/// TS: `/^\/\/\s*Name:\s*(.+?)\s*$/m`
static NAME_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*Name:\s*(.+?)\s*$").expect("valid regex"));
/// TS: `/^\/\/\s*Name:/m`
static NAME_HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*Name:").expect("valid regex"));
/// TS: `/^\/\/\s*Resolved target:\s*(.+?)\s*$/m`
static RESOLVED_TARGET_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*Resolved target:\s*(.+?)\s*$").expect("valid regex"));
/// TS: `/^\/\/\s*Resolved target:/m`
static RESOLVED_TARGET_HEAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*Resolved target:").expect("valid regex"));
/// `// Address: 0xNNN` header — the symbol's virtual address.
static ADDRESS_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^//\s*Address:\s*0x([0-9A-Fa-f]+)").expect("valid regex"));
/// `// Size: N bytes` / `// Function size: N bytes` header.
static SIZE_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^//\s*(?:Function )?[Ss]ize:\s*([0-9]+)\s*bytes").expect("valid regex")
});
/// `// Target type:` — the real signature of a thunk/trampoline (the only
/// place a thunk's type info lives; often `<no type info>`).
static TARGET_TYPE_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^//\s*Target type:\s*(.+?)\s*$").expect("valid regex"));
/// IDA global data symbols: address-named (`off_`/`dword_`/`qword_`/`byte_`/
/// `word_`/`unk_`/`stru_`/`asc_`/`xmmword_`/`flt_`/`dbl_`/`jpt_` + hex) and
/// named vtables (`<Class>_vtable`). Code labels (`loc_`/`sub_`) are excluded.
static DATA_SYMBOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b((?:off|dword|qword|byte|word|unk|stru|asc|xmmword|flt|dbl|jpt|algn|funcs|tbyte)_[0-9A-Fa-f]+|[A-Za-z_][A-Za-z0-9_]*_vtable)\b",
    )
    .expect("valid regex")
});
/// C string literal `"…"` (with escapes). Used to surface format/UI strings.
static STRING_LITERAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(?:[^"\\]|\\.)*""#).expect("valid regex"));
/// Identifier tokens in a signature prefix: `/[A-Za-z_.$~][A-Za-z0-9_.$:~]*/g`
static NAME_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_.$~][A-Za-z0-9_.$:~]*").expect("valid regex"));
/// Call sites:
/// `/(?:\b[A-Za-z_][A-Za-z0-9_]*::)*~?[A-Za-z_][A-Za-z0-9_]*|(?:\.[A-Za-z_][A-Za-z0-9_.$]*)/g`
static CALL_PATTERN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:\b[A-Za-z_][A-Za-z0-9_]*::)*~?[A-Za-z_][A-Za-z0-9_]*|(?:\.[A-Za-z_][A-Za-z0-9_.$]*)",
    )
    .expect("valid regex")
});
/// TS: `/^\s*\(/` (text immediately after a candidate call name)
static CALL_PAREN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\(").expect("valid regex"));
/// TS: `/\/\*.*?\*\//g`
static BLOCK_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/\*.*?\*/").expect("valid regex"));
/// TS: `/\b(?:const|volatile)\s+(?=[*&])/g` — the `regex` crate has no
/// lookahead, so we consume the `*`/`&` and re-insert it via `$1`
/// (string-for-string equivalent on every input).
static CONST_VOLATILE_PTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:const|volatile)\s+([*&])").expect("valid regex"));
/// TS: `/[()[\],;*&]+/g`
static TYPE_PUNCT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[()\[\],;*&]+").expect("valid regex"));
/// TS: `/[A-Za-z_][A-Za-z0-9_]*(?:::[~A-Za-z_][A-Za-z0-9_]*)*/g`
static TYPE_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:::[~A-Za-z_][A-Za-z0-9_]*)*").expect("valid regex")
});
/// TS: `/^u?int(?:8|16|32|64|128)?_t$/`
static STDINT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^u?int(?:8|16|32|64|128)?_t$").expect("valid regex"));
/// TS: `/\s*\/\/.*$/` (strip a trailing line comment)
static TRAILING_LINE_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*//.*$").expect("valid regex"));
/// TS: `/^(?:return|if|for|while|switch|goto|break|continue|else|do)\b/`
static STATEMENT_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:return|if|for|while|switch|goto|break|continue|else|do)\b")
        .expect("valid regex")
});
/// TS: `/\(\s*(?:__\w+\s+)?\*+\s*[A-Za-z_][A-Za-z0-9_]*\s*\)/`
static FUNCTION_POINTER_LIKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\(\s*(?:__[0-9A-Za-z_]+\s+)?\*+\s*[A-Za-z_][A-Za-z0-9_]*\s*\)")
        .expect("valid regex")
});
/// TS: `/^(.*?\(\s*(?:__\w+\s+)?\*+\s*)([A-Za-z_][A-Za-z0-9_]*)(\s*\).*)$/`
static FUNCTION_POINTER_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*?\(\s*(?:__[0-9A-Za-z_]+\s+)?\*+\s*)([A-Za-z_][A-Za-z0-9_]*)(\s*\).*)$")
        .expect("valid regex")
});
/// TS: `/^(.*?)([A-Za-z_][A-Za-z0-9_]*)(\s*(?:\[[^\]]+\])*)$/`
static DECLARATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*?)([A-Za-z_][A-Za-z0-9_]*)(\s*(?:\[[^\]]+\])*)$").expect("valid regex")
});
/// TS: `/\s+/g`
static WS_COLLAPSE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));
/// EXTERNAL IMPORT banner body: `extern <type> NAME(/* … */);`. Used as the
/// signature for the otherwise body-less import node.
static EXTERN_DECL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(extern\s+.+;)\s*$").expect("valid regex"));
/// RAW DISASSEMBLY FALLBACK failure reason → function docstring.
static DISASM_FAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^//\s*Hex-Rays decompilation failed:\s*(.+?)\s*$").expect("valid regex")
});
/// RAW DISASSEMBLY instruction `call <target>` inside a `//` comment. Captures
/// the symbol operand (optionally `cs:`-prefixed); register/indirect targets
/// are filtered against [`X86_REGISTERS`].
static DISASM_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*//\s+0x[0-9A-Fa-f]+\s+call\s+(?:cs:)?([A-Za-z_][A-Za-z0-9_]*)")
        .expect("valid regex")
});
/// RAW DISASSEMBLY instruction `lea reg, <symbol>` — an address-taken data or
/// function reference (stack `[rsp+…]` operands start with `[` and don't match).
static DISASM_LEA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*//\s+0x[0-9A-Fa-f]+\s+lea\s+\w+,\s*(?:cs:)?([A-Za-z_][A-Za-z0-9_]*)")
        .expect("valid regex")
});
/// A function passed BY NAME as an argument (callback): `sub_XXXX` immediately
/// followed by `,` or `)` rather than `(`. The `__cxa_atexit(sub_…, …)` /
/// qsort-comparator pattern — a real fn→fn edge the `name(` call scan drops.
static FN_POINTER_ARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(sub_[0-9A-Fa-f]+)\s*[,)]").expect("valid regex"));
static IDA_MEMORY_DEREF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\s*\(\s*([A-Za-z_][A-Za-z0-9_:]*(?:\s+[A-Za-z_][A-Za-z0-9_:]*)?)\s*\*\s*\)\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*([+-])\s*(0x[0-9A-Fa-f]+|[0-9A-Fa-f]+h|[0-9]+)\s*\)")
        .expect("valid regex")
});
static IDA_CFG_LABEL_DEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(LABEL_[0-9]+)\s*:").expect("valid regex"));
static IDA_CFG_GOTO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgoto\s+(LABEL_[0-9]+)\s*;").expect("valid regex"));
static IDA_CFG_SWITCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bswitch\s*\(").expect("valid regex"));

/// x86-64 register names — excluded as `call`/`lea` targets when mining raw
/// disassembly (a register operand is an indirect call/load with no symbol).
static X86_REGISTERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "rip", "eax", "ebx", "ecx", "edx",
        "esi", "edi", "ebp", "esp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15", "r8d",
        "r9d", "r10d", "r11d", "r12d", "r13d", "r14d", "r15d", "ax", "bx", "cx", "dx", "si", "di",
        "bp", "sp", "al", "bl", "cl", "dl", "ah", "bh", "ch", "dh", "sil", "dil", "bpl", "spl",
    ]
    .into_iter()
    .collect()
});

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

/// `path.basename(p)` for the forward-slash paths codegraph uses.
fn basename(file_path: &str) -> &str {
    file_path.rsplit('/').next().unwrap_or(file_path)
}

/// `path.extname(p)` — from the last `.` of the basename to the end; empty if
/// there is no `.` or the only `.` is the basename's first character.
fn extname(file_path: &str) -> &str {
    let base = basename(file_path);
    match base.rfind('.') {
        Some(idx) if idx > 0 => &base[idx..],
        _ => "",
    }
}

fn simple_name(name: &str) -> &str {
    name.split("::")
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or(name)
}

fn is_builtin_type_name(name: &str) -> bool {
    IDA_TYPE_WORDS.contains(name)
        || TYPE_QUALIFIER_WORDS.contains(name)
        || STDINT_RE.is_match(name)
        || name.starts_with("__")
}

/// Demangle an Itanium-mangled `_Z…` symbol to its human qualified name WITHOUT
/// parameters or return type (`_ZN2QT10QByteArrayaSERKS0_` →
/// `QT::QByteArray::operator=`), so a mangled node unifies with the demangled
/// call sites in other functions' bodies. Covariant/virtual `_ZThn` thunk
/// prefixes are stripped to the underlying target. Returns `None` for
/// non-mangled or unparseable names (callers keep the raw symbol).
pub(crate) fn demangle_name(name: &str) -> Option<String> {
    let raw = name.strip_prefix('.').unwrap_or(name);
    if !raw.starts_with("_Z") {
        return None;
    }
    let opts = DemangleOptions::new().no_params().no_return_type();
    let demangled = Symbol::new(raw).ok()?.demangle_with_options(&opts).ok()?;
    let cleaned = demangled
        .strip_prefix("non-virtual thunk to ")
        .or_else(|| demangled.strip_prefix("virtual thunk to "))
        .map(str::to_string)
        .unwrap_or(demangled);
    Some(cleaned)
}

/// Full demangled signature WITH parameters (no return type) for use as the
/// node signature: `_ZN2QT10QByteArrayaSERKS0_` →
/// `QT::QByteArray::operator=(QT::QByteArray const&)`.
fn demangle_full(name: &str) -> Option<String> {
    let raw = name.strip_prefix('.').unwrap_or(name);
    if !raw.starts_with("_Z") {
        return None;
    }
    let opts = DemangleOptions::new().no_return_type();
    Symbol::new(raw).ok()?.demangle_with_options(&opts).ok()
}

/// Virtual address embedded in an IDA address-named symbol (`sub_<HEX>`,
/// `loc_<HEX>`, `locret_<HEX>`, optionally leading-dot). `nullsub_N` / `j_<name>`
/// are indices/labels, not addresses, so they're left to the `// Address:`
/// header instead.
fn address_from_name(name: &str) -> Option<u64> {
    let n = name.strip_prefix('.').unwrap_or(name);
    for prefix in ["sub_", "locret_", "loc_"] {
        if let Some(rest) = n.strip_prefix(prefix) {
            let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if !hex.is_empty() {
                return u64::from_str_radix(&hex, 16).ok();
            }
        }
    }
    None
}

/// Virtual address embedded in an address-named data symbol (`off_8A5020` →
/// `0x8A5020`). Named symbols without a hex tail (`Foo_vtable`) return `None`.
fn data_address_from_name(name: &str) -> Option<u64> {
    let tail = name.rsplit('_').next()?;
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(tail, 16).ok()
}

/// Count top-level function bodies — a depth-0 `{` whose preceding significant
/// character is `)` (a signature's parameter list). Skips string/char literals
/// and `//` / `/* */` comments so braces inside them don't miscount. Used only
/// to detect (and warn about) multi-function files, which this single-function
/// extractor cannot fully model.
fn count_function_bodies(source: &str) -> usize {
    let bytes = source.as_bytes();
    let (mut depth, mut count, mut i) = (0i32, 0usize, 0usize);
    let (mut line_comment, mut block_comment, mut string, mut chr) = (false, false, false, false);
    let mut last_significant = 0u8;
    while i < bytes.len() {
        let b = bytes[i];
        if line_comment {
            if b == b'\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_comment {
            if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if string {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                string = false;
            }
            i += 1;
            continue;
        }
        if chr {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'\'' {
                chr = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                line_comment = true;
                i += 2;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                block_comment = true;
                i += 2;
                continue;
            }
            b'"' => string = true,
            b'\'' => chr = true,
            b'{' => {
                if depth == 0 && last_significant == b')' {
                    count += 1;
                }
                depth += 1;
            }
            b'}' => depth = (depth - 1).max(0),
            _ => {}
        }
        if !b.is_ascii_whitespace() {
            last_significant = b;
        }
        i += 1;
    }
    count
}

/// A `call`/`lea` operand that is a register or assembler size/segment keyword
/// rather than a symbol — these denote indirect/computed targets with no name.
fn is_asm_nonsymbol(tok: &str) -> bool {
    X86_REGISTERS.contains(tok)
        || matches!(
            tok,
            "qword"
                | "dword"
                | "word"
                | "byte"
                | "short"
                | "near"
                | "far"
                | "ptr"
                | "offset"
                | "cs"
                | "ds"
                | "ss"
                | "es"
                | "fs"
                | "gs"
        )
}

/// Byte index of the `>` matching the `<` at `open`, accounting for nesting.
/// `None` when unbalanced (e.g. an `operator<` whose `<` has no closer).
fn matching_angle(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, ch) in s[open..].char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Remove balanced `<…>` template-argument spans from a signature prefix so the
/// name tokenizer cannot walk INTO them (`std::string::_S_construct<char
/// const*>` must yield `_S_construct`, not `const`). Unbalanced `<` — as in the
/// comparison operators `operator<` / `operator<<` — is left intact.
fn strip_template_args(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let ch = s[i..].chars().next().expect("char boundary");
        if ch == '<' {
            if let Some(close) = matching_angle(s, i) {
                i = close + 1; // skip the whole balanced <…>
                continue;
            }
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Extract a function's qualified name from its signature prefix (the text
/// before the parameter `(`). Strips template arguments first, then takes the
/// last identifier token — and, for operator overloads, re-attaches the trailing
/// operator symbol that the identifier regex stops on, so `std::string::operator=`
/// is not truncated to `operator` and `operator==`/`operator[]` survive intact.
fn qualified_name_from_prefix(prefix: &str) -> Option<String> {
    let stripped = strip_template_args(prefix);
    let last = NAME_TOKEN_RE.find_iter(&stripped).last()?;
    let token = last.as_str();
    if token == "operator" || token.ends_with("::operator") {
        let tail = stripped[last.end()..].trim();
        if !tail.is_empty() {
            return Some(format!("{token}{tail}"));
        }
    }
    Some(token.to_string())
}

/// Detect C-like files produced by IDA/Hex-Rays dump scripts. These are often
/// close to C, but include invalid C identifiers (e.g. `.mysql_init`) and very
/// large one-function outputs that are better handled with a lightweight pass.
pub fn is_ida_generated_c(file_path: &str, source: &str) -> bool {
    let ext = extname(file_path).to_lowercase();
    if !IDA_DUMP_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }

    let base_name = basename(file_path);
    let mut sample_end = IDA_SAMPLE_BYTES.min(source.len());
    while !source.is_char_boundary(sample_end) {
        sample_end -= 1;
    }
    let sample = &source[..sample_end];

    if sample.contains("THUNK / TRAMPOLINE")
        // EXTERNAL IMPORT banners are brace-less `extern …;` stubs with no
        // __fastcall/_QWORD/sub_( markers, so the name+content heuristic below
        // misses all ~3,386 of them; the banner is the unambiguous signal.
        || sample.contains("EXTERNAL IMPORT")
        // RAW DISASSEMBLY FALLBACK bodies are pure `//` comments (every call is
        // `call sub_X` with no parens), so STACK_REF/SUB_CALL probes miss them.
        || sample.contains("RAW DISASSEMBLY FALLBACK")
        || sample.contains("Hex-Rays decompilation failed")
        || (ADDRESS_COMMENT_RE.is_match(sample) && DISASSEMBLY_COMMENT_RE.is_match(sample))
        || RESOLVED_TARGET_COMMENT_RE.is_match(sample)
    {
        return true;
    }

    if has_strong_ida_content_marker(sample) {
        return true;
    }

    let looks_like_dump_name = IDA_NAME_PATTERNS.iter().any(|p| p.is_match(base_name));
    if !looks_like_dump_name {
        return false;
    }

    IDA_INT_KEYWORD_RE.is_match(sample)
        || IDA_WORD_TYPE_RE.is_match(sample)
        || STACK_REF_RE.is_match(sample)
        || LABEL_RE.is_match(sample)
        || SUB_CALL_RE.is_match(sample)
}

fn has_strong_ida_content_marker(sample: &str) -> bool {
    let has_ida_type_surface =
        IDA_INT_KEYWORD_RE.is_match(sample) || IDA_WORD_TYPE_RE.is_match(sample);
    let has_decompiler_only_surface = STACK_REF_RE.is_match(sample)
        || LABEL_RE.is_match(sample)
        || SUB_CALL_RE.is_match(sample)
        || contains_ida_intrinsic_like_call(sample);
    has_ida_type_surface && has_decompiler_only_surface
}

fn contains_ida_intrinsic_like_call(sample: &str) -> bool {
    for m in CALL_PATTERN_RE.find_iter(sample) {
        let name = m.as_str();
        if !(IDA_CALL_MACROS.contains(name) || IDA_INTRINSIC_CALL_RE.is_match(name)) {
            continue;
        }
        if CALL_PAREN_RE.is_match(&sample[m.end()..]) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
struct FunctionInfo {
    name: String,
    qualified_name: String,
    /// `Function` by default; `Method` for a demangled C++ member (has `::`).
    kind: NodeKind,
    signature: Option<String>,
    return_type: Option<String>,
    parameters: Vec<TypedSymbol>,
    line: u32,
    column: u32,
    /// `None` mirrors the TS `-1` sentinel (`indexOf` miss).
    body_start_index: Option<usize>,
    /// RAW DISASSEMBLY FALLBACK failure reason, surfaced as the node docstring.
    docstring: Option<String>,
    /// Virtual address from `// Address:` or the `sub_<HEX>` name.
    address: Option<u64>,
    /// Byte size from `// Size:` / `// Function size:`.
    size: Option<u32>,
}

#[derive(Debug, Clone)]
struct TypedSymbol {
    name: String,
    type_text: String,
    line: u32,
    column: u32,
    signature: String,
}

#[derive(Debug, Clone)]
struct TypeRef {
    name: String,
    qualified_name: String,
    original: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryAccessMode {
    Read,
    Write,
}

impl MemoryAccessMode {
    fn edge_kind(self) -> EdgeKind {
        match self {
            MemoryAccessMode::Read => EdgeKind::Reads,
            MemoryAccessMode::Write => EdgeKind::Writes,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            MemoryAccessMode::Read => "read",
            MemoryAccessMode::Write => "write",
        }
    }
}

pub struct IdaCExtractor<'a> {
    file_path: String,
    source: &'a str,
    language: Language,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    line_starts: Vec<usize>,
    type_node_ids: HashMap<String, String>,
    /// symbol name → DataSymbol node id (file-independent, so the same global
    /// across files collapses to one canonical node via INSERT OR REPLACE).
    data_node_ids: HashMap<String, String>,
    /// string content → StringLiteral node id (content-hashed, file-independent).
    string_node_ids: HashMap<String, String>,
}

impl<'a> IdaCExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str, language: Language) -> Self {
        // TS computes lineStarts lazily; eager here (no behavioral difference).
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == 10 {
                line_starts.push(i + 1);
            }
        }
        IdaCExtractor {
            file_path: file_path.into(),
            source,
            language,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            line_starts,
            type_node_ids: HashMap::new(),
            data_node_ids: HashMap::new(),
            string_node_ids: HashMap::new(),
        }
    }

    pub fn extract(mut self) -> ExtractionResult {
        let start_time = std::time::Instant::now();

        let file_node = self.create_file_node();
        let file_node_id = file_node.id.clone();
        self.nodes.push(file_node);

        if let Some(info) = self.extract_function_info() {
            let func_node = self.create_function_node(&info);
            let func_node_id = func_node.id.clone();
            self.nodes.push(func_node);
            self.edges.push(Edge::new(
                file_node_id,
                func_node_id.clone(),
                EdgeKind::Contains,
            ));
            self.add_type_references(
                &func_node_id,
                info.return_type.as_deref(),
                EdgeKind::Returns,
                info.line,
                info.column,
            );
            self.create_parameter_nodes(&func_node_id, &info);
            self.extract_local_variables(&func_node_id, &info);
            let mut self_names: HashSet<String> = HashSet::new();
            self_names.insert(info.name.clone());
            self_names.insert(info.qualified_name.clone());

            // THUNK/TRAMPOLINE: the forwarding target is an ALIAS, not a real
            // call. Emit an `Aliases` edge and suppress the body's forwarding
            // `target(…)` call so hot library functions aren't credited with a
            // caller per thunk.
            if let Some(target) = self.match_line_value(&RESOLVED_TARGET_VALUE_RE) {
                if target != "<no type info>" {
                    // Demangle the (often mangled) target so it resolves to the
                    // target node, which also lives in demangled space.
                    let target = demangle_name(&target).unwrap_or(target);
                    let line = self
                        .line_of_comment_value(&RESOLVED_TARGET_HEAD_RE)
                        .unwrap_or(1);
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: func_node_id.clone(),
                        reference_name: target.clone(),
                        reference_kind: EdgeKind::Aliases,
                        line,
                        column: 0,
                        file_path: None,
                        language: None,
                        candidates: None,
                        metadata: None,
                    });
                    self_names.insert(target);
                }
            }

            self.extract_calls(&func_node_id, &self_names);
            // Callbacks passed by name (`__cxa_atexit(sub_X, …)`).
            self.extract_callback_args(&func_node_id, &self_names);
            // Global data symbols (off_/dword_/qword_/…/_vtable) + string
            // literals → DataSymbol / StringLiteral nodes with Reads/Writes/
            // References edges (the cross-function data-coupling surface).
            self.extract_data_symbols(&func_node_id, info.body_start_index);
            self.extract_memory_accesses(&func_node_id, info.body_start_index);
            self.extract_string_literals(&func_node_id, info.body_start_index);
            self.extract_call_argument_role_facts(&func_node_id);
            self.extract_ida_cfg_facts(&func_node_id, info.body_start_index);
            // RAW DISASSEMBLY FALLBACK: the body is `//` comments, so the call
            // graph for these functions lives only in the disassembly text.
            if self.source.contains("RAW DISASSEMBLY FALLBACK")
                || self.source.contains("Hex-Rays decompilation failed")
            {
                self.mine_disasm_edges(&func_node_id, &self_names);
            }
        }

        // Defensive: this extractor models ONE function per file (true for 100%
        // of the IDA dump corpus). A whole-program file — e.g. unlace's own
        // concatenated output, which upstream code splits per-function before
        // it reaches here — would otherwise be silently truncated to its first
        // function. Surface that as a warning instead of losing symbols.
        let body_count = count_function_bodies(self.source);
        if body_count > 1 {
            self.errors.push(ExtractionError {
                message: format!(
                    "IDA extractor saw {body_count} top-level functions but models one per file; only the first was extracted (split the file upstream)"
                ),
                file_path: Some(self.file_path.clone()),
                line: None,
                column: None,
                severity: Severity::Warning,
                code: Some("ida_multi_function".to_string()),
            });
        }

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start_time.elapsed().as_millis() as f64,
        }
    }

    fn create_file_node(&self) -> Node {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let mut node = Node::new(
            format!("file:{}", self.file_path),
            NodeKind::File,
            basename(&self.file_path),
            self.file_path.clone(),
            self.file_path.clone(),
            self.language,
            1,
            lines.len() as u32,
        );
        node.start_column = 0;
        node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        // The file node spans the whole source by definition. Function nodes
        // stay byte-less: this extractor tracks line/column only.
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.is_exported = Some(false);
        node.updated_at = now_ms();
        node
    }

    fn create_function_node(&self, info: &FunctionInfo) -> Node {
        let mut node = Node::new(
            generate_node_id(&self.file_path, info.kind, &info.qualified_name, info.line),
            info.kind,
            info.name.clone(),
            info.qualified_name.clone(),
            self.file_path.clone(),
            self.language,
            info.line,
            self.source.split('\n').count() as u32,
        );
        node.start_column = info.column;
        node.end_column = 0;
        node.signature = info.signature.clone();
        node.docstring = info.docstring.clone();
        node.address = info.address;
        node.size = info.size;
        node.updated_at = now_ms();
        node
    }

    fn extract_function_info(&self) -> Option<FunctionInfo> {
        let comment_name = self.match_line_value(&NAME_VALUE_RE);
        let signature_info = self.extract_signature_info();
        let fallback_name = self.name_from_file_path();
        let raw_name = comment_name
            .or_else(|| signature_info.as_ref().map(|s| s.qualified_name.clone()))
            .or(fallback_name)?;

        let (address, size) = self.parse_address_and_size(&raw_name);

        // Itanium demangling: a mangled `_Z…` name becomes its human qualified
        // form so this node unifies with the demangled call sites elsewhere;
        // demangled C++ members (with `::`) are modeled as `Method` nodes.
        let demangled = demangle_name(&raw_name);
        let kind = match &demangled {
            Some(d) if d.contains("::") => NodeKind::Method,
            _ => NodeKind::Function,
        };
        let qualified_name = demangled.unwrap_or_else(|| raw_name.clone());

        // Thunks/trampolines carry their real signature only in `// Target
        // type:` (the body is a placeholder forwarding stub).
        let target_type = self
            .match_line_value(&TARGET_TYPE_VALUE_RE)
            .filter(|t| t != "<no type info>");
        // Signature precedence: thunk Target type > full demangled signature >
        // parsed body signature > EXTERNAL IMPORT extern-declaration line.
        let signature = target_type
            .clone()
            .or_else(|| demangle_full(&raw_name))
            .or_else(|| signature_info.as_ref().and_then(|s| s.signature.clone()))
            .or_else(|| self.match_line_value(&EXTERN_DECL_RE));
        let return_type = target_type
            .as_deref()
            .and_then(|t| t.split('(').next())
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .or_else(|| signature_info.as_ref().and_then(|s| s.return_type.clone()));

        Some(FunctionInfo {
            name: simple_name(&qualified_name).to_string(),
            qualified_name,
            kind,
            signature,
            return_type,
            parameters: signature_info
                .as_ref()
                .map(|s| s.parameters.clone())
                .unwrap_or_default(),
            line: signature_info
                .as_ref()
                .map(|s| s.line)
                .or_else(|| self.line_of_comment_value(&NAME_HEAD_RE))
                .unwrap_or(1),
            column: signature_info.as_ref().map(|s| s.column).unwrap_or(0),
            body_start_index: signature_info
                .as_ref()
                .and_then(|s| s.body_start_index)
                .or_else(|| self.source.find('{')),
            // RAW DISASSEMBLY FALLBACK: the "why decompilation failed" reason.
            docstring: self.match_line_value(&DISASM_FAIL_RE),
            address,
            size,
        })
    }

    /// Virtual address (`// Address:` header, else the `sub_<HEX>` name) and
    /// byte size (`// Size:` / `// Function size:`) of the function.
    fn parse_address_and_size(&self, name: &str) -> (Option<u64>, Option<u32>) {
        let address = ADDRESS_VALUE_RE
            .captures(self.source)
            .and_then(|c| u64::from_str_radix(c.get(1)?.as_str(), 16).ok())
            .or_else(|| address_from_name(name));
        let size = SIZE_VALUE_RE
            .captures(self.source)
            .and_then(|c| c.get(1)?.as_str().parse::<u32>().ok());
        (address, size)
    }

    fn extract_signature_info(&self) -> Option<FunctionInfo> {
        let brace_index = self.source.find('{')?;

        let before_body = &self.source[..brace_index];
        let lines: Vec<&str> = before_body.split('\n').collect();
        let mut first_signature_line: Option<usize> = None;
        let mut signature_lines: Vec<&str> = Vec::new();

        for (i, l) in lines.iter().enumerate() {
            let trimmed = l.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.starts_with("/*") || trimmed.starts_with('*') {
                continue;
            }

            if first_signature_line.is_none() {
                first_signature_line = Some(i);
            }
            signature_lines.push(trimmed);
        }

        let first_signature_line = first_signature_line?;
        if signature_lines.is_empty() {
            return None;
        }

        let signature = WS_COLLAPSE_RE
            .replace_all(&signature_lines.join(" "), " ")
            .trim()
            .to_string();
        let paren_index = signature.find('(')?;

        let prefix = signature[..paren_index].trim();
        let qualified_name = qualified_name_from_prefix(prefix)?;
        if IDA_TYPE_WORDS.contains(qualified_name.as_str()) {
            return None;
        }

        let name = simple_name(&qualified_name).to_string();
        let return_type = prefix[..prefix.rfind(&qualified_name).unwrap_or(0)]
            .trim()
            .to_string();
        let parameters =
            self.extract_parameters(&signature, paren_index, (first_signature_line + 1) as u32);

        let source_line = lines[first_signature_line];
        let column = source_line.find(|c: char| !c.is_whitespace()).unwrap_or(0) as u32;

        Some(FunctionInfo {
            name,
            qualified_name,
            // Intermediate value; the final kind is decided in
            // extract_function_info after demangling.
            kind: NodeKind::Function,
            signature: Some(signature),
            return_type: Some(return_type),
            parameters,
            line: (first_signature_line + 1) as u32,
            column,
            body_start_index: Some(brace_index),
            docstring: None,
            address: None,
            size: None,
        })
    }

    fn create_parameter_nodes(&mut self, function_node_id: &str, info: &FunctionInfo) {
        for param in &info.parameters {
            let id = generate_node_id(
                &self.file_path,
                NodeKind::Parameter,
                &format!("{}:{}", info.qualified_name, param.name),
                param.line,
            );
            let mut node = Node::new(
                id.clone(),
                NodeKind::Parameter,
                param.name.clone(),
                format!("{}::{}", info.qualified_name, param.name),
                self.file_path.clone(),
                self.language,
                param.line,
                param.line,
            );
            node.start_column = param.column;
            node.end_column = param.column + param.name.len() as u32;
            node.signature = Some(param.signature.clone());
            node.updated_at = now_ms();
            self.nodes.push(node);
            self.edges
                .push(Edge::new(function_node_id, id.clone(), EdgeKind::Contains));
            self.add_type_references(
                &id,
                Some(&param.type_text),
                EdgeKind::TypeOf,
                param.line,
                param.column,
            );
        }
    }

    fn extract_local_variables(&mut self, function_node_id: &str, info: &FunctionInfo) {
        let Some(body_start_index) = info.body_start_index else {
            return;
        };

        let body_line = self.line_column_at(body_start_index).0;
        let source = self.source;
        let body = &source[body_start_index + 1..];
        let mut locals_created = 0usize;
        let mut saw_declaration = false;

        for (i, raw_line) in body.split('\n').enumerate() {
            let line = body_line + i as u32 + 1;
            let trimmed = raw_line.trim();

            if trimmed.is_empty() || trimmed.starts_with("//") {
                continue;
            }

            let local = parse_local_declaration(raw_line, line);
            let Some(local) = local else {
                if saw_declaration || !trimmed.starts_with('}') {
                    break;
                }
                continue;
            };

            saw_declaration = true;
            locals_created += 1;
            if locals_created > MAX_IDA_LOCAL_VARIABLES {
                self.errors.push(ExtractionError {
                    message: format!(
                        "IDA local variable extraction capped at {}",
                        MAX_IDA_LOCAL_VARIABLES
                    ),
                    file_path: Some(self.file_path.clone()),
                    line: None,
                    column: None,
                    severity: Severity::Warning,
                    code: Some("ida_local_limit".to_string()),
                });
                break;
            }

            let id = generate_node_id(
                &self.file_path,
                NodeKind::Variable,
                &format!("{}:{}", info.qualified_name, local.name),
                local.line,
            );
            let mut node = Node::new(
                id.clone(),
                NodeKind::Variable,
                local.name.clone(),
                format!("{}::{}", info.qualified_name, local.name),
                self.file_path.clone(),
                self.language,
                local.line,
                local.line,
            );
            node.start_column = local.column;
            node.end_column = local.column + local.name.len() as u32;
            node.signature = Some(local.signature.clone());
            node.updated_at = now_ms();
            self.nodes.push(node);
            self.edges
                .push(Edge::new(function_node_id, id.clone(), EdgeKind::Contains));
            self.add_type_references(
                &id,
                Some(&local.type_text),
                EdgeKind::TypeOf,
                local.line,
                local.column,
            );
        }
    }

    fn extract_calls(&mut self, from_node_id: &str, self_names: &HashSet<String>) {
        // The thunk `// Resolved target:` is handled in `extract` as an
        // `Aliases` edge (and added to `self_names`), so it is intentionally
        // not emitted here as a `Calls`.
        let mut seen: HashSet<String> = HashSet::new();
        let source = self.source;

        for m in CALL_PATTERN_RE.find_iter(source) {
            let name = m.as_str();
            let index = m.start();
            if self.is_in_line_comment(index) {
                continue;
            }
            if !CALL_PAREN_RE.is_match(&source[index + name.len()..]) {
                continue;
            }
            if CONTROL_WORDS.contains(name) || IDA_TYPE_WORDS.contains(name) {
                continue;
            }
            if IDA_CALL_MACROS.contains(name) {
                continue;
            }
            if IDA_INTRINSIC_CALL_RE.is_match(name) {
                continue;
            }
            // A mangled callee (e.g. an EXTERNAL IMPORT body's own `_Z…(…)`,
            // or a `_Z` call) is demangled so it both resolves to the target's
            // demangled node and is correctly recognized as a self-call.
            let callee = demangle_name(name).unwrap_or_else(|| name.to_string());
            if self_names.contains(name) || self_names.contains(&callee) {
                continue;
            }

            let (line, column) = self.line_column_at(index);
            let metadata = self.call_argument_roles_metadata(name, index);
            self.add_call_ref(from_node_id, &callee, line, column, &mut seen, metadata);
        }
    }

    /// Capture functions passed BY NAME as arguments (callbacks / comparators):
    /// `__cxa_atexit(sub_X, …)`, `qsort(.., sub_cmp)`. These are real fn→fn
    /// relationships the `name(` call scan misses — the name is followed by `,`
    /// or `)`, not `(`. Modeled as `References` (address-taken), not `Calls`.
    fn extract_callback_args(&mut self, from_node_id: &str, self_names: &HashSet<String>) {
        let source = self.source;
        let mut seen: HashSet<String> = HashSet::new();
        for caps in FN_POINTER_ARG_RE.captures_iter(source) {
            let m = caps.get(1).expect("group 1");
            let name = m.as_str();
            if self.is_in_line_comment(m.start()) || self_names.contains(name) {
                continue;
            }
            if !seen.insert(name.to_string()) {
                continue;
            }
            let (line, column) = self.line_column_at(m.start());
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_node_id.to_string(),
                reference_name: name.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }

    /// Mine `call`/`lea` edges from a RAW DISASSEMBLY FALLBACK body. Hex-Rays
    /// emitted the disassembly as `//` comments — which the normal call scan
    /// skips — so without this pass these 63 functions are call-graph black
    /// holes. `call <sym>` → `Calls`; `lea reg, <sym>` → `References`
    /// (address-taken). Register/size-keyword operands are not symbols.
    fn mine_disasm_edges(&mut self, from_node_id: &str, self_names: &HashSet<String>) {
        let source = self.source;
        let mut seen_calls: HashSet<String> = HashSet::new();
        for caps in DISASM_CALL_RE.captures_iter(source) {
            let m = caps.get(1).expect("group 1");
            let target = m.as_str();
            if is_asm_nonsymbol(target) || self_names.contains(target) {
                continue;
            }
            if !seen_calls.insert(target.to_string()) {
                continue;
            }
            let (line, column) = self.line_column_at(m.start());
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_node_id.to_string(),
                reference_name: target.to_string(),
                reference_kind: EdgeKind::Calls,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
        let mut seen_refs: HashSet<String> = HashSet::new();
        for caps in DISASM_LEA_RE.captures_iter(source) {
            let m = caps.get(1).expect("group 1");
            let target = m.as_str();
            if is_asm_nonsymbol(target) || self_names.contains(target) {
                continue;
            }
            if !seen_refs.insert(target.to_string()) {
                continue;
            }
            let (line, column) = self.line_column_at(m.start());
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_node_id.to_string(),
                reference_name: target.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }

    /// Extract global data-symbol references from the function body. Each
    /// `off_`/`dword_`/`qword_`/…/`_vtable` symbol becomes a (shared, file-
    /// independent) DataSymbol node, and the access becomes a `Reads`, `Writes`
    /// (assignment target), or `References` (address-taken) edge — the
    /// cross-function data-coupling that "where is this global written?" needs.
    fn extract_data_symbols(&mut self, from_node_id: &str, body_start: Option<usize>) {
        let Some(body_start) = body_start else { return };
        let source = self.source;
        let mut seen: HashSet<(String, EdgeKind)> = HashSet::new();
        let hits: Vec<(String, usize, EdgeKind)> = DATA_SYMBOL_RE
            .captures_iter(&source[body_start..])
            .filter_map(|c| {
                let m = c.get(1).expect("group 1");
                let abs = body_start + m.start();
                if self.is_in_line_comment(abs) {
                    return None;
                }
                let kind = self.classify_data_access(abs, body_start + m.end());
                Some((m.as_str().to_string(), abs, kind))
            })
            .collect();
        for (name, abs, kind) in hits {
            if !seen.insert((name.clone(), kind)) {
                continue;
            }
            let data_id = self.ensure_data_node(&name);
            let mut edge = Edge::new(from_node_id, data_id, kind);
            let (line, column) = self.line_column_at(abs);
            edge.line = Some(line);
            edge.column = Some(column);
            self.edges.push(edge);
        }
    }

    fn extract_memory_accesses(&mut self, from_node_id: &str, body_start: Option<usize>) {
        let Some(body_start) = body_start else { return };
        let source = self.source;
        let mut seen: HashSet<(String, EdgeKind)> = HashSet::new();
        let hits: Vec<(String, usize, Metadata, EdgeKind)> = IDA_MEMORY_DEREF_RE
            .captures_iter(&source[body_start..])
            .filter_map(|caps| {
                let full = caps.get(0).expect("group 0");
                let abs = body_start + full.start();
                if self.is_in_line_comment(abs) {
                    return None;
                }
                let type_text = caps.get(1)?.as_str().trim();
                let base = caps.get(2)?.as_str();
                let sign = caps.get(3)?.as_str();
                let raw_offset = caps.get(4)?.as_str();
                let offset_value = parse_ida_int(raw_offset)?;
                let signed_offset = if sign == "-" {
                    -offset_value
                } else {
                    offset_value
                };
                let name = format_memory_symbol_name(base, signed_offset, raw_offset, sign);
                let mode = match self.classify_data_access(abs, body_start + full.end()) {
                    EdgeKind::Writes => MemoryAccessMode::Write,
                    _ => MemoryAccessMode::Read,
                };
                let mut metadata = Metadata::new();
                metadata.insert("kind".to_string(), serde_json::json!("memory_access"));
                metadata.insert("type".to_string(), serde_json::json!(type_text));
                metadata.insert("base".to_string(), serde_json::json!(base));
                metadata.insert("offset".to_string(), serde_json::json!(signed_offset));
                metadata.insert("rawOffset".to_string(), serde_json::json!(raw_offset));
                metadata.insert("mode".to_string(), serde_json::json!(mode.as_str()));
                Some((name, abs, metadata, mode.edge_kind()))
            })
            .collect();
        for (name, abs, metadata, kind) in hits {
            if !seen.insert((name.clone(), kind)) {
                continue;
            }
            let data_id = self.ensure_data_node(&name);
            let mut edge = Edge::new(from_node_id, data_id, kind);
            let (line, column) = self.line_column_at(abs);
            edge.line = Some(line);
            edge.column = Some(column);
            edge.metadata = Some(metadata);
            self.edges.push(edge);
        }
    }

    fn extract_call_argument_role_facts(&mut self, from_node_id: &str) {
        let source = self.source;
        let hits: Vec<(String, usize, Metadata)> = CALL_PATTERN_RE
            .find_iter(source)
            .filter_map(|m| {
                if self.is_in_line_comment(m.start()) {
                    return None;
                }
                self.call_argument_roles_metadata(m.as_str(), m.start())
                    .map(|metadata| (m.as_str().to_string(), m.start(), metadata))
            })
            .collect();
        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        for (callee, abs, metadata) in hits {
            let (line, column) = self.line_column_at(abs);
            if !seen.insert((callee.clone(), line, column)) {
                continue;
            }
            let node_id = self.ensure_data_node(&format!("callarg:{callee}:{line}:{column}"));
            let mut edge = Edge::new(from_node_id, node_id, EdgeKind::References);
            edge.line = Some(line);
            edge.column = Some(column);
            edge.metadata = Some(metadata);
            self.edges.push(edge);
        }
    }

    fn extract_ida_cfg_facts(&mut self, from_node_id: &str, body_start: Option<usize>) {
        let Some(body_start) = body_start else { return };
        let source = self.source;
        let body = &source[body_start..];
        let label_hits: Vec<(String, usize)> = IDA_CFG_LABEL_DEF_RE
            .captures_iter(body)
            .filter_map(|caps| {
                let m = caps.get(1)?;
                Some((m.as_str().to_string(), body_start + m.start()))
            })
            .collect();
        for (label, abs) in label_hits {
            self.add_cfg_fact(
                from_node_id,
                &format!("label:{label}"),
                abs,
                "label",
                Some(&label),
            );
        }

        let goto_hits: Vec<(String, usize)> = IDA_CFG_GOTO_RE
            .captures_iter(body)
            .filter_map(|caps| {
                let m = caps.get(1)?;
                Some((m.as_str().to_string(), body_start + m.start()))
            })
            .collect();
        for (label, abs) in goto_hits {
            self.add_cfg_fact(
                from_node_id,
                &format!("label:{label}"),
                abs,
                "goto",
                Some(&label),
            );
        }

        let switch_hits: Vec<usize> = IDA_CFG_SWITCH_RE
            .find_iter(body)
            .map(|m| body_start + m.start())
            .collect();
        for abs in switch_hits {
            let (line, _) = self.line_column_at(abs);
            self.add_cfg_fact(from_node_id, &format!("switch:{line}"), abs, "switch", None);
        }

        let jump_hits: Vec<(String, usize)> = DATA_SYMBOL_RE
            .captures_iter(body)
            .filter_map(|caps| {
                let m = caps.get(1)?;
                let name = m.as_str();
                name.starts_with("jpt_")
                    .then(|| (name.to_string(), body_start + m.start()))
            })
            .collect();
        for (name, abs) in jump_hits {
            self.add_cfg_fact(from_node_id, &name, abs, "jump_table", Some(&name));
        }
    }

    fn add_cfg_fact(
        &mut self,
        from_node_id: &str,
        target_name: &str,
        abs: usize,
        role: &str,
        label: Option<&str>,
    ) {
        if self.is_in_line_comment(abs) {
            return;
        }
        let target_id = self.ensure_data_node(target_name);
        let (line, column) = self.line_column_at(abs);
        let mut metadata = Metadata::new();
        metadata.insert("kind".to_string(), serde_json::json!("ida_cfg"));
        metadata.insert("role".to_string(), serde_json::json!(role));
        if let Some(label) = label {
            metadata.insert("label".to_string(), serde_json::json!(label));
        }
        let mut edge = Edge::new(from_node_id, target_id, EdgeKind::References);
        edge.line = Some(line);
        edge.column = Some(column);
        edge.metadata = Some(metadata);
        self.edges.push(edge);
    }

    /// Classify a data-symbol access at byte range `[start, end)`: `&sym` is
    /// address-taken (`References`); `sym = …` / `sym[i] = …` / `sym op= …` is a
    /// `Writes`; everything else (loads, comparisons, args) is a `Reads`.
    fn classify_data_access(&self, start: usize, end: usize) -> EdgeKind {
        if self.source[..start].trim_end().ends_with('&') {
            return EdgeKind::References;
        }
        let mut rest = self.source[end..].trim_start();
        // Skip a trailing `[…]` index so `sym[i] = …` still classifies as write.
        while let Some(stripped) = rest.strip_prefix('[') {
            match stripped.find(']') {
                Some(close) => rest = stripped[close + 1..].trim_start(),
                None => break,
            }
        }
        let b = rest.as_bytes();
        let plain_assign = b.first() == Some(&b'=') && b.get(1) != Some(&b'=');
        let compound_assign = matches!(
            b.first(),
            Some(b'+' | b'-' | b'*' | b'/' | b'&' | b'|' | b'^' | b'%')
        ) && b.get(1) == Some(&b'=');
        if plain_assign || compound_assign {
            EdgeKind::Writes
        } else {
            EdgeKind::Reads
        }
    }

    /// Get-or-create the canonical DataSymbol node for `name`. The id is
    /// file-independent (`data_symbol:<name>`), so the same global referenced
    /// from many functions/files collapses to one row via INSERT OR REPLACE,
    /// and every Reads/Writes edge lands on the same target.
    fn ensure_data_node(&mut self, name: &str) -> String {
        if let Some(id) = self.data_node_ids.get(name) {
            return id.clone();
        }
        let id = format!("data_symbol:{name}");
        self.data_node_ids.insert(name.to_string(), id.clone());
        let mut node = Node::new(
            id.clone(),
            NodeKind::DataSymbol,
            name,
            name,
            self.file_path.clone(),
            self.language,
            1,
            1,
        );
        node.address = data_address_from_name(name);
        node.updated_at = now_ms();
        self.nodes.push(node);
        self.edges.push(Edge::new(
            format!("file:{}", self.file_path),
            id.clone(),
            EdgeKind::Contains,
        ));
        id
    }

    /// Extract string/format literals from the function body as StringLiteral
    /// nodes (content-hashed, file-independent) with `References` edges.
    fn extract_string_literals(&mut self, from_node_id: &str, body_start: Option<usize>) {
        let Some(body_start) = body_start else { return };
        let source = self.source;
        let hits: Vec<(String, usize)> = STRING_LITERAL_RE
            .find_iter(&source[body_start..])
            .filter_map(|m| {
                let abs = body_start + m.start();
                if self.is_in_line_comment(abs) {
                    return None;
                }
                Some((m.as_str().to_string(), abs))
            })
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut count = 0usize;
        for (literal, abs) in hits {
            // Drop the surrounding quotes for the content key.
            let content = &literal[1..literal.len().saturating_sub(1)];
            if content.is_empty() || !seen.insert(content.to_string()) {
                continue;
            }
            count += 1;
            if count > MAX_IDA_STRINGS {
                break;
            }
            let string_id = self.ensure_string_node(content);
            let mut edge = Edge::new(from_node_id, string_id, EdgeKind::References);
            let (line, column) = self.line_column_at(abs);
            edge.line = Some(line);
            edge.column = Some(column);
            self.edges.push(edge);
        }
    }

    /// Get-or-create the canonical StringLiteral node for `content`. The id is
    /// a content hash, so identical strings across functions/files unify.
    fn ensure_string_node(&mut self, content: &str) -> String {
        if let Some(id) = self.string_node_ids.get(content) {
            return id.clone();
        }
        let hash = sha256_hex(content.as_bytes());
        let id = format!("string_literal:{}", &hash[..32]);
        self.string_node_ids.insert(content.to_string(), id.clone());
        // A readable, bounded display name (the raw bytes are the qualified name).
        let display: String = content.chars().take(80).collect();
        let mut node = Node::new(
            id.clone(),
            NodeKind::StringLiteral,
            display,
            content,
            self.file_path.clone(),
            self.language,
            1,
            1,
        );
        node.updated_at = now_ms();
        self.nodes.push(node);
        self.edges.push(Edge::new(
            format!("file:{}", self.file_path),
            id.clone(),
            EdgeKind::Contains,
        ));
        id
    }

    fn add_call_ref(
        &mut self,
        from_node_id: &str,
        reference_name: &str,
        line: u32,
        column: u32,
        seen: &mut HashSet<String>,
        metadata: Option<Metadata>,
    ) {
        let cleaned_name = reference_name.trim();
        if cleaned_name.is_empty() || cleaned_name == "<no type info>" {
            return;
        }
        if !seen.insert(cleaned_name.to_string()) {
            return;
        }

        self.unresolved_references.push(UnresolvedReference {
            from_node_id: from_node_id.to_string(),
            reference_name: cleaned_name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            column,
            file_path: None,
            language: None,
            candidates: None,
            metadata,
        });
    }

    fn call_argument_roles_metadata(&self, raw_name: &str, name_start: usize) -> Option<Metadata> {
        let callee = simple_name(raw_name.trim_start_matches('.'));
        let roles = call_argument_roles(callee)?;
        let after_name = name_start + raw_name.len();
        let rest = &self.source[after_name..];
        let open_rel = rest.find('(')?;
        if !rest[..open_rel].trim().is_empty() {
            return None;
        }
        let open = after_name + open_rel;
        let close = find_matching_paren(self.source, open)?;
        let args = split_top_level(&self.source[open + 1..close], ',');
        let mut arg_values = Vec::new();
        for (index, role) in roles_for_args(&roles, args.len()).into_iter().enumerate() {
            let Some(role) = role else { continue };
            let expr = args.get(index).copied().unwrap_or_default().trim();
            arg_values.push(serde_json::json!({
                "index": index,
                "role": role,
                "expr": expr,
            }));
        }
        if arg_values.is_empty() {
            return None;
        }
        let mut metadata = Metadata::new();
        metadata.insert("kind".to_string(), serde_json::json!("call_argument_roles"));
        metadata.insert("callee".to_string(), serde_json::json!(callee));
        metadata.insert(
            "arguments".to_string(),
            serde_json::Value::Array(arg_values),
        );
        Some(metadata)
    }

    fn add_type_references(
        &mut self,
        from_node_id: &str,
        type_text: Option<&str>,
        reference_kind: EdgeKind,
        line: u32,
        column: u32,
    ) {
        // TS: `if (!typeText) return` — empty string is falsy.
        let Some(type_text) = type_text.filter(|t| !t.is_empty()) else {
            return;
        };
        for type_ref in extract_type_refs(type_text) {
            self.ensure_type_node(&type_ref, line, column);
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_node_id.to_string(),
                reference_name: type_ref.qualified_name.clone(),
                reference_kind,
                line,
                column,
                file_path: None,
                language: None,
                candidates: None,
                metadata: None,
            });
        }
    }

    fn ensure_type_node(&mut self, type_ref: &TypeRef, line: u32, column: u32) -> String {
        if let Some(existing) = self.type_node_ids.get(&type_ref.qualified_name) {
            return existing.clone();
        }

        let id = generate_node_id(
            &self.file_path,
            NodeKind::TypeAlias,
            &type_ref.qualified_name,
            1,
        );
        self.type_node_ids
            .insert(type_ref.qualified_name.clone(), id.clone());

        let mut node = Node::new(
            id.clone(),
            NodeKind::TypeAlias,
            type_ref.name.clone(),
            type_ref.qualified_name.clone(),
            self.file_path.clone(),
            self.language,
            line,
            line,
        );
        node.start_column = column;
        node.end_column = column + type_ref.name.len() as u32;
        node.signature = Some(type_ref.original.clone());
        node.updated_at = now_ms();
        self.nodes.push(node);
        self.edges.push(Edge::new(
            format!("file:{}", self.file_path),
            id.clone(),
            EdgeKind::Contains,
        ));
        id
    }

    fn extract_parameters(
        &self,
        signature: &str,
        open_paren: usize,
        line: u32,
    ) -> Vec<TypedSymbol> {
        let Some(close_paren) = find_matching_paren(signature, open_paren) else {
            return Vec::new();
        };

        let params_text = signature[open_paren + 1..close_paren].trim();
        if params_text.is_empty() || params_text == "void" {
            return Vec::new();
        }

        split_top_level(params_text, ',')
            .into_iter()
            .filter_map(|raw_param| parse_typed_declarator(raw_param, line, 0))
            .collect()
    }

    fn match_line_value(&self, pattern: &Regex) -> Option<String> {
        pattern
            .captures(self.source)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn line_of_comment_value(&self, pattern: &Regex) -> Option<u32> {
        let m = pattern.find(self.source)?;
        Some(self.line_column_at(m.start()).0)
    }

    fn name_from_file_path(&self) -> Option<String> {
        let ext = extname(&self.file_path);
        let base = basename(&self.file_path);
        // path.basename(p, ext) strips a matching suffix unless base === ext.
        let base = if !ext.is_empty() && base.len() > ext.len() && base.ends_with(ext) {
            &base[..base.len() - ext.len()]
        } else {
            base
        };
        if base.is_empty() {
            None
        } else {
            Some(base.to_string())
        }
    }

    fn line_column_at(&self, index: usize) -> (u32, u32) {
        let starts = &self.line_starts;
        let mut low: i64 = 0;
        let mut high: i64 = starts.len() as i64 - 1;

        while low <= high {
            let mid = (low + high) / 2;
            let start = starts[mid as usize];
            let next = starts.get(mid as usize + 1).copied().unwrap_or(usize::MAX);
            if index < start {
                high = mid - 1;
            } else if index >= next {
                low = mid + 1;
            } else {
                return ((mid + 1) as u32, (index - start) as u32);
            }
        }

        (1, index as u32)
    }

    fn is_in_line_comment(&self, index: usize) -> bool {
        let line_start = self.source[..index].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let comment_start = self.source[line_start..].find("//").map(|i| i + line_start);
        matches!(comment_start, Some(c) if c < index)
    }
}

fn parse_local_declaration(raw_line: &str, line: u32) -> Option<TypedSymbol> {
    let without_comment = TRAILING_LINE_COMMENT_RE.replace(raw_line, "");
    let trimmed = without_comment.trim();
    if !trimmed.ends_with(';') {
        return None;
    }
    if STATEMENT_KEYWORD_RE.is_match(trimmed) {
        return None;
    }

    let mut declaration = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
    if declaration.is_empty() {
        return None;
    }

    if let Some(assignment_index) = find_top_level_char(declaration, '=') {
        declaration = declaration[..assignment_index].trim();
    }

    let function_pointer_like = FUNCTION_POINTER_LIKE_RE.is_match(declaration);
    if (declaration.contains('(') || declaration.contains(')')) && !function_pointer_like {
        return None;
    }

    let column = raw_line
        .find(|c: char| !c.is_whitespace())
        .map(|c| c as u32)
        .unwrap_or(0);
    parse_typed_declarator(declaration, line, column)
}

fn parse_typed_declarator(raw: &str, line: u32, column: u32) -> Option<TypedSymbol> {
    let text = raw.trim();
    if text.is_empty() || text == "void" || text == "..." {
        return None;
    }

    if let Some(function_pointer) = FUNCTION_POINTER_DECL_RE.captures(text) {
        let combined = format!(
            "{}{}",
            function_pointer.get(1).expect("group 1").as_str(),
            function_pointer.get(3).expect("group 3").as_str()
        );
        let type_text = WS_COLLAPSE_RE
            .replace_all(&combined, " ")
            .trim()
            .to_string();
        let name = function_pointer.get(2).expect("group 2").as_str();
        return Some(TypedSymbol {
            name: name.to_string(),
            type_text: type_text.clone(),
            line,
            column,
            signature: format!("{} {}", type_text, name),
        });
    }

    let caps = DECLARATOR_RE.captures(text)?;

    let mut type_text = caps.get(1).expect("group 1").as_str().trim().to_string();
    let name = caps.get(2).expect("group 2").as_str();
    let array_suffix = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("");

    if type_text.is_empty() || is_builtin_type_name(name) {
        return None;
    }
    if !array_suffix.is_empty() {
        type_text = format!("{}{}", type_text, array_suffix);
    }

    Some(TypedSymbol {
        name: name.to_string(),
        type_text: type_text.clone(),
        line,
        column,
        signature: format!("{} {}", type_text, name).trim().to_string(),
    })
}

fn extract_type_refs(type_text: &str) -> Vec<TypeRef> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut refs: Vec<TypeRef> = Vec::new();
    let cleaned = BLOCK_COMMENT_RE.replace_all(type_text, " ");
    let cleaned = CONST_VOLATILE_PTR_RE.replace_all(&cleaned, " $1");
    let cleaned = TYPE_PUNCT_RE.replace_all(&cleaned, " ");

    for m in TYPE_TOKEN_RE.find_iter(&cleaned) {
        let qualified_name = m.as_str();
        let name = simple_name(qualified_name);
        if is_builtin_type_name(qualified_name) || is_builtin_type_name(name) {
            continue;
        }
        if seen.insert(qualified_name.to_string()) {
            refs.push(TypeRef {
                name: name.to_string(),
                qualified_name: qualified_name.to_string(),
                original: type_text.trim().to_string(),
            });
        }
    }
    refs
}

fn parse_ida_int(raw: &str) -> Option<i64> {
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(hex) = raw.strip_suffix('h').or_else(|| raw.strip_suffix('H')) {
        return i64::from_str_radix(hex, 16).ok();
    }
    raw.parse::<i64>().ok()
}

fn format_memory_symbol_name(
    base: &str,
    signed_offset: i64,
    raw_offset: &str,
    sign: &str,
) -> String {
    if signed_offset == 0 {
        return format!("mem:{base}");
    }
    if signed_offset > 0 {
        format!("mem:{base}+{signed_offset}")
    } else {
        let magnitude = signed_offset.checked_abs().unwrap_or(i64::MAX);
        if sign == "-" {
            format!("mem:{base}-{magnitude}")
        } else {
            format!("mem:{base}+{raw_offset}")
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CallRoleProfile {
    MemCopy,
    StrCopy,
    Sprintf,
    QRead,
    Free,
    New,
    Realloc,
}

fn call_argument_roles(callee: &str) -> Option<CallRoleProfile> {
    match callee {
        "memcpy" | "qmemcpy" | "memmove" => Some(CallRoleProfile::MemCopy),
        "strcpy" => Some(CallRoleProfile::StrCopy),
        "sprintf" => Some(CallRoleProfile::Sprintf),
        "qlread" | "qfread" => Some(CallRoleProfile::QRead),
        "free" => Some(CallRoleProfile::Free),
        "new" => Some(CallRoleProfile::New),
        "realloc" => Some(CallRoleProfile::Realloc),
        _ => None,
    }
}

fn roles_for_args(profile: &CallRoleProfile, arg_count: usize) -> Vec<Option<&'static str>> {
    let mut roles = vec![None; arg_count];
    let mut set = |index: usize, role: &'static str| {
        if let Some(slot) = roles.get_mut(index) {
            *slot = Some(role);
        }
    };
    match profile {
        CallRoleProfile::MemCopy => {
            set(0, "write_dst");
            set(1, "read_src");
            set(2, "size");
        }
        CallRoleProfile::StrCopy => {
            set(0, "write_dst");
            set(1, "read_src");
        }
        CallRoleProfile::Sprintf => {
            set(0, "write_dst");
            set(1, "format");
            for index in 2..arg_count {
                set(index, "format_arg");
            }
        }
        CallRoleProfile::QRead => {
            set(0, "handle");
            set(1, "write_dst");
            set(2, "size");
        }
        CallRoleProfile::Free => {
            set(0, "freed_ptr");
        }
        CallRoleProfile::New => {
            set(0, "alloc_size");
        }
        CallRoleProfile::Realloc => {
            set(0, "readwrite_ptr");
            set(1, "alloc_size");
        }
    }
    roles
}

fn find_matching_paren(text: &str, open_paren: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, ch) in text[open_paren..].char_indices() {
        if ch == '(' {
            depth += 1;
        }
        if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(open_paren + i);
            }
        }
    }
    None
}

fn split_top_level(text: &str, delimiter: char) -> Vec<&str> {
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut bracket_depth = 0i32;

    for (i, ch) in text.char_indices() {
        if ch == '(' {
            paren_depth += 1;
        } else if ch == ')' {
            paren_depth = (paren_depth - 1).max(0);
        } else if ch == '<' {
            angle_depth += 1;
        } else if ch == '>' {
            angle_depth = (angle_depth - 1).max(0);
        } else if ch == '[' {
            bracket_depth += 1;
        } else if ch == ']' {
            bracket_depth = (bracket_depth - 1).max(0);
        } else if ch == delimiter && paren_depth == 0 && angle_depth == 0 && bracket_depth == 0 {
            parts.push(text[start..i].trim());
            start = i + ch.len_utf8();
        }
    }

    parts.push(text[start..].trim());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

fn find_top_level_char(text: &str, target: char) -> Option<usize> {
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    for (i, ch) in text.char_indices() {
        if ch == '(' {
            paren_depth += 1;
        } else if ch == ')' {
            paren_depth = (paren_depth - 1).max(0);
        } else if ch == '[' {
            bracket_depth += 1;
        } else if ch == ']' {
            bracket_depth = (bracket_depth - 1).max(0);
        } else if ch == target && paren_depth == 0 && bracket_depth == 0 {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(path: &str, source: &str) -> ExtractionResult {
        IdaCExtractor::new(path, source, Language::C).extract()
    }

    #[test]
    fn extracts_leading_dot_thunk_functions_and_alias_target() {
        let code = "\n// ============================================================\n// THUNK / TRAMPOLINE\n// ============================================================\n// Address:         0x19E10\n// Name:            .mysql_init\n// Disassembly:\n//   0x19E10 jmp     cs:off_23E6F0\n//\n// Resolved target: mysql_init\n// Target address:  0x2442C0\n// Target type:     MYSQL *(MYSQL *)\n// ============================================================\n\nvoid .mysql_init(/* see target signature */)\n{\n    return mysql_init(/* forwarded args */);\n}\n";

        assert!(is_ida_generated_c("lumina/all/.mysql_init.c", code));

        let result = extract("lumina/all/.mysql_init.c", code);
        let thunk = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("function node");
        assert_eq!(thunk.name, ".mysql_init");
        assert_eq!(thunk.address, Some(0x19E10));
        // `// Target type:` is the thunk's real signature.
        assert_eq!(thunk.signature.as_deref(), Some("MYSQL *(MYSQL *)"));

        // The forwarding target is an Aliases edge, NOT a Calls (and the body's
        // forwarding `mysql_init(…)` is suppressed, so it isn't double-counted).
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "mysql_init" && r.reference_kind == EdgeKind::Aliases),
            "alias edge missing: {:?}",
            result.unresolved_references
        );
        assert!(
            !result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "mysql_init" && r.reference_kind == EdgeKind::Calls),
            "thunk target double-counted as a call"
        );
    }

    #[test]
    fn data_symbols_become_nodes_with_read_write_edges() {
        let code = "__int64 __fastcall sub_5000(__int64 a1)\n{\n  qword_8CC980 = a1;\n  return off_8A5020[*(_DWORD *)(a1 + 8)] + dword_8C1234;\n}\n";
        let result = extract("all/sub_5000.c", code);

        // Three distinct data symbols, each a (shared, address-bearing) node.
        let data: Vec<(&str, Option<u64>)> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::DataSymbol)
            .map(|n| (n.name.as_str(), n.address))
            .collect();
        assert!(data.contains(&("qword_8CC980", Some(0x8CC980))), "{data:?}");
        assert!(data.contains(&("off_8A5020", Some(0x8A5020))), "{data:?}");
        assert!(data.contains(&("dword_8C1234", Some(0x8C1234))), "{data:?}");
        // Node ids are file-independent so cross-file globals unify.
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.id == "data_symbol:qword_8CC980")
        );

        // qword_8CC980 is written; off_/dword_ are read.
        let edge_kinds: Vec<(&str, EdgeKind)> = result
            .edges
            .iter()
            .filter_map(|e| {
                e.target
                    .strip_prefix("data_symbol:")
                    .filter(|_| e.kind != EdgeKind::Contains)
                    .map(|n| (n, e.kind))
            })
            .collect();
        assert!(
            edge_kinds.contains(&("qword_8CC980", EdgeKind::Writes)),
            "{edge_kinds:?}"
        );
        assert!(
            edge_kinds.contains(&("off_8A5020", EdgeKind::Reads)),
            "{edge_kinds:?}"
        );
        assert!(
            edge_kinds.contains(&("dword_8C1234", EdgeKind::Reads)),
            "{edge_kinds:?}"
        );
    }

    #[test]
    fn address_taken_data_symbol_is_a_reference() {
        let code = "void __fastcall sub_6000()\n{\n  __cxa_atexit(sub_1234, &qword_8C9540, &off_85BC80);\n}\n";
        let result = extract("all/sub_6000.c", code);
        let kinds: Vec<(&str, EdgeKind)> = result
            .edges
            .iter()
            .filter_map(|e| {
                e.target
                    .strip_prefix("data_symbol:")
                    .filter(|_| e.kind != EdgeKind::Contains)
                    .map(|n| (n, e.kind))
            })
            .collect();
        assert!(
            kinds.contains(&("qword_8C9540", EdgeKind::References)),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&("off_85BC80", EdgeKind::References)),
            "{kinds:?}"
        );
    }

    #[test]
    fn string_literals_become_nodes() {
        let code = "void __fastcall sub_7000()\n{\n  sub_8000(\"connect failed: %s\");\n  sub_8000(\"connect failed: %s\");\n  sub_9000(\"retrying\");\n}\n";
        let result = extract("all/sub_7000.c", code);
        let strings: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::StringLiteral)
            .map(|n| n.qualified_name.as_str())
            .collect();
        // Duplicate string unifies to one node.
        assert_eq!(
            strings
                .iter()
                .filter(|s| **s == "connect failed: %s")
                .count(),
            1
        );
        assert!(strings.contains(&"retrying"), "{strings:?}");

        let ref_targets: Vec<&str> = result
            .edges
            .iter()
            .filter(|e| e.target.starts_with("string_literal:") && e.kind == EdgeKind::References)
            .map(|e| e.source.as_str())
            .collect();
        assert!(!ref_targets.is_empty(), "no string reference edges");
    }

    #[test]
    fn extracts_hexrays_sub_functions_and_call_references() {
        let code = "\n__int64 __fastcall sub_E2F10(__int64 a1, int a2, unsigned __int8 *a3)\n{\n  __int64 v4; // rax\n\n  v4 = *(int *)(a1 + 144);\n  LODWORD(v4) = 1;\n  if ( (int)v4 + a2 > *(_DWORD *)(a1 + 148) )\n  {\n    if ( (unsigned int)sub_E15B0(a1) )\n      return 0;\n  }\n  return tag_strlen((const char *)a3);\n}\n";

        assert!(is_ida_generated_c("lumina/all/sub_E2F10.c", code));

        let result = extract("lumina/all/sub_E2F10.c", code);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("function node");
        assert_eq!(func.name, "sub_E2F10");

        let call_names: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(call_names.contains(&"sub_E15B0"));
        assert!(call_names.contains(&"tag_strlen"));
        assert!(!call_names.contains(&"if"));
        assert!(!call_names.contains(&"_DWORD"));
        assert!(!call_names.contains(&"LODWORD"));
    }

    #[test]
    fn extracts_ida_parameters_locals_and_type_edges() {
        let code = "\nida_mcp::mcp::Response *__fastcall ida_mcp::tools::debugger::make_response(\n        ida_mcp::tools::debugger *this,\n        ida_mcp::mcp::McpServer *server)\n{\n  ida_mcp::mcp::RequestContext *ctx; // [rsp+0h] [rbp-8h] BYREF\n  int status; // eax\n\n  status = ida_mcp::mcp::build_response(ctx, server);\n  return ctx;\n}\n";

        assert!(is_ida_generated_c(
            "ida_mcp/all/_ZN7ida_mcp5tools8debugger13make_response.c",
            code
        ));

        let result = extract(
            "ida_mcp/all/_ZN7ida_mcp5tools8debugger13make_response.c",
            code,
        );
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("function node");
        assert_eq!(func.name, "make_response");
        assert_eq!(
            func.qualified_name,
            "ida_mcp::tools::debugger::make_response"
        );

        let parameter_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Parameter)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(parameter_names, vec!["this", "server"]);

        let variable_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Variable)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(variable_names, vec!["ctx", "status"]);

        let mut type_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::TypeAlias)
            .map(|n| n.qualified_name.as_str())
            .collect();
        type_names.sort_unstable();
        assert_eq!(
            type_names,
            vec![
                "ida_mcp::mcp::McpServer",
                "ida_mcp::mcp::RequestContext",
                "ida_mcp::mcp::Response",
                "ida_mcp::tools::debugger",
            ]
        );

        let return_refs: Vec<&UnresolvedReference> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Returns)
            .collect();
        assert!(
            return_refs
                .iter()
                .any(|r| r.reference_name == "ida_mcp::mcp::Response")
        );

        let type_refs: Vec<&UnresolvedReference> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::TypeOf)
            .collect();
        assert!(
            type_refs
                .iter()
                .any(|r| r.reference_name == "ida_mcp::mcp::McpServer")
        );
        assert!(
            type_refs
                .iter()
                .any(|r| r.reference_name == "ida_mcp::mcp::RequestContext")
        );

        let call_refs: Vec<&UnresolvedReference> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .collect();
        assert!(
            call_refs
                .iter()
                .any(|r| r.reference_name == "ida_mcp::mcp::build_response")
        );
    }

    #[test]
    fn file_and_function_nodes_are_linked() {
        let code = "__int64 __fastcall sub_1000(__int64 a1)\n{\n  return sub_2000(a1);\n}\n";
        let result = extract("sub_1000.c", code);

        let file_node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .expect("file node");
        assert_eq!(file_node.id, "file:sub_1000.c");
        assert_eq!(file_node.is_exported, Some(false));

        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("function node");
        assert!(result.edges.iter().any(|e| e.source == file_node.id
            && e.target == func.id
            && e.kind == EdgeKind::Contains));

        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "sub_2000")
        );
    }

    #[test]
    fn ordinary_c_files_are_not_detected_as_ida_dumps() {
        assert!(!is_ida_generated_c(
            "src/main.c",
            "int main() { return 0; }\n"
        ));
        // Dump-shaped name but no IDA markers in content.
        assert!(!is_ida_generated_c(
            "lumina/all/sub_E2F10.c",
            "int add(int a, int b) { return a + b; }\n"
        ));
        // Non-C extension never qualifies.
        assert!(!is_ida_generated_c("sub_E2F10.rs", "__int64 __fastcall x"));
    }

    #[test]
    fn compiler_intrinsics_do_not_leak_as_calls() {
        // SIMD, stack-canary, atomics, and slice macros print like calls but
        // lower to single instructions — none should become Calls refs, while
        // real calls (sub_, libc) must survive.
        let code = "__int64 __fastcall sub_ABCDE(__int64 a1, __m128i a2)\n{\n  __int64 v3; // rax\n  v3 = __readfsqword(0x28u);\n  a2 = _mm_add_epi32(a2, a2);\n  _InterlockedAdd(a1, 1);\n  if ( SDWORD2(v3) )\n    v3 = _byteswap_ulong(v3);\n  memcpy(a1, a2, 16);\n  return sub_12345(a1);\n}\n";
        assert!(is_ida_generated_c("lumina/all/sub_ABCDE.c", code));

        let result = extract("lumina/all/sub_ABCDE.c", code);
        let call_names: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .map(|r| r.reference_name.as_str())
            .collect();

        // Real calls survive.
        assert!(call_names.contains(&"sub_12345"), "got: {call_names:?}");
        assert!(call_names.contains(&"memcpy"), "got: {call_names:?}");
        // Intrinsics are filtered.
        for leak in [
            "__readfsqword",
            "_mm_add_epi32",
            "_InterlockedAdd",
            "SDWORD2",
            "_byteswap_ulong",
        ] {
            assert!(
                !call_names.contains(&leak),
                "intrinsic {leak} leaked as a call: {call_names:?}"
            );
        }
    }

    #[test]
    fn ida_bool_types_do_not_leak_as_type_aliases() {
        // `_BOOL*` / `_TBYTE` are IDA primitives; they must not become
        // TypeAlias nodes, but real local variables of that type still do.
        let code = "_BOOL8 __fastcall sub_BEEF(__int64 a1)\n{\n  _BOOL4 v2; // eax\n  _TBYTE v3; // st0\n  v2 = sub_CAFE(a1);\n  return v2;\n}\n";
        assert!(is_ida_generated_c("lumina/all/sub_BEEF.c", code));

        let result = extract("lumina/all/sub_BEEF.c", code);
        let type_alias_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::TypeAlias)
            .map(|n| n.name.as_str())
            .collect();
        for builtin in ["_BOOL8", "_BOOL4", "_TBYTE"] {
            assert!(
                !type_alias_names.contains(&builtin),
                "{builtin} leaked as a TypeAlias: {type_alias_names:?}"
            );
        }
        // The locals themselves are still real variables.
        let var_names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Variable)
            .map(|n| n.name.as_str())
            .collect();
        assert!(var_names.contains(&"v2"), "vars: {var_names:?}");
        assert!(var_names.contains(&"v3"), "vars: {var_names:?}");
    }

    #[test]
    fn function_address_and_size_are_parsed() {
        // From the `sub_<HEX>` name (headerless bulk).
        let code = "__int64 __fastcall sub_1719D0(int a1)\n{\n  return a1;\n}\n";
        let result = extract("all/sub_1719D0.c", code);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .unwrap();
        assert_eq!(func.address, Some(0x1719D0));
        assert_eq!(func.size, None);

        // From the `// Address:` / `// Size:` header (thunk/import banners),
        // which is authoritative over the (mangled) name.
        let thunk = "// ============================================================\n// THUNK / TRAMPOLINE\n// ============================================================\n// Address:         0x113E30\n// Name:            ._Unwind_Resume\n// Size:            6 bytes (1 instructions)\n// Resolved target: _Unwind_Resume\n// ============================================================\n\nvoid ._Unwind_Resume(/* see target signature */)\n{\n    return _Unwind_Resume(/* forwarded args */);\n}\n";
        let result = extract("all/._Unwind_Resume.c", thunk);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .unwrap();
        assert_eq!(func.address, Some(0x113E30));
        assert_eq!(func.size, Some(6));
    }

    #[test]
    fn external_import_files_are_detected_and_extracted() {
        let code = "// ============================================================\n// EXTERNAL IMPORT\n// ============================================================\n// Address:  0x8D5D90\n// Name:     _ZN2QT10QArrayData8allocateEPPS0_xxxNS0_16AllocationOptionE\n// Segment:  extern (SEG_XTRN)\n// Note:     This is an extern symbol resolved at link/load time.\n// ============================================================\n\nextern void _ZN2QT10QArrayData8allocateEPPS0_xxxNS0_16AllocationOptionE(/* see import library declaration */);\n";
        let path = "all/_ZN2QT10QArrayData8allocateEPPS0_xxxNS0_16AllocationOptionE.c";
        assert!(
            is_ida_generated_c(path, code),
            "EXTERNAL IMPORT not detected"
        );

        let result = extract(path, code);
        // The mangled import is demangled into a C++ Method node.
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method)
            .expect("import method node");
        assert_eq!(func.name, "allocate");
        assert_eq!(func.qualified_name, "QT::QArrayData::allocate");
        // The brace-less body must not mint bogus (self-)calls.
        assert!(
            result
                .unresolved_references
                .iter()
                .all(|r| r.reference_kind != EdgeKind::Calls),
            "import body leaked calls: {:?}",
            result.unresolved_references
        );
    }

    #[test]
    fn cpp_mangled_names_are_demangled_to_methods() {
        // A libstdc++ std::string::operator= file (authoritative // Name: header).
        let code = "// Name:            _ZNSsaSEOSs\n\nstd::string *__fastcall foo(std::string *a1, std::string *a2)\n{\n  return a1;\n}\n";
        let result = extract("all/_ZNSsaSEOSs.c", code);
        let m = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method)
            .expect("method node");
        assert_eq!(m.qualified_name, "std::string::operator=");
        assert_eq!(m.name, "operator=");

        // A Qt mangled member resolves through the // Name: header.
        let qt = "// Name:            _ZN2QT10QByteArray6appendEc\n\nvoid __fastcall foo() {}\n";
        let result = extract("all/_ZN2QT10QByteArray6appendEc.c", qt);
        let m = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Method)
            .expect("qt method node");
        assert_eq!(m.qualified_name, "QT::QByteArray::append");
        assert_eq!(m.name, "append");
    }

    #[test]
    fn demangle_name_handles_operators_and_thunks() {
        assert_eq!(
            demangle_name("_ZN2QT10QByteArrayaSERKS0_").as_deref(),
            Some("QT::QByteArray::operator=")
        );
        // Leading-dot thunk variant demangles the same.
        assert_eq!(
            demangle_name("._ZN2QT10QByteArray6appendEc").as_deref(),
            Some("QT::QByteArray::append")
        );
        // Non-mangled names pass through untouched.
        assert_eq!(demangle_name("sub_1234"), None);
        assert_eq!(demangle_name("memcpy"), None);
    }

    #[test]
    fn operator_and_template_names_are_not_truncated() {
        // operator= must not collapse to `operator`.
        let op = "std::string *__fastcall std::string::operator=(std::string *a1, std::string *a2)\n{\n  std::string::swap(a1, a2);\n  return a1;\n}\n";
        let result = extract("all/_ZNSsaSEOSs.c", op);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("fn");
        assert_eq!(func.name, "operator=");
        assert_eq!(func.qualified_name, "std::string::operator=");

        // Template args must not be walked into (`_S_construct<…>` → `const`).
        let tpl = "char *__fastcall std::string::_S_construct<char const*>(char const *a1, char const *a2)\n{\n  return a1;\n}\n";
        let result = extract("all/_ZNSs12_S_construct.c", tpl);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("fn");
        assert_eq!(func.name, "_S_construct");
        assert_eq!(func.qualified_name, "std::string::_S_construct");
    }

    #[test]
    fn callbacks_passed_by_name_become_references() {
        let code = "__int64 __fastcall sub_162150(__int64 a1)\n{\n  __cxa_atexit(sub_16B750, &qword_8C9540, &off_85BC80);\n  return sub_99999(a1);\n}\n";
        let result = extract("all/sub_162150.c", code);

        // The directly-called fn is a Call; the callback is a Reference.
        let refs: Vec<(&str, EdgeKind)> = result
            .unresolved_references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind))
            .collect();
        assert!(refs.contains(&("sub_99999", EdgeKind::Calls)), "{refs:?}");
        assert!(
            refs.contains(&("sub_16B750", EdgeKind::References)),
            "callback ref missing: {refs:?}"
        );
        // sub_16B750 must NOT also be a Call.
        assert!(!refs.contains(&("sub_16B750", EdgeKind::Calls)), "{refs:?}");
    }

    #[test]
    fn raw_disassembly_fallback_mines_call_and_lea_edges() {
        let code = "// ============================================================\n// RAW DISASSEMBLY FALLBACK\n// ============================================================\n// Hex-Rays decompilation failed: call analysis failed\n// Address:        0x17ACE0\n// Name:           sub_17ACE0\n// ============================================================\n//\n// Instructions:\n//   0x17AD13 call    sub_1A1870\n//   0x17AD1C lea     rax, unk_8CD7E8\n//   0x17AD37 call    sub_27E0B0\n//   0x17AD40 call    rax\n//   0x17AD45 call    qword ptr [rbx]\n";
        let path = "all/sub_17ACE0.c";
        assert!(is_ida_generated_c(path, code), "raw-disasm not detected");

        let result = extract(path, code);
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("fn");
        assert_eq!(func.name, "sub_17ACE0");
        assert_eq!(func.docstring.as_deref(), Some("call analysis failed"));

        let calls: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(calls.contains(&"sub_1A1870"), "{calls:?}");
        assert!(calls.contains(&"sub_27E0B0"), "{calls:?}");
        // Indirect targets (register, qword ptr) are not symbols.
        assert!(!calls.contains(&"rax"), "{calls:?}");
        assert!(!calls.contains(&"qword"), "{calls:?}");

        let refs: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::References)
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(refs.contains(&"unk_8CD7E8"), "lea ref missing: {refs:?}");
    }

    #[test]
    fn multi_function_files_warn_instead_of_silently_truncating() {
        // A whole-program-style file with two top-level functions.
        let code = "__int64 __fastcall sub_1000(__int64 a1)\n{\n  return sub_3000(a1);\n}\n\n__int64 __fastcall sub_2000(__int64 a1)\n{\n  return a1;\n}\n";
        let result = extract("all/whole.c", code);
        // First function still extracted.
        assert!(result.nodes.iter().any(|n| n.name == "sub_1000"));
        // …but a warning flags the dropped second function.
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.code.as_deref() == Some("ida_multi_function")),
            "expected multi-function warning: {:?}",
            result.errors
        );

        // Single-function files do NOT warn.
        let single = "__int64 __fastcall sub_1000(__int64 a1)\n{\n  return a1;\n}\n";
        let result = extract("all/sub_1000.c", single);
        assert!(
            result
                .errors
                .iter()
                .all(|e| e.code.as_deref() != Some("ida_multi_function"))
        );
    }

    #[test]
    fn function_pointer_locals_are_parsed() {
        let symbol =
            parse_typed_declarator("void (__cdecl *handler)(int)", 3, 2).expect("declarator");
        assert_eq!(symbol.name, "handler");
        assert_eq!(symbol.type_text, "void (__cdecl *)(int)");
        assert_eq!(symbol.signature, "void (__cdecl *)(int) handler");
    }
}
