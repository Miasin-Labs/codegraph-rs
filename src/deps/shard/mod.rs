//! Shards: one self-contained codegraph database per dependency version,
//! shared by every project that pins it.

mod build;
mod handle;
mod meta;

pub use build::{BuildOutcome, BuildRequest, build_shard};
pub use handle::ShardHandle;
pub use meta::{META_FORMAT, ShardCounts, ShardMeta};
