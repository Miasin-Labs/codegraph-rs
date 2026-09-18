//! Building missing artifacts: each crate is scanned by one of a few
//! worker threads and written to the store, until the deadline.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use super::lockfile::LockedCrate;
use super::scan::{Limits, public_method_names, reexported_crates};
use super::store::{self, CrateApi};

/// How many crates are scanned at once.
const MAX_WORKERS: usize = 8;

/// One artifact to build: `krate`'s source is `dir`, its artifact `path`.
pub(super) struct Job {
    pub(super) krate: LockedCrate,
    pub(super) dir: PathBuf,
    pub(super) path: PathBuf,
}

/// Scan and store every job's crate; each comes back with its API, or
/// `None` when the deadline passed first or the artifact could not be
/// written.
pub(super) fn build_all(jobs: Vec<Job>, deadline: Instant) -> Vec<(Job, Option<CrateApi>)> {
    let next = AtomicUsize::new(0);
    let apis: Mutex<Vec<Option<CrateApi>>> = Mutex::new(vec![None; jobs.len()]);
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, MAX_WORKERS)
        .min(jobs.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    let api = build(job, deadline);
                    apis.lock().unwrap_or_else(|poisoned| poisoned.into_inner())[index] = api;
                }
            });
        }
    });
    let apis = apis
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    jobs.into_iter().zip(apis).collect()
}

fn build(job: &Job, deadline: Instant) -> Option<CrateApi> {
    let api = CrateApi {
        methods: public_method_names(&job.dir, Limits::default(), deadline)?,
        reexports: reexported_crates(&job.dir),
    };
    store::write(&job.path, &job.krate, &api).ok()?;
    Some(api)
}
