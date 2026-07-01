//! UnlaceCExtractor — splits WHOLE-PROGRAM decompiled-C output (one file for an
//! entire binary) into per-function units and runs the IDA extractor on each.
//!
//! Handles two aggregate formats:
//! - **unlace** `/* Function: <name> | Address: 0x.. | … */` block comments.
//! - **IDA `decompile_many()`** `//----- (HEXADDR) ----------` boundary
//!   comments (the raw batch-decompile output; codegraph splits it natively, so
//!   the external `split_decompile_many.py` helper is obsolete).
//!
//! unlace (a Rust decompiler) emits one `.c` file for an entire binary: a large
//! preamble (`_QWORD` typedefs, slice/flag macros, `static inline` SSE helpers,
//! C11 atomics, then global + forward declarations), followed by every function
//! as a `/* Function: <name> | Address: 0x.. | Size: 0x.. (N bytes) |
//! Convention: .. | Xrefs to: a, b | .. */` BLOCK comment plus an IDA-style
//! Hex-Rays body. The single-function [`IdaCExtractor`] would parse only the
//! first function (and choke on the preamble's first `{`), so this extractor:
//!
//! 1. skips the preamble (everything before the first `/* Function:`),
//! 2. for each block, synthesizes a one-function IDA source (`// Name:` /
//!    `// Address:` / `// Size:` LINE-comment headers + the body) and runs
//!    [`IdaCExtractor`] on it,
//! 3. merges every function's nodes/edges under ONE file node, and
//! 4. adds the authoritative `Xrefs to:` list as `Calls` edges.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::ida_c_extractor::IdaCExtractor;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
};

/// The per-function block-comment header. Fields are `|`-separated; only
/// `Function:` (the name) is required, the rest are optional.
static FUNCTION_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)/\*\s*Function:\s*(.*?)\*/").expect("valid regex"));
static ADDRESS_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Address:\s*0x([0-9A-Fa-f]+)").expect("valid regex"));
static SIZE_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Size:[^(]*\(\s*([0-9]+)\s*bytes").expect("valid regex"));
static XREFS_TO_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Xrefs to:\s*([^|]*?)\s*(?:\||$)").expect("valid regex"));
/// IDA `decompile_many()` per-function boundary: `//----- (HEXADDR) ----------`.
/// This is the raw aggregate format IDA's batch decompiler writes; the
/// (now-removed) `split_decompile_many.py` helper used to split it for codegraph.
static DECOMPILE_MANY_BOUNDARY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^//-----\s*\(([0-9A-Fa-f]+)\)\s*-+\s*$").expect("valid regex")
});
/// An identifier immediately before a `(` — a function-name candidate in a
/// signature (mirrors the splitter script's `NAME_RE`).
static SIG_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z_.$~][A-Za-z0-9_.$:~]*)\s*\(").expect("valid regex"));

/// C/IDA type & calling-convention words that are never the function NAME (the
/// `decompile_many` boundary carries no name, so it's recovered from the
/// signature — the last identifier-before-`(` that isn't one of these).
const C_TYPE_WORDS: &[&str] = &[
    "__int8",
    "__int16",
    "__int32",
    "__int64",
    "__int128",
    "__fastcall",
    "__cdecl",
    "__stdcall",
    "__thiscall",
    "__usercall",
    "__noreturn",
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
    "_BYTE",
    "_WORD",
    "_DWORD",
    "_QWORD",
    "_OWORD",
    "_BOOL",
];

/// Recover a function name from its signature (text before the first `{`): the
/// last identifier-before-`(` that isn't a type/convention word, else
/// `sub_<ADDR>`.
fn name_from_signature(code: &str, address: Option<u64>) -> String {
    let before_body = code.split('{').next().unwrap_or(code);
    let cand = SIG_NAME_RE
        .captures_iter(before_body)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .filter(|n| !C_TYPE_WORDS.contains(n))
        .last();
    match (cand, address) {
        (Some(n), _) => n.to_string(),
        (None, Some(a)) => format!("sub_{a:X}"),
        (None, None) => "function".to_string(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

fn line_for_byte(source: &str, byte: usize) -> u32 {
    source[..byte.min(source.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count() as u32
        + 1
}

fn rebase_line(line: u32, delta: i64) -> u32 {
    (i64::from(line) + delta).max(1) as u32
}

/// A whole-program unlace C dump: per-function `/* Function: … */` block
/// comments carrying an `Address:` field. Distinct from IDA's `// Name:`
/// LINE-comment banners (handled by [`IdaCExtractor`]).
pub fn is_unlace_c(_file_path: &str, source: &str) -> bool {
    source.contains("/* Function:") && source.contains("Address: 0x")
}

/// IDA `decompile_many()` aggregate output: every function in one file,
/// separated by `//----- (HEXADDR) ----------` boundary comments. codegraph
/// splits these natively now (the `split_decompile_many.py` helper is obsolete).
pub fn is_ida_decompile_many(source: &str) -> bool {
    source.contains("//----- (") && DECOMPILE_MANY_BOUNDARY_RE.is_match(source)
}

struct FnBlock<'a> {
    name: String,
    address: Option<u64>,
    size: Option<u32>,
    xrefs_to: Vec<String>,
    original_start_line: u32,
    /// Signature + body text following the block comment.
    code: &'a str,
}

pub struct UnlaceCExtractor<'a> {
    file_path: String,
    source: &'a str,
    language: Language,
}

impl<'a> UnlaceCExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str, language: Language) -> Self {
        UnlaceCExtractor {
            file_path: file_path.into(),
            source,
            language,
        }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = ExtractionResult::default();

        let file_node_id = format!("file:{}", self.file_path);
        result.nodes.push(self.file_node(&file_node_id));

        for block in self.split_functions() {
            self.extract_block(&block, &file_node_id, &mut result);
        }

        result.duration_ms = start.elapsed().as_millis() as f64;
        result
    }

    fn file_node(&self, id: &str) -> Node {
        let lines = self.source.split('\n').count().max(1) as u32;
        let mut node = Node::new(
            id.to_string(),
            NodeKind::File,
            self.file_path.rsplit('/').next().unwrap_or(&self.file_path),
            self.file_path.clone(),
            self.file_path.clone(),
            self.language,
            1,
            lines,
        );
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.is_exported = Some(false);
        node.updated_at = now_ms();
        node
    }

    /// Split into function blocks, detecting the format: unlace's
    /// `/* Function: */` block comments, else IDA's `decompile_many()`
    /// `//----- (addr) -----` boundaries.
    fn split_functions(&self) -> Vec<FnBlock<'_>> {
        let unlace = self.split_unlace_blocks();
        if !unlace.is_empty() {
            return unlace;
        }
        self.split_decompile_many()
    }

    /// IDA `decompile_many()`: split on `//----- (addr) -----` boundaries,
    /// recovering each function's name from its signature and address from the
    /// boundary. The boundary line is a `//` comment the body parser skips.
    fn split_decompile_many(&self) -> Vec<FnBlock<'_>> {
        let bounds: Vec<(usize, u64)> = DECOMPILE_MANY_BOUNDARY_RE
            .captures_iter(self.source)
            .filter_map(|c| {
                let start = c.get(0)?.start();
                let addr = u64::from_str_radix(c.get(1)?.as_str(), 16).ok()?;
                Some((start, addr))
            })
            .collect();
        let mut blocks = Vec::with_capacity(bounds.len());
        for (i, &(start, addr)) in bounds.iter().enumerate() {
            let end = bounds.get(i + 1).map(|n| n.0).unwrap_or(self.source.len());
            let code = &self.source[start..end];
            blocks.push(FnBlock {
                name: name_from_signature(code, Some(addr)),
                address: Some(addr),
                size: None,
                xrefs_to: Vec::new(),
                original_start_line: line_for_byte(self.source, start),
                code,
            });
        }
        blocks
    }

    /// unlace `/* Function: … */` blocks (preamble before the first one is
    /// ignored).
    fn split_unlace_blocks(&self) -> Vec<FnBlock<'_>> {
        let headers: Vec<(usize, usize, &str)> = FUNCTION_BLOCK_RE
            .captures_iter(self.source)
            .map(|c| {
                let whole = c.get(0).expect("match");
                (
                    whole.start(),
                    whole.end(),
                    c.get(1).expect("group 1").as_str(),
                )
            })
            .collect();

        let mut blocks = Vec::with_capacity(headers.len());
        for (i, &(_start, body_start, fields)) in headers.iter().enumerate() {
            let code_end = headers.get(i + 1).map(|n| n.0).unwrap_or(self.source.len());
            let name = fields
                .split('|')
                .next()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or("anonymous")
                .to_string();
            let address = ADDRESS_FIELD_RE
                .captures(fields)
                .and_then(|c| u64::from_str_radix(c.get(1)?.as_str(), 16).ok());
            let size = SIZE_FIELD_RE
                .captures(fields)
                .and_then(|c| c.get(1)?.as_str().parse::<u32>().ok());
            let xrefs_to = XREFS_TO_FIELD_RE
                .captures(fields)
                .and_then(|c| c.get(1))
                .map(|m| {
                    m.as_str()
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && *s != "unknown")
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            blocks.push(FnBlock {
                name,
                address,
                size,
                xrefs_to,
                original_start_line: line_for_byte(self.source, body_start),
                code: &self.source[body_start..code_end],
            });
        }
        blocks
    }

    /// Extract one function block by running [`IdaCExtractor`] on a synthesized
    /// single-function IDA source, then re-homing the result under the
    /// whole-program file node.
    fn extract_block(&self, block: &FnBlock<'_>, file_node_id: &str, out: &mut ExtractionResult) {
        // Synthetic IDA source: line-comment headers (authoritative name +
        // address + size) followed by the function code.
        let mut synthetic = format!("// Name: {}\n", block.name);
        if let Some(addr) = block.address {
            synthetic.push_str(&format!("// Address: 0x{addr:X}\n"));
        }
        if let Some(size) = block.size {
            synthetic.push_str(&format!("// Size: {size} bytes\n"));
        }
        let synthetic_header_lines = synthetic.bytes().filter(|b| *b == b'\n').count() as i64;
        let line_delta = i64::from(block.original_start_line) - 1 - synthetic_header_lines;
        synthetic.push_str(block.code);

        // A per-function synthetic path keeps generated node ids unique across
        // functions in the same file (the id hash includes the path).
        let synth_path = match block.address {
            Some(addr) => format!("{}#{addr:X}", self.file_path),
            None => format!("{}#{}", self.file_path, block.name),
        };
        let synth_file_node = format!("file:{synth_path}");
        let sub = IdaCExtractor::new(&synth_path, &synthetic, self.language).extract();

        let mut function_node_id: Option<String> = None;
        for mut node in sub.nodes {
            if node.kind == NodeKind::File {
                continue; // drop the per-function synthetic file node
            }
            // Re-home every node onto the real whole-program file.
            node.file_path = self.file_path.clone();
            node.start_line = rebase_line(node.start_line, line_delta);
            node.end_line = rebase_line(node.end_line, line_delta);
            if matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && function_node_id.is_none()
            {
                function_node_id = Some(node.id.clone());
            }
            out.nodes.push(node);
        }
        for mut edge in sub.edges {
            // Containment from the synthetic file node → the real file node.
            if edge.source == synth_file_node {
                edge.source = file_node_id.to_string();
            }
            if let Some(line) = edge.line {
                edge.line = Some(rebase_line(line, line_delta));
            }
            out.edges.push(edge);
        }
        out.unresolved_references
            .extend(sub.unresolved_references.into_iter().map(|mut reference| {
                reference.line = rebase_line(reference.line, line_delta);
                reference
            }));
        out.errors.extend(sub.errors.into_iter().map(|mut error| {
            error.line = error.line.map(|line| rebase_line(line, line_delta));
            error
        }));

        // The authoritative `Xrefs to:` list as Calls (catches callees the body
        // regex might miss, e.g. via tail calls / indirect thunks).
        if let Some(fn_id) = function_node_id {
            for callee in &block.xrefs_to {
                out.unresolved_references.push(UnresolvedReference {
                    from_node_id: fn_id.clone(),
                    reference_name: callee.clone(),
                    reference_kind: EdgeKind::Calls,
                    line: block.original_start_line,
                    column: 0,
                    file_path: None,
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
            // Containment under the real file node.
            out.edges
                .push(Edge::new(file_node_id, fn_id, EdgeKind::Contains));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE_PROGRAM: &str = "typedef unsigned long long _QWORD;\n#define LOBYTE(x) ((_BYTE)(x))\n_QWORD qword_3FD8;\n__int64 sub_1040();\n\n/* Function:   main | Address: 0x1060 | Size: 0x40d (1037 bytes) | Convention: System V AMD64 | Xrefs to:   f_helper, sub_1040 | Xrefs from: unknown */\nint __fastcall main(int a1)\n{\n  _QWORD c0; // rax\n  c0 = sub_1040();\n  qword_3FD8 = c0;\n  return f_helper(a1);\n}\n\n/* Function:   f_helper | Address: 0x1480 | Size: 0x20 (32 bytes) | Xrefs from: unknown */\n__int64 __fastcall f_helper(int a1)\n{\n  return a1 + 1;\n}\n";

    const DECOMPILE_MANY: &str = "//----- (1060) ----------------------------------------------------\nint __fastcall main(int a1)\n{\n  qword_3FD8 = a1;\n  return sub_1040(a1);\n}\n\n//----- (1040) ----------------------------------------------------\n__int64 __fastcall sub_1040()\n{\n  return 42;\n}\n";

    #[test]
    fn detects_and_splits_ida_decompile_many() {
        assert!(is_ida_decompile_many(DECOMPILE_MANY));
        assert!(!is_ida_decompile_many("int main() { return 0; }"));

        let result =
            UnlaceCExtractor::new("__decompile_many.c", DECOMPILE_MANY, Language::C).extract();
        // Two functions split from the aggregate, names from signatures,
        // addresses from the `//----- (addr) -----` boundaries.
        let fns: Vec<(&str, Option<u64>)> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| (n.name.as_str(), n.address))
            .collect();
        assert!(fns.contains(&("main", Some(0x1060))), "{fns:?}");
        assert!(fns.contains(&("sub_1040", Some(0x1040))), "{fns:?}");

        // Both under the one file node; main's call to sub_1040 captured.
        let files = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .count();
        assert_eq!(files, 1);
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "sub_1040" && r.reference_kind == EdgeKind::Calls)
        );
    }

    #[test]
    fn detects_unlace_whole_program_output() {
        assert!(is_unlace_c("medium.c", WHOLE_PROGRAM));
        assert!(!is_unlace_c(
            "sub_1000.c",
            "__int64 __fastcall sub_1000() { return 0; }"
        ));
    }

    #[test]
    fn splits_into_per_function_nodes_under_one_file() {
        let result = UnlaceCExtractor::new("medium.c", WHOLE_PROGRAM, Language::C).extract();

        // Exactly one file node; both functions extracted.
        let files: Vec<&Node> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, "file:medium.c");

        let fns: Vec<(&str, Option<u64>)> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| (n.name.as_str(), n.address))
            .collect();
        assert!(fns.contains(&("main", Some(0x1060))), "{fns:?}");
        assert!(fns.contains(&("f_helper", Some(0x1480))), "{fns:?}");

        // main's size came from the block comment.
        let main = result.nodes.iter().find(|n| n.name == "main").unwrap();
        assert_eq!(main.size, Some(1037));
        assert_eq!(main.start_line, 7);
        let helper = result.nodes.iter().find(|n| n.name == "f_helper").unwrap();
        assert_eq!(helper.start_line, 16);

        // Every function is contained by the one file node.
        for f in result.nodes.iter().filter(|n| n.kind == NodeKind::Function) {
            assert!(
                result.edges.iter().any(|e| e.source == "file:medium.c"
                    && e.target == f.id
                    && e.kind == EdgeKind::Contains),
                "{} not contained by file node",
                f.name
            );
            assert_eq!(f.file_path, "medium.c");
        }
    }

    #[test]
    fn extracts_calls_data_symbols_and_xrefs() {
        let result = UnlaceCExtractor::new("medium.c", WHOLE_PROGRAM, Language::C).extract();
        let call_names: Vec<&str> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .map(|r| r.reference_name.as_str())
            .collect();
        // Body calls + Xrefs-to list.
        assert!(call_names.contains(&"sub_1040"), "{call_names:?}");
        assert!(call_names.contains(&"f_helper"), "{call_names:?}");

        // The global `qword_3FD8` written in main is a DataSymbol with a Writes edge.
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::DataSymbol && n.name == "qword_3FD8")
        );
        assert!(
            result
                .edges
                .iter()
                .any(|e| e.target == "data_symbol:qword_3FD8" && e.kind == EdgeKind::Writes)
        );
    }
}
