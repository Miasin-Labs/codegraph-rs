//! Reference Resolution Orchestrator
//!
//! Coordinates all reference resolution strategies.
//! Ported from `src/resolution/index.ts` (the `ReferenceResolver` class +
//! `createResolver`; the `export * from './types'` re-export lives in
//! `resolution/mod.rs`).

mod batch;
mod cache;
mod context;
mod edges;
mod engine;
#[cfg(feature = "gpu")]
mod gpu_batch;
#[cfg(feature = "gpu")]
mod gpu_exact;
#[cfg(feature = "gpu")]
mod gpu_fuzzy;
#[cfg(feature = "gpu")]
mod gpu_method;
mod lookup;
#[cfg(not(feature = "gpu"))]
mod parallel;
mod persist;
mod policy;
mod post_extract;
mod resolution;
#[cfg(not(feature = "gpu"))]
mod snapshot;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::sync::Arc;

pub use context::ResolverContext;

use super::frameworks::detect_frameworks;
use super::types::{FrameworkResolver, ResolutionContext};
use crate::db::QueryBuilder;

/// Reference Resolver
///
/// Orchestrates reference resolution using multiple strategies.
pub struct ReferenceResolver {
    context: ResolverContext,
    frameworks: RefCell<Arc<Vec<Box<dyn FrameworkResolver>>>>,
}

impl ReferenceResolver {
    pub fn new(project_root: impl Into<String>, queries: QueryBuilder) -> Self {
        ReferenceResolver {
            context: ResolverContext::new(project_root.into(), queries),
            frameworks: RefCell::new(Arc::new(Vec::new())),
        }
    }

    /// Initialize the resolver (detect frameworks, etc.)
    pub fn initialize(&self) {
        *self.frameworks.borrow_mut() = Arc::new(detect_frameworks(&self.context));
        self.clear_caches();
    }

    /// The production resolution context (exposed for wiring — the callback
    /// synthesizer and tests use it; TS kept it private but passed it to the
    /// same collaborators).
    pub fn context(&self) -> &dyn ResolutionContext {
        &self.context
    }
}

/// Create a reference resolver instance
pub fn create_resolver(
    project_root: impl Into<String>,
    queries: QueryBuilder,
) -> ReferenceResolver {
    let resolver = ReferenceResolver::new(project_root, queries);
    resolver.initialize();
    resolver
}
