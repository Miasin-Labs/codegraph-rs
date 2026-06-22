use super::super::{DslOp, ParseError, Token};
use super::op_advanced::{parse_affected, parse_cluster, parse_hot, parse_reachable, parse_since};
use super::op_arguments::{
    parse_depth,
    parse_filter,
    parse_select_fn,
    parse_select_type,
    parse_show,
    parse_taint,
};

pub(super) fn parse_op(tokens: &[Token], pos: &mut usize) -> Result<DslOp, ParseError> {
    let token = &tokens[*pos];
    match token {
        Token::Fn => parse_select_fn(tokens, pos),
        Token::Type => parse_select_type(tokens, pos),
        Token::Callers => {
            *pos += 1;
            Ok(DslOp::Callers)
        }
        Token::Callees => {
            *pos += 1;
            Ok(DslOp::Callees)
        }
        Token::Depth => parse_depth(tokens, pos),
        Token::Filter => parse_filter(tokens, pos),
        Token::Show => parse_show(tokens, pos),
        Token::Taint => parse_taint(tokens, pos),
        Token::Preconditions => {
            *pos += 1;
            Ok(DslOp::Preconditions)
        }
        Token::Since => parse_since(tokens, pos),
        Token::Hot => parse_hot(tokens, pos),
        Token::Scc => {
            *pos += 1;
            Ok(DslOp::Scc)
        }
        Token::Dispatch => {
            *pos += 1;
            Ok(DslOp::Dispatch)
        }
        Token::Untested => {
            *pos += 1;
            Ok(DslOp::Untested)
        }
        Token::PossibleTypes => {
            *pos += 1;
            Ok(DslOp::PossibleTypes)
        }
        Token::CoChanges => {
            *pos += 1;
            Ok(DslOp::CoChanges)
        }
        Token::Communities => {
            *pos += 1;
            Ok(DslOp::Communities)
        }
        Token::Complexity => {
            *pos += 1;
            Ok(DslOp::Complexity)
        }
        Token::Cfg => {
            *pos += 1;
            Ok(DslOp::Cfg)
        }
        Token::Dataflow => {
            *pos += 1;
            Ok(DslOp::Dataflow)
        }
        Token::Reachable => parse_reachable(tokens, pos),
        Token::Cluster => parse_cluster(tokens, pos),
        Token::Affected => parse_affected(tokens, pos),
        Token::Ident(s) => Err(ParseError::new(
            *pos,
            format!(
                "unknown operation '{s}'. Valid operations: fn, type, callers, callees, depth, filter, show, taint, preconditions, since, hot, scc, dispatch, cluster, affected, co_changes, reachable"
            ),
        )),
        _ => Err(ParseError::new(
            *pos,
            format!(
                "unexpected token {:?}. Expected an operation (fn, type, callers, callees, depth, filter, show, taint, preconditions, since, hot, scc, dispatch, cluster, affected, co_changes, reachable)",
                token
            ),
        )),
    }
}
