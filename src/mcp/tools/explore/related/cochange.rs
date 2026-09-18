//! Git co-change with the seed files: which files changed in the same commits.
//!
//! One `git log` over the repository's recent history, started when explore
//! starts — before its search, whose duration hides the read — and collected
//! under a hard deadline, so explore never waits on a slow repository. It
//! needs no pathspec, so it can start before the seed files are known; the
//! commits are filtered to the seeds afterwards. Read-only
//! (`GIT_OPTIONAL_LOCKS=0`); a missing `git`, a non-repository, or a timeout
//! all mean "no co-change signal".

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Most recent commits read per explore call.
const MAX_COMMITS: usize = 400;
/// Commits touching more files than this are sweeping refactors, formatting
/// passes, or vendor drops: they say nothing about coupling.
const MAX_COMMIT_FILES: usize = 40;
/// Hard ceiling on the wait for `git log`, counted from spawn.
const DEADLINE: Duration = Duration::from_millis(1_500);
/// Output kept from `git log`; the rest is drained unread.
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// Commit record separator in the `--format` below.
const RECORD_SEPARATOR: char = '\x1e';

/// A running `git log`, collected with [`CoChangeProbe::collect`]. Dropping
/// it uncollected kills the process.
pub(in crate::mcp::tools::explore) struct CoChangeProbe {
    child: Child,
    reader: Option<JoinHandle<Vec<u8>>>,
    started: Instant,
}

impl CoChangeProbe {
    /// Start `git log` over the most recent commits of the repository holding
    /// `root`, listing changed paths relative to `root`. `None` when git
    /// cannot be started.
    pub(in crate::mcp::tools::explore) fn spawn(root: &Path) -> Option<Self> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "log",
                "--no-merges",
                "--relative",
                "--name-only",
                "--format=%x1e",
                &format!("-n{MAX_COMMITS}"),
            ])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::Builder::new()
            .name("explore-cochange".into())
            .spawn(move || {
                let mut kept = Vec::new();
                let mut chunk = [0u8; 16 * 1024];
                while let Ok(read) = stdout.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..read.min(room)]);
                }
                kept
            });
        let Ok(reader) = reader else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        Some(Self {
            child,
            reader: Some(reader),
            started: Instant::now(),
        })
    }

    /// The commits' file lists, or `None` when git failed or ran past the
    /// deadline (the process is killed then).
    pub(super) fn collect(mut self) -> Option<Vec<Vec<String>>> {
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let output = self.reader.take()?.join().ok()?;
                    return status.success().then(|| parse_log(&output));
                }
                Ok(None) if self.started.elapsed() < DEADLINE => {
                    std::thread::sleep(Duration::from_millis(3));
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }
}

impl Drop for CoChangeProbe {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Split `git log --format=%x1e --name-only` output into per-commit file lists.
pub(super) fn parse_log(output: &[u8]) -> Vec<Vec<String>> {
    String::from_utf8_lossy(output)
        .split(RECORD_SEPARATOR)
        .map(|record| {
            record
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|files| !files.is_empty())
        .collect()
}

/// Co-change evidence for one neighbour file.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct CoChangeEntry {
    /// Seed-weighted commit mass, damped by commit size.
    pub weight: f64,
    /// Commits shared with any seed.
    pub commits: usize,
    /// The seed rank it shared the most commits with.
    pub seed_rank: usize,
}

/// Co-change evidence for every file that shared a commit with a seed.
#[derive(Clone, Debug, Default)]
pub(super) struct CoChange {
    pub by_file: HashMap<String, CoChangeEntry>,
}

impl CoChange {
    /// Tally commits: every non-seed, non-excluded file in a commit that also
    /// touched a seed gains weight from each seed in it, less for big commits.
    pub(super) fn from_commits(
        commits: &[Vec<String>],
        seeds: &[&str],
        excluded: &HashSet<&str>,
    ) -> Self {
        let seed_rank: HashMap<&str, usize> = seeds
            .iter()
            .enumerate()
            .map(|(rank, seed)| (*seed, rank))
            .collect();
        let mut by_file: HashMap<String, CoChangeEntry> = HashMap::new();
        let mut per_seed: HashMap<String, HashMap<usize, usize>> = HashMap::new();
        for files in commits {
            if files.len() > MAX_COMMIT_FILES {
                continue;
            }
            let mut ranks: Vec<usize> = files
                .iter()
                .filter_map(|file| seed_rank.get(file.as_str()).copied())
                .collect();
            ranks.sort_unstable();
            ranks.dedup();
            if ranks.is_empty() {
                continue;
            }
            let damping = (2.0 + files.len() as f64).ln();
            let mass: f64 = ranks
                .iter()
                .map(|rank| super::score::seed_weight(*rank))
                .sum::<f64>()
                / damping;
            let unique: HashSet<&str> = files.iter().map(String::as_str).collect();
            for file in unique {
                if seed_rank.contains_key(file) || excluded.contains(file) {
                    continue;
                }
                let entry = by_file.entry(file.to_string()).or_default();
                entry.weight += mass;
                entry.commits += 1;
                let counts = per_seed.entry(file.to_string()).or_default();
                for rank in &ranks {
                    *counts.entry(*rank).or_default() += 1;
                }
            }
        }
        for (file, counts) in per_seed {
            if let (Some(entry), Some((rank, _))) = (
                by_file.get_mut(&file),
                counts
                    .into_iter()
                    .max_by(|(ra, ca), (rb, cb)| ca.cmp(cb).then(rb.cmp(ra))),
            ) {
                entry.seed_rank = rank;
            }
        }
        Self { by_file }
    }
}
