use crate::extraction_test::fixture::*;

// describe('Instantiates + Decorates edge extraction')
// =============================================================================

#[test]
fn instantiates_emits_an_instantiates_ref_for_new_foo() {
    let code = "
class Foo {}
function bootstrap() { return new Foo(); }
";
    let result = extract("app.ts", code);
    assert!(find_ref(&result, EdgeKind::Instantiates, "Foo").is_some());
}

#[test]
fn instantiates_strips_type_argument_suffix_from_generic_constructors() {
    let code = "
class Container<T> { constructor(_: T) {} }
function go() { return new Container<string>('x'); }
";
    let result = extract("app.ts", code);
    let instantiates = refs_of_kind(&result, EdgeKind::Instantiates);
    let r = instantiates.first().expect("instantiates ref");
    // Container<string> must be normalised to "Container" — otherwise
    // resolution can never match the class node.
    assert_eq!(r.reference_name, "Container");
}

#[test]
fn instantiates_keeps_trailing_identifier_from_qualified_new_ns_foo() {
    let code = "
const ns = { Foo: class {} };
function go() { return new ns.Foo(); }
";
    let result = extract("app.ts", code);
    let instantiates = refs_of_kind(&result, EdgeKind::Instantiates);
    // We can't always resolve which Foo, but the name should be the
    // simple identifier so name-matching has a chance.
    assert_eq!(
        instantiates.first().map(|r| r.reference_name.as_str()),
        Some("Foo")
    );
}

#[test]
fn decorates_emits_a_decorates_ref_for_foo_class_x() {
    let code = "
function Foo(_arg: string) { return (cls: any) => cls; }
@Foo('x')
class X {}
";
    let result = extract("app.ts", code);
    assert!(find_ref(&result, EdgeKind::Decorates, "Foo").is_some());
}

#[test]
fn decorates_does_not_attribute_a_prior_classs_decorator_to_the_next_class() {
    // Regression: the sibling-walk must stop at the first non-
    // decorator separator. `@A class Foo {} @B class Bar {}` must
    // produce `decorates(Foo, A)` and `decorates(Bar, B)` — never
    // `decorates(Bar, A)`.
    let code = "
function A(cls: any) { return cls; }
function B(cls: any) { return cls; }
@A
class Foo {}
@B
class Bar {}
";
    let result = extract("app.ts", code);
    let decorates_edges = refs_of_kind(&result, EdgeKind::Decorates);
    // Exactly one decorates ref per decorated class, no cross-attribution.
    let from_bar: Vec<_> = decorates_edges
        .iter()
        .filter(|r| {
            result
                .nodes
                .iter()
                .any(|n| n.id == r.from_node_id && n.name == "Bar")
        })
        .collect();
    assert_eq!(from_bar.len(), 1);
    assert_eq!(from_bar[0].reference_name, "B");
}

#[test]
fn decorates_emits_a_decorates_ref_for_foo_method() {
    let code = "
function Get(p: string) { return (t: any, k: string) => t; }
class Svc {
  @Get('/x') method() { return 1; }
}
";
    let result = extract("app.ts", code);
    let decor_method = find_ref(&result, EdgeKind::Decorates, "Get").expect("decorates ref");
    // The decorated symbol must be `method`, not the constructor or class.
    let decorated_node = result
        .nodes
        .iter()
        .find(|n| n.id == decor_method.from_node_id)
        .expect("decorated node");
    assert_eq!(decorated_node.name, "method");
}
