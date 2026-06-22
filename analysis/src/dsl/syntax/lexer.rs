use super::{ParseError, Token};

pub fn lex(input: &str) -> Result<Vec<Token>, ParseError> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut pos = 0;
    let mut tokens = Vec::new();

    while pos < len {
        if bytes[pos].is_ascii_whitespace() {
            pos += 1;
            continue;
        }

        // Unicode arrow `→` (3 bytes: 0xE2 0x86 0x92). Detect before falling
        // through to the byte-by-byte ASCII match — `→` is the natural
        // notation in the documented path-query grammar.
        if pos + 2 < len && bytes[pos] == 0xE2 && bytes[pos + 1] == 0x86 && bytes[pos + 2] == 0x92 {
            tokens.push(Token::Arrow);
            pos += 3;
            continue;
        }

        match bytes[pos] {
            b'|' => {
                tokens.push(Token::Pipe);
                pos += 1;
            }
            b'=' => {
                tokens.push(Token::Equals);
                pos += 1;
            }
            b'(' => {
                tokens.push(Token::LParen);
                pos += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                pos += 1;
            }
            b'\\' => {
                tokens.push(Token::Diff);
                pos += 1;
            }
            b'{' => {
                tokens.push(Token::LBrace);
                pos += 1;
            }
            b'}' => {
                tokens.push(Token::RBrace);
                pos += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                pos += 1;
            }
            b'-' if pos + 1 < len && bytes[pos + 1] == b'>' => {
                tokens.push(Token::Arrow);
                pos += 2;
            }
            b'"' => {
                let start = pos;
                pos += 1;
                let content_start = pos;
                while pos < len && bytes[pos] != b'"' {
                    pos += 1;
                }
                if pos >= len {
                    return Err(ParseError::new(start, "unterminated string literal"));
                }
                let content = &input[content_start..pos];
                tokens.push(Token::String(content.to_string()));
                pos += 1;
            }
            b'0'..=b'9' => {
                let start = pos;
                while pos < len && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                let num_str = &input[start..pos];
                let num: usize = num_str
                    .parse()
                    .map_err(|_| ParseError::new(start, format!("invalid number: {num_str}")))?;
                tokens.push(Token::Number(num));
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = pos;
                while pos < len && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                    pos += 1;
                }
                let word = &input[start..pos];

                match word {
                    "fn" | "type" => {
                        let kw_token = if word == "fn" { Token::Fn } else { Token::Type };
                        // Peek ahead for '(' — if absent, `type` is a plain
                        // ident (e.g. `cluster by type`), not a selector.
                        let mut peek = pos;
                        while peek < len && bytes[peek].is_ascii_whitespace() {
                            peek += 1;
                        }
                        if peek >= len || bytes[peek] != b'(' {
                            if word == "type" {
                                // Not a type("...") selector — emit as ident
                                // so `cluster by type` parses correctly.
                                tokens.push(Token::Ident("type".to_string()));
                                continue;
                            }
                            return Err(ParseError::new(
                                pos,
                                format!("expected '(' after '{word}'"),
                            ));
                        }
                        // Consume the whitespace we peeked past.
                        pos = peek;
                        pos += 1;

                        while pos < len && bytes[pos].is_ascii_whitespace() {
                            pos += 1;
                        }

                        if pos >= len || bytes[pos] != b'"' {
                            return Err(ParseError::new(
                                pos,
                                format!("expected string argument for '{word}'"),
                            ));
                        }
                        pos += 1;
                        let content_start = pos;
                        while pos < len && bytes[pos] != b'"' {
                            pos += 1;
                        }
                        if pos >= len {
                            return Err(ParseError::new(
                                content_start - 1,
                                "unterminated string literal",
                            ));
                        }
                        let content = &input[content_start..pos];
                        pos += 1;

                        while pos < len && bytes[pos].is_ascii_whitespace() {
                            pos += 1;
                        }
                        if pos >= len || bytes[pos] != b')' {
                            return Err(ParseError::new(
                                pos,
                                format!("expected ')' after string in '{word}(...)'"),
                            ));
                        }
                        pos += 1;

                        tokens.push(kw_token);
                        tokens.push(Token::String(content.to_string()));
                    }
                    "callers" => tokens.push(Token::Callers),
                    "callees" => tokens.push(Token::Callees),
                    "depth" => tokens.push(Token::Depth),
                    "filter" => tokens.push(Token::Filter),
                    "show" => tokens.push(Token::Show),
                    "taint" => tokens.push(Token::Taint),
                    "preconditions" => tokens.push(Token::Preconditions),
                    "kind" => tokens.push(Token::Kind),
                    "union" => tokens.push(Token::Union),
                    "intersect" => tokens.push(Token::Intersect),
                    "diff" => tokens.push(Token::Diff),
                    "path" => tokens.push(Token::Path),
                    "paths" => tokens.push(Token::Paths),
                    "where" => tokens.push(Token::Where),
                    "intermediate" => tokens.push(Token::Intermediate),
                    "via" => tokens.push(Token::Via),
                    "entrypoints" => tokens.push(Token::Entrypoints),
                    // Postfix history filter: `since N` — see `DslOp::Since`.
                    // Sits at the same grammar level as `depth N` (postfix,
                    // takes a single numeric argument) so it can chain after
                    // any selector or traversal.
                    "since" => tokens.push(Token::Since),
                    // Centrality / structural / type-cluster operators —
                    // see `DslOp::{Hot,Scc,Dominators,...}` for the full
                    // wiring story. Each is a single-keyword token that the
                    // pipe-chain parser dispatches on.
                    "hot" => tokens.push(Token::Hot),
                    "scc" => tokens.push(Token::Scc),
                    "dominators" => tokens.push(Token::Dominators),
                    "dominates" => tokens.push(Token::Dominates),
                    "of" => tokens.push(Token::Of),
                    "trait_impls" => tokens.push(Token::TraitImpls),
                    "dispatch" => tokens.push(Token::Dispatch),
                    "cluster" => tokens.push(Token::Cluster),
                    "by" => tokens.push(Token::By),
                    "affected" => tokens.push(Token::Affected),
                    "multi_path" => tokens.push(Token::MultiPath),
                    "untested" => tokens.push(Token::Untested),
                    "possible_types" => tokens.push(Token::PossibleTypes),
                    "co_changes" => tokens.push(Token::CoChanges),
                    "communities" => tokens.push(Token::Communities),
                    "complexity" => tokens.push(Token::Complexity),
                    "cfg" => tokens.push(Token::Cfg),
                    "dataflow" => tokens.push(Token::Dataflow),
                    "reachable" => tokens.push(Token::Reachable),
                    _ => tokens.push(Token::Ident(word.to_string())),
                }
            }
            other => {
                return Err(ParseError::new(
                    pos,
                    format!("unexpected character: '{}'", other as char),
                ));
            }
        }
    }

    Ok(tokens)
}
