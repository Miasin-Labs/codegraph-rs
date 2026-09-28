//! The rules-driven lowering of Rust: values of blocks, `if` and `match`,
//! pattern bindings, macros and method calls.

use super::tests::lower;
use crate::ir::{IrFunction, IrOp, Operand, Var};

fn calls(ir: &IrFunction) -> Vec<(String, Option<Operand>, Vec<Operand>)> {
    ir.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Call {
                callee,
                receiver,
                args,
                ..
            } => Some((callee.clone(), receiver.clone(), args.clone())),
            _ => None,
        })
        .collect()
}

/// Every source of `var`'s assignments.
fn sources_of(ir: &IrFunction, var: &str) -> Vec<Operand> {
    ir.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Assign { dst, src } if dst.as_str() == var => Some(src.clone()),
            _ => None,
        })
        .collect()
}

fn returns(ir: &IrFunction) -> Vec<Option<Operand>> {
    ir.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Return { value } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn params_receiver_and_tuple_struct_patterns() {
    let ir = lower(
        "rust",
        "impl H { fn f(&self, Path(id): Path<String>, _: u8, mut n: u32) {} }",
    );
    assert_eq!(ir.receiver, Some(Var::new("self")));
    assert_eq!(
        ir.params,
        vec![Var::new("id"), Var::new("__param1"), Var::new("n")]
    );
}

#[test]
fn format_macros_read_their_arguments_and_placeholders() {
    let ir = lower(
        "rust",
        "fn f(a: &str, b: &str) -> String { let q = format!(\"x {} {b:?} {{c}}\", a.trim()); q }",
    );
    let format = calls(&ir)
        .into_iter()
        .find(|(callee, _, _)| callee == "format!")
        .expect("format! call");
    // The format string reads `b`; the second group reads `a` (not `trim`).
    assert_eq!(format.2.len(), 2);
    let reads: Vec<String> = ir
        .body
        .iter()
        .filter_map(|op| match op {
            IrOp::Call { callee, args, .. } if callee.starts_with('<') => Some(args.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|operand| match operand {
            Operand::Var(var) => Some(var.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert!(reads.contains(&"a".to_string()), "{reads:?}");
    assert!(reads.contains(&"b".to_string()), "{reads:?}");
    assert!(!reads.contains(&"trim".to_string()));
    assert!(!reads.contains(&"c".to_string()));
    // The block's last expression is the function's value.
    assert_eq!(returns(&ir).first(), Some(&Some(Operand::var("q"))));
}

#[test]
fn if_and_match_have_values_and_patterns_bind() {
    let ir = lower(
        "rust",
        "fn f(o: Option<String>, r: Result<u8, E>) -> String {\n\
         let v = match r { Ok(n) if n > 1 => n, Err(e) => { log(e); 0 } };\n\
         let s = if let Some(x) = o { x } else { String::new() };\n\
         s\n}",
    );
    // `Ok(n)` binds `n` from the scrutinee; `Ok` itself binds nothing.
    assert_eq!(sources_of(&ir, "n"), vec![Operand::var("r")]);
    assert_eq!(sources_of(&ir, "e"), vec![Operand::var("r")]);
    assert!(sources_of(&ir, "Ok").is_empty());
    assert_eq!(sources_of(&ir, "x"), vec![Operand::var("o")]);
    // `v` and `s` take the match's and the if's temporaries.
    assert!(
        matches!(sources_of(&ir, "v").as_slice(), [Operand::Var(t)] if t.as_str().starts_with("__t"))
    );
    assert!(
        matches!(sources_of(&ir, "s").as_slice(), [Operand::Var(t)] if t.as_str().starts_with("__t"))
    );
    assert!(calls(&ir).iter().any(|(callee, _, _)| callee == "log"));
}

#[test]
fn method_calls_keep_their_receiver() {
    let ir = lower(
        "rust",
        "fn f(p: String) { let n = p.parse::<u32>(); Command::new(\"sh\").arg(p).spawn(); }",
    );
    let calls = calls(&ir);
    let parse = calls
        .iter()
        .find(|(c, _, _)| c == "p.parse")
        .expect("parse");
    assert_eq!(parse.1, Some(Operand::var("p")));
    let arg = calls
        .iter()
        .find(|(c, _, _)| c.ends_with(".arg"))
        .expect("arg");
    assert_eq!(arg.2, vec![Operand::var("p")]);
    assert!(
        calls
            .iter()
            .any(|(c, r, _)| c == "Command::new" && r.is_none())
    );
}

#[test]
fn let_else_and_for_patterns_bind() {
    let ir = lower(
        "rust",
        "fn f(v: Vec<(String, u8)>, o: Option<String>) {\n\
         let Some(x) = o else { return; };\n\
         for (k, n) in v { use_it(k, x); }\n}",
    );
    assert_eq!(sources_of(&ir, "x"), vec![Operand::var("o")]);
    assert_eq!(sources_of(&ir, "k").len(), 1);
    assert!(sources_of(&ir, "Some").is_empty());
}

#[test]
fn placeholders_skip_escapes_and_positions() {
    use super::values::format_placeholders;
    assert_eq!(
        format_placeholders("\"{a} {0} {{b}} {c:>4} {} {d.e}\""),
        vec!["a".to_string(), "c".to_string()]
    );
}
