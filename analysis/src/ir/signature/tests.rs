//! Receivers, parameter positions and method-call receivers, lowered from
//! real source in each IR language.

use tree_sitter::{Language, Node, Parser};

use crate::ir::{IrFunction, IrOp, Operand, Var, lower_for_language};

/// Lower every function-like node of `src`.
fn lower_all(lang_id: &str, grammar: Language, src: &str) -> Vec<IrFunction> {
    let mut parser = Parser::new();
    parser.set_language(&grammar).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        out.extend(lower_for_language(lang_id, node, src));
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

fn find<'f>(functions: &'f [IrFunction], name: &str) -> &'f IrFunction {
    functions.iter().find(|f| f.name == name).unwrap()
}

fn vars(names: &[&str]) -> Vec<Var> {
    names.iter().map(|n| Var::new(*n)).collect()
}

type CallView<'f> = (&'f str, Option<&'f Operand>, &'f [Operand]);

/// `(callee, receiver, args)` of every call op, in body order.
fn calls(f: &IrFunction) -> Vec<CallView<'_>> {
    f.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Call {
                callee,
                receiver,
                args,
                ..
            } => Some((callee.as_str(), receiver.as_ref(), args.as_slice())),
            _ => None,
        })
        .collect()
}

#[test]
fn rust_receiver_is_self_and_method_calls_carry_it() {
    let src = "impl A {
        fn m(&mut self, mut x: i32, (a, b): (u8, u8), #[allow(unused)] y: u8) {
            self.f.g::<u8>(x); Foo::new(y); crate::a::f(/* why */ x); make().h();
            k(&x, &mut y, (a), b?);
        }
        fn boxed(self: Box<Self>, z: u8) {}
    }";
    let functions = lower_all("rust", tree_sitter_rust::LANGUAGE.into(), src);
    let m = find(&functions, "m");
    assert_eq!(m.receiver, Some(Var::new("self")));
    assert_eq!(m.params, vars(&["x", "(a, b)", "y"]));
    let boxed = find(&functions, "boxed");
    assert_eq!(boxed.receiver, Some(Var::new("self")));
    assert_eq!(boxed.params, vars(&["z"]));

    let calls = calls(m);
    // `self.f` is read into a temp, and the temp is the receiver.
    let (callee, receiver, args) = calls[0];
    assert_eq!(callee, "self.f.g::<u8>");
    let Some(Operand::Var(recv)) = receiver else {
        panic!("receiver {receiver:?}")
    };
    assert!(m.body.iter().any(|op| matches!(
        op,
        IrOp::FieldRead { dst, base: Operand::Var(base), field }
            if dst == recv && base.as_str() == "self" && field == "f"
    )));
    assert_eq!(args, [Operand::var("x")]);
    assert_eq!((calls[1].0, calls[1].1), ("Foo::new", None));
    assert_eq!(calls[2].1, None);
    assert_eq!(
        calls[2].2,
        [Operand::var("x")],
        "the comment is no argument"
    );
    // `make().h()` lowers the inner call; its result is the receiver.
    assert_eq!(calls[3].0, "make");
    assert_eq!(calls[4].0, "make().h");
    assert!(calls[4].1.is_some());
    // Borrows, parentheses and `?` pass the value itself.
    assert_eq!(
        calls[5].2,
        [
            Operand::var("x"),
            Operand::var("y"),
            Operand::var("a"),
            Operand::var("b")
        ]
    );
}

#[test]
fn python_receiver_is_first_parameter_of_a_bound_method() {
    let src = "\
class C:
    @staticmethod
    def s(x: int, y=1, *args, z: int = 2, **kw):
        pass
    @classmethod
    def c(cls, a):
        pass
    def m(self, a, /, b, *, c):
        self.helper(a)
        C.m(self, a)

def free(self, a):
    pass

def plain(a, b):
    pass
";
    let functions = lower_all("python", tree_sitter_python::LANGUAGE.into(), src);
    let s = find(&functions, "s");
    assert_eq!(s.receiver, None);
    assert_eq!(s.params, vars(&["x", "y", "args", "z", "kw"]));
    let c = find(&functions, "c");
    assert_eq!(c.receiver, Some(Var::new("cls")));
    assert_eq!(c.params, vars(&["a"]));
    let m = find(&functions, "m");
    assert_eq!(m.receiver, Some(Var::new("self")));
    assert_eq!(m.params, vars(&["a", "b", "c"]));
    assert_eq!(find(&functions, "free").receiver, Some(Var::new("self")));
    let plain = find(&functions, "plain");
    assert_eq!(plain.receiver, None);
    assert_eq!(plain.params, vars(&["a", "b"]));

    let calls = calls(m);
    let self_var = Operand::var("self");
    assert_eq!(
        calls[0],
        ("self.helper", Some(&self_var), &[Operand::var("a")][..])
    );
    assert_eq!(calls[1].1, Some(&Operand::var("C")));
    assert_eq!(calls[1].2, [Operand::var("self"), Operand::var("a")]);
}

#[test]
fn typescript_receiver_is_this_for_instance_methods() {
    let src = "class C {
        static s(x: number) {}
        m(this: C, a?: number, b = 1, ...r: number[]) { this.x.y(a); a?.b(1); }
        p(public q: number, /* note */ { k }: T) {}
    }
    function f(this: Window, x) {}";
    let functions = lower_all(
        "typescript",
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        src,
    );
    let s = find(&functions, "s");
    assert_eq!(s.receiver, None);
    assert_eq!(s.params, vars(&["x"]));
    let m = find(&functions, "m");
    assert_eq!(m.receiver, Some(Var::new("this")));
    assert_eq!(m.params, vars(&["a", "b", "r"]));
    assert_eq!(find(&functions, "p").params, vars(&["q", "{ k }"]));
    let f = find(&functions, "f");
    assert_eq!(f.receiver, Some(Var::new("this")));
    assert_eq!(f.params, vars(&["x"]));

    let calls = calls(m);
    assert_eq!(calls[0].0, "this.x.y");
    assert!(matches!(calls[0].1, Some(Operand::Var(_))));
    assert!(m.body.iter().any(|op| matches!(
        op,
        IrOp::FieldRead { base: Operand::Var(base), field, .. }
            if base.as_str() == "this" && field == "x"
    )));
    assert_eq!(calls[1].1, Some(&Operand::var("a")));
}

#[test]
fn go_receiver_and_every_parameter_name() {
    let src = "package p
func (s *S) M(a, b int, rest ...string) { s.x.N(a); pkg.F(b) }
func (S) U(v int) {}";
    let functions = lower_all("go", tree_sitter_go::LANGUAGE.into(), src);
    let m = find(&functions, "M");
    assert_eq!(m.receiver, Some(Var::new("s")));
    assert_eq!(m.params, vars(&["a", "b", "rest"]));
    let u = find(&functions, "U");
    assert_eq!(u.receiver, None);
    assert_eq!(u.params, vars(&["v"]));

    let calls = calls(m);
    assert_eq!(calls[0].0, "s.x.N");
    assert!(calls[0].1.is_some());
    let pkg = Operand::var("pkg");
    assert_eq!(calls[1], ("pkg.F", Some(&pkg), &[Operand::var("b")][..]));
}
