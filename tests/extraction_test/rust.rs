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

// =============================================================================
