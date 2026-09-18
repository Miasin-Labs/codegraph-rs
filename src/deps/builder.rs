//! Build the pending shards of one project (or all), within a time budget,
//! keeping the registry's shard states in step.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::error::DepsResult;
use super::model::DepKey;
use super::registry::{PendingScope, PendingShard, Registry};
use super::scope::ShardLimits;
use super::shard::{BuildOutcome, BuildRequest, ShardMeta, build_shard};
use super::store::DepsHome;

/// Knobs for [`build_pending`].
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    pub limits: ShardLimits,
    /// Stop starting new shards after this long (`None`: no limit). A shard
    /// never gets more time than what is left.
    pub budget: Option<Duration>,
    /// Rebuild shards that are already up to date.
    pub force: bool,
    /// Only the scope's direct dependencies.
    pub direct_only: bool,
}

/// One shard's result, reported as it finishes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum ShardResult {
    Built {
        meta: Box<ShardMeta>,
    },
    UpToDate {
        meta: Box<ShardMeta>,
    },
    /// Another builder holds its lock.
    Locked {
        key: DepKey,
    },
    /// Nothing to index (recorded `unavailable`, retried after an
    /// extractor change).
    NoSources {
        key: DepKey,
    },
    Failed {
        key: DepKey,
        error: String,
    },
    /// None of the recorded source directories exists any more.
    SourceGone {
        key: DepKey,
    },
}

/// What [`build_pending`] did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub results: Vec<ShardResult>,
    /// Pending shards not attempted because the budget ran out.
    pub remaining: usize,
    pub elapsed_ms: u64,
}

impl BuildReport {
    pub fn built(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r, ShardResult::Built { .. }))
            .count()
    }

    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r, ShardResult::Failed { .. }))
            .count()
    }
}

/// Build what [`Registry::pending`] lists for `scope`, most useful first.
/// `on_result` sees each shard as it completes.
pub async fn build_pending(
    home: &DepsHome,
    registry: &Registry,
    scope: PendingScope<'_>,
    options: &BuildOptions,
    on_result: &mut dyn FnMut(&ShardResult),
) -> DepsResult<BuildReport> {
    let started = Instant::now();
    let mut pending = registry.pending(scope, options.force)?;
    if options.direct_only {
        pending.retain(|p| p.direct);
    }
    let mut report = BuildReport::default();
    for (index, shard) in pending.iter().enumerate() {
        let left = options
            .budget
            .map(|budget| budget.saturating_sub(started.elapsed()));
        if left.is_some_and(|l| l.is_zero()) {
            report.remaining = pending.len() - index;
            break;
        }
        let mut limits = options.limits;
        if let Some(left) = left {
            limits.time_ms = limits.time_ms.min(left.as_millis() as u64).max(1);
        }
        let result = build_one(home, registry, shard, limits, options.force).await?;
        on_result(&result);
        report.results.push(result);
    }
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

async fn build_one(
    home: &DepsHome,
    registry: &Registry,
    shard: &PendingShard,
    limits: ShardLimits,
    force: bool,
) -> DepsResult<ShardResult> {
    let key = &shard.key;
    let Some(source_dir) = shard.source_dirs.iter().find(|d| d.is_dir()) else {
        registry.mark_missing(key)?;
        return Ok(ShardResult::SourceGone { key: key.clone() });
    };
    let previous = shard.state;
    registry.mark_building(key)?;
    let outcome = build_shard(
        home,
        &BuildRequest {
            key,
            source: &shard.source,
            source_dir,
            limits,
            force,
        },
    )
    .await;
    Ok(match outcome {
        BuildOutcome::Built(meta) => {
            registry.mark_built(&meta)?;
            ShardResult::Built {
                meta: Box::new(meta),
            }
        }
        BuildOutcome::UpToDate(meta) => {
            registry.mark_built(&meta)?;
            ShardResult::UpToDate {
                meta: Box::new(meta),
            }
        }
        BuildOutcome::Locked => {
            // The other builder records its own result (if it dies, the row
            // stays `building` and the next build retries it). Meanwhile a
            // shard it is replacing is still there to read.
            if previous.has_shard() {
                if let Some(meta) = ShardMeta::read(&home.shard_dir(key)) {
                    registry.mark_built(&meta)?;
                }
            }
            ShardResult::Locked { key: key.clone() }
        }
        BuildOutcome::NoSources => {
            registry.mark_no_sources(key)?;
            ShardResult::NoSources { key: key.clone() }
        }
        BuildOutcome::Failed(error) => {
            registry.mark_failed(key, &error)?;
            ShardResult::Failed {
                key: key.clone(),
                error,
            }
        }
    })
}
