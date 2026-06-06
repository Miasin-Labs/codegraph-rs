//! React Native cross-language bridge resolver.
//!
//! Closes the JS ↔ native flow gap in React Native projects. Covers:
//!
//! **Legacy bridge** (older / still-prevalent in mid-tier RN libs):
//!   - ObjC: `RCT_EXPORT_MODULE([opt_name])` declares a module; the module
//!     name defaults to the class name minus an `RCT` prefix when no
//!     argument is given. `RCT_EXPORT_METHOD(selector:(args))` declares a
//!     JS-callable method whose JS name is the selector's first keyword.
//!     `RCT_REMAP_METHOD(jsName, nativeSelector:(args))` overrides the JS
//!     name explicitly.
//!   - Java/Kotlin: `@ReactMethod` annotated methods on a
//!     `ReactContextBaseJavaModule` subclass; the module name comes from
//!     `getName()` returning a literal string.
//!
//! **TurboModules** (modern, used by react-native-svg, screens, FBSDK
//! Next-gen libraries):
//!   - TS spec interface declared in a `Native<X>.ts` file exporting
//!     `TurboModuleRegistry.getEnforcing<Spec>('<ModuleName>')` (or
//!     `.get<Spec>('<ModuleName>')`). The Spec interface methods are the
//!     JS-callable surface; the matching native implementation is a class
//!     whose method names match (selector first-keyword on ObjC,
//!     identifier on Kotlin/Java).
//!
//! The two mechanisms share an end shape: a map from `(moduleName,
//! jsMethodName)` to a native method node, plus a smaller map from
//! `jsMethodName` alone for cases where the JS callsite doesn't carry
//! the module qualifier (the most common JS pattern is
//! `import Geo from './NativeGeolocation'; Geo.getPosition()` — the
//! receiver is the default export, not literally `NativeModules.<Mod>`,
//! so name-by-method-only is what actually resolves in practice).
//!
//! **Not covered** (deferred to a follow-up phase, per design doc §6):
//!   - Fabric view components (`RCT_EXPORT_VIEW_PROPERTY` / Codegen view
//!     specs) — these connect JSX props to native renderers, a different
//!     flow shape that composes with the existing JSX synthesizer.
//!   - Native → JS events (`RCTEventEmitter` / `NativeEventEmitter`) —
//!     belongs in the callback synthesizer's cross-language channel.
//!
//! Ported from `src/resolution/frameworks/react-native.ts`.
//!
//! Rust-port deviation: the TS file cached the built maps in a
//! per-context `WeakMap`; here the cache lives on the resolver instance
//! (`Mutex<Option<RnMaps>>`). Construct a fresh `ReactNativeBridgeResolver`
//! per resolution run/context (the TS registry effectively did the same —
//! one context per resolver lifetime).

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::resolution::types::{
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

/// One native RN method known to the resolver. Indexed by JS-visible name.
#[derive(Debug, Clone)]
struct NativeMethod {
    /// Module name as seen from JS (`Geolocation`, `RNSVGRenderableModule`, …).
    #[allow(dead_code)]
    module_name: String,
    /// JS-visible method name.
    #[allow(dead_code)]
    js_name: String,
    /// Native implementation node (ObjC method / Java method / Kotlin function).
    node: Node,
}

#[derive(Debug, Default)]
struct RnMaps {
    by_js_name: HashMap<String, Vec<NativeMethod>>,
}

// ─── Native-side extraction ─────────────────────────────────────────────────

/// Default ObjC module name when `RCT_EXPORT_MODULE()` has no argument:
/// strip a leading `RCT` prefix from the class name (Apple's convention)
/// and treat the rest as the JS-visible module name. `RCTGeolocation` →
/// `Geolocation`. Class names without an `RCT` prefix are returned
/// unchanged.
fn default_objc_module_name(class_name: &str) -> &str {
    if class_name.starts_with("RCT") && class_name.len() > 3 {
        &class_name[3..]
    } else {
        class_name
    }
}

struct ObjcExport {
    module_name: String,
    js_name: String,
    native_selector_first_kw: String,
}

/// Parse an ObjC `.m`/`.mm` file's source for `RCT_EXPORT_MODULE` and
/// `RCT_EXPORT_METHOD` / `RCT_REMAP_METHOD` declarations, returning the
/// inferred (moduleName, jsMethodName) pairs.
///
/// The macro forms (a single `RCT_EXPORT_MODULE` per file conventionally
/// matched to a single `@implementation`):
///   - `RCT_EXPORT_MODULE()` — module name = class name with `RCT` prefix
///     stripped
///   - `RCT_EXPORT_MODULE(jsName)` — explicit name
///   - `RCT_EXPORT_METHOD(selector:(arg1)label1:(arg2)label2)` — JS name =
///     `selector` (the first keyword)
///   - `RCT_REMAP_METHOD(jsName, selector:(arg1)label1:(arg2)label2)` —
///     JS name = literal `jsName`
///
/// Regex-based scan is sufficient — these macros are highly stylized and
/// appear at top level. Pulling them out of the full AST would require a
/// macro-aware ObjC parse the tree-sitter grammar doesn't provide.
fn parse_objc_rn_exports(source: &str, class_name: Option<&str>) -> Vec<ObjcExport> {
    static MODULE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"RCT_EXPORT_MODULE\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)?\s*\)").unwrap()
    });
    static EXPORT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"RCT_EXPORT_METHOD\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)").unwrap());
    static REMAP_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"RCT_REMAP_METHOD\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)",
        )
        .unwrap()
    });

    let mut results: Vec<ObjcExport> = Vec::new();

    // RCT_EXPORT_MODULE — one per file by convention. Capture the optional arg.
    let module_match = MODULE_RE.captures(source);
    // Need a module name to attribute methods. Prefer the explicit macro arg,
    // then the class name, then bail (no module = nothing useful to register).
    let module_name = module_match
        .as_ref()
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .or_else(|| class_name.map(|c| default_objc_module_name(c).to_string()));
    let Some(module_name) = module_name else {
        return results;
    };

    // RCT_EXPORT_METHOD(selectorFirstKw:(args)…)
    // The first keyword (everything up to the first `:` or open paren) is the
    // JS-visible name. We don't try to parse full multi-keyword selectors —
    // RN's JS view of the method uses only the first keyword.
    for m in EXPORT_RE.captures_iter(source) {
        let kw = &m[1];
        results.push(ObjcExport {
            module_name: module_name.clone(),
            js_name: kw.to_string(),
            native_selector_first_kw: kw.to_string(),
        });
    }

    // RCT_REMAP_METHOD(jsName, nativeSelectorFirstKw:(args)…)
    for m in REMAP_RE.captures_iter(source) {
        results.push(ObjcExport {
            module_name: module_name.clone(),
            js_name: m[1].to_string(),
            native_selector_first_kw: m[2].to_string(),
        });
    }

    results
}

/// Find the `@implementation` class name in an ObjC file — used as the
/// fallback module name when `RCT_EXPORT_MODULE()` has no argument.
/// (Categories of the form `@implementation Foo (Bar)` are correctly
/// captured here as `Foo`, but a category file probably isn't where a
/// fresh `RCT_EXPORT_MODULE` lives anyway.)
fn find_objc_class_name(source: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"@implementation\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());
    RE.captures(source).map(|c| c[1].to_string())
}

struct JvmExport {
    module_name: String,
    js_name: String,
}

/// Parse a Java/Kotlin source file for `@ReactMethod` annotated methods
/// and the surrounding class's `getName()` return value (the JS-visible
/// module name).
///
/// Java: `@ReactMethod public void getCurrentPosition(Callback cb) { … }`
/// Kotlin: `@ReactMethod fun getCurrentPosition(cb: Callback) { … }`
///
/// Class name comes from `class XxxModule extends ReactContextBaseJavaModule`
/// (Java) or `class XxxModule : ReactContextBaseJavaModule(...)` (Kotlin).
/// The JS-visible module name comes from `getName()` returning a literal
/// string — fall back to the class name with a `Module` suffix stripped
/// when the literal isn't present.
fn parse_jvm_rn_exports(source: &str) -> Vec<JvmExport> {
    static GET_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"\bgetName\s*\([^)]*\)\s*(?::\s*String)?\s*(?:=\s*|\{[^}]*return\s*)"([^"]+)""#,
        )
        .unwrap()
    });
    static CLASS_BASE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\b[^{]*ReactContextBaseJavaModule").unwrap()
    });
    static CLASS_PKG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\b[^{]*ReactPackage").unwrap()
    });
    static METHOD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"@ReactMethod\b[^{]*?(?:\bfun\s+|\bvoid\s+|\bpublic\s+[A-Za-z0-9_][A-Za-z0-9_<>\[\]]*\s+)([A-Za-z_][A-Za-z0-9_]*)\s*\(",
        )
        .unwrap()
    });

    let mut results: Vec<JvmExport> = Vec::new();

    // getName() literal — Java + Kotlin both look something like:
    //   public String getName() { return "Geolocation"; }
    //   fun getName(): String = "Geolocation"
    //   fun getName() = "Geolocation"
    let get_name = GET_NAME_RE.captures(source);
    // Class name fallback.
    let class_match = CLASS_BASE_RE
        .captures(source)
        .or_else(|| CLASS_PKG_RE.captures(source));
    let module_name = get_name.map(|c| c[1].to_string()).or_else(|| {
        class_match.map(|c| {
            let name = &c[1];
            name.strip_suffix("Module").unwrap_or(name).to_string()
        })
    });
    let Some(module_name) = module_name else {
        return results;
    };

    // @ReactMethod annotations — followed (after optional modifiers / args /
    // newlines) by either `void <name>(` (Java) or `fun <name>(` (Kotlin).
    for m in METHOD_RE.captures_iter(source) {
        results.push(JvmExport {
            module_name: module_name.clone(),
            js_name: m[1].to_string(),
        });
    }

    results
}

struct TurboModuleSpec {
    module_name: String,
    methods: Vec<String>,
}

/// Parse a TS file for a TurboModule spec declaration. The spec file is
/// the JS↔native source-of-truth in the new architecture — its interface
/// lists every JS-visible method, and a `TurboModuleRegistry.get*<Spec>(...)`
/// default export pins the module name.
///
/// Returns `None` when the file isn't a TurboModule spec.
fn parse_turbo_module_spec(source: &str) -> Option<TurboModuleSpec> {
    static REG_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"TurboModuleRegistry\.(?:getEnforcing|get)\s*<[^>]*>\s*\(\s*['"]([^'"]+)['"]\s*\)"#,
        )
        .unwrap()
    });
    static IFACE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)export\s+interface\s+Spec\b[^\{]*\{(.*?)\n\}").unwrap());
    static METHOD_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap());

    // `TurboModuleRegistry.getEnforcing<Spec>('ModuleName')` or
    // `TurboModuleRegistry.get<Spec>('ModuleName')`. The literal must be a
    // single-or-double-quoted string.
    let reg_match = REG_RE.captures(source)?;
    let module_name = reg_match[1].to_string();

    // Find `export interface Spec extends TurboModule { … }` and pull each
    // method declaration's name. We don't need types — just names.
    let iface_match = IFACE_RE.captures(source)?;
    let body = iface_match.get(1)?.as_str();

    // Method shape: `name(args): ReturnType;` or `name(): void;`. Skip
    // properties (no parens before colon).
    let methods: Vec<String> = METHOD_RE
        .captures_iter(body)
        .map(|m| m[1].to_string())
        .collect();
    Some(TurboModuleSpec {
        module_name,
        methods,
    })
}

// ─── Map building ───────────────────────────────────────────────────────────

/// RCTEventEmitter built-ins that every emitter subclass inherits. JS code
/// doesn't directly call these — they're internal plumbing for the
/// `NativeEventEmitter` abstraction. If we leave them in the bridge map,
/// every JS `addListener` / `remove` call (Firestore subscribers, RxJS
/// pipelines, plain Array.remove, etc.) gets mis-bridged to whichever
/// emitter happens to define them. Skip during map building.
static RN_EMITTER_BUILTINS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "addListener",
        "removeListeners",
        "remove",
        "invalidate",
        "startObserving",
        "stopObserving",
    ]
    .into_iter()
    .collect()
});

fn build_rn_maps(context: &dyn ResolutionContext) -> RnMaps {
    let mut by_js_name: HashMap<String, Vec<NativeMethod>> = HashMap::new();
    let all_files = context.get_all_files();
    // Pre-index native methods by name for fast lookup when matching to
    // their bridge exports.
    let mut objc_methods_by_first_kw: HashMap<String, Vec<Node>> = HashMap::new();
    let mut jvm_methods_by_name: HashMap<String, Vec<Node>> = HashMap::new();
    for node in context.get_nodes_by_kind(NodeKind::Method) {
        if node.language == Language::Objc {
            let first_kw = if node.name.contains(':') {
                node.name.split(':').next().unwrap_or("")
            } else {
                node.name.as_str()
            };
            if !first_kw.is_empty() {
                objc_methods_by_first_kw
                    .entry(first_kw.to_string())
                    .or_default()
                    .push(node);
            }
        } else if node.language == Language::Java || node.language == Language::Kotlin {
            jvm_methods_by_name
                .entry(node.name.clone())
                .or_default()
                .push(node);
        }
    }

    for file in &all_files {
        // Legacy bridge — ObjC side.
        if file.ends_with(".m") || file.ends_with(".mm") {
            let Some(source) = context.read_file(file) else {
                continue;
            };
            let class_name = find_objc_class_name(&source);
            let exports = parse_objc_rn_exports(&source, class_name.as_deref());
            for exp in exports {
                if RN_EMITTER_BUILTINS.contains(exp.js_name.as_str()) {
                    continue;
                }
                // Resolve to the native node by selector first-keyword. Multiple
                // ObjC methods may share a first keyword across modules; filter by
                // file path to attribute the export to this module's
                // implementation file.
                let empty: Vec<Node> = Vec::new();
                let candidates = objc_methods_by_first_kw
                    .get(&exp.native_selector_first_kw)
                    .unwrap_or(&empty);
                let node = candidates
                    .iter()
                    .find(|c| &c.file_path == file)
                    .or_else(|| candidates.first());
                let Some(node) = node else {
                    continue;
                };
                by_js_name
                    .entry(exp.js_name.clone())
                    .or_default()
                    .push(NativeMethod {
                        module_name: exp.module_name,
                        js_name: exp.js_name,
                        node: node.clone(),
                    });
            }
        }

        // Legacy bridge — Java/Kotlin side.
        if file.ends_with(".java") || file.ends_with(".kt") {
            let Some(source) = context.read_file(file) else {
                continue;
            };
            let exports = parse_jvm_rn_exports(&source);
            for exp in exports {
                if RN_EMITTER_BUILTINS.contains(exp.js_name.as_str()) {
                    continue;
                }
                let empty: Vec<Node> = Vec::new();
                let candidates = jvm_methods_by_name.get(&exp.js_name).unwrap_or(&empty);
                let node = candidates
                    .iter()
                    .find(|c| &c.file_path == file)
                    .or_else(|| candidates.first());
                let Some(node) = node else {
                    continue;
                };
                by_js_name
                    .entry(exp.js_name.clone())
                    .or_default()
                    .push(NativeMethod {
                        module_name: exp.module_name,
                        js_name: exp.js_name,
                        node: node.clone(),
                    });
            }
        }

        // TurboModule spec — TS side.
        if file.ends_with(".ts") || file.ends_with(".tsx") {
            let Some(source) = context.read_file(file) else {
                continue;
            };
            let Some(spec) = parse_turbo_module_spec(&source) else {
                continue;
            };
            // For each spec method, find a matching native implementation by
            // name. The spec's module name doesn't determine the native file
            // path (Codegen wires it via name convention), so we match across
            // all native methods of the right name.
            for method_name in &spec.methods {
                if RN_EMITTER_BUILTINS.contains(method_name.as_str()) {
                    continue;
                }
                // ObjC first-keyword match, then JVM bare-name match. Don't
                // require module-name match for ObjC because the native side may
                // have stripped a prefix.
                let empty: Vec<Node> = Vec::new();
                let objc_cands = objc_methods_by_first_kw.get(method_name).unwrap_or(&empty);
                let jvm_cands = jvm_methods_by_name.get(method_name).unwrap_or(&empty);
                for node in objc_cands.iter().chain(jvm_cands.iter()) {
                    by_js_name
                        .entry(method_name.clone())
                        .or_default()
                        .push(NativeMethod {
                            module_name: spec.module_name.clone(),
                            js_name: method_name.clone(),
                            node: node.clone(),
                        });
                }
            }
        }
    }

    RnMaps { by_js_name }
}

// ─── Resolver ───────────────────────────────────────────────────────────────

/// `reactNativeBridgeResolver` — struct implementing [`FrameworkResolver`]
/// with a per-instance lazy map cache (the TS per-context `WeakMap`).
#[derive(Default)]
pub struct ReactNativeBridgeResolver {
    cache: Mutex<Option<RnMaps>>,
}

impl ReactNativeBridgeResolver {
    pub fn new() -> Self {
        Self::default()
    }
}

const RN_LANGUAGES: [Language; 4] = [
    Language::Javascript,
    Language::Typescript,
    Language::Tsx,
    Language::Jsx,
];

impl FrameworkResolver for ReactNativeBridgeResolver {
    fn name(&self) -> &str {
        "react-native-bridge"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&RN_LANGUAGES)
    }

    /// Detect: package.json depends on `react-native`, OR any source file
    /// uses the `RCT_EXPORT_MODULE` / `RCT_EXPORT_METHOD` /
    /// `TurboModuleRegistry` markers. Either signal is enough — different
    /// libraries split the JS package from the native code (`react-native-svg`'s
    /// apple/ + android/ directories vs its src/), so we don't require both.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        static PKG_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"["']react-native["']\s*:"#).unwrap());
        static OBJC_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"RCT_EXPORT_MODULE\b").unwrap());
        static TURBO_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"TurboModuleRegistry\.(?:get|getEnforcing)\s*<").unwrap());

        if let Some(pkg) = context.read_file("package.json") {
            if PKG_RE.is_match(&pkg) {
                return true;
            }
        }
        // Fallback: scan a small number of files for the macro markers — only
        // looking at the first ones returned by get_all_files to keep detect()
        // fast on huge repos.
        let files = context.get_all_files();
        for f in files.iter().take(200) {
            if f.ends_with(".mm") || f.ends_with(".m") {
                if let Some(src) = context.read_file(f) {
                    if OBJC_RE.is_match(&src) {
                        return true;
                    }
                }
            }
            if f.ends_with(".ts") || f.ends_with(".tsx") {
                if let Some(src) = context.read_file(f) {
                    if TURBO_RE.is_match(&src) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn claims_reference(&self, _name: &str) -> bool {
        // JS-visible method names are ordinary identifiers and are typically
        // already in `knownNames` (every TurboModule spec method, every
        // RCT_EXPORT_METHOD, has a node somewhere). So we don't need to
        // claim through the pre-filter — the ref reaches us via the normal
        // hasAnyPossibleMatch path.
        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // We only redirect JS callers — native callers don't need this resolver.
        if reference.language != Language::Javascript
            && reference.language != Language::Typescript
            && reference.language != Language::Tsx
            && reference.language != Language::Jsx
        {
            return None;
        }

        // JS callsites of `obj.method()` reach the resolver as either
        // `obj.method` (qualified) or `method` (bare). Strip a single dot
        // prefix to get the JS-visible method name.
        let name = match reference.reference_name.rfind('.') {
            Some(idx) => &reference.reference_name[idx + 1..],
            None => reference.reference_name.as_str(),
        };

        let mut cache = self.cache.lock().unwrap();
        if cache.is_none() {
            *cache = Some(build_rn_maps(context));
        }
        let maps = cache.as_ref().unwrap();
        let entries = maps.by_js_name.get(name)?;
        if entries.is_empty() {
            return None;
        }

        // Prefer the iOS (ObjC) target over Android when both exist — iOS is
        // the conventional first-class platform for RN library docs and most
        // graph queries. We still record only one edge; a JVM-only resolution
        // is fine when no ObjC target exists.
        let objc = entries.iter().find(|e| e.node.language == Language::Objc);
        let target = objc.or_else(|| entries.first())?;
        Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.node.id.clone(),
            confidence: 0.6,
            resolved_by: ResolvedBy::Framework,
        })
    }
}
