//! Miri's report, read back from its output: what went wrong (the UB kind,
//! or why Miri could not go on), where (the primary span), and the stack
//! that got there.
//!
//! Miri prints one rustc-style diagnostic and stops:
//!
//! ```text
//! error: Undefined Behavior: memory access failed: alloc42 has been freed, so this pointer is dangling
//!   --> src/lib.rs:15:14
//!    |
//! help: alloc42 was allocated here:
//!   --> src/lib.rs:12:13
//!    = note: stack backtrace:
//!            0: uaf
//!                at src/lib.rs:15:14: 15:16
//!            1: tests::t_uaf
//!                at src/lib.rs:57:18: 57:23
//! ```
//!
//! The header's prefix is the category (`Undefined Behavior`, `unsupported
//! operation`, `memory leaked`, …); the message after it names the kind
//! ([`UbKind::classify`]). Miri's own UI tests normalize places to `LL:CC`,
//! which reads as line 0.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// What kind of report it is: the header's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// `Undefined Behavior:` — proof of UB on this execution.
    UndefinedBehavior,
    /// `memory leaked:` — a leak at exit (not UB, but proven).
    MemoryLeak,
    /// `unsupported operation:` — Miri cannot run this code (FFI, inline
    /// assembly, a syscall, an isolated operation); nothing is proven.
    Unsupported,
    /// `abnormal termination:` — the program aborted (a panic that may not
    /// unwind, `process::abort`, allocation failure).
    Abort,
    /// `the evaluated program deadlocked`.
    Deadlock,
    /// `resource exhaustion:` — the interpreter ran out of memory or stack.
    ResourceExhaustion,
    /// A compile-time error of the interpreted program
    /// (`post-monomorphization error`, a failed const).
    CompileError,
}

impl Category {
    /// A proven problem in the program (not a limit of Miri).
    pub fn is_proof(self) -> bool {
        matches!(self, Self::UndefinedBehavior | Self::MemoryLeak)
    }
}

/// The kind of problem, from the message. Rule ids are `miri::<kind>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UbKind {
    UseAfterFree,
    OutOfBounds,
    /// A null or dangling (never-allocated) pointer or reference.
    DanglingPointer,
    UninitRead,
    /// A value invalid for its type (a `bool` of 3, a bad enum tag, a
    /// dangling or null reference constructed).
    InvalidValue,
    Alignment,
    StackedBorrows,
    TreeBorrows,
    DataRace,
    /// A deallocation with the wrong layout, a double free, freeing what
    /// was not heap-allocated.
    InvalidDealloc,
    /// A call through the wrong ABI, signature, vtable or function pointer.
    InvalidCall,
    /// `unreachable_unchecked`, `assume(false)`.
    Unreachable,
    /// Overflowing `unchecked_*` arithmetic or shifts, division by zero,
    /// out-of-range float-to-int.
    Arithmetic,
    /// `copy_nonoverlapping` on overlapping ranges.
    OverlappingCopy,
    /// A write to read-only memory.
    ReadOnlyWrite,
    /// Pointer provenance (`offset_from` across allocations, int-to-ptr).
    Provenance,
    /// Unwinding out of a function that may not unwind.
    InvalidUnwind,
    MemoryLeak,
    Abort,
    Deadlock,
    ResourceExhaustion,
    CompileError,
    /// Unsupported by Miri (see the message for what).
    Unsupported,
    /// UB of a kind not listed above.
    Other,
}

impl UbKind {
    /// The kebab-case id (`use-after-free`).
    pub fn id(self) -> &'static str {
        match self {
            Self::UseAfterFree => "use-after-free",
            Self::OutOfBounds => "out-of-bounds",
            Self::DanglingPointer => "dangling-pointer",
            Self::UninitRead => "uninit-read",
            Self::InvalidValue => "invalid-value",
            Self::Alignment => "alignment",
            Self::StackedBorrows => "stacked-borrows",
            Self::TreeBorrows => "tree-borrows",
            Self::DataRace => "data-race",
            Self::InvalidDealloc => "invalid-dealloc",
            Self::InvalidCall => "invalid-call",
            Self::Unreachable => "unreachable",
            Self::Arithmetic => "arithmetic",
            Self::OverlappingCopy => "overlapping-copy",
            Self::ReadOnlyWrite => "read-only-write",
            Self::Provenance => "provenance",
            Self::InvalidUnwind => "invalid-unwind",
            Self::MemoryLeak => "memory-leak",
            Self::Abort => "abort",
            Self::Deadlock => "deadlock",
            Self::ResourceExhaustion => "resource-exhaustion",
            Self::CompileError => "compile-error",
            Self::Unsupported => "unsupported",
            Self::Other => "other-ub",
        }
    }

    /// The finding's rule id (`miri::use-after-free`).
    pub fn rule(self) -> String {
        format!("miri::{}", self.id())
    }

    /// The kind a report of `category` with `message` (the header after
    /// its prefix) and `help` lines is. The aliasing model is named in a
    /// help line (`the Stacked Borrows rules it violated`).
    pub fn classify(category: Category, message: &str, help: &[String]) -> Self {
        match category {
            Category::MemoryLeak => return Self::MemoryLeak,
            Category::Unsupported => return Self::Unsupported,
            Category::Abort => return Self::Abort,
            Category::Deadlock => return Self::Deadlock,
            Category::ResourceExhaustion => return Self::ResourceExhaustion,
            Category::CompileError => return Self::CompileError,
            Category::UndefinedBehavior => {}
        }
        let m = message.to_ascii_lowercase();
        let has = |needle: &str| m.contains(needle);
        let helps = |needle: &str| help.iter().any(|h| h.contains(needle));
        if helps("Tree Borrows") {
            return Self::TreeBorrows;
        }
        if helps("Stacked Borrows") {
            return Self::StackedBorrows;
        }
        if has("data race") || has("race condition") {
            Self::DataRace
        } else if has("has been freed") || has("use-after-free") || has("dead local") {
            Self::UseAfterFree
        } else if has("uninitialized") {
            Self::UninitRead
        } else if has("alignment") && has("required") || has("unaligned") {
            Self::Alignment
        } else if has("constructing invalid value")
            || has("invalid tag")
            || has("interpreting an invalid")
            || has("uninhabited")
            || has("set discriminant")
            || has("simd mask")
        {
            Self::InvalidValue
        } else if has("deallocat") || has("incorrect layout") {
            Self::InvalidDealloc
        } else if has("out-of-bounds")
            || has("beyond the end")
            || has("from the end of the allocation")
            || has("before the beginning")
            || has("in-bounds pointer arithmetic")
            || has("overflowing pointer arithmetic")
        {
            Self::OutOfBounds
        } else if has("null pointer")
            || has("null box")
            || has("null reference")
            || has("dangling")
            || has("[noalloc]")
            || has("not dereferenceable")
            || has("memory access failed")
        {
            Self::DanglingPointer
        } else if has("calling a function")
            || has("extern static")
            || has("exported symbol")
            || has("contains a function")
            || has("calling convention")
            || has("abi mismatch")
            || has("vtable")
            || has("function pointer")
            || has("va_arg")
            || has("variadic")
            || has("more arguments")
        {
            Self::InvalidCall
        } else if has("unreachable") || has("`assume`") {
            Self::Unreachable
        } else if has("overflow")
            || has("_nonzero` called on 0")
            || has("divid")
            || has("divisor of zero")
            || has("cannot be represented")
            || has("non-finite")
        {
            Self::Arithmetic
        } else if has("overlapping") {
            Self::OverlappingCopy
        } else if has("read-only") {
            Self::ReadOnlyWrite
        } else if has("provenance") || has("ptr_offset_from") {
            Self::Provenance
        } else if has("unwinding") || has("unwind") {
            Self::InvalidUnwind
        } else {
            Self::Other
        }
    }
}

/// A place: `file:line:col` (line 0 when Miri's UI tests normalized it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// One frame of Miri's stack backtrace, innermost first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MiriFrame {
    /// The function as Miri names it (`tests::t_uaf::{closure#0}`).
    pub function: String,
    #[serde(flatten)]
    pub place: Place,
}

/// A labelled place from a `help:`/`note:` block (`alloc42 was allocated
/// here`, `<TAG> was later invalidated`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Related {
    pub note: String,
    #[serde(flatten)]
    pub place: Place,
}

/// One diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MiriDiagnostic {
    pub category: Category,
    pub kind: UbKind,
    /// The header after its prefix, allocation ids and tags normalized
    /// (`alloc42` → `ALLOC`, `<1234>` → `<TAG>`), so runs compare equal.
    pub message: String,
    /// Where it happened (the `-->` line under the header).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<Place>,
    /// Innermost first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<MiriFrame>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Related>,
    /// `help:` lines that are advice (how to rerun, what the model is).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub help: Vec<String>,
    /// The thread it happened on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// A panic (a failed test that is not UB).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Panic {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<Place>,
}

/// Everything read from one `cargo miri test` (or `cargo miri run`)
/// output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedOutput {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<MiriDiagnostic>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub panics: Vec<Panic>,
    /// libtest totals, summed over the test binaries that finished.
    pub passed: u32,
    pub failed: u32,
    pub ignored: u32,
    /// Some test binary printed `running N tests` with N > 0.
    pub ran_tests: bool,
    /// Tests that failed, by name (`tests::t_uaf`), from libtest's list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_tests: Vec<String>,
}

impl ParsedOutput {
    /// The first diagnostic that proves something, else the first one.
    pub fn primary(&self) -> Option<&MiriDiagnostic> {
        self.diagnostics
            .iter()
            .find(|d| d.category.is_proof())
            .or_else(|| self.diagnostics.first())
    }
}

/// `error: <prefix>: message`, anywhere in the line (libtest prints
/// `test x ... ` before it).
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|\s)error: (Undefined Behavior|unsupported operation|memory leaked|abnormal termination|resource exhaustion|post-monomorphization error|the evaluated program deadlocked)(?::\s*(.*))?$",
    )
    .expect("valid regex")
});
static ARROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*-->\s*(.+?):(\d+|LL):(\d+|CC)\s*$").expect("valid regex"));
static FRAME_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:= note:\s*)?(\d+): (.+?)\s*$").expect("valid regex"));
static FRAME_AT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*at (.+?):(\d+|LL):(\d+|CC)(?:: \d+:\d+)?\s*$").expect("valid regex")
});
static HELP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:= )?(help|note): (.*)$").expect("valid regex"));
static THREAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this is on thread `([^`]*)`").expect("valid regex"));
static RESULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored")
        .expect("valid regex")
});
static RUNNING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^running (\d+) tests?$").expect("valid regex"));
static FAILED_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{4}(\S+)$").expect("valid regex"));
static PANIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"panicked at (?:'(.*)', )?([^\s:][^:]*):(\d+):(\d+):?\s*$").expect("valid regex")
});
static ALLOC_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\balloc\d+\b").expect("valid regex"));
static TAG_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<\d+>").expect("valid regex"));

fn category_of(prefix: &str) -> Category {
    match prefix {
        "Undefined Behavior" => Category::UndefinedBehavior,
        "unsupported operation" => Category::Unsupported,
        "memory leaked" => Category::MemoryLeak,
        "abnormal termination" => Category::Abort,
        "resource exhaustion" => Category::ResourceExhaustion,
        "the evaluated program deadlocked" => Category::Deadlock,
        _ => Category::CompileError,
    }
}

fn line_number(text: &str) -> u32 {
    text.parse().unwrap_or(0)
}

/// Allocation ids and borrow tags differ from run to run.
pub fn normalize_message(message: &str) -> String {
    let message = ALLOC_ID.replace_all(message, "ALLOC");
    TAG_ID.replace_all(&message, "<TAG>").trim().to_string()
}

/// Read every Miri diagnostic, panic and libtest total from `output`.
pub fn parse_output(output: &str) -> ParsedOutput {
    let mut parsed = ParsedOutput::default();
    let lines: Vec<&str> = output.lines().collect();
    let mut index = 0;
    let mut in_failures_list = false;
    while index < lines.len() {
        let line = lines[index];
        if let Some(caps) = HEADER.captures(line) {
            let (diagnostic, next) = parse_diagnostic(&lines, index, &caps);
            parsed.diagnostics.push(diagnostic);
            index = next;
            continue;
        }
        if let Some(caps) = PANIC.captures(line) {
            // New format: the message is the next line; old: quoted inline.
            let message = match caps.get(1) {
                Some(inline) => inline.as_str().to_string(),
                None => lines
                    .get(index + 1)
                    .map(|m| m.trim().to_string())
                    .unwrap_or_default(),
            };
            parsed.panics.push(Panic {
                message,
                place: Some(Place {
                    file: caps[2].to_string(),
                    line: line_number(&caps[3]),
                    col: line_number(&caps[4]),
                }),
            });
        }
        if let Some(caps) = RESULT.captures(line) {
            parsed.passed += line_number(&caps[1]);
            parsed.failed += line_number(&caps[2]);
            parsed.ignored += line_number(&caps[3]);
        }
        if let Some(caps) = RUNNING.captures(line.trim()) {
            parsed.ran_tests |= line_number(&caps[1]) > 0;
        }
        if line.trim() == "failures:" {
            in_failures_list = true;
        } else if in_failures_list {
            match FAILED_TEST.captures(line) {
                Some(caps) if !caps[1].starts_with("----") => {
                    if !parsed.failed_tests.contains(&caps[1].to_string()) {
                        parsed.failed_tests.push(caps[1].to_string());
                    }
                }
                _ if line.trim().is_empty() => {}
                _ => in_failures_list = false,
            }
        }
        index += 1;
    }
    parsed
}

/// The diagnostic whose header is `lines[start]`, and the index after it:
/// the next header, `error: aborting due to`, or a line that is plainly
/// not part of a diagnostic.
fn parse_diagnostic(
    lines: &[&str],
    start: usize,
    caps: &regex::Captures<'_>,
) -> (MiriDiagnostic, usize) {
    let category = category_of(&caps[1]);
    let message = caps
        .get(2)
        .map_or_else(|| caps[1].to_string(), |m| m.as_str().to_string());
    let mut primary: Option<Place> = None;
    let mut frames: Vec<MiriFrame> = Vec::new();
    let mut related: Vec<Related> = Vec::new();
    let mut help: Vec<String> = Vec::new();
    let mut thread = None;
    // The label of the `help:`/`note:` line a following `-->` belongs to.
    let mut pending_label: Option<String> = None;
    let mut in_backtrace = false;
    let mut pending_frame: Option<String> = None;
    let mut index = start + 1;
    while index < lines.len() {
        let line = lines[index];
        if HEADER.is_match(line) || line.starts_with("error: aborting due to") {
            break;
        }
        if line.starts_with("test result:") || line.starts_with("error: test failed") {
            break;
        }
        if let Some(caps) = ARROW.captures(line) {
            let place = Place {
                file: caps[1].to_string(),
                line: line_number(&caps[2]),
                col: line_number(&caps[3]),
            };
            match pending_label.take() {
                Some(note) => related.push(Related { note, place }),
                None if primary.is_none() => primary = Some(place),
                None => {}
            }
            index += 1;
            continue;
        }
        if in_backtrace {
            if let Some(caps) = FRAME_AT.captures(line) {
                if let Some(function) = pending_frame.take() {
                    frames.push(MiriFrame {
                        function,
                        place: Place {
                            file: caps[1].to_string(),
                            line: line_number(&caps[2]),
                            col: line_number(&caps[3]),
                        },
                    });
                }
                index += 1;
                continue;
            }
            if let Some(caps) = FRAME_NAME.captures(line) {
                pending_frame = Some(caps[2].to_string());
                index += 1;
                continue;
            }
            in_backtrace = false;
        }
        if let Some(caps) = HELP.captures(line) {
            let text = caps[2].trim().to_string();
            // rustc's layout: a sub-diagnostic with a place starts at
            // column 0 and its `-->` follows; `= help:`/`= note:` lines
            // are text only.
            let has_place = line.starts_with("help:") || line.starts_with("note:");
            if let Some(thread_caps) = THREAD.captures(&text) {
                thread = Some(thread_caps[1].to_string());
            } else if text.starts_with("stack backtrace:") {
                in_backtrace = true;
            } else if has_place {
                pending_label = Some(normalize_message(text.trim_end_matches(':')));
            } else {
                help.push(normalize_message(&text));
            }
        }
        index += 1;
    }
    let kind = UbKind::classify(category, &message, &help);
    (
        MiriDiagnostic {
            category,
            kind,
            message: normalize_message(&message),
            primary,
            frames,
            related,
            help,
            thread,
        },
        index,
    )
}
