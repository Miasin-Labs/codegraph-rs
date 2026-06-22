mod framework_detection;
mod git;
mod ignore;
mod index;
mod index_files;
mod parse;
mod pipeline;
mod progress;
mod reconcile;
mod scan;
mod store;
mod sync;
#[cfg(test)]
mod tests;

pub use ignore::build_default_ignore;
pub use parse::extract_from_source;
pub use pipeline::ExtractionOrchestrator;
pub use progress::{
    ChangedFiles,
    FileStats,
    IndexPhase,
    IndexProgress,
    IndexResult,
    ReconcileResult,
    SyncResult,
};
pub use scan::scan_directory;
pub use store::hash_content;
