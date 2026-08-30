use crate::fixture::*;

// C++ operator overloads invoked via infix (`a + b`) and subscript (`a[i]`)
// must record a `calls` edge into the operator method (#1258). The call site
// parses as `binary_expression` / `subscript_expression`; extraction now emits
// a `receiver.operator<sym>` reference, and the resolver infers the receiver's
// type (`const V& a` → `V`) and matches the operator method on it.

#[tokio::test(flavor = "current_thread")]
async fn cpp_infix_operator_call_resolves_to_operator_method() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "optest.cpp",
        "struct V {\n    int x;\n    V operator+(const V& o) const { return V{x + o.x}; }\n    int get() const { return x; }\n};\nV infixCaller(const V& a, const V& b) { return a + b; }\n",
    );
    fx.track(&q, "optest.cpp", Language::Cpp);

    let infix_caller = node(
        "func:optest:infixCaller",
        NodeKind::Function,
        "infixCaller",
        "infixCaller",
        "optest.cpp",
        Language::Cpp,
        6,
        6,
    );
    let op_plus = node(
        "method:v:operator+",
        NodeKind::Method,
        "operator+",
        "V::operator+",
        "optest.cpp",
        Language::Cpp,
        3,
        3,
    );
    // Decoy: a same-named operator on an unrelated type must NOT win.
    let decoy = node(
        "method:w:operator+",
        NodeKind::Method,
        "operator+",
        "W::operator+",
        "other.cpp",
        Language::Cpp,
        2,
        2,
    );
    q.insert_nodes(&[infix_caller.clone(), op_plus.clone(), decoy.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &infix_caller.id,
        "a.operator+",
        EdgeKind::Calls,
        6,
        "optest.cpp",
        Language::Cpp,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let calls = outgoing(&q, &infix_caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1, "expected one resolved operator+ call");
    assert_eq!(calls[0].target, op_plus.id);
    assert_ne!(calls[0].target, decoy.id);
}

#[tokio::test(flavor = "current_thread")]
async fn cpp_subscript_operator_call_resolves_to_operator_method() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "optest.cpp",
        "struct V {\n    int x;\n    V operator[](int i) const { return V{x + i}; }\n};\nV subscriptCaller(const V& a) { return a[3]; }\n",
    );
    fx.track(&q, "optest.cpp", Language::Cpp);

    let subscript_caller = node(
        "func:optest:subscriptCaller",
        NodeKind::Function,
        "subscriptCaller",
        "subscriptCaller",
        "optest.cpp",
        Language::Cpp,
        5,
        5,
    );
    let op_sub = node(
        "method:v:operator[]",
        NodeKind::Method,
        "operator[]",
        "V::operator[]",
        "optest.cpp",
        Language::Cpp,
        3,
        3,
    );
    q.insert_nodes(&[subscript_caller.clone(), op_sub.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &subscript_caller.id,
        "a.operator[]",
        EdgeKind::Calls,
        5,
        "optest.cpp",
        Language::Cpp,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let calls = outgoing(&q, &subscript_caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1, "expected one resolved operator[] call");
    assert_eq!(calls[0].target, op_sub.id);
}
