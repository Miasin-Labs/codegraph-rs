use crate::fixture::*;

#[test]
fn detects_react_framework() {
    let ctx = MockCtx {
        files: vec!["package.json".into(), "src/App.tsx".into()],
        contents: HashMap::from([(
            "package.json".to_string(),
            r#"{"dependencies":{"react":"^18.0.0"}}"#.to_string(),
        )]),
        existing: vec![],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "react"));
}

#[test]
fn detects_express_framework() {
    let ctx = MockCtx {
        files: vec!["package.json".into(), "src/app.js".into()],
        contents: HashMap::from([(
            "package.json".to_string(),
            r#"{"dependencies":{"express":"^4.18.0"}}"#.to_string(),
        )]),
        existing: vec![],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "express"));
}

#[test]
fn detects_laravel_framework() {
    let ctx = MockCtx {
        files: vec!["artisan".into(), "app/Http/Kernel.php".into()],
        contents: HashMap::new(),
        existing: vec!["artisan".into()],
        root: "/test".into(),
    };
    let frameworks = detect_frameworks(&ctx);
    assert!(frameworks.iter().any(|f| f.name() == "laravel"));
}

#[test]
fn returns_all_framework_resolvers() {
    let resolvers = get_all_framework_resolvers();
    assert!(!resolvers.is_empty());
    assert!(resolvers.iter().any(|r| r.name() == "react"));
    assert!(resolvers.iter().any(|r| r.name() == "express"));
    assert!(resolvers.iter().any(|r| r.name() == "laravel"));
}
