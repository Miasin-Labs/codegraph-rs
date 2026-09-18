//! Synthetic machines for the dependency-store tests: a fake `$CARGO_HOME`
//! registry, a Go module cache, projects with lockfiles and `node_modules`,
//! and a scratch `$CODEGRAPH_HOME`. Nothing touches the real home.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use codegraph::deps::DepsHome;
use codegraph::deps::locate::SourceRoots;

pub struct Machine {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub cargo_home: PathBuf,
    pub go_mod_cache: PathBuf,
    pub codegraph_home: PathBuf,
}

impl Machine {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("codegraph-deps-test-")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let machine = Machine {
            cargo_home: root.join("cargo"),
            go_mod_cache: root.join("gomod"),
            codegraph_home: root.join("cghome"),
            root,
            _tmp: tmp,
        };
        machine.add_crate(
            "alpha",
            "1.0.0",
            &[
                (
                    "src/lib.rs",
                    "pub mod parse;\n\npub struct Alpha { pub n: u32 }\n\nimpl Alpha {\n    pub fn new(n: u32) -> Self { Alpha { n } }\n    pub fn double(&self) -> u32 { parse::twice(self.n) }\n}\n",
                ),
                ("src/parse.rs", "pub fn twice(n: u32) -> u32 { n * 2 }\n\npub fn alpha_only_helper() {}\n"),
                ("tests/it.rs", "fn in_test_only() {}\n"),
                ("benches/b.rs", "fn in_bench_only() {}\n"),
                ("examples/e.rs", "fn in_example_only() {}\n"),
                ("build.rs", "fn main() {}\n"),
            ],
        );
        machine.add_crate(
            "beta",
            "2.0.0",
            &[(
                "src/lib.rs",
                "pub trait Beta { fn beta(&self) -> u8; }\n\npub fn make_beta() -> u8 { 7 }\n",
            )],
        );
        machine
    }

    pub fn home(&self) -> DepsHome {
        DepsHome::at(self.codegraph_home.join("deps"))
    }

    pub fn roots(&self) -> SourceRoots {
        SourceRoots::new(
            Some(self.cargo_home.clone()),
            Some(self.go_mod_cache.clone()),
        )
    }

    pub fn crate_dir(&self, name: &str, version: &str) -> PathBuf {
        self.cargo_home
            .join("registry/src/index.crates.io-1949cf8c6b5b557f")
            .join(format!("{name}-{version}"))
    }

    pub fn add_crate(&self, name: &str, version: &str, files: &[(&str, &str)]) {
        let dir = self.crate_dir(name, version);
        write(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        );
        for (path, text) in files {
            write(&dir.join(path), text);
        }
    }

    /// A Rust project whose Cargo.lock pins alpha (direct), beta
    /// (transitive via alpha), a git crate that isn't checked out, and a
    /// path dependency.
    pub fn rust_project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        write(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        );
        write(
            &dir.join("src/main.rs"),
            "fn main() { let a = alpha::Alpha::new(1); a.double(); }\n",
        );
        write(
            &dir.join("Cargo.lock"),
            &format!(
                "version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\n \"alpha\",\n \"localdep\",\n \"forked\",\n]\n\n[[package]]\nname = \"alpha\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\ndependencies = [\n \"beta\",\n]\n\n[[package]]\nname = \"beta\"\nversion = \"2.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"forked\"\nversion = \"0.3.0\"\nsource = \"git+https://github.com/someone/forked?rev=abc#abcdef0123456789abcdef0123456789abcdef01\"\n\n[[package]]\nname = \"localdep\"\nversion = \"0.0.1\"\n"
            ),
        );
        dir
    }

    /// A project that only uses beta.
    pub fn beta_project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        write(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        );
        write(
            &dir.join("Cargo.lock"),
            &format!(
                "version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\n \"beta\",\n]\n\n[[package]]\nname = \"beta\"\nversion = \"2.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
            ),
        );
        dir
    }

    /// A TypeScript project (package-lock v3) with a typed package in
    /// `node_modules` and one lockfile entry that isn't installed.
    pub fn npm_project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        write(
            &dir.join("package.json"),
            r#"{"name":"web","dependencies":{"typed-lib":"^1.0.0","absent":"^3"}}"#,
        );
        write(
            &dir.join("package-lock.json"),
            r#"{"name":"web","lockfileVersion":3,"packages":{
                "":{"name":"web","dependencies":{"typed-lib":"^1.0.0","absent":"^3"}},
                "node_modules/typed-lib":{"version":"1.2.0"},
                "node_modules/absent":{"version":"3.0.0"}}}"#,
        );
        let lib = dir.join("node_modules/typed-lib");
        write(
            &lib.join("package.json"),
            r#"{"name":"typed-lib","version":"1.2.0","types":"index.d.ts"}"#,
        );
        write(
            &lib.join("index.d.ts"),
            "export declare class Client {\n  constructor(url: string);\n  fetchUser(id: number): Promise<User>;\n}\nexport interface User { id: number; name: string }\nexport declare function connect(url: string): Client;\n",
        );
        write(
            &lib.join("index.js"),
            "exports.connect = function connect(url) { return url; };\n",
        );
        write(&lib.join("test/client.test.ts"), "const inTestOnly = 1;\n");
        dir
    }

    /// A Go module requiring a mixed-case module from the module cache.
    pub fn go_project(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        write(
            &dir.join("go.mod"),
            "module example.com/app\n\ngo 1.22\n\nrequire (\n\tgithub.com/BurntSushi/toml v1.2.3\n\tgolang.org/x/sys v0.1.0 // indirect\n)\n",
        );
        let module = self
            .go_mod_cache
            .join("github.com/!burnt!sushi/toml@v1.2.3");
        write(
            &module.join("go.mod"),
            "module github.com/BurntSushi/toml\n",
        );
        write(
            &module.join("decode.go"),
            "package toml\n\n// Decode parses data into v.\nfunc Decode(data string, v interface{}) error { return nil }\n\ntype Decoder struct{}\n\nfunc (d *Decoder) Decode(v interface{}) error { return nil }\n",
        );
        write(
            &module.join("decode_test.go"),
            "package toml\n\nfunc TestOnly() {}\n",
        );
        dir
    }
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Every file under `dir` with its size and modification time.
pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .map(|e| {
            let meta = e.metadata().unwrap();
            (
                e.path().to_path_buf(),
                (
                    if meta.is_file() { meta.len() } else { 0 },
                    meta.modified().unwrap(),
                ),
            )
        })
        .collect()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
