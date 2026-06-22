use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Language Detection')
// =============================================================================

#[test]
fn language_detection_typescript_files() {
    assert_eq!(detect_language("src/index.ts", None), Language::Typescript);
    assert_eq!(
        detect_language("components/Button.tsx", None),
        Language::Tsx
    );
}

#[test]
fn language_detection_javascript_files() {
    assert_eq!(detect_language("index.js", None), Language::Javascript);
    assert_eq!(detect_language("App.jsx", None), Language::Jsx);
    assert_eq!(detect_language("config.mjs", None), Language::Javascript);
}

#[test]
fn language_detection_python_files() {
    assert_eq!(detect_language("main.py", None), Language::Python);
}

#[test]
fn language_detection_go_files() {
    assert_eq!(detect_language("main.go", None), Language::Go);
}

#[test]
fn language_detection_rust_files() {
    assert_eq!(detect_language("lib.rs", None), Language::Rust);
}

#[test]
fn language_detection_java_files() {
    assert_eq!(detect_language("Main.java", None), Language::Java);
}

#[test]
fn language_detection_c_files() {
    assert_eq!(detect_language("main.c", None), Language::C);
    assert_eq!(detect_language("utils.h", None), Language::C);
}

#[test]
fn language_detection_cpp_files() {
    assert_eq!(detect_language("main.cpp", None), Language::Cpp);
    assert_eq!(detect_language("class.hpp", None), Language::Cpp);
}

#[test]
fn language_detection_csharp_files() {
    assert_eq!(detect_language("Program.cs", None), Language::Csharp);
}

#[test]
fn language_detection_php_files() {
    assert_eq!(detect_language("index.php", None), Language::Php);
}

#[test]
fn language_detection_ruby_files() {
    assert_eq!(detect_language("app.rb", None), Language::Ruby);
}

#[test]
fn language_detection_swift_files() {
    assert_eq!(
        detect_language("ViewController.swift", None),
        Language::Swift
    );
}

#[test]
fn language_detection_kotlin_files() {
    assert_eq!(detect_language("MainActivity.kt", None), Language::Kotlin);
    assert_eq!(detect_language("build.gradle.kts", None), Language::Kotlin);
}

#[test]
fn language_detection_dart_files() {
    assert_eq!(detect_language("main.dart", None), Language::Dart);
}

#[test]
fn language_detection_objective_c_files() {
    assert_eq!(detect_language("AppDelegate.m", None), Language::Objc);
    assert_eq!(detect_language("ViewController.mm", None), Language::Objc);
    let objc_header = "@interface Foo : NSObject\n@end\n";
    assert_eq!(detect_language("Foo.h", Some(objc_header)), Language::Objc);
    assert_eq!(
        detect_language("stdio.h", Some("#ifndef STDIO_H\nvoid printf();\n#endif\n")),
        Language::C
    );
}

#[test]
fn language_detection_unknown_for_unsupported_extensions() {
    assert_eq!(detect_language("styles.css", None), Language::Unknown);
    assert_eq!(detect_language("data.json", None), Language::Unknown);
}
