use super::super::{DslOp, Expr, ParseError, SetOp, Token, lex};
use super::op::parse_op;
use super::path::{parse_multi_path_query, parse_path_query};
use super::selectors::{
    parse_dominates_query,
    parse_dominators_query,
    parse_entrypoint_query,
    parse_trait_impls_query,
};

// ---------------------------------------------------------------------------
// Extended expression parser: set algebra, path patterns, entrypoint selector.
//
// Grammar (left-associative, single precedence level for set ops):
//
//     expr     := atom (('union' | 'intersect' | 'diff' | '\') atom)*
//     atom     := '(' expr ')'
//                | path_query
//                | entrypoint_query
//                | pipe_chain
//
// The pipe-chain parser is reused unchanged for back-compat. Set-ops only
// preserve the `nodes` set across operands; per-operand metadata is dropped.
// ---------------------------------------------------------------------------

/// Parse a top-level [`Expr`] from a query string.
///
/// Use this entry point when callers want set algebra, path patterns, or
/// the `entrypoints` selector. For pure pipe-chain back-compat, prefer
/// [`parse_query`] which returns `Vec<DslOp>` directly.
pub fn parse_expr(input: &str) -> Result<Expr, ParseError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Err(ParseError::new(0, "empty query"));
    }
    let mut pos = 0;
    let expr = parse_expr_inner(&tokens, &mut pos)?;
    if pos < tokens.len() {
        return Err(ParseError::new(
            pos,
            format!("trailing tokens after expression: {:?}", &tokens[pos..]),
        ));
    }
    Ok(expr)
}

fn parse_expr_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    // Recursion guard — nested set-op / atom expression nesting depth.
    crate::ensure_sufficient_stack(|| parse_expr_inner_inner(tokens, pos))
}

fn parse_expr_inner_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    let mut left = parse_atom(tokens, pos)?;
    loop {
        if *pos >= tokens.len() {
            break;
        }
        let op = match &tokens[*pos] {
            Token::Union => SetOp::Union,
            Token::Intersect => SetOp::Intersect,
            Token::Diff => SetOp::Diff,
            _ => break,
        };
        *pos += 1;
        let right = parse_atom(tokens, pos)?;
        left = Expr::SetOp {
            op,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

pub(super) fn parse_atom(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    // Recursion guard — parenthesised group / sub-query operand nesting depth.
    crate::ensure_sufficient_stack(|| parse_atom_inner(tokens, pos))
}

fn parse_atom_inner(tokens: &[Token], pos: &mut usize) -> Result<Expr, ParseError> {
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected expression"));
    }
    let base = match &tokens[*pos] {
        Token::LParen => {
            *pos += 1;
            let inner = parse_expr_inner(tokens, pos)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err(ParseError::new(*pos, "expected ')' to close group"));
            }
            *pos += 1;
            inner
        }
        Token::Path | Token::Paths => parse_path_query(tokens, pos)?,
        Token::Entrypoints => parse_entrypoint_query(tokens, pos)?,
        Token::Dominators => parse_dominators_query(tokens, pos)?,
        Token::Dominates => parse_dominates_query(tokens, pos)?,
        Token::TraitImpls => parse_trait_impls_query(tokens, pos)?,
        Token::MultiPath => parse_multi_path_query(tokens, pos)?,
        _ => return parse_pipe_chain_atom(tokens, pos).map(Expr::Pipe),
    };

    // If the atom is followed by `| ops...`, absorb them as a PipeFrom.
    // This enables `entrypoints kind=PublicApi | untested | depth 3`.
    if *pos < tokens.len() && tokens[*pos] == Token::Pipe {
        let mut trailing_ops = Vec::new();
        while *pos < tokens.len() && tokens[*pos] == Token::Pipe {
            *pos += 1;
            trailing_ops.push(parse_op(tokens, pos)?);
        }
        Ok(Expr::PipeFrom {
            base: Box::new(base),
            ops: trailing_ops,
        })
    } else {
        Ok(base)
    }
}

/// Parse a pipe-chain that is also a sub-expression. Stops at end-of-input
/// or any token that does not belong inside a pipe chain (set-op keyword,
/// closing paren, end of path-query operand). This is the same operator
/// soup as legacy [`parse`] but bounded so the outer expression parser can
/// continue.
fn parse_pipe_chain_atom(tokens: &[Token], pos: &mut usize) -> Result<Vec<DslOp>, ParseError> {
    let start = *pos;
    let mut ops = Vec::new();
    loop {
        if *pos >= tokens.len() {
            break;
        }
        // Stop at any token that ends a pipe-chain in the extended grammar.
        if matches!(
            tokens[*pos],
            Token::Union
                | Token::Intersect
                | Token::Diff
                | Token::RParen
                | Token::RBrace
                | Token::Comma
                | Token::Arrow
                | Token::Where
                | Token::Via
                | Token::Of
        ) {
            break;
        }
        let op = parse_op(tokens, pos)?;
        ops.push(op);
        if *pos < tokens.len() && tokens[*pos] == Token::Pipe {
            *pos += 1;
            if *pos >= tokens.len() {
                return Err(ParseError::new(*pos, "expected operation after '|'"));
            }
        } else {
            break;
        }
    }
    if ops.is_empty() {
        return Err(ParseError::new(start, "expected pipe-chain operator"));
    }
    Ok(ops)
}
