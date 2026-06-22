//! Extraction Tests
//!
//! Port of `__tests__/extraction.test.ts` (the extraction crown-jewel suite)
//! plus the extraction half of `__tests__/object-literal-methods.test.ts`.
//!
//! Test names are `<describe>_<it>` snake-cased. Fixture source strings are
//! byte-identical to the TS suite. Tests that needed `CodeGraph.init` +
//! reference resolution (IDA callers/callees, store-action caller resolution)
//! are deferred to the public-API wiring wave and noted in
//! `notes/extraction-orchestrator.md`; the `Full Indexing` describe block is
//! ported here against `ExtractionOrchestrator` + `QueryBuilder` directly
//! (the layers under `CodeGraph`).

#[path = "extraction_test/mod.rs"]
mod extraction_test;
