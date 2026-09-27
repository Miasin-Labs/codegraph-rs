//! What a function returns, read from the index's signature text: whether
//! its result carries a value worth using.

use super::rules::Rules;

/// A function's declared result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Returns {
    /// A value (`-> Result<LocalIdentity>`, `: number`).
    Value(String),
    /// Nothing worth using (`()`, `Result<()>`, `void`, `&mut Self`).
    Unit,
    /// Not declared where that means "unknown" (TS/JS, Python).
    Unknown,
}

/// Classify the result of a function with `signature` under `rules`.
pub(super) fn returns(rules: &Rules, signature: Option<&str>) -> Returns {
    let Some(signature) = signature else {
        return Returns::Unknown;
    };
    let Some(declared) = declared_return(signature) else {
        return if rules.undeclared_return_is_unit {
            Returns::Unit
        } else {
            Returns::Unknown
        };
    };
    if is_unit(rules, &declared) {
        Returns::Unit
    } else {
        Returns::Value(declared)
    }
}

/// The return type text after the parameter list: `-> T` or `: T`.
fn declared_return(signature: &str) -> Option<String> {
    // The parameter list opens at the first `(` outside generics (`<F: Fn()
    // -> u8>(f: F)`); the `>` of an arrow closes nothing.
    let mut angle = 0usize;
    let mut previous = ' ';
    let mut open = None;
    for (at, ch) in signature.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' if previous != '-' && previous != '=' => angle = angle.saturating_sub(1),
            '(' if angle == 0 => {
                open = Some(at);
                break;
            }
            _ => {}
        }
        previous = ch;
    }
    let open = open?;
    let mut depth = 0usize;
    let mut close = None;
    for (at, ch) in signature[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let rest = signature[close? + 1..].trim_start();
    let rest = rest
        .strip_prefix("->")
        .or_else(|| rest.strip_prefix(':'))?
        .trim();
    // A trailing `where` clause, body or arrow is not part of the type.
    let end = [" where", "\nwhere", "{", "=>"]
        .iter()
        .filter_map(|stop| rest.find(stop))
        .min()
        .unwrap_or(rest.len());
    let declared = rest[..end].trim();
    (!declared.is_empty()).then(|| declared.to_string())
}

fn is_unit(rules: &Rules, declared: &str) -> bool {
    let compact: String = declared.chars().filter(|ch| !ch.is_whitespace()).collect();
    if rules
        .unit_returns
        .iter()
        .any(|unit| unit.replace(' ', "") == compact)
    {
        return true;
    }
    // A mutable borrow handed back is for chaining (`&mut Self`, `&mut T`).
    if compact.starts_with("&mut") {
        return true;
    }
    // `Result<()>`, `io::Result<(), E>`, `Option<()>`, `Poll<()>`,
    // `impl Future<Output = ()>`, `Promise<void>`: a wrapper of nothing.
    let Some(open) = compact.find('<') else {
        return false;
    };
    let args = compact[open + 1..].trim_end_matches('>');
    let first = args.split(',').next().unwrap_or("");
    let first = first.strip_prefix("Output=").unwrap_or(first);
    matches!(first, "()" | "void" | "None" | "undefined")
        || args.starts_with("dynFuture<Output=()")
        || args.contains("Future<Output=()>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::bugs::deviance::rules::for_language;
    use crate::types::Language;

    #[test]
    fn classifies_declared_results() {
        let rust = for_language(Language::Rust).unwrap();
        let ts = for_language(Language::Typescript).unwrap();
        let py = for_language(Language::Python).unwrap();
        let value = |rules, sig| matches!(returns(rules, Some(sig)), Returns::Value(_));
        assert!(value(
            rust,
            "(&mut self, cloud_file: &CloudFile) -> Result<LocalIdentity>"
        ));
        assert!(value(rust, "(&self) -> bool"));
        assert!(value(rust, "<F: Fn() -> u8>(f: F) -> Option<u8>"));
        assert!(!value(rust, "(&mut self)"));
        assert!(!value(rust, "(x: u8) -> Result<()>"));
        assert!(!value(rust, "(x: u8) -> io::Result<(), Error>"));
        assert!(!value(rust, "(&mut self) -> &mut Self"));
        assert!(!value(rust, "(x: u8) -> impl Future<Output = ()>"));
        assert_eq!(returns(rust, Some("(x: u8)")), Returns::Unit);
        assert!(value(rust, "(x: u8) -> Vec<T>\nwhere T: Clone"));

        assert!(value(ts, "(a: string): number"));
        assert!(!value(ts, "(a: string): Promise<void>"));
        assert_eq!(returns(ts, Some("(a)")), Returns::Unknown);
        assert!(value(py, "(a, b) -> int"));
        assert!(!value(py, "(a) -> None"));
        assert_eq!(returns(py, Some("(a)")), Returns::Unknown);
    }
}
