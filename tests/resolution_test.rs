//! Resolution pipeline integration tests.
//!
//! Ports the full-pipeline suites of `__tests__/resolution.test.ts` that the
//! import/match agents left to the stitch owner (Framework Detection,
//! Integration Tests, tsconfig path aliases, re-export chain following, the
//! C/C++ end-to-end include case), the `getApplicableFrameworks` suite from
//! `__tests__/frameworks.test.ts`, the resolution half of
//! `__tests__/object-literal-methods.test.ts`, and the resolution parts of
//! `__tests__/pr19-improvements.test.ts` (Best-Candidate Resolution +
//! Resolution Warm Caches).
//!
//! The TS suites drive `CodeGraph.init` + `indexAll`; the extraction
//! orchestrator is still in flight, so these fixtures insert the
//! extraction-shaped nodes/files the TS extractors would produce (same ids,
//! kinds, qualified-name schemes) over REAL files in a tempdir and REAL
//! SQLite — no mocks. The resolution side (import maps, alias/workspace/
//! go.mod loading, barrel chases, name matching, edge persistence) runs the
//! real production code end-to-end via `create_resolver` +
//! `resolve_and_persist_batched`.

#[path = "resolution_test/aliases.rs"]
mod aliases;
#[path = "resolution_test/barrels.rs"]
mod barrels;
#[path = "resolution_test/builtins.rs"]
mod builtins;
#[path = "resolution_test/component_re_exports.rs"]
mod component_re_exports;
#[path = "resolution_test/cpp_includes.rs"]
mod cpp_includes;
#[path = "resolution_test/dotnet.rs"]
mod dotnet;
#[path = "resolution_test/fixture.rs"]
mod fixture;
#[path = "resolution_test/framework_detection.rs"]
mod framework_detection;
#[path = "resolution_test/framework_selection.rs"]
mod framework_selection;
#[path = "resolution_test/go.rs"]
mod go;
#[path = "resolution_test/integration.rs"]
mod integration;
#[path = "resolution_test/jvm.rs"]
mod jvm;
#[path = "resolution_test/progress.rs"]
mod progress;
#[path = "resolution_test/re_exports.rs"]
mod re_exports;
#[path = "resolution_test/store_actions.rs"]
mod store_actions;
#[path = "resolution_test/type_alias_members.rs"]
mod type_alias_members;
