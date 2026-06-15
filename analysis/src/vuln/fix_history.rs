//! Fix-history rule synthesis: learn predicate signatures from the project's
//! own bug fixes.
//!
//! The insight that ties the history flywheel to bug-finding: **a fix commit is
//! a labeled example of a rule.** When a fix *adds* a call (`authorize(...)`,
//! `sanitize(...)`, a `?` on a previously-dropped send), the added call is
//! evidence that it belongs on the guarded path — exactly the predicate a
//! frequency pass would have to discover the hard way. Mining "what calls were
//! added in fix commits" synthesizes guard/sanitizer signatures directly, and
//! feeds them into the [`LearnedStore`](super::learned::LearnedStore) so they
//! reinforce (or are reinforced by) the other mechanisms.
//!
//! The diff parsing is deliberately dependency-free and language-agnostic: it
//! scans added/removed lines for `identifier(` call shapes. Precise per-function
//! attribution is left to the caller that has the AST; this layer extracts the
//! statistical signal that survives across many commits.

use super::InferenceOrigin;
use super::learned::LearnedStore;

/// Commit-message tokens that mark a bug/security fix. Lowercased substring
/// match.
const FIX_KEYWORDS: &[&str] = &[
    "fix",
    "bug",
    "patch",
    "vuln",
    "security",
    "cve",
    "regression",
    "hotfix",
    "exploit",
    "overflow",
    "injection",
    "bypass",
    "leak",
    "race",
    "deadlock",
];

/// A commit considered for synthesis: its message and unified diff text.
#[derive(Debug, Clone)]
pub struct FixCommit {
    pub message: String,
    pub diff: String,
}

/// A synthesized predicate signal ready to fuse into the learned store.
#[derive(Debug, Clone, PartialEq)]
pub struct SynthesizedRole {
    /// The call identifier the fix added (or removed).
    pub name: String,
    /// `"guard"` for added checks, `"sink"` for calls a fix removed/wrapped.
    pub role: String,
    /// Confidence scaled by how many fix commits agree.
    pub confidence: f64,
    /// How many fix commits contributed this signal.
    pub support: u32,
}

/// True if a commit message reads as a bug/security fix.
pub fn is_fix_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    FIX_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Extract `identifier(`-shaped call names from diff lines with the given
/// prefix (`'+'` for added, `'-'` for removed), ignoring file headers
/// (`+++`/`---`).
fn calls_on_diff_lines(diff: &str, sign: char) -> Vec<String> {
    let header = format!("{sign}{sign}{sign}");
    let mut out = Vec::new();
    for line in diff.lines() {
        if !line.starts_with(sign) || line.starts_with(&header) {
            continue;
        }
        out.extend(extract_call_names(&line[1..]));
    }
    out
}

/// Pull `name(` call identifiers out of a single source line. Dependency-free
/// scan: collect a run of identifier chars immediately followed by `(`. Keeps
/// the last path segment of `a::b::c(` so `self.authorize(` → `authorize`.
fn extract_call_names(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < bytes.len() {
        if bytes[i] == b'(' && i > 0 {
            // Walk back over the identifier ending just before '('.
            let end = i;
            let mut start = i;
            while start > 0 && is_ident(bytes[start - 1]) {
                start -= 1;
            }
            if start < end {
                let ident = &line[start..end];
                // Skip language keywords that look like calls.
                if !ident.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && !matches!(
                        ident,
                        "if" | "while" | "for" | "match" | "switch" | "return" | "fn"
                    )
                {
                    out.push(ident.to_owned());
                }
            }
        }
        i += 1;
    }
    out
}

/// Synthesize predicate signals from a batch of commits. Only fix commits
/// contribute. Added calls become `guard` signals; calls removed in a fix
/// become weaker `sink` signals (a fix that *removes* a call hints the call was
/// hazardous). Confidence grows with cross-commit agreement (saturating).
pub fn synthesize(commits: &[FixCommit]) -> Vec<SynthesizedRole> {
    use std::collections::HashMap;
    // (name, role) -> count
    let mut counts: HashMap<(String, &'static str), u32> = HashMap::new();
    for commit in commits {
        if !is_fix_message(&commit.message) {
            continue;
        }
        // Dedup within a commit so one commit counts once per signal.
        let mut added: Vec<String> = calls_on_diff_lines(&commit.diff, '+');
        added.sort();
        added.dedup();
        for name in added {
            *counts.entry((name, "guard")).or_insert(0) += 1;
        }
        let mut removed: Vec<String> = calls_on_diff_lines(&commit.diff, '-');
        removed.sort();
        removed.dedup();
        for name in removed {
            *counts.entry((name, "sink")).or_insert(0) += 1;
        }
    }
    let mut out: Vec<SynthesizedRole> = counts
        .into_iter()
        .map(|((name, role), support)| SynthesizedRole {
            name,
            role: role.to_owned(),
            // saturating: 1 fix -> 0.4, 2 -> 0.64, 3 -> 0.78, ...
            confidence: 1.0 - 0.6_f64.powi(support as i32),
            support,
        })
        .collect();
    out.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.role.cmp(&b.role))
    });
    out
}

/// Fuse synthesized signals into the learned store under
/// [`InferenceOrigin::History`].
pub fn apply_to_store(store: &mut LearnedStore, commits: &[FixCommit], rev: u64) {
    for sig in synthesize(commits) {
        store.observe(
            &sig.name,
            &sig.role,
            InferenceOrigin::History,
            sig.confidence,
            rev,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_fix_messages() {
        assert!(is_fix_message("fix: remove panic vectors in MCP hot paths"));
        assert!(is_fix_message("security: patch SSRF in webhook fetch"));
        assert!(!is_fix_message("feat: add salesforce extractor"));
    }

    #[test]
    fn extracts_added_guard_from_a_fix_diff() {
        let commit = FixCommit {
            message: "fix: enforce authorization before delete".into(),
            diff: "\
@@ -10,6 +10,7 @@ fn delete_handler(req: Req) {
     let id = req.param(\"id\");
+    authorize(&req.user, id)?;
     delete_user(id);
 }
"
            .into(),
        };
        let roles = synthesize(&[commit]);
        // `authorize` (and `param`) are added calls -> guard signals.
        assert!(
            roles
                .iter()
                .any(|r| r.name == "authorize" && r.role == "guard"),
            "got {roles:#?}"
        );
    }

    #[test]
    fn cross_commit_agreement_raises_confidence() {
        let mk = |n: u32| FixCommit {
            message: format!("fix bug {n}"),
            diff: "+    authorize(user);\n".into(),
        };
        let one = synthesize(&[mk(1)]);
        let three = synthesize(&[mk(1), mk(2), mk(3)]);
        let c1 = one
            .iter()
            .find(|r| r.name == "authorize")
            .unwrap()
            .confidence;
        let c3 = three
            .iter()
            .find(|r| r.name == "authorize")
            .unwrap()
            .confidence;
        assert!(
            c3 > c1,
            "more agreeing fixes must raise confidence: {c1} vs {c3}"
        );
    }

    #[test]
    fn non_fix_commits_contribute_nothing() {
        let commit = FixCommit {
            message: "refactor: rename things".into(),
            diff: "+    authorize(user);\n".into(),
        };
        assert!(synthesize(&[commit]).is_empty());
    }

    #[test]
    fn feeds_learned_store_under_history_origin() {
        let mut store = LearnedStore::new();
        let commit = FixCommit {
            message: "fix: add owner check".into(),
            diff: "+    check_owner(user, doc);\n".into(),
        };
        apply_to_store(&mut store, &[commit], 7);
        assert!(store.confidence("check_owner", "guard") > 0.0);
    }
}
