use super::*;

// -- Apex case-insensitive resolution -------------------------------------

#[test]
fn apex_exact_name_matches_case_insensitively() {
    let ctx = Fixture::new(vec![node(
        "class:AccountService",
        NodeKind::Class,
        "AccountService",
        "AccountService",
        "classes/AccountService.cls",
        Language::Apex,
        1,
        30,
    )]);
    // Case-mismatched Apex reference still lands on the class.
    let r = make_ref(
        "accountservice",
        EdgeKind::References,
        5,
        "classes/Caller.cls",
        Language::Apex,
    );
    let result = match_by_exact_name(&r, &ctx).expect("resolved");
    assert_eq!(result.target_node_id, "class:AccountService");
    assert_eq!(result.resolved_by, ResolvedBy::ExactMatch);

    // The same lookup from a case-sensitive language stays unresolved.
    let js = make_ref(
        "accountservice",
        EdgeKind::References,
        5,
        "src/app.js",
        Language::Javascript,
    );
    assert!(match_by_exact_name(&js, &ctx).is_none());
}

#[test]
fn apex_method_call_matches_case_insensitively() {
    let ctx = Fixture::new(vec![
        node(
            "class:AccountService",
            NodeKind::Class,
            "AccountService",
            "AccountService",
            "classes/AccountService.cls",
            Language::Apex,
            1,
            30,
        ),
        node(
            "method:createAccount",
            NodeKind::Method,
            "createAccount",
            "AccountService::createAccount",
            "classes/AccountService.cls",
            Language::Apex,
            3,
            10,
        ),
    ]);
    // Receiver and method both case-mismatched.
    let r = make_ref(
        "accountservice.CREATEACCOUNT",
        EdgeKind::Calls,
        5,
        "classes/Caller.cls",
        Language::Apex,
    );
    let result = match_method_call(&r, &ctx).expect("resolved");
    assert_eq!(result.target_node_id, "method:createAccount");
}
