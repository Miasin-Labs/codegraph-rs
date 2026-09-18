use std::fs;

use super::*;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn real(path: &Path) -> PathBuf {
    real_path_lenient(path)
}

/// `(kind, target relative to base, manifest file name relative to base, line, detail)`.
fn summary(
    scan: &ManifestScan,
    base: &Path,
) -> Vec<(LinkKind, String, String, Option<u32>, String)> {
    let base = real(base);
    scan.links
        .iter()
        .map(|l| {
            let rel = |p: &Path| {
                real(p)
                    .strip_prefix(&base)
                    .map_or_else(|_| p.display().to_string(), |r| r.display().to_string())
            };
            (
                l.kind,
                rel(&l.target),
                rel(&l.manifest),
                l.line,
                l.detail.clone().unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn cargo_path_deps_and_workspace_members_with_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let root = base.join("app");
    write(
        &root.join("Cargo.toml"),
        r#"[workspace]
members = [
    "crates/*",
    "tools/cli",
]
exclude = ["crates/experimental"]

[workspace.dependencies]
shared = { path = "../shared" }

[package]
name = "app"

[dependencies]
serde = "1"
local-lib = { path = "../lib", version = "0.1" }

[dev-dependencies.testkit]
path = "../testkit"

[target.'cfg(unix)'.dependencies]
unix-only = { path = "../unix-only" }

[patch.crates-io]
tree-sitter = { path = "vendor/ts" }
"#,
    );
    write(
        &root.join("crates/core/Cargo.toml"),
        "[package]\nname = \"core\"\n\n[dependencies]\napp-util = { path = \"../util\" }\n",
    );
    write(
        &root.join("crates/util/Cargo.toml"),
        "[package]\nname = \"util\"\n",
    );
    write(
        &root.join("crates/experimental/Cargo.toml"),
        "[dependencies]\nx = { path = \"../../../x\" }\n",
    );
    write(
        &root.join("tools/cli/Cargo.toml"),
        "[package]\nname = \"cli\"\n",
    );
    // Vendored manifests are the dependency's, not the project's.
    write(
        &root.join("target/package/Cargo.toml"),
        "[dependencies]\nleak = { path = \"../../leak\" }\n",
    );

    let scan = scan_manifests(&real(&root), &["target/package/Cargo.toml".into()]);
    assert_eq!(scan.root_package.as_deref(), Some("app"));
    let got = summary(&scan, base);
    let expect = |kind, target: &str, manifest: &str, line, detail: &str| {
        (
            kind,
            target.to_owned(),
            manifest.to_owned(),
            Some(line),
            detail.to_owned(),
        )
    };
    assert_eq!(
        got,
        vec![
            expect(
                LinkKind::CargoWorkspaceMember,
                "app/crates/core",
                "app/Cargo.toml",
                3,
                "crates/core"
            ),
            expect(
                LinkKind::CargoWorkspaceMember,
                "app/crates/util",
                "app/Cargo.toml",
                3,
                "crates/util"
            ),
            expect(
                LinkKind::CargoWorkspaceMember,
                "app/tools/cli",
                "app/Cargo.toml",
                4,
                "tools/cli"
            ),
            expect(
                LinkKind::CargoPathDep,
                "shared",
                "app/Cargo.toml",
                9,
                "shared"
            ),
            expect(
                LinkKind::CargoPathDep,
                "lib",
                "app/Cargo.toml",
                16,
                "local-lib"
            ),
            expect(
                LinkKind::CargoPathDep,
                "testkit",
                "app/Cargo.toml",
                19,
                "testkit"
            ),
            expect(
                LinkKind::CargoPathDep,
                "unix-only",
                "app/Cargo.toml",
                22,
                "unix-only"
            ),
            expect(
                LinkKind::CargoPathDep,
                "app/vendor/ts",
                "app/Cargo.toml",
                25,
                "tree-sitter"
            ),
            expect(
                LinkKind::CargoPathDep,
                "app/crates/util",
                "app/crates/core/Cargo.toml",
                5,
                "app-util"
            ),
        ]
    );
}

#[test]
fn npm_workspaces_and_local_deps() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let root = base.join("web");
    write(
        &root.join("package.json"),
        r#"{
  "name": "web-root",
  "private": true,
  "workspaces": {
    "packages": ["packages/*", "!packages/ignored"]
  },
  "dependencies": {
    "left-pad": "^1.0.0",
    "sibling": "file:../sibling",
    "linked": "link:../linked-lib"
  },
  "devDependencies": {
    "@web/ui": "workspace:*",
    "@web/missing": "workspace:^",
    "by-path": "workspace:../by-path"
  }
}
"#,
    );
    write(
        &root.join("packages/ui/package.json"),
        r#"{ "name": "@web/ui", "dependencies": { "@web/core": "workspace:^1.0.0" } }"#,
    );
    write(
        &root.join("packages/core/package.json"),
        r#"{ "name": "@web/core" }"#,
    );
    write(
        &root.join("packages/ignored/package.json"),
        r#"{ "name": "ignored" }"#,
    );

    let scan = scan_manifests(&real(&root), &[]);
    assert_eq!(scan.root_package.as_deref(), Some("web-root"));
    let got = summary(&scan, base);
    let ws = |target: &str, detail: &str| {
        (
            LinkKind::NpmWorkspace,
            target.to_owned(),
            "web/package.json".to_owned(),
            Some(5),
            detail.to_owned(),
        )
    };
    let dep = |target: &str, manifest: &str, line, detail: &str| {
        (
            LinkKind::NpmFileDep,
            target.to_owned(),
            manifest.to_owned(),
            Some(line),
            detail.to_owned(),
        )
    };
    assert_eq!(
        got,
        vec![
            ws("web/packages/core", "packages/core"),
            ws("web/packages/ui", "packages/ui"),
            dep("sibling", "web/package.json", 9, "sibling"),
            dep("linked-lib", "web/package.json", 10, "linked"),
            dep("web/packages/ui", "web/package.json", 13, "@web/ui"),
            dep("by-path", "web/package.json", 15, "by-path"),
            dep(
                "web/packages/core",
                "web/packages/ui/package.json",
                1,
                "@web/core"
            ),
        ]
    );
}

#[test]
fn pnpm_workspace_yaml_lists_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("mono");
    write(
        &root.join("pnpm-workspace.yaml"),
        "# workspace\npackages:\n  - 'apps/*'\n  - \"libs/one\" # the one\ncatalog:\n  - not/a/package\n",
    );
    write(&root.join("apps/site/package.json"), r#"{"name":"site"}"#);
    write(&root.join("libs/one/package.json"), r#"{"name":"one"}"#);
    write(&root.join("not/a/package/package.json"), r#"{"name":"no"}"#);
    let scan = scan_manifests(&real(&root), &[]);
    let got = summary(&scan, tmp.path());
    assert_eq!(
        got.iter()
            .map(|(k, t, _, l, _)| (*k, t.as_str(), *l))
            .collect::<Vec<_>>(),
        [
            (LinkKind::NpmWorkspace, "mono/apps/site", Some(3)),
            (LinkKind::NpmWorkspace, "mono/libs/one", Some(4)),
        ]
    );
}

#[test]
fn go_replace_directives_to_local_dirs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("svc");
    write(
        &root.join("go.mod"),
        "module github.com/acme/svc\n\ngo 1.22\n\nreplace github.com/acme/lib => ../lib\n\n\
         replace (\n\tgithub.com/acme/util v1.0.0 => ./internal/util // local fork\n\
         \tgithub.com/x/y => github.com/fork/y v1.2.3\n\t\"github.com/acme/abs\" => /opt/abs\n)\n",
    );
    let scan = scan_manifests(&real(&root), &[]);
    assert_eq!(scan.root_package.as_deref(), Some("svc"));
    let got = summary(&scan, tmp.path());
    assert_eq!(
        got.iter()
            .map(|(k, t, _, l, d)| (*k, t.as_str(), *l, d.as_str()))
            .collect::<Vec<_>>(),
        [
            (LinkKind::GoReplace, "lib", Some(5), "github.com/acme/lib"),
            (
                LinkKind::GoReplace,
                "svc/internal/util",
                Some(8),
                "github.com/acme/util"
            ),
            (
                LinkKind::GoReplace,
                "/opt/abs",
                Some(10),
                "github.com/acme/abs"
            ),
        ]
    );
}

#[test]
fn malformed_manifests_yield_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bad");
    write(&root.join("Cargo.toml"), "[dependencies\nx = { path = ");
    write(&root.join("package.json"), "{ not json");
    let scan = scan_manifests(&real(&root), &[]);
    assert!(scan.links.is_empty());
    assert_eq!(scan.manifests_read, 2);
}
