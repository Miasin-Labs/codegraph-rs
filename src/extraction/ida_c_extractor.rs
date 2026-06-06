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

use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    Severity,
    UnresolvedReference,
};

const IDA_DUMP_EXTENSIONS: &[&str] = &[".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".hxx"];
const IDA_SAMPLE_BYTES: usize = 16 * 1024;
const MAX_IDA_LOCAL_VARIABLES: usize = 2000;

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
static IDA_WORD_TYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:_QWORD|_DWORD|_BYTE|_OWORD|BYREF)\b").expect("valid regex"));
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
        || (ADDRESS_COMMENT_RE.is_match(sample) && DISASSEMBLY_COMMENT_RE.is_match(sample))
        || RESOLVED_TARGET_COMMENT_RE.is_match(sample)
    {
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

#[derive(Debug, Clone)]
struct FunctionInfo {
    name: String,
    qualified_name: String,
    signature: Option<String>,
    return_type: Option<String>,
    parameters: Vec<TypedSymbol>,
    line: u32,
    column: u32,
    /// `None` mirrors the TS `-1` sentinel (`indexOf` miss).
    body_start_index: Option<usize>,
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
            self.extract_calls(&func_node_id, &self_names);
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
            generate_node_id(
                &self.file_path,
                NodeKind::Function,
                &info.qualified_name,
                info.line,
            ),
            NodeKind::Function,
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
        node.updated_at = now_ms();
        node
    }

    fn extract_function_info(&self) -> Option<FunctionInfo> {
        let comment_name = self.match_line_value(&NAME_VALUE_RE);
        let signature_info = self.extract_signature_info();
        let fallback_name = self.name_from_file_path();
        let qualified_name = comment_name
            .or_else(|| signature_info.as_ref().map(|s| s.qualified_name.clone()))
            .or(fallback_name)?;

        Some(FunctionInfo {
            name: simple_name(&qualified_name).to_string(),
            qualified_name: qualified_name.clone(),
            signature: signature_info.as_ref().and_then(|s| s.signature.clone()),
            return_type: signature_info.as_ref().and_then(|s| s.return_type.clone()),
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
        })
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
        let qualified_name = NAME_TOKEN_RE.find_iter(prefix).last()?.as_str();
        if IDA_TYPE_WORDS.contains(qualified_name) {
            return None;
        }

        let name = simple_name(qualified_name).to_string();
        let return_type = prefix[..prefix.rfind(qualified_name).unwrap_or(0)]
            .trim()
            .to_string();
        let parameters =
            self.extract_parameters(&signature, paren_index, (first_signature_line + 1) as u32);

        let source_line = lines[first_signature_line];
        let column = source_line.find(|c: char| !c.is_whitespace()).unwrap_or(0) as u32;

        Some(FunctionInfo {
            name,
            qualified_name: qualified_name.to_string(),
            signature: Some(signature),
            return_type: Some(return_type),
            parameters,
            line: (first_signature_line + 1) as u32,
            column,
            body_start_index: Some(brace_index),
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
        let mut seen: HashSet<String> = HashSet::new();
        let source = self.source;

        let resolved_target = self.match_line_value(&RESOLVED_TARGET_VALUE_RE);
        if let Some(resolved_target) = resolved_target {
            if resolved_target != "<no type info>" {
                let line = self
                    .line_of_comment_value(&RESOLVED_TARGET_HEAD_RE)
                    .unwrap_or(1);
                self.add_call_ref(from_node_id, &resolved_target, line, 0, &mut seen);
            }
        }

        for m in CALL_PATTERN_RE.find_iter(source) {
            let name = m.as_str();
            let index = m.start();
            if self.is_in_line_comment(index) {
                continue;
            }
            if !CALL_PAREN_RE.is_match(&source[index + name.len()..]) {
                continue;
            }
            if self_names.contains(name) {
                continue;
            }
            if CONTROL_WORDS.contains(name) || IDA_TYPE_WORDS.contains(name) {
                continue;
            }
            if IDA_CALL_MACROS.contains(name) {
                continue;
            }

            let (line, column) = self.line_column_at(index);
            self.add_call_ref(from_node_id, name, line, column, &mut seen);
        }
    }

    fn add_call_ref(
        &mut self,
        from_node_id: &str,
        reference_name: &str,
        line: u32,
        column: u32,
        seen: &mut HashSet<String>,
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
        });
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
    fn extracts_leading_dot_thunk_functions_and_resolved_target_calls() {
        let code = "\n// ============================================================\n// THUNK / TRAMPOLINE\n// ============================================================\n// Address:         0x19E10\n// Name:            .mysql_init\n// Disassembly:\n//   0x19E10 jmp     cs:off_23E6F0\n//\n// Resolved target: mysql_init\n// Target address:  0x2442C0\n// ============================================================\n\nvoid .mysql_init(/* see target signature */)\n{\n    return mysql_init(/* forwarded args */);\n}\n";

        assert!(is_ida_generated_c("lumina/all/.mysql_init.c", code));

        let result = extract("lumina/all/.mysql_init.c", code);
        let thunk = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("function node");
        assert_eq!(thunk.name, ".mysql_init");

        let calls: Vec<&UnresolvedReference> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .collect();
        assert!(calls.iter().any(|r| r.reference_name == "mysql_init"));
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
    fn function_pointer_locals_are_parsed() {
        let symbol =
            parse_typed_declarator("void (__cdecl *handler)(int)", 3, 2).expect("declarator");
        assert_eq!(symbol.name, "handler");
        assert_eq!(symbol.type_text, "void (__cdecl *)(int)");
        assert_eq!(symbol.signature, "void (__cdecl *)(int) handler");
    }
}
