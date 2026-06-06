//! Project-level import-path alias loading.
//!
//! Reads `compilerOptions.paths` from `tsconfig.json` / `jsconfig.json`
//! at the project root and converts the patterns into a form the
//! import-resolver can consult.
//!
//! This is the single biggest blocker to accurate resolution on modern
//! JS/TS codebases: aliases like `@/components/Foo` (Next, Nuxt, Nest,
//! Vite scaffolds) point into a `paths` map the resolver previously
//! ignored — every import through an alias was treated as unresolvable
//! unless it happened to match the small hard-coded fallback list.
//!
//! Scope deliberately small for v1:
//!   - reads tsconfig.json, then jsconfig.json
//!   - honours top-level `compilerOptions.baseUrl` and `compilerOptions.paths`
//!   - supports `*` wildcard (the only TS-supported wildcard)
//!   - does NOT follow `extends` chains yet (most projects don't need it)
//!   - does NOT read Vite/webpack/Rollup configs (separate follow-up)
//!
//! The file is parsed as JSON-with-comments-tolerant — tsconfigs in the
//! wild routinely contain `//` and `/* */` comments and trailing
//! commas, which strict JSON parsing rejects. We strip those before parsing.
//!
//! Ported from `src/resolution/path-aliases.ts`. The `AliasMap` /
//! `AliasPattern` data types are defined in [`super::types`] (see
//! `notes/resolution-types.md`); this file re-exports them and implements
//! only the loader + rewrite.

use std::path::{Component, Path};
use std::sync::LazyLock;

use regex::Regex;

pub use super::types::{AliasMap, AliasPattern};
use crate::error::log_debug;
use crate::utils::lexical_resolve;

/// Strip JSONC comments + trailing commas so a tsconfig with the usual
/// VS Code-style annotations parses cleanly. Walks the source as a
/// tiny state machine that tracks string context — a regex-only
/// version would corrupt any URL inside a string value
/// (`"baseUrl": "https://cdn.example.com"` had everything after `//`
/// truncated).
fn strip_jsonc(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_string {
            if ch == b'\\' && i + 1 < bytes.len() {
                out.push('\\');
                let next_len = utf8_len(bytes[i + 1]);
                out.push_str(&src[i + 1..i + 1 + next_len]);
                i += 1 + next_len;
                continue;
            }
            if ch == b'"' {
                in_string = false;
            }
            // Push the full UTF-8 character (multi-byte sequences contain
            // no ASCII bytes, so byte-wise delimiter checks are exact).
            let ch_len = utf8_len(ch);
            out.push_str(&src[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        if ch == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i += 2;
            i = i.min(bytes.len());
            continue;
        }
        let ch_len = utf8_len(ch);
        out.push_str(&src[i..i + ch_len]);
        i += ch_len;
    }
    // Trailing commas before } or ] — outside strings, so safe to
    // run on the comment-stripped output.
    static TRAILING_COMMA_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r",(\s*[}\]])").expect("valid regex"));
    TRAILING_COMMA_RE.replace_all(&out, "$1").into_owned()
}

/// Length in bytes of the UTF-8 sequence starting with `first_byte`.
fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b >= 0xF0 => 4,
        b if b >= 0xE0 => 3,
        _ => 2,
    }
}

fn read_tsconfig_like(file_path: &Path) -> Option<serde_json::Value> {
    let raw = match std::fs::read_to_string(file_path) {
        Ok(r) => r,
        Err(err) => {
            log_debug(
                "path-aliases: failed to parse",
                Some(&serde_json::json!({
                    "filePath": file_path.to_string_lossy(),
                    "err": err.to_string(),
                })),
            );
            return None;
        }
    };
    match serde_json::from_str::<serde_json::Value>(&strip_jsonc(&raw)) {
        // TS: `parsed && typeof parsed === 'object'` — arrays pass too
        // (typeof [] === 'object'); their missing `compilerOptions` then
        // yields None in the caller without falling through to jsconfig.
        Ok(parsed) if parsed.is_object() || parsed.is_array() => Some(parsed),
        Ok(_) => None,
        Err(err) => {
            log_debug(
                "path-aliases: failed to parse",
                Some(&serde_json::json!({
                    "filePath": file_path.to_string_lossy(),
                    "err": err.to_string(),
                })),
            );
            None
        }
    }
}

fn split_wildcard(pattern: &str) -> (String, String, bool) {
    match pattern.find('*') {
        None => (pattern.to_string(), String::new(), false),
        Some(star) => (
            pattern[..star].to_string(),
            pattern[star + 1..].to_string(),
            true,
        ),
    }
}

/// Load aliases for `project_root`. Returns `None` when no tsconfig /
/// jsconfig is present or when the file has no usable `paths`.
///
/// Cheap to call repeatedly — caching is the caller's job (the
/// resolver memoises it per instance).
pub fn load_project_aliases(project_root: &str) -> Option<AliasMap> {
    let candidates = ["tsconfig.json", "jsconfig.json"];
    let mut raw: Option<serde_json::Value> = None;
    let mut used_file: Option<&str> = None;
    for name in candidates {
        let p = Path::new(project_root).join(name);
        if p.exists() {
            raw = read_tsconfig_like(&p);
            if raw.is_some() {
                used_file = Some(name);
                break;
            }
        }
    }
    let raw = raw?;

    let co = raw.get("compilerOptions");
    let base_url_rel = co
        .and_then(|c| c.get("baseUrl"))
        .and_then(|b| b.as_str())
        .unwrap_or(".");
    let base_url = lexical_resolve(Path::new(project_root), base_url_rel);

    // baseUrl alone isn't an "alias" per se; with no paths we'd just
    // be redirecting the whole tree. Skip — the existing resolver
    // already handles relative imports.
    let paths = co
        .and_then(|c| c.get("paths"))
        .and_then(|p| p.as_object())?;

    let mut patterns: Vec<AliasPattern> = Vec::new();
    for (pattern, targets) in paths {
        let Some(targets) = targets.as_array() else {
            continue;
        };
        if targets.is_empty() {
            continue;
        }
        let filtered: Vec<String> = targets
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        if filtered.is_empty() {
            continue;
        }
        let (prefix, suffix, has_wildcard) = split_wildcard(pattern);
        patterns.push(AliasPattern {
            prefix,
            suffix,
            has_wildcard,
            replacements: filtered,
        });
    }

    if patterns.is_empty() {
        return None;
    }

    // Specificity sort: longer prefix first; literal patterns before
    // wildcard patterns of the same prefix length. TypeScript itself
    // uses a similar "most specific match wins" rule. (Stable sort,
    // matching TS Array.prototype.sort semantics.)
    patterns.sort_by(|a, b| {
        if a.prefix.len() != b.prefix.len() {
            return b.prefix.len().cmp(&a.prefix.len());
        }
        if a.has_wildcard != b.has_wildcard {
            return a.has_wildcard.cmp(&b.has_wildcard); // literal (false) first
        }
        std::cmp::Ordering::Equal
    });

    log_debug(
        "path-aliases loaded",
        Some(&serde_json::json!({
            "file": used_file,
            "baseUrl": base_url.to_string_lossy(),
            "patternCount": patterns.len(),
        })),
    );

    Some(AliasMap { base_url, patterns })
}

/// Resolve an import path through an [`AliasMap`]. Returns the list
/// of candidate filesystem paths (relative to `project_root`), in the
/// priority order defined by tsconfig (multiple replacements per alias
/// are tried in order). Returns `[]` when no alias matches.
///
/// Callers still need to try each candidate with the language's
/// extension list — this function only does the alias rewrite.
pub fn apply_aliases(import_path: &str, aliases: &AliasMap, project_root: &str) -> Vec<String> {
    for pat in &aliases.patterns {
        if !import_path.starts_with(&pat.prefix) {
            continue;
        }
        if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
            continue;
        }

        let mut captured = "";
        if pat.has_wildcard {
            // Clamped like JS `String.slice` (an overlapping prefix/suffix
            // yields an empty capture rather than a panic).
            let end = import_path.len().saturating_sub(pat.suffix.len());
            let start = pat.prefix.len().min(end);
            captured = &import_path[start..end];
        } else if import_path != pat.prefix {
            // Literal pattern must match exactly.
            continue;
        }

        let mut out: Vec<String> = Vec::new();
        for target in &pat.replacements {
            // TS `String.replace('*', captured)` replaces the FIRST `*` only.
            let filled = if pat.has_wildcard {
                target.replacen('*', captured, 1)
            } else {
                target.clone()
            };
            // baseUrl is absolute; produce a path relative to projectRoot
            let absolute = lexical_resolve(&aliases.base_url, &filled);
            let relative =
                relative_lexical(&lexical_resolve(Path::new(""), project_root), &absolute);
            // Skip if the rewrite escapes the project root (unsafe + can't
            // be looked up via the file index anyway).
            if relative.starts_with("..") {
                continue;
            }
            out.push(relative.replace('\\', "/"));
        }
        return out;
    }
    Vec::new()
}

/// Lexical equivalent of Node's `path.relative(from, to)` for paths that
/// were both produced by [`lexical_resolve`] against the same base: walks
/// off the common component prefix and joins `..` segments for whatever
/// remains of `from`. (Shared with `import_resolver`'s
/// compile_commands.json -I directory normalization.)
pub(crate) fn relative_lexical(from: &Path, to: &Path) -> String {
    let from_comps: Vec<_> = from.components().collect();
    let to_comps: Vec<_> = to.components().collect();
    let mut common = 0;
    while common < from_comps.len()
        && common < to_comps.len()
        && from_comps[common] == to_comps[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_comps.len() {
        parts.push("..".to_string());
    }
    for comp in &to_comps[common..] {
        match comp {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {
                // Different roots (e.g. different Windows drives): no
                // relative form exists; mirror Node by returning `to`.
                return to.to_string_lossy().into_owned();
            }
            _ => {}
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn strip_jsonc_removes_comments_and_trailing_commas() {
        let src = r#"{
  // line comment
  "a": 1, /* block
  comment */
  "b": [1, 2,],
  "url": "https://cdn.example.com", // keep the URL intact
}"#;
        let cleaned = strip_jsonc(src);
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["url"], "https://cdn.example.com");
    }

    #[test]
    fn loads_wildcard_alias_and_applies_it() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    // Vite-style alias
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"],
    }
  }
}"#,
        )
        .unwrap();

        let aliases = load_project_aliases(&root).unwrap();
        assert_eq!(aliases.patterns.len(), 1);
        let pat = &aliases.patterns[0];
        assert_eq!(pat.prefix, "@/");
        assert_eq!(pat.suffix, "");
        assert!(pat.has_wildcard);
        assert_eq!(pat.replacements, vec!["src/*".to_string()]);

        let candidates = apply_aliases("@/components/Foo", &aliases, &root);
        assert_eq!(candidates, vec!["src/components/Foo".to_string()]);
    }

    #[test]
    fn literal_alias_must_match_exactly() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {"lib": ["src/lib/index.ts"]}}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert_eq!(
            apply_aliases("lib", &aliases, &root),
            vec!["src/lib/index.ts".to_string()]
        );
        // `lib2` starts with the literal prefix but isn't an exact match.
        assert!(apply_aliases("lib2", &aliases, &root).is_empty());
    }

    #[test]
    fn multiple_replacements_keep_tsconfig_priority_order() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r##"{"compilerOptions": {"paths": {"#/*": ["src/*", "generated/*"]}}}"##,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert_eq!(
            apply_aliases("#/x", &aliases, &root),
            vec!["src/x".to_string(), "generated/x".to_string()]
        );
    }

    #[test]
    fn more_specific_prefix_wins() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {
                "@/*": ["src/*"],
                "@/special/*": ["special-src/*"]
            }}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        // Longer prefix sorted first.
        assert_eq!(aliases.patterns[0].prefix, "@/special/");
        assert_eq!(
            apply_aliases("@/special/thing", &aliases, &root),
            vec!["special-src/thing".to_string()]
        );
        assert_eq!(
            apply_aliases("@/other/thing", &aliases, &root),
            vec!["src/other/thing".to_string()]
        );
    }

    #[test]
    fn honours_base_url() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "./app", "paths": {"@/*": ["lib/*"]}}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert_eq!(
            apply_aliases("@/util", &aliases, &root),
            vec!["app/lib/util".to_string()]
        );
    }

    #[test]
    fn rewrites_escaping_project_root_are_skipped() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {"@/*": ["../outside/*"]}}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert!(apply_aliases("@/x", &aliases, &root).is_empty());
    }

    #[test]
    fn falls_back_to_jsconfig() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("jsconfig.json"),
            r#"{"compilerOptions": {"paths": {"~/*": ["app/*"]}}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert_eq!(
            apply_aliases("~/c", &aliases, &root),
            vec!["app/c".to_string()]
        );
    }

    #[test]
    fn returns_none_without_config_or_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        assert!(load_project_aliases(&root).is_none());

        // tsconfig without a paths block → None too.
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"baseUrl": "."}}"#,
        )
        .unwrap();
        assert!(load_project_aliases(&root).is_none());
    }

    #[test]
    fn returns_none_for_empty_or_non_string_targets() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {"@/*": [], "%/*": [42]}}}"#,
        )
        .unwrap();
        assert!(load_project_aliases(&root).is_none());
    }

    #[test]
    fn no_match_returns_empty() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions": {"paths": {"@/*": ["src/*"]}}}"#,
        )
        .unwrap();
        let aliases = load_project_aliases(&root).unwrap();
        assert!(apply_aliases("react", &aliases, &root).is_empty());
    }
}
