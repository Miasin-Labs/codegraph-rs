use crate::extraction_test::fixture::*;

// describe('Dart Extraction')
// =============================================================================

#[test]
fn dart_extracts_class_declarations() {
    let code = r#"
class UserService {
  final Database _db;

  Future<User> findById(String id) async {
    return await _db.query(id);
  }

  void _privateMethod() {}
}
"#;
    let result = extract("service.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));

    let method_nodes = filter_kind(&result, NodeKind::Method);
    assert!(method_nodes.len() >= 2);

    let find_by_id = method_nodes
        .iter()
        .find(|m| m.name == "findById")
        .expect("findById");
    assert_eq!(find_by_id.is_async, Some(true));

    let private_method = method_nodes
        .iter()
        .find(|m| m.name == "_privateMethod")
        .expect("_privateMethod");
    assert_eq!(private_method.visibility, Some(Visibility::Private));

    // Dart models a method body as a SIBLING of the signature, so the method
    // node must be extended to span its body (not just the signature line) —
    // required for body-level analysis (callees, the callback synthesizer).
    assert!(find_by_id.end_line > find_by_id.start_line);
}

#[test]
fn dart_extracts_top_level_function_declarations() {
    let code = r#"
void topLevelFunction(String name) {
  print(name);
}
"#;
    let result = extract("utils.dart", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "topLevelFunction");
    assert_eq!(func_node.language, Language::Dart);
}

#[test]
fn dart_extracts_enum_declarations() {
    let code = r#"
enum Status { active, inactive, pending }
"#;
    let result = extract("models.dart", code);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "Status");
}

#[test]
fn dart_extracts_mixin_declarations() {
    let code = r#"
mixin LoggerMixin {
  void log(String message) {}
}
"#;
    let result = extract("mixins.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "LoggerMixin");

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "log");
}

#[test]
fn dart_extracts_extension_declarations() {
    let code = r#"
extension StringExt on String {
  bool get isBlank => trim().isEmpty;
}
"#;
    let result = extract("extensions.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "StringExt");
}

#[test]
fn dart_detects_static_methods() {
    let code = r#"
class Utils {
  static void doWork() {}
}
"#;
    let result = extract("utils.dart", code);

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "doWork");
    assert_eq!(method_node.is_static, Some(true));
}

#[test]
fn dart_detects_async_functions() {
    let code = r#"
Future<String> fetchData() async {
  return await http.get('/data');
}
"#;
    let result = extract("api.dart", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "fetchData");
    assert_eq!(func_node.is_async, Some(true));
}

#[test]
fn dart_detects_private_visibility_via_underscore_convention() {
    let code = r#"
void _privateHelper() {}

void publicFunction() {}
"#;
    let result = extract("helpers.dart", code);

    let functions = filter_kind(&result, NodeKind::Function);
    let private_func = functions.iter().find(|f| f.name == "_privateHelper");
    let public_func = functions.iter().find(|f| f.name == "publicFunction");

    assert_eq!(
        private_func.and_then(|f| f.visibility),
        Some(Visibility::Private)
    );
    assert_eq!(
        public_func.and_then(|f| f.visibility),
        Some(Visibility::Public)
    );
}
