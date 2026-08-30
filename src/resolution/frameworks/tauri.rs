//! Tauri IPC cross-language bridge resolver.
//!
//! Joins TypeScript command/event callsites to their Rust handlers:
//!
//! **Commands** (TS call -> Rust fn):
//!   - Typed (tauri-specta): `commands.fooBar(...)` where `fooBar` is the
//!     camelCase form of a `#[tauri::command] fn foo_bar`.
//!   - Raw: `invoke('foo_bar', ...)` using the exact snake_case wire name.
//!
//! **Events** (TS listen -> Rust emit):
//!   - Typed: `events.fooBar.listen(...)` or wrapper functions.
//!   - Raw: `listen('foo-bar', ...)` using the kebab-case wire name.
//!
//! The resolver only redirects JS/TS callers to Rust targets; Rust-side
//! references resolve through the normal Rust extractor.
//!
//! The wire join is a heuristic name inference (the real link is a runtime IPC
//! hop, not an AST edge), so it runs through [`resolve`](FrameworkResolver::resolve)
//! like the React Native / Fabric native bridges: the edge carries
//! `metadata.resolvedBy: 'framework'` with a sub-1.0 confidence, and an exact
//! name-match always wins a tie. (The `provenance: 'heuristic'` column is
//! reserved for the callback synthesizer, which fabricates edges for dynamic
//! dispatch that has no statically-resolvable name; a Tauri command/event name
//! IS statically joinable, so it belongs on the resolver path.)
//!
//! Ported from `src/resolution/frameworks/tauri.ts` (upstream #878, issue #1543).
//!
//! Rust-port deviation: the TS file cached the built index in a per-context
//! `WeakMap`; here the cache lives on the resolver instance
//! (`Mutex<Option<TauriIndex>>`). Construct a fresh [`TauriBridgeResolver`] per
//! resolution run/context (the TS registry effectively did the same -- one
//! context per resolver lifetime).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, Node, NodeKind};

// -- Name conversion utilities ------------------------------------------------

/// `snake_case` -> `camelCase` (e.g. `get_mcp_port` -> `getMcpPort`).
fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for c in s.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `kebab-case` -> `snake_case` (e.g. `volume-space-changed` -> `volume_space_changed`).
fn kebab_to_snake(s: &str) -> String {
    s.replace('-', "_")
}

/// `PascalCase` -> `snake_case` (e.g. `VolumeSpaceChanged` -> `volume_space_changed`).
fn pascal_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// -- Rust-side index ----------------------------------------------------------

/// One Tauri command or event known to the resolver, indexed by JS-visible name.
#[derive(Debug, Clone)]
struct TauriTarget {
    /// The graph node for the Rust fn (command) or struct/enum (event).
    node: Node,
}

#[derive(Debug, Default)]
struct TauriIndex {
    /// `#[tauri::command]` fns keyed by camelCase and snake_case names.
    commands: HashMap<String, TauriTarget>,
    /// `#[derive(...Event)]` structs keyed by camel/kebab/Pascal/snake names.
    events: HashMap<String, TauriTarget>,
}

/// `#[tauri::command]` (in any attribute order) followed by `fn name`.
static CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:#\[[^\]]*\]\s*)*#\[tauri::command[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)",
    )
    .expect("valid regex")
});

/// `#[derive(...Event...)]` ... `struct Name` (bare/qualified `Event` derive).
static EVENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"#\[derive\([^\)]*\b(?:tauri_specta::|tauri::)?Event\b[^\)]*\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?struct\s+(\w+)",
    )
    .expect("valid regex")
});

/// `#[tauri_specta(event_name = "...")]` override.
static EVENT_NAME_OVERRIDE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"#\[tauri_specta\s*\(\s*event_name\s*=\s*"([^"]+)"\s*\)\]"#).expect("valid regex")
});

/// Scan all Rust files for `#[tauri::command]` functions and event structs,
/// building lookup maps keyed by the JS-visible name.
fn build_index(context: &dyn ResolutionContext) -> TauriIndex {
    let mut commands: HashMap<String, TauriTarget> = HashMap::new();
    let mut events: HashMap<String, TauriTarget> = HashMap::new();

    for file in context.get_all_files() {
        if !file.ends_with(".rs") {
            continue;
        }
        let Some(source) = context.read_file(&file) else {
            continue;
        };

        // Commands: `#[tauri::command]` (possibly preceded/followed by other
        // attributes) on a `fn`.
        if source.contains("tauri::command") {
            let nodes = context.get_nodes_in_file(&file);
            for caps in CMD_RE.captures_iter(&source) {
                let rust_name = caps.get(1).expect("group 1").as_str();
                let camel_name = snake_to_camel(rust_name);
                let Some(node) = nodes.iter().find(|n| {
                    n.name == rust_name
                        && (n.kind == NodeKind::Function || n.kind == NodeKind::Method)
                }) else {
                    continue;
                };
                let entry = TauriTarget { node: node.clone() };
                commands.insert(camel_name, entry.clone());
                commands.insert(rust_name.to_string(), entry);
            }
        }

        // Events: struct deriving `Event` (tauri_specta::Event, tauri::Event,
        // or bare `Event` when `use tauri_specta::Event` is in scope).
        if source.contains("Event") {
            let nodes = context.get_nodes_in_file(&file);
            for caps in EVENT_RE.captures_iter(&source) {
                let whole = caps.get(0).expect("match");
                let struct_name = caps.get(1).expect("group 1").as_str();

                // `#[tauri_specta(event_name = "...")]` override between the
                // derive and the `struct` keyword.
                let start = whole.start().saturating_sub(200);
                let block_before = &source[start..whole.end()];
                let override_name = EVENT_NAME_OVERRIDE_RE
                    .captures(block_before)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string());

                let snake_name = pascal_to_snake(struct_name);
                let camel_name = snake_to_camel(&snake_name);
                let kebab_name = override_name.unwrap_or_else(|| snake_name.replace('_', "-"));

                let Some(node) = nodes.iter().find(|n| {
                    n.name == struct_name
                        && (n.kind == NodeKind::Struct || n.kind == NodeKind::Enum)
                }) else {
                    continue;
                };
                let entry = TauriTarget { node: node.clone() };
                events.insert(camel_name, entry.clone());
                events.insert(kebab_name, entry.clone());
                events.insert(struct_name.to_string(), entry.clone());
                events.insert(snake_name, entry);
            }
        }
    }

    TauriIndex { commands, events }
}

// -- Extraction (raw invoke/listen wire-name references) ----------------------

/// JS-family language for a file path, or `None` when not a JS/TS source.
fn js_language_for_file(file_path: &str) -> Option<Language> {
    let dot = file_path.rfind('.')?;
    let ext = file_path[dot..].to_lowercase();
    let lang = match ext.as_str() {
        ".ts" | ".mts" | ".cts" => Language::Typescript,
        ".tsx" => Language::Tsx,
        ".js" | ".mjs" | ".cjs" => Language::Javascript,
        ".jsx" => Language::Jsx,
        ".svelte" => Language::Svelte,
        ".vue" => Language::Typescript,
        _ => return None,
    };
    Some(lang)
}

/// `invoke('foo_bar')` / `listen('foo-bar')` / `once('foo-bar')` with a
/// string-literal first argument (optional generics). A dynamic name
/// (`invoke(...template...)`) fails the literal match and is skipped.
static WIRE_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    // The Rust `regex` crate has no backreferences, so we can't require the
    // closing quote to match the opening one. Wire names never contain quote
    // characters, so matching any of the three quote styles on each side is
    // exact enough in practice.
    Regex::new(r#"\b(?:invoke|listen|once)\s*(?:<[^>(]*>)?\s*\(\s*['"`]([\w./:-]+)['"`]"#)
        .expect("valid regex")
});

/// Raw Tauri IPC uses string wire names the JS extractor never emits as
/// references. Surface each wire name as a `calls` reference attributed to the
/// file node (`file:<path>`, a stable un-hashed id) so [`resolve`] can join it
/// to the Rust handler / Event struct.
///
/// File granularity still answers "which files use this command" for
/// `callers`/`impact` -- coarser than the typed path's call-site edge, but
/// accurate and robust (a fn-node id is a hash of `path:kind:name:line` a
/// string scan cannot rebuild without re-parsing).
///
/// [`resolve`]: FrameworkResolver::resolve
fn extract_wire_references(file_path: &str, source: &str) -> Vec<UnresolvedRef> {
    let Some(language) = js_language_for_file(file_path) else {
        return Vec::new();
    };

    let mut refs = Vec::new();
    let from_node_id = format!("file:{file_path}");
    for caps in WIRE_CALL_RE.captures_iter(source) {
        let whole = caps.get(0).expect("match");
        let wire_name = caps.get(1).expect("group 1").as_str();

        // Line/column of the wire-call, for the edge's location.
        let upto = &source[..whole.start()];
        let line = upto.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        let last_nl = upto.rfind('\n').map(|i| i as isize).unwrap_or(-1);
        let column = (whole.start() as isize - last_nl - 1).max(0) as u32;

        refs.push(UnresolvedRef {
            from_node_id: from_node_id.clone(),
            reference_name: wire_name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            column,
            file_path: file_path.to_string(),
            language,
            candidates: None,
            metadata: None,
        });
    }
    refs
}

// -- Resolver -----------------------------------------------------------------

const TS_LANGUAGES: [Language; 5] = [
    Language::Javascript,
    Language::Typescript,
    Language::Tsx,
    Language::Jsx,
    Language::Svelte,
];

/// `tauriBridgeResolver` -- struct implementing [`FrameworkResolver`] with a
/// per-instance lazy index cache (the TS per-context `WeakMap`).
#[derive(Default)]
pub struct TauriBridgeResolver {
    cache: Mutex<Option<TauriIndex>>,
}

impl TauriBridgeResolver {
    pub fn new() -> Self {
        Self::default()
    }
}

const TAURI_LANGUAGES: [Language; 6] = [
    Language::Javascript,
    Language::Typescript,
    Language::Tsx,
    Language::Jsx,
    Language::Svelte,
    Language::Rust,
];

impl FrameworkResolver for TauriBridgeResolver {
    fn name(&self) -> &str {
        "tauri-ipc"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&TAURI_LANGUAGES)
    }

    /// Detect: `tauri.conf.json[5]` (root or nested in a monorepo), a
    /// `@tauri-apps/api` dependency in root `package.json`, or any tracked Rust
    /// file using `tauri::command`.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // tauri.conf.json (or tauri.conf.json5) is the definitive marker at root.
        if context.file_exists("tauri.conf.json") || context.file_exists("tauri.conf.json5") {
            return true;
        }
        // Nested Tauri app (e.g. monorepo): any tracked tauri.conf.json[5].
        let files = context.get_all_files();
        if files
            .iter()
            .any(|f| f.ends_with("tauri.conf.json") || f.ends_with("tauri.conf.json5"))
        {
            return true;
        }
        // Fallback 1: root package.json depends on @tauri-apps/api.
        if let Some(pkg) = context.read_file("package.json") {
            if pkg.contains("\"@tauri-apps/api\"") {
                return true;
            }
        }
        // Fallback 2 (definitive for monorepos): any tracked Rust file uses
        // tauri::command.
        for f in &files {
            if !f.ends_with(".rs") {
                continue;
            }
            if let Some(src) = context.read_file(f) {
                if src.contains("tauri::command") {
                    return true;
                }
            }
        }
        false
    }

    /// Only the raw `invoke`/`listen`/`once` wire-name references; the typed
    /// `commands.*` / `events.*` path resolves through the JS extractor's own
    /// member references. No framework nodes.
    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !content.contains("invoke") && !content.contains("listen") && !content.contains("once") {
            return Some(FrameworkExtractionResult::default());
        }
        Some(FrameworkExtractionResult {
            nodes: Vec::new(),
            references: extract_wire_references(file_path, content),
        })
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Only redirect JS/TS callers.
        if !TS_LANGUAGES.contains(&reference.language) {
            return None;
        }

        let mut cache = self.cache.lock().unwrap();
        if cache.is_none() {
            *cache = Some(build_index(context));
        }
        let index = cache.as_ref().unwrap();

        // Strip receiver prefix: `commands.getMcpPort` -> `getMcpPort`,
        // `events.volumeSpaceChanged` -> `volumeSpaceChanged`.
        let name = match reference.reference_name.rfind('.') {
            Some(idx) => &reference.reference_name[idx + 1..],
            None => reference.reference_name.as_str(),
        };

        // Command lookup first; for raw invoke('snake_name') also try
        // kebab -> snake conversion.
        let cmd = index
            .commands
            .get(name)
            .or_else(|| index.commands.get(&kebab_to_snake(name)));
        if let Some(entry) = cmd {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: entry.node.id.clone(),
                confidence: 0.7,
                resolved_by: ResolvedBy::Framework,
            });
        }

        // Event lookup.
        let evt = index
            .events
            .get(name)
            .or_else(|| index.events.get(&kebab_to_snake(name)));
        if let Some(entry) = evt {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: entry.node.id.clone(),
                confidence: 0.6,
                resolved_by: ResolvedBy::Framework,
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ImportMapping;

    struct Ctx {
        nodes: Vec<Node>,
        files: std::collections::HashMap<String, String>,
    }

    impl Ctx {
        fn new(nodes: Vec<Node>, files: &[(&str, &str)]) -> Self {
            Ctx {
                nodes,
                files: files
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            }
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, qn: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == qn)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn file_exists(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }
        fn read_file(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/test"
        }
        fn get_all_files(&self) -> Vec<String> {
            let mut all: Vec<String> = self.files.keys().cloned().collect();
            for n in &self.nodes {
                if !all.contains(&n.file_path) {
                    all.push(n.file_path.clone());
                }
            }
            all
        }
        fn get_import_mappings(&self, _file_path: &str, _lang: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn rust_fn(name: &str, file_path: &str, start_line: u32) -> Node {
        Node::new(
            format!("rust:{file_path}:{name}:{start_line}"),
            NodeKind::Function,
            name,
            format!("{file_path}::{name}"),
            file_path,
            Language::Rust,
            start_line,
            start_line + 5,
        )
    }

    fn rust_struct(name: &str, file_path: &str, start_line: u32) -> Node {
        Node::new(
            format!("rust:{file_path}:{name}:{start_line}"),
            NodeKind::Struct,
            name,
            format!("{file_path}::{name}"),
            file_path,
            Language::Rust,
            start_line,
            start_line + 5,
        )
    }

    fn make_ref(name: &str, language: Language, file_path: &str) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: format!("caller:{file_path}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            column: 0,
            file_path: file_path.to_string(),
            language,
            candidates: None,
            metadata: None,
        }
    }

    // -- name conversions --

    #[test]
    fn name_conversions() {
        assert_eq!(snake_to_camel("get_mcp_port"), "getMcpPort");
        assert_eq!(snake_to_camel("already"), "already");
        assert_eq!(
            kebab_to_snake("volume-space-changed"),
            "volume_space_changed"
        );
        assert_eq!(
            pascal_to_snake("VolumeSpaceChanged"),
            "volume_space_changed"
        );
        assert_eq!(
            pascal_to_snake("LowDiskSpacePayload"),
            "low_disk_space_payload"
        );
    }

    // -- detect() --

    #[test]
    fn detects_tauri_conf_at_root() {
        let ctx = Ctx::new(vec![], &[("tauri.conf.json", "{}")]);
        assert!(TauriBridgeResolver::new().detect(&ctx));
    }

    #[test]
    fn detects_tauri_conf_in_subdirectory() {
        let ctx = Ctx::new(vec![], &[("apps/desktop/src-tauri/tauri.conf.json", "{}")]);
        assert!(TauriBridgeResolver::new().detect(&ctx));
    }

    #[test]
    fn detects_tauri_apps_api_dependency() {
        let ctx = Ctx::new(
            vec![],
            &[(
                "package.json",
                r#"{"dependencies":{"@tauri-apps/api":"^2.0.0"}}"#,
            )],
        );
        assert!(TauriBridgeResolver::new().detect(&ctx));
    }

    #[test]
    fn detects_tauri_command_in_rust() {
        let ctx = Ctx::new(
            vec![],
            &[("src-tauri/src/main.rs", "#[tauri::command]\nfn x() {}\n")],
        );
        assert!(TauriBridgeResolver::new().detect(&ctx));
    }

    #[test]
    fn no_detect_without_tauri_signals() {
        let ctx = Ctx::new(
            vec![],
            &[("package.json", r#"{"dependencies":{"react":"^18.0"}}"#)],
        );
        assert!(!TauriBridgeResolver::new().detect(&ctx));
    }

    // -- commands: this is the #1543 regression (empty callers before the fix) --

    #[test]
    fn resolves_typed_camelcase_call_to_snake_case_command() {
        let node = rust_fn("get_mcp_port", "src/commands/settings.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/commands/settings.rs",
                    "#[tauri::command]\npub async fn get_mcp_port() -> u16 { 0 }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("getMcpPort", Language::Typescript, "src/settings.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
    }

    #[test]
    fn resolves_receiver_qualified_command() {
        let node = rust_fn("get_mcp_port", "src/commands/settings.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/commands/settings.rs",
                    "#[tauri::command]\npub async fn get_mcp_port() -> u16 { 0 }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref(
                    "commands.getMcpPort",
                    Language::Typescript,
                    "src/settings.ts",
                ),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn resolves_raw_invoke_snake_case_wire_name() {
        let node = rust_fn("list_directory", "src/commands/fs.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/commands/fs.rs",
                    "#[tauri::command]\npub async fn list_directory(path: String) {}\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("list_directory", Language::Typescript, "src/fs.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn handles_stacked_specta_and_command_attributes() {
        let node = rust_fn("get_settings", "src/commands/settings.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/commands/settings.rs",
                    "#[specta::specta]\n#[tauri::command]\npub async fn get_settings() {}\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("getSettings", Language::Typescript, "src/app.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn ignores_rust_language_caller() {
        let node = rust_fn("get_mcp_port", "src/commands/settings.rs", 10);
        let ctx = Ctx::new(
            vec![node],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/commands/settings.rs",
                    "#[tauri::command]\npub async fn get_mcp_port() -> u16 { 0 }\n",
                ),
            ],
        );
        assert!(
            TauriBridgeResolver::new()
                .resolve(
                    &make_ref("get_mcp_port", Language::Rust, "src/other.rs"),
                    &ctx
                )
                .is_none()
        );
    }

    #[test]
    fn returns_none_when_no_matching_command() {
        let ctx = Ctx::new(vec![], &[("src-tauri/tauri.conf.json", "{}")]);
        assert!(
            TauriBridgeResolver::new()
                .resolve(
                    &make_ref("nonExistent", Language::Typescript, "src/app.ts"),
                    &ctx
                )
                .is_none()
        );
    }

    // -- events --

    #[test]
    fn resolves_typed_camelcase_event_to_struct() {
        let node = rust_struct("VolumeSpaceChanged", "src/space_poller.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/space_poller.rs",
                    "#[derive(Clone, serde::Serialize, tauri_specta::Event)]\npub struct VolumeSpaceChanged { pub path: String }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("volumeSpaceChanged", Language::Typescript, "src/volumes.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
    }

    #[test]
    fn resolves_receiver_qualified_event() {
        let node = rust_struct("VolumeSpaceChanged", "src/space_poller.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/space_poller.rs",
                    "#[derive(Clone, serde::Serialize, tauri_specta::Event)]\npub struct VolumeSpaceChanged { pub path: String }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref(
                    "events.volumeSpaceChanged",
                    Language::Typescript,
                    "src/volumes.ts",
                ),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn resolves_raw_kebab_case_event_listener() {
        let node = rust_struct("VolumeSpaceChanged", "src/space_poller.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/space_poller.rs",
                    "#[derive(Clone, serde::Serialize, tauri_specta::Event)]\npub struct VolumeSpaceChanged { pub path: String }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref(
                    "volume-space-changed",
                    Language::Typescript,
                    "src/volumes.ts",
                ),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn handles_bare_event_derive() {
        let node = rust_struct("AccentColorChanged", "src/theme.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/theme.rs",
                    "use tauri_specta::Event;\n#[derive(Clone, Serialize, Event)]\npub struct AccentColorChanged { pub color: String }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("accentColorChanged", Language::Typescript, "src/theme.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    #[test]
    fn respects_event_name_override() {
        let node = rust_struct("LowDiskSpacePayload", "src/space_poller.rs", 10);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src/space_poller.rs",
                    "#[derive(Clone, Serialize, tauri_specta::Event)]\n#[tauri_specta(event_name = \"low-disk-space\")]\npub struct LowDiskSpacePayload { pub path: String }\n",
                ),
            ],
        );
        let resolved = TauriBridgeResolver::new()
            .resolve(
                &make_ref("low-disk-space", Language::Typescript, "src/space.ts"),
                &ctx,
            )
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
    }

    // -- extract(): raw invoke/listen wire names --

    #[test]
    fn extract_emits_reference_for_invoke_attributed_to_file_node() {
        let result = TauriBridgeResolver::new()
            .extract(
                "src/api.ts",
                "import { invoke } from '@tauri-apps/api/core';\nexport const detect = (t) => invoke('lang_detect', { text: t });\n",
            )
            .expect("extraction ran");
        assert!(result.nodes.is_empty());
        assert_eq!(result.references.len(), 1);
        let r = &result.references[0];
        assert_eq!(r.reference_name, "lang_detect");
        assert_eq!(r.from_node_id, "file:src/api.ts");
        assert_eq!(r.reference_kind, EdgeKind::Calls);
        assert_eq!(r.language, Language::Typescript);
    }

    #[test]
    fn extract_emits_references_for_generics_listen_and_once() {
        let names: Vec<String> = TauriBridgeResolver::new()
            .extract(
                "src/x.tsx",
                "invoke<number>('get_port');\nlisten('volume-space-changed', cb);\nonce('app-ready', cb);\n",
            )
            .expect("extraction ran")
            .references
            .into_iter()
            .map(|r| r.reference_name)
            .collect();
        assert_eq!(names, vec!["get_port", "volume-space-changed", "app-ready"]);
    }

    #[test]
    fn extract_skips_dynamic_names() {
        let result = TauriBridgeResolver::new()
            .extract("src/x.ts", "invoke(cmdName);\ninvoke(`evt_${id}`);\n")
            .expect("extraction ran");
        assert!(result.references.is_empty());
    }

    #[test]
    fn extract_returns_nothing_for_non_js_file() {
        let result = TauriBridgeResolver::new()
            .extract("src/main.rs", "invoke('x')")
            .expect("extraction ran");
        assert!(result.references.is_empty());
    }

    #[test]
    fn extract_wire_reference_reports_line_and_column() {
        let result = TauriBridgeResolver::new()
            .extract("src/api.ts", "const x = 1;\n  invoke('do_thing');\n")
            .expect("extraction ran");
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].line, 2);
        assert_eq!(result.references[0].column, 2);
    }

    #[test]
    fn raw_invoke_wire_reference_resolves_to_command() {
        // The full raw-invoke flow: extract() surfaces the wire name, then
        // resolve() joins it to the Rust command node.
        let node = rust_fn("lang_detect", "src-tauri/src/main.rs", 2);
        let ctx = Ctx::new(
            vec![node.clone()],
            &[
                ("src-tauri/tauri.conf.json", "{}"),
                (
                    "src-tauri/src/main.rs",
                    "#[tauri::command]\npub fn lang_detect(text: String) -> String { text }\n",
                ),
            ],
        );
        let extracted = TauriBridgeResolver::new()
            .extract(
                "src/detect.ts",
                "import { invoke } from '@tauri-apps/api/core';\nexport const detect = (t) => invoke('lang_detect', { text: t });\n",
            )
            .expect("extraction ran");
        assert_eq!(extracted.references.len(), 1);
        let resolved = TauriBridgeResolver::new()
            .resolve(&extracted.references[0], &ctx)
            .expect("resolved");
        assert_eq!(resolved.target_node_id, node.id);
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
    }
}
