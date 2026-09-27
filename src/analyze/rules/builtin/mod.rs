//! The starter ruleset, embedded (`codegraph analyze rules --builtin`).
//! Each file is ordinary rule YAML; every rule carries examples that
//! `rules_builtin_examples_pass` runs.

/// `(file name, YAML)` of every built-in rule file.
pub const BUILTIN_RULES: &[(&str, &str)] = &[
    ("c-memory.yaml", include_str!("c-memory.yaml")),
    ("c-strings.yaml", include_str!("c-strings.yaml")),
    ("c-input.yaml", include_str!("c-input.yaml")),
    ("java.yaml", include_str!("java.yaml")),
    ("python.yaml", include_str!("python.yaml")),
    ("javascript.yaml", include_str!("javascript.yaml")),
    ("php.yaml", include_str!("php.yaml")),
    ("rust.yaml", include_str!("rust.yaml")),
];
