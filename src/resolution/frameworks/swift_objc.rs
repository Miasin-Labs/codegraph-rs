//! Swift ↔ Objective-C bridge resolver.
//!
//! Closes the cross-language flow gap in mixed iOS codebases. The pure
//! bridging name math lives in `../swift-objc-bridge.ts` in the TS tree;
//! this file wires it into the resolution pipeline.
//!
//! Ported from `src/resolution/frameworks/swift-objc.ts`.
//!
//! NOTE (port workaround): at port time `crate::resolution::swift_objc_bridge`
//! was an unported stub owned by another agent, so the two bridge-math
//! functions this resolver needs (`swiftBaseNamesForObjcSelector`,
//! `isObjcExposed`) are implemented here as private functions (faithful
//! ports of `src/resolution/swift-objc-bridge.ts`). When
//! `swift_objc_bridge.rs` lands, switch the two `bridge_*` private fns
//! below to `use crate::resolution::swift_objc_bridge::{...}` and delete
//! the local copies. See `notes/frameworks-systems.md`.
//!
//! **Two directions to close:**
//!
//! 1. **Swift call → ObjC method** — A Swift caller writes
//!    `imageDownloader.download(url:completion:)`. Tree-sitter-swift parses
//!    this as a call_expression whose callee identifier is `download`
//!    (parameter labels live in the argument list, not the callee). The
//!    name-matcher tries to find any node named `download` and fails (no
//!    Swift method by that name in this project; the ObjC implementation is
//!    `-downloadURL:completion:`). We catch it here: from the bare Swift
//!    name `download`, look up ObjC methods whose bridged Swift base name
//!    would be `download` (using the reverse map, precomputed once per
//!    session).
//!
//! 2. **ObjC call → Swift method** — An ObjC caller writes
//!    `[swiftThing fooWithBar:42]`. Tree-sitter-objc parses this as a
//!    message_expression with selector `fooWithBar:`. The name-matcher
//!    tries to find a node named `fooWithBar:` — no Swift node has colons
//!    in its name, so it fails. We catch it: from the ObjC selector,
//!    derive candidate Swift base names (`['fooWithBar', 'foo']`), and
//!    look up Swift methods named those.
//!
//! **Provenance:** every edge produced here is recorded as a framework-
//! resolved reference (`resolvedBy: 'framework'`) with `confidence: 0.6`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::resolution::types::{
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

/// Names that are too generic to bridge with any precision. These are common
/// Cocoa / NSObject conventions that almost every ObjC class implements; if a
/// Swift caller writes `init()` or `description`, mapping it to an arbitrary
/// project-local ObjC method of the same name produces noise, not signal.
///
/// Critically, refs of these names virtually always resolve via the regular
/// name-matcher (every project has many `init` nodes) — skipping them here
/// just keeps the bridge from competing with name-match on already-handled
/// refs.
const GENERIC_NAMES: &[&str] = &[
    "init",
    "description",
    "debugDescription",
    "hash",
    "isEqual",
    "isEqualTo",
    "copy",
    "mutableCopy",
    "class",
    "self",
    "count",
    "length",
    "value",
    "name",
    "data",
    "string",
    "object",
    "add",
    "remove",
    "update",
    "load",
    "save",
    "reload",
    "cancel",
    "start",
    "stop",
    "pause",
    "resume",
    "close",
    "open",
    "show",
    "hide",
    "toString",
    "dealloc",
    "release",
    "retain",
    "autorelease",
];

// =============================================================================
// Private bridge math (faithful local port of swift-objc-bridge.ts — see the
// module-level NOTE for why it lives here)
// =============================================================================

/// Lowercase the first character. Used in reverse: `setName:` setter ↔
/// Swift property `name`.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Reverse: from an ObjC selector, return the candidate Swift base names
/// the resolver should try when looking for the bridged Swift declaration.
///
/// Examples:
///   `play`                 → ['play']
///   `play:`                → ['play']
///   `playWithSong:`        → ['play', 'playWithSong']  (insertion order: raw first)
///   `initWithName:`        → ['init']                  (init is its own base name)
///   `setName:`             → ['name', 'setName']       (could be a setter OR a regular func)
fn bridge_swift_base_names_for_objc_selector(selector: &str) -> Vec<String> {
    if selector.is_empty() {
        return Vec::new();
    }

    // Strip trailing colons and split into keywords.
    let keywords: Vec<&str> = selector.trim_end_matches(':').split(':').collect();
    let first_keyword = keywords[0];
    if first_keyword.is_empty() {
        return Vec::new();
    }

    // Insertion-ordered set (mirrors the TS `Set`).
    let mut candidates: Vec<String> = Vec::new();
    let add = |candidates: &mut Vec<String>, c: String| {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    };

    // Always a candidate: the raw first keyword. Covers
    //   `play:`           → `play`
    //   `play:by:`        → `play`
    //   `playWithSong:`   → `playWithSong` (a literal Swift name)
    //   `tableView:...:`  → `tableView`
    add(&mut candidates, first_keyword.to_string());

    // `initWith<X>:` and `initWith<X>:<more>:` always reduce to `init`.
    if first_keyword.starts_with("initWith") {
        add(&mut candidates, "init".to_string());
    }

    // Preposition-prefix patterns: `<base>(With|For|By|In|On|At|From|To|Of|As)<Cap>:`
    // covers both Swift's @objc EXPORT rule (always "With") and Cocoa's
    // IMPORTED selectors which use other prepositions natively (e.g.
    // `objectForKey:`, `stringWithFormat:`, `compareTo:`,
    // `imageNamed:inBundle:`). Strip to recover the Swift base name a caller
    // would use (e.g. `object`, `string`, `compare`, `image`).
    static PREPOSITION_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([a-z][a-zA-Z0-9]*?)(?:With|For|By|In|On|At|From|To|Of|As)[A-Z]").unwrap()
    });
    if let Some(caps) = PREPOSITION_RE.captures(first_keyword) {
        if let Some(base) = caps.get(1) {
            if !base.as_str().is_empty() {
                add(&mut candidates, base.as_str().to_string());
            }
        }
    }

    // `setX:` could be a property setter — the Swift property is `x` (lowercase).
    // Only fires for the obvious shape: `set` + capital letter + ':' (one param).
    static SET_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^set[A-Z]").unwrap());
    if keywords.len() == 1 && SET_RE.is_match(first_keyword) && selector.ends_with(':') {
        let prop_name = lower_first(&first_keyword[3..]);
        if !prop_name.is_empty() {
            add(&mut candidates, prop_name);
        }
    }

    candidates
}

/// Detect whether a Swift declaration is `@objc`-exposed by scanning the
/// source slice that precedes it. Returns true for explicit `@objc` or
/// `@objc(custom:)`.
///
/// `@nonobjc` returns false even if `@objc` also appears (per Swift's rule
/// that `@nonobjc` opts out of class-level `@objcMembers`).
fn bridge_is_objc_exposed(source_slice: &str) -> bool {
    static NONOBJC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@nonobjc\b").unwrap());
    static OBJC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@objc\b").unwrap());
    if NONOBJC_RE.is_match(source_slice) {
        return false;
    }
    OBJC_RE.is_match(source_slice)
}

// =============================================================================
// Resolver
// =============================================================================

/// Window of source text around a Swift declaration used by the
/// `@objc`-exposure check. Read lines above + the declaration line — Swift
/// attributes typically sit on the preceding line (`@objc` on a line of its
/// own) or inline.
const SOURCE_PROBE_LINES: u32 = 3;

/// Build the reverse-bridge map: for every ObjC method node in the graph,
/// compute the Swift base names that would auto-bridge to its selector and
/// record the node under each.
///
/// Runs once per resolver lifetime; the cost scales linearly with the count
/// of ObjC method nodes. On Wikipedia-iOS (~2500 files, ~25k ObjC methods)
/// this is a few hundred ms — much cheaper than re-parsing source on each
/// unresolved ref.
fn build_objc_map(context: &dyn ResolutionContext) -> HashMap<String, Vec<Node>> {
    let mut map: HashMap<String, Vec<Node>> = HashMap::new();
    let objc_methods: Vec<Node> = context
        .get_nodes_by_kind(NodeKind::Method)
        .into_iter()
        .filter(|n| n.language == Language::Objc)
        .collect();
    for node in objc_methods {
        let candidates = bridge_swift_base_names_for_objc_selector(&node.name);
        for c in candidates {
            // Skip the trivial case where the Swift base name equals the ObjC
            // method name verbatim (no colons) — the regular name-matcher
            // already handles that and our map would just duplicate the work.
            if c == node.name && !node.name.contains(':') {
                continue;
            }
            // Skip generic Cocoa names (init, description, etc.) — they would
            // false-positive against any project-local ObjC method of the same
            // name. The regular name-matcher handles them.
            if GENERIC_NAMES.contains(&c.as_str()) {
                continue;
            }
            map.entry(c).or_default().push(node.clone());
        }
    }
    map
}

/// Read a small window of source ending at `node.start_line`, used to
/// inspect Swift attribute annotations attached to a declaration. Returns
/// an empty string if the source can't be read.
fn declaration_source_window(node: &Node, context: &dyn ResolutionContext) -> String {
    let Some(content) = context.read_file(&node.file_path) else {
        return String::new();
    };
    // TS content.split(/\r?\n/) — str::lines() splits on '\n' and strips a
    // trailing '\r' (the trailing-empty-line difference is harmless here).
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = node.start_line.saturating_sub(1 + SOURCE_PROBE_LINES) as usize;
    let end_idx = std::cmp::min(lines.len(), node.start_line as usize);
    if start_idx >= end_idx {
        return String::new();
    }
    lines[start_idx..end_idx].join("\n")
}

/// TS `swiftObjcBridgeResolver` (name: `"swift-objc-bridge"`).
///
/// Holds the memoized "Swift base name → ObjC method nodes" map (TS used a
/// module-level `WeakMap<ResolutionContext, Map>`; here it's per-instance,
/// keyed by project root so multiple projects sharing a process — the
/// daemon — don't bleed maps between them).
#[derive(Debug, Default)]
pub struct SwiftObjcBridgeResolver {
    objc_by_candidate_swift_base: RefCell<HashMap<String, HashMap<String, Vec<Node>>>>,
}

impl SwiftObjcBridgeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to resolve a Swift caller's bare reference to an ObjC implementation.
    ///
    /// Strategy: look up the ObjC reverse-bridge map for nodes whose Swift base
    /// name would match. Return the first match (matches the existing
    /// single-target resolution contract).
    fn resolve_swift_call_to_objc(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Swift call sites of `obj.foo(bar:)` reach the resolver as either bare
        // name `foo` (tree-sitter-swift) or qualified `obj.foo` — strip prefix.
        let raw_name = match reference.reference_name.rfind('.') {
            Some(i) => &reference.reference_name[i + 1..],
            None => &reference.reference_name,
        };

        let root = context.get_project_root().to_string();
        let mut cache = self.objc_by_candidate_swift_base.borrow_mut();
        let map = cache.entry(root).or_insert_with(|| build_objc_map(context));
        let candidates = map.get(raw_name)?;
        if candidates.is_empty() {
            return None;
        }

        // Prefer ObjC methods whose corresponding Swift declaration isn't itself
        // present (so we don't wrongly redirect a Swift call to ObjC when a Swift
        // method of the same name is the real target — that's the in-language case
        // and should already be resolved by the name-matcher). Since this resolver
        // runs AFTER exact-match, any matching Swift node would already have won;
        // so a candidate reaching us is a legitimate cross-language hit.
        let target = candidates.first()?;
        Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: target.id.clone(),
            confidence: 0.6,
            resolved_by: ResolvedBy::Framework,
        })
    }

    /// Try to resolve an ObjC caller's selector reference to a Swift `@objc`
    /// implementation.
    ///
    /// Strategy: derive candidate Swift base names from the selector. For each,
    /// look up Swift methods named that and verify with a source-window check
    /// that the declaration is `@objc`-exposed (filters out false matches where
    /// a Swift function happens to share the name but isn't bridged).
    fn resolve_objc_call_to_swift(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // ObjC call sites get receiver-prefixed when the receiver isn't self/super
        // (see tree-sitter.ts message_expression handling): `[obj foo:bar:]`
        // becomes `obj.foo:bar:`. Strip the receiver prefix to recover the raw
        // selector for the bridge math.
        let raw_selector = match reference.reference_name.rfind('.') {
            Some(i) => &reference.reference_name[i + 1..],
            None => &reference.reference_name,
        };

        // Bridge math only applies to selector-shape names (contain `:`).
        if !raw_selector.contains(':') {
            return None;
        }

        let candidates = bridge_swift_base_names_for_objc_selector(raw_selector);
        for candidate in candidates {
            let matches: Vec<Node> = context
                .get_nodes_by_name(&candidate)
                .into_iter()
                .filter(|n| {
                    n.language == Language::Swift
                        && (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                })
                .collect();
            for m in matches {
                let window = declaration_source_window(&m, context);
                if bridge_is_objc_exposed(&window) {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: m.id.clone(),
                        confidence: 0.6,
                        resolved_by: ResolvedBy::Framework,
                    });
                }
            }
        }
        None
    }
}

impl FrameworkResolver for SwiftObjcBridgeResolver {
    fn name(&self) -> &str {
        "swift-objc-bridge"
    }

    /// Applies to both languages — bridging crosses the boundary.
    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Swift, Language::Objc])
    }

    /// Detect: this resolver is relevant when the project has both Swift and
    /// Objective-C source. Either-side-only projects don't need bridging
    /// (and the empty reverse-map would be a no-op anyway).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let files = context.get_all_files();
        let mut has_swift = false;
        let mut has_objc = false;
        for f in &files {
            if f.ends_with(".swift") {
                has_swift = true;
            } else if f.ends_with(".m") || f.ends_with(".mm") {
                has_objc = true;
            }
            if has_swift && has_objc {
                return true;
            }
        }
        false
    }

    /// Let selector-shape references (anything containing a `:`) through the
    /// resolver's name-exists pre-filter — no Swift node has a colon in its
    /// name, so without this opt-in those refs would be dropped before
    /// `resolve()` sees them.
    fn claims_reference(&self, name: &str) -> bool {
        if name.contains(':') {
            return true;
        }
        // Bare names without colons are handled by the regular name-exists
        // pre-filter — no need to opt them in here.
        false
    }

    /// Route based on which language the caller is in. The two directions are
    /// symmetric in shape but very different in implementation (forward
    /// direction uses the precomputed reverse-bridge map; reverse direction
    /// uses the deterministic name-derivation).
    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        if reference.language == Language::Swift {
            return self.resolve_swift_call_to_objc(reference, context);
        }
        if reference.language == Language::Objc {
            return self.resolve_objc_call_to_swift(reference, context);
        }
        None
    }
}
