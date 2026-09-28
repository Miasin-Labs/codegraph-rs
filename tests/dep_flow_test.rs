//! The reqwest proxy case in miniature, answered by `deps flow`: two
//! fixture crates shaped like reqwest (`reqish`) and hyper-util
//! (`hyperish`). A client builder reads its proxies from the environment
//! through the other crate's matcher (`env::var("HTTP_PROXY")` → …
//! `ClientBuilder::build`'s return), and the connector's proxy branch hands
//! the resolving connect only the proxy's address — the target (`dst`)
//! goes to the CONNECT tunnel, never to the resolver.

use std::fs;
use std::path::{Path, PathBuf};

use codegraph::deps::builder::{BuildOptions, build_pending};
use codegraph::deps::locate::SourceRoots;
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::PendingScope;
use codegraph::deps::summaries::flow::{FlowFrom, FlowLimits, FlowReport, flow};
use codegraph::deps::{DepsHome, Registry};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const HYPERISH: &str = r#"pub mod matcher;
pub struct HttpConnector {
    resolver: Resolver,
}
impl HttpConnector {
    pub fn call(&mut self, dst: Uri) -> Conn {
        self.resolver.resolve(dst.host())
    }
}
"#;

const MATCHER: &str = r#"pub struct Matcher {
    http: String,
    no: String,
}
impl Matcher {
    pub fn from_system() -> Self {
        Builder::from_env().build()
    }
}
pub struct Builder {
    http: String,
    no: String,
}
impl Builder {
    fn from_env() -> Self {
        Builder {
            http: get_first_env(&["HTTP_PROXY", "http_proxy"]),
            no: get_first_env(&["NO_PROXY", "no_proxy"]),
        }
    }
    pub fn build(self) -> Matcher {
        Matcher {
            http: self.http,
            no: self.no,
        }
    }
}
fn get_first_env(names: &[&str]) -> String {
    for name in names {
        if let Ok(val) = std::env::var(name) {
            return val;
        }
    }
    String::new()
}
"#;

const REQISH: &str = r#"use hyperish::matcher;
use hyperish::HttpConnector;

pub struct ProxyMatcher {
    inner: matcher::Matcher,
}
impl ProxyMatcher {
    pub(crate) fn system() -> Self {
        ProxyMatcher {
            inner: matcher::Matcher::from_system(),
        }
    }
}
pub struct ClientBuilder {
    proxies: Vec<ProxyMatcher>,
    auto_sys_proxy: bool,
}
pub struct Client {
    proxies: Vec<ProxyMatcher>,
}
impl ClientBuilder {
    pub fn build(self) -> Client {
        let mut proxies = self.proxies;
        if self.auto_sys_proxy {
            proxies.push(ProxyMatcher::system());
        }
        Client { proxies }
    }
}
pub struct Connector {
    http: HttpConnector,
}
impl Connector {
    fn connect_via_proxy(self, dst: Uri, proxy: Uri) -> Conn {
        let proxy_dst = proxy.clone();
        let mut tunnel = Tunnel::new(proxy_dst.clone());
        tunnel.call(dst.clone());
        self.connect_with_maybe_proxy(proxy_dst)
    }
    fn connect_with_maybe_proxy(self, dst: Uri) -> Conn {
        let mut http = self.http;
        http.call(dst)
    }
}
"#;

struct Machine {
    _tmp: tempfile::TempDir,
    cargo_home: PathBuf,
    home: DepsHome,
    app: PathBuf,
}

fn machine() -> Machine {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let cargo_home = root.join("cargo");
    let registry = cargo_home.join("registry/src/index.crates.io-1949cf8c6b5b557f");
    for (name, files) in [
        (
            "hyperish",
            vec![("src/lib.rs", HYPERISH), ("src/matcher.rs", MATCHER)],
        ),
        ("reqish", vec![("src/lib.rs", REQISH)]),
    ] {
        let dir = registry.join(format!("{name}-0.1.0"));
        write(
            &dir.join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        );
        for (path, text) in files {
            write(&dir.join(path), text);
        }
    }
    let app = root.join("app");
    write(
        &app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nreqish = \"0.1\"\n",
    );
    write(&app.join("src/lib.rs"), "pub fn main() {}\n");
    let source = "source = \"registry+https://github.com/rust-lang/crates.io-index\"";
    write(
        &app.join("Cargo.lock"),
        &format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"reqish\",\n]\n\n\
             [[package]]\nname = \"hyperish\"\nversion = \"0.1.0\"\n{source}\n\n\
             [[package]]\nname = \"reqish\"\nversion = \"0.1.0\"\n{source}\ndependencies = [\n \"hyperish\",\n]\n"
        ),
    );
    Machine {
        home: DepsHome::at(root.join("cghome/deps")),
        _tmp: tmp,
        cargo_home,
        app,
    }
}

fn run(m: &Machine, function: &str, from: FlowFrom) -> FlowReport {
    flow(
        &m.home,
        &m.app,
        "reqish",
        function,
        &from,
        &FlowLimits::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_case_across_two_crates() {
    let m = machine();
    m.home.ensure().unwrap();
    let mut registry = Registry::open(&m.home.registry_path()).unwrap();
    record_project(
        &mut registry,
        &m.app,
        &SourceRoots::new(Some(m.cargo_home.clone()), None),
        false,
        1,
    )
    .unwrap();
    let root = canonical_root(&m.app);
    build_pending(
        &m.home,
        &registry,
        PendingScope::Project(&root),
        &BuildOptions::default(),
        &mut |_| {},
    )
    .await
    .unwrap();

    // The environment reaches the built client, through the other crate.
    let report = run(&m, "ClientBuilder::build", FlowFrom::Environment);
    assert!(!report.partial);
    let env = report
        .flows
        .iter()
        .find(|f| f.from == "std::env::var" && f.to == "return")
        .unwrap_or_else(|| panic!("env reaches build's return: {report:#?}"));
    let packages: Vec<&str> = env.steps.iter().map(|s| s.package.as_str()).collect();
    assert_eq!(packages.first(), Some(&"hyperish"), "{packages:?}");
    assert_eq!(packages.last(), Some(&"reqish"), "{packages:?}");
    let first = &env.steps[0];
    assert!(
        first.file.ends_with("hyperish-0.1.0/src/matcher.rs"),
        "{first:?}"
    );
    assert!(first.code.contains("std::env::var(name)"), "{first:?}");
    assert!(
        env.steps
            .iter()
            .any(|s| s.code.contains("proxies.push(ProxyMatcher::system())")),
        "{env:#?}"
    );

    // Through the proxy, the target goes to the tunnel, never to the
    // resolving connect; the proxy's address does.
    let dst = run(&m, "connect_via_proxy", FlowFrom::Param("dst".into()));
    let to: Vec<&str> = dst.flows.iter().map(|f| f.to.as_str()).collect();
    assert!(to.contains(&"tunnel.call#0"), "{to:?}");
    assert!(
        !to.iter().any(|t| t.contains("connect_with_maybe_proxy")),
        "{to:?}"
    );
    let proxy = run(&m, "connect_via_proxy", FlowFrom::Param("proxy".into()));
    let to: Vec<&str> = proxy.flows.iter().map(|f| f.to.as_str()).collect();
    assert!(to.contains(&"self.connect_with_maybe_proxy#0"), "{to:?}");
    // And that connect is the one that resolves.
    let resolving = run(
        &m,
        "connect_with_maybe_proxy",
        FlowFrom::Param("dst".into()),
    );
    let to: Vec<&str> = resolving.flows.iter().map(|f| f.to.as_str()).collect();
    assert!(to.contains(&"http.call#0"), "{to:?}");
}
