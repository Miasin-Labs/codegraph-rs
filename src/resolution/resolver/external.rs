//! The external resolution pass, run with this resolver's project context
//! (see [`crate::resolution::external`]).

use std::path::Path;

use super::ReferenceResolver;
#[cfg(not(feature = "gpu"))]
use super::snapshot::ResolverSnapshot;
use crate::error::Result;
use crate::resolution::external::{self, ExternalOptions, ExternalRefs, ExternalReport};

/// Most threads one full pass resolves on (each opens its own graphs).
#[cfg(not(feature = "gpu"))]
const MAX_WORKERS: usize = 8;

impl ReferenceResolver {
    /// Resolve what in-project resolution left unresolved into the graphs
    /// the project reaches. A pass that examines every reference runs over
    /// the in-memory snapshot of the project graph (as batched resolution
    /// does); a handful of references use the query-backed context.
    pub fn resolve_external(
        &self,
        refs: ExternalRefs<'_>,
        options: &ExternalOptions,
    ) -> Result<ExternalReport> {
        let queries = &self.context.queries;
        let root = Path::new(&self.context.project_root);
        let plan = external::pass::plan(queries, root, refs, options)?;
        #[cfg(not(feature = "gpu"))]
        if plan.examines_everything() {
            let at = std::time::Instant::now();
            let snapshot = ResolverSnapshot::build(&self.context.project_root, queries)?;
            external::pass::trace("snapshot", at);
            let workers = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(MAX_WORKERS);
            return plan.execute_parallel(queries, snapshot.context(), workers);
        }
        let at = std::time::Instant::now();
        let report = plan.execute(queries, &self.context);
        external::pass::trace("resolve (sequential)", at);
        report
    }
}
