//! Garbage collection of the shard store.
//!
//! 1. Forget projects whose checkout no longer exists.
//! 2. Remove shards no recorded project uses, or none has used (recorded)
//!    within `max_age`.
//! 3. While the store is over `max_total_bytes`, remove the least recently
//!    used shards.
//! 4. Remove shard directories the registry doesn't know, and build
//!    leftovers (`.tmp-`/`.old-`) older than an hour.
//!
//! A shard whose build lock is held is never touched, and nothing but shard
//! directories (those with a `meta.json`) and the builder's own leftovers is
//! ever deleted.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::error::DepsResult;
use super::model::{DepKey, Ecosystem};
use super::registry::Registry;
use super::shard::ShardMeta;
use super::store::{DepsHome, META_FILE, OLD_PREFIX, StoreLock, TMP_PREFIX};

/// Build leftovers younger than this may belong to a running build.
const LEFTOVER_MIN_AGE: Duration = Duration::from_secs(3600);

/// What `gc` may remove.
#[derive(Debug, Clone, Copy)]
pub struct GcPolicy {
    /// Shards no project has recorded using for this long go.
    pub max_age: Duration,
    /// Keep the shard store under this many bytes (LRU).
    pub max_total_bytes: Option<u64>,
    /// Report only.
    pub dry_run: bool,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(30 * 24 * 3600),
            max_total_bytes: Some(10 * 1024 * 1024 * 1024),
            dry_run: false,
        }
    }
}

/// Why a shard was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GcReason {
    /// No recorded project uses it.
    Unused,
    /// Not used within `max_age`.
    Stale,
    /// Least recently used while over the size cap.
    SizeCap,
    /// On disk but unknown to the registry.
    Orphan,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovedShard {
    pub key: DepKey,
    pub bytes: u64,
    pub reason: GcReason,
}

/// What `gc` did (or, dry, would do).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub dry_run: bool,
    pub projects_forgotten: Vec<String>,
    pub removed: Vec<RemovedShard>,
    pub leftovers_removed: usize,
    pub bytes_freed: u64,
    pub bytes_kept: u64,
    pub versions_forgotten: usize,
}

pub fn gc(
    home: &DepsHome,
    registry: &Registry,
    policy: &GcPolicy,
    now: SystemTime,
) -> DepsResult<GcReport> {
    let mut report = GcReport {
        dry_run: policy.dry_run,
        ..GcReport::default()
    };

    for project in registry.projects()? {
        if !Path::new(&project.root).is_dir() {
            if !policy.dry_run {
                registry.delete_project(&project.root)?;
            }
            report.projects_forgotten.push(project.root);
        }
    }
    let forgotten: HashSet<String> = report.projects_forgotten.iter().cloned().collect();

    let now_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let cutoff = now_ms - policy.max_age.as_millis() as i64;
    let shards = registry.shards()?; // least recently used first
    let mut known: HashSet<DepKey> = HashSet::new();
    let mut kept: Vec<(DepKey, u64)> = Vec::new();
    for row in &shards {
        let key = row.key();
        known.insert(key.clone());
        let users = if policy.dry_run {
            // Dry runs didn't delete the vanished projects; count as if.
            registry
                .users_of(&key)?
                .iter()
                .filter(|root| !forgotten.contains(*root))
                .count() as u64
        } else {
            row.users
        };
        let reason = if users == 0 {
            Some(GcReason::Unused)
        } else if row.last_used < cutoff {
            Some(GcReason::Stale)
        } else {
            None
        };
        match reason {
            Some(reason) => remove_shard(
                home,
                registry,
                &key,
                row.shard_bytes,
                reason,
                policy,
                &mut report,
            )?,
            None => kept.push((key, row.shard_bytes)),
        }
    }

    if let Some(cap) = policy.max_total_bytes {
        let mut total: u64 = kept.iter().map(|(_, b)| *b).sum();
        let mut survivors = Vec::new();
        for (key, bytes) in kept {
            if total > cap {
                let before = report.removed.len();
                remove_shard(
                    home,
                    registry,
                    &key,
                    bytes,
                    GcReason::SizeCap,
                    policy,
                    &mut report,
                )?;
                if report.removed.len() > before {
                    total = total.saturating_sub(bytes);
                    continue;
                }
            }
            survivors.push((key, bytes));
        }
        kept = survivors;
    }
    report.bytes_kept = kept.iter().map(|(_, b)| *b).sum();

    sweep_disk(home, registry, &known, policy, now, &mut report)?;
    if !policy.dry_run {
        report.versions_forgotten = registry.delete_unused_packages()?;
    }
    Ok(report)
}

fn remove_shard(
    home: &DepsHome,
    registry: &Registry,
    key: &DepKey,
    bytes: u64,
    reason: GcReason,
    policy: &GcPolicy,
    report: &mut GcReport,
) -> DepsResult<()> {
    if policy.dry_run {
        report.removed.push(RemovedShard {
            key: key.clone(),
            bytes,
            reason,
        });
        report.bytes_freed += bytes;
        return Ok(());
    }
    let Some(_lock) = StoreLock::try_acquire(&home.shard_lock_path(key))? else {
        return Ok(()); // being rebuilt right now
    };
    let dir = home.shard_dir(key);
    if dir.join(META_FILE).is_file() {
        fs::remove_dir_all(&dir)?;
    }
    registry.mark_missing(key)?;
    report.removed.push(RemovedShard {
        key: key.clone(),
        bytes,
        reason,
    });
    report.bytes_freed += bytes;
    Ok(())
}

/// Shard directories no recorded project uses that the registry doesn't
/// list as shards, and stale build leftovers.
fn sweep_disk(
    home: &DepsHome,
    registry: &Registry,
    known: &HashSet<DepKey>,
    policy: &GcPolicy,
    now: SystemTime,
    report: &mut GcReport,
) -> DepsResult<()> {
    for ecosystem in Ecosystem::ALL {
        let Ok(entries) = fs::read_dir(home.ecosystem_dir(ecosystem)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue; // other passes' per-package files live here too
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(TMP_PREFIX) || name.starts_with(OLD_PREFIX) {
                let old_enough = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age >= LEFTOVER_MIN_AGE);
                if old_enough {
                    if !policy.dry_run {
                        fs::remove_dir_all(&path)?;
                    }
                    report.leftovers_removed += 1;
                }
                continue;
            }
            let Some(meta) = ShardMeta::read(&path) else {
                continue;
            };
            let key = meta.key();
            if known.contains(&key) || home.shard_dir(&key) != path {
                continue;
            }
            if let Some(row) = registry.package(&key)? {
                if row.users > 0 {
                    // Published, but the registry missed it (a builder died
                    // between publishing and recording): record it.
                    if !policy.dry_run && !StoreLock::is_held(&home.shard_lock_path(&key)) {
                        registry.mark_built(&meta)?;
                    }
                    continue;
                }
            }
            if policy.dry_run {
                report.removed.push(RemovedShard {
                    key,
                    bytes: meta.db_bytes,
                    reason: GcReason::Orphan,
                });
                report.bytes_freed += meta.db_bytes;
                continue;
            }
            let Some(_lock) = StoreLock::try_acquire(&home.shard_lock_path(&key))? else {
                continue;
            };
            fs::remove_dir_all(&path)?;
            report.bytes_freed += meta.db_bytes;
            report.removed.push(RemovedShard {
                key,
                bytes: meta.db_bytes,
                reason: GcReason::Orphan,
            });
        }
    }
    Ok(())
}
