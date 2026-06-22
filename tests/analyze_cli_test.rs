//! CLI integration tests for the `codegraph analyze` subcommand family.
//!
//! Behavior clusters live under `tests/analyze_cli_test/`; this wrapper keeps
//! `cargo test --test analyze_cli_test` and the historical test names intact.

#[path = "analyze_cli_test/fixture.rs"]
mod fixture;
#[path = "analyze_cli_test/support.rs"]
mod support;

use fixture::{git, init_close_fixture, init_fixture, write_lcov};
use support::{
    bin,
    names_of,
    run_analyze_envelope,
    run_analyze_json,
    run_cli,
    stderr_str,
    stdout_str,
    temp_project,
};

include!("analyze_cli_test/complexity.rs");
include!("analyze_cli_test/communities.rs");
include!("analyze_cli_test/dominators.rs");
include!("analyze_cli_test/slice.rs");
include!("analyze_cli_test/cycles.rs");
include!("analyze_cli_test/impact.rs");
include!("analyze_cli_test/taint.rs");
include!("analyze_cli_test/query.rs");
include!("analyze_cli_test/shared_contract.rs");
include!("analyze_cli_test/co_change.rs");
include!("analyze_cli_test/coverage.rs");
include!("analyze_cli_test/validate.rs");
include!("analyze_cli_test/traits.rs");
include!("analyze_cli_test/centrality.rs");
include!("analyze_cli_test/export.rs");
include!("analyze_cli_test/type_flow.rs");
include!("analyze_cli_test/generics.rs");
include!("analyze_cli_test/boundaries.rs");
include!("analyze_cli_test/capabilities.rs");
include!("analyze_cli_test/schema.rs");
include!("analyze_cli_test/stats.rs");
