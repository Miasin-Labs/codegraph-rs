//! Reference resolution (ported from `src/resolution/`).
//!
//! `src/resolution/index.ts` exports `ReferenceResolver`, `createResolver`
//! and re-exports everything from `./types` — mirrored below. Leaf-module
//! functions (`matchReference`, `resolveViaImport`, …) were NOT re-exported
//! by the TS barrel; reach them via their modules
//! (`crate::resolution::name_matcher::match_reference`, etc.).

pub mod callback_synthesizer;
pub mod frameworks;
pub mod go_module;
pub mod import_resolver;
pub mod lru_cache;
pub mod name_matcher;
pub mod path_aliases;
pub mod resolver;
pub mod strip_comments;
pub mod swift_objc_bridge;
pub mod types;
pub mod workspace_packages;

// Re-export types (TS `export * from './types'`)
// The module's public classes/functions (TS `export class ReferenceResolver`
// / `export function createResolver`).
pub use resolver::{ReferenceResolver, ResolverContext, create_resolver};
pub use types::*;
