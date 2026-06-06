//! Cargo Workspace Resolver Helper
//!
//! Parses a project's root Cargo.toml and member crate manifests to
//! build a crate-name -> member-directory map. Used by the Rust
//! resolver to resolve `use crate_name::...` references that point
//! into workspace member crates.
//!
//! Ported from `src/resolution/frameworks/cargo-workspace.ts`. The TS
//! glob matching used `picomatch`; here it is `globset` with
//! `literal_separator(true)` so `*` does not cross `/` (picomatch's
//! default). See `notes/frameworks-systems.md` for the deviation notes.

use std::collections::{HashMap, HashSet};

use globset::{GlobBuilder, GlobMatcher};
use regex::Regex;

use crate::resolution::types::ResolutionContext;

const GLOB_CHARS: &[char] = &['*', '?', '[', ']', '{', '}', '!'];
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "dist", "build"];
const MAX_GLOB_WALK_DEPTH: u32 = 5;

/// Return the body of `[sectionName]` (lines between the header and the
/// next `[...]` header), or `None` when the section is absent.
fn get_section(content: &str, section_name: &str) -> Option<String> {
    let header = format!("[{section_name}]");
    let mut in_section = false;
    let mut section_lines: Vec<&str> = Vec::new();

    for line in content.split('\n') {
        let trimmed = line.trim();
        if !in_section {
            if trimmed == header {
                in_section = true;
            }
            continue;
        }

        // TS: /^\[[^\]]+\]$/
        if is_table_header(trimmed) {
            break;
        }

        section_lines.push(line);
    }

    if !in_section {
        return None;
    }
    Some(section_lines.join("\n"))
}

/// TS `/^\[[^\]]+\]$/` — a whole-line `[...]` header with a non-empty,
/// `]`-free body.
fn is_table_header(trimmed: &str) -> bool {
    trimmed.len() > 2
        && trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && !trimmed[1..trimmed.len() - 1].contains(']')
}

fn extract_quoted_values(value_list: &str) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut current = String::new();

    for ch in value_list.chars() {
        match quote {
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    current.clear();
                }
            }
            Some(q) => {
                if escaped {
                    current.push(ch);
                    escaped = false;
                    continue;
                }

                if ch == '\\' {
                    escaped = true;
                    continue;
                }

                if ch == q {
                    values.push(current.trim().to_string());
                    quote = None;
                    current.clear();
                    continue;
                }

                current.push(ch);
            }
        }
    }

    values.into_iter().filter(|v| !v.is_empty()).collect()
}

/// Extract the inner text of the `[...]` array assigned to `key` within a
/// TOML section body, honoring quotes/escapes and nested brackets.
fn get_array_value(section: &str, key: &str) -> Option<String> {
    let key_regex = Regex::new(&format!(r"\b{}\b\s*=", regex::escape(key))).ok()?;
    let key_match = key_regex.find(section)?;

    let mut i = key_match.end();
    while i < section.len() {
        let ch = section[i..].chars().next()?;
        if ch.is_whitespace() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    if i >= section.len() || !section[i..].starts_with('[') {
        return None;
    }
    i += 1;

    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    let mut depth: u32 = 1;
    let start = i;

    while i < section.len() {
        let ch = section[i..].chars().next()?;
        let len = ch.len_utf8();

        if let Some(q) = in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                in_quote = None;
            }
            i += len;
            continue;
        }

        if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
            i += len;
            continue;
        }

        if ch == '[' {
            depth += 1;
            i += len;
            continue;
        }

        if ch == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(section[start..i].to_string());
            }
            i += len;
            continue;
        }

        i += len;
    }

    None
}

fn parse_workspace_members(cargo_toml: &str) -> Vec<String> {
    let Some(workspace_section) = get_section(cargo_toml, "workspace") else {
        return Vec::new();
    };
    let Some(members_value) = get_array_value(&workspace_section, "members") else {
        return Vec::new();
    };
    extract_quoted_values(&members_value)
}

fn parse_package_name(cargo_toml: &str) -> Option<String> {
    static NAME_RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r#"name\s*=\s*["']([^"'\n]+)["']"#).unwrap());
    let package_section = get_section(cargo_toml, "package")?;
    NAME_RE
        .captures(&package_section)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

fn add_crate_alias(map: &mut HashMap<String, String>, crate_name: &str, member_path: &str) {
    let normalized = crate_name.replace('-', "_");
    map.insert(crate_name.to_string(), member_path.to_string());
    if normalized != crate_name {
        map.insert(normalized, member_path.to_string());
    }
}

fn clean_path(member_path: &str) -> String {
    let replaced = member_path.replace('\\', "/");
    // TS replace(/\/$/, '') — strips ONE trailing slash.
    replaced
        .strip_suffix('/')
        .map(|s| s.to_string())
        .unwrap_or(replaced)
}

fn expand_glob_member(member: &str, context: &dyn ResolutionContext) -> Vec<String> {
    // TS guarded on `context.listDirectories` being present; in Rust the
    // trait method always exists (default returns an empty Vec), which
    // yields the same observable no-matches result.

    let first_glob_idx = member.find(GLOB_CHARS).unwrap_or(member.len());
    // TS: member.slice(0, idx).replace(/[^/]*$/, '').replace(/\/$/, '')
    let prefix = &member[..first_glob_idx];
    let static_prefix = match prefix.rfind('/') {
        Some(i) => &prefix[..i],
        None => "",
    };

    let Ok(glob) = GlobBuilder::new(member).literal_separator(true).build() else {
        return Vec::new();
    };
    let matcher = glob.compile_matcher();
    let mut matches: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    fn walk(
        dir: &str,
        depth: u32,
        context: &dyn ResolutionContext,
        matcher: &GlobMatcher,
        seen: &mut HashSet<String>,
        matches: &mut Vec<String>,
    ) {
        if depth > MAX_GLOB_WALK_DEPTH {
            return;
        }
        let children = context.list_directories(dir);
        for child in children {
            if SKIP_DIRS.contains(&child.as_str()) || child.starts_with('.') {
                continue;
            }
            let rel = if dir == "." {
                child.clone()
            } else {
                format!("{dir}/{child}")
            };
            if matcher.is_match(&rel) && !seen.contains(&rel) {
                seen.insert(rel.clone());
                matches.push(rel.clone());
            }
            walk(&rel, depth + 1, context, matcher, seen, matches);
        }
    }

    let start = if static_prefix.is_empty() {
        "."
    } else {
        static_prefix
    };
    walk(start, 0, context, &matcher, &mut seen, &mut matches);
    matches
}

fn expand_members(members: &[String], context: &dyn ResolutionContext) -> Vec<String> {
    let mut expanded: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for member in members {
        let candidates: Vec<String> = if member.contains(GLOB_CHARS) {
            expand_glob_member(member, context)
        } else {
            vec![member.clone()]
        };
        for candidate in candidates {
            let cleaned = clean_path(&candidate);
            if seen.contains(&cleaned) {
                continue;
            }
            seen.insert(cleaned.clone());
            expanded.push(cleaned);
        }
    }
    expanded
}

/// Build a map from crate-name aliases to workspace member directory paths.
/// Example: "mytool-core" and "mytool_core" -> "crates/mytool-core"
///
/// Supports glob members (e.g. `members = ["crates/*"]`) via globset
/// when the context's `list_directories` yields entries.
pub fn get_cargo_workspace_crate_map(context: &dyn ResolutionContext) -> HashMap<String, String> {
    let mut result: HashMap<String, String> = HashMap::new();
    let root_cargo_toml = match context.read_file("Cargo.toml") {
        // TS truthiness: empty string is falsy.
        Some(s) if !s.is_empty() => s,
        _ => return result,
    };

    let raw_members = parse_workspace_members(&root_cargo_toml);
    let members = expand_members(&raw_members, context);

    for member_path in members {
        let member_cargo_path = format!("{member_path}/Cargo.toml");
        let member_cargo_toml = match context.read_file(&member_cargo_path) {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };

        let Some(package_name) = parse_package_name(&member_cargo_toml) else {
            continue;
        };

        add_crate_alias(&mut result, &package_name, &member_path);
    }

    result
}
