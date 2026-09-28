//! The starter ruleset, embedded (`codegraph analyze rules --builtin`).
//! Each file is ordinary rule YAML; every rule carries examples that
//! `rules_builtin_examples_pass` runs.

/// `(file name, YAML)` of every built-in rule file.
pub const BUILTIN_RULES: &[(&str, &str)] = &[
    ("c-memory.yaml", include_str!("c-memory.yaml")),
    ("c-strings.yaml", include_str!("c-strings.yaml")),
    ("c-input.yaml", include_str!("c-input.yaml")),
    ("c-taint.yaml", include_str!("c-taint.yaml")),
    ("java.yaml", include_str!("java.yaml")),
    ("java-taint.yaml", include_str!("java-taint.yaml")),
    ("python.yaml", include_str!("python.yaml")),
    ("python-taint.yaml", include_str!("python-taint.yaml")),
    ("javascript.yaml", include_str!("javascript.yaml")),
    (
        "javascript-taint.yaml",
        include_str!("javascript-taint.yaml"),
    ),
    ("php.yaml", include_str!("php.yaml")),
    ("php-taint.yaml", include_str!("php-taint.yaml")),
    ("rust.yaml", include_str!("rust.yaml")),
    ("rust-server.yaml", include_str!("rust-server.yaml")),
    ("rust-rfc.yaml", include_str!("rust-rfc.yaml")),
];
