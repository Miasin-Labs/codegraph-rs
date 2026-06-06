//! Swift ↔ Objective-C bridging rules.
//!
//! Ported from `src/resolution/swift-objc-bridge.ts`.
//!
//! Apple's auto-bridging mechanism exposes Swift declarations to the ObjC
//! runtime under a deterministic selector name. The full rule set:
//! <https://developer.apple.com/documentation/swift/importing-swift-into-objective-c>
//!
//! This module is **pure name math** — given a Swift declaration's base
//! name + parameter external labels (or the raw signature text), produce
//! the bridged ObjC selector(s); given an ObjC selector, produce the
//! candidate Swift base names. No graph/DB access here.
//!
//! Used by `frameworks/swift_objc.rs` (the framework resolver that wires
//! the rules into the resolution pipeline) and by its tests.
//!
//! ─── Bridging cheat sheet ───────────────────────────────────────────────
//!
//! ```text
//!   Swift declaration                             ObjC selector
//!   ─────────────────────────────────────────     ─────────────────────────
//!   func play()                                    play
//!   func play(_ song: String)                      play:
//!   func play(song: String)                        playWithSong:
//!   func play(_ song: String, by artist: String)   play:by:
//!   func play(song: String, by artist: String)     playWithSong:by:
//!   init(name: String)                             initWithName:
//!   init(name: String, age: Int)                   initWithName:age:
//!   var name: String  (getter / setter)            name  /  setName:
//!   @objc(custom:) func f(_ x: Int)                custom:        (literal override)
//! ```
//!
//! The reverse direction (ObjC → Swift) collapses the bridge: a Swift call
//! site for `play(song:)` reaches us as the bare base name `play` (Swift's
//! tree-sitter call_expression strips parameter labels from the callee
//! name). So `swift_base_names_for_objc_selector("playWithSong:")` returns
//! `["play"]` — the resolver looks up Swift methods named `play`.

use std::sync::OnceLock;

use regex::Regex;

/// Capitalize the first character of a string. Used for the "With"-prefix
/// form on the first selector keyword when the Swift declaration has an
/// explicit first-parameter label (e.g. `func play(song:)` → `playWithSong:`).
fn cap_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Lowercase the first character. Used in reverse: `setName:` setter ↔
/// Swift property `name`.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// True when the label counts as "unlabeled" for first-keyword formation
/// (TS: `first === null || first === undefined || first === '_' || first === ''`).
fn is_unlabeled(label: Option<&str>) -> bool {
    matches!(label, None | Some("_") | Some(""))
}

/// Compute the auto-bridged ObjC selector for a Swift method declaration.
///
/// * `base_name` — the Swift method's base name (e.g. `play`).
/// * `external_labels` — parameter EXTERNAL labels in declaration order;
///   `None` for a `_` (unlabeled) parameter. Empty slice for a
///   no-parameter method.
/// * `explicit_objc_name` — if `@objc(customSel:)` was specified, the
///   literal selector — short-circuits the rule and is returned as-is.
///
/// Returns the ObjC selector (e.g. `playWithSong:by:`), or `None` if it
/// can't be determined.
///
/// **Method rules:**
/// - No params → base name (no colons)
/// - Single param, `_` label → `baseName:`
/// - Single param, explicit label `L` → `baseNameWithL:`
/// - Multi-param, `_` first label → `baseName:label2:label3:`
/// - Multi-param, explicit first label `L1` → `baseNameWithL1:label2:label3:`
///
/// Initializer rules are handled by [`objc_selector_for_swift_init`].
pub fn objc_selector_for_swift_method(
    base_name: &str,
    external_labels: &[Option<&str>],
    explicit_objc_name: Option<&str>,
) -> Option<String> {
    if base_name.is_empty() {
        return None;
    }
    if let Some(explicit) = explicit_objc_name {
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
    }

    if external_labels.is_empty() {
        return Some(base_name.to_string());
    }

    let first = external_labels[0];
    let rest = &external_labels[1..];
    // Single param: "_" → "base:" ; "label" → "baseWithLabel:"
    // Multi-param mirrors the same first-keyword formation, then appends each
    // subsequent label as its own keyword. A `None` later label is invalid
    // ObjC (no way to express unlabeled middle params) — keep as `:` to be safe.
    let first_keyword = if is_unlabeled(first) {
        format!("{base_name}:")
    } else {
        format!("{base_name}With{}:", cap_first(first.unwrap_or("")))
    };

    let rest_keywords: String = rest
        .iter()
        .map(|l| format!("{}:", l.unwrap_or("")))
        .collect();
    Some(first_keyword + &rest_keywords)
}

/// Compute the bridged ObjC selector for a Swift `init(...)` declaration.
///
/// **Init rules** (different from regular methods — Apple always uses
/// `initWith` regardless of whether the first label is `_`):
/// - `init()`                       → `init`
/// - `init(_ name: String)`         → `initWithName:`  (uses the INTERNAL
///   name when external is `_`, per Apple's bridging conventions)
/// - `init(name: String)`           → `initWithName:`
/// - `init(name: String, age: Int)` → `initWithName:age:`
///
/// For the `_` case we need the internal (second identifier) name —
/// passed via `internal_names`.
pub fn objc_selector_for_swift_init(
    external_labels: &[Option<&str>],
    internal_names: &[&str],
    explicit_objc_name: Option<&str>,
) -> Option<String> {
    if let Some(explicit) = explicit_objc_name {
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
    }

    if external_labels.is_empty() {
        return Some("init".to_string());
    }

    let first_ext = external_labels[0];
    let rest_ext = &external_labels[1..];
    let first_int = internal_names.first().copied();
    // Use the internal name when external is "_"; ObjC needs *some* keyword,
    // and Swift's auto-bridger uses the parameter's local name in this case.
    let first_label = if matches!(first_ext, None | Some("_") | Some("")) {
        first_int
    } else {
        first_ext
    };
    let first_label = match first_label {
        Some(l) if !l.is_empty() => l,
        _ => return None,
    };

    let first_keyword = format!("initWith{}:", cap_first(first_label));
    let rest_keywords: String = rest_ext
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let internal = internal_names.get(idx + 1).copied();
            let name = match label {
                Some(l) if !l.is_empty() && *l != "_" => l,
                _ => internal.unwrap_or(""),
            };
            format!("{name}:")
        })
        .collect();
    Some(first_keyword + &rest_keywords)
}

/// Bridged ObjC getter + setter pair for a Swift property
/// (TS: inline `{ getter, setter }` object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjcAccessors {
    pub getter: String,
    pub setter: String,
}

/// Compute the bridged ObjC getter + setter for a Swift `@objc` property.
///
/// - `var name: String`        → getter `name`, setter `setName:`
/// - `var isReady: Bool`       → getter `isReady`, setter `setIsReady:`
///   (no special `is` handling — Swift's `isReady` stays as `isReady` in ObjC;
///   `@objc(name:)` overrides if a Cocoa-style getter `isReady` / setter
///   `setReady:` pairing is needed — that's the responsibility of the
///   declaration's `@objc(customGetter)` annotation, which we surface via
///   `explicit_objc_name`.)
pub fn objc_accessors_for_swift_property(
    swift_name: &str,
    explicit_objc_name: Option<&str>,
) -> Option<ObjcAccessors> {
    if swift_name.is_empty() {
        return None;
    }
    // The override syntax `@objc(customGetterName)` re-points the GETTER only;
    // the setter still follows the `setX:` rule but is keyed off the override.
    // (`@objc(getX:setY:)` is not currently supported — that's a rarer
    // shape; can extend later if a real codebase needs it.)
    // NB: TS used `??` (nullish), not `||` — an explicitly-passed empty
    // string IS used as the getter. Preserved.
    let getter = explicit_objc_name.unwrap_or(swift_name);
    Some(ObjcAccessors {
        getter: getter.to_string(),
        setter: format!("set{}:", cap_first(getter)),
    })
}

fn preposition_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([a-z][a-zA-Z0-9]*?)(?:With|For|By|In|On|At|From|To|Of|As)[A-Z]")
            .expect("valid regex")
    })
}

fn set_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^set[A-Z]").expect("valid regex"))
}

/// Reverse: from an ObjC selector, return the candidate Swift base names
/// the resolver should try when looking for the bridged Swift declaration.
///
/// Examples:
/// ```text
///   `play`                 → ["play"]
///   `play:`                → ["play"]
///   `playWithSong:`        → ["play", "playWithSong"]
///   `play:by:`             → ["play"]
///   `playWithSong:by:`     → ["play", "playWithSong"]
///   `initWithName:`        → ["init"]                      (init is its own base name)
///   `initWithName:age:`    → ["init"]
///   `setName:`             → ["name", "setName"]           (could be a setter OR a regular func)
///   `tableView:didSel…:`   → ["tableView"]
/// ```
///
/// Returns multiple candidates because the bare base name is ambiguous —
/// `playWithSong:` could correspond to either `func play(song:)` or
/// `func playWithSong(_ x:)` (a Swift method literally named that with a
/// `_` first label). The resolver tries each.
pub fn swift_base_names_for_objc_selector(selector: &str) -> Vec<String> {
    if selector.is_empty() {
        return Vec::new();
    }

    // Strip trailing colons and split into keywords.
    let stripped = selector.trim_end_matches(':');
    let keywords: Vec<&str> = stripped.split(':').collect();
    let first_keyword = keywords[0];
    if first_keyword.is_empty() {
        return Vec::new();
    }

    // Insertion-ordered set (TS used `Set`).
    let mut candidates: Vec<String> = Vec::new();
    let add = |candidates: &mut Vec<String>, s: String| {
        if !candidates.contains(&s) {
            candidates.push(s);
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
    if let Some(caps) = preposition_re().captures(first_keyword) {
        if let Some(base) = caps.get(1) {
            if !base.as_str().is_empty() {
                add(&mut candidates, base.as_str().to_string());
            }
        }
    }

    // `setX:` could be a property setter — the Swift property is `x` (lowercase).
    // Only fires for the obvious shape: `set` + capital letter + ':' (one param).
    if keywords.len() == 1 && set_prefix_re().is_match(first_keyword) && selector.ends_with(':') {
        let prop_name = lower_first(&first_keyword[3..]);
        if !prop_name.is_empty() {
            add(&mut candidates, prop_name);
        }
    }

    candidates
}

fn objc_override_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@objc\s*\(\s*([^)\s]+)\s*\)").expect("valid regex"))
}

fn nonobjc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@nonobjc\b").expect("valid regex"))
}

fn objc_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@objc\b").expect("valid regex"))
}

/// Detect whether a Swift method `@objc` declaration uses the `@objc(custom:)`
/// override form, returning the literal selector when present.
///
/// Regex-based scan over the small chunk of source preceding the declaration —
/// tree-sitter would be more precise but this is only consulted as a fallback
/// when the structured AST isn't available (e.g. resolver-time lookups
/// via `context.read_file`).
///
/// Returns `None` when the declaration is plain `@objc` (no override) or has
/// no `@objc` attribute at all.
pub fn detect_explicit_objc_name(source_slice: &str) -> Option<String> {
    // `@objc(customName:)` or `@objc(custom:name:)` — the parens contents are
    // the literal ObjC selector. Whitespace permitted.
    objc_override_re()
        .captures(source_slice)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect whether a Swift declaration is `@objc`-exposed by scanning the
/// source slice that precedes it. Returns true for explicit `@objc`,
/// `@objc(custom:)`, or membership in a `@objcMembers` class (caller's
/// responsibility to pass class-level context if relevant).
///
/// `@nonobjc` returns false even if `@objc` also appears (per Swift's rule
/// that `@nonobjc` opts out of class-level `@objcMembers`).
pub fn is_objc_exposed(source_slice: &str) -> bool {
    if nonobjc_re().is_match(source_slice) {
        return false;
    }
    objc_attr_re().is_match(source_slice)
}

// ---------------------------------------------------------------------------
// Tests — ported 1:1 from __tests__/swift-objc-bridge.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    // ── Swift → ObjC selector bridging (auto-name rules) ────────────────────

    #[test]
    fn method_no_parameters_bare_base_name() {
        assert_eq!(
            objc_selector_for_swift_method("play", &[], None),
            Some("play".to_string())
        );
    }

    #[test]
    fn method_single_underscore_param_base_plus_colon() {
        assert_eq!(
            objc_selector_for_swift_method("play", &[Some("_")], None),
            Some("play:".to_string())
        );
        assert_eq!(
            objc_selector_for_swift_method("play", &[None], None),
            Some("play:".to_string())
        );
    }

    #[test]
    fn method_single_labeled_param_base_with_label() {
        assert_eq!(
            objc_selector_for_swift_method("play", &[Some("song")], None),
            Some("playWithSong:".to_string())
        );
    }

    #[test]
    fn method_multi_param_leading_underscore() {
        assert_eq!(
            objc_selector_for_swift_method("play", &[Some("_"), Some("by")], None),
            Some("play:by:".to_string())
        );
        assert_eq!(
            objc_selector_for_swift_method(
                "tableView",
                &[Some("_"), Some("didSelectRowAtIndexPath")],
                None
            ),
            Some("tableView:didSelectRowAtIndexPath:".to_string())
        );
    }

    #[test]
    fn method_multi_param_leading_explicit_label() {
        assert_eq!(
            objc_selector_for_swift_method("play", &[Some("song"), Some("by")], None),
            Some("playWithSong:by:".to_string())
        );
    }

    #[test]
    fn method_objc_custom_overrides_the_rule_literally() {
        assert_eq!(
            objc_selector_for_swift_method("whateverName", &[Some("ignored")], Some("custom:")),
            Some("custom:".to_string())
        );
    }

    #[test]
    fn method_returns_none_on_empty_base_name() {
        assert_eq!(objc_selector_for_swift_method("", &[], None), None);
    }

    #[test]
    fn init_no_params() {
        assert_eq!(
            objc_selector_for_swift_init(&[], &[], None),
            Some("init".to_string())
        );
    }

    #[test]
    fn init_named_param() {
        assert_eq!(
            objc_selector_for_swift_init(&[Some("name")], &["name"], None),
            Some("initWithName:".to_string())
        );
    }

    #[test]
    fn init_two_named_params() {
        assert_eq!(
            objc_selector_for_swift_init(&[Some("name"), Some("age")], &["name", "age"], None),
            Some("initWithName:age:".to_string())
        );
    }

    #[test]
    fn init_underscore_uses_internal_name() {
        assert_eq!(
            objc_selector_for_swift_init(&[Some("_")], &["name"], None),
            Some("initWithName:".to_string())
        );
    }

    #[test]
    fn init_objc_custom_override() {
        assert_eq!(
            objc_selector_for_swift_init(&[Some("name")], &["name"], Some("custom:")),
            Some("custom:".to_string())
        );
    }

    #[test]
    fn property_getter_name_setter_set_name() {
        assert_eq!(
            objc_accessors_for_swift_property("name", None),
            Some(ObjcAccessors {
                getter: "name".to_string(),
                setter: "setName:".to_string(),
            })
        );
    }

    #[test]
    fn property_camel_case_set_capitalizes_first() {
        assert_eq!(
            objc_accessors_for_swift_property("isReady", None),
            Some(ObjcAccessors {
                getter: "isReady".to_string(),
                setter: "setIsReady:".to_string(),
            })
        );
    }

    #[test]
    fn property_explicit_objc_custom_overrides_getter_name() {
        assert_eq!(
            objc_accessors_for_swift_property("name", Some("displayName")),
            Some(ObjcAccessors {
                getter: "displayName".to_string(),
                setter: "setDisplayName:".to_string(),
            })
        );
    }

    // ── ObjC selector → Swift base name candidates (reverse map) ────────────

    #[test]
    fn bare_no_colon_selector_itself() {
        assert_eq!(swift_base_names_for_objc_selector("play"), vec!["play"]);
    }

    #[test]
    fn play_colon_to_play() {
        assert_eq!(swift_base_names_for_objc_selector("play:"), vec!["play"]);
    }

    #[test]
    fn play_with_song_to_play_and_literal() {
        assert_eq!(
            sorted(swift_base_names_for_objc_selector("playWithSong:")),
            sorted(vec!["play".to_string(), "playWithSong".to_string()])
        );
    }

    #[test]
    fn cocoa_style_object_for_key_includes_object() {
        assert!(
            swift_base_names_for_objc_selector("objectForKey:").contains(&"object".to_string())
        );
    }

    #[test]
    fn cocoa_style_string_with_format_includes_string() {
        assert!(
            swift_base_names_for_objc_selector("stringWithFormat:").contains(&"string".to_string())
        );
    }

    #[test]
    fn image_named_in_bundle_no_preposition_falls_through() {
        // First keyword is `imageNamed` — no With/For/By in it, so candidates is
        // just the raw keyword. (`Named` is not in our preposition list — keep
        // it that way, otherwise we over-match on perfectly normal verbs.)
        assert_eq!(
            swift_base_names_for_objc_selector("imageNamed:inBundle:"),
            vec!["imageNamed"]
        );
    }

    #[test]
    fn play_by_to_play() {
        assert_eq!(swift_base_names_for_objc_selector("play:by:"), vec!["play"]);
    }

    #[test]
    fn play_with_song_by_to_play_and_literal() {
        assert_eq!(
            sorted(swift_base_names_for_objc_selector("playWithSong:by:")),
            sorted(vec!["play".to_string(), "playWithSong".to_string()])
        );
    }

    #[test]
    fn init_with_name_includes_init() {
        assert!(swift_base_names_for_objc_selector("initWithName:").contains(&"init".to_string()));
    }

    #[test]
    fn init_with_name_age_includes_init() {
        assert!(
            swift_base_names_for_objc_selector("initWithName:age:").contains(&"init".to_string())
        );
    }

    #[test]
    fn set_name_includes_property_name() {
        assert!(swift_base_names_for_objc_selector("setName:").contains(&"name".to_string()));
    }

    #[test]
    fn table_view_did_select_row_at_index_path() {
        assert_eq!(
            swift_base_names_for_objc_selector("tableView:didSelectRowAtIndexPath:"),
            vec!["tableView"]
        );
    }

    // ── Source-window attribute detection ───────────────────────────────────

    #[test]
    fn detects_literal_objc_custom() {
        assert_eq!(
            detect_explicit_objc_name("  @objc(custom:)\n  func foo() {}"),
            Some("custom:".to_string())
        );
    }

    #[test]
    fn returns_none_for_plain_objc() {
        assert_eq!(detect_explicit_objc_name("@objc func foo() {}"), None);
    }

    #[test]
    fn returns_none_when_no_objc_at_all() {
        assert_eq!(detect_explicit_objc_name("public func foo() {}"), None);
    }

    #[test]
    fn is_objc_exposed_true_for_objc() {
        assert!(is_objc_exposed("@objc func foo() {}"));
    }

    #[test]
    fn is_objc_exposed_true_for_objc_custom() {
        assert!(is_objc_exposed("@objc(custom:) func foo() {}"));
    }

    #[test]
    fn is_objc_exposed_false_for_no_annotation() {
        assert!(!is_objc_exposed("public func foo() {}"));
    }

    #[test]
    fn nonobjc_opts_out_even_if_objc_also_present() {
        assert!(!is_objc_exposed("@nonobjc @objc func foo() {}"));
    }
}
