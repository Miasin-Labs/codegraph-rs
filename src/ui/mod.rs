//! Terminal UI — glyph selection and the shimmer progress display.
//!
//! Ported from `src/ui/` (`glyphs.ts`, `types.ts`, `shimmer-progress.ts`,
//! `shimmer-worker.ts` — the worker is folded into `shimmer_progress.rs`
//! as a `std::thread`).

pub mod glyphs;
pub mod shimmer_progress;
pub mod types;

pub use glyphs::{
    _reset_glyphs_cache,
    ASCII_GLYPHS,
    Glyphs,
    UNICODE_GLYPHS,
    get_glyphs,
    supports_unicode,
};
pub use shimmer_progress::{IndexProgress, ShimmerProgress, create_shimmer_progress};
pub use types::{ShimmerMainMessage, ShimmerWorkerMessage};
