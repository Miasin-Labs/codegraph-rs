use super::super::{Fixture, make_ref, node};
use crate::resolution::name_matcher::chains::match_call_chain;
use crate::types::{EdgeKind, Language, NodeKind};

fn callable(
    id: &str,
    name: &str,
    qualified_name: &str,
    language: Language,
    return_type: Option<&str>,
) -> crate::types::Node {
    let mut node = node(
        id,
        NodeKind::Method,
        name,
        qualified_name,
        "src/factory",
        language,
        1,
        2,
    );
    node.return_type = return_type.map(str::to_string);
    node
}

#[test]
fn cpp_factory_chain_uses_recorded_return_type() {
    let ctx = Fixture::new(vec![
        callable(
            "factory",
            "instance",
            "WidgetFactory::instance",
            Language::Cpp,
            Some("Widget"),
        ),
        callable("render", "render", "Widget::render", Language::Cpp, None),
    ]);
    let reference = make_ref(
        "WidgetFactory::instance().render",
        EdgeKind::Calls,
        1,
        "src/use.cpp",
        Language::Cpp,
    );
    assert_eq!(
        match_call_chain(&reference, &ctx).unwrap().target_node_id,
        "render"
    );
}

#[test]
fn dotted_factory_chain_validates_outer_method_on_return_type() {
    let ctx = Fixture::new(vec![
        callable(
            "create",
            "create",
            "Factory::create",
            Language::Java,
            Some("Worker"),
        ),
        callable("run", "run", "Worker::run", Language::Java, None),
        callable("decoy", "stop", "Other::stop", Language::Java, None),
    ]);
    let reference = make_ref(
        "Factory.create().run",
        EdgeKind::Calls,
        1,
        "src/Main.java",
        Language::Java,
    );
    assert_eq!(
        match_call_chain(&reference, &ctx).unwrap().target_node_id,
        "run"
    );

    let absent = make_ref(
        "Factory.create().stop",
        EdgeKind::Calls,
        1,
        "src/Main.java",
        Language::Java,
    );
    assert!(match_call_chain(&absent, &ctx).is_none());
}
