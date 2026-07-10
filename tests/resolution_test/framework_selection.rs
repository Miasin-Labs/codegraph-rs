use crate::fixture::*;

#[test]
fn framework_registry_preserves_ts_registration_order() {
    // The TS FRAMEWORK_RESOLVERS array order is load-bearing: detection
    // results and per-reference strategy order iterate it.
    let names: Vec<String> = get_all_framework_resolvers()
        .iter()
        .map(|r| r.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "laravel",
            "drupal",
            "express",
            "nestjs",
            "react",
            "svelte",
            "vue",
            "astro",
            "django",
            "flask",
            "fastapi",
            "rails",
            "spring",
            "play",
            "go",
            "goframe",
            "rust",
            "aspnet",
            "swiftui",
            "uikit",
            "vapor",
            "swift-objc-bridge",
            "react-native-bridge",
            "expo-modules",
            "fabric-view",
            "cics",
            "terraform",
            "salesforce",
        ]
    );
}

// =============================================================================
// getApplicableFrameworks (frameworks.test.ts)
// =============================================================================

struct FakeFw {
    name: &'static str,
    langs: Option<&'static [Language]>,
}

impl FrameworkResolver for FakeFw {
    fn name(&self) -> &str {
        self.name
    }
    fn languages(&self) -> Option<&[Language]> {
        self.langs
    }
    fn detect(&self, _: &dyn ResolutionContext) -> bool {
        true
    }
    fn resolve(&self, _: &UnresolvedRef, _: &dyn ResolutionContext) -> Option<ResolvedRef> {
        None
    }
}

fn fake_fws() -> Vec<Box<dyn FrameworkResolver>> {
    static PY: [Language; 1] = [Language::Python];
    static JS: [Language; 2] = [Language::Javascript, Language::Typescript];
    vec![
        Box::new(FakeFw {
            name: "py",
            langs: Some(&PY),
        }),
        Box::new(FakeFw {
            name: "js",
            langs: Some(&JS),
        }),
        Box::new(FakeFw {
            name: "any",
            langs: None,
        }),
    ]
}

#[test]
fn get_applicable_frameworks_filters_by_language() {
    let fws = fake_fws();
    let result = get_applicable_frameworks(&fws, Language::Python);
    let names: Vec<&str> = result.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["py", "any"]);
}

#[test]
fn get_applicable_frameworks_returns_universal_only_when_no_match() {
    let fws = fake_fws();
    let result = get_applicable_frameworks(&fws, Language::Rust);
    let names: Vec<&str> = result.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["any"]);
}

// =============================================================================
// Integration Tests (resolution.test.ts)
// =============================================================================
