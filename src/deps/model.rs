//! The vocabulary shared by every part of `deps`: which ecosystem a package
//! comes from, where its code comes from, and the key a shard is filed under.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A package ecosystem with its own lockfiles, source caches and shard dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    /// Rust crates (Cargo.lock; `$CARGO_HOME/registry/src`).
    Crates,
    /// JavaScript/TypeScript packages (package-lock.json, pnpm-lock.yaml,
    /// bun.lock, yarn.lock; the project's `node_modules`).
    Npm,
    /// Go modules (go.mod; `$GOMODCACHE`).
    Go,
}

impl Ecosystem {
    pub const ALL: [Ecosystem; 3] = [Ecosystem::Crates, Ecosystem::Npm, Ecosystem::Go];

    /// Stable id: the registry column value and the shard subdirectory.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crates => "crates",
            Self::Npm => "npm",
            Self::Go => "go",
        }
    }
}

impl fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Ecosystem {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "crates" | "crate" | "cargo" | "rust" => Ok(Self::Crates),
            "npm" | "node" | "js" | "javascript" | "typescript" => Ok(Self::Npm),
            "go" | "golang" => Ok(Self::Go),
            other => Err(format!(
                "unknown ecosystem `{other}` (expected crates, npm, go)"
            )),
        }
    }
}

/// Where a dependency's code comes from, as its lockfile records it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DepSource {
    /// A package registry (crates.io, npmjs, the Go module proxy). The
    /// source is identified by name and version alone.
    Registry,
    /// A git repository pinned to a commit.
    Git { url: String, rev: String },
    /// A local directory (path dependency, workspace link, `replace => ../x`).
    /// Owned by the project atlas; never built into a shared shard.
    Path { path: Option<String> },
}

impl DepSource {
    /// Registry column value.
    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Registry => SourceKind::Registry,
            Self::Git { .. } => SourceKind::Git,
            Self::Path { .. } => SourceKind::Path,
        }
    }
}

/// [`DepSource`] without its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Registry,
    Git,
    Path,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Git => "git",
            Self::Path => "path",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "registry" => Some(Self::Registry),
            "git" => Some(Self::Git),
            "path" => Some(Self::Path),
            _ => None,
        }
    }
}

/// The identity a shard is built and looked up under.
///
/// `version` is the lockfile version, except that a git source appends
/// semver build metadata naming the commit (`0.1.0+git.3c76713b531a`) and a
/// path source appends `+path`: a fork pinned at the same version as the
/// registry release is different code, and must not share its shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DepKey {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
}

/// Commit characters kept in a git dependency's key version.
const GIT_REV_CHARS: usize = 12;

impl DepKey {
    pub fn new(ecosystem: Ecosystem, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            ecosystem,
            name: name.into(),
            version: version.into(),
        }
    }

    /// The key for `version` of `name` taken from `source`.
    pub fn for_source(ecosystem: Ecosystem, name: &str, version: &str, source: &DepSource) -> Self {
        let version = match source {
            DepSource::Registry => version.to_string(),
            DepSource::Git { rev, .. } => {
                let short: String = rev.chars().take(GIT_REV_CHARS).collect();
                if short.is_empty() {
                    format!("{version}+git")
                } else {
                    format!("{version}+git.{short}")
                }
            }
            DepSource::Path { .. } => format!("{version}+path"),
        };
        Self::new(ecosystem, name, version)
    }

    /// The shard directory name under `deps/<ecosystem>/`: `<name>-<version>`
    /// with `/` (npm scopes, Go module paths) folded to `+`, which neither
    /// ecosystem allows in a name, and Go's upper case escaped the way the
    /// module cache does (`!x`), so two keys never share a directory even on
    /// a case-insensitive filesystem.
    pub fn dir_name(&self) -> String {
        let name = match self.ecosystem {
            Ecosystem::Go => super::locate::go::escape_module_path(&self.name),
            Ecosystem::Crates | Ecosystem::Npm => self.name.clone(),
        };
        let version = match self.ecosystem {
            Ecosystem::Go => super::locate::go::escape_module_path(&self.version),
            Ecosystem::Crates | Ecosystem::Npm => self.version.clone(),
        };
        sanitize_segment(&format!("{name}-{version}"))
    }
}

impl fmt::Display for DepKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.ecosystem, self.name, self.version)
    }
}

/// One filesystem-safe path segment: `/` and `\` become `+`, anything outside
/// a conservative portable set becomes `_`, and a leading `.` is escaped so a
/// shard never looks like one of the builder's hidden temp directories.
fn sanitize_segment(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' => '+',
            c if c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '+' | '@' | '!' | '~') =>
            {
                c
            }
            _ => '_',
        })
        .collect();
    if out.starts_with('.') {
        out.insert(0, '_');
    }
    out
}

/// One dependency a project's lockfile resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDep {
    pub key: DepKey,
    /// Version exactly as the lockfile spells it (no source suffix).
    pub lock_version: String,
    pub source: DepSource,
    /// `Some(true)` when the project names it directly, `Some(false)` when
    /// only another dependency does, `None` when the lockfile doesn't say.
    pub direct: Option<bool>,
    /// The lockfile that pinned it, relative to the project root.
    pub lockfile: String,
    /// Where the package manager installed it, relative to the project root,
    /// when the lockfile says (npm `node_modules/a/node_modules/b`).
    pub install_path: Option<String>,
}

/// Lifecycle of one dependency version's shard, as the registry records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShardState {
    /// Recorded, source located, no shard yet.
    Missing,
    /// A builder holds the shard lock.
    Building,
    /// Built from every selected library file.
    Ready,
    /// Built, but a file/byte/time budget cut it short (see `meta.json`).
    Partial,
    /// No local source (not downloaded, or a local path dependency).
    Unavailable,
    /// The last build failed (see the registry `error` column).
    Failed,
}

impl ShardState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "missing" => Some(Self::Missing),
            "building" => Some(Self::Building),
            "ready" => Some(Self::Ready),
            "partial" => Some(Self::Partial),
            "unavailable" => Some(Self::Unavailable),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// A shard directory exists for this state.
    pub fn has_shard(self) -> bool {
        matches!(self, Self::Ready | Self::Partial)
    }
}

impl fmt::Display for ShardState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_and_path_sources_get_their_own_key_versions() {
        let git = DepSource::Git {
            url: "https://github.com/a/b".into(),
            rev: "3c76713b531a5bf75d41e89ce132a33f42f31f5f".into(),
        };
        let key = DepKey::for_source(Ecosystem::Crates, "linkscope", "0.1.0", &git);
        assert_eq!(key.version, "0.1.0+git.3c76713b531a");
        let registry = DepKey::for_source(
            Ecosystem::Crates,
            "linkscope",
            "0.1.0",
            &DepSource::Registry,
        );
        assert_eq!(registry.version, "0.1.0");
        let path = DepKey::for_source(
            Ecosystem::Crates,
            "local",
            "0.1.0",
            &DepSource::Path { path: None },
        );
        assert_eq!(path.version, "0.1.0+path");
    }

    #[test]
    fn dir_names_are_single_safe_segments() {
        let scoped = DepKey::new(Ecosystem::Npm, "@types/node", "25.0.3");
        assert_eq!(scoped.dir_name(), "@types+node-25.0.3");
        let go = DepKey::new(Ecosystem::Go, "github.com/BurntSushi/toml", "v1.2.3");
        assert_eq!(go.dir_name(), "github.com+!burnt!sushi+toml-v1.2.3");
        let odd = DepKey::new(Ecosystem::Npm, ".hidden", "1.0.0");
        assert!(!odd.dir_name().starts_with('.'));
        assert_eq!(
            DepKey::new(Ecosystem::Crates, "serde", "1.0.219").dir_name(),
            "serde-1.0.219"
        );
    }

    #[test]
    fn states_round_trip() {
        for state in [
            ShardState::Missing,
            ShardState::Building,
            ShardState::Ready,
            ShardState::Partial,
            ShardState::Unavailable,
            ShardState::Failed,
        ] {
            assert_eq!(ShardState::parse(state.as_str()), Some(state));
        }
        for eco in Ecosystem::ALL {
            assert_eq!(eco.as_str().parse::<Ecosystem>(), Ok(eco));
        }
    }
}
