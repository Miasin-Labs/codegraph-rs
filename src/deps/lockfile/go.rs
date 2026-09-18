//! `go.mod` (+ `go.sum` for pre-1.17 modules).
//!
//! Since Go 1.17 `go.mod` lists every module the build needs, marking the
//! ones only other modules import `// indirect`. Older modules list just the
//! direct requirements, so their remaining modules come from `go.sum` (the
//! highest version of each module whose source, not just `go.mod`, was
//! downloaded — what minimal version selection would pick). `replace`
//! directives redirect a module to another module version or to a local
//! directory (a path dependency, left to the atlas).

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

#[derive(Debug, Clone)]
struct Require {
    module: String,
    version: String,
    indirect: bool,
}

#[derive(Debug, Clone)]
enum Replacement {
    Module { module: String, version: String },
    Dir(String),
}

/// Parse `go_mod` (with `go_sum`, when present) at `lockfile`.
pub(crate) fn parse(go_mod: &str, go_sum: Option<&str>, lockfile: &str) -> Vec<ResolvedDep> {
    let mut requires: Vec<Require> = Vec::new();
    // (old module, optional old version) → replacement
    let mut replaces: Vec<(String, Option<String>, Replacement)> = Vec::new();
    let mut go_version: Option<(u32, u32)> = None;
    let mut block: Option<&str> = None;

    for raw in go_mod.lines() {
        let (code, comment) = match raw.split_once("//") {
            Some((code, comment)) => (code.trim(), Some(comment.trim())),
            None => (raw.trim(), None),
        };
        if code.is_empty() {
            continue;
        }
        if let Some(open) = block {
            if code == ")" {
                block = None;
                continue;
            }
            directive(open, code, comment, &mut requires, &mut replaces);
            continue;
        }
        let (verb, rest) = code.split_once(char::is_whitespace).unwrap_or((code, ""));
        let rest = rest.trim();
        match verb {
            "go" => go_version = parse_go_directive(rest),
            "require" | "replace" if rest == "(" => {
                block = Some(if verb == "require" {
                    "require"
                } else {
                    "replace"
                });
            }
            "require" | "replace" => directive(verb, rest, comment, &mut requires, &mut replaces),
            "exclude" | "retract" | "tool" | "ignore" | "godebug" if rest == "(" => {
                block = Some("skip")
            }
            _ => {}
        }
    }

    // Pre-1.17 go.mod lists only direct requirements.
    let complete_graph = go_version.is_some_and(|v| v >= (1, 17));
    if !complete_graph {
        if let Some(sum) = go_sum {
            let listed: Vec<String> = requires.iter().map(|r| r.module.clone()).collect();
            for (module, version) in highest_sum_versions(sum) {
                if !listed.contains(&module) {
                    requires.push(Require {
                        module,
                        version,
                        indirect: true,
                    });
                }
            }
        }
    }

    requires
        .into_iter()
        .map(|req| {
            let replacement = replaces
                .iter()
                .find(|(old, old_version, _)| {
                    *old == req.module && old_version.as_ref().is_none_or(|v| *v == req.version)
                })
                .map(|(_, _, r)| r.clone());
            let (name, version, source) = match replacement {
                Some(Replacement::Module { module, version }) => {
                    (module, version, DepSource::Registry)
                }
                Some(Replacement::Dir(dir)) => {
                    (req.module, req.version, DepSource::Path { path: Some(dir) })
                }
                None => (req.module, req.version, DepSource::Registry),
            };
            ResolvedDep {
                key: DepKey::for_source(Ecosystem::Go, &name, &version, &source),
                lock_version: version,
                direct: match source {
                    DepSource::Path { .. } => None,
                    _ => Some(!req.indirect),
                },
                source,
                lockfile: lockfile.to_string(),
                install_path: None,
            }
        })
        .collect()
}

fn directive(
    verb: &str,
    rest: &str,
    comment: Option<&str>,
    requires: &mut Vec<Require>,
    replaces: &mut Vec<(String, Option<String>, Replacement)>,
) {
    match verb {
        "require" => {
            let mut words = rest.split_whitespace().map(unquote);
            if let (Some(module), Some(version)) = (words.next(), words.next()) {
                requires.push(Require {
                    module,
                    version,
                    indirect: comment
                        .is_some_and(|c| c.split(';').any(|part| part.trim() == "indirect")),
                });
            }
        }
        "replace" => {
            let Some((old, new)) = rest.split_once("=>") else {
                return;
            };
            let mut old_words = old.split_whitespace().map(unquote);
            let Some(old_module) = old_words.next() else {
                return;
            };
            let old_version = old_words.next();
            let mut new_words = new.split_whitespace().map(unquote);
            let Some(target) = new_words.next() else {
                return;
            };
            let replacement = match new_words.next() {
                Some(version) => Replacement::Module {
                    module: target,
                    version,
                },
                None => Replacement::Dir(target),
            };
            replaces.push((old_module, old_version, replacement));
        }
        _ => {}
    }
}

fn unquote(word: &str) -> String {
    word.trim_matches(|c| c == '"' || c == '`').to_string()
}

fn parse_go_directive(rest: &str) -> Option<(u32, u32)> {
    let mut parts = rest.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts
        .next()
        .and_then(|m| m.trim().parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}

/// Highest version per module among go.sum's source (non-`/go.mod`) lines.
fn highest_sum_versions(sum: &str) -> Vec<(String, String)> {
    let mut best: HashMap<String, String> = HashMap::new();
    for line in sum.lines() {
        let mut words = line.split_whitespace();
        let (Some(module), Some(version)) = (words.next(), words.next()) else {
            continue;
        };
        if version.ends_with("/go.mod") {
            continue;
        }
        match best.get(module) {
            Some(current) if compare_versions(current, version) != Ordering::Less => {}
            _ => {
                best.insert(module.to_string(), version.to_string());
            }
        }
    }
    let mut out: Vec<(String, String)> = best.into_iter().collect();
    out.sort();
    out
}

/// Semantic-version order for Go versions (`v1.2.3`, `v1.2.3-pre`,
/// pseudo-versions, `+incompatible`): numeric core first, a pre-release
/// below its release, pre-releases compared as strings.
pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<&str>) {
        let v = v.trim_start_matches('v');
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        (
            core.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let (core_a, pre_a) = split(a);
    let (core_b, pre_b) = split(b);
    core_a.cmp(&core_b).then_with(|| match (pre_a, pre_b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => x.cmp(y),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_MOD: &str = "module github.com/majd/ipatool/v2\n\ngo 1.23.0\n\nrequire (\n\tgithub.com/99designs/keyring v1.2.1\n\tgithub.com/avast/retry-go v3.0.0+incompatible\n\tgolang.org/x/net v0.38.0 // indirect\n\tgithub.com/old/thing v1.0.0\n\tgithub.com/local/lib v0.1.0\n)\n\nrequire github.com/BurntSushi/toml v1.2.3 // indirect; for tests\n\nreplace github.com/old/thing => github.com/new/thing v1.1.0\n\nreplace (\n\tgithub.com/local/lib => ../lib\n)\n\nexclude (\n\tgithub.com/bad/mod v0.0.1\n)\n";

    #[test]
    fn requires_indirect_and_replacements() {
        let deps = parse(GO_MOD, None, "go.mod");
        assert_eq!(deps.len(), 6);
        let find = |n: &str| deps.iter().find(|d| d.key.name == n).unwrap();
        assert_eq!(find("github.com/99designs/keyring").direct, Some(true));
        assert_eq!(find("golang.org/x/net").direct, Some(false));
        assert_eq!(find("github.com/BurntSushi/toml").direct, Some(false));
        assert_eq!(
            find("github.com/avast/retry-go").lock_version,
            "v3.0.0+incompatible"
        );
        let replaced = find("github.com/new/thing");
        assert_eq!(replaced.lock_version, "v1.1.0");
        assert_eq!(replaced.direct, Some(true));
        let local = find("github.com/local/lib");
        assert_eq!(
            local.source,
            DepSource::Path {
                path: Some("../lib".into())
            }
        );
        assert!(deps.iter().all(|d| d.key.name != "github.com/bad/mod"));
    }

    #[test]
    fn pre_117_modules_take_the_rest_from_go_sum() {
        let go_mod = "module github.com/evanw/esbuild\n\ngo 1.13\n\nrequire golang.org/x/sys v0.0.0-20220715151400-c0bba94af5f8\n";
        let go_sum = "golang.org/x/sys v0.0.0-20220715151400-c0bba94af5f8 h1:a=\ngolang.org/x/sys v0.0.0-20220715151400-c0bba94af5f8/go.mod h1:b=\ngithub.com/a/b v1.0.0 h1:c=\ngithub.com/a/b v1.2.0 h1:d=\ngithub.com/a/b v1.10.0/go.mod h1:e=\n";
        let deps = parse(go_mod, Some(go_sum), "go.mod");
        assert_eq!(deps.len(), 2);
        let sys = deps
            .iter()
            .find(|d| d.key.name == "golang.org/x/sys")
            .unwrap();
        assert_eq!(sys.direct, Some(true));
        let ab = deps
            .iter()
            .find(|d| d.key.name == "github.com/a/b")
            .unwrap();
        assert_eq!(ab.lock_version, "v1.2.0");
        assert_eq!(ab.direct, Some(false));
    }

    #[test]
    fn version_order() {
        assert_eq!(compare_versions("v1.10.0", "v1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("v1.0.0-rc1", "v1.0.0"), Ordering::Less);
        assert_eq!(
            compare_versions("v2.0.0+incompatible", "v2.0.0"),
            Ordering::Equal
        );
    }
}
