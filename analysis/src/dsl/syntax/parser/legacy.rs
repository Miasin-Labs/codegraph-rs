use super::super::{DslOp, ParseError, Token, lex};
use super::op::parse_op;

pub fn parse(tokens: &[Token]) -> Result<Vec<DslOp>, ParseError> {
    if tokens.is_empty() {
        return Err(ParseError::new(0, "empty query"));
    }

    let mut ops = Vec::new();
    let mut pos = 0;

    loop {
        if pos >= tokens.len() {
            break;
        }

        let op = parse_op(tokens, &mut pos)?;
        ops.push(op);

        if pos < tokens.len() {
            if tokens[pos] == Token::Pipe {
                pos += 1;
                if pos >= tokens.len() {
                    return Err(ParseError::new(pos, "expected operation after '|'"));
                }
            } else {
                return Err(ParseError::new(
                    pos,
                    format!("expected '|' or end of query, found {:?}", tokens[pos]),
                ));
            }
        }
    }

    if ops.is_empty() {
        return Err(ParseError::new(0, "empty query"));
    }

    Ok(ops)
}

pub fn parse_query(input: &str) -> Result<Vec<DslOp>, ParseError> {
    let tokens = lex(input)?;
    parse(&tokens)
}
