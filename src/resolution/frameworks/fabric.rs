//! React Native Fabric / Codegen view components — Phase 6 of the
//! mixed-iOS/RN bridging effort.
//!
//! In the new RN architecture, JS-visible view components are declared via
//! Codegen TS spec files of the shape:
//!
//! ```text
//!   // src/fabric/MyComponentNativeComponent.ts
//!   import { codegenNativeComponent } from 'react-native';
//!   import type { ViewProps, CodegenTypes as CT } from 'react-native';
//!
//!   export interface NativeProps extends ViewProps {
//!     color?: ColorValue;
//!     onTap?: CT.DirectEventHandler<TapEvent>;
//!   }
//!
//!   export default codegenNativeComponent<NativeProps>('MyComponent');
//! ```
//!
//! Codegen then generates a native ComponentDescriptor that wires the JS
//! component name to a native implementation class — by RN convention,
//! one of `MyComponent`, `MyComponentView`, `MyComponentComponentView`,
//! `MyComponentManager`, `MyComponentViewManager`. The actual implementation
//! lives in ObjC++ (.mm) on iOS or Kotlin/Java on Android.
//!
//! Without bridging, JSX `<MyComponent color="red"/>` in a consumer app has
//! nothing in the graph to land on — the JS-visible name `MyComponent` isn't
//! a node anywhere (only `MyComponentView` is, in the .mm), and the JSX
//! synthesizer matches strictly by name.
//!
//! What this extractor does:
//!   1. Parse the spec file's `codegenNativeComponent<Props>('Name', ...)`
//!      literal — emit a `component` node named `Name`, attributed to the
//!      spec file.
//!   2. Parse the `NativeProps` interface and emit one `property` node per
//!      prop, attributed to the spec file. Props like `onTap` /
//!      `onFinishTransitioning` are JS-callable event-handler bindings;
//!      surfacing them as nodes lets the agent discover the JS surface of
//!      the component.
//!
//! A companion synthesizer (`fabricNativeImplEdges` in
//! callback-synthesizer.ts) links the emitted component node to its
//! native implementation class via the convention-based name+suffix
//! lookup — that produces the cross-language hop the JSX synthesizer's
//! `<MyComponent>` edges naturally chain through.
//!
//! Ported from `src/resolution/frameworks/fabric.ts`.

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

/// 1-based line number of the byte offset `index`.
fn line_at(s: &str, index: usize) -> u32 {
    (s[..index].matches('\n').count() + 1) as u32
}

/// 0-based column (bytes since the last newline) of the byte offset `index`.
fn column_at(s: &str, index: usize) -> u32 {
    let before = &s[..index];
    match before.rfind('\n') {
        Some(i) => (before.len() - i - 1) as u32,
        None => before.len() as u32,
    }
}

static CODEGEN_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"codegenNativeComponent\s*(?:<[^>]+>)?\s*\(\s*['"]([A-Za-z_][A-Za-z0-9_]*)['"]"#)
        .unwrap()
});

/// Legacy Paper view manager macros — older RN libs (still very common,
/// especially small libs that haven't migrated to Codegen) declare a
/// ViewManager class and expose props via these macros. Both shapes:
///
/// ```text
///   RCT_EXPORT_VIEW_PROPERTY(values, NSArray)
///   RCT_EXPORT_VIEW_PROPERTY(onChange, RCTBubblingEventBlock)
///   RCT_CUSTOM_VIEW_PROPERTY(text, NSString, RNCMyView) { … }
///   RCT_REMAP_VIEW_PROPERTY(jsName, nativeKeyPath, NSString)
/// ```
///
/// Capture the FIRST argument — that's the JS-visible prop name.
static RCT_VIEW_PROP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bRCT_(?:EXPORT|CUSTOM|REMAP)_VIEW_PROPERTY\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)")
        .unwrap()
});

/// ObjC `@implementation Foo` extraction. Used to identify the ViewManager
/// class so we can derive a JS-visible component name (strip the `Manager`
/// suffix and a leading `RCT` prefix, both standard conventions).
static OBJC_IMPL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@implementation\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap());

/// Derive the JS-visible component name from a native ViewManager class.
/// Strip a trailing `Manager` (and optionally `ViewManager`) — RN's view
/// registry maps `XXXManager` ↔ JS `<XXX/>` by this convention. The
/// leading `RCT` prefix is also stripped (matches what
/// `defaultObjcModuleName` does for RN's legacy bridge modules).
fn derive_component_name_from_manager(class_name: &str) -> String {
    let name = class_name.strip_prefix("RCT").unwrap_or(class_name);
    // Trim ViewManager > Manager > View, in order.
    if let Some(stripped) = name.strip_suffix("ViewManager") {
        stripped.to_string()
    } else if let Some(stripped) = name.strip_suffix("Manager") {
        stripped.to_string()
    } else {
        name.to_string()
    }
}

/// Cheap source-level detector — must contain `codegenNativeComponent` to
/// be worth parsing. The presence of that import is the canonical Fabric
/// spec signal.
fn is_fabric_spec(source: &str) -> bool {
    source.contains("codegenNativeComponent")
}

/// Pull the `NativeProps` interface body out of a Fabric spec source.
/// Returns `None` when the interface isn't declared in the expected shape.
fn find_native_props_body(source: &str) -> Option<regex::Match<'_>> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        // Permissive: `export interface NativeProps [extends X, Y] { … }`.
        Regex::new(r"(?s)export\s+interface\s+NativeProps\b[^\{]*\{(.*?)\n\}").unwrap()
    });
    RE.captures(source).and_then(|c| c.get(1))
}

/// Parse the NativeProps interface body and return prop names.
/// Each prop is `name?: Type;` or `name: Type;` on its own line.
/// We don't care about types — just the JS-visible name.
fn extract_prop_names(body: &str) -> Vec<String> {
    static PROP_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\??\s*:").unwrap());
    static FN_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\(").unwrap());
    let mut props: Vec<String> = Vec::new();
    // Anchor to start-of-line (after optional whitespace), then capture an
    // identifier, then optional `?`, then `:`. Skip lines that look like
    // method declarations (`name(`) — those are TurboModule spec methods,
    // not view props.
    for m in PROP_RE.captures_iter(body) {
        let whole = m.get(0).unwrap();
        let name = &m[1];
        // Exclude any line that immediately turns into a function-shape (e.g.
        // `onTap?: () => void` is fine — it's a prop, not a method body —
        // but a literal `name(arg: T): R` is a method declaration).
        let mut end = (whole.end() + 80).min(body.len());
        while end > whole.end() && !body.is_char_boundary(end) {
            end -= 1;
        }
        let after = &body[whole.end()..end];
        if FN_SHAPE_RE.is_match(after) {
            continue; // method-shape, skip
        }
        props.push(name.to_string());
    }
    props
}

/// Extract legacy Paper view-manager declarations from a .m/.mm file.
/// Emits a `component` node named after the JS-visible name (derived from
/// the @implementation class) plus a `property` node per
/// `RCT_EXPORT_VIEW_PROPERTY(name, ...)` macro.
///
/// Returns `[]` if the file doesn't look like a ViewManager (no
/// RCT_EXPORT_VIEW_PROPERTY macros).
fn extract_legacy_view_manager_nodes(file_path: &str, source: &str) -> Vec<Node> {
    // Cheap gate: no view-property macros at all → not a view manager.
    if !source.contains("RCT_EXPORT_VIEW_PROPERTY")
        && !source.contains("RCT_CUSTOM_VIEW_PROPERTY")
        && !source.contains("RCT_REMAP_VIEW_PROPERTY")
    {
        return Vec::new();
    }
    let Some(impl_match) = OBJC_IMPL_RE.captures(source) else {
        return Vec::new();
    };
    let class_name = &impl_match[1];
    // Only process actual ViewManagers — classes ending in Manager or
    // (legacy) ViewManager. Classes with view-property macros that don't
    // follow the naming convention are unusual; skip to keep precision.
    if !class_name.ends_with("Manager") && !class_name.ends_with("ViewManager") {
        return Vec::new();
    }
    let component_name = derive_component_name_from_manager(class_name);
    if component_name.is_empty() {
        return Vec::new();
    }

    let now = now_ms();
    let mut nodes: Vec<Node> = Vec::new();

    // Component node — same shape as Codegen Fabric's, so the
    // fabricNativeImplEdges synthesizer linking component → native class
    // works for legacy too. The native class IS the manager itself in this
    // case; the convention-based suffix lookup in the synthesizer
    // (`Manager`, `ViewManager`) will find it.
    let start_line = line_at(source, impl_match.get(0).unwrap().start());
    let mut component = Node::new(
        format!("fabric-component:{file_path}:{component_name}:{start_line}"),
        NodeKind::Component,
        &component_name,
        format!("{file_path}::{component_name}"),
        file_path,
        Language::Objc,
        start_line,
        start_line,
    );
    component.end_column = component_name.len() as u32;
    component.docstring = Some(format!(
        "Legacy Paper ViewManager component '{component_name}' (from @implementation {class_name})"
    ));
    component.signature = Some(format!("RCT_EXPORT_MODULE() // ViewManager: {class_name}"));
    component.is_exported = Some(true);
    component.updated_at = now;
    nodes.push(component);

    // Property nodes per RCT_EXPORT_VIEW_PROPERTY macro.
    let mut seen: HashSet<String> = HashSet::new();
    for m in RCT_VIEW_PROP_RE.captures_iter(source) {
        let prop_name = &m[1];
        if seen.contains(prop_name) {
            continue;
        }
        seen.insert(prop_name.to_string());
        let prop_line = line_at(source, m.get(0).unwrap().start());
        let mut prop = Node::new(
            format!("fabric-prop:{file_path}:{prop_name}:{prop_line}"),
            NodeKind::Property,
            prop_name,
            format!("{file_path}::{component_name}.{prop_name}"),
            file_path,
            Language::Objc,
            prop_line,
            prop_line,
        );
        prop.end_column = prop_name.len() as u32;
        prop.docstring = Some(format!(
            "Legacy Paper view prop '{prop_name}' on {component_name}"
        ));
        prop.is_exported = Some(true);
        prop.updated_at = now;
        nodes.push(prop);
    }
    nodes
}

/// Java/Kotlin `@ReactProp("name")` extraction. The annotation precedes a
/// setter method on a class that extends `ViewManager` /
/// `SimpleViewManager` (or in Kotlin, `:` syntax).
///
/// Returns `[]` if no @ReactProp annotations are found.
fn extract_jvm_view_manager_nodes(file_path: &str, source: &str) -> Vec<Node> {
    static CLASS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\b").unwrap());
    // @ReactProp("name") followed (after optional modifiers / args) by a
    // setter declaration. The annotation argument is the JS-visible prop
    // name. Permissive about the rest — we only need the literal.
    static REACT_PROP_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"@ReactProp\s*\(\s*(?:name\s*=\s*)?"([^"]+)""#).unwrap());

    if !source.contains("@ReactProp") {
        return Vec::new();
    }

    // Class name — looking for `class FooManager [extends ViewManager...]`
    // (Java) or `class FooManager : ViewManager...` (Kotlin). Either gates
    // us into a ViewManager file; non-Manager classes with @ReactProp are
    // unusual.
    let Some(class_match) = CLASS_RE.captures(source) else {
        return Vec::new();
    };
    let class_name = &class_match[1];
    if !class_name.ends_with("Manager") && !class_name.ends_with("ViewManager") {
        return Vec::new();
    }
    let component_name = derive_component_name_from_manager(class_name);
    if component_name.is_empty() {
        return Vec::new();
    }

    let language = if file_path.ends_with(".kt") {
        Language::Kotlin
    } else {
        Language::Java
    };
    let now = now_ms();
    let mut nodes: Vec<Node> = Vec::new();

    let start_line = line_at(source, class_match.get(0).unwrap().start());
    let mut component = Node::new(
        format!("fabric-component:{file_path}:{component_name}:{start_line}"),
        NodeKind::Component,
        &component_name,
        format!("{file_path}::{component_name}"),
        file_path,
        language,
        start_line,
        start_line,
    );
    component.end_column = component_name.len() as u32;
    component.docstring = Some(format!(
        "Android view-manager component '{component_name}' (from class {class_name})"
    ));
    component.signature = Some(format!("class {class_name} : ViewManager"));
    component.is_exported = Some(true);
    component.updated_at = now;
    nodes.push(component);

    let mut seen: HashSet<String> = HashSet::new();
    for m in REACT_PROP_RE.captures_iter(source) {
        let prop_name = &m[1];
        if seen.contains(prop_name) {
            continue;
        }
        seen.insert(prop_name.to_string());
        let prop_line = line_at(source, m.get(0).unwrap().start());
        let mut prop = Node::new(
            format!("fabric-prop:{file_path}:{prop_name}:{prop_line}"),
            NodeKind::Property,
            prop_name,
            format!("{file_path}::{component_name}.{prop_name}"),
            file_path,
            language,
            prop_line,
            prop_line,
        );
        prop.end_column = prop_name.len() as u32;
        prop.docstring = Some(format!(
            "Android @ReactProp prop '{prop_name}' on {component_name}"
        ));
        prop.is_exported = Some(true);
        prop.updated_at = now;
        nodes.push(prop);
    }
    nodes
}

fn extract_fabric_nodes(file_path: &str, source: &str) -> Vec<Node> {
    if !is_fabric_spec(source) {
        return Vec::new();
    }

    let now = now_ms();
    let mut nodes: Vec<Node> = Vec::new();
    let spec_lang = if file_path.ends_with(".tsx") {
        Language::Tsx
    } else {
        Language::Typescript
    };

    for m in CODEGEN_DECL_RE.captures_iter(source) {
        let whole = m.get(0).unwrap();
        let component_name = &m[1];
        let start_line = line_at(source, whole.start());
        let start_column = column_at(source, whole.start());

        // The component itself — kind: 'component' so the existing
        // reactJsxChildEdges synthesizer matches `<MyComponent>` JSX tags to
        // it (its name+kind filter is the gate).
        let mut component = Node::new(
            format!("fabric-component:{file_path}:{component_name}:{start_line}"),
            NodeKind::Component,
            component_name,
            format!("{file_path}::{component_name}"),
            file_path,
            // The spec file is .ts or .tsx; use the file's apparent language
            // by extension. Trim to a known Language value.
            spec_lang,
            start_line,
            start_line,
        );
        component.start_column = start_column;
        component.end_column = start_column + "codegenNativeComponent".len() as u32;
        component.docstring = Some(format!(
            "Fabric/Codegen native component '{component_name}'"
        ));
        component.signature = Some(format!(
            "codegenNativeComponent<NativeProps>('{component_name}')"
        ));
        component.is_exported = Some(true);
        component.updated_at = now;
        nodes.push(component);
    }

    // Props from the NativeProps interface. These are not "method" semantic
    // — they're JS-visible bindings the consumer sets via JSX attributes —
    // so use `property` kind. (The JSX synthesizer doesn't currently
    // produce per-attribute edges, but surfacing the prop names as nodes
    // lets `codegraph_search('onFinishTransitioning')` discover them.)
    if let Some(body_match) = find_native_props_body(source) {
        let body = body_match.as_str();
        let props = extract_prop_names(body);
        // TS: `source.indexOf(body)` (first occurrence of the body text).
        let body_index = source.find(body).unwrap_or(0);
        for prop_name in props {
            let prop_before = source[body_index..]
                .find(&prop_name)
                .map(|i| i + body_index);
            let prop_line = match prop_before {
                Some(i) => line_at(source, i),
                None => 1,
            };
            let mut prop = Node::new(
                format!("fabric-prop:{file_path}:{prop_name}:{prop_line}"),
                NodeKind::Property,
                &prop_name,
                format!("{file_path}::NativeProps.{prop_name}"),
                file_path,
                spec_lang,
                prop_line,
                prop_line,
            );
            prop.end_column = prop_name.len() as u32;
            prop.docstring = Some(format!("Fabric NativeProps prop '{prop_name}'"));
            prop.is_exported = Some(true);
            prop.updated_at = now;
            nodes.push(prop);
        }
    }

    nodes
}

/// `fabricViewResolver` — unit struct implementing [`FrameworkResolver`].
pub struct FabricViewResolver;

const FABRIC_LANGUAGES: [Language; 5] = [
    Language::Typescript,
    Language::Tsx,
    Language::Objc,
    Language::Java,
    Language::Kotlin,
];

impl FrameworkResolver for FabricViewResolver {
    fn name(&self) -> &str {
        "fabric-view"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&FABRIC_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        static PKG_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"["']react-native["']\s*:"#).unwrap());
        // Root package.json is the common case. The indexer only tracks
        // SOURCE files in get_all_files(), so package.jsons in subpackages
        // aren't enumerable that way — we have to probe them explicitly via
        // list_directories() for monorepos.
        let check_pkg = |relative_path: &str| -> bool {
            context
                .read_file(relative_path)
                .map(|pkg| PKG_RE.is_match(&pkg))
                .unwrap_or(false)
        };
        if check_pkg("package.json") {
            return true;
        }
        // Monorepo escape hatch — react-native-skia and similar workspace
        // repos have the RN dep only in `packages/<sub>/package.json`. Walk
        // the common workspace roots one level deep.
        for root in ["packages", "apps", "modules", "libraries"] {
            for sub in context.list_directories(root) {
                if check_pkg(&format!("{root}/{sub}/package.json")) {
                    return true;
                }
            }
        }
        false
    }

    fn extract(&self, file_path: &str, source: &str) -> Option<FrameworkExtractionResult> {
        // Pick the right extractor by file language. The framework registry
        // already filters by `languages` so we only see relevant files.
        let nodes: Vec<Node> = if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
            extract_fabric_nodes(file_path, source)
        } else if file_path.ends_with(".m") || file_path.ends_with(".mm") {
            extract_legacy_view_manager_nodes(file_path, source)
        } else if file_path.ends_with(".java") || file_path.ends_with(".kt") {
            extract_jvm_view_manager_nodes(file_path, source)
        } else {
            Vec::new()
        };
        Some(FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }

    fn resolve(
        &self,
        _reference: &UnresolvedRef,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // The companion synthesizer (`fabricNativeImplEdges`) handles
        // cross-language edges; standard name resolution handles
        // <MyComponent> → component-node via the JSX synthesizer.
        None
    }
}
