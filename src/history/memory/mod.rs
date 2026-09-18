//! Cross-session memory: what earlier agent sessions in a repository
//! already explored, looked up, broke and fixed — so the next one doesn't
//! re-discover it.
//!
//! Built from the same adapters and redactor as the tool-call flywheel
//! (tables in the history DB, never the per-project index):
//!
//! * **sessions** (hashed native id, source, parent for sub-agents) split
//!   into **episodes** at human prompts; sub-agent calls roll up into the
//!   root session's episode running at the time;
//! * per episode, **touches** (repo-relative file, read/edit/search, count,
//!   bytes, the index's content hash at the touch when known),
//!   **lookups** (identifier → file:line read next; the identifier in the
//!   clear only when the repo's index knows it, else a hash) and
//!   **outcomes** (build/test/commit: masked command template, pass/fail,
//!   first error codes, hash of the first error lines);
//! * per repository, **coedits** (files edited in one episode, merged with
//!   git co-change) and a ≤1 KB **digest** for the prompt hook.
//!
//! Writes happen only in `codegraph history ingest` ([`writer`], then
//! [`rollup`]); readers ([`recall`], [`digest::read_digest`]) open the store
//! read-only and run the indexed, bounded [`queries`].

mod digest;
mod index_probe;
mod queries;
mod recall;
mod report;
mod rollup;
mod writer;

pub use digest::{DIGEST_BUDGET, DigestRead, DigestStatus, last_ingest_ms, read_digest};
pub(crate) use index_probe::IndexProbe;
pub(crate) use queries::Queries;
pub use recall::{About, RecallRequest, recall_at, recall_with_atlas};
pub use report::{
    EpisodeRow,
    FailureRow,
    FileTouch,
    PairRow,
    RECALL_BUDGET,
    RecallReport,
    RelatedRecall,
    SymbolRow,
};
pub(crate) use rollup::roll_up;
pub(crate) use writer::MemoryWriter;
pub use writer::WriteStats;

#[cfg(test)]
mod tests;
