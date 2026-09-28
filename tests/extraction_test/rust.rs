use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Rust Extraction')
// =============================================================================

#[test]
fn rust_extracts_function_declarations() {
    let code = r#"
pub fn process_data(input: &str) -> Result<Output, Error> {
    // Process data
    Ok(Output::new())
}
"#;
    let result = extract("lib.rs", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "process_data");
    assert_eq!(func_node.visibility, Some(Visibility::Public));
}

#[test]
fn rust_extracts_struct_declarations() {
    let code = r#"
pub struct User {
    pub id: String,
    pub name: String,
    email: String,
}
"#;
    let result = extract("models.rs", code);

    let struct_node = find_kind(&result, NodeKind::Struct).expect("struct");
    assert_eq!(struct_node.name, "User");
}

#[test]
fn rust_extracts_trait_declarations() {
    let code = r#"
pub trait Repository {
    fn find(&self, id: &str) -> Option<Entity>;
    fn save(&mut self, entity: Entity) -> Result<(), Error>;
}
"#;
    let result = extract("traits.rs", code);

    let trait_node = find_kind(&result, NodeKind::Trait).expect("trait");
    assert_eq!(trait_node.name, "Repository");
}

#[test]
fn rust_extracts_impl_trait_for_type_as_implements_edges() {
    let code = r#"
pub struct MyCache {}

pub trait Cache {
    fn get(&self, key: &str) -> Option<String>;
}

impl Cache for MyCache {
    fn get(&self, key: &str) -> Option<String> {
        None
    }
}
"#;
    let result = extract("cache.rs", code);

    // Should have an unresolved reference for implements
    let impl_ref = find_ref(&result, EdgeKind::Implements, "Cache").expect("implements ref");

    // The struct MyCache should be the source
    let my_cache_node = find_named(&result, NodeKind::Struct, "MyCache").expect("MyCache");
    assert_eq!(impl_ref.from_node_id, my_cache_node.id);
}

#[test]
fn rust_extracts_trait_supertraits_as_extends_references() {
    let code = r#"
pub trait Display {}

pub trait Error: Display {
    fn description(&self) -> &str;
}
"#;
    let result = extract("error.rs", code);

    let extends_ref = find_ref(&result, EdgeKind::Extends, "Display").expect("extends ref");

    let error_trait = find_named(&result, NodeKind::Trait, "Error").expect("Error trait");
    assert_eq!(extends_ref.from_node_id, error_trait.id);
}

#[test]
fn rust_does_not_create_implements_edges_for_plain_impl_blocks() {
    let code = r#"
pub struct Counter {
    count: u32,
}

impl Counter {
    pub fn new() -> Counter {
        Counter { count: 0 }
    }
    pub fn increment(&mut self) {
        self.count += 1;
    }
}
"#;
    let result = extract("counter.rs", code);

    // Should have no implements references (no trait involved)
    let impl_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(impl_refs.len(), 0);
}

#[test]
fn rust_derive_attributes_become_implements_edges() {
    let code = r#"
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Config {
    pub name: String,
}

#[derive(Default)]
enum Mode {
    A,
    B,
}
"#;
    let result = extract("config.rs", code);
    let implements = ref_names(&refs_of_kind(&result, EdgeKind::Implements));
    for t in ["Clone", "Debug", "PartialEq", "Default"] {
        assert!(
            implements.contains(&t.to_string()),
            "missing derive {t}: {implements:?}"
        );
    }
    // A path trait resolves to its last segment.
    assert!(
        implements.contains(&"Serialize".to_string()),
        "path-derive Serialize missing: {implements:?}"
    );
    assert!(
        !implements.contains(&"serde".to_string()),
        "path qualifier leaked: {implements:?}"
    );
}

#[test]
fn rust_unit_struct_value_bindings_become_references() {
    let code = r#"
struct ConstantFoldPass;

fn build_pipeline() {
    let fold_pass = ConstantFoldPass;
    let count = 5;
    let name = some_local;
}
"#;
    let result = extract("pipeline.rs", code);
    let refs = ref_names(&refs_of_kind(&result, EdgeKind::References));
    // The unit-struct binding links to the struct.
    assert!(
        refs.contains(&"ConstantFoldPass".to_string()),
        "value-path ref missing: {refs:?}"
    );
    // Lowercase locals and literals are not referenced (no edge noise).
    assert!(!refs.contains(&"some_local".to_string()), "{refs:?}");
}

#[test]
fn rust_unit_struct_is_indexed_with_implements_edge() {
    // Regression for upstream #1513: a Rust unit struct (`struct Unit;`) is a
    // complete definition, not a forward declaration. It must be indexed and
    // keep its `impl Trait for Unit` implements edge — like brace and tuple
    // structs already do.
    let code = r#"
pub struct UnitStruct;
pub struct TupleStruct(pub u32);
pub struct BraceStruct { pub x: u32 }

pub trait Greet { fn hi(&self) -> String; }

impl Greet for UnitStruct  { fn hi(&self) -> String { "unit".into()  } }
impl Greet for TupleStruct { fn hi(&self) -> String { "tuple".into() } }
impl Greet for BraceStruct { fn hi(&self) -> String { "brace".into() } }
"#;
    let result = extract("lib.rs", code);

    // All three struct forms are indexed (the unit form was previously dropped).
    let struct_names = names(&filter_kind(&result, NodeKind::Struct));
    for expected in ["UnitStruct", "TupleStruct", "BraceStruct"] {
        assert!(
            struct_names.contains(&expected.to_string()),
            "missing struct {expected}: {struct_names:?}"
        );
    }

    // Each struct keeps its `impl Greet for _` implements edge.
    let implements = refs_of_kind(&result, EdgeKind::Implements);
    let greet_impls = implements
        .iter()
        .filter(|r| r.reference_name == "Greet")
        .count();
    assert_eq!(
        greet_impls, 3,
        "expected 3 Greet implements edges: {implements:?}"
    );
}

// =============================================================================

/// A path type (`fmt::Result`, `crate::error::Error`, `D::Error`,
/// `Self::Item`) is one reference to the whole path: its last segment alone
/// named a same-named project type the path rules out. `Iterator<Item = T>`
/// references `T`, not the trait's associated type `Item`.
#[test]
fn rust_path_types_are_referenced_whole() {
    let code = r#"
use std::fmt;
pub struct Feed;
pub fn show(f: &mut fmt::Formatter<'_>, e: crate::error::Error) -> fmt::Result { Ok(()) }
pub fn parse<D: Deserializer>(d: D) -> Result<Feed, D::Error> { todo!() }
pub fn items(v: Vec<u8>) -> impl Iterator<Item = Feed> { std::iter::empty() }
pub fn cast<T: Tr>(x: <T as Tr>::Output) {}
"#;
    let result = extract("feed.rs", code);
    let refs = ref_names(&refs_of_kind(&result, EdgeKind::References));
    for whole in [
        "fmt::Formatter",
        "crate::error::Error",
        "fmt::Result",
        "D::Error",
    ] {
        assert!(
            refs.contains(&whole.to_string()),
            "missing {whole}: {refs:?}"
        );
    }
    for part in ["Formatter", "Error", "Item"] {
        assert!(
            !refs.contains(&part.to_string()),
            "bare {part} leaked: {refs:?}"
        );
    }
    // The bound type is still referenced; a qualified path's types are
    // walked one by one.
    assert!(refs.contains(&"Feed".to_string()), "{refs:?}");
    assert!(refs.contains(&"Tr".to_string()), "{refs:?}");
}

/// An impl whose Self type is defined in another file (`impl Visitor for
/// Map<K, V>`), borrowed (`for &mut Deserializer<R>`) or path-qualified
/// (`for a::B`) still qualifies its associated items by that type; a trait
/// path's generic arguments stay out of the implements reference.
#[test]
fn rust_impl_members_are_qualified_by_any_self_type() {
    let code = r#"
pub struct Deserializer<R>(R);
impl<'de, R: Read<'de>> de::Deserializer<'de> for &mut Deserializer<R> {
    type Error = Error;
    const LIMIT: usize = 1;
}
impl<'de> de::Visitor<'de> for Map<String, Value> {
    type Value = Map<String, Value>;
}
pub mod a { pub struct B; }
impl Marker for a::B {}
impl Index for usize {
    type Output = Value;
}
"#;
    let result = extract("de.rs", code);
    let aliases: Vec<&str> = filter_kind(&result, NodeKind::TypeAlias)
        .iter()
        .map(|node| node.qualified_name.as_str())
        .collect();
    for qualified in ["Deserializer::Error", "Map::Value", "usize::Output"] {
        assert!(
            aliases.contains(&qualified),
            "missing {qualified}: {aliases:?}"
        );
    }
    assert!(
        !aliases.contains(&"Error"),
        "an impl's type is no free item: {aliases:?}"
    );
    let limit = find_named(&result, NodeKind::Constant, "LIMIT").expect("impl const");
    assert_eq!(limit.qualified_name, "Deserializer::LIMIT");

    let implements = refs_of_kind(&result, EdgeKind::Implements);
    let deserializer = find_named(&result, NodeKind::Struct, "Deserializer").expect("struct");
    assert!(
        implements
            .iter()
            .any(|r| r.reference_name == "de::Deserializer" && r.from_node_id == deserializer.id),
        "{:?}",
        ref_names(&implements)
    );
    let b = find_named(&result, NodeKind::Struct, "B").expect("a::B");
    assert!(
        implements
            .iter()
            .any(|r| r.reference_name == "Marker" && r.from_node_id == b.id),
        "{:?}",
        ref_names(&implements)
    );
}

/// A `const`/`static` initializer's calls, closures included, are the
/// item's calls; the item is its name, never an identifier its initializer
/// reads. Calls with a turbofish inside macro arguments are calls too.
#[test]
fn rust_const_initializers_and_macro_turbofish_calls_are_extracted() {
    let code = r#"
fn helper() -> u32 { 1 }
fn build(x: u32) -> u32 { x }
fn parse<T>(x: u32) -> u32 { x }
const LIMIT: u32 = helper() + 1;
static TABLE: fn() -> u32 = || build(helper());
const ALIAS: u32 = LIMIT;
pub struct S;
impl S { const ZERO: u32 = helper(); pub fn get<T>(&self) -> Option<T> { None } }
pub fn run(s: &S) {
    assert!(s.get::<u32>().is_none());
    assert!(parse::<u8>(1) < 2);
}
"#;
    let result = extract("lib.rs", code);
    let calls_from = |name: &str| -> Vec<String> {
        let node = result
            .nodes
            .iter()
            .find(|node| {
                node.name == name
                    && matches!(
                        node.kind,
                        NodeKind::Constant | NodeKind::Variable | NodeKind::Function
                    )
            })
            .unwrap_or_else(|| panic!("{name}"));
        let mut names: Vec<String> = result
            .unresolved_references
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls && r.from_node_id == node.id)
            .map(|r| r.reference_name.clone())
            .collect();
        names.sort();
        names
    };
    assert_eq!(calls_from("LIMIT"), ["helper"]);
    assert_eq!(calls_from("TABLE"), ["build", "helper"]);
    assert_eq!(calls_from("ZERO"), ["helper"]);
    assert!(
        calls_from("run").contains(&"s.get".to_string()),
        "{:?}",
        calls_from("run")
    );
    assert!(
        calls_from("run").contains(&"parse".to_string()),
        "{:?}",
        calls_from("run")
    );
    // `const ALIAS: u32 = LIMIT;` is one constant, `ALIAS`: `LIMIT` is
    // defined once, not again by the initializer reading it.
    let constants = names(&filter_kind(&result, NodeKind::Constant));
    let count = |name: &str| {
        constants
            .iter()
            .filter(|constant| *constant == name)
            .count()
    };
    assert_eq!((count("ALIAS"), count("LIMIT")), (1, 1), "{constants:?}");
}

/// Pins how much the extractor records for a file exercising the shapes
/// above (a regression guard on node and reference counts).
#[test]
fn rust_scoped_items_extraction_counts_are_pinned() {
    let code = r#"
use std::fmt;
pub struct Deserializer<R>(R);
impl<'de, R: Read<'de>> de::Deserializer<'de> for &mut Deserializer<R> {
    type Error = Error;
    fn deserialize_any(self, f: &mut fmt::Formatter<'_>) -> fmt::Result { helper() }
}
fn helper() -> fmt::Result { Ok(()) }
const LIMIT: u32 = helper_count() + 1;
static TABLE: fn() -> u32 = || helper_count();
fn helper_count() -> u32 { 1 }
"#;
    let result = extract("de.rs", code);
    assert_eq!(
        result.nodes.len(),
        10,
        "{:#?}",
        result
            .nodes
            .iter()
            .map(|n| (n.kind, &n.qualified_name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result.unresolved_references.len(),
        9,
        "{:#?}",
        result
            .unresolved_references
            .iter()
            .map(|r| (r.reference_kind, &r.reference_name))
            .collect::<Vec<_>>()
    );
}
