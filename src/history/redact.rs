//! Credential redaction for everything the history store keeps.
//!
//! [`redact`] masks every credential-shaped value in a string: all
//! occurrences, keys matched case-insensitively. It runs over raw adapter
//! strings *before* anything is derived from them, and again over each
//! derived value, so no raw secret reaches a row (see
//! [`super::ToolEvent::from_raw`]).
//!
//! The rules are ordered: structural shapes first (PEM blocks, URL userinfo,
//! headers, `sudo -S` pipes, CLI flags, `KEY=value` assignments), then
//! vendor token prefixes, JWTs and email local parts, and last a
//! high-entropy sweep for long base64/hex runs no rule named. Every
//! replacement contains `<REDACTED`, which every rule leaves as it is, so
//! redaction is idempotent: `redact(redact(x).0) == (redact(x).0, false)`.
//!
//! Known limit: a standard-base64 secret containing `/` is judged as
//! `/`-separated pieces (so file paths aren't masked), and escapes the
//! entropy sweep when every piece is shorter than 32 characters.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Replacement for a masked value.
const MASK: &str = "<REDACTED>";

/// Mask credential-shaped values. Returns `(masked, was_redacted)`.
///
/// Deliberately conservative about bare short flags so benign
/// `cargo -p crate` survives; `-p` is only treated as a password where the
/// tool is known to take one there (`sshpass`, `mysql`).
pub fn redact(input: &str) -> (String, bool) {
    let mut text = Cow::Borrowed(input);
    let mut hit = false;
    for rule in RULES.iter() {
        // A rule may rewrite a match to itself (an already-masked value), so
        // compare instead of trusting `Cow::Owned` as "changed".
        if let Cow::Owned(next) = rule.apply(&text) {
            if next != *text {
                text = Cow::Owned(next);
                hit = true;
            }
        }
    }
    if let Some(masked) = mask_high_entropy(&text) {
        text = Cow::Owned(masked);
        hit = true;
    }
    (text.into_owned(), hit)
}

/// How a rule rewrites a match.
enum Replace {
    /// A `regex` replacement template (`${name}` group references).
    Template(&'static str),
    /// Computed from the captures.
    With(fn(&Captures<'_>) -> String),
}

struct Rule {
    re: Regex,
    replace: Replace,
}

impl Rule {
    fn template(pattern: &str, template: &'static str) -> Self {
        Self {
            re: compile(pattern),
            replace: Replace::Template(template),
        }
    }

    fn with(pattern: &str, f: fn(&Captures<'_>) -> String) -> Self {
        Self {
            re: compile(pattern),
            replace: Replace::With(f),
        }
    }

    /// `Cow::Owned` iff something matched.
    fn apply<'t>(&self, text: &'t str) -> Cow<'t, str> {
        match &self.replace {
            Replace::Template(t) => self.re.replace_all(text, *t),
            Replace::With(f) => self.re.replace_all(text, *f),
        }
    }
}

fn compile(pattern: &str) -> Regex {
    // The patterns are compile-time constants covered by this module's tests;
    // failing to compile one is a programming error, not an input condition.
    Regex::new(pattern).unwrap_or_else(|e| panic!("invalid redaction pattern {pattern:?}: {e}"))
}

/// Quoted or bare value: `"…"`, `'…'`, or a run up to whitespace/quote.
/// An unterminated quote (a truncated command) runs to the end: fail closed.
const VALUE: &str = r#"(?:"[^"]*"?|'[^']*'?|[^\s"']+)"#;

/// Authorization schemes kept readable in front of a masked credential.
const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "token", "digest", "negotiate", "apikey"];

/// Key words that name a secret when they end an assignment key.
const SECRET_KEY: &str = r"(?:password|passwd|passphrase|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret|auth[_-]?token|credentials?|sshpass|mysql_pwd)";

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // PEM private-key blocks, to their END marker (or the end of text
        // when truncated). Newlines may be real or `\n`-escaped.
        Rule::template(
            r"(?s)-----BEGIN[A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END[A-Z0-9 ]*PRIVATE KEY-----|\z)",
            "<REDACTED_PRIVATE_KEY>",
        ),
        // `scheme://user:pass@host` — keep scheme and user, mask the password.
        Rule::template(
            r#"(?i)\b(?P<pre>[a-z][a-z0-9+.\-]*://[^\s/:@"'<>]+:)[^\s/@"'<>]+@"#,
            "${pre}<REDACTED>@",
        ),
        // `Authorization: <scheme> <credential>` (also `=`-style), keeping the scheme.
        Rule::with(
            r#"(?i)\b(?P<key>(?:proxy-)?authorization\s*[:=]\s*)(?:(?P<scheme>bearer|basic|token|digest|negotiate|apikey)\s+)?(?P<val>[^\s"'<>,;]+)"#,
            |c| match c.name("scheme") {
                Some(s) => format!("{}{} {MASK}", &c["key"], s.as_str()),
                // `Authorization: Bearer <REDACTED>` backtracks to "no scheme,
                // credential = Bearer"; leave an already-masked header alone.
                None if AUTH_SCHEMES.contains(&c["val"].to_ascii_lowercase().as_str()) => {
                    c[0].to_owned()
                }
                None => format!("{}{MASK}", &c["key"]),
            },
        ),
        // API-key style headers.
        Rule::template(
            r#"(?i)\b(?P<key>(?:x-api-key|api-key|x-auth-token|x-access-token|private-token|x-goog-api-key)\s*:\s*)[^\s"'<>]+"#,
            "${key}<REDACTED>",
        ),
        // `Bearer <credential>` anywhere.
        Rule::template(
            r"(?i)\b(?P<key>bearer\s+)[A-Za-z0-9._~+/=\-]{6,}",
            "${key}<REDACTED>",
        ),
        // `echo <pw> | sudo -S …`: whatever feeds sudo's stdin is the password.
        // `-S` is case-sensitive (`sudo -s` is a shell, not stdin).
        Rule::template(
            r#"(?m)(?P<lead>^|[;&|(\n"'`\s]|\$\()(?P<cmd>echo|printf|cat|yes)\b[^|;&\n]*?(?P<pipe>\s*\|\s*sudo\b[^|;&\n]*?\s(?:-[A-Za-z]*S[A-Za-z]*\b|--stdin\b))"#,
            "${lead}${cmd} <REDACTED>${pipe}",
        ),
        // `sudo -S … <<< pw`.
        Rule::template(
            &format!(
                r"(?P<pre>\bsudo\b[^|;&\n]*?\s(?:-[A-Za-z]*S[A-Za-z]*|--stdin)\b[^|;&\n]*?<<<\s*){VALUE}"
            ),
            "${pre}<REDACTED>",
        ),
        // `sshpass -p pw` / `sshpass -ppw`.
        Rule::template(
            &format!(r"(?P<pre>\bsshpass\s+(?:-[A-Za-oq-z]\S*\s+)*-p)(?P<sep>\s*){VALUE}"),
            "${pre}${sep}<REDACTED>",
        ),
        // MySQL-family `-p<pw>` (attached value; a bare `-p` just prompts).
        Rule::template(
            r#"(?P<pre>\b(?:mysql|mysqldump|mysqladmin|mysqlimport|mysqlshow|mysqlcheck|mysqlsh|mariadb|mariadb-dump)\b[^|;&\n]*?\s-p)(?:"[^"]*"?|'[^']*'?|[^\s"']+)"#,
            "${pre}<REDACTED>",
        ),
        // `--password pw`, `--token=pw`, … (case-insensitive flag names).
        Rule::template(
            &format!(
                r"(?i)(?P<flag>--(?:password|passwd|pass|passphrase|token|secret|api-key|api_key|apikey|access-token|auth-token|client-secret|secret-key|private-key|key-password|db-password))(?P<sep>\s*=\s*|\s+){VALUE}"
            ),
            "${flag}${sep}<REDACTED>",
        ),
        // `KEY=value` where KEY ends in a secret word (`PGPASSWORD=`, `GITHUB_TOKEN=`, `Password=`).
        Rule::template(
            &format!(
                r#"(?i)(?P<key>\b[A-Za-z0-9_.\-]*{SECRET_KEY})(?P<sep>\s*=\s*)(?:"[^"]*"?|'[^']*'?|[^\s"'&;|<>]+)"#
            ),
            "${key}${sep}<REDACTED>",
        ),
        // JSON-style `"password": "value"` (optionally backslash-escaped
        // quotes). A value starting with `<` is already masked.
        Rule::template(
            &format!(
                r#"(?i)(?P<key>\\?"[A-Za-z0-9_.\-]*(?:{SECRET_KEY}|authorization)\\?"\s*:\s*)\\?"(?:[^"\\<]|\\[^"])(?:[^"\\]|\\[^"])*\\?""#
            ),
            r#"${key}"<REDACTED>""#,
        ),
        // YAML-style `password: value`.
        Rule::template(
            r#"(?i)(?P<key>\b[A-Za-z0-9_.\-]*(?:password|passwd|secret|api[_-]?key|client[_-]?secret)\s*:\s+)[^\s"',}<>]+"#,
            "${key}<REDACTED>",
        ),
        // Vendor token prefixes.
        Rule::template(r"\bsk-(?:ant-|proj-|live-|test-)?[A-Za-z0-9_\-]{16,}", MASK),
        Rule::template(r"\b[rs]k_(?:live|test)_[A-Za-z0-9]{16,}", MASK),
        Rule::template(
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})",
            MASK,
        ),
        Rule::template(r"\bglpat-[A-Za-z0-9_\-]{20,}", MASK),
        Rule::template(r"\bxox[abposr]-[A-Za-z0-9\-]{10,}", MASK),
        Rule::template(
            r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[A-Z0-9]{16}\b",
            MASK,
        ),
        Rule::template(r"\bAIza[0-9A-Za-z_\-]{35}", MASK),
        Rule::template(r"\bnpm_[A-Za-z0-9]{36}\b", MASK),
        Rule::template(r"\bhf_[A-Za-z0-9]{30,}", MASK),
        // JWTs and other base64-JSON blobs (`eyJ` is base64 for `{"`).
        Rule::template(
            r"\beyJ[A-Za-z0-9_\-]{10,}(?:\.[A-Za-z0-9_\-=]*){0,2}",
            "<REDACTED_JWT>",
        ),
        // Email addresses: mask the local part, keep the domain.
        Rule::template(
            r"\b[A-Za-z0-9._%+\-]+@(?P<domain>[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?)*\.[A-Za-z]{2,})\b",
            "<REDACTED>@${domain}",
        ),
    ]
});

// ─── high-entropy sweep ──────────────────────────────────────────────────────

/// Shortest token the entropy sweep considers.
const MIN_TOKEN: usize = 32;

/// A byte of a base64/base64url/hex token. `/` is excluded so a file path
/// is judged segment by segment; `=` only counts as trailing padding.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'_' | b'-')
}

/// Mask long tokens that look random. `None` when nothing matched.
fn mask_high_entropy(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !is_token_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_token_byte(bytes[i]) {
            i += 1;
        }
        let end = i;
        while i < bytes.len() && i - end < 2 && bytes[i] == b'=' {
            i += 1;
        }
        if end - start >= MIN_TOKEN && looks_random(&text[start..end]) {
            spans.push((start, i));
        }
    }
    if spans.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (s, e) in spans {
        out.push_str(&text[last..s]);
        out.push_str(MASK);
        last = e;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// Digits and letters mixed, not a `-`/`_`-joined identifier, and near the
/// entropy ceiling for its length.
///
/// Calibrated on random base64/alnum samples (entropy ≥ ~0.79 of the
/// ceiling at 32–64 chars) against identifiers such as
/// `x86_64-unknown-linux-gnu`, UUIDs and `-home-…-agent-ab9f361c283d489cf`
/// project dirs (≤ ~0.76, and made of word-like pieces). A pure-hex token
/// only needs 3 bits/char: its alphabet is 16 symbols.
fn looks_random(s: &str) -> bool {
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = s.bytes().any(|b| b.is_ascii_alphabetic());
    if !has_digit || !has_alpha {
        return false;
    }
    let h = shannon_entropy(s);
    if s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return h >= 3.0;
    }
    let mut pieces = s.split(['-', '_']).filter(|p| !p.is_empty());
    if s.contains(['-', '_']) && pieces.all(is_wordish) {
        return false;
    }
    h >= 0.77 * (s.len().min(64) as f64).log2()
}

/// A piece of a kebab/snake identifier: a one-case word or id (`x86`,
/// `release`, `ab9f361c283d489cf`) or a `CamelCase` word (`RustProjects`).
fn is_wordish(piece: &str) -> bool {
    let b = piece.as_bytes();
    let one_case = b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        || b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if one_case {
        return b.len() <= 20;
    }
    b.len() <= 24
        && b[0].is_ascii_uppercase()
        && b.iter().all(u8::is_ascii_alphanumeric)
        && !b
            .windows(2)
            .any(|w| w[0].is_ascii_uppercase() && w[1].is_ascii_uppercase())
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests;
