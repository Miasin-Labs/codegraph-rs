//! Find a dependency's source on this machine — never over the network.
//!
//! Registry crates and Go modules come from the machine-wide caches cargo
//! and go already filled (`$CARGO_HOME`, `$GOMODCACHE`), so one located
//! source serves every project pinning that version. JS packages are found
//! inside the project's own `node_modules`. A source that isn't there is
//! *unavailable* — recorded, never an error. Path dependencies are never
//! located: they are the atlas's, not shared shards.

mod cargo;
pub(crate) mod go;
mod npm;

use std::path::{Path, PathBuf};

use super::model::{DepSource, Ecosystem, ResolvedDep};

/// The machine-wide source caches to look in.
#[derive(Debug, Clone, Default)]
pub struct SourceRoots {
    /// `$CARGO_HOME` (default `~/.cargo`).
    pub cargo_home: Option<PathBuf>,
    /// `$GOMODCACHE` (default `$GOPATH/pkg/mod`, `~/go/pkg/mod`).
    pub go_mod_cache: Option<PathBuf>,
    /// `$CARGO_HOME/registry/src/*`, listed once.
    cargo_registry_dirs: Vec<PathBuf>,
}

impl SourceRoots {
    /// Explicit roots (tests, or callers with their own configuration).
    pub fn new(cargo_home: Option<PathBuf>, go_mod_cache: Option<PathBuf>) -> Self {
        let cargo_registry_dirs = cargo_home
            .as_deref()
            .map(cargo::registry_dirs)
            .unwrap_or_default();
        Self {
            cargo_home,
            go_mod_cache,
            cargo_registry_dirs,
        }
    }

    /// The caches this environment's cargo and go use.
    pub fn from_env() -> Self {
        let home = dirs::home_dir();
        let env_path = |var: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let cargo_home = env_path("CARGO_HOME").or_else(|| home.as_ref().map(|h| h.join(".cargo")));
        let go_mod_cache = env_path("GOMODCACHE").or_else(|| {
            let gopath = std::env::var_os("GOPATH")
                .filter(|v| !v.is_empty())
                .and_then(|v| std::env::split_paths(&v).next())
                .or_else(|| home.as_ref().map(|h| h.join("go")))?;
            Some(gopath.join("pkg").join("mod"))
        });
        Self::new(cargo_home, go_mod_cache)
    }

    /// Where `dep` (from the project at `project_root`) has its source here.
    pub fn locate(&self, dep: &ResolvedDep, project_root: &Path) -> Option<PathBuf> {
        if matches!(dep.source, DepSource::Path { .. }) {
            return None;
        }
        let found = match dep.key.ecosystem {
            Ecosystem::Crates => cargo::locate(
                dep,
                project_root,
                self.cargo_home.as_deref(),
                &self.cargo_registry_dirs,
            ),
            Ecosystem::Npm => npm::locate(dep, project_root),
            Ecosystem::Go => go::locate(dep, project_root, self.go_mod_cache.as_deref()),
        }?;
        // pnpm links and hoisted symlinks resolve to the real directory, so
        // every project sharing the store names the same source.
        Some(std::fs::canonicalize(&found).unwrap_or(found))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::deps::model::DepKey;

    fn dep(
        eco: Ecosystem,
        name: &str,
        version: &str,
        source: DepSource,
        lockfile: &str,
    ) -> ResolvedDep {
        ResolvedDep {
            key: DepKey::for_source(eco, name, version, &source),
            lock_version: version.to_string(),
            source,
            direct: Some(true),
            lockfile: lockfile.to_string(),
            install_path: None,
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn locates_registry_git_and_vendored_crates() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo_home = tmp.path().join("cargo");
        let project = tmp.path().join("proj");
        write(
            &cargo_home
                .join("registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.219/Cargo.toml"),
            "[package]\nname = \"serde\"\nversion = \"1.0.219\"\n",
        );
        write(
            &cargo_home
                .join("git/checkouts/linkscope-a03b6ff875fefbb5/3c76713/crates/core/Cargo.toml"),
            "[package]\nname = \"linkscope\"\nversion = \"0.1.0\"\n",
        );
        write(
            &project.join("vendor/itoa/Cargo.toml"),
            "[package]\nname = \"itoa\"\nversion = \"1.0.15\"\n",
        );
        let roots = SourceRoots::new(Some(cargo_home.clone()), None);

        let serde = dep(
            Ecosystem::Crates,
            "serde",
            "1.0.219",
            DepSource::Registry,
            "Cargo.lock",
        );
        assert!(
            roots
                .locate(&serde, &project)
                .unwrap()
                .ends_with("serde-1.0.219")
        );

        let git = dep(
            Ecosystem::Crates,
            "linkscope",
            "0.1.0",
            DepSource::Git {
                url: "https://github.com/Miasin-Labs/linkscope".into(),
                rev: "3c76713b531a5bf75d41e89ce132a33f42f31f5f".into(),
            },
            "Cargo.lock",
        );
        assert!(
            roots
                .locate(&git, &project)
                .unwrap()
                .ends_with("3c76713/crates/core")
        );

        let vendored = dep(
            Ecosystem::Crates,
            "itoa",
            "1.0.15",
            DepSource::Registry,
            "Cargo.lock",
        );
        assert!(
            roots
                .locate(&vendored, &project)
                .unwrap()
                .ends_with("vendor/itoa")
        );

        let wrong_version = dep(
            Ecosystem::Crates,
            "itoa",
            "1.0.14",
            DepSource::Registry,
            "Cargo.lock",
        );
        assert_eq!(roots.locate(&wrong_version, &project), None);
        let missing = dep(
            Ecosystem::Crates,
            "nope",
            "1.0.0",
            DepSource::Registry,
            "Cargo.lock",
        );
        assert_eq!(roots.locate(&missing, &project), None);
        let path = dep(
            Ecosystem::Crates,
            "serde",
            "1.0.219",
            DepSource::Path { path: None },
            "Cargo.lock",
        );
        assert_eq!(roots.locate(&path, &project), None);
    }

    #[test]
    fn locates_go_modules_with_case_encoding_and_vendor() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("mod");
        let project = tmp.path().join("proj");
        fs::create_dir_all(cache.join("github.com/!burnt!sushi/toml@v1.2.3")).unwrap();
        write(
            &project.join("svc/vendor/modules.txt"),
            "# example.com/Vend v0.1.0\n## explicit\nexample.com/Vend\n",
        );
        fs::create_dir_all(project.join("svc/vendor/example.com/Vend")).unwrap();
        let roots = SourceRoots::new(None, Some(cache));

        let toml = dep(
            Ecosystem::Go,
            "github.com/BurntSushi/toml",
            "v1.2.3",
            DepSource::Registry,
            "go.mod",
        );
        assert!(
            roots
                .locate(&toml, &project)
                .unwrap()
                .ends_with("github.com/!burnt!sushi/toml@v1.2.3")
        );
        let lower = dep(
            Ecosystem::Go,
            "github.com/burntsushi/toml",
            "v1.2.3",
            DepSource::Registry,
            "go.mod",
        );
        assert_eq!(roots.locate(&lower, &project), None, "case matters");

        let vend = dep(
            Ecosystem::Go,
            "example.com/Vend",
            "v0.1.0",
            DepSource::Registry,
            "svc/go.mod",
        );
        assert!(
            roots
                .locate(&vend, &project)
                .unwrap()
                .ends_with("vendor/example.com/Vend")
        );
        let vend_other = dep(
            Ecosystem::Go,
            "example.com/Vend",
            "v0.2.0",
            DepSource::Registry,
            "svc/go.mod",
        );
        assert_eq!(roots.locate(&vend_other, &project), None);
    }

    #[test]
    fn locates_npm_installs_pnpm_store_and_hoisted() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        write(
            &project.join("web/node_modules/chalk/node_modules/left-pad/package.json"),
            r#"{"name":"left-pad","version":"1.0.0"}"#,
        );
        write(
            &project.join(
                "node_modules/.pnpm/@types+node@20.1.0/node_modules/@types/node/package.json",
            ),
            r#"{"name":"@types/node","version":"20.1.0"}"#,
        );
        write(
            &project.join("node_modules/.pnpm/react-dom@18.2.0_react@18.2.0/node_modules/react-dom/package.json"),
            r#"{"name":"react-dom","version":"18.2.0"}"#,
        );
        write(
            &project.join("node_modules/zod/package.json"),
            r#"{"name":"zod","version":"4.1.8"}"#,
        );
        let roots = SourceRoots::default();

        let mut nested = dep(
            Ecosystem::Npm,
            "left-pad",
            "1.0.0",
            DepSource::Registry,
            "web/package-lock.json",
        );
        nested.install_path = Some("web/node_modules/chalk/node_modules/left-pad".into());
        assert!(roots.locate(&nested, &project).is_some());
        nested.lock_version = "1.0.1".into();
        assert_eq!(roots.locate(&nested, &project), None, "stale install");

        let types = dep(
            Ecosystem::Npm,
            "@types/node",
            "20.1.0",
            DepSource::Registry,
            "pnpm-lock.yaml",
        );
        assert!(
            roots
                .locate(&types, &project)
                .unwrap()
                .ends_with("node_modules/@types/node")
        );
        let peer = dep(
            Ecosystem::Npm,
            "react-dom",
            "18.2.0",
            DepSource::Registry,
            "pnpm-lock.yaml",
        );
        assert!(roots.locate(&peer, &project).is_some());
        let hoisted = dep(
            Ecosystem::Npm,
            "zod",
            "4.1.8",
            DepSource::Registry,
            "bun.lock",
        );
        assert!(roots.locate(&hoisted, &project).is_some());
    }
}
