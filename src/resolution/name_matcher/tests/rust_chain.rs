//! Rust method calls on chained receivers (`a.b().m()`), typed link by link
//! from the receiver text extraction records, and calls on receivers of
//! unknown type whose method a dependency defines.

use super::Fixture;
use super::rust_call::{FILE, at_call, id, project_with, rust_node, target};
use crate::resolution::types::UnresolvedRef;
use crate::types::{Node, NodeKind, dropped_receiver_metadata};

fn method(qualified: &str, file: &str, line: u32, signature: &str) -> Node {
    let mut node = rust_node(NodeKind::Method, qualified, file, line);
    node.signature = Some(signature.into());
    node
}

fn function(qualified: &str, file: &str, line: u32, signature: &str) -> Node {
    let mut node = rust_node(NodeKind::Function, qualified, file, line);
    node.signature = Some(signature.into());
    node
}

/// Project types whose methods the chains below reach, each with a
/// same-named decoy on another type that name matching used to pick.
fn types() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Struct, "Rule", "src/rule.rs", 1),
        method("Rule::new", "src/rule.rs", 5, "(a: u8, b: u8) -> Self"),
        method("Rule::neg", "src/rule.rs", 10, "(self) -> Rule"),
        method("Negation::neg", "src/negation.rs", 10, "(self) -> Negation"),
        rust_node(NodeKind::Struct, "Hasher", "src/hash.rs", 1),
        method("Hasher::finish", "src/hash.rs", 5, "(&self) -> Digest"),
        rust_node(NodeKind::Struct, "Digest", "src/hash.rs", 20),
        method("Digest::as_u64", "src/hash.rs", 25, "(&self) -> u64"),
        method("Count::as_u64", "src/count.rs", 5, "(&self) -> u64"),
        rust_node(NodeKind::Struct, "Installer", "src/install.rs", 1),
        function("target", "src/install.rs", 20, "() -> Installer"),
        method(
            "Installer::install",
            "src/install.rs",
            5,
            "(&self, dir: &Path)",
        ),
        method(
            "Package::install",
            "src/package.rs",
            5,
            "(&self, dir: &Path)",
        ),
        rust_node(NodeKind::Struct, "Cache", "src/cache.rs", 1),
        method("Cache::clear", "src/cache.rs", 5, "(&mut self)"),
        method(
            "Registry::get",
            "src/registry.rs",
            5,
            "(&self, key: &str) -> Option<u8>",
        ),
        rust_node(NodeKind::Struct, "Registry", "src/registry.rs", 1),
        rust_node(NodeKind::Struct, "Widget", "src/widget.rs", 1),
        method("Widget::render", "src/widget.rs", 5, "(&self) -> String"),
        method("Page::render", "src/page.rs", 5, "(&self) -> String"),
        rust_node(NodeKind::Struct, "Builder", "src/builder.rs", 1),
        method("Builder::new", "src/builder.rs", 5, "() -> Self"),
        method(
            "Builder::name",
            "src/builder.rs",
            10,
            "(mut self, name: &str) -> Self",
        ),
        method("Builder::build", "src/builder.rs", 15, "(self) -> Widget"),
        method("Server::build", "src/server.rs", 5, "(self) -> u8"),
    ]
}

/// The holder whose fields the `self.…` chains start from.
fn holder() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Struct, "Holder", FILE, 1),
        rust_node(NodeKind::Field, "Holder::cache", FILE, 2),
        rust_node(NodeKind::Field, "Holder::map", FILE, 3),
        rust_node(NodeKind::Field, "Holder::items", FILE, 4),
        rust_node(NodeKind::Field, "Holder::names", FILE, 5),
    ]
}

const SOURCE: &str = "\
struct Holder {
    cache: RefCell<Cache>,
    map: Arc<Mutex<Registry>>,
    items: Vec<Widget>,
    names: Vec<String>,
}
impl Holder {
    fn walk(&self, x: Hasher) {
        Rule::new(1, 2).neg(); // assoc
        x.finish().as_u64(); // method return
        target().install(dir); // free fn
        self.cache.borrow_mut().clear(); // refcell
        self.map.lock().unwrap().get(k); // mutex
        self.items.get(0).unwrap().render(); // vec get
        self.items.iter().find(|w| w.ok()).unwrap().render(); // iter find
        Builder::new().name(\"a\").build(); // builder
        self.names.first().unwrap().render(); // external end
        let first = self.items.first().unwrap();
        first.render(); // typed local
    }
}
";

fn fixture() -> Fixture {
    let mut extra = types();
    extra.extend(holder());
    project_with(SOURCE, extra)
}

/// The dropped-receiver call of `name` on the line marked `marker`, with the
/// receiver extraction records for it.
fn chained(fixture: &Fixture, marker: &str, name: &str, receiver: &str) -> UnresolvedRef {
    let first = receiver.split(['.', ':', '(']).next().unwrap_or(receiver);
    UnresolvedRef {
        metadata: Some(dropped_receiver_metadata(Some(receiver.to_string()))),
        ..at_call(fixture, SOURCE, marker, name, first)
    }
}

fn resolved(fixture: &Fixture, marker: &str, name: &str, receiver: &str) -> Option<String> {
    target(fixture, &chained(fixture, marker, name, receiver))
}

#[test]
fn associated_fns_free_fns_and_methods_type_the_next_link() {
    let fixture = fixture();
    assert_eq!(
        resolved(&fixture, "// assoc", "neg", "Rule::new(..)"),
        Some(id(NodeKind::Method, "Rule::neg", "src/rule.rs"))
    );
    assert_eq!(
        resolved(&fixture, "// method return", "as_u64", "x.finish()"),
        Some(id(NodeKind::Method, "Digest::as_u64", "src/hash.rs"))
    );
    assert_eq!(
        resolved(&fixture, "// free fn", "install", "target()"),
        Some(id(NodeKind::Method, "Installer::install", "src/install.rs"))
    );
    assert_eq!(
        resolved(&fixture, "// builder", "build", "Builder::new().name(..)"),
        Some(id(NodeKind::Method, "Builder::build", "src/builder.rs"))
    );
}

/// `RefCell::borrow_mut`, `Mutex::lock` + `unwrap`, `Vec::get` + `unwrap`,
/// and `iter().find(..)` hand out what they hold, so even a common std name
/// (`clear`, `get`) resolves on the project type behind them.
#[test]
fn wrappers_and_containers_are_seen_through() {
    let fixture = fixture();
    assert_eq!(
        resolved(&fixture, "// refcell", "clear", "self.cache.borrow_mut()"),
        Some(id(NodeKind::Method, "Cache::clear", "src/cache.rs"))
    );
    assert_eq!(
        resolved(&fixture, "// mutex", "get", "self.map.lock().unwrap()"),
        Some(id(NodeKind::Method, "Registry::get", "src/registry.rs"))
    );
    let render = Some(id(NodeKind::Method, "Widget::render", "src/widget.rs"));
    assert_eq!(
        resolved(
            &fixture,
            "// vec get",
            "render",
            "self.items.get(..).unwrap()"
        ),
        render
    );
    assert_eq!(
        resolved(
            &fixture,
            "// iter find",
            "render",
            "self.items.iter().find(..).unwrap()"
        ),
        render
    );
}

/// A chain ending on an external type (`String`) runs no project method,
/// and a local initialized by a chain is typed like the chain.
#[test]
fn chains_end_on_external_types_and_type_locals() {
    let fixture = fixture();
    assert_eq!(
        resolved(
            &fixture,
            "// external end",
            "render",
            "self.names.first().unwrap()"
        ),
        None
    );
    let local = UnresolvedRef {
        reference_name: "first.render".into(),
        ..at_call(&fixture, SOURCE, "// typed local", "first.render", "first")
    };
    assert_eq!(
        target(&fixture, &local),
        Some(id(NodeKind::Method, "Widget::render", "src/widget.rs"))
    );
}

/// Locals bound by a tuple pattern, a `for` loop, or a closure parameter
/// take their type from what they destructure or iterate.
#[test]
fn tuples_loops_and_closure_params_are_typed() {
    let source = "\
impl Holder {
    fn walk(&self) {
        let (dir, widget) = setup();
        widget.render(); // tuple
        for item in &self.items {
            item.render(); // loop
        }
        for (i, w) in self.items.iter().enumerate() {
            w.render(); // enumerate
        }
        self.items.iter().filter(|w| w.render()); // closure
        let named = self.by_name.values().find(|p| p.render()); // map values
        self.by_name.iter().max_by(|(ka, a), (kb, b)| b.render()); // pair
        let (one, page) = (self.items.first().unwrap(), 2);
        one.render(); // tuple expression
        for shown in ALL_WIDGETS {
            shown.render(); // static elsewhere
        }
        match self.shown {
            Shown::Pages(pages) => pages.render(), // variant
        }
    }
}
";
    let mut extra = types();
    extra.extend(vec![
        rust_node(NodeKind::Struct, "Holder", "src/holder.rs", 1),
        rust_node(NodeKind::Field, "Holder::items", "src/holder.rs", 2),
        rust_node(NodeKind::Field, "Holder::by_name", "src/holder.rs", 3),
        function("setup", "src/setup.rs", 1, "() -> (TempDir, Widget)"),
        rust_node(NodeKind::Struct, "Page", "src/page.rs", 1),
        rust_node(NodeKind::Variable, "ALL_WIDGETS", "src/statics.rs", 1),
        rust_node(NodeKind::Enum, "Shown", "src/shown.rs", 1),
        rust_node(NodeKind::EnumMember, "Shown::Pages", "src/shown.rs", 2),
    ]);
    let mut fixture = project_with(source, extra);
    fixture.files.insert(
        "src/holder.rs".into(),
        "struct Holder {\n    items: Vec<Widget>,\n    by_name: HashMap<String, Page>,\n}\n".into(),
    );
    fixture.files.insert(
        "src/statics.rs".into(),
        "pub static ALL_WIDGETS: [&Widget; 1] = [&W];\n".into(),
    );
    fixture.files.insert(
        "src/shown.rs".into(),
        "pub enum Shown {\n    Pages(Box<Page>),\n}\n".into(),
    );
    let at = |marker: &str, receiver: &str| {
        let name = format!("{receiver}.render");
        UnresolvedRef {
            reference_name: name.clone(),
            ..at_call(&fixture, source, marker, &name, &name)
        }
    };
    let widget = Some(id(NodeKind::Method, "Widget::render", "src/widget.rs"));
    let page = Some(id(NodeKind::Method, "Page::render", "src/page.rs"));
    assert_eq!(target(&fixture, &at("// tuple", "widget")), widget);
    assert_eq!(target(&fixture, &at("// loop", "item")), widget);
    assert_eq!(target(&fixture, &at("// enumerate", "w")), widget);
    assert_eq!(target(&fixture, &at("// closure", "w")), widget);
    assert_eq!(target(&fixture, &at("// map values", "p")), page);
    assert_eq!(target(&fixture, &at("// pair", "b")), page);
    assert_eq!(target(&fixture, &at("// tuple expression", "one")), widget);
    assert_eq!(
        target(&fixture, &at("// static elsewhere", "shown")),
        widget
    );
    assert_eq!(target(&fixture, &at("// variant", "pages")), page);
}

/// `self.0` on a tuple struct is the field its declaration writes: a
/// wrapped project type runs its own methods, a wrapped foreign one none
/// (not the wrapper's same-named method).
#[test]
fn tuple_struct_fields_are_typed_from_the_declaration() {
    let source = "\
pub struct Wrapper(pub Cache);
pub struct Foreign(pub(crate) TextureElement<u8>);
impl Wrapper {
    fn clear(&mut self) {
        self.0.clear(); // wrapped
    }
}
impl Foreign {
    fn src(&self) -> u8 {
        self.0.src() // foreign
    }
}
";
    let mut extra = types();
    extra.extend(vec![
        rust_node(NodeKind::Struct, "Wrapper", FILE, 1),
        rust_node(NodeKind::Struct, "Foreign", FILE, 2),
    ]);
    let fixture = project_with(source, extra);
    let at = |marker: &str, name: &str| UnresolvedRef {
        metadata: Some(dropped_receiver_metadata(Some("self.0".into()))),
        ..at_call(&fixture, source, marker, name, "self.0")
    };
    assert_eq!(
        target(&fixture, &at("// wrapped", "clear")),
        Some(id(NodeKind::Method, "Cache::clear", "src/cache.rs"))
    );
    assert_eq!(target(&fixture, &at("// foreign", "src")), None);
}

/// A link whose type is not known stops the chain: the call is then
/// decided as before, by the method's name.
#[test]
fn an_unknown_link_leaves_the_call_to_its_name() {
    let fixture = fixture();
    // `unknown()` is on no type: `render` is project-specific, so name
    // matching still gets it, but no link after `unknown` is guessed.
    let reference = chained(&fixture, "// vec get", "render", "self.items.unknown()");
    assert!(target(&fixture, &reference).is_some());
    // `clear` is a std name: unresolved.
    let reference = chained(&fixture, "// refcell", "clear", "self.cache.unknown()");
    assert_eq!(target(&fixture, &reference), None);
}

fn graph_methods() -> Vec<Node> {
    vec![
        rust_node(NodeKind::Struct, "Graph", "src/graph.rs", 1),
        method("Graph::walk", "src/graph.rs", 5, "(&self)"),
        method("Graph::render_all", "src/graph.rs", 10, "(&self)"),
    ]
}

/// A method a direct dependency defines is not guessed onto a project
/// method when the receiver's type is unknown, dropped or plain, and the
/// file never names the project type that defines it; a project-specific
/// name still resolves by name.
#[test]
fn dependency_method_names_are_not_guessed() {
    let source = "\
impl Walker {
    fn run(&self) {
        nodes.iter().for_each(|node| node.walk()); // closure
        tree.root().walk(); // dropped
        tree.root().render_all(); // project name
    }
}
";
    let mut fixture = project_with(source, graph_methods());
    let walk = Some(id(NodeKind::Method, "Graph::walk", "src/graph.rs"));
    let closure = |fixture: &Fixture| {
        let at = at_call(fixture, source, "// closure", "node.walk", "node.walk");
        UnresolvedRef {
            reference_name: "node.walk".into(),
            ..at
        }
    };
    let dropped = |fixture: &Fixture, name: &str, marker: &str| UnresolvedRef {
        metadata: Some(dropped_receiver_metadata(Some("tree.root()".into()))),
        ..at_call(fixture, source, marker, name, "tree")
    };
    // Without the dependency names, both guesses land on `Graph::walk`.
    assert_eq!(target(&fixture, &closure(&fixture)), walk);
    assert_eq!(
        target(&fixture, &dropped(&fixture, "walk", "// dropped")),
        walk
    );

    fixture.dependency_methods = vec!["walk".into()];
    assert_eq!(target(&fixture, &closure(&fixture)), None);
    assert_eq!(
        target(&fixture, &dropped(&fixture, "walk", "// dropped")),
        None
    );
    assert_eq!(
        target(
            &fixture,
            &dropped(&fixture, "render_all", "// project name")
        ),
        Some(id(NodeKind::Method, "Graph::render_all", "src/graph.rs"))
    );
}

/// Where the file names the project type (`Graph`), a dependency's method
/// name may still land on that type's method, typed receiver or not.
#[test]
fn dependency_method_names_reach_types_the_file_names() {
    let source = "\
impl Walker {
    fn run(&self, graph: Graph) {
        graph.walk(); // typed
        nodes.iter().for_each(|node| node.walk()); // closure
    }
}
";
    let mut fixture = project_with(source, graph_methods());
    fixture.dependency_methods = vec!["walk".into()];
    let walk = Some(id(NodeKind::Method, "Graph::walk", "src/graph.rs"));
    for (marker, name) in [("// typed", "graph.walk"), ("// closure", "node.walk")] {
        let reference = UnresolvedRef {
            reference_name: name.into(),
            ..at_call(&fixture, source, marker, name, name)
        };
        assert_eq!(target(&fixture, &reference), walk, "{name}");
    }
}
