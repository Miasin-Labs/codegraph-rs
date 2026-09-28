//! Rust trait-object and generic dispatch, end to end: index a small crate
//! and read the edges back. A call on `dyn Trait` (behind `&`, `Box`,
//! `Arc`), on `impl Trait`, or on a generic bounded by a project trait has
//! one static target — the trait's declaration, which rustc and
//! rust-analyzer name — and the implementations hang off that declaration
//! as `interface-impl` dispatch edges. The call edge says how it
//! dispatches (`dispatch`: `dynamic` / `generic`, `resolvedBy`:
//! `trait-dispatch`); no single implementation is ever guessed.

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

/// Every `calls` edge as `source -> target @file [resolvedBy/dispatch]`,
/// dispatch edges as `source => target @file [interface-impl]`.
fn call_edges(root: &Path) -> Vec<String> {
    let conn = DatabaseConnection::open(get_database_path(root)).unwrap();
    let db = conn.get_db().unwrap();
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.qualified_name,
                    CASE WHEN json_extract(e.metadata, '$.synthesizedBy') = 'interface-impl'
                         THEN ' => ' ELSE ' -> ' END,
                    t.qualified_name, t.file_path,
                    coalesce(json_extract(e.metadata, '$.synthesizedBy'),
                             json_extract(e.metadata, '$.resolvedBy'), ''),
                    coalesce(json_extract(e.metadata, '$.dispatch'), '')
             FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
             WHERE e.kind = 'calls' ORDER BY 1, 3",
        )
        .unwrap();
    stmt.query_map([], |row| {
        let dispatch: String = row.get(5)?;
        let how: String = row.get(4)?;
        let label = if dispatch.is_empty() {
            how
        } else {
            format!("{how}/{dispatch}")
        };
        Ok(format!(
            "{}{}{} @{} [{label}]",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })
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
    let edges = call_edges(dir.path());
    (dir, edges)
}

fn has(edges: &[String], expected: &str) -> bool {
    edges.iter().any(|edge| edge == expected)
}

const SHAPES: &str = "\
use std::sync::Arc;

pub trait Named {
    fn name(&self) -> String;
}

pub trait Shape: Named {
    fn area(&self) -> f64;
    fn describe(&self) -> String {
        self.name()
    }
}

pub struct Square(pub f64);
pub struct Circle(pub f64);
pub enum Blob {
    Unit,
}

impl Named for Square {
    fn name(&self) -> String {
        String::new()
    }
}
impl Shape for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}
impl Shape for Circle {
    fn area(&self) -> f64 {
        self.0 * 3.0
    }
}
impl Shape for Blob {
    fn area(&self) -> f64 {
        1.0
    }
}
";

/// `&dyn Shape`, `Box<dyn Shape>`, `Arc<dyn Shape + Send>`: the call is
/// the trait's declaration, `dynamic`; every implementation (an enum's
/// too) is a dispatch edge off that declaration.
#[tokio::test(flavor = "current_thread")]
async fn a_trait_object_call_targets_the_declaration_and_dispatches() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod shapes;\npub mod use_dyn;\n"),
        ("src/shapes.rs", SHAPES),
        (
            "src/use_dyn.rs",
            "use std::sync::Arc;\n\
             use crate::shapes::Shape;\n\
             pub fn on_ref(s: &dyn Shape) -> f64 { s.area() }\n\
             pub fn on_box(s: Box<dyn Shape>) -> f64 { s.area() }\n\
             pub fn on_arc(s: Arc<dyn Shape + Send>) -> String { s.describe() }\n",
        ),
    ])
    .await;
    for caller in ["on_ref", "on_box"] {
        assert!(
            has(
                &edges,
                &format!("{caller} -> Shape::area @src/shapes.rs [trait-dispatch/dynamic]")
            ),
            "{caller}: {edges:#?}"
        );
    }
    assert!(
        has(
            &edges,
            "on_arc -> Shape::describe @src/shapes.rs [trait-dispatch/dynamic]"
        ),
        "{edges:#?}"
    );
    for implementation in ["Square", "Circle", "Blob"] {
        assert!(
            has(
                &edges,
                &format!("Shape::area => {implementation}::area @src/shapes.rs [interface-impl]")
            ),
            "{implementation}: {edges:#?}"
        );
    }
    // Never one implementation guessed for the call site.
    assert!(
        !edges
            .iter()
            .any(|edge| edge.starts_with("on_") && edge.contains("Square::area")),
        "{edges:#?}"
    );
}

/// `S: Shape`, `where S: Shape`, `impl Shape`, `S::area(s)`: the
/// declaration, `generic`; a supertrait's method through its subtrait
/// bound; a concrete receiver still runs its own impl.
#[tokio::test(flavor = "current_thread")]
async fn a_bounded_generic_call_targets_the_bound_trait() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "pub mod shapes;\npub mod use_generic;\n"),
        ("src/shapes.rs", SHAPES),
        (
            "src/use_generic.rs",
            "use crate::shapes::{Shape, Square};\n\
             pub fn bound<S: Shape>(s: &S) -> f64 { s.area() }\n\
             pub fn clause<S>(s: S) -> f64\n\
             where\n\
             \x20   S: Shape + Clone,\n\
             {\n\
             \x20   s.area()\n\
             }\n\
             pub fn opaque(s: impl Shape) -> f64 { s.area() }\n\
             pub fn path<S: Shape>(s: &S) -> f64 { S::area(s) }\n\
             pub fn inherited<S: Shape>(s: &S) -> String { s.name() }\n\
             pub fn concrete(s: &Square) -> f64 { s.area() }\n\
             pub struct Holder<S> { inner: S }\n\
             impl<S: Shape> Holder<S> {\n\
             \x20   pub fn area(&self) -> f64 { self.inner.area() }\n\
             }\n",
        ),
    ])
    .await;
    for caller in ["bound", "clause", "opaque", "path", "Holder::area"] {
        assert!(
            has(
                &edges,
                &format!("{caller} -> Shape::area @src/shapes.rs [trait-dispatch/generic]")
            ),
            "{caller}: {edges:#?}"
        );
    }
    assert!(
        has(
            &edges,
            "inherited -> Named::name @src/shapes.rs [trait-dispatch/generic]"
        ),
        "a supertrait's method: {edges:#?}"
    );
    assert!(
        has(
            &edges,
            "concrete -> Square::area @src/shapes.rs [instance-method]"
        ),
        "{edges:#?}"
    );
}

/// Two bounds declaring the same method name leave the call without a
/// target, as rustc would reject it unqualified; a bound that is another
/// crate's trait runs no project method.
#[tokio::test(flavor = "current_thread")]
async fn an_ambiguous_or_foreign_bound_resolves_to_nothing() {
    let (_dir, edges) = index_crate(&[(
        "src/lib.rs",
        "pub trait A { fn go(&self); }\n\
             pub trait B { fn go(&self); }\n\
             pub struct S;\n\
             impl S { pub fn read(&self) {} }\n\
             pub fn both<T: A + B>(t: &T) { t.go() }\n\
             pub fn foreign<R: std::io::Read>(r: &mut R) { let _ = r.read(&mut []); }\n",
    )])
    .await;
    assert!(
        !edges.iter().any(|edge| edge.starts_with("both -> ")),
        "{edges:#?}"
    );
    assert!(
        !edges.iter().any(|edge| edge.starts_with("foreign -> ")),
        "{edges:#?}"
    );
}
