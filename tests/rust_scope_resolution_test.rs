//! Rust names resolved the way rustc scopes them, end to end: index a small
//! crate, read the edges back. Each crate reproduces a shape where the
//! compiler (rust-analyzer's SCIP index) disagreed with tree-sitter
//! resolution on real projects (codegraph-rs, rms, serde_json, reqwest,
//! tokio): a `use`d std/dependency type taken for a same-named project
//! type, the first same-named struct picked regardless of `use`, a
//! same-named item of another module, a dependency's method or a wrapper's
//! delegation taken for a project method or for the caller itself.

use std::fs;
use std::path::Path;

use codegraph::db::{DatabaseConnection, get_database_path};
use codegraph::{CodeGraph, IndexOptions};

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Every `calls`/`references`/`implements` edge as `source -kind-> target
/// @file`.
fn edges(root: &Path) -> Vec<String> {
    let conn = DatabaseConnection::open(get_database_path(root)).unwrap();
    let db = conn.get_db().unwrap();
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.qualified_name || ' -' || e.kind || '-> ' || t.qualified_name || ' @' || t.file_path
             FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
             WHERE e.kind IN ('calls', 'references', 'implements') ORDER BY 1",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

async fn index_crate(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::TempDir::new().unwrap();
    write(
        &dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    for (path, content) in files {
        write(&dir.path().join(path), content);
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg.close();
    let edges = edges(dir.path());
    (dir, edges)
}

fn has(edges: &[String], expected: &str) -> bool {
    edges.iter().any(|edge| edge == expected)
}

/// No edge from `source` of `kind` lands on a node whose qualified name is
/// `target` (in any file).
fn none_to(edges: &[String], source: &str, kind: &str, target: &str) -> bool {
    let prefix = format!("{source} -{kind}-> {target} @");
    !edges.iter().any(|edge| edge.starts_with(&prefix))
}

/// `use crate::error::Result;` names `error.rs`'s alias, not a nearer
/// module's; `use axum::extract::State;`, `fmt::Formatter` and
/// `fmt::Result` name no project item whatever the project calls
/// `State`/`Formatter`/`Result`.
#[tokio::test(flavor = "current_thread")]
async fn a_use_decides_what_a_bare_type_names() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod calendar;\npub mod error;\npub mod feeds;\npub mod ser;\n"),
        (
            "src/error.rs",
            "pub struct Error;\npub type Result<T> = std::result::Result<T, Error>;\n",
        ),
        (
            "src/calendar.rs",
            "pub type Result<T> = std::result::Result<T, String>;\n\
             pub enum State { On }\n",
        ),
        ("src/ser.rs", "pub trait Formatter { fn begin(&mut self); }\n"),
        (
            "src/feeds.rs",
            "use std::fmt;\n\
             use axum::extract::State;\n\
             use crate::error::Result;\n\
             pub struct Feed;\n\
             pub fn load(state: State<u32>) -> Result<Feed> { let _ = state; Ok(Feed) }\n\
             impl fmt::Display for Feed {\n\
             \x20   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, \"feed\") }\n\
             }\n",
        ),
    ])
    .await;
    assert!(
        has(&edges, "load -references-> Result @src/error.rs"),
        "{edges:#?}"
    );
    assert!(none_to(&edges, "load", "references", "State"), "{edges:#?}");
    for target in ["Formatter", "Result"] {
        assert!(
            !edges
                .iter()
                .any(|edge| edge.starts_with("Feed::fmt -references->")
                    && edge.contains(&format!("> {target} @"))),
            "fmt:: path taken for the project's {target}: {edges:#?}"
        );
    }
}

/// A bare PascalCase name is not the first same-named struct of the project:
/// the one the module binds (its own, or `use`d) is, and one it defines in
/// an inline test module beats another file's namesake.
#[tokio::test(flavor = "current_thread")]
async fn a_struct_name_resolves_in_its_module_not_by_first_match() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "pub mod a_types;\npub mod graph;\npub mod walk;\npub mod share;\n",
        ),
        (
            "src/a_types.rs",
            "pub struct Node;\npub mod tests { pub struct Remote; }\n",
        ),
        ("src/graph.rs", "pub struct Node;\n"),
        (
            "src/walk.rs",
            "use crate::graph::Node;\npub fn visit(n: &Node) { let _ = n; }\n",
        ),
        (
            "src/share.rs",
            "pub fn serve() {}\n\
             #[cfg(test)]\n\
             mod tests {\n\
             \x20   pub struct Remote;\n\
             \x20   fn peer(r: &Remote) { let _ = r; }\n\
             }\n",
        ),
    ])
    .await;
    assert!(
        has(&edges, "visit -references-> Node @src/graph.rs"),
        "{edges:#?}"
    );
    assert!(
        has(
            &edges,
            "tests::peer -references-> tests::Remote @src/share.rs"
        ) || has(&edges, "peer -references-> tests::Remote @src/share.rs"),
        "{edges:#?}"
    );
}

/// Integration tests name the library by its crate name; `crate_name::a::B`
/// in a type or a call is followed from the library's root.
#[tokio::test(flavor = "current_thread")]
async fn a_workspace_crate_path_resolves_from_that_crate_root() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod types;\npub mod other;\n"),
        (
            "src/types.rs",
            "pub struct Node;\nimpl Node { pub fn new() -> Node { Node } }\n",
        ),
        ("src/other.rs", "pub struct Node;\n"),
        (
            "tests/api.rs",
            "use demo::types::Node;\n\
             fn build() -> Node { Node::new() }\n\
             fn path() -> demo::types::Node { demo::types::Node::new() }\n",
        ),
    ])
    .await;
    assert!(
        has(&edges, "build -references-> Node @src/types.rs"),
        "{edges:#?}"
    );
    assert!(
        has(&edges, "path -references-> Node @src/types.rs"),
        "{edges:#?}"
    );
    assert!(
        has(&edges, "build -calls-> Node::new @src/types.rs"),
        "{edges:#?}"
    );
}

/// `cfg_rt! { mod builder; pub use self::builder::Builder; }`: a re-export
/// inside a macro call is followed like any other, and `cfg` alternatives
/// (`#[cfg(test)] use mocks::spawn;` beside the real one) resolve to one of
/// them, never to a third namesake.
#[tokio::test(flavor = "current_thread")]
async fn uses_inside_macro_calls_and_cfg_alternatives_are_followed() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "macro_rules! cfg_rt { ($($item:item)*) => { $($item)* } }\n\
             pub mod runtime;\npub mod other;\npub mod user;\npub mod blocking;\npub mod mocks;\npub mod third;\n",
        ),
        (
            "src/runtime/mod.rs",
            "cfg_rt! {\n    mod builder;\n    pub use self::builder::Builder;\n}\n",
        ),
        (
            "src/runtime/builder.rs",
            "pub struct Builder;\nimpl Builder { pub fn new() -> Builder { Builder } }\n",
        ),
        ("src/other.rs", "pub struct Builder;\n"),
        ("src/blocking.rs", "pub fn spawn() {}\n"),
        ("src/mocks.rs", "pub fn spawn() {}\n"),
        ("src/third.rs", "pub fn spawn() {}\n"),
        (
            "src/user.rs",
            "use crate::runtime::Builder;\n\
             #[cfg(not(test))]\nuse crate::blocking::spawn;\n\
             #[cfg(test)]\nuse crate::mocks::spawn;\n\
             pub fn make() -> Builder { spawn(); Builder::new() }\n",
        ),
    ])
    .await;
    assert!(
        has(&edges, "make -references-> Builder @src/runtime/builder.rs"),
        "{edges:#?}"
    );
    assert!(
        has(&edges, "make -calls-> Builder::new @src/runtime/builder.rs"),
        "{edges:#?}"
    );
    assert!(
        has(&edges, "make -calls-> spawn @src/blocking.rs")
            || has(&edges, "make -calls-> spawn @src/mocks.rs"),
        "{edges:#?}"
    );
    assert!(
        !has(&edges, "make -calls-> spawn @src/third.rs"),
        "{edges:#?}"
    );
}

/// A binary's `main.rs` shares the root module path with `lib.rs` but not
/// its `use`s; a fn nested in another fn sees the file's `use`s, not a
/// module named after the outer fn.
#[tokio::test(flavor = "current_thread")]
async fn main_rs_and_nested_fns_see_their_own_scope() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "pub mod error;\npub mod summary;\npub use error::Result;\n\
             use std::path::Path;\n\
             pub fn outer(root: &Path) {\n\
             \x20   fn walk(dir: &Path) { let _ = dir; }\n\
             \x20   walk(root)\n\
             }\n",
        ),
        (
            "src/error.rs",
            "pub type Result<T> = std::result::Result<T, ()>;\n",
        ),
        ("src/summary.rs", "pub type Path = Vec<u32>;\n"),
        (
            "src/main.rs",
            "use anyhow::Result;\nfn main() -> Result<()> { Ok(()) }\n",
        ),
    ])
    .await;
    assert!(
        none_to(&edges, "main", "references", "Result"),
        "{edges:#?}"
    );
    assert!(
        none_to(&edges, "outer::walk", "references", "Path")
            && none_to(&edges, "walk", "references", "Path"),
        "{edges:#?}"
    );
    assert!(none_to(&edges, "outer", "references", "Path"), "{edges:#?}");
}

/// `Self::Output` in `impl Future for T` is the trait's associated type,
/// not the impl's `type Output`; a generic parameter (`R`) is no project
/// type named `R`; `T::Value` with `T: Trait` is the trait's declaration;
/// `include!(…)` is a macro, not a project fn named `include`.
#[tokio::test(flavor = "current_thread")]
async fn generics_associated_types_and_macros_resolve_to_their_namespace() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod io;\npub mod config;\npub mod files;\n"),
        (
            "src/io.rs",
            "pub struct R;\n\
             pub struct Take<R> { inner: R }\n\
             impl<R: std::io::Read> Take<R> {\n\
             \x20   pub fn get_ref(&self) -> &R { &self.inner }\n\
             }\n\
             pub struct Ready;\n\
             impl std::future::Future for Ready {\n\
             \x20   type Output = u8;\n\
             \x20   fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> { std::task::Poll::Ready(1) }\n\
             }\n",
        ),
        (
            "src/config.rs",
            "pub trait ConfigValue { type Value; }\n\
             pub struct Config<T: ConfigValue>(Option<T::Value>);\n\
             impl<T: ConfigValue> Config<T> { pub fn new(v: Option<T::Value>) -> Self { Config(v) } }\n",
        ),
        (
            "src/files.rs",
            "pub fn include(path: &str) -> &str { path }\n\
             pub const TEXT: &str = include!(\"text.rs\");\n",
        ),
        ("src/text.rs", "\"text\"\n"),
    ])
    .await;
    assert!(
        none_to(&edges, "Take::get_ref", "references", "R"),
        "{edges:#?}"
    );
    assert!(
        none_to(&edges, "Ready::poll", "references", "Ready::Output"),
        "{edges:#?}"
    );
    assert!(
        has(
            &edges,
            "Config::new -references-> ConfigValue::Value @src/config.rs"
        ),
        "{edges:#?}"
    );
    assert!(
        none_to(&edges, "TEXT", "references", "include"),
        "{edges:#?}"
    );
}

/// `self.m()` runs `m` on the enclosing impl's type: its inherent method
/// before a trait impl's, and a trait impl's `m` that calls `Type::m(self)`
/// delegates to the inherent `m` rather than recursing.
#[tokio::test(flavor = "current_thread")]
async fn self_calls_prefer_the_inherent_method_and_delegation_is_not_recursion() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "pub mod extractor;\npub mod declarations;\npub mod context;\n",
        ),
        (
            "src/context.rs",
            "pub trait Context { fn create(&mut self) -> u32; }\n",
        ),
        (
            "src/extractor.rs",
            "use crate::context::Context;\n\
             pub struct Extractor;\n\
             impl Context for Extractor {\n\
             \x20   fn create(&mut self) -> u32 { Extractor::create(self) }\n\
             }\n",
        ),
        (
            "src/declarations.rs",
            "use crate::extractor::Extractor;\n\
             impl Extractor {\n\
             \x20   pub(crate) fn create(&mut self) -> u32 { 1 }\n\
             \x20   pub(crate) fn build(&mut self) -> u32 { self.create() }\n\
             }\n",
        ),
    ])
    .await;
    assert!(
        has(
            &edges,
            "Extractor::build -calls-> Extractor::create @src/declarations.rs"
        ),
        "{edges:#?}"
    );
    assert!(
        has(
            &edges,
            "Extractor::create -calls-> Extractor::create @src/declarations.rs"
        ),
        "{edges:#?}"
    );
    assert!(
        !has(
            &edges,
            "Extractor::create -calls-> Extractor::create @src/extractor.rs"
        ),
        "{edges:#?}"
    );
}

/// Same-named types of different modules keep their own methods: the
/// `Sender` `mpsc::channel()` returns is `mpsc`'s, and a method called on a
/// field of another module's `Semaphore` is that one's, not the caller's.
#[tokio::test(flavor = "current_thread")]
async fn a_type_keeps_the_module_the_module_tree_places_it_in() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod sync;\npub mod app;\n"),
        (
            "src/sync/mod.rs",
            "pub mod mpsc;\npub mod broadcast;\npub mod semaphore;\npub mod batch_semaphore;\n",
        ),
        (
            "src/sync/mpsc.rs",
            "pub struct Sender;\n\
             impl Sender { pub fn send(&self) {} pub fn capacity(&self) -> usize { 0 } }\n\
             pub fn channel() -> (Sender, u8) { (Sender, 0) }\n",
        ),
        (
            "src/sync/broadcast.rs",
            "pub struct Sender;\nimpl Sender { pub fn send(&self) {} }\n",
        ),
        (
            "src/sync/batch_semaphore.rs",
            "pub struct Semaphore;\nimpl Semaphore { pub fn acquire(&self) {} }\n",
        ),
        (
            "src/sync/semaphore.rs",
            "use super::batch_semaphore as ll;\n\
             pub struct Semaphore { ll_sem: ll::Semaphore }\n\
             impl Semaphore { pub fn acquire(&self) { self.ll_sem.acquire() } }\n",
        ),
        (
            "src/app.rs",
            "use crate::sync::mpsc;\n\
             pub fn go() { let (tx, _rx) = mpsc::channel(); tx.send(); let _ = tx.capacity(); }\n",
        ),
    ])
    .await;
    assert!(
        has(&edges, "go -calls-> Sender::send @src/sync/mpsc.rs"),
        "{edges:#?}"
    );
    assert!(
        !has(&edges, "go -calls-> Sender::send @src/sync/broadcast.rs"),
        "{edges:#?}"
    );
    assert!(
        has(
            &edges,
            "Semaphore::acquire -calls-> Semaphore::acquire @src/sync/batch_semaphore.rs"
        ),
        "{edges:#?}"
    );
    assert!(
        !has(
            &edges,
            "Semaphore::acquire -calls-> Semaphore::acquire @src/sync/semaphore.rs"
        ),
        "{edges:#?}"
    );
}

/// A call on a receiver of unknown type inside `fn m` is not `m` itself (a
/// wrapper delegates to what it wraps); a tuple (`(StatusCode, body)`) or
/// another crate's builder chain runs no project method.
#[tokio::test(flavor = "current_thread")]
async fn wrappers_tuples_and_dependency_chains_run_no_guessed_project_method() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod wrap;\npub mod error;\npub mod routes;\npub mod state;\n"),
        (
            "src/wrap.rs",
            "pub struct Wrapper<T> { inner: T }\n\
             impl<T: Poll> Wrapper<T> {\n\
             \x20   pub fn poll_ready(&mut self) -> bool { self.inner.poll_ready() }\n\
             }\n\
             pub trait Poll { fn poll_ready(&mut self) -> bool; }\n",
        ),
        (
            "src/error.rs",
            "pub struct AppError;\n\
             pub trait IntoResponse { fn into_response(self) -> u16; }\n\
             impl IntoResponse for AppError { fn into_response(self) -> u16 { 500 } }\n\
             pub fn reply() -> u16 { let _e = AppError; (200, \"ok\").into_response() }\n",
        ),
        (
            "src/state.rs",
            "pub struct AppState;\nimpl AppState { pub fn with_state(self) -> Self { self } }\n",
        ),
        (
            "src/routes.rs",
            "use axum::Router;\n\
             use crate::state::AppState;\n\
             pub fn app(state: AppState) { let _ = Router::new().route(\"/\", 1).with_state(state); }\n",
        ),
    ])
    .await;
    assert!(
        !has(
            &edges,
            "Wrapper::poll_ready -calls-> Wrapper::poll_ready @src/wrap.rs"
        ),
        "{edges:#?}"
    );
    assert!(
        none_to(&edges, "reply", "calls", "AppError::into_response"),
        "{edges:#?}"
    );
    assert!(
        none_to(&edges, "app", "calls", "AppState::with_state"),
        "{edges:#?}"
    );
}

/// An axum route's namespaced handler (`get(api::list)`) names the fn of
/// that module, not the first project fn called `list`.
#[tokio::test(flavor = "current_thread")]
async fn a_namespaced_route_handler_resolves_in_its_module() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "pub mod api;\npub mod admin;\n\
             pub fn router() { let _ = axum::Router::new().route(\"/items\", get(api::list)); }\n",
        ),
        ("src/admin.rs", "pub async fn list() {}\n"),
        ("src/api.rs", "pub async fn list() {}\n"),
    ])
    .await;
    assert!(
        edges
            .iter()
            .any(|edge| edge.ends_with("-references-> list @src/api.rs")),
        "{edges:#?}"
    );
    assert!(
        !edges
            .iter()
            .any(|edge| edge.ends_with("-references-> list @src/admin.rs")),
        "{edges:#?}"
    );
}
