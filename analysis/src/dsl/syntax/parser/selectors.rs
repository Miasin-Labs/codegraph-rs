use super::super::{Expr, ParseError, Token};
use super::expr::parse_atom;
use super::kind::parse_entrypoint_kind;

/// `dominators of <atom>` — `of` is required even though it's purely
/// connective, because `dominators fn("foo")` would otherwise let the
/// pipe-chain parser eat `dominators` as an unknown operator. Keeping
/// `of` explicit also leaves room for future `dominators since N` etc.
pub(super) fn parse_dominators_query(
    tokens: &[Token],
    pos: &mut usize,
) -> Result<Expr, ParseError> {
    // Recursion guard — nested `dominators of <atom>` operand depth.
    crate::ensure_sufficient_stack(|| parse_dominators_query_inner(tokens, pos))
}

fn parse_dominators_query_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    debug_assert!(matches!(tokens[*pos], Token::Dominators));
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Of {
        return Err(ParseError::new(*pos, "expected 'of' after 'dominators'"));
    }
    *pos += 1;
    let inner = parse_atom(tokens, pos)?;
    Ok(Expr::DominatorsOf(Box::new(inner)))
}

/// `dominates <atom>` — no `of` here because `dominates` is naturally a
/// transitive verb (`X dominates Y`). Mirrors the petgraph API name.
pub(super) fn parse_dominates_query(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    // Recursion guard — nested `dominates <atom>` operand depth.
    crate::ensure_sufficient_stack(|| parse_dominates_query_inner(tokens, pos))
}

fn parse_dominates_query_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    debug_assert!(matches!(tokens[*pos], Token::Dominates));
    *pos += 1;
    let inner = parse_atom(tokens, pos)?;
    Ok(Expr::DominatesOf(Box::new(inner)))
}

/// `trait_impls of <atom>` — `<atom>` should select `Trait` nodes; non-trait
/// nodes contribute no implementors and are silently skipped at execution.
pub(super) fn parse_trait_impls_query(
    tokens: &[Token],
    pos: &mut usize,
) -> Result<Expr, ParseError> {
    // Recursion guard — nested `trait_impls of <atom>` operand depth.
    crate::ensure_sufficient_stack(|| parse_trait_impls_query_inner(tokens, pos))
}

fn parse_trait_impls_query_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    debug_assert!(matches!(tokens[*pos], Token::TraitImpls));
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Of {
        return Err(ParseError::new(*pos, "expected 'of' after 'trait_impls'"));
    }
    *pos += 1;
    let inner = parse_atom(tokens, pos)?;
    Ok(Expr::TraitImplsOf(Box::new(inner)))
}

pub(super) fn parse_entrypoint_query(
    tokens: &[Token],
    pos: &mut usize,
) -> Result<Expr, ParseError> {
    debug_assert!(matches!(tokens[*pos], Token::Entrypoints));
    *pos += 1;
    // Optional `kind=Main|PublicApi|Test` filter.
    if *pos < tokens.len() && tokens[*pos] == Token::Kind {
        *pos += 1;
        if *pos >= tokens.len() || tokens[*pos] != Token::Equals {
            return Err(ParseError::new(*pos, "expected '=' after 'kind'"));
        }
        *pos += 1;
        if *pos >= tokens.len() {
            return Err(ParseError::new(*pos, "expected entrypoint kind"));
        }
        let kind = match &tokens[*pos] {
            Token::Ident(s) => parse_entrypoint_kind(s, *pos)?,
            _ => return Err(ParseError::new(*pos, "expected entrypoint kind identifier")),
        };
        *pos += 1;
        Ok(Expr::Entrypoints(Some(kind)))
    } else {
        Ok(Expr::Entrypoints(None))
    }
}
