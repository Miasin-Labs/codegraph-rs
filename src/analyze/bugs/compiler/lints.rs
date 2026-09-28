//! The lints the compiler detector asks rustc and clippy for, and how much
//! each is trusted.
//!
//! Chosen for bugs, not style: clippy's `correctness` and `suspicious`
//! groups (minus their documentation and tooling hygiene members), a few
//! restriction/pedantic/nursery lints that are bugs when the value comes
//! from outside (a request, a file, a peer), and rustc lints that are bugs
//! whatever the context. Every name was checked against the installed
//! clippy (`clippy-driver -W help`, clippy for rustc 1.101 nightly,
//! 2026-09); `lint_names_exist_in_the_installed_clippy` re-checks them
//! wherever clippy is installed. (`integer_arithmetic` is gone: clippy
//! renamed it `arithmetic_side_effects`.)
//!
//! A lint's [`Precision`] is its base confidence, measured
//! (`docs/architecture/compiler-detector.md`): hand-judged findings on rms and
//! codegraph-rs, and RustSec vuln/fixed pairs where a lint that fires near
//! the fix and not after it is a catch. [`Exposure::Input`] lints are only
//! bugs when an attacker controls the value, so their confidence moves with
//! reachability from an entry point (`super::reach`).

/// Where a lint lives, which decides its rule id: `clippy::<name>` or
/// `rustc::<name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Clippy,
    Rustc,
}

/// How often a finding of the lint is a real bug, before context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    /// Almost always a bug where it fires (a comparison that is always
    /// false, a lock guard dropped at once).
    High,
    /// Often a bug; needs reading.
    Medium,
    /// A shape that is a bug only in some contexts (a panic on bad input).
    Low,
    /// Fires on nearly every function; measured almost never at a fix
    /// (`indexing_slicing`: 0 of 1,245 RustSec findings, 0 of 85 on rms).
    Rare,
}

impl Precision {
    pub fn confidence(self) -> f64 {
        match self {
            Self::High => 0.8,
            Self::Medium => 0.55,
            Self::Low => 0.3,
            Self::Rare => 0.15,
        }
    }

    /// The most a request handler's reach can raise an input lint to: two
    /// and a half times its base, at most 0.9. Reach is context, not proof —
    /// on rms the reached `indexing_slicing`/`unwrap_used` findings were as
    /// guarded as the rest, so a rare lint stays below a correctness one.
    pub fn reach_ceiling(self) -> f64 {
        (self.confidence() * 2.5).min(0.9)
    }
}

/// Whether the lint's bug depends on who controls the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// A bug wherever it fires.
    Any,
    /// A crash or wrong result only for values from outside: ranked by
    /// reachability from an entry point that takes untrusted input.
    Input,
}

/// One curated lint.
#[derive(Debug)]
pub struct Lint {
    /// As rustc and clippy spell it in diagnostics (`indexing_slicing`).
    pub name: &'static str,
    pub source: Source,
    pub precision: Precision,
    pub exposure: Exposure,
    /// Why it is in the table.
    pub why: &'static str,
    /// What a reviewer decides (review packets' checklist).
    pub questions: &'static [&'static str],
}

const INPUT_PANIC: &[&str] = &[
    "Can a request, file or peer choose the value that makes this panic (an index, a length, an \
     Err, a byte offset)? Trace it back to where it enters.",
    "Is it bounded or validated before this line on every path (a length check, a parse that \
     rejects it)?",
    "What does the panic take down: one request (the framework catches it), a spawned task, a \
     worker thread holding a lock (poisoned), or the process?",
];

/// The table. Clippy's `correctness`/`suspicious` members not listed here
/// take their group's defaults ([`GROUPS`]).
pub static LINTS: &[Lint] = &[
    // --- restriction / pedantic / nursery: bugs on untrusted input ---
    Lint {
        name: "indexing_slicing",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "`v[i]` / `&s[a..b]` panic out of bounds: a DoS when a request picks `i`. RFC 1679 \
              added `get(range)` as the total alternative.",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "string_slice",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Input,
        why: "Slicing a `str` at a byte offset panics inside a multi-byte character: the classic \
              untrusted-text DoS (offsets from `find` on another string, fixed widths).",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "arithmetic_side_effects",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "Overflowing `+ - * /` panics in debug builds and wraps in release: a length or \
              offset computed from input wraps into a small allocation or a wrong bound. RFC 560 \
              makes overflow a program error, not a defined wrap. (Replaces the removed \
              `integer_arithmetic`.)",
        questions: &[
            "Can the operands come from input (a header, a length field, a count)?",
            "If it wraps in release, what uses the result — an allocation size, a slice bound, a \
             loop limit?",
            "Would `checked_*`/`saturating_*` with an error be right here?",
        ],
    },
    Lint {
        name: "unwrap_used",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "`.unwrap()` turns an input error into a panic; in a request handler that is a \
              remote crash.",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "expect_used",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "Same as `unwrap_used`, with a message.",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "unwrap_in_result",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Input,
        why: "A function that can already return an error panics instead: the error path the \
              caller handles is skipped.",
        questions: &[
            "The function returns `Result`/`Option`. Should this failure be returned instead of \
             panicking?",
            "Can input make it fail?",
        ],
    },
    Lint {
        name: "panic",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "An explicit `panic!` on a path input can reach is a crash an attacker can trigger.",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "cast_possible_truncation",
        source: Source::Clippy,
        precision: Precision::Rare,
        exposure: Exposure::Input,
        why: "`len as u32` silently truncates: a length or offset from input that wraps defeats a \
              later bound check.",
        questions: &[
            "Can the value exceed the target type's range (a 64-bit length, a file size)?",
            "What uses the truncated value — a bound, a size field written back out, an index?",
        ],
    },
    Lint {
        name: "unchecked_time_subtraction",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Input,
        why: "`Instant - Duration` / `Instant - Instant` panic on underflow; a duration from \
              input (a timeout, a clock value) crashes.",
        questions: INPUT_PANIC,
    },
    Lint {
        name: "path_buf_push_overwrite",
        source: Source::Clippy,
        precision: Precision::Medium,
        exposure: Exposure::Input,
        why: "`PathBuf::push` of an absolute path replaces the base: a user-supplied name escapes \
              the directory (path traversal).",
        questions: &[
            "Can the pushed component come from input? An absolute path or `..` escapes the base.",
            "Is the result checked to stay under the base directory (canonicalize + starts_with)?",
        ],
    },
    Lint {
        name: "large_futures",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Any,
        why: "A future over 16 KiB held on the stack overflows small (spawned-task) stacks.",
        questions: &["Is this future polled on a small stack (a spawned task)? Box it if so."],
    },
    Lint {
        name: "mem_forget",
        source: Source::Clippy,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "`mem::forget` of a value with a destructor leaks it — a lock, a file, a buffer's \
              cleanup that never runs (safe since RFC 1066, so the compiler never objects).",
        questions: &[
            "What does the forgotten value's `Drop` do (release a lock, flush, free)?",
            "Is the leak intended (ownership handed to FFI) and documented?",
        ],
    },
    Lint {
        name: "undocumented_unsafe_blocks",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Any,
        why: "An `unsafe` block with no `// SAFETY:` argument: nobody wrote down why it is sound, \
              which is where soundness bugs live.",
        questions: &[
            "What invariant makes this `unsafe` sound, and does every caller uphold it?",
            "Can a safe caller violate it (a length, an alignment, an aliasing rule)?",
        ],
    },
    Lint {
        name: "read_zero_byte_vec",
        source: Source::Clippy,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`read` into a `Vec::with_capacity(n)` reads zero bytes: the buffer's length is 0.",
        questions: &["The buffer has length 0; was `vec![0; n]` or `resize` meant?"],
    },
    Lint {
        name: "significant_drop_in_scrutinee",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Any,
        why: "A lock guard in a `match`/`if let` scrutinee lives through the whole block: \
              deadlocks when an arm locks again.",
        questions: &[
            "Does any arm (or a function it calls) take the same lock again?",
            "Could the guard be dropped before the block (bind the value first)?",
        ],
    },
    Lint {
        name: "cast_ptr_alignment",
        source: Source::Clippy,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "Casting to a pointer of stricter alignment then reading it is undefined behaviour.",
        questions: &["Is the pointer's alignment guaranteed (or is the read `read_unaligned`)?"],
    },
    Lint {
        name: "unsafe_derive_deserialize",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Any,
        why: "Derived `Deserialize` on a type with `unsafe` methods builds values that skip the \
              invariants those methods rely on.",
        questions: &[
            "Which invariant do the unsafe methods assume, and can deserialized data break it?",
        ],
    },
    Lint {
        name: "fallible_impl_from",
        source: Source::Clippy,
        precision: Precision::Medium,
        exposure: Exposure::Input,
        why: "A `From` impl that can panic: conversions look infallible to callers.",
        questions: &["Should this be `TryFrom`? Can input make the conversion fail?"],
    },
    // --- members of the default groups listed for their own reasons ---
    Lint {
        name: "await_holding_lock",
        source: Source::Clippy,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "A sync `Mutex` guard held across `.await`: deadlocks the executor thread or blocks \
              every task waiting on it (the hazard RFC 3014's `must_not_suspend` names).",
        questions: &[
            "Can another task on this thread try the same lock while this one is suspended?",
            "Can the guard be dropped before the await, or an async mutex used?",
        ],
    },
    Lint {
        name: "let_underscore_future",
        source: Source::Clippy,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`let _ = fut;` drops a future unpolled: the work never runs.",
        questions: &["The future is never awaited or spawned. Was `.await` or `spawn` meant?"],
    },
    Lint {
        name: "unused_io_amount",
        source: Source::Clippy,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`read`/`write` may transfer fewer bytes than asked; ignoring the count loses or \
              truncates data (`read_exact`/`write_all` were meant).",
        questions: &["Can a short read/write happen here (sockets, pipes)? Use `*_exact`/`*_all`."],
    },
    Lint {
        name: "unconditional_recursion",
        source: Source::Clippy,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "A trait impl that calls itself (`PartialEq::eq` via `==` on `Self`): stack overflow.",
        questions: &["Which implementation was meant to be called instead of this one?"],
    },
    Lint {
        name: "suspicious_op_assign_impl",
        source: Source::Clippy,
        precision: Precision::Low,
        exposure: Exposure::Any,
        why: "A `*Assign` impl using an unexpected operator. Measured: 20 findings on RustSec \
              pairs, none at a fix (bit-twiddling impls do it on purpose), so below its group.",
        questions: &["Is the other operator in this `*Assign` impl deliberate?"],
    },
    // --- rustc lints ---
    Lint {
        name: "unused_must_use",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "A dropped `Result` (an ignored error), an unpolled future, or the result of a \
              function its author marked `#[must_use]` (RFC 1940).",
        questions: &[
            "What failure does the dropped value report, and should it stop or be logged here?",
            "If it is a future, was `.await` forgotten?",
        ],
    },
    Lint {
        name: "unconditional_recursion",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "A function that cannot return without calling itself: stack overflow.",
        questions: &["Which base case or other function was meant?"],
    },
    Lint {
        name: "arithmetic_overflow",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "Constant arithmetic that overflows (downgraded from deny so the rest still builds).",
        questions: &["Which constant or type was meant?"],
    },
    Lint {
        name: "unconditional_panic",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "An operation that always panics (constant out-of-bounds index, division by zero).",
        questions: &["Which index or divisor was meant?"],
    },
    Lint {
        name: "overflowing_literals",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "A literal out of range for its type wraps to another value.",
        questions: &["Which type or value was meant?"],
    },
    Lint {
        name: "let_underscore_lock",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`let _ = mutex.lock()` releases the lock at once: the section is unprotected.",
        questions: &["Was the guard meant to be held (`let _guard = …`)?"],
    },
    Lint {
        name: "dangling_pointers_from_temporaries",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "A pointer into a temporary that is dropped at the end of the statement: \
              use-after-free.",
        questions: &["Bind the temporary so it outlives the pointer."],
    },
    Lint {
        name: "invalid_value",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`mem::zeroed`/`uninitialized` of a type that cannot be all-zero/uninit: UB.",
        questions: &["Use `MaybeUninit` or a real constructor."],
    },
    Lint {
        name: "invalid_reference_casting",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "Casting `&T` to `&mut T` without interior mutability: UB.",
        questions: &["Use `UnsafeCell`/`Cell`/`RefCell` for the mutation."],
    },
    Lint {
        name: "deref_nullptr",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "Dereferencing a null pointer: UB.",
        questions: &["Which pointer was meant?"],
    },
    Lint {
        name: "static_mut_refs",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "A reference to a `static mut`: aliasing UB as soon as two exist.",
        questions: &["Can two references coexist (threads, reentrancy)? Use an atomic or lock."],
    },
    Lint {
        name: "invalid_from_utf8",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "`from_utf8` of bytes known to be invalid: always an error (or UB when unchecked).",
        questions: &["Which bytes or encoding were meant?"],
    },
    Lint {
        name: "invalid_nan_comparisons",
        source: Source::Rustc,
        precision: Precision::High,
        exposure: Exposure::Any,
        why: "Comparing with NaN is always false: the check never fires.",
        questions: &["Was `is_nan()` meant?"],
    },
    Lint {
        name: "useless_ptr_null_checks",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "A null check on a pointer that cannot be null (from a reference or fn): the real \
              check is missing elsewhere.",
        questions: &["Where could the value actually be null, and is that checked?"],
    },
    Lint {
        name: "dropping_references",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "`drop(&x)` drops the reference, not `x`: the resource (often a lock guard) stays \
              held.",
        questions: &["Was the owned value (a guard) meant to be dropped here?"],
    },
    Lint {
        name: "forgetting_references",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "`mem::forget(&x)` forgets the reference; `x` is still dropped.",
        questions: &["Was the owned value meant?"],
    },
    Lint {
        name: "unpredictable_function_pointer_comparisons",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "Function pointer equality is unreliable across codegen units: a dispatch check may \
              fail at random.",
        questions: &["Compare something stable (an enum, an id) instead."],
    },
    Lint {
        name: "ambiguous_wide_pointer_comparisons",
        source: Source::Rustc,
        precision: Precision::Medium,
        exposure: Exposure::Any,
        why: "Comparing fat pointers compares metadata (vtables) too: identity checks misfire.",
        questions: &["Was `ptr::addr_eq` meant?"],
    },
];

/// Clippy groups enabled wholesale, with the defaults their members take.
pub struct Group {
    pub name: &'static str,
    pub precision: Precision,
    pub members: &'static [&'static str],
    pub why: &'static str,
}

pub static GROUPS: &[Group] = &[
    Group {
        name: "correctness",
        precision: Precision::High,
        members: CORRECTNESS,
        why: "Code that is outright wrong or useless (clippy's own definition); deny by default.",
    },
    Group {
        name: "suspicious",
        precision: Precision::Medium,
        members: SUSPICIOUS,
        why: "Code that is most likely wrong or useless.",
    },
];

/// Members of the groups turned off (`-A`): documentation, attribute and
/// tooling hygiene, and style that happened to land in a bug group. None
/// changes what the program does.
pub static EXCLUDED: &[&str] = &[
    "blanket_clippy_restriction_lints",
    "crate_in_macro_def",
    "deprecated_clippy_cfg_attr",
    "deprecated_semver",
    "doc_nested_refdefs",
    "doc_suspicious_footnotes",
    "duplicated_attributes",
    "empty_docs",
    "empty_line_after_doc_comments",
    "empty_line_after_outer_attr",
    "four_forward_slashes",
    "incompatible_msrv",
    "inline_fn_without_body",
    "lint_groups_priority",
    "manual_unwrap_or_default",
    "missing_transmute_annotations",
    "needless_character_iteration",
    "needless_maybe_sized",
    "redundant_locals",
    "suspicious_doc_comments",
    "test_attr_in_doctest",
    "unnecessary_clippy_cfg",
    "unnecessary_option_map_or_else",
    "unnecessary_result_map_or_else",
    "useless_attribute",
];

/// `clippy::correctness` as of clippy for rustc 1.101 (2026-09).
static CORRECTNESS: &[&str] = &[
    "absurd_extreme_comparisons",
    "almost_swapped",
    "approx_constant",
    "async_yields_async",
    "bad_bit_mask",
    "cast_slice_different_sizes",
    "char_indices_as_byte_indices",
    "deprecated_semver",
    "derive_ord_xor_partial_ord",
    "derived_hash_with_manual_eq",
    "eager_transmute",
    "enum_clike_unportable_variant",
    "eq_op",
    "erasing_op",
    "if_let_mutex",
    "ifs_same_cond",
    "impl_hash_borrow_with_str_and_bytes",
    "impossible_comparisons",
    "ineffective_bit_mask",
    "infinite_iter",
    "inherent_to_string_shadow_display",
    "inline_fn_without_body",
    "invalid_regex",
    "inverted_saturating_sub",
    "invisible_characters",
    "iter_next_loop",
    "iter_skip_zero",
    "iterator_step_by_zero",
    "let_underscore_lock",
    "lint_groups_priority",
    "match_str_case_mismatch",
    "mem_replace_with_uninit",
    "min_max",
    "mistyped_literal_suffixes",
    "modulo_one",
    "mut_from_ref",
    "never_loop",
    "non_octal_unix_permissions",
    "nonsensical_open_options",
    "not_unsafe_ptr_arg_deref",
    "option_env_unwrap",
    "out_of_bounds_indexing",
    "panicking_overflow_checks",
    "panicking_unwrap",
    "possible_missing_comma",
    "read_line_without_trim",
    "recursive_format_impl",
    "redundant_comparisons",
    "reversed_empty_ranges",
    "self_assignment",
    "serde_api_misuse",
    "size_of_in_element_count",
    "suspicious_splitn",
    "transmute_null_to_fn",
    "transmuting_null",
    "uninit_assumed_init",
    "uninit_vec",
    "unit_cmp",
    "unit_hash",
    "unit_return_expecting_ord",
    "unsound_collection_transmute",
    "unused_io_amount",
    "useless_attribute",
    "vec_resize_to_zero",
    "while_immutable_condition",
    "wrong_transmute",
    "zst_offset",
];

/// `clippy::suspicious` as of clippy for rustc 1.101 (2026-09).
static SUSPICIOUS: &[&str] = &[
    "almost_complete_range",
    "arc_with_non_send_sync",
    "await_holding_invalid_type",
    "await_holding_lock",
    "await_holding_refcell_ref",
    "blanket_clippy_restriction_lints",
    "block_scrutinee",
    "by_ref_peekable_peek",
    "cast_abs_to_unsigned",
    "cast_enum_constructor",
    "cast_enum_truncation",
    "cast_nan_to_int",
    "cast_slice_from_raw_parts",
    "confusing_method_to_numeric_cast",
    "const_is_empty",
    "crate_in_macro_def",
    "crosspointer_transmute",
    "declare_interior_mutable_const",
    "deprecated_clippy_cfg_attr",
    "doc_nested_refdefs",
    "doc_suspicious_footnotes",
    "drop_non_drop",
    "duplicate_mod",
    "duplicated_attributes",
    "empty_docs",
    "empty_line_after_doc_comments",
    "empty_line_after_outer_attr",
    "empty_loop",
    "float_equality_without_abs",
    "for_unbounded_range",
    "forget_non_drop",
    "four_forward_slashes",
    "from_raw_with_void_ptr",
    "incompatible_msrv",
    "ineffective_open_options",
    "infallible_try_from",
    "iter_out_of_bounds",
    "join_absolute_paths",
    "let_underscore_future",
    "lines_filter_map_ok",
    "macro_metavars_in_unsafe",
    "manual_unwrap_or_default",
    "mismatched_bit_width_type",
    "misnamed_getters",
    "misrefactored_assign_op",
    "missing_transmute_annotations",
    "multi_assignments",
    "mut_range_bound",
    "mutable_key_type",
    "needless_character_iteration",
    "needless_maybe_sized",
    "no_effect_replace",
    "non_canonical_clone_impl",
    "non_canonical_partial_ord_impl",
    "octal_escapes",
    "option_zip_none",
    "path_ends_with_ext",
    "permissions_set_readonly_false",
    "pointers_in_nomem_asm_block",
    "possible_missing_else",
    "print_in_format_impl",
    "rc_clone_in_vec_init",
    "redundant_locals",
    "repeat_vec_with_capacity",
    "repr_packed_without_abi",
    "single_range_in_vec_init",
    "size_of_ref",
    "suspicious_arithmetic_impl",
    "suspicious_assignment_formatting",
    "suspicious_command_arg_space",
    "suspicious_doc_comments",
    "suspicious_else_formatting",
    "suspicious_map",
    "suspicious_op_assign_impl",
    "suspicious_open_options",
    "suspicious_to_owned",
    "suspicious_unary_op_formatting",
    "swap_ptr_to_ref",
    "test_attr_in_doctest",
    "type_id_on_box",
    "unconditional_recursion",
    "unnecessary_clippy_cfg",
    "unnecessary_option_map_or_else",
    "unnecessary_result_map_or_else",
    "zero_repeat_side_effects",
    "zombie_processes",
];

/// What the detector knows about a lint a diagnostic names.
#[derive(Debug, Clone, Copy)]
pub struct LintInfo {
    /// `clippy::indexing_slicing`, `rustc::unused_must_use`.
    pub source: Source,
    pub name: &'static str,
    pub precision: Precision,
    pub exposure: Exposure,
    pub questions: &'static [&'static str],
}

impl LintInfo {
    pub fn rule(&self) -> String {
        match self.source {
            Source::Clippy => format!("clippy::{}", self.name),
            Source::Rustc => format!("rustc::{}", self.name),
        }
    }
}

/// The curated lint a diagnostic's `code` names (`clippy::eq_op`,
/// `unused_must_use`), or `None` for anything else (style lints, rustc
/// errors, lints this table leaves out).
pub fn lookup(code: &str) -> Option<LintInfo> {
    let (source, name) = match code.strip_prefix("clippy::") {
        Some(name) => (Source::Clippy, name),
        None => (Source::Rustc, code),
    };
    if let Some(lint) = LINTS
        .iter()
        .find(|lint| lint.source == source && lint.name == name)
    {
        return Some(LintInfo {
            source,
            name: lint.name,
            precision: lint.precision,
            exposure: lint.exposure,
            questions: lint.questions,
        });
    }
    if source == Source::Rustc || EXCLUDED.contains(&name) {
        return None;
    }
    let (group, name) = GROUPS.iter().find_map(|group| {
        group
            .members
            .iter()
            .find(|member| **member == name)
            .map(|member| (group, *member))
    })?;
    Some(LintInfo {
        source,
        name,
        precision: group.precision,
        exposure: Exposure::Any,
        questions: &[],
    })
}

/// The clippy-driver arguments: everything in clippy's default set off,
/// then the curated groups and lints on at `warn` (deny-by-default ones
/// too, so one finding does not stop the crate's other checks), then the
/// excluded group members off again. Later flags win.
pub fn driver_args() -> Vec<String> {
    let mut args = vec!["-A".to_string(), "clippy::all".to_string()];
    let mut warn = |lint: String| {
        args.push("-W".to_string());
        args.push(lint);
    };
    for group in GROUPS {
        warn(format!("clippy::{}", group.name));
    }
    for lint in LINTS {
        warn(match lint.source {
            Source::Clippy => format!("clippy::{}", lint.name),
            Source::Rustc => lint.name.to_string(),
        });
    }
    for name in EXCLUDED {
        args.push("-A".to_string());
        args.push(format!("clippy::{name}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_lint_is_listed_once_with_a_reason_and_questions() {
        let mut seen = HashSet::new();
        for lint in LINTS {
            assert!(
                seen.insert((lint.source, lint.name)),
                "{} listed twice",
                lint.name
            );
            assert!(
                !lint.why.is_empty() && !lint.questions.is_empty(),
                "{}",
                lint.name
            );
            assert!(
                !EXCLUDED.contains(&lint.name),
                "{} is also excluded",
                lint.name
            );
        }
        for name in EXCLUDED {
            assert!(
                GROUPS.iter().any(|g| g.members.contains(name)),
                "{name} is excluded but in no enabled group"
            );
        }
    }

    #[test]
    fn lookup_names_rules_and_takes_group_defaults() {
        let index = lookup("clippy::indexing_slicing").unwrap();
        assert_eq!(index.rule(), "clippy::indexing_slicing");
        assert_eq!(index.exposure, Exposure::Input);
        let eq_op = lookup("clippy::eq_op").unwrap();
        assert_eq!(eq_op.precision, Precision::High, "a correctness member");
        let map = lookup("clippy::suspicious_map").unwrap();
        assert_eq!(map.precision, Precision::Medium, "a suspicious member");
        assert_eq!(
            lookup("unused_must_use").unwrap().rule(),
            "rustc::unused_must_use"
        );
        assert!(
            lookup("clippy::unconditional_recursion").unwrap().source == Source::Clippy
                && lookup("unconditional_recursion").unwrap().source == Source::Rustc,
            "the clippy and rustc lints of one name stay apart"
        );
        assert!(lookup("clippy::needless_return").is_none(), "style");
        assert!(lookup("clippy::empty_docs").is_none(), "excluded");
        assert!(lookup("dead_code").is_none(), "not curated");
        assert!(lookup("E0308").is_none(), "an error code");
    }

    #[test]
    fn driver_args_turn_the_default_set_off_first_and_exclusions_off_last() {
        let args = driver_args();
        assert_eq!(&args[..2], ["-A", "clippy::all"]);
        let pos = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        assert!(pos("clippy::correctness") < pos("clippy::empty_docs"));
        assert!(args.contains(&"unused_must_use".to_string()));
        assert!(args.contains(&"clippy::indexing_slicing".to_string()));
        assert_eq!(args.len() % 2, 0);
    }

    /// Every name the table passes must be a lint the installed toolchain
    /// knows: a misspelt `-W` is only a warning, so the lint would silently
    /// never fire. Runs where clippy is installed.
    #[test]
    fn lint_names_exist_in_the_installed_clippy() {
        let Ok(out) = std::process::Command::new("clippy-driver")
            .args(["-W", "help"])
            .output()
        else {
            eprintln!("clippy-driver not installed; skipped");
            return;
        };
        let help = String::from_utf8_lossy(&out.stdout);
        let known: HashSet<String> = help
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(|name| name.replace('-', "_"))
            .collect();
        if known.is_empty() {
            eprintln!("clippy-driver printed no lints; skipped");
            return;
        }
        let mut missing = Vec::new();
        for lint in LINTS {
            let name = match lint.source {
                Source::Clippy => format!("clippy::{}", lint.name),
                Source::Rustc => lint.name.to_string(),
            };
            if !known.contains(&name) {
                missing.push(name);
            }
        }
        for group in GROUPS {
            for member in group.members {
                let name = format!("clippy::{member}");
                if !known.contains(&name) {
                    missing.push(name);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "unknown to the installed clippy: {missing:?}"
        );
    }
}
