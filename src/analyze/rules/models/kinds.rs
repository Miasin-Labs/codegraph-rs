//! Model kinds as rules select them: CodeQL's names (`sql-injection`,
//! `remote`), CodeQL's threat-model groups for sources (`local`,
//! `remote`, `all`), and short aliases for sink classes (`sql`, `command`,
//! `path`, `xss`, `ssrf`…).

use super::Role;

/// Source threat models by group (CodeQL's `threatModelGrouping`).
const SOURCE_GROUPS: &[(&str, &[&str])] = &[
    ("remote", &["remote", "request", "response"]),
    (
        "local",
        &[
            "database",
            "commandargs",
            "environment",
            "stdin",
            "file",
            "windows-registry",
        ],
    ),
];

/// Sink classes by alias: codegraph's short names for CodeQL's kinds.
const SINK_ALIASES: &[(&str, &[&str])] = &[
    ("sql", &["sql-injection"]),
    ("nosql", &["nosql-injection"]),
    ("command", &["command-injection"]),
    ("path", &["path-injection"]),
    ("xss", &["html-injection", "js-injection"]),
    ("ssrf", &["request-forgery", "request-url"]),
    ("redirect", &["url-redirection", "url-forward"]),
    ("ldap", &["ldap-injection"]),
    ("xpath", &["xpath-injection"]),
    ("log", &["log-injection"]),
    (
        "code",
        &["code-injection", "groovy-injection", "mvel-injection"],
    ),
    (
        "template",
        &["template-injection", "ognl-injection", "jexl-injection"],
    ),
    ("deserialization", &["unsafe-deserialization"]),
    ("jndi", &["jndi-injection"]),
    ("response-splitting", &["response-splitting"]),
    ("xxe", &["xxe", "xslt-injection"]),
];

/// The kinds `selector` stands for in `role`: the selector itself (a
/// CodeQL kind; C++ models also name a `local` kind) plus, for a group or
/// alias, its members. `all`/`any` is every kind.
pub fn expand(role: Role, selector: &str) -> Vec<String> {
    let selector = selector.trim();
    let table: &[(&str, &[&str])] = match role {
        Role::Source => SOURCE_GROUPS,
        _ => SINK_ALIASES,
    };
    let mut out = vec![selector.to_string()];
    if let Some((_, members)) = table.iter().find(|(name, _)| *name == selector) {
        for member in *members {
            if !out.iter().any(|m| m == member) {
                out.push(member.to_string());
            }
        }
    }
    out
}

/// Whether a model of `kind` answers any of `selectors` (already
/// [`expand`]ed; `all`/`any` match everything).
pub fn matches(selectors: &[String], kind: &str) -> bool {
    selectors
        .iter()
        .any(|s| s == kind || s == "all" || s == "any")
}

/// Every alias and group, for help and errors.
pub fn known_selectors(role: Role) -> Vec<&'static str> {
    match role {
        Role::Source => SOURCE_GROUPS.iter().map(|(name, _)| *name).collect(),
        _ => SINK_ALIASES.iter().map(|(name, _)| *name).collect(),
    }
}
