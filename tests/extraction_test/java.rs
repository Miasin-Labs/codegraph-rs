use crate::extraction_test::fixture::*;

// describe('Java Extraction')
// =============================================================================

#[test]
fn java_extracts_class_declarations() {
    let code = r#"
public class UserService {
    private final UserRepository repository;

    public UserService(UserRepository repository) {
        this.repository = repository;
    }

    public User getUser(String id) {
        return repository.findById(id);
    }
}
"#;
    let result = extract("UserService.java", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));
}

#[test]
fn java_extracts_method_declarations() {
    let code = r#"
public class Calculator {
    public static int add(int a, int b) {
        return a + b;
    }
}
"#;
    let result = extract("Calculator.java", code);

    let method_node = find_named(&result, NodeKind::Method, "add").expect("add method");
    assert_eq!(method_node.is_static, Some(true));
}

#[test]
fn java_wraps_top_level_declarations_in_a_namespace_from_package_declaration() {
    let code = r#"
package com.example.foo;

public class Bar {
    public String greet() { return "hi"; }
}
"#;
    let result = extract("Bar.java", code);

    let ns = find_kind(&result, NodeKind::Namespace).expect("namespace");
    assert_eq!(ns.name, "com.example.foo");

    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "com.example.foo::Bar");

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    assert_eq!(greet.qualified_name, "com.example.foo::Bar::greet");
}

#[test]
fn java_does_not_wrap_when_no_package_is_declared() {
    let code = r#"
public class Bar {
    public String greet() { return "hi"; }
}
"#;
    let result = extract("Bar.java", code);
    assert!(find_kind(&result, NodeKind::Namespace).is_none());
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "Bar");
}

#[test]
fn java_extracts_anonymous_class_overrides_from_new_t() {
    // The pattern that breaks the trace through `strategy.foo()` in
    // libraries like guava's Splitter: the lambda-returned anonymous
    // class overrides abstract methods on the base, but without
    // extracting those overrides the interface→impl synthesizer has
    // nothing to bridge.
    let code = r#"
package com.example;

abstract class Base {
  abstract int compute(int x);
}

public class Factory {
  public Base make() {
    return new Base() {
      @Override
      int compute(int x) { return x + 1; }
    };
  }
}
"#;
    let result = extract("Factory.java", code);

    let anon = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name.contains("Base$anon@"))
        .expect("anonymous Base subclass should be extracted as a class");

    let compute = result
        .nodes
        .iter()
        .find(|n| {
            n.kind == NodeKind::Method && n.name == "compute" && n.qualified_name.contains("$anon@")
        })
        .expect("override method should be a method on the anon class");
    assert!(
        compute
            .qualified_name
            .contains("Factory::make::<Base$anon@")
    );
    assert!(compute.qualified_name.ends_with("::compute"));

    // Anon class must extend Base so Phase 5.5 (interface-impl) can bridge.
    let extends_ref = result.unresolved_references.iter().find(|r| {
        r.reference_kind == EdgeKind::Extends
            && r.reference_name == "Base"
            && r.from_node_id == anon.id
    });
    assert!(
        extends_ref.is_some(),
        "anon class should carry an `extends Base` reference"
    );

    // The enclosing `make` method still emits an instantiates edge to Base —
    // anon extraction must not swallow that signal.
    let instantiates_ref = find_ref(&result, EdgeKind::Instantiates, "Base");
    assert!(
        instantiates_ref.is_some(),
        "enclosing method should still instantiate Base"
    );
}

#[test]
fn java_extracts_anonymous_class_overrides_inside_a_lambda_body() {
    // The exact guava pattern: a lambda is passed to a constructor, and the
    // lambda body returns `new T() { @Override ... }`. The anon class must
    // still surface even though it sits inside a lambda_expression node.
    let code = r#"
package com.example;

interface Strategy {
  java.util.Iterator<String> iterator(String s);
}

abstract class BaseIter implements java.util.Iterator<String> {
  abstract int separatorStart(int start);
}

public class Splitter {
  private final Strategy strategy;
  public Splitter(Strategy s) { this.strategy = s; }

  public static Splitter on(char c) {
    return new Splitter((seq) ->
        new BaseIter() {
          @Override
          int separatorStart(int start) { return start + 1; }
          @Override public boolean hasNext() { return false; }
          @Override public String next() { return null; }
        });
  }
}
"#;
    let result = extract("Splitter.java", code);

    let anon = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name.contains("BaseIter$anon@"));
    assert!(
        anon.is_some(),
        "anon BaseIter inside the lambda body should be extracted"
    );

    let sep_start = result.nodes.iter().find(|n| {
        n.kind == NodeKind::Method
            && n.name == "separatorStart"
            && n.qualified_name.contains("$anon@")
    });
    assert!(
        sep_start.is_some(),
        "override inside the lambda-returned anon class should be a method node"
    );
}

// =============================================================================
