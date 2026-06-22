use super::super::{Expr, ParseError, PathMode, PathQuery, Token};
use super::expr::parse_atom;
use super::kind::{parse_edge_kind, parse_node_kind};
use crate::edges::EdgeKind;
use crate::nodes::NodeKind;

/// `multi_path { <expr>, <expr>, ... } -> <expr>` with optional `depth N`.
///
/// Parses the brace list as one-or-more sub-expressions separated by commas.
/// We use `multi_path` rather than overloading `path { ... }` because the
/// `path` token is already deeply integrated with single-source parsing
/// and ambiguity around `path fn("a")` (single source) vs.
/// `path {fn("a")}` (one-element multi source) would surprise the LLM.
pub(super) fn parse_multi_path_query(
    tokens: &[Token],
    pos: &mut usize,
) -> Result<Expr, ParseError> {
    // Recursion guard — nested multi_path source / target operand depth.
    crate::ensure_sufficient_stack(|| parse_multi_path_query_inner(tokens, pos))
}

fn parse_multi_path_query_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    debug_assert!(matches!(tokens[*pos], Token::MultiPath));
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::LBrace {
        return Err(ParseError::new(
            *pos,
            "expected '{' after 'multi_path' to open the source list",
        ));
    }
    *pos += 1;

    let mut sources: Vec<Expr> = Vec::new();
    loop {
        if *pos >= tokens.len() {
            return Err(ParseError::new(
                *pos,
                "unterminated source list: expected ',' or '}'",
            ));
        }
        if tokens[*pos] == Token::RBrace {
            *pos += 1;
            break;
        }
        let item = parse_atom(tokens, pos)?;
        sources.push(item);
        match tokens.get(*pos) {
            Some(Token::Comma) => {
                *pos += 1;
            }
            Some(Token::RBrace) => {
                *pos += 1;
                break;
            }
            _ => {
                return Err(ParseError::new(
                    *pos,
                    "expected ',' or '}' in multi_path source list",
                ));
            }
        }
    }

    if sources.is_empty() {
        return Err(ParseError::new(
            *pos,
            "multi_path requires at least one source",
        ));
    }

    if *pos >= tokens.len() || tokens[*pos] != Token::Arrow {
        return Err(ParseError::new(
            *pos,
            "expected '->' or '→' after multi_path source list",
        ));
    }
    *pos += 1;

    let to = parse_atom(tokens, pos)?;

    let mut max_depth: Option<usize> = None;
    if *pos < tokens.len() && tokens[*pos] == Token::Depth {
        *pos += 1;
        if *pos >= tokens.len() {
            return Err(ParseError::new(*pos, "expected number after 'depth'"));
        }
        match &tokens[*pos] {
            Token::Number(n) => {
                max_depth = Some(*n);
                *pos += 1;
            }
            _ => return Err(ParseError::new(*pos, "expected number after 'depth'")),
        }
    }

    Ok(Expr::MultiPath {
        sources,
        to: Box::new(to),
        max_depth,
    })
}

pub(super) fn parse_path_query(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    // Recursion guard — nested path-query endpoint operand depth.
    crate::ensure_sufficient_stack(|| parse_path_query_inner(tokens, pos))
}

fn parse_path_query_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    let mode = match &tokens[*pos] {
        Token::Path => PathMode::Shortest,
        Token::Paths => PathMode::AllSimple,
        _ => return Err(ParseError::new(*pos, "expected 'path' or 'paths'")),
    };
    *pos += 1;

    let from = parse_atom(tokens, pos)?;

    if *pos >= tokens.len() || tokens[*pos] != Token::Arrow {
        return Err(ParseError::new(
            *pos,
            "expected '->' or '→' between path endpoints",
        ));
    }
    *pos += 1;

    let to = parse_atom(tokens, pos)?;

    let mut intermediate_kind: Option<NodeKind> = None;
    let mut via_edge: Option<EdgeKind> = None;
    let mut max_depth: Option<usize> = None;

    // Trailing qualifiers may appear in any order; loop until we run out.
    loop {
        if *pos >= tokens.len() {
            break;
        }
        match &tokens[*pos] {
            Token::Where => {
                *pos += 1;
                if *pos >= tokens.len() || tokens[*pos] != Token::Intermediate {
                    return Err(ParseError::new(
                        *pos,
                        "expected 'intermediate' after 'where'",
                    ));
                }
                *pos += 1;
                if *pos >= tokens.len() || tokens[*pos] != Token::Kind {
                    return Err(ParseError::new(
                        *pos,
                        "expected 'kind' after 'where intermediate'",
                    ));
                }
                *pos += 1;
                if *pos >= tokens.len() || tokens[*pos] != Token::Equals {
                    return Err(ParseError::new(
                        *pos,
                        "expected '=' after 'where intermediate kind'",
                    ));
                }
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err(ParseError::new(*pos, "expected node kind"));
                }
                let kind = match &tokens[*pos] {
                    Token::Ident(s) => parse_node_kind(s, *pos)?,
                    _ => return Err(ParseError::new(*pos, "expected node kind identifier")),
                };
                *pos += 1;
                intermediate_kind = Some(kind);
            }
            Token::Via => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err(ParseError::new(*pos, "expected edge kind after 'via'"));
                }
                let edge = match &tokens[*pos] {
                    Token::Ident(s) => parse_edge_kind(s, *pos)?,
                    _ => return Err(ParseError::new(*pos, "expected edge kind identifier")),
                };
                *pos += 1;
                via_edge = Some(edge);
            }
            Token::Depth => {
                *pos += 1;
                if *pos >= tokens.len() {
                    return Err(ParseError::new(*pos, "expected number after 'depth'"));
                }
                let n = match &tokens[*pos] {
                    Token::Number(n) => *n,
                    _ => return Err(ParseError::new(*pos, "expected number after 'depth'")),
                };
                *pos += 1;
                max_depth = Some(n);
            }
            _ => break,
        }
    }

    Ok(Expr::PathQuery(PathQuery {
        mode,
        from: Box::new(from),
        to: Box::new(to),
        intermediate_kind,
        via_edge,
        max_depth,
    }))
}
