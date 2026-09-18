//! One artifact per crate version: the public method names its source
//! defines and the crates it re-exports, as a small versioned text file.
//!
//! ```text
//! codegraph-crate-api 1
//! generator rust-api-scanner/2 tree-sitter-rust/0.24
//! crate clap 4.6.1
//! reexport clap_builder
//! get_matches
//! ```
//!
//! The first two lines say how the file was produced: one written by
//! another format or scanner version is stale and rebuilt, never read.
//! Registry crates (immutable per version) and git crates (per commit) are
//! shared by every project under the global store; vendored sources belong
//! to their project and are kept in its `.codegraph/deps/`. Writes go to a
//! temporary file renamed into place, so concurrent indexers never see a
//! partial artifact; directories are created `0700` and files `0600`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::lockfile::LockedCrate;
use super::sources::Origin;

const FORMAT: &str = "codegraph-crate-api 1";
/// Bump the scanner version whenever `scan` changes what it records.
const GENERATOR: &str = "generator rust-api-scanner/2 tree-sitter-rust/0.24";
const REEXPORT: &str = "reexport ";

/// What one crate version offers a dependent crate.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CrateApi {
    /// Public method names, sorted.
    pub(crate) methods: Vec<String>,
    /// Crates its library root re-exports, as code names them.
    pub(crate) reexports: Vec<String>,
}

/// Where the artifact for `krate` from `origin` lives: under `global` (the
/// shared store's `crates/` dir) or, vendored, under `project_store`.
pub(crate) fn artifact_path(
    krate: &LockedCrate,
    origin: &Origin,
    global: &Path,
    project_store: &Path,
) -> PathBuf {
    let stem = format!("{}-{}", sanitize(&krate.name), sanitize(&krate.version));
    match origin {
        Origin::Registry => global.join(format!("{stem}.api")),
        Origin::Git { commit } => global.join(format!("{stem}+git.{}.api", sanitize(commit))),
        Origin::Vendored => project_store.join(format!("{stem}.api")),
    }
}

/// The artifact at `path`, if it is current and for `krate`.
pub(crate) fn read(path: &Path, krate: &LockedCrate) -> Option<CrateApi> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let header = format!("crate {} {}", krate.name, krate.version);
    if lines.next()? != FORMAT || lines.next()? != GENERATOR || lines.next()? != header {
        return None;
    }
    let mut api = CrateApi::default();
    for line in lines.filter(|line| !line.is_empty()) {
        match line.strip_prefix(REEXPORT) {
            Some(reexport) => api.reexports.push(reexport.to_string()),
            None => api.methods.push(line.to_string()),
        }
    }
    Some(api)
}

/// Write the artifact for `krate` to `path`, atomically.
pub(crate) fn write(path: &Path, krate: &LockedCrate, api: &CrateApi) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("artifact path has no directory"))?;
    create_private_dir(dir)?;
    let mut text = format!(
        "{FORMAT}\n{GENERATOR}\ncrate {} {}\n",
        krate.name, krate.version
    );
    for reexport in &api.reexports {
        text.push_str(REEXPORT);
        text.push_str(reexport);
        text.push('\n');
    }
    for method in &api.methods {
        text.push_str(method);
        text.push('\n');
    }
    let temp = dir.join(format!(
        ".{}.{}.{:x}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact"),
        std::process::id(),
        nonce()
    ));
    let written = create_private_file(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

/// Keep a name or version to one path segment.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::{CrateApi, artifact_path, read, write};
    use crate::resolution::rust_deps::lockfile::LockedCrate;
    use crate::resolution::rust_deps::sources::Origin;

    fn krate() -> LockedCrate {
        LockedCrate {
            name: "serde_json".into(),
            version: "1.0.140".into(),
            source: Some("registry+https://github.com/rust-lang/crates.io-index".into()),
        }
    }

    #[test]
    fn artifacts_round_trip_privately_and_reject_other_versions() {
        let store = tempfile::tempdir().unwrap();
        let global = store.path().join("deps/crates");
        let path = artifact_path(&krate(), &Origin::Registry, &global, store.path());
        assert!(path.ends_with("deps/crates/serde_json-1.0.140.api"));
        let api = CrateApi {
            methods: vec!["as_array".into(), "as_object".into()],
            reexports: vec!["itoa".into()],
        };
        write(&path, &krate(), &api).unwrap();
        assert_eq!(read(&path, &krate()), Some(api));
        let other = LockedCrate {
            version: "1.0.141".into(),
            ..krate()
        };
        assert_eq!(read(&path, &other), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&global), 0o700);
        }
        // A file from another scanner version is stale.
        std::fs::write(
            &path,
            "codegraph-crate-api 1\ngenerator old\ncrate serde_json 1.0.140\nx\n",
        )
        .unwrap();
        assert_eq!(read(&path, &krate()), None);
    }

    #[test]
    fn git_and_vendored_artifacts_are_keyed_apart() {
        let git = Origin::Git {
            commit: "abcdef12".into(),
        };
        let global = std::path::Path::new("/g");
        let local = std::path::Path::new("/p/.codegraph/deps");
        assert_eq!(
            artifact_path(&krate(), &git, global, local),
            global.join("serde_json-1.0.140+git.abcdef12.api")
        );
        assert_eq!(
            artifact_path(&krate(), &Origin::Vendored, global, local),
            local.join("serde_json-1.0.140.api")
        );
    }
}
