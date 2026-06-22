use super::super::{DslOp, ParseError, Token};
use super::kind::{parse_node_kind, parse_projection};

pub(super) fn parse_select_fn(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    parse_string_arg(tokens, pos, "fn").map(DslOp::SelectFn)
}

pub(super) fn parse_select_type(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    parse_string_arg(tokens, pos, "type").map(DslOp::SelectType)
}

pub(super) fn parse_depth(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected number after 'depth'"));
    }
    match &tokens[*pos] {
        Token::Number(n) => {
            let depth = *n;
            *pos += 1;
            Ok(DslOp::Depth(depth))
        }
        _ => Err(ParseError::new(*pos, "expected number after 'depth'")),
    }
}

pub(super) fn parse_filter(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Kind {
        return Err(ParseError::new(*pos, "expected 'kind' after 'filter'"));
    }
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Equals {
        return Err(ParseError::new(*pos, "expected '=' after 'filter kind'"));
    }
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(
            *pos,
            "expected node kind after 'filter kind='",
        ));
    }
    let kind = match &tokens[*pos] {
        Token::Ident(s) => parse_node_kind(s, *pos)?,
        _ => {
            return Err(ParseError::new(
                *pos,
                "expected node kind (Function, Struct, Enum, Module, Trait) after 'filter kind='",
            ));
        }
    };
    *pos += 1;
    Ok(DslOp::Filter(kind))
}

pub(super) fn parse_show(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(
            *pos,
            "expected projection (fields, signature, body) after 'show'",
        ));
    }
    let projection = match &tokens[*pos] {
        Token::Ident(s) => parse_projection(s, *pos)?,
        _ => {
            return Err(ParseError::new(
                *pos,
                "expected projection (fields, signature, body) after 'show'",
            ));
        }
    };
    *pos += 1;
    Ok(DslOp::Show(projection))
}

pub(super) fn parse_taint(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    parse_string_arg(tokens, pos, "taint").map(DslOp::Taint)
}

fn parse_string_arg(
    tokens: &[Token],
    pos: &mut usize,
    op_name: &str,
) -> Result<String, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(
            *pos,
            format!("expected string after '{op_name}'"),
        ));
    }
    match &tokens[*pos] {
        Token::String(s) => {
            let name = s.clone();
            *pos += 1;
            Ok(name)
        }
        _ => Err(ParseError::new(
            *pos,
            format!("expected string after '{op_name}'"),
        )),
    }
}
