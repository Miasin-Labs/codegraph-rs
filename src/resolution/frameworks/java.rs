//! Java Framework Resolver
//!
//! Handles Spring Boot and general Java patterns.
//! Ported from `src/resolution/frameworks/java.ts`.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, Node, NodeKind};

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn line_of(content: &str, idx: usize) -> u32 {
    content[..idx].matches('\n').count() as u32 + 1
}

/// TS `safe.slice(i, i + 600)` — bounded lookahead window. Byte-based here
/// (TS sliced UTF-16 units); clamped back to a char boundary.
fn slice_bounded(s: &str, start: usize, max_len: usize) -> &str {
    let mut end = (start + max_len).min(s.len());
    while end > start && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[start..end]
}

// Class-level @RequestMapping prefix (an @RequestMapping whose tail leads to a
// `class`). Joined onto each method's path — and, crucially, NOT treated as a
// route itself (the old regex did, creating one bogus class route and missing
// every BARE method mapping like `@PostMapping` with the path on the class).
static CLASS_REQUEST_MAPPING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"@RequestMapping\s*\(([^)]*)\)\s*(?:@[\w.]+(?:\([^)]*\))?\s*)*(?:public\s+|final\s+|abstract\s+|open\s+|data\s+|sealed\s+)*class\b",
    )
    .unwrap()
});

// Verb-specific method mappings — always method-level, BARE or with a path.
static MAPPING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping)\b\s*(\([^)]*\))?")
        .unwrap()
});

// Method it decorates: first declared method after (skip stacked annotations;
// Java puts the return type before the name; Kotlin uses `fun name(...)`).
static METHOD_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bfun\s+(\w+)\s*\(|\b(?:public|private|protected)\s+[^;{=]*?\s+(\w+)\s*\(")
        .unwrap()
});

// Method-level @RequestMapping (older style).
static REQUEST_MAPPING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@RequestMapping\b\s*(\([^)]*\))?").unwrap());

// Does the tail after an @RequestMapping lead to a class declaration?
static CLASS_AFTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:@[\w.]+(?:\([^)]*\))?\s*)*(?:public\s+|final\s+|abstract\s+|open\s+|data\s+|sealed\s+)*class\b",
    )
    .unwrap()
});

static REQUEST_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"method\s*=\s*(?:RequestMethod\.)?(\w+)").unwrap());

static MAPPING_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']([^"']*)["']"#).unwrap());

static VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@Value\s*\(\s*["']\$\{([^}:]+)(?::[^}]*)?\}["']\s*\)"#).unwrap()
});
static CONFIG_PROPERTIES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@ConfigurationProperties\s*\(\s*(?:prefix\s*=\s*)?["']([^"']+)["']"#).unwrap()
});

static SPRING_CONFIG_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(application|bootstrap)(-[\w.-]+)?\.(yml|yaml|properties)$").unwrap()
});
static SPRING_BASE_CONFIG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(application|bootstrap)\.(yml|yaml|properties)$").unwrap());

static ENTITY_NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());

/// Spring framework resolver (TS `springResolver`).
pub struct SpringResolver;

impl FrameworkResolver for SpringResolver {
    fn name(&self) -> &str {
        "spring"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[
            Language::Java,
            Language::Kotlin,
            Language::Yaml,
            Language::Properties,
        ])
    }

    /// `@ConfigurationProperties(prefix="app.cache")` emits a reference whose
    /// name carries the `:prefix` sentinel — there's no declared symbol with
    /// that exact spelling, so the resolver's name-existence pre-filter would
    /// drop it. Opt those through.
    fn claims_reference(&self, name: &str) -> bool {
        name.ends_with(":prefix")
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for pom.xml with Spring
        if let Some(pom_xml) = context.read_file("pom.xml") {
            if pom_xml.contains("spring-boot") || pom_xml.contains("springframework") {
                return true;
            }
        }

        // Check for build.gradle with Spring
        if let Some(build_gradle) = context.read_file("build.gradle") {
            if build_gradle.contains("spring-boot") || build_gradle.contains("springframework") {
                return true;
            }
        }

        if let Some(build_gradle_kts) = context.read_file("build.gradle.kts") {
            if build_gradle_kts.contains("spring-boot")
                || build_gradle_kts.contains("springframework")
            {
                return true;
            }
        }

        // Check for Spring annotations in Java files
        let all_files = context.get_all_files();
        for file in &all_files {
            if file.ends_with(".java") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("@SpringBootApplication")
                        || content.contains("@RestController")
                        || content.contains("@Service")
                        || content.contains("@Repository")
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();

        // Spring config-key references — `@Value("${key}")` (single leaf) and
        // `@ConfigurationProperties(prefix="X")` (entire subtree, marked with the
        // `:prefix` suffix in extract_spring_value_bindings). Lookup goes through
        // Spring's relaxed binding (kebab/camel/snake → canonical lowercase).
        if let Some(prefix) = name.strip_suffix(":prefix") {
            let canon_prefix = canonical_config_key(prefix);
            // Prefer an exact prefix match (one node = the prefix subtree). Without
            // node-level subtree representation we map to the closest matching key.
            let candidates: Vec<Node> = context
                .get_nodes_by_kind(NodeKind::Constant)
                .into_iter()
                .filter(|n| {
                    (n.language == Language::Yaml || n.language == Language::Properties)
                        && canonical_config_key(&n.qualified_name).starts_with(&canon_prefix)
                })
                .collect();
            if candidates.is_empty() {
                return None;
            }
            // Pick the SHORTEST canonical name — it's the closest binding point
            // (`app.cache` over `app.cache.name.user-token` for prefix=`app.cache`).
            let best = candidates.iter().skip(1).fold(&candidates[0], |a, b| {
                if canonical_config_key(&a.qualified_name).len()
                    <= canonical_config_key(&b.qualified_name).len()
                {
                    a
                } else {
                    b
                }
            });
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: best.id.clone(),
                confidence: 0.85,
                resolved_by: ResolvedBy::Framework,
            });
        }
        if (reference.language == Language::Java || reference.language == Language::Kotlin)
            && name.contains('.')
            && !name.contains("::")
            // Exclude method-call style (single-dot, both sides lower-camel). Spring
            // config keys are typically 3+ segments and contain kebabs/dashes; we
            // can't filter perfectly but skipping single-dot keeps the lookup tight.
            && name.split('.').count() >= 2
        {
            let canon_ref = canonical_config_key(name);
            let candidates: Vec<Node> = context
                .get_nodes_by_kind(NodeKind::Constant)
                .into_iter()
                .filter(|n| {
                    (n.language == Language::Yaml || n.language == Language::Properties)
                        && canonical_config_key(&n.qualified_name) == canon_ref
                })
                .collect();
            if candidates.len() == 1 {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: candidates[0].id.clone(),
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Framework,
                });
            }
            if candidates.len() > 1 {
                // Multiple profile-specific files (application-dev.yml +
                // application-prod.yml) can define the same key. Prefer the one with
                // the shortest profile suffix (the base `application.yml` wins over
                // profile variants when both exist), then by alphabetical path so the
                // pick is deterministic across reindexes.
                let score = |n: &Node| -> usize {
                    let base = n.file_path.split('/').next_back().unwrap_or("");
                    let is_base = SPRING_BASE_CONFIG_RE.is_match(base);
                    (if is_base { 0 } else { 1 }) * 1000 + base.len()
                };
                let best = candidates.iter().skip(1).fold(&candidates[0], |a, b| {
                    if score(a) <= score(b) { a } else { b }
                });
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: best.id.clone(),
                    confidence: 0.75,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 1: Service references (dependency injection)
        if name.ends_with("Service") {
            if let Some(result) =
                resolve_by_name_and_kind(name, SERVICE_KINDS, SERVICE_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Repository references
        if name.ends_with("Repository") {
            if let Some(result) = resolve_by_name_and_kind(name, SERVICE_KINDS, REPO_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Controller references
        if name.ends_with("Controller") {
            if let Some(result) =
                resolve_by_name_and_kind(name, CLASS_KINDS, CONTROLLER_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Entity/Model references
        if ENTITY_NAME_RE.is_match(name) {
            if let Some(result) = resolve_by_name_and_kind(name, CLASS_KINDS, ENTITY_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 5: Component references
        if name.ends_with("Component") || name.ends_with("Config") {
            if let Some(result) =
                resolve_by_name_and_kind(name, CLASS_KINDS, COMPONENT_DIRS, context)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        // Spring config files (application.yml / application.properties /
        // bootstrap.yml + per-profile variants) are extracted on the framework
        // path, not in the language extractor, so the keys become first-class
        // nodes a `@Value("${k}")` reference can resolve to.
        if is_spring_config_file(file_path) {
            return Some(extract_spring_config(file_path, content));
        }
        // Spring Boot is used from both Java and Kotlin (identical @GetMapping etc.
        // annotations); the difference is method syntax — Kotlin `fun name(...)` vs
        // Java `public X name(...)` — handled in the method regex below.
        if !file_path.ends_with(".java") && !file_path.ends_with(".kt") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let lang = if file_path.ends_with(".kt") {
            Language::Kotlin
        } else {
            Language::Java
        };
        let safe = strip_comments_for_regex(content, CommentLang::Java);

        let mut class_prefix = String::new();
        if let Some(cls) = CLASS_REQUEST_MAPPING_RE.captures(&safe) {
            class_prefix = parse_mapping_path(&cls[1]);
        }

        for m in MAPPING_RE.captures_iter(&safe) {
            let method = match &m[1] {
                "GetMapping" => "GET",
                "PostMapping" => "POST",
                "PutMapping" => "PUT",
                "PatchMapping" => "PATCH",
                "DeleteMapping" => "DELETE",
                _ => unreachable!(),
            };
            let args = m.get(2).map(|g| g.as_str()).unwrap_or("");
            let sub = parse_mapping_path(strip_outer_parens(args));
            let route_path = join_path(&class_prefix, &sub);
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                lang,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            // Method it decorates: first declared method after (skip stacked annotations;
            // Java puts the return type before the name). Bounded so we don't grab a far one.
            let tail = slice_bounded(&safe, whole.end(), 600);
            if let Some(method_match) = METHOD_DECL_RE.captures(tail) {
                let handler = method_match
                    .get(1)
                    .or_else(|| method_match.get(2))
                    .unwrap()
                    .as_str();
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: handler.to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: lang,
                    candidates: None,
                });
            }
        }

        // Method-level @RequestMapping (older style: `@RequestMapping(value="/x",
        // method=RequestMethod.GET)` on a method). The class-level @RequestMapping is
        // the prefix (handled above) — skip it here so it isn't double-counted.
        for m in REQUEST_MAPPING_RE.captures_iter(&safe) {
            let args = strip_outer_parens(m.get(1).map(|g| g.as_str()).unwrap_or(""));
            let whole = m.get(0).unwrap();
            let after = slice_bounded(&safe, whole.end(), 600);
            if CLASS_AFTER_RE.is_match(after) {
                continue; // class-level prefix
            }
            let Some(method_match) = METHOD_DECL_RE.captures(after) else {
                continue;
            };
            let method = REQUEST_METHOD_RE
                .captures(args)
                .map(|v| v[1].to_uppercase())
                .unwrap_or_else(|| "ANY".to_string());
            let route_path = join_path(&class_prefix, &parse_mapping_path(args));
            let line = line_of(&safe, whole.start());
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                lang,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);
            let handler = method_match
                .get(1)
                .or_else(|| method_match.get(2))
                .unwrap()
                .as_str();
            references.push(UnresolvedRef {
                from_node_id: route_id,
                reference_name: handler.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: lang,
                candidates: None,
            });
        }

        // @Value("${key}") and @ConfigurationProperties(prefix="...") — bind
        // Spring config-key references in Java/Kotlin source. The reference target
        // is the corresponding YAML/properties leaf-key node emitted by
        // extract_spring_config; SpringResolver::resolve looks it up with relaxed
        // binding (kebab/camel/snake collapse).
        extract_spring_value_bindings(file_path, &safe, lang, now, &mut nodes, &mut references);

        Some(FrameworkExtractionResult { nodes, references })
    }
}

/// Spring config file patterns: application(-profile)?.{yml,yaml,properties} +
/// bootstrap variants. Matches the basename, not the path, so a project that
/// vendors `application.yml` under `src/main/resources` and one under `src/test/
/// resources` are both picked up.
fn is_spring_config_file(file_path: &str) -> bool {
    let base = file_path.split('/').next_back().unwrap_or("");
    SPRING_CONFIG_FILE_RE.is_match(base)
}

/// Parse a Spring config file (YAML or .properties) and emit one `constant`
/// node per LEAF key, with `qualified_name` = the dotted path. Leaf keys are
/// what `@Value("${k}")` references hit; intermediate keys aren't bound by
/// Spring's `@Value` (a `@ConfigurationProperties` class binds a SUBTREE, and
/// those references are resolved at lookup time by prefix-suffix matching).
fn extract_spring_config(file_path: &str, content: &str) -> FrameworkExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let is_properties = file_path.to_lowercase().ends_with(".properties");
    let lang = if is_properties {
        Language::Properties
    } else {
        Language::Yaml
    };
    let now = now_millis();

    let mut emit_leaf = |dotted_key: &str, line: u32, value_text: &str| {
        if dotted_key.is_empty() {
            return;
        }
        let mut node = Node::new(
            format!("spring-config:{file_path}:{line}:{dotted_key}"),
            NodeKind::Constant,
            dotted_key.split('.').next_back().unwrap_or(dotted_key),
            dotted_key,
            file_path,
            lang,
            line,
            line,
        );
        node.end_column = value_text.chars().count() as u32;
        node.signature = Some(dotted_key.to_string());
        node.docstring = Some(value_text.chars().take(200).collect());
        node.updated_at = now;
        nodes.push(node);
    };

    if is_properties {
        // Properties format: `k1.k2.k3 = value` (or `:` separator, or no value).
        // Lines starting with `#`/`!` are comments. Backslash continuations are
        // valid but rare; we don't try to join them (a continued value is still
        // a value of the same key).
        for (i, raw) in content.split(['\n']).map(strip_cr).enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            let Some(sep) = properties_sep_index(raw) else {
                continue;
            };
            let key = raw[..sep].trim();
            let val = raw[sep + 1..].trim();
            emit_leaf(key, i as u32 + 1, val);
        }
        return FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        };
    }

    // YAML: indent-based. We track a stack of (indent, key) so the dotted path
    // is built by joining ancestor keys with `.`. A leaf is a line with a value
    // on the same line (after `:`). List items, flow-style scalars, and `---`
    // separators are ignored — they don't bind to `@Value` anyway.
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (i, raw) in content.split(['\n']).map(strip_cr).enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed == "---"
            || trimmed.starts_with("- ")
        {
            continue;
        }
        let indent = raw.len() - raw.trim_start_matches(['\t', ' ']).len();
        let Some(colon_idx) = yaml_colon_index(raw) else {
            continue;
        };
        if colon_idx < indent {
            continue;
        }
        let key = raw[indent..colon_idx].trim();
        if key.is_empty() {
            continue;
        }
        let after = raw[colon_idx + 1..].trim();
        while stack.last().is_some_and(|(si, _)| *si >= indent) {
            stack.pop();
        }
        let dotted = stack
            .iter()
            .map(|(_, k)| k.as_str())
            .chain(std::iter::once(key))
            .collect::<Vec<&str>>()
            .join(".");
        if after.is_empty() || after.starts_with('#') {
            stack.push((indent, key.to_string()));
        } else {
            // A leaf with an inline value (or a flow-mapping like `{ a: 1 }` — we
            // emit it as a leaf, not as a subtree; precision is fine for `@Value`).
            let val_stripped = strip_wrapping_quotes(after);
            emit_leaf(&dotted, i as u32 + 1, val_stripped);
        }
    }
    FrameworkExtractionResult {
        nodes,
        references: Vec::new(),
    }
}

/// TS `content.split(/\r?\n/)` parity: drop a trailing `\r` left by `split('\n')`.
fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Find the first unescaped `=` or `:` separator in a .properties line.
fn properties_sep_index(raw: &str) -> Option<usize> {
    let bytes = raw.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        let ch = bytes[j];
        if ch == b'=' || ch == b':' {
            return Some(j);
        }
        if ch == b'\\' && j + 1 < bytes.len() {
            j += 2;
            continue;
        }
        j += 1;
    }
    None
}

/// Find the first `:` outside of a quoted string in a YAML line.
fn yaml_colon_index(raw: &str) -> Option<usize> {
    let mut in_str: Option<char> = None;
    let mut prev: Option<char> = None;
    for (j, ch) in raw.char_indices() {
        if let Some(q) = in_str {
            if ch == q && prev != Some('\\') {
                in_str = None;
            }
            prev = Some(ch);
            continue;
        }
        if ch == '"' || ch == '\'' {
            in_str = Some(ch);
            prev = Some(ch);
            continue;
        }
        if ch == ':' {
            return Some(j);
        }
        prev = Some(ch);
    }
    None
}

/// TS `after.replace(/^["']|["']$/g, '')` — strip one leading and one trailing quote.
fn strip_wrapping_quotes(s: &str) -> &str {
    let s = s.strip_prefix(['"', '\'']).unwrap_or(s);
    s.strip_suffix(['"', '\'']).unwrap_or(s)
}

/// Append `@Value("${k}")` and `@ConfigurationProperties(prefix=...)`
/// references discovered in `safe` (comments stripped) into the caller's
/// `nodes`/`references` vectors.
fn extract_spring_value_bindings(
    file_path: &str,
    safe: &str,
    lang: Language,
    now: i64,
    nodes: &mut Vec<Node>,
    references: &mut Vec<UnresolvedRef>,
) {
    for m in VALUE_RE.captures_iter(safe) {
        let key = m[1].trim().to_string();
        if key.is_empty() {
            continue;
        }
        let whole = m.get(0).unwrap();
        let line = line_of(safe, whole.start());
        let mut bind_node = Node::new(
            format!("spring-value:{file_path}:{line}:{key}"),
            NodeKind::Constant,
            key.clone(),
            format!("{file_path}::@Value:{key}"),
            file_path,
            lang,
            line,
            line,
        );
        bind_node.end_column = whole.as_str().len() as u32;
        bind_node.signature = Some(format!("@Value(\"{key}\")"));
        bind_node.updated_at = now;
        let bind_id = bind_node.id.clone();
        nodes.push(bind_node);
        references.push(UnresolvedRef {
            from_node_id: bind_id,
            reference_name: key,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language: lang,
            candidates: None,
        });
    }

    for m in CONFIG_PROPERTIES_RE.captures_iter(safe) {
        let prefix = m[1].trim().to_string();
        if prefix.is_empty() {
            continue;
        }
        let whole = m.get(0).unwrap();
        let line = line_of(safe, whole.start());
        let mut bind_node = Node::new(
            format!("spring-cp:{file_path}:{line}:{prefix}"),
            NodeKind::Constant,
            prefix.clone(),
            format!("{file_path}::@ConfigurationProperties:{prefix}"),
            file_path,
            lang,
            line,
            line,
        );
        bind_node.end_column = whole.as_str().len() as u32;
        bind_node.signature = Some(format!("@ConfigurationProperties(\"{prefix}\")"));
        bind_node.updated_at = now;
        let bind_id = bind_node.id.clone();
        nodes.push(bind_node);
        references.push(UnresolvedRef {
            from_node_id: bind_id,
            // Mark the reference with a `:prefix` suffix so SpringResolver::resolve
            // knows to expand it into the SUBTREE rather than a single key.
            reference_name: format!("{prefix}:prefix"),
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language: lang,
            candidates: None,
        });
    }
}

/// Spring's relaxed binding (`cache-list` ↔ `cacheList` ↔ `cache_list` ↔
/// `CACHE_LIST`) collapses on lowercase + dash/underscore removal. We compare
/// candidate keys to a reference in this canonical form.
fn canonical_config_key(key: &str) -> String {
    key.to_lowercase().replace(['-', '_'], "")
}

// Directory patterns
const SERVICE_DIRS: &[&str] = &["/service/", "/services/"];
const REPO_DIRS: &[&str] = &["/repository/", "/repositories/"];
const CONTROLLER_DIRS: &[&str] = &["/controller/", "/controllers/"];
const ENTITY_DIRS: &[&str] = &["/entity/", "/entities/", "/model/", "/models/", "/domain/"];
const COMPONENT_DIRS: &[&str] = &["/component/", "/components/", "/config/"];

const CLASS_KINDS: &[NodeKind] = &[NodeKind::Class];
const SERVICE_KINDS: &[NodeKind] = &[NodeKind::Class, NodeKind::Interface];

/// TS `(match[2] || '').replace(/^\(|\)$/g, '')` — strip one leading `(` and
/// one trailing `)`.
fn strip_outer_parens(args: &str) -> &str {
    let args = args.strip_prefix('(').unwrap_or(args);
    args.strip_suffix(')').unwrap_or(args)
}

/// Path string from a mapping's args (`"/x"`, `value = "/x"`, `path = "/x"`); '' if bare.
fn parse_mapping_path(args: &str) -> String {
    MAPPING_PATH_RE
        .captures(args)
        .map(|m| m[1].to_string())
        .unwrap_or_default()
}

/// Join a class-level prefix and a method sub-path into one normalized `/path`.
fn join_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = [prefix, sub]
        .iter()
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// Resolve a symbol by name using indexed queries instead of scanning all files.
fn resolve_by_name_and_kind(
    name: &str,
    kinds: &[NodeKind],
    preferred_dir_patterns: &[&str],
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    let kind_filtered: Vec<&Node> = candidates
        .iter()
        .filter(|n| kinds.contains(&n.kind))
        .collect();
    if kind_filtered.is_empty() {
        return None;
    }

    // Prefer candidates in framework-conventional directories
    if let Some(preferred) = kind_filtered.iter().find(|n| {
        preferred_dir_patterns
            .iter()
            .any(|d| n.file_path.contains(d))
    }) {
        return Some(preferred.id.clone());
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}
