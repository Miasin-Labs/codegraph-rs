use super::super::{DslOp, ParseError, Token};
use crate::closure::ClosureDirection;
use crate::label_reachability;

pub(super) fn parse_since(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected number after 'since'"));
    }
    match &tokens[*pos] {
        Token::Number(n) => {
            let rev = *n as u64;
            *pos += 1;
            Ok(DslOp::Since(rev))
        }
        _ => Err(ParseError::new(*pos, "expected number after 'since'")),
    }
}

pub(super) fn parse_hot(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected number after 'hot'"));
    }
    match &tokens[*pos] {
        Token::Number(n) => {
            let k = *n;
            *pos += 1;
            Ok(DslOp::Hot(k))
        }
        _ => Err(ParseError::new(*pos, "expected number after 'hot'")),
    }
}

pub(super) fn parse_reachable(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Via {
        return Err(ParseError::new(*pos, "expected 'via' after 'reachable'"));
    }
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(
            *pos,
            "expected pattern string after 'reachable via'",
        ));
    }
    let pattern = match &tokens[*pos] {
        Token::String(s) => label_reachability::parse_pattern(s)
            .map_err(|e| ParseError::new(*pos, e.to_string()))?,
        _ => {
            return Err(ParseError::new(
                *pos,
                "expected pattern string (e.g. \"Contains Calls+\") after 'reachable via'",
            ));
        }
    };
    *pos += 1;
    let direction = match tokens.get(*pos) {
        Some(Token::Ident(s)) if s == "incoming" => {
            *pos += 1;
            ClosureDirection::Incoming
        }
        Some(Token::Ident(s)) if s == "outgoing" => {
            *pos += 1;
            ClosureDirection::Outgoing
        }
        Some(Token::Ident(s)) => {
            return Err(ParseError::new(
                *pos,
                format!(
                    "unknown direction '{s}' after 'reachable via \"...\"'. Valid: incoming, outgoing (default)"
                ),
            ));
        }
        _ => ClosureDirection::Outgoing,
    };
    Ok(DslOp::ReachableVia { pattern, direction })
}

pub(super) fn parse_cluster(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::By {
        return Err(ParseError::new(*pos, "expected 'by' after 'cluster'"));
    }
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected 'type' after 'cluster by'"));
    }
    match &tokens[*pos] {
        Token::Type => {
            *pos += 1;
            Ok(DslOp::ClusterByType)
        }
        Token::Ident(s) if s == "type" => {
            *pos += 1;
            Ok(DslOp::ClusterByType)
        }
        _ => Err(ParseError::new(*pos, "expected 'type' after 'cluster by'")),
    }
}

pub(super) fn parse_affected(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(*pos, "expected number after 'affected'"));
    }
    let depth = match &tokens[*pos] {
        Token::Number(n) => *n,
        _ => {
            return Err(ParseError::new(
                *pos,
                "expected number (depth) after 'affected'",
            ));
        }
    };
    *pos += 1;
    if *pos >= tokens.len() || tokens[*pos] != Token::Since {
        return Err(ParseError::new(*pos, "expected 'since' after 'affected N'"));
    }
    *pos += 1;
    if *pos >= tokens.len() {
        return Err(ParseError::new(
            *pos,
            "expected number (revision) after 'affected N since'",
        ));
    }
    let since_rev = match &tokens[*pos] {
        Token::Number(n) => *n as u64,
        _ => {
            return Err(ParseError::new(
                *pos,
                "expected number after 'affected N since'",
            ));
        }
    };
    *pos += 1;
    Ok(DslOp::Affected { depth, since_rev })
}
