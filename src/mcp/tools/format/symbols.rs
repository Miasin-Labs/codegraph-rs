//! Symbol, path, and source-slice formatting primitives.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    FILE_EXT_RE,
    LOW_VALUE_RES,
    QUALIFIER_SPLIT_RE,
    TEST_PATH_DIR_RE,
    TEST_PATH_EXT_RE,
    TOKEN_RE,
    TOKEN_SPLIT_RE,
};
use crate::types::Node;
use crate::utils::lexical_resolve;

/// Last `::` / `.` / `/`-separated segment of a qualified symbol.
pub(in crate::mcp::tools) fn last_qualifier_part(symbol: &str) -> String {
    QUALIFIER_SPLIT_RE
        .split(symbol)
        .filter(|p| !p.is_empty())
        .last()
        .map(|s| s.to_string())
        .unwrap_or_else(|| symbol.to_string())
}

pub(in crate::mcp::tools) fn display_symbol(node: &Node) -> String {
    let base = if node.qualified_name.is_empty() {
        node.name.as_str()
    } else {
        node.qualified_name.as_str()
    };
    if base.contains("::") || base.contains('.') {
        base.to_string()
    } else {
        let file = node.file_path.as_str();
        if file.is_empty() || file == "<unresolved>" {
            base.to_string()
        } else {
            format!("{file}::{base}")
        }
    }
}

/// Prefix each line of a source slice with its 1-based line number, matching
/// the Read tool's `cat -n` convention (number + tab).
pub(in crate::mcp::tools) fn number_source_lines(slice: &str, first_line_number: usize) -> String {
    slice
        .split('\n')
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", first_line_number + i, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `Number.prototype.toLocaleString()` parity for non-negative integers
/// (en-US grouping: `12345` → `"12,345"`).
pub(in crate::mcp::tools) fn to_locale_string(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Largest byte index `<= idx` that is a char boundary of `s`.
pub(in crate::mcp::tools) fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(in crate::mcp::tools) fn truthy_meta_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f != 0.0 && !f.is_nan() {
                Some(n.to_string())
            } else {
                None
            }
        }
        Value::Bool(true) => Some("true".to_string()),
        _ => None,
    }
}

/// Symbol-ish tokens extracted from an explore query (shared by the flow
/// builder and named-symbol seeding — identical pipeline in TS).
pub(in crate::mcp::tools) fn extract_symbol_tokens(query: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for part in TOKEN_SPLIT_RE.split(query) {
        let t = FILE_EXT_RE.replace(part, "").trim().to_string();
        if t.chars().count() >= 3 && TOKEN_RE.is_match(&t) && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out.truncate(16);
    out
}

pub(in crate::mcp::tools) fn is_qualified_token(t: &str) -> bool {
    t.contains('.') || t.contains('/') || t.contains("::")
}

pub(in crate::mcp::tools) fn is_test_path(p: &str) -> bool {
    TEST_PATH_DIR_RE.is_match(p) || TEST_PATH_EXT_RE.is_match(p)
}

pub(in crate::mcp::tools) fn is_low_value(p: &str) -> bool {
    let lp = p.to_lowercase();
    LOW_VALUE_RES.iter().any(|re| re.is_match(&lp))
}

/// `path.resolve(p)` parity for project-root comparisons.
pub(in crate::mcp::tools) fn resolve_path(p: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    lexical_resolve(&cwd, &p.to_string_lossy())
}

/// `localeCompare` approximation: case-insensitive primary, byte-order tiebreak.
pub(in crate::mcp::tools) fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// TS `fileLines.slice(start - 1, end).join('\n')` with clamped bounds.
pub(in crate::mcp::tools) fn slice_lines(
    lines: &[&str],
    start_1based: i64,
    end_1based: i64,
) -> String {
    let s = ((start_1based - 1).max(0) as usize).min(lines.len());
    let e = (end_1based.max(0) as usize).min(lines.len());
    if s >= e {
        String::new()
    } else {
        lines[s..e].join("\n")
    }
}

/// JS `Number(x) || default` (also used where TS does `(x as number) || d`).
pub(in crate::mcp::tools) fn num_or(args: &Map<String, Value>, key: &str, default: f64) -> f64 {
    match args.get(key) {
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(f64::NAN);
            if v != 0.0 && !v.is_nan() { v } else { default }
        }
        Some(Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(v) if v != 0.0 => v,
            _ => default,
        },
        Some(Value::Bool(true)) => 1.0,
        _ => default,
    }
}

// =============================================================================
