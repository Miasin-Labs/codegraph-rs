//! Extraction module — mirrors `src/extraction/index.ts`.
//!
//! The public surface re-exports everything the TS barrel exported
//! (`ExtractionOrchestrator`, `extractFromSource`, the grammar helpers,
//! `scanDirectory`, `hashContent`, `buildDefaultIgnore`) plus the types the
//! TS files exported individually (extractors, `LanguageExtractor` contract,
//! `generateNodeId`, `isGeneratedFile`).

pub mod dfm_extractor;
pub mod generated_detection;
pub mod grammars;
pub mod ida_c_extractor;
pub mod ida_manifest;
pub mod languages;
pub mod liquid_extractor;
pub mod lwc_template;
pub mod mybatis_extractor;
pub mod orchestrator;
pub mod salesforce_markup;
pub mod svelte_extractor;
pub mod tree_sitter_helpers;
pub mod tree_sitter_types;
pub mod tree_sitter_wrapper;
pub mod unlace_extractor;
pub mod vue_extractor;

// ---- src/extraction/index.ts exports ----
// ---- per-file TS exports re-surfaced at the barrel ----
pub use dfm_extractor::DfmExtractor;
pub use generated_detection::is_generated_file;
// `export { detectLanguage, isSourceFile, isLanguageSupported, isGrammarLoaded,
//  getSupportedLanguages, initGrammars, loadGrammarsForLanguages, loadAllGrammars }
//  from './grammars'` — the whole grammar surface is re-exported (superset).
pub use grammars::*;
pub use ida_c_extractor::{IdaCExtractor, is_ida_generated_c};
pub use ida_manifest::{FuncManifest, parse_failed_addrs, synthesize_stub_nodes};
pub use languages::extractor_for;
pub use liquid_extractor::LiquidExtractor;
pub use lwc_template::LwcTemplateExtractor;
pub use mybatis_extractor::MyBatisExtractor;
pub use orchestrator::{
    ChangedFiles,
    ExtractionOrchestrator,
    FileStats,
    IndexPhase,
    IndexProgress,
    IndexResult,
    ReconcileResult,
    SyncResult,
    build_default_ignore,
    extract_from_source,
    hash_content,
    scan_directory,
};
pub use salesforce_markup::SalesforceMarkupExtractor;
pub use svelte_extractor::{ScriptExtractorLookup, SvelteExtractor};
pub use tree_sitter_helpers::{
    generate_node_id,
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
};
pub use tree_sitter_types::*;
pub use tree_sitter_wrapper::TreeSitterExtractor;
pub use unlace_extractor::{UnlaceCExtractor, is_unlace_c};
pub use vue_extractor::VueExtractor;
