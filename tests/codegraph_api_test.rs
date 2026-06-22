#[path = "codegraph_api_test/support.rs"]
mod support;

pub(crate) use support::*;

include!("codegraph_api_test/sync_functionality.rs");
include!("codegraph_api_test/git_based_sync.rs");
include!("codegraph_api_test/concurrent_locking.rs");
include!("codegraph_api_test/path_traversal_prevention.rs");
include!("codegraph_api_test/foundation_facade.rs");
include!("codegraph_api_test/watcher_integration.rs");
include!("codegraph_api_test/end_to_end.rs");
