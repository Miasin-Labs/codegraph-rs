//! The typed kinds the atlas stores as text.

use serde::Serialize;

/// How one project points at another directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A Cargo dependency (or `[patch]`/`[replace]` entry) with `path = …`.
    CargoPathDep,
    /// A `[workspace] members` entry of a Cargo.toml.
    CargoWorkspaceMember,
    /// A package.json `workspaces` entry or a pnpm-workspace.yaml package.
    NpmWorkspace,
    /// A package.json dependency on `file:`, `link:` or `workspace:`.
    NpmFileDep,
    /// A go.mod `replace … => ./local/dir`.
    GoReplace,
    /// Separate clones (not worktrees) of one normalized remote.
    SameRemote,
    /// An index whose root lies inside another registered project's root.
    NestedWorkspace,
}

impl LinkKind {
    /// Every kind, in display order.
    pub const ALL: [Self; 7] = [
        Self::CargoPathDep,
        Self::CargoWorkspaceMember,
        Self::NpmWorkspace,
        Self::NpmFileDep,
        Self::GoReplace,
        Self::SameRemote,
        Self::NestedWorkspace,
    ];

    /// The stored (and JSON) spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CargoPathDep => "cargo_path_dep",
            Self::CargoWorkspaceMember => "cargo_workspace_member",
            Self::NpmWorkspace => "npm_workspace",
            Self::NpmFileDep => "npm_file_dep",
            Self::GoReplace => "go_replace",
            Self::SameRemote => "same_remote",
            Self::NestedWorkspace => "nested_workspace",
        }
    }

    /// Parse the stored spelling (unknown kinds from a newer build: `None`).
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Short human label (diagram edges, CLI listings).
    pub fn label(self) -> &'static str {
        match self {
            Self::CargoPathDep => "cargo path dep",
            Self::CargoWorkspaceMember => "cargo member",
            Self::NpmWorkspace => "npm workspace",
            Self::NpmFileDep => "npm file dep",
            Self::GoReplace => "go replace",
            Self::SameRemote => "same remote",
            Self::NestedWorkspace => "nested",
        }
    }

    /// Derived from the project rows rather than read from a manifest:
    /// recomputed for every project whenever any project changes.
    pub fn is_derived(self) -> bool {
        matches!(self, Self::SameRemote | Self::NestedWorkspace)
    }
}

impl std::fmt::Display for LinkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Health of a registered project's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ProjectStatus {
    /// Readable, at this build's schema.
    #[serde(rename = "ok")]
    Ok,
    /// The root or its index is gone (see `codegraph projects prune`).
    #[serde(rename = "missing")]
    Missing,
    /// Readable but on an older index schema (the next write migrates it).
    #[serde(rename = "stale-schema")]
    StaleSchema,
    /// Present but could not be read (locked, corrupt, not an index).
    #[serde(rename = "unreadable")]
    Unreadable,
}

impl ProjectStatus {
    /// The stored (and JSON) spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::StaleSchema => "stale-schema",
            Self::Unreadable => "unreadable",
        }
    }

    /// Parse the stored spelling; unknown values read as unreadable.
    pub fn parse(text: &str) -> Self {
        match text {
            "ok" => Self::Ok,
            "missing" => Self::Missing,
            "stale-schema" => Self::StaleSchema,
            _ => Self::Unreadable,
        }
    }
}

impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_their_stored_spelling() {
        for kind in LinkKind::ALL {
            assert_eq!(LinkKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
        assert_eq!(LinkKind::parse("from_the_future"), None);
        for status in [
            ProjectStatus::Ok,
            ProjectStatus::Missing,
            ProjectStatus::StaleSchema,
            ProjectStatus::Unreadable,
        ] {
            assert_eq!(ProjectStatus::parse(status.as_str()), status);
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::json!(status.as_str())
            );
        }
    }
}
