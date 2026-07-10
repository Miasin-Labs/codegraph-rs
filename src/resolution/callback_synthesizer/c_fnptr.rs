//! C/C++ function-pointer dispatch synthesis.
//!
//! Static extraction cannot connect a call through a function-pointer field or
//! array slot to the functions registered in that slot. This pass joins those
//! registrations to their dispatch sites. It covers direct and designated
//! struct initializers, macro-built tables, local included tables, field-to-
//! field propagation, chained receivers, and bare arrays of function pointers.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{count_newlines, slice_lines};
use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

const FANOUT_CAP: usize = 300;

static C_CPP_EXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:c|h|cc|cpp|cxx|hpp|hh|hxx|cppm|ipp|inl|tcc)$")
        .expect("valid C/C++ extension regex")
});
static INCLUDABLE_EXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:def|inc|h|hh|hpp|hxx|c|cc|cpp|cxx|ipp|tcc|tbl)$")
        .expect("valid includable extension regex")
});
static FNPTR_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(")
        .expect("valid function-pointer declaration regex")
});
static FNPTR_TYPEDEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\btypedef\b[^;{}]*?\(\s*(?:\w+\s+)*\*\s*(\w+)\s*\)\s*\(")
        .expect("valid function-pointer typedef regex")
});
static TYPEDEF_STMT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\btypedef\b([^;{}]*);").expect("valid typedef statement regex"));
static TYPEDEF_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").expect("valid typedef name regex"));
static INCLUDE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"#[ \t]*include[ \t]+\"([^\"\n]+)\""#).expect("valid include regex")
});
static FUNCTION_MACRO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*#[ \t]*define[ \t]+(\w+)\(([^)]*)\)\s+(.+)$")
        .expect("valid function macro regex")
});
static OBJECT_MACRO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*#[ \t]*define[ \t]+(\w+)[ \t]+(\S[^\n]*)$")
        .expect("valid object macro regex")
});
static DEFINED_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*#[ \t]*define[ \t]+(\w+)").expect("valid define regex")
});
static IDENT_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\w+\b").expect("valid identifier token regex"));
static MACRO_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").expect("valid macro call regex"));
static FIRST_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\w+)\s+\**\s*(\w+)\s*$").expect("valid field declaration regex")
});
static FOLLOWING_FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\**\s*(\w+)").expect("valid field declarator regex"));
static DESIGNATED_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\.\s*(\w+)\s*=\s*(?:&\s*)?(\w+)\s*$").expect("valid designated initializer regex")
});
static IDENT_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^&?\s*(\w+)\s*$").expect("valid identifier value regex"));
static ARRAY_DESIGNATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[[^\]]*\]\s*=\s*([\s\S]*)$").expect("valid array designator regex")
});
static LEADING_CAST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\((?:[\w\s*]+)\)\s*").expect("valid leading C cast regex"));
static INLINE_STRUCT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bstruct\s+(\w+)\s*\{").expect("valid inline struct regex"));
static INLINE_STRUCT_TAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\w+)\s*(\[[^\]]*\])?\s*(=\s*\{)?").expect("valid inline struct tail regex")
});
static INIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[;{}])\s*(?:(?:static|const|extern|register|volatile)\s+)*(?:struct\s+)?(\w+)\s+(\w+)\s*(\[[^\]]*\])?\s*=\s*\{",
    )
    .expect("valid struct initializer regex")
});
static ARRAY_TABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[;{}])\s*(?:(?:static|const|extern|register|volatile)\s+)*(\w+)\s+(\*\s*)?(\w+)\s*\[[^\]]*\]\s*=\s*\{",
    )
    .expect("valid function-pointer array regex")
});
static FIELD_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\w+)\s*(?:->|\.)\s*(\w+)\s*=\s*(\w+)\s*(?:->|\.)\s*(\w+)")
        .expect("valid field propagation regex")
});
static DIRECT_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\w+)\s*(?:->|\.)\s*(\w+)\s*=\s*&?\s*(\w+)\b")
        .expect("valid direct field assignment regex")
});
static ARRAY_SUBSCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\[[^\]]*\]").expect("valid array subscript regex"));
static MEMBER_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:->|\.)\s*").expect("valid member split regex"));
static DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"((?:\w+(?:\s*\[[^\[\]]*\])?\s*(?:->|\.)\s*)+)(\w+)\s*\)?\s*\(")
        .expect("valid function-pointer dispatch regex")
});
static ARRAY_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\(\s*\*\s*)?\b(\w+)\s*\[[^\[\]]*\]\s*\)?\s*\(")
        .expect("valid array dispatch regex")
});
static PREPROCESSOR_IF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\s*if").expect("valid preprocessor-if regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldInfo {
    name: String,
    index: usize,
    is_fn_ptr: bool,
    type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MacroDef {
    params: Vec<String>,
    expansion: String,
}

#[derive(Debug, Clone)]
struct ArrayRegistration {
    file: String,
    ids: HashSet<String>,
}

fn is_function_kind(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Function | NodeKind::Method)
}

fn is_c_type_keyword(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "int"
            | "char"
            | "short"
            | "long"
            | "unsigned"
            | "signed"
            | "float"
            | "double"
            | "const"
            | "struct"
            | "union"
            | "enum"
            | "static"
            | "volatile"
            | "register"
            | "inline"
    )
}

fn match_delimiter(source: &str, open: usize, opening: u8, closing: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open).copied() != Some(opening) {
        return None;
    }
    let mut depth = 0usize;
    for (offset, byte) in bytes.iter().copied().enumerate().skip(open) {
        if byte == opening {
            depth += 1;
        } else if byte == closing {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(offset);
            }
        }
    }
    None
}

fn match_brace(source: &str, open: usize) -> Option<usize> {
    match_delimiter(source, open, b'{', b'}')
}

fn match_paren(source: &str, open: usize) -> Option<usize> {
    match_delimiter(source, open, b'(', b')')
}

fn split_top_level(source: &str, separator: u8) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut start = 0usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            _ if byte == separator && depth == 0 => {
                out.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&source[start..]);
    out
}

fn joined_continuations(source: &str) -> String {
    source.replace("\\\r\n", " ").replace("\\\n", " ")
}

fn parse_function_macros(source: &str) -> HashMap<String, MacroDef> {
    let mut out = HashMap::new();
    if !source.contains("#define") && !source.contains("# define") {
        return out;
    }
    let joined = joined_continuations(source);
    for captures in FUNCTION_MACRO_RE.captures_iter(&joined) {
        let params: Vec<String> = captures[2]
            .split(',')
            .map(str::trim)
            .filter(|param| !param.is_empty())
            .map(str::to_string)
            .collect();
        if params
            .iter()
            .any(|param| param == "..." || param.ends_with("..."))
        {
            continue;
        }
        out.insert(
            captures[1].to_string(),
            MacroDef {
                params,
                expansion: captures[3].trim().to_string(),
            },
        );
    }
    out
}

fn parse_object_macros(source: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if !source.contains("#define") && !source.contains("# define") {
        return out;
    }
    let joined = joined_continuations(source);
    for captures in OBJECT_MACRO_RE.captures_iter(&joined) {
        out.insert(captures[1].to_string(), captures[2].trim().to_string());
    }
    out
}

fn parse_defined_names(source: &str) -> HashSet<String> {
    if !source.contains("#define") && !source.contains("# define") {
        return HashSet::new();
    }
    DEFINED_NAME_RE
        .captures_iter(source)
        .map(|captures| captures[1].to_string())
        .collect()
}

fn condition_defined(expression: &str, defined: &HashSet<String>) -> Option<bool> {
    let mut expression = expression.trim();
    let negated = expression.starts_with('!');
    if negated {
        expression = expression[1..].trim();
    }
    expression = expression.strip_prefix("defined")?.trim();
    let name = expression
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(expression)
        .trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let present = defined.contains(name);
    Some(if negated { !present } else { present })
}

fn eval_conditionals(source: &str, defined: &HashSet<String>) -> String {
    #[derive(Debug)]
    struct Frame {
        parent_active: bool,
        active: bool,
        taken: bool,
    }

    if !PREPROCESSOR_IF_RE.is_match(source) {
        return source.to_string();
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut output = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        let directive = trimmed.strip_prefix('#').map(str::trim);
        let active_now = stack.last().is_none_or(|frame| frame.active);

        if let Some(name) = directive.and_then(|value| value.strip_prefix("ifdef ")) {
            let condition = defined.contains(name.trim());
            stack.push(Frame {
                parent_active: active_now,
                active: active_now && condition,
                taken: condition,
            });
            output.push(String::new());
            continue;
        }
        if let Some(name) = directive.and_then(|value| value.strip_prefix("ifndef ")) {
            let condition = !defined.contains(name.trim());
            stack.push(Frame {
                parent_active: active_now,
                active: active_now && condition,
                taken: condition,
            });
            output.push(String::new());
            continue;
        }
        if let Some(expression) = directive.and_then(|value| value.strip_prefix("if ")) {
            let condition = condition_defined(expression, defined).unwrap_or(true);
            stack.push(Frame {
                parent_active: active_now,
                active: active_now && condition,
                taken: condition,
            });
            output.push(String::new());
            continue;
        }
        if directive.is_some_and(|value| value.starts_with("elif")) {
            if let Some(frame) = stack.last_mut() {
                frame.active = frame.parent_active && !frame.taken;
                frame.taken = true;
            }
            output.push(String::new());
            continue;
        }
        if directive == Some("else") {
            if let Some(frame) = stack.last_mut() {
                frame.active = frame.parent_active && !frame.taken;
                frame.taken = true;
            }
            output.push(String::new());
            continue;
        }
        if directive == Some("endif") {
            stack.pop();
            output.push(String::new());
            continue;
        }
        output.push(if active_now {
            line.to_string()
        } else {
            String::new()
        });
    }
    output.join("\n")
}

fn substitute_macro(definition: &MacroDef, arguments: &[&str]) -> String {
    let substitutions: HashMap<&str, &str> = definition
        .params
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            (
                parameter.as_str(),
                arguments.get(index).copied().unwrap_or(""),
            )
        })
        .collect();
    IDENT_TOKEN_RE
        .replace_all(&definition.expansion, |captures: &regex::Captures<'_>| {
            substitutions
                .get(captures.get(0).expect("whole identifier match").as_str())
                .copied()
                .unwrap_or_else(|| captures.get(0).expect("whole identifier match").as_str())
                .to_string()
        })
        .into_owned()
}

fn expand_macro_calls(source: &str, environment: &HashMap<String, MacroDef>) -> String {
    if environment.is_empty() {
        return source.to_string();
    }
    let mut output = source.to_string();
    for _ in 0..6 {
        let mut replacement = None;
        for captures in MACRO_CALL_RE.captures_iter(&output) {
            let name = &captures[1];
            let Some(definition) = environment.get(name) else {
                continue;
            };
            let whole = captures.get(0).expect("whole macro call prefix");
            let open = whole.end() - 1;
            let Some(close) = match_paren(&output, open) else {
                continue;
            };
            let raw_arguments = split_top_level(&output[open + 1..close], b',');
            let arguments: Vec<&str> = raw_arguments
                .iter()
                .map(|argument| argument.trim())
                .collect();
            replacement = Some((
                whole.start(),
                close + 1,
                substitute_macro(definition, &arguments),
            ));
            break;
        }
        let Some((start, end, value)) = replacement else {
            break;
        };
        output.replace_range(start..end, &value);
    }
    output
}

fn resolve_type_name(name: &str, environment: &HashMap<String, String>) -> String {
    let mut current = name.to_string();
    for _ in 0..5 {
        let Some(value) = environment.get(&current) else {
            break;
        };
        let value = value.trim().strip_prefix("struct ").unwrap_or(value.trim());
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            break;
        }
        current = value.to_string();
    }
    current
}

fn parse_struct_fields(
    body: &str,
    fn_ptr_typedefs: &HashSet<String>,
    fn_type_typedefs: &HashSet<String>,
) -> Vec<FieldInfo> {
    let mut fields = Vec::new();
    let mut index = 0usize;
    for raw_declaration in split_top_level(body, b';') {
        let declaration = raw_declaration.trim();
        if declaration.is_empty() {
            continue;
        }
        let parts = split_top_level(declaration, b',');
        let first_typed = FIRST_FIELD_RE.captures(parts[0].trim());
        let shared_type = first_typed
            .as_ref()
            .map(|captures| captures[1].to_string())
            .unwrap_or_default();
        for (part_index, raw_part) in parts.iter().enumerate() {
            let part = raw_part.trim();
            let mut name = String::new();
            let mut type_name = String::new();
            let mut is_fn_ptr = false;
            if let Some(pointer) = FNPTR_DECL_RE.captures(part) {
                name = pointer[1].to_string();
                is_fn_ptr = true;
            } else if part_index == 0 {
                if let Some(first) = &first_typed {
                    name = first[2].to_string();
                    type_name.clone_from(&shared_type);
                }
            } else if let Some(following) = FOLLOWING_FIELD_RE.captures(part) {
                name = following[1].to_string();
                type_name.clone_from(&shared_type);
            }
            if !is_fn_ptr && !type_name.is_empty() {
                is_fn_ptr =
                    fn_ptr_typedefs.contains(&type_name) || fn_type_typedefs.contains(&type_name);
            }
            fields.push(FieldInfo {
                name,
                index,
                is_fn_ptr,
                type_name,
            });
            index += 1;
        }
    }
    fields
}

fn normalize_relative_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut components: Vec<&str> = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    components.join("/")
}

struct FnPtrSynthesis<'a> {
    ctx: &'a dyn ResolutionContext,
    files: Vec<String>,
    fn_ptr_typedefs: HashSet<String>,
    fn_type_typedefs: HashSet<String>,
    struct_layout: HashMap<String, Vec<FieldInfo>>,
    all_struct_fields: HashMap<String, Vec<Vec<FieldInfo>>>,
    field_to_structs: HashMap<String, HashSet<String>>,
    registrations: HashMap<String, HashSet<String>>,
    array_registrations: HashMap<String, Vec<ArrayRegistration>>,
    global_var_type: HashMap<String, String>,
}

impl<'a> FnPtrSynthesis<'a> {
    fn new(ctx: &'a dyn ResolutionContext, files: Vec<String>) -> Self {
        Self {
            ctx,
            files,
            fn_ptr_typedefs: HashSet::new(),
            fn_type_typedefs: HashSet::new(),
            struct_layout: HashMap::new(),
            all_struct_fields: HashMap::new(),
            field_to_structs: HashMap::new(),
            registrations: HashMap::new(),
            array_registrations: HashMap::new(),
            global_var_type: HashMap::new(),
        }
    }

    fn raw_source(&self, file: &str) -> Option<String> {
        self.ctx.read_file(file)
    }

    fn stripped_source(&self, file: &str) -> Option<String> {
        self.raw_source(file)
            .map(|source| strip_comments_for_regex(&source, CommentLang::Csharp))
    }

    fn collect_typedefs(&mut self) {
        for file in &self.files {
            let Some(source) = self.stripped_source(file) else {
                continue;
            };
            if !source.contains("typedef") {
                continue;
            }
            for captures in FNPTR_TYPEDEF_RE.captures_iter(&source) {
                self.fn_ptr_typedefs.insert(captures[1].to_string());
            }
            for captures in TYPEDEF_STMT_RE.captures_iter(&source) {
                let declaration = &captures[1];
                if declaration.contains("(*") || declaration.contains("( *") {
                    continue;
                }
                let Some(name) = TYPEDEF_NAME_RE
                    .captures(declaration)
                    .map(|name| name[1].to_string())
                else {
                    continue;
                };
                if !is_c_type_keyword(&name) {
                    self.fn_type_typedefs.insert(name);
                }
            }
        }
    }

    fn register_struct_layout(&mut self, name: String, fields: Vec<FieldInfo>) {
        self.all_struct_fields
            .entry(name.clone())
            .or_default()
            .push(fields.clone());
        for field in &fields {
            if field.is_fn_ptr && !field.name.is_empty() {
                self.field_to_structs
                    .entry(field.name.clone())
                    .or_default()
                    .insert(name.clone());
            }
        }
        if fields.iter().any(|field| field.is_fn_ptr) {
            self.struct_layout.insert(name, fields);
        }
    }

    fn collect_struct_layouts(&mut self, queries: &QueryBuilder) -> Result<()> {
        queries.iterate_nodes_by_kind(NodeKind::Struct, |node| {
            if !C_CPP_EXT_RE.is_match(&node.file_path) {
                return true;
            }
            let Some(source) = self.stripped_source(&node.file_path) else {
                return true;
            };
            let Some(body) = slice_lines(&source, node.start_line, node.end_line) else {
                return true;
            };
            let Some(open) = body.find('{') else {
                return true;
            };
            let Some(close) = match_brace(&body, open) else {
                return true;
            };
            let fields = parse_struct_fields(
                &body[open + 1..close],
                &self.fn_ptr_typedefs,
                &self.fn_type_typedefs,
            );
            self.register_struct_layout(node.name, fields);
            true
        })
    }

    fn is_fn_ptr_field(&self, struct_name: &str, field_name: &str) -> bool {
        self.struct_layout.get(struct_name).is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field.name == field_name && field.is_fn_ptr)
        })
    }

    fn resolve_function(&self, name: &str, preferred_file: &str) -> Option<Node> {
        let candidates: Vec<Node> = self
            .ctx
            .get_nodes_by_name(name)
            .into_iter()
            .filter(|node| is_function_kind(node.kind))
            .collect();
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
        candidates
            .iter()
            .find(|node| node.file_path == preferred_file)
            .cloned()
            .or_else(|| candidates.into_iter().next())
    }

    fn add_registration(&mut self, struct_name: &str, field_name: &str, function: &Node) {
        self.registrations
            .entry(format!("{struct_name}.{field_name}"))
            .or_default()
            .insert(function.id.clone());
    }

    fn add_array_registration(&mut self, array: &str, file: &str, function: &Node) {
        let entries = self
            .array_registrations
            .entry(array.to_string())
            .or_default();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.file == file) {
            entry.ids.insert(function.id.clone());
        } else {
            entries.push(ArrayRegistration {
                file: file.to_string(),
                ids: HashSet::from([function.id.clone()]),
            });
        }
    }

    fn register_struct_value(
        &mut self,
        struct_name: &str,
        body: &str,
        file: &str,
        environment: &HashMap<String, MacroDef>,
    ) {
        let Some(layout) = self.struct_layout.get(struct_name).cloned() else {
            return;
        };
        let mut body = expand_macro_calls(body, environment).trim().to_string();
        if body.starts_with('{') {
            if let Some(close) = match_brace(&body, 0) {
                if body[close + 1..].trim().is_empty() {
                    body = body[1..close].to_string();
                }
            }
        }

        let mut position = 0usize;
        for raw_item in split_top_level(&body, b',') {
            let item = raw_item.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(designated) = DESIGNATED_FIELD_RE.captures(item) {
                let field = &designated[1];
                let function_name = &designated[2];
                if self.is_fn_ptr_field(struct_name, field) {
                    if let Some(function) = self.resolve_function(function_name, file) {
                        self.add_registration(struct_name, field, &function);
                    }
                }
                continue;
            }
            if let Some(field) = layout.iter().find(|field| field.index == position) {
                if field.is_fn_ptr {
                    if let Some(identifier) = IDENT_VALUE_RE.captures(item) {
                        if let Some(function) = self.resolve_function(&identifier[1], file) {
                            self.add_registration(struct_name, &field.name, &function);
                        }
                    }
                }
            }
            position += 1;
        }
    }

    fn register_array_value(
        &mut self,
        array: &str,
        body: &str,
        file: &str,
        environment: &HashMap<String, MacroDef>,
    ) {
        let expanded = expand_macro_calls(body, environment);
        for raw_item in split_top_level(&expanded, b',') {
            let mut item = raw_item.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(designated) = ARRAY_DESIGNATOR_RE.captures(item) {
                item = designated
                    .get(1)
                    .expect("array designator value")
                    .as_str()
                    .trim();
            }
            let without_cast = LEADING_CAST_RE.replace(item, "").into_owned();
            let value = without_cast
                .trim()
                .strip_prefix('&')
                .unwrap_or(without_cast.trim())
                .trim();
            let Some(identifier) = IDENT_VALUE_RE.captures(value) else {
                continue;
            };
            if let Some(function) = self.resolve_function(&identifier[1], file) {
                self.add_array_registration(array, file, &function);
            }
        }
    }

    fn resolve_include(&self, includer: &str, include: &str) -> Option<String> {
        let normalized_includer = includer.replace('\\', "/");
        let parent = normalized_includer
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        let candidate = normalize_relative_path(&format!("{parent}/{include}"));
        if self.ctx.file_exists(&candidate) {
            return Some(candidate);
        }
        self.ctx.file_exists(include).then(|| include.to_string())
    }

    fn local_includes_of(&self, file: &str) -> Vec<String> {
        let Some(raw) = self.raw_source(file) else {
            return Vec::new();
        };
        if !raw.contains("include") {
            return Vec::new();
        }
        INCLUDE_RE
            .captures_iter(&raw)
            .filter_map(|captures| {
                let include = captures.get(1)?.as_str();
                INCLUDABLE_EXT_RE
                    .is_match(include)
                    .then(|| self.resolve_include(file, include))
                    .flatten()
            })
            .collect()
    }

    fn build_environment(
        &self,
        file: &str,
        depth: i32,
        seen: &mut HashSet<String>,
        functions: &mut HashMap<String, MacroDef>,
        objects: &mut HashMap<String, String>,
        defined: &mut HashSet<String>,
    ) {
        if depth < 0 || !seen.insert(file.to_string()) {
            return;
        }
        let source = self.stripped_source(file).unwrap_or_default();
        for (name, definition) in parse_function_macros(&source) {
            functions.entry(name).or_insert(definition);
        }
        for (name, value) in parse_object_macros(&source) {
            objects.entry(name).or_insert(value);
        }
        defined.extend(parse_defined_names(&source));
        for include in self.local_includes_of(file) {
            self.build_environment(&include, depth - 1, seen, functions, objects, defined);
        }
    }

    fn process_initializer(
        &mut self,
        struct_name: &str,
        body: &str,
        is_array: bool,
        file: &str,
        environment: &HashMap<String, MacroDef>,
    ) {
        if !is_array {
            self.register_struct_value(struct_name, body, file, environment);
            return;
        }
        for element in split_top_level(body, b',') {
            let element = element.trim();
            if element.starts_with('{') {
                if let Some(close) = match_brace(element, 0) {
                    self.register_struct_value(struct_name, &element[1..close], file, environment);
                }
            } else if !element.is_empty() {
                self.register_struct_value(struct_name, element, file, environment);
            }
        }
    }

    fn process_unit(
        &mut self,
        source: &str,
        file: &str,
        environment: &HashMap<String, MacroDef>,
        object_environment: &HashMap<String, String>,
    ) {
        if source.is_empty() || !source.contains('{') {
            return;
        }

        let inline_matches: Vec<(String, usize, usize)> = INLINE_STRUCT_RE
            .captures_iter(source)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                Some((captures[1].to_string(), whole.end() - 1, whole.end()))
            })
            .collect();
        for (tag, open, _) in inline_matches {
            let Some(close) = match_brace(source, open) else {
                continue;
            };
            let after = &source[close + 1..];
            let Some(variable) = INLINE_STRUCT_TAIL_RE.captures(after) else {
                continue;
            };
            let fields = parse_struct_fields(
                &source[open + 1..close],
                &self.fn_ptr_typedefs,
                &self.fn_type_typedefs,
            );
            if !fields.iter().any(|field| field.is_fn_ptr) {
                continue;
            }
            if !self.struct_layout.contains_key(&tag) {
                self.register_struct_layout(tag.clone(), fields);
            }
            let variable_name = variable[1].to_string();
            self.global_var_type.insert(variable_name, tag.clone());
            if variable.get(3).is_some() {
                let Some(variable_match) = variable.get(0) else {
                    continue;
                };
                let Some(relative_open) = after[..variable_match.end()].rfind('{') else {
                    continue;
                };
                let initializer_open = close + 1 + relative_open;
                if let Some(initializer_close) = match_brace(source, initializer_open) {
                    self.process_initializer(
                        &tag,
                        &source[initializer_open + 1..initializer_close],
                        variable.get(2).is_some(),
                        file,
                        environment,
                    );
                }
            }
        }

        if !source.contains('=') {
            return;
        }
        let initializers: Vec<(String, String, bool, usize)> = INIT_RE
            .captures_iter(source)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                Some((
                    captures[1].to_string(),
                    captures[2].to_string(),
                    captures.get(3).is_some(),
                    whole.end() - 1,
                ))
            })
            .collect();
        for (mut struct_name, variable, is_array, open) in initializers {
            if !self.struct_layout.contains_key(&struct_name) {
                struct_name = resolve_type_name(&struct_name, object_environment);
            }
            if !self.struct_layout.contains_key(&struct_name) {
                continue;
            }
            let Some(close) = match_brace(source, open) else {
                continue;
            };
            self.global_var_type.insert(variable, struct_name.clone());
            self.process_initializer(
                &struct_name,
                &source[open + 1..close],
                is_array,
                file,
                environment,
            );
        }

        let arrays: Vec<(String, bool, String, usize)> = ARRAY_TABLE_RE
            .captures_iter(source)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                Some((
                    captures[1].to_string(),
                    captures.get(2).is_some(),
                    captures[3].to_string(),
                    whole.end() - 1,
                ))
            })
            .collect();
        for (element_type, has_star, variable, open) in arrays {
            let is_function_array = (has_star && self.fn_type_typedefs.contains(&element_type))
                || self.fn_ptr_typedefs.contains(&element_type);
            if !is_function_array {
                continue;
            }
            let Some(close) = match_brace(source, open) else {
                continue;
            };
            self.register_array_value(&variable, &source[open + 1..close], file, environment);
        }
    }

    fn collect_registrations(&mut self) {
        let indexed: HashSet<String> = self.files.iter().cloned().collect();
        let mut seen_includes = HashSet::new();
        for file in self.files.clone() {
            let mut functions = HashMap::new();
            let mut objects = HashMap::new();
            let mut defined = HashSet::new();
            self.build_environment(
                &file,
                2,
                &mut HashSet::new(),
                &mut functions,
                &mut objects,
                &mut defined,
            );
            if let Some(source) = self.stripped_source(&file) {
                self.process_unit(&source, &file, &functions, &objects);
            }

            for include in self.local_includes_of(&file) {
                if !seen_includes.insert(format!("{file}>{include}")) {
                    continue;
                }
                let Some(include_source) = self.stripped_source(&include) else {
                    continue;
                };
                if indexed.contains(&include) {
                    let own_defined = parse_defined_names(&include_source);
                    let adds_definition = defined.iter().any(|name| !own_defined.contains(name));
                    if !adds_definition || !PREPROCESSOR_IF_RE.is_match(&include_source) {
                        continue;
                    }
                }
                let active_source = eval_conditionals(&include_source, &defined);
                let mut include_functions = functions.clone();
                include_functions.extend(parse_function_macros(&active_source));
                let mut include_objects = objects.clone();
                include_objects.extend(parse_object_macros(&active_source));
                self.process_unit(
                    &active_source,
                    &include,
                    &include_functions,
                    &include_objects,
                );
            }
        }
    }

    fn receiver_type_in(&self, source: &str, receiver: &str) -> Option<String> {
        let regex = Regex::new(&format!(
            r"(?:struct\s+)?(\w+)\s*\*?\s*\b{}\b\s*(?:[,)=;]|\[)",
            regex::escape(receiver)
        ))
        .expect("escaped receiver makes a valid declaration regex");
        let resolved = regex.captures_iter(source).find_map(|captures| {
            let type_name = captures[1].to_string();
            self.struct_layout
                .contains_key(&type_name)
                .then_some(type_name)
        });
        resolved
    }

    fn variable_type_in(&self, source: &str, variable: &str) -> Option<String> {
        let regex = Regex::new(&format!(
            r"(?:struct\s+)?(\w+)\s*\*?\s*\b{}\b\s*(?:[,)=;]|\[)",
            regex::escape(variable)
        ))
        .expect("escaped variable makes a valid declaration regex");
        let resolved = regex
            .captures_iter(source)
            .find_map(|captures| {
                let type_name = captures[1].to_string();
                (!is_c_type_keyword(&type_name)).then_some(type_name)
            })
            .or_else(|| self.global_var_type.get(variable).cloned());
        resolved
    }

    fn resolve_chain_type(&self, source: &str, chain: &str) -> Option<String> {
        let without_subscripts = ARRAY_SUBSCRIPT_RE.replace_all(chain, "");
        let segments: Vec<&str> = MEMBER_SPLIT_RE
            .split(&without_subscripts)
            .filter(|segment| !segment.is_empty())
            .collect();
        let mut current = self.variable_type_in(source, segments.first().copied()?)?;
        for segment in segments.iter().skip(1) {
            current = self
                .all_struct_fields
                .get(&current)?
                .iter()
                .find_map(|layout| {
                    layout
                        .iter()
                        .find(|field| field.name == *segment && !field.type_name.is_empty())
                        .map(|field| field.type_name.clone())
                })?;
        }
        Some(current)
    }

    fn collect_direct_assignments_and_propagate(&mut self) {
        let mut propagations = Vec::new();
        for file in self.files.clone() {
            let Some(source) = self.stripped_source(&file) else {
                continue;
            };
            if !source.contains('=') {
                continue;
            }
            for function in self.ctx.get_nodes_in_file(&file) {
                if !is_function_kind(function.kind) {
                    continue;
                }
                let Some(body) = slice_lines(&source, function.start_line, function.end_line)
                else {
                    continue;
                };
                for captures in DIRECT_ASSIGN_RE.captures_iter(&body) {
                    let whole = captures.get(0).expect("whole direct assignment match");
                    let tail = body[whole.end()..].trim_start();
                    if tail.starts_with("->") || tail.starts_with('.') {
                        continue;
                    }
                    let Some(struct_name) = self.receiver_type_in(&body, &captures[1]) else {
                        continue;
                    };
                    let field = &captures[2];
                    if !self.is_fn_ptr_field(&struct_name, field) {
                        continue;
                    }
                    if let Some(handler) = self.resolve_function(&captures[3], &file) {
                        self.add_registration(&struct_name, field, &handler);
                    }
                }
                for captures in FIELD_ASSIGN_RE.captures_iter(&body) {
                    let Some(target_struct) = self.receiver_type_in(&body, &captures[1]) else {
                        continue;
                    };
                    let Some(source_struct) = self.receiver_type_in(&body, &captures[3]) else {
                        continue;
                    };
                    if self.is_fn_ptr_field(&target_struct, &captures[2])
                        && self.is_fn_ptr_field(&source_struct, &captures[4])
                    {
                        propagations.push((
                            format!("{}.{}", target_struct, &captures[2]),
                            format!("{}.{}", source_struct, &captures[4]),
                        ));
                    }
                }
            }
        }

        for _ in 0..3 {
            let mut changed = false;
            for (target, source) in &propagations {
                let Some(source_ids) = self.registrations.get(source).cloned() else {
                    continue;
                };
                let target_ids = self.registrations.entry(target.clone()).or_default();
                let before = target_ids.len();
                target_ids.extend(source_ids);
                changed |= target_ids.len() != before;
            }
            if !changed {
                break;
            }
        }
    }

    fn synthesize_dispatch_edges(&self) -> Vec<Edge> {
        let mut edges = Vec::new();
        let mut seen = HashSet::new();
        for file in &self.files {
            let Some(source) = self.stripped_source(file) else {
                continue;
            };
            for function in self.ctx.get_nodes_in_file(file) {
                if !is_function_kind(function.kind) {
                    continue;
                }
                let Some(body) = slice_lines(&source, function.start_line, function.end_line)
                else {
                    continue;
                };
                let mut added = 0usize;
                for captures in DISPATCH_RE.captures_iter(&body) {
                    if added >= FANOUT_CAP {
                        break;
                    }
                    let whole = captures.get(0).expect("whole field dispatch match");
                    let base = captures[1]
                        .trim_end()
                        .strip_suffix("->")
                        .or_else(|| captures[1].trim_end().strip_suffix('.'))
                        .unwrap_or(&captures[1])
                        .trim();
                    let field = &captures[2];
                    let Some(owners) = self.field_to_structs.get(field) else {
                        continue;
                    };
                    let mut owner = self
                        .resolve_chain_type(&body, base)
                        .filter(|candidate| owners.contains(candidate));
                    if owner.is_none() {
                        let without_subscripts = ARRAY_SUBSCRIPT_RE.replace_all(base, "");
                        let last = MEMBER_SPLIT_RE
                            .split(&without_subscripts)
                            .filter(|segment| !segment.is_empty())
                            .last();
                        owner = last
                            .and_then(|receiver| self.receiver_type_in(&body, receiver))
                            .filter(|candidate| owners.contains(candidate));
                    }
                    if owner.is_none() && owners.len() == 1 {
                        owner = owners.iter().next().cloned();
                    }
                    let Some(owner) = owner else {
                        continue;
                    };
                    let Some(targets) = self.registrations.get(&format!("{owner}.{field}")) else {
                        continue;
                    };
                    let line = function.start_line + count_newlines(&body[..whole.start()]);
                    for target in targets {
                        if target == &function.id
                            || !seen.insert(format!("{}>{target}", function.id))
                        {
                            continue;
                        }
                        edges.push(synthesized_edge(
                            &function.id,
                            target,
                            Some(line),
                            edge_meta(vec![
                                ("synthesizedBy", Value::from("fn-pointer-dispatch")),
                                ("via", Value::from(format!("{owner}.{field}"))),
                                (
                                    "registeredAt",
                                    Value::from(format!("{}:{line}", function.file_path)),
                                ),
                            ]),
                        ));
                        added += 1;
                        if added >= FANOUT_CAP {
                            break;
                        }
                    }
                }

                if added >= FANOUT_CAP || self.array_registrations.is_empty() {
                    continue;
                }
                for captures in ARRAY_DISPATCH_RE.captures_iter(&body) {
                    if added >= FANOUT_CAP {
                        break;
                    }
                    let array = &captures[1];
                    let Some(entries) = self.array_registrations.get(array) else {
                        continue;
                    };
                    let ids = if entries.len() == 1 {
                        Some(&entries[0].ids)
                    } else {
                        entries
                            .iter()
                            .find(|entry| entry.file == function.file_path)
                            .map(|entry| &entry.ids)
                    };
                    let Some(ids) = ids else {
                        continue;
                    };
                    let whole = captures.get(0).expect("whole array dispatch match");
                    let line = function.start_line + count_newlines(&body[..whole.start()]);
                    for target in ids {
                        if target == &function.id
                            || !seen.insert(format!("{}>{target}", function.id))
                        {
                            continue;
                        }
                        edges.push(synthesized_edge(
                            &function.id,
                            target,
                            Some(line),
                            edge_meta(vec![
                                ("synthesizedBy", Value::from("fn-pointer-dispatch")),
                                ("via", Value::from(format!("{array}[]"))),
                                (
                                    "registeredAt",
                                    Value::from(format!("{}:{line}", function.file_path)),
                                ),
                            ]),
                        ));
                        added += 1;
                        if added >= FANOUT_CAP {
                            break;
                        }
                    }
                }
            }
        }
        edges
    }
}

/// Build heuristic calls edges from C/C++ function-pointer dispatch sites to
/// registered handlers.
pub(super) fn c_fn_pointer_dispatch_edges(
    queries: &QueryBuilder,
    ctx: &dyn ResolutionContext,
) -> Result<Vec<Edge>> {
    let files: Vec<String> = ctx
        .get_all_files()
        .into_iter()
        .filter(|file| C_CPP_EXT_RE.is_match(file))
        .collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut synthesis = FnPtrSynthesis::new(ctx, files);
    synthesis.collect_typedefs();
    synthesis.collect_struct_layouts(queries)?;
    synthesis.collect_registrations();
    synthesis.collect_direct_assignments_and_propagate();
    if synthesis.registrations.is_empty() && synthesis.array_registrations.is_empty() {
        return Ok(Vec::new());
    }
    Ok(synthesis.synthesize_dispatch_edges())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::db::DatabaseConnection;
    use crate::resolution::types::ImportMapping;
    use crate::types::Language;

    struct TestContext {
        root: PathBuf,
        root_string: String,
        files: Vec<String>,
        queries: QueryBuilder,
    }

    impl ResolutionContext for TestContext {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.queries
                .get_nodes_by_file(file_path)
                .unwrap_or_default()
        }

        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.queries.get_nodes_by_name(name).unwrap_or_default()
        }

        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.queries
                .get_nodes_by_qualified_name_exact(qualified_name)
                .unwrap_or_default()
        }

        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.queries.get_nodes_by_kind(kind).unwrap_or_default()
        }

        fn file_exists(&self, file_path: &str) -> bool {
            self.root.join(file_path).exists()
        }

        fn read_file(&self, file_path: &str) -> Option<String> {
            fs::read_to_string(self.root.join(file_path)).ok()
        }

        fn get_project_root(&self) -> &str {
            &self.root_string
        }

        fn get_all_files(&self) -> Vec<String> {
            self.files.clone()
        }

        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.queries
                .get_nodes_by_lower_name(lower_name)
                .unwrap_or_default()
        }

        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    #[test]
    fn splits_only_at_top_level_delimiters() {
        assert_eq!(
            split_top_level("a, fn(x, y), {p, q}, tail", b','),
            vec!["a", " fn(x, y)", " {p, q}", " tail"]
        );
        assert_eq!(match_brace("{a,{b}} tail", 0), Some(6));
    }

    #[test]
    fn expands_macro_built_table_elements() {
        let definitions =
            parse_function_macros("#define MAKE_CMD(name, proc, flags) { name, flags, proc }\n");
        assert_eq!(
            expand_macro_calls("MAKE_CMD(\"get\", getCommand, READONLY)", &definitions),
            "{ \"get\", READONLY, getCommand }"
        );
    }

    #[test]
    fn evaluates_includer_controlled_conditional_tables() {
        let source =
            "#ifdef DECLARE_TABLE\nstruct cmd table[] = {{fn}};\n#else\nenum cmd { A };\n#endif";
        let active = eval_conditionals(source, &HashSet::from(["DECLARE_TABLE".to_string()]));
        assert!(active.contains("struct cmd table"));
        assert!(!active.contains("enum cmd"));
    }

    #[test]
    fn recognizes_inline_and_typedef_function_pointer_fields() {
        let pointer_types = HashSet::from(["hook_func".to_string()]);
        let function_types = HashSet::from(["redisCommandProc".to_string()]);
        let fields = parse_struct_fields(
            "int flags; void (*run)(int); hook_func hook; redisCommandProc *proc; struct child *next, *last;",
            &pointer_types,
            &function_types,
        );
        assert_eq!(
            fields
                .iter()
                .filter(|field| field.is_fn_ptr)
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            vec!["run", "hook", "proc"]
        );
        assert_eq!(fields[4].type_name, "child");
        assert_eq!(fields[5].type_name, "child");
    }

    #[test]
    fn resolves_object_macro_type_aliases_transitively() {
        let aliases = HashMap::from([
            ("COMMAND_STRUCT".to_string(), "redisCommand".to_string()),
            (
                "TABLE_TYPE".to_string(),
                "struct COMMAND_STRUCT".to_string(),
            ),
        ]);
        assert_eq!(resolve_type_name("TABLE_TYPE", &aliases), "redisCommand");
    }

    #[test]
    fn links_a_struct_field_dispatch_to_its_designated_handler() {
        let directory = tempdir().expect("temporary project");
        let source = "typedef void (*handler_t)(int);\nstruct hooks { handler_t run; };\nvoid on_event(int value) {}\nvoid dispatch(struct hooks *h) { h->run(1); }\nstatic struct hooks default_hooks = { .run = on_event };\n";
        fs::write(directory.path().join("hooks.c"), source).expect("write C fixture");
        let connection = DatabaseConnection::initialize(directory.path().join("codegraph.db"))
            .expect("initialize database");
        let queries = QueryBuilder::new(connection.get_db().expect("database handle"));
        queries
            .insert_nodes(&[
                Node::new(
                    "struct-hooks",
                    NodeKind::Struct,
                    "hooks",
                    "hooks",
                    "hooks.c",
                    Language::C,
                    2,
                    2,
                ),
                Node::new(
                    "handler",
                    NodeKind::Function,
                    "on_event",
                    "on_event",
                    "hooks.c",
                    Language::C,
                    3,
                    3,
                ),
                Node::new(
                    "dispatcher",
                    NodeKind::Function,
                    "dispatch",
                    "dispatch",
                    "hooks.c",
                    Language::C,
                    4,
                    4,
                ),
            ])
            .expect("insert fixture nodes");
        let context = TestContext {
            root: directory.path().to_path_buf(),
            root_string: directory.path().to_string_lossy().into_owned(),
            files: vec!["hooks.c".to_string()],
            queries,
        };

        let edges = c_fn_pointer_dispatch_edges(&context.queries, &context)
            .expect("synthesize function-pointer edge");
        let edge = edges
            .iter()
            .find(|edge| edge.source == "dispatcher" && edge.target == "handler")
            .expect("dispatcher reaches its registered handler");
        assert_eq!(edge.line, Some(4));
        assert_eq!(
            edge.metadata
                .as_ref()
                .and_then(|metadata| metadata.get("via"))
                .and_then(Value::as_str),
            Some("hooks.run")
        );
    }
}
