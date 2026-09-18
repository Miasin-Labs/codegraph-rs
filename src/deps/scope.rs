//! Which files of a dependency a shard indexes: the library sources callers
//! reach — never tests, benches, examples, docs or build scripts — capped by
//! per-shard budgets so a generated-bindings crate (`windows-sys`,
//! `linux-raw-sys`, `web-sys`) yields a bounded, `partial` shard instead of
//! a runaway build. File and source-byte budgets apply here, before
//! extraction; the time and database-size budgets are enforced while
//! extracting ([`crate::deps::shard`]) — bindings are dense (18 MB of
//! windows-sys source is 226k symbols), so source bytes alone don't bound
//! the shard.
//!
//! Shallow files come first (`src/lib.rs` before `src/a/b/c.rs`), so a
//! truncated shard still holds the crate's top-level API.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::DirEntry;

use super::model::Ecosystem;

/// Budgets one shard build must stay within.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardLimits {
    /// Most files indexed.
    pub max_files: usize,
    /// Most source bytes indexed.
    pub max_bytes: u64,
    /// Larger single files are skipped as generated.
    pub max_file_bytes: u64,
    /// Wall-clock budget for extraction.
    pub time_ms: u64,
    /// Extraction stops once the shard database (with its WAL) grows past
    /// this.
    #[serde(default = "default_max_db_bytes")]
    pub max_db_bytes: u64,
}

fn default_max_db_bytes() -> u64 {
    ShardLimits::default().max_db_bytes
}

impl Default for ShardLimits {
    fn default() -> Self {
        Self {
            max_files: 3_000,
            max_bytes: 32 * 1024 * 1024,
            max_file_bytes: 1024 * 1024,
            time_ms: 120_000,
            max_db_bytes: 64 * 1024 * 1024,
        }
    }
}

impl ShardLimits {
    /// Every budget of `self` is at least `other`'s: a shard cut short under
    /// `other` could not grow under `self` unless this is false.
    pub fn covers(&self, other: &ShardLimits) -> bool {
        self.max_files >= other.max_files
            && self.max_bytes >= other.max_bytes
            && self.max_file_bytes >= other.max_file_bytes
            && self.time_ms >= other.time_ms
            && self.max_db_bytes >= other.max_db_bytes
    }
}

/// Why a shard is partial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartialReason {
    /// More library files than `max_files`.
    Files,
    /// More library bytes than `max_bytes`.
    Bytes,
    /// Files over `max_file_bytes` were skipped.
    FileSize,
    /// Extraction ran out of `time_ms`.
    Time,
    /// The database reached `max_db_bytes`.
    DbSize,
}

/// The files chosen for one shard.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Paths relative to the source root, `/`-separated, in index order.
    pub files: Vec<String>,
    pub bytes: u64,
    /// Library files found before any budget applied.
    pub candidates: usize,
    pub candidate_bytes: u64,
    /// Files skipped for exceeding `max_file_bytes`.
    pub oversized: usize,
    pub truncated_by: Vec<PartialReason>,
    /// Hash of every candidate's path and size: equal fingerprints mean the
    /// same source (budgets aside).
    pub fingerprint: String,
    /// Newest candidate modification time (epoch ms).
    pub newest_mtime_ms: i64,
}

struct Candidate {
    rel: String,
    size: u64,
    depth: usize,
}

/// Select the library files of the `ecosystem` package at `root`.
pub fn select(ecosystem: Ecosystem, root: &Path, limits: &ShardLimits) -> Selection {
    let rules = Rules::for_package(ecosystem, root);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut newest_mtime_ms = 0i64;
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_type().is_dir() || rules.keep_dir(e));
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if !rules.keep_file(&rel, entry.depth()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if let Some(ms) = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        {
            newest_mtime_ms = newest_mtime_ms.max(ms.as_millis() as i64);
        }
        candidates.push(Candidate {
            rel,
            size: meta.len(),
            depth: entry.depth(),
        });
    }
    rules.narrow(&mut candidates);

    let mut hasher = Sha256::new();
    for c in &candidates {
        hasher.update(c.rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(c.size.to_le_bytes());
    }
    let mut selection = Selection {
        candidates: candidates.len(),
        candidate_bytes: candidates.iter().map(|c| c.size).sum(),
        fingerprint: format!("{:x}", hasher.finalize()),
        newest_mtime_ms,
        ..Selection::default()
    };

    candidates.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.rel.cmp(&b.rel)));
    for c in candidates {
        if c.size > limits.max_file_bytes {
            selection.oversized += 1;
            note(&mut selection.truncated_by, PartialReason::FileSize);
            continue;
        }
        if selection.files.len() >= limits.max_files {
            note(&mut selection.truncated_by, PartialReason::Files);
            break;
        }
        if selection.bytes + c.size > limits.max_bytes {
            note(&mut selection.truncated_by, PartialReason::Bytes);
            continue;
        }
        selection.bytes += c.size;
        selection.files.push(c.rel);
    }
    selection
}

fn note(reasons: &mut Vec<PartialReason>, reason: PartialReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

/// Per-ecosystem pruning and file rules.
enum Rules {
    Crates,
    Npm {
        entry_dir: Option<String>,
    },
    Go {
        host_os: &'static str,
        host_arch: &'static str,
    },
}

/// Top-level crate directories that are never library code.
const CRATE_NON_LIBRARY_DIRS: &[&str] = &[
    "tests", "benches", "examples", "docs", "doc", "fuzz", "target", "ci", "scripts", "xtask",
];

/// JS directories that are never library code, at any depth.
const NPM_NON_LIBRARY_DIRS: &[&str] = &[
    "node_modules",
    "test",
    "tests",
    "__tests__",
    "__mocks__",
    "spec",
    "specs",
    "example",
    "examples",
    "docs",
    "doc",
    "bench",
    "benchmark",
    "benchmarks",
    "coverage",
    "fixtures",
    "__fixtures__",
    "umd",
];

const GO_NON_LIBRARY_DIRS: &[&str] = &["testdata", "vendor", "examples", "example", "cmd"];

impl Rules {
    fn for_package(ecosystem: Ecosystem, root: &Path) -> Self {
        match ecosystem {
            Ecosystem::Crates => Self::Crates,
            Ecosystem::Npm => Self::Npm {
                entry_dir: npm_entry_dir(root),
            },
            Ecosystem::Go => Self::Go {
                host_os: go_host_os(),
                host_arch: go_host_arch(),
            },
        }
    }

    fn keep_dir(&self, entry: &DirEntry) -> bool {
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') {
            return false;
        }
        match self {
            Self::Crates => {
                !(name == "target"
                    || (entry.depth() == 1 && CRATE_NON_LIBRARY_DIRS.contains(&name.as_ref())))
            }
            Self::Npm { .. } => !NPM_NON_LIBRARY_DIRS.contains(&name.as_ref()),
            Self::Go { .. } => {
                !(name.starts_with('_')
                    || GO_NON_LIBRARY_DIRS.contains(&name.as_ref())
                    // A nested module is another dependency, not this one.
                    || entry.path().join("go.mod").is_file())
            }
        }
    }

    fn keep_file(&self, rel: &str, depth: usize) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        match self {
            Self::Crates => file.ends_with(".rs") && !(depth == 1 && file == "build.rs"),
            Self::Npm { .. } => {
                let js_or_ts = [".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"]
                    .iter()
                    .any(|ext| file.ends_with(ext));
                js_or_ts
                    && !file.ends_with(".min.js")
                    && ![".test.", ".spec.", ".stories."]
                        .iter()
                        .any(|m| file.contains(m))
            }
            Self::Go { host_os, host_arch } => {
                file.ends_with(".go")
                    && !file.ends_with("_test.go")
                    && go_file_matches_host(file, host_os, host_arch)
            }
        }
    }

    /// Whole-package narrowing once every candidate is known: a package that
    /// ships TypeScript declarations is indexed through them (its API, typed)
    /// rather than its compiled JS; a JS-only package through the directory
    /// its entry point lives in (not every esm/cjs/umd copy).
    fn narrow(&self, candidates: &mut Vec<Candidate>) {
        let Self::Npm { entry_dir } = self else {
            return;
        };
        let is_decl = |rel: &str| {
            [".d.ts", ".d.mts", ".d.cts"]
                .iter()
                .any(|e| rel.ends_with(e))
        };
        let is_ts = |rel: &str| {
            [".ts", ".mts", ".cts", ".tsx"]
                .iter()
                .any(|e| rel.ends_with(e))
        };
        if candidates.iter().any(|c| is_decl(&c.rel)) {
            candidates.retain(|c| is_ts(&c.rel));
            return;
        }
        if let Some(dir) = entry_dir {
            let prefix = format!("{dir}/");
            if candidates.iter().any(|c| c.rel.starts_with(&prefix)) {
                candidates.retain(|c| c.depth == 1 || c.rel.starts_with(&prefix));
            }
        }
    }
}

/// The top-level directory of a package.json `module`/`main` entry
/// (`./dist/index.js` → `dist`); `None` for a root-level entry.
fn npm_entry_dir(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = ["module", "main"]
        .iter()
        .find_map(|k| manifest.get(*k).and_then(serde_json::Value::as_str))?;
    let entry = entry.trim_start_matches("./");
    let (dir, _) = entry.split_once('/')?;
    (!dir.is_empty() && dir != "." && dir != "..").then(|| dir.to_string())
}

const GO_OSES: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];
const GO_ARCHES: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "armbe",
    "arm64",
    "arm64be",
    "loong64",
    "mips",
    "mipsle",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

fn go_host_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        "android" => "android",
        "ios" => "ios",
        _ => "linux",
    }
}

fn go_host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm",
        "riscv64" => "riscv64",
        "powerpc64" => "ppc64le",
        "s390x" => "s390x",
        "loongarch64" => "loong64",
        _ => "amd64",
    }
}

/// Go's implicit build constraints from `name_GOOS_GOARCH.go`,
/// `name_GOOS.go`, `name_GOARCH.go`: files for another platform are not
/// what this machine's callers compile against (and are most of
/// `golang.org/x/sys`).
fn go_file_matches_host(file: &str, host_os: &str, host_arch: &str) -> bool {
    let stem = file.trim_end_matches(".go");
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() < 2 {
        return true;
    }
    let last = parts[parts.len() - 1];
    let prev = (parts.len() >= 3).then(|| parts[parts.len() - 2]);
    let os_ok = |os: &str| {
        os == host_os
            || (os == "darwin" && host_os == "ios")
            || (os == "linux" && host_os == "android")
    };
    if GO_ARCHES.contains(&last) {
        if let Some(os) = prev.filter(|p| GO_OSES.contains(p)) {
            return os_ok(os) && last == host_arch;
        }
        return last == host_arch;
    }
    if GO_OSES.contains(&last) {
        return os_ok(last);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn tree(files: &[(&str, usize)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, size) in files {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, "x".repeat(*size)).unwrap();
        }
        dir
    }

    #[test]
    fn crates_keep_library_sources_only() {
        let dir = tree(&[
            ("Cargo.toml", 10),
            ("build.rs", 10),
            ("src/lib.rs", 10),
            ("src/de/mod.rs", 10),
            ("src/de/impls/deep.rs", 10),
            ("tests/it.rs", 10),
            ("benches/b.rs", 10),
            ("examples/e.rs", 10),
            ("src/tests/helper.rs", 10),
            ("target/debug/gen.rs", 10),
            (".github/x.rs", 10),
        ]);
        let s = select(Ecosystem::Crates, dir.path(), &ShardLimits::default());
        assert_eq!(
            s.files,
            vec![
                "src/lib.rs",
                "src/de/mod.rs",
                "src/tests/helper.rs",
                "src/de/impls/deep.rs"
            ]
        );
        assert!(s.truncated_by.is_empty());
    }

    #[test]
    fn budgets_truncate_shallow_first() {
        let dir = tree(&[
            ("src/lib.rs", 100),
            ("src/a.rs", 100),
            ("src/b/c.rs", 100),
            ("src/huge.rs", 5_000),
        ]);
        let limits = ShardLimits {
            max_files: 2,
            max_bytes: 10_000,
            max_file_bytes: 1_000,
            ..ShardLimits::default()
        };
        let s = select(Ecosystem::Crates, dir.path(), &limits);
        assert_eq!(s.files, vec!["src/a.rs", "src/lib.rs"]);
        assert_eq!(s.oversized, 1);
        assert!(s.truncated_by.contains(&PartialReason::Files));
        assert!(s.truncated_by.contains(&PartialReason::FileSize));
        assert_eq!(s.candidates, 4);

        let bytes = ShardLimits {
            max_bytes: 150,
            ..ShardLimits::default()
        };
        let s = select(Ecosystem::Crates, dir.path(), &bytes);
        assert_eq!(s.files, vec!["src/a.rs"]);
        assert!(s.truncated_by.contains(&PartialReason::Bytes));
    }

    #[test]
    fn npm_prefers_declarations_then_the_entry_dir() {
        let typed = tree(&[
            ("package.json", 2),
            ("index.js", 10),
            ("index.d.ts", 10),
            ("lib/util.d.ts", 10),
            ("lib/util.js", 10),
            ("test/a.test.ts", 10),
            ("node_modules/dep/index.d.ts", 10),
        ]);
        let s = select(Ecosystem::Npm, typed.path(), &ShardLimits::default());
        assert_eq!(s.files, vec!["index.d.ts", "lib/util.d.ts"]);

        let js = tree(&[
            ("dist/index.js", 10),
            ("dist/esm/index.js", 10),
            ("umd/bundle.js", 10),
            ("src/raw.js", 10),
            ("dist/x.min.js", 10),
        ]);
        fs::write(
            js.path().join("package.json"),
            r#"{"main":"./dist/index.js"}"#,
        )
        .unwrap();
        let s = select(Ecosystem::Npm, js.path(), &ShardLimits::default());
        assert_eq!(s.files, vec!["dist/index.js", "dist/esm/index.js"]);
    }

    #[test]
    fn go_skips_tests_nested_modules_and_foreign_platforms() {
        let dir = tree(&[
            ("go.mod", 10),
            ("a.go", 10),
            ("a_test.go", 10),
            ("sys_windows.go", 10),
            ("sys_linux.go", 10),
            ("sys_darwin_arm64.go", 10),
            ("sys_linux_amd64.go", 10),
            ("sys_linux_arm64.go", 10),
            ("errors_unix.go", 10),
            ("internal/x/x.go", 10),
            ("testdata/t.go", 10),
            ("cmd/tool/main.go", 10),
            ("_tools/t.go", 10),
            ("nested/go.mod", 10),
            ("nested/n.go", 10),
        ]);
        let s = select(Ecosystem::Go, dir.path(), &ShardLimits::default());
        let host_os = go_host_os();
        let host_arch = go_host_arch();
        for f in &s.files {
            assert!(
                !f.ends_with("_test.go") && !f.starts_with("testdata") && !f.starts_with("cmd")
            );
            assert!(!f.starts_with("nested") && !f.starts_with("_tools"));
        }
        assert!(s.files.contains(&"a.go".to_string()));
        assert!(s.files.contains(&"errors_unix.go".to_string()));
        assert!(s.files.contains(&"internal/x/x.go".to_string()));
        assert_eq!(
            s.files.contains(&"sys_windows.go".to_string()),
            host_os == "windows"
        );
        assert_eq!(
            s.files.contains(&"sys_linux_amd64.go".to_string()),
            host_os == "linux" && host_arch == "amd64"
        );
    }

    #[test]
    fn fingerprint_tracks_candidates_not_budgets() {
        let dir = tree(&[("src/lib.rs", 10), ("src/a.rs", 10)]);
        let a = select(Ecosystem::Crates, dir.path(), &ShardLimits::default());
        let b = select(
            Ecosystem::Crates,
            dir.path(),
            &ShardLimits {
                max_files: 1,
                ..ShardLimits::default()
            },
        );
        assert_eq!(a.fingerprint, b.fingerprint);
        fs::write(dir.path().join("src/a.rs"), "a longer body than before").unwrap();
        let c = select(Ecosystem::Crates, dir.path(), &ShardLimits::default());
        assert_ne!(a.fingerprint, c.fingerprint);
    }
}
