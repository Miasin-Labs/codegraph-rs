//! Expo Modules framework — close the JS → native flow for Expo SDK packages.
//!
//! Expo Modules use a Swift / Kotlin DSL distinct from the React Native legacy
//! bridge. Each native module is a class extending `Module` whose
//! `definition()` body declares the JS surface via literal `Name(...)`,
//! `Function(...)`, `AsyncFunction(...)`, `Property(...)`, and `View {...}`
//! calls. Tree-sitter parses these as ordinary call_expressions with trailing
//! closures, so the JS-visible methods don't exist as named symbol nodes by
//! default — `Camera.takePictureAsync(...)` on the JS side has nothing to
//! resolve to.
//!
//! This framework extractor walks the file source for those declarative
//! literals and emits method nodes named `takePictureAsync` /
//! `notificationAsync` / `width` / etc., attributed to the Swift / Kotlin
//! file. The standard name-matcher then resolves JS `Foo.takePictureAsync(...)`
//! to them via the existing `obj.method` → method-name path — no separate
//! resolve() branch needed.
//!
//! Real-world shape (expo-haptics):
//!
//! ```text
//!   public class HapticsModule: Module {
//!     public func definition() -> ModuleDefinition {
//!       Name("ExpoHaptics")
//!       AsyncFunction("notificationAsync") { ... }
//!       AsyncFunction("impactAsync") { ... }
//!       AsyncFunction("selectionAsync") { ... }
//!     }
//!   }
//! ```
//!
//! Kotlin Module declarations are the same DSL (the API mirrors Swift).
//!
//! Anti-goals (deferred):
//! - The trailing-closure BODY is not extracted as the method's body — it
//!   remains attributed to `definition()` in the existing extraction. Future
//!   work could synthesize a body-range for richer `trace` output, but the
//!   reachability (which is the bridge's main value) is already complete.
//! - `View { ... }` blocks expose JSX prop bindings; that overlaps with
//!   Fabric (Phase 6) and is left to that phase.
//!
//! Ported from `src/resolution/frameworks/expo-modules.ts`.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Match `Function("name")`, `AsyncFunction("name")`, or `Property("name")`
/// at the start of an expression (line-anchored after optional whitespace).
/// The trailing closure that follows isn't captured — we just need the name
/// literal that becomes the JS-visible method.
///
/// NOTE: the regex deliberately requires the open paren to live on the same
/// line as the keyword, which matches every real Expo Module declaration
/// style. Multi-line `AsyncFunction(\n"x"\n)` forms aren't a real shape in
/// the SDK; if any appear we'd extend the regex.
static EXPO_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(Function|AsyncFunction|Property|Constants)\s*\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']"#,
    )
    .unwrap()
});

/// Match the module name literal `Name("ExpoX")`. Used to enrich each emitted
/// method's qualifiedName so the same JS callsite to `Foo.fn` doesn't ambiguate
/// across multiple Expo modules in a monorepo.
static EXPO_MODULE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bName\s*\(\s*["']([A-Za-z_][A-Za-z0-9_]*)["']"#).unwrap());

/// Heuristic class-name match — used as a fallback if `Name(...)` literal
/// isn't found. Detects `class XxxModule: Module` (Swift) or
/// `class XxxModule : Module` (Kotlin / with whitespace tolerance).
static EXPO_CLASS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*Module\b").unwrap());

/// Detect whether a file is plausibly an Expo Module — looking for both
/// the `: Module` inheritance and at least one declarative `Function(...)`
/// / `AsyncFunction(...)` / `Property(...)` / `Name(...)` literal. Any one
/// of those alone produces too many false positives (random Swift code can
/// have `class X: Module` for unrelated reasons).
fn is_expo_module_source(source: &str) -> bool {
    if !EXPO_CLASS_RE.is_match(source) {
        return false;
    }
    EXPO_DECL_RE.is_match(source)
}

/// Extract Expo Module method declarations from a Swift / Kotlin source
/// file. Each `Function("X") { … }` / `AsyncFunction("X") { … }` /
/// `Property("X") { … }` literal becomes a method node named `X`,
/// attributed to the file at the line of the literal.
fn extract_expo_methods(file_path: &str, source: &str, language: Language) -> Vec<Node> {
    if !is_expo_module_source(source) {
        return Vec::new();
    }
    let mut nodes: Vec<Node> = Vec::new();

    let name_match = EXPO_MODULE_NAME_RE
        .captures(source)
        .map(|c| c[1].to_string());
    let class_match = EXPO_CLASS_RE.captures(source).map(|c| c[1].to_string());
    // Prefer the explicit `Name("X")` literal — that's the JS-visible
    // module name. Class name is the fallback.
    let module_name = name_match
        .or(class_match)
        .unwrap_or_else(|| "ExpoModule".to_string());

    let now = now_ms();
    let mut seen_at_line: HashSet<String> = HashSet::new();
    for m in EXPO_DECL_RE.captures_iter(source) {
        let whole = m.get(0).unwrap();
        let kind = &m[1];
        let method_name = &m[2];
        // Compute line number from match index.
        let before = &source[..whole.start()];
        let start_line = (before.matches('\n').count() + 1) as u32;
        // Avoid duplicates if the same method literal appears twice in one
        // file (e.g., declared and re-declared inside a `View {...}` block).
        let dedup_key = format!("{method_name}:{start_line}");
        if seen_at_line.contains(&dedup_key) {
            continue;
        }
        seen_at_line.insert(dedup_key);

        let start_column = match before.rfind('\n') {
            Some(i) => (before.len() - i - 1) as u32,
            None => before.len() as u32,
        };
        let mut node = Node::new(
            format!("expo-module:{file_path}:{module_name}:{method_name}:{start_line}"),
            NodeKind::Method,
            method_name,
            format!("{file_path}::{module_name}.{method_name}"),
            file_path,
            language,
            start_line,
            // We don't extract the closure body's end-line — use the literal's
            // line as a single-line range. trace/explore still surfaces the
            // declaration site, which is the main user-visible signal.
            start_line,
        );
        node.start_column = start_column;
        node.end_column = start_column + (kind.len() + 2 + method_name.len() + 2) as u32;
        node.docstring = Some(format!(
            "Expo Modules {kind}(\"{method_name}\") in {module_name}"
        ));
        node.signature = Some(format!("{kind}(\"{method_name}\")"));
        node.is_exported = Some(true);
        node.updated_at = now;
        nodes.push(node);
    }

    nodes
}

/// `expoModulesResolver` — unit struct implementing [`FrameworkResolver`].
pub struct ExpoModulesResolver;

const EXPO_LANGUAGES: [Language; 2] = [Language::Swift, Language::Kotlin];

impl FrameworkResolver for ExpoModulesResolver {
    fn name(&self) -> &str {
        "expo-modules"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&EXPO_LANGUAGES)
    }

    /// Detect Expo Modules by looking at the project's package.json or
    /// a small scan of source files for the `: Module` + declarative-DSL
    /// markers. Either signal suffices.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        static PKG_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"["']expo-modules-core["']\s*:"#).unwrap());
        if let Some(pkg) = context.read_file("package.json") {
            if PKG_RE.is_match(&pkg) {
                return true;
            }
        }
        let files = context.get_all_files();
        for f in files.iter().take(200) {
            if f.ends_with(".swift") || f.ends_with(".kt") {
                if let Some(src) = context.read_file(f) {
                    if is_expo_module_source(&src) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Per-file extraction — the orchestrator invokes this for every
    /// `.swift` / `.kt` file in the project. We only emit nodes when the
    /// file looks like an Expo Module; otherwise return empty.
    fn extract(&self, file_path: &str, source: &str) -> Option<FrameworkExtractionResult> {
        let language = if file_path.ends_with(".kt") {
            Language::Kotlin
        } else {
            Language::Swift
        };
        Some(FrameworkExtractionResult {
            nodes: extract_expo_methods(file_path, source, language),
            references: Vec::new(),
        })
    }

    /// No bespoke resolution needed — the synthetic method nodes emitted by
    /// `extract()` get picked up by the standard name-matcher when a JS
    /// callsite like `Foo.takePictureAsync(args)` resolves. Returning None
    /// here is correct.
    fn resolve(
        &self,
        _reference: &UnresolvedRef,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }
}
