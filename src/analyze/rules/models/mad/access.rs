//! CodeQL access paths (`Argument[0].Element`, `ReturnValue.Field[x]`,
//! `Argument[this]`, `Argument[*1]`, `Argument[0,path:]`) onto codegraph's
//! [`Pos`]itions.
//!
//! The first token names the position; codegraph's taint tracks whole
//! values (with fields only through its own lowering), so content below it
//! — elements, map keys/values, fields, references, futures — is collapsed
//! onto the position and the model marked approximate. Anything that is
//! not a position of the call itself is dropped, with the reason counted:
//! parameters (of a callback, or of an overriding method), a callback's
//! arguments or result, CodeQL-only barrier/step markers.

use super::super::Pos;

/// One `Name[argument]` token of an access path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub name: &'a str,
    pub arg: Option<&'a str>,
}

/// The tokens of `path`, split on `.` outside brackets.
pub fn tokens(path: &str) -> Vec<Token<'_>> {
    fn push<'a>(piece: &'a str, out: &mut Vec<Token<'a>>) {
        let piece = piece.trim();
        if piece.is_empty() {
            return;
        }
        let (name, arg) = match piece.find('[') {
            Some(open) if piece.ends_with(']') => {
                (&piece[..open], Some(&piece[open + 1..piece.len() - 1]))
            }
            _ => (piece, None),
        };
        out.push(Token { name, arg });
    }
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let bytes = path.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'.' if depth == 0 => {
                push(&path[start..i], &mut out);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&path[start..], &mut out);
    out
}

/// Content tokens collapsed onto the position above them.
const CONTENT: &[&str] = &[
    "Element",
    "ArrayElement",
    "ListElement",
    "SetElement",
    "TupleElement",
    "DictionaryElement",
    "DictionaryElementAny",
    "MapKey",
    "MapValue",
    "Field",
    "SyntheticField",
    "Attribute",
    "Union",
    "Reference",
    "Future",
    "Awaited",
    "Instance",
    "Member",
    "AnyMember",
    "WithElement",
    "WithoutElement",
    "WithArity",
];

/// A mapped access path: its positions (one per alternative), whether
/// content was collapsed, and which tokens were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub positions: Vec<Pos>,
    pub collapsed: Vec<&'static str>,
    /// Alternatives left out (keyword-only arguments, `any`).
    pub skipped: usize,
}

/// Map an access path onto positions, or say why it cannot be.
pub fn map(path: &str) -> Result<Mapped, String> {
    let tokens = tokens(path);
    let Some(first) = tokens.first() else {
        return Err("empty access path".into());
    };
    let mut skipped = 0usize;
    let positions: Vec<Pos> = match first.name {
        "ReturnValue" => vec![Pos::Ret],
        "Qualifier" => vec![Pos::Recv],
        "Argument" => {
            let (positions, left_out) = arguments(first.arg.unwrap_or_default())?;
            skipped += left_out;
            positions
        }
        "Parameter" => {
            return Err("a parameter (of an overriding method or callback)".into());
        }
        other => return Err(format!("access `{other}` is not a call position")),
    };
    if positions.is_empty() {
        return Err("only keyword or `any` arguments".into());
    }
    let mut collapsed = Vec::new();
    for token in &tokens[1..] {
        if let Some(name) = CONTENT.iter().find(|name| **name == token.name) {
            if !collapsed.contains(name) {
                collapsed.push(*name);
            }
            continue;
        }
        return Err(match token.name {
            "Parameter" | "Argument" | "ReturnValue" => {
                "a callback's parameter, argument or result".into()
            }
            "OptionalBarrier" | "OptionalStep" => "a CodeQL-configurable step".into(),
            other => format!("access `{other}` below a position"),
        });
    }
    Ok(Mapped {
        positions,
        collapsed,
        skipped,
    })
}

/// `Argument[…]`'s positions: `0`, `this`/`self`/`-1`/`receiver`, C++
/// indirections `*0`/`**1`/`@0` (the pointee — codegraph marks the storage the
/// argument names), ranges `0..2` and `1..`, Python/JS keyword
/// alternatives `0,path:` (kept on the positional one). Returns the
/// positions and how many alternatives were left out.
fn arguments(spec: &str) -> Result<(Vec<Pos>, usize), String> {
    let mut out: Vec<Pos> = Vec::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut left_out = 0usize;
    let push = |pos: Pos, out: &mut Vec<Pos>| {
        if !out.contains(&pos) {
            out.push(pos);
        }
    };
    for part in spec.split(',') {
        // `*0`/`**1` (C++ indirections) and `@0` (any indirection).
        let part = part.trim().trim_start_matches(['*', '@']);
        if part.is_empty() {
            continue;
        }
        match part {
            "this" | "self" | "-1" | "receiver" => push(Pos::Recv, &mut out),
            "any" | "any-named" => left_out += 1,
            _ if part.ends_with(':') => keywords.push(part.trim_end_matches(':').to_string()),
            _ => {
                if let Some((from, to)) = part.split_once("..") {
                    let from: u8 = from
                        .parse()
                        .map_err(|_| format!("argument range `{part}`"))?;
                    if to.is_empty() {
                        push(Pos::ArgsFrom(from), &mut out);
                    } else {
                        let to: u8 = to.parse().map_err(|_| format!("argument range `{part}`"))?;
                        for n in from..=to {
                            push(Pos::arg(n), &mut out);
                        }
                    }
                } else {
                    let n: u8 = part
                        .parse()
                        .map_err(|_| format!("argument `{part}` is not a position"))?;
                    push(Pos::arg(n), &mut out);
                }
            }
        }
    }
    // `Argument[0,path:]`: the keyword names the positional argument
    // before it (CodeQL's Python/JS spelling of one parameter).
    let positional: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, pos)| matches!(pos, Pos::Arg { .. }))
        .map(|(i, _)| i)
        .collect();
    if positional.len() == keywords.len() {
        for (index, keyword) in positional.into_iter().zip(keywords) {
            if let Pos::Arg { keyword: slot, .. } = &mut out[index] {
                *slot = Some(keyword);
            }
        }
    } else {
        left_out += keywords.len();
    }
    Ok((out, left_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_outside_brackets() {
        let t = tokens("ReturnValue.Field[core::result::Result::Ok(0)].Element");
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].name, "Field");
        assert_eq!(t[1].arg, Some("core::result::Result::Ok(0)"));
        let t = tokens("Member[a.b].Argument[0]");
        assert_eq!(t[0].arg, Some("a.b"));
    }

    #[test]
    fn positions_map_and_content_collapses() {
        assert_eq!(map("Argument[0]").unwrap().positions, vec![Pos::arg(0)]);
        assert_eq!(map("Argument[this]").unwrap().positions, vec![Pos::Recv]);
        assert_eq!(map("Argument[self]").unwrap().positions, vec![Pos::Recv]);
        assert_eq!(map("Argument[-1]").unwrap().positions, vec![Pos::Recv]);
        assert_eq!(map("Argument[*1]").unwrap().positions, vec![Pos::arg(1)]);
        assert_eq!(map("Argument[**8]").unwrap().positions, vec![Pos::arg(8)]);
        assert_eq!(
            map("Argument[0..2]").unwrap().positions,
            vec![Pos::arg(0), Pos::arg(1), Pos::arg(2)]
        );
        assert_eq!(
            map("Argument[1..]").unwrap().positions,
            vec![Pos::ArgsFrom(1)]
        );
        assert_eq!(map("ReturnValue[*]").unwrap().positions, vec![Pos::Ret]);
        let mapped = map("Argument[this].MapValue.Element").unwrap();
        assert_eq!(mapped.positions, vec![Pos::Recv]);
        assert_eq!(mapped.collapsed, vec!["MapValue", "Element"]);
        let mapped = map("ReturnValue.Field[core::option::Option::Some(0)]").unwrap();
        assert_eq!(mapped.collapsed, vec!["Field"]);
    }

    #[test]
    fn keywords_name_their_positional_argument() {
        let mapped = map("Argument[0,path:]").unwrap();
        assert_eq!(
            mapped.positions,
            vec![Pos::Arg {
                n: 0,
                keyword: Some("path".into())
            }]
        );
        let mapped = map("Argument[0,new_root:,1,pathname:]").unwrap();
        assert_eq!(mapped.positions.len(), 2);
        assert_eq!(
            mapped.positions[1],
            Pos::Arg {
                n: 1,
                keyword: Some("pathname".into())
            }
        );
        // Keyword-only.
        assert!(map("Argument[text:]").is_err());
    }

    #[test]
    fn non_positions_are_dropped_with_a_reason() {
        assert!(map("Parameter[0]").unwrap_err().contains("parameter"));
        assert!(
            map("Argument[1].Parameter[0]")
                .unwrap_err()
                .contains("callback")
        );
        assert!(
            map("Argument[0].ReturnValue")
                .unwrap_err()
                .contains("callback")
        );
        assert!(
            map("Argument[0].OptionalBarrier[x]")
                .unwrap_err()
                .contains("configurable")
        );
        assert!(map("Argument[foo]").is_err());
    }
}
