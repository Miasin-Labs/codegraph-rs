use super::super::TreeSitterExtractor;
use super::value_references::value_reference_readers;
use crate::extraction::languages::extractor_for;
use crate::types::{ExtractionResult, Language};

struct Case {
    label: &'static str,
    path: &'static str,
    language: Language,
    source: &'static str,
    expected: &'static [(&'static str, &'static [&'static str])],
}

fn extract(case: &Case) -> ExtractionResult {
    TreeSitterExtractor::new(
        case.path,
        case.source,
        Some(case.language),
        extractor_for(case.language),
    )
    .extract()
}

#[test]
fn source_supported_languages_emit_value_reference_edges() {
    // Given: the positive language/declaration fixtures from value-reference-edges.test.ts.
    let cases = [
        Case {
            label: "tsx jsx reads",
            path: "widget.tsx",
            language: Language::Tsx,
            source: r#"export const THEME_TOKENS = { color: "red", size: 12 };
export function Label() { return <span style={{ color: THEME_TOKENS.color }}>hi</span>; }
export const Box = () => <div data-size={THEME_TOKENS.size} />;"#,
            expected: &[("THEME_TOKENS", &["Box", "Label"])],
        },
        Case {
            label: "rust const and static",
            path: "lib.rs",
            language: Language::Rust,
            source: r#"const MAX_RETRIES: u32 = 3;
static DEFAULT_LABEL: &str = "prod";
fn retry() -> u32 { MAX_RETRIES }
fn label() -> &'static str { DEFAULT_LABEL }"#,
            expected: &[("MAX_RETRIES", &["retry"]), ("DEFAULT_LABEL", &["label"])],
        },
        Case {
            label: "go package const and var",
            path: "main.go",
            language: Language::Go,
            source: r#"package main
const MaxRetries = 3
var DefaultLabels = map[string]string{"env": "prod"}
func retry() int { return MaxRetries }
func labels() map[string]string { return DefaultLabels }"#,
            expected: &[("MaxRetries", &["retry"]), ("DefaultLabels", &["labels"])],
        },
        Case {
            label: "python conditional module const",
            path: "cond.py",
            language: Language::Python,
            source: "try:\n\tHAS_SSL = True\nexcept ImportError:\n\tHAS_SSL = False\ndef uses_ssl():\n\treturn HAS_SSL",
            expected: &[("HAS_SSL", &["uses_ssl"])],
        },
        Case {
            label: "ruby top-level and class constants",
            path: "app.rb",
            language: Language::Ruby,
            source: r#"MAX_RETRIES = 3
def retry_count; MAX_RETRIES; end
class Config
  TIMEOUT = 30
  def self.get_timeout; TIMEOUT; end
  def describe; "timeout=#{TIMEOUT}"; end
end"#,
            expected: &[
                ("MAX_RETRIES", &["retry_count"]),
                ("TIMEOUT", &["describe", "get_timeout"]),
            ],
        },
        Case {
            label: "c file constants",
            path: "config.c",
            language: Language::C,
            source: r#"static const int MAX_ITEMS = 100;
static const char *const STATUS_NAMES[] = { "ok", "fail" };
int capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }
const char *label(int i) { return STATUS_NAMES[i]; }"#,
            expected: &[("MAX_ITEMS", &["capped"]), ("STATUS_NAMES", &["label"])],
        },
        Case {
            label: "java static final constants",
            path: "Limits.java",
            language: Language::Java,
            source: r#"class Limits {
  public static final int MAX_ITEMS = 100;
  static final String[] STATUS_NAMES = { "ok", "fail" };
  int capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }
  String label(int i) { return STATUS_NAMES[i]; }
}"#,
            expected: &[("MAX_ITEMS", &["capped"]), ("STATUS_NAMES", &["label"])],
        },
        Case {
            label: "csharp const and static readonly",
            path: "Limits.cs",
            language: Language::Csharp,
            source: r#"class Limits {
  const int MAX_ITEMS = 100;
  static readonly string[] STATUS_NAMES = { "ok", "fail" };
  int Capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }
  string Label(int i) { return STATUS_NAMES[i]; }
}"#,
            expected: &[("MAX_ITEMS", &["Capped"]), ("STATUS_NAMES", &["Label"])],
        },
        Case {
            label: "php top-level and class constants",
            path: "Config.php",
            language: Language::Php,
            source: r#"<?php
const APP_VERSION = "1.0";
class Config {
  const MAX_ITEMS = 100;
  const STATUS_NAMES = ["ok", "fail"];
  function capped($n) { return $n > self::MAX_ITEMS ? self::MAX_ITEMS : $n; }
  function label($i) { return Config::STATUS_NAMES[$i]; }
  function version() { return APP_VERSION; }
}"#,
            expected: &[
                ("MAX_ITEMS", &["capped"]),
                ("STATUS_NAMES", &["label"]),
                ("APP_VERSION", &["version"]),
            ],
        },
        Case {
            label: "scala object values",
            path: "Demo.scala",
            language: Language::Scala,
            source: r#"object Config {
  val TIMEOUT_MS = 30
  val STATUS_NAMES = List("ok", "fail")
  def capped(n: Int): Int = if (n > TIMEOUT_MS) TIMEOUT_MS else n
  def label(i: Int): String = STATUS_NAMES(i)
}"#,
            expected: &[("TIMEOUT_MS", &["capped"]), ("STATUS_NAMES", &["label"])],
        },
        Case {
            label: "kotlin shared constants",
            path: "Demo.kt",
            language: Language::Kotlin,
            source: r#"const val TOP_LEVEL_MAX = 100
object Config {
  val STATUS_NAMES = listOf("ok", "fail")
  fun label(i: Int): String = STATUS_NAMES[i]
}
class Widget {
  companion object { const val MAX_RETRIES = 3 }
  fun retries(): Int = MAX_RETRIES
  fun within(n: Int): Int = if (n < TOP_LEVEL_MAX) n else TOP_LEVEL_MAX
}"#,
            expected: &[
                ("STATUS_NAMES", &["label"]),
                ("MAX_RETRIES", &["retries"]),
                ("TOP_LEVEL_MAX", &["within"]),
            ],
        },
        Case {
            label: "swift shared lets",
            path: "Demo.swift",
            language: Language::Swift,
            source: r#"let topLevelMax = 100
enum Constants { static let STATUS_NAMES = ["ok", "fail"] }
struct Widget {
  static let MAX_RETRIES = 3
  func retries() -> Int { return Widget.MAX_RETRIES }
  func within(_ n: Int) -> Int { return n < topLevelMax ? n : topLevelMax }
}
func labels(_ i: Int) -> String { return Constants.STATUS_NAMES[i] }"#,
            expected: &[
                ("STATUS_NAMES", &["labels"]),
                ("MAX_RETRIES", &["retries"]),
                ("topLevelMax", &["within"]),
            ],
        },
        Case {
            label: "dart shared constants",
            path: "demo.dart",
            language: Language::Dart,
            source: r#"const TOP_LEVEL_MAX = 100;
class Config {
  static const TIMEOUT_MS = 30;
  static final STATUS_NAMES = ["ok", "fail"];
  int capped(int n) => n > TIMEOUT_MS ? TIMEOUT_MS : n;
  String label(int i) { return STATUS_NAMES[i]; }
  int withinLimit(int n) => n < TOP_LEVEL_MAX ? n : TOP_LEVEL_MAX;
}"#,
            expected: &[
                ("TIMEOUT_MS", &["capped"]),
                ("STATUS_NAMES", &["label"]),
                ("TOP_LEVEL_MAX", &["withinLimit"]),
            ],
        },
        Case {
            label: "pascal unit constants",
            path: "demo.pas",
            language: Language::Pascal,
            source: "unit Demo;\ninterface\nconst\n  MAX_ITEMS = 100;\n  APP_NAME = 'MyApp';\nimplementation\nfunction Capped(n: Integer): Integer;\nbegin\n  if n > MAX_ITEMS then Capped := MAX_ITEMS else Capped := n;\nend;\nfunction AppLabel: string;\nbegin\n  AppLabel := APP_NAME;\nend;\nend.",
            expected: &[("MAX_ITEMS", &["Capped"]), ("APP_NAME", &["AppLabel"])],
        },
    ];

    // When/Then: each file is extracted and every source reader maps to its shared value.
    for case in cases {
        let result = extract(&case);
        assert!(
            result.errors.is_empty(),
            "{}: {:?}",
            case.label,
            result.errors
        );
        for (target, expected) in case.expected {
            assert_eq!(
                value_reference_readers(&result, target),
                *expected,
                "{} target {target}; nodes={:?}",
                case.label,
                result
                    .nodes
                    .iter()
                    .map(|node| (&node.name, node.kind))
                    .collect::<Vec<_>>()
            );
        }
    }
}
