use crate::extraction_test::fixture::*;

// describe('Kotlin Extraction')
// =============================================================================

#[test]
fn kotlin_extracts_class_declarations() {
    let code = r#"
class UserRepository(private val database: Database) {
    fun findById(id: String): User? {
        return database.query("SELECT * FROM users WHERE id = ?", id)
    }

    suspend fun save(user: User) {
        database.insert(user)
    }
}
"#;
    let result = extract("UserRepository.kt", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserRepository");
}

#[test]
fn kotlin_extracts_function_declarations() {
    let code = r#"
fun calculateTotal(items: List<Item>): Double {
    return items.sumOf { it.price }
}

suspend fun fetchUserData(userId: String): User {
    return api.getUser(userId)
}
"#;
    let result = extract("utils.kt", code);

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(!functions.is_empty());
}

#[test]
fn kotlin_detects_suspend_functions_as_async() {
    let code = r#"
suspend fun loadData(): List<String> {
    delay(1000)
    return listOf("a", "b", "c")
}
"#;
    let result = extract("loader.kt", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.is_async, Some(true));
}

#[test]
fn kotlin_extracts_fun_interface_declarations() {
    let code = r#"
fun interface OnObjectRetainedListener {
  fun onObjectRetained()
}
"#;
    let result = extract("listener.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "OnObjectRetainedListener");

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "onObjectRetained");
    assert_eq!(
        method_node.qualified_name,
        "OnObjectRetainedListener::onObjectRetained"
    );
}

#[test]
fn kotlin_extracts_complex_fun_interface_with_nested_classes() {
    let code = r#"
fun interface EventListener {
  fun onEvent(event: Event)

  sealed class Event {
    class DumpingHeap : Event()
  }
}
"#;
    let result = extract("events.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "EventListener");

    // Nested sealed class should still be extracted (as sibling due to grammar limitations)
    assert!(find_named(&result, NodeKind::Class, "Event").is_some());
    assert!(find_named(&result, NodeKind::Class, "DumpingHeap").is_some());
}

#[test]
fn kotlin_does_not_affect_regular_function_declarations() {
    let code = r#"
fun interface MyCallback {
  fun invoke(value: Int)
}

fun regularFunction(): String {
  return "hello"
}
"#;
    let result = extract("mixed.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "MyCallback");

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "regularFunction");
}

#[test]
fn kotlin_extracts_fun_interface_with_annotation_on_method_pattern_2b() {
    // When the SAM method has annotations like @Throws, tree-sitter produces a different
    // misparse: function_declaration > ERROR("interface Name {") instead of
    // function_declaration > user_type("interface"). This is the OkHttp Interceptor pattern.
    let code = r#"
import java.io.IOException

fun interface Interceptor {
  @Throws(IOException::class)
  fun intercept(chain: Chain): Response
}
"#;
    let result = extract("interceptor.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "Interceptor");
}

#[test]
fn kotlin_extracts_methods_from_interface_with_nested_fun_interface() {
    // When an interface contains a nested `fun interface`, tree-sitter misparsed
    // the parent body as ERROR. Methods inside should still be extracted.
    let code = r#"
interface WebSocket {
  fun request(): Request
  fun send(text: String): Boolean
  fun cancel()
  fun interface Factory {
    fun newWebSocket(request: Request): WebSocket
  }
}
"#;
    let result = extract("websocket.kt", code);

    assert!(find_named(&result, NodeKind::Interface, "WebSocket").is_some());

    let method_names: Vec<String> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method && n.qualified_name.starts_with("WebSocket::"))
        .map(|n| n.name.clone())
        .collect();
    assert!(method_names.contains(&"request".to_string()));
    assert!(method_names.contains(&"send".to_string()));
    assert!(method_names.contains(&"cancel".to_string()));
}

#[test]
fn kotlin_wraps_top_level_declarations_in_a_namespace_from_package_header() {
    let code = r#"
package com.example.foo

class Bar {
  fun greet(): String = "hi"
}

fun util(): Int = 42
"#;
    let result = extract("Bar.kt", code);

    let ns = find_kind(&result, NodeKind::Namespace).expect("namespace");
    assert_eq!(ns.name, "com.example.foo");

    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "com.example.foo::Bar");

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    assert_eq!(greet.qualified_name, "com.example.foo::Bar::greet");

    let util = find_named(&result, NodeKind::Function, "util").expect("util");
    assert_eq!(util.qualified_name, "com.example.foo::util");
}

#[test]
fn kotlin_handles_a_single_segment_package() {
    let code = r#"
package foo

class Bar
"#;
    let result = extract("Bar.kt", code);
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "foo::Bar");
}

#[test]
fn kotlin_does_not_wrap_when_no_package_is_declared() {
    let code = r#"
class Bar {
  fun greet() = "hi"
}
"#;
    let result = extract("Bar.kt", code);
    assert!(find_kind(&result, NodeKind::Namespace).is_none());
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "Bar");
}

// =============================================================================
