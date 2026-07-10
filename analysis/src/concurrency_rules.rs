//! Per-language rules for concurrency / control-plane lint extraction.
//!
//! Where [`crate::cfg_rules`] and [`crate::dataflow_rules`] classify *syntax*
//! (which tree-sitter node is a loop, an assignment, a call), this table
//! classifies *concurrency semantics* — which method calls deliver a message
//! best-effort vs. guaranteed, which calls remove pending state, and which
//! names mark a terminal "completion" signal. The [`crate::concurrency`]
//! detector consumes this to surface the lossy-send / split-transaction bug
//! classes (the JFC `try_send` family) without re-encoding the rules per call
//! site.
//!
//! Conventions match the sibling rule tables: a `&'static` struct per language,
//! looked up by language identifier, with method names compared verbatim
//! (`completion_markers` are the exception — matched case-insensitively as
//! substrings, since event/variant names embed them, e.g. `AllComplete`).

/// Language-specific classification of concurrency-relevant calls.
pub struct ConcurrencyRules {
    /// Method names whose call is a **best-effort (lossy) send**: it returns
    /// immediately and drops the message when the channel is full or closed
    /// (tokio's `try_send`, broadcast's `try_broadcast`). Delivering a value a
    /// downstream awaiter *must* receive through one of these is the core
    /// hazard.
    pub lossy_send_methods: &'static [&'static str],
    /// Method / wrapper names that are **guaranteed delivery**: they block or
    /// await until the message is accepted, or surface a hard error instead of
    /// silently dropping (`blocking_send`, and project wrappers like
    /// `send_critical`). A lossy send paired with one of these for a terminal
    /// event is the "result dropped but turn reported complete" shape.
    pub guaranteed_send_methods: &'static [&'static str],
    /// Method names that **remove or extract pending state** from a container.
    /// Popping a pending request *before* re-delivering it (then delivering
    /// best-effort) loses it on failure — the split-transaction hazard.
    pub state_removal_methods: &'static [&'static str],
    /// Lowercased substrings marking a value/event as a **terminal completion
    /// signal**. Matched case-insensitively against the called method/event
    /// name so `AllComplete`, `mark_done`, `finish_turn` all hit.
    pub completion_markers: &'static [&'static str],
    /// Function / method names that **spawn a detached async task** (ownership
    /// and cancellation lineage; consumed by later passes, not the v1 detector).
    pub spawn_methods: &'static [&'static str],
}

impl ConcurrencyRules {
    /// Look up rules for a language by its identifier. Returns `None` for
    /// languages without a concurrency model encoded yet — callers treat that
    /// as "no concurrency lint available", not an error.
    pub fn for_language(lang: &str) -> Option<&'static ConcurrencyRules> {
        match lang {
            "rust" => Some(&RUST_CONCURRENCY_RULES),
            "typescript" | "javascript" | "arkts" => Some(&TYPESCRIPT_CONCURRENCY_RULES),
            "go" => Some(&GO_CONCURRENCY_RULES),
            _ => None,
        }
    }

    /// True if `method` is a best-effort/lossy send under these rules.
    pub fn is_lossy_send(&self, method: &str) -> bool {
        self.lossy_send_methods.contains(&method)
    }

    /// True if `method` is a guaranteed-delivery send under these rules.
    pub fn is_guaranteed_send(&self, method: &str) -> bool {
        self.guaranteed_send_methods.contains(&method)
    }

    /// True if `method` removes/extracts pending state from a container.
    pub fn is_state_removal(&self, method: &str) -> bool {
        self.state_removal_methods.contains(&method)
    }

    /// True if `name` reads as a terminal completion signal (case-insensitive
    /// substring match against the markers).
    pub fn looks_like_completion(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.completion_markers.iter().any(|m| lower.contains(m))
    }
}

// ─── Rust ──────────────────────────────────────────────────────────────────

static RUST_CONCURRENCY_RULES: ConcurrencyRules = ConcurrencyRules {
    // tokio::sync::mpsc::Sender::try_send, broadcast::Sender::try_broadcast,
    // watch has no lossy variant. `unbounded_send` is deliberately excluded:
    // it only fails on a closed channel, so flagging it would be mostly noise.
    lossy_send_methods: &["try_send", "try_broadcast"],
    // `send` is intentionally NOT here: `mpsc::Sender::send(..).await` is
    // guaranteed, but `std::sync::mpsc::Sender::send` and a bare unawaited
    // `send` are not — the detector reasons about `.await` separately. The
    // names below are unambiguously guaranteed-or-error.
    guaranteed_send_methods: &["blocking_send", "send_critical", "send_blocking"],
    state_removal_methods: &[
        "pop",
        "pop_front",
        "pop_back",
        "pop_first",
        "pop_last",
        "remove",
        "swap_remove",
        "remove_entry",
        "take",
        "take_pending",
    ],
    completion_markers: &[
        "complete",
        "completed",
        "allcomplete",
        "done",
        "finish",
        "finished",
        "terminal",
        "resolve",
        "resolved",
    ],
    spawn_methods: &["spawn", "spawn_blocking", "spawn_local"],
};

// ─── TypeScript / JavaScript ─────────────────────────────────────────────────

static TYPESCRIPT_CONCURRENCY_RULES: ConcurrencyRules = ConcurrencyRules {
    // Best-effort enqueue patterns on common async primitives / event buses.
    lossy_send_methods: &["tryEmit", "tryPost", "offer"],
    guaranteed_send_methods: &["sendCritical", "flush"],
    state_removal_methods: &["pop", "shift", "splice", "delete"],
    completion_markers: &[
        "complete",
        "completed",
        "done",
        "finish",
        "finished",
        "resolve",
        "resolved",
    ],
    spawn_methods: &["setImmediate", "queueMicrotask"],
};

// ─── Go ──────────────────────────────────────────────────────────────────────

static GO_CONCURRENCY_RULES: ConcurrencyRules = ConcurrencyRules {
    // Go's lossy send is the `select { case ch <- v: ... default: }` idiom
    // rather than a named method; method-name matching only catches helper
    // wrappers. Kept minimal until a Go-specific select-walker lands.
    lossy_send_methods: &["TrySend"],
    guaranteed_send_methods: &["SendCritical"],
    state_removal_methods: &["Pop", "Remove", "Delete"],
    completion_markers: &["complete", "completed", "done", "finish", "finished"],
    spawn_methods: &["go"],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_classifies_send_families() {
        let r = ConcurrencyRules::for_language("rust").unwrap();
        assert!(r.is_lossy_send("try_send"));
        assert!(!r.is_lossy_send("send"));
        assert!(r.is_guaranteed_send("send_critical"));
        assert!(r.is_state_removal("pop_front"));
        assert!(r.is_state_removal("remove"));
    }

    #[test]
    fn completion_markers_are_case_insensitive_substrings() {
        let r = ConcurrencyRules::for_language("rust").unwrap();
        assert!(r.looks_like_completion("AllComplete"));
        assert!(r.looks_like_completion("mark_turn_done"));
        assert!(r.looks_like_completion("FINISHED"));
        assert!(!r.looks_like_completion("set_in_progress"));
    }

    #[test]
    fn unknown_language_has_no_rules() {
        assert!(ConcurrencyRules::for_language("haskell").is_none());
    }
}
