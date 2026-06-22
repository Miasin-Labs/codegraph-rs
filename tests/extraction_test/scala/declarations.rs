use crate::extraction_test::fixture::*;

// describe('Scala Extraction')
// =============================================================================

#[test]
fn scala_detects_scala_files() {
    assert_eq!(detect_language("Main.scala", None), Language::Scala);
    assert_eq!(detect_language("script.sc", None), Language::Scala);
    assert_eq!(
        detect_language("src/UserService.scala", None),
        Language::Scala
    );
}

#[test]
fn scala_is_reported_as_supported() {
    assert!(is_language_supported(Language::Scala));
    assert!(get_supported_languages().contains(&Language::Scala));
}

#[test]
fn scala_extracts_class_definitions() {
    let code = "
class UserService(private val repo: UserRepository) {
  def findUser(id: String): Option[String] = Some(id)
}
";
    let result = extract("UserService.scala", code);
    let cls = find_named(&result, NodeKind::Class, "UserService").expect("UserService");
    assert_eq!(cls.language, Language::Scala);
}

#[test]
fn scala_extracts_object_definitions_as_class_kind() {
    let code = "
object DatabaseConfig {
  val url = \"jdbc:postgresql://localhost/mydb\"
}
";
    let result = extract("Config.scala", code);
    assert!(find_named(&result, NodeKind::Class, "DatabaseConfig").is_some());
}

#[test]
fn scala_extracts_trait_definitions_as_trait_kind() {
    let code = "
trait Repository[A] {
  def findById(id: String): Option[A]
  def save(entity: A): Unit
}
";
    let result = extract("Repository.scala", code);
    assert!(find_named(&result, NodeKind::Trait, "Repository").is_some());
}

#[test]
fn scala_extracts_method_definitions_inside_a_class() {
    let code = "
class Calculator {
  def add(a: Int, b: Int): Int = a + b
  def divide(a: Double, b: Double): Double = a / b
}
";
    let result = extract("Calculator.scala", code);
    let methods = filter_kind(&result, NodeKind::Method);
    assert!(methods.iter().any(|m| m.name == "add"));
    assert!(methods.iter().any(|m| m.name == "divide"));
}

#[test]
fn scala_extracts_method_signatures() {
    let code = "
class Greeter {
  def greet(name: String): String = s\"Hello, ${name}!\"
}
";
    let result = extract("Greeter.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "greet")
        .expect("greet");
    let signature = method.signature.as_deref().unwrap_or_default();
    assert!(signature.contains("name: String"));
    assert!(signature.contains("String"));
}

#[test]
fn scala_extracts_top_level_function_definitions_as_functions() {
    let code = "
def factorial(n: Int): Int = if (n <= 1) 1 else n * factorial(n - 1)
def greet(name: String): String = s\"Hello, ${name}!\"
";
    let result = extract("utils.scala", code);
    let fns = filter_kind(&result, NodeKind::Function);
    assert!(fns.iter().any(|f| f.name == "factorial"));
    assert!(fns.iter().any(|f| f.name == "greet"));
}

#[test]
fn scala_extracts_val_inside_a_class_as_field() {
    let code = "
class Config {
  val timeout: Int = 30
  val host: String = \"localhost\"
}
";
    let result = extract("Config.scala", code);
    let fields = filter_kind(&result, NodeKind::Field);
    assert!(fields.iter().any(|f| f.name == "timeout"));
    assert!(fields.iter().any(|f| f.name == "host"));
}

#[test]
fn scala_extracts_var_inside_a_class_as_field() {
    let code = "
class Counter {
  var count: Int = 0
}
";
    let result = extract("Counter.scala", code);
    assert!(find_named(&result, NodeKind::Field, "count").is_some());
}

#[test]
fn scala_extracts_top_level_val_as_constant() {
    let code = "
val MaxConnections: Int = 100
val DefaultTimeout = 30
";
    let result = extract("constants.scala", code);
    let consts = filter_kind(&result, NodeKind::Constant);
    assert!(consts.iter().any(|c| c.name == "MaxConnections"));
}

#[test]
fn scala_extracts_top_level_var_as_variable() {
    let code = "
var retries: Int = 3
";
    let result = extract("state.scala", code);
    assert!(find_named(&result, NodeKind::Variable, "retries").is_some());
}

#[test]
fn scala_includes_type_in_val_var_signature() {
    let code = "
class Service {
  val timeout: Int = 30
}
";
    let result = extract("Service.scala", code);
    let field = result
        .nodes
        .iter()
        .find(|n| n.name == "timeout")
        .expect("timeout");
    let signature = field.signature.as_deref().unwrap_or_default();
    assert!(signature.contains("timeout"));
    assert!(signature.contains("Int"));
}

#[test]
fn scala_extracts_enum_definitions() {
    let code = "
enum Color:
  case Red
  case Green
  case Blue
";
    let result = extract("Color.scala", code);
    assert!(find_named(&result, NodeKind::Enum, "Color").is_some());
}

#[test]
fn scala_extracts_enum_cases_as_enum_member() {
    let code = "
enum Direction:
  case North
  case South
  case East
  case West
";
    let result = extract("Direction.scala", code);
    let members = filter_kind(&result, NodeKind::EnumMember);
    assert!(members.iter().any(|m| m.name == "North"));
    assert!(members.iter().any(|m| m.name == "South"));
    assert!(members.len() >= 4);
}

#[test]
fn scala_extracts_type_aliases() {
    let code = "
type UserId = String
type UserMap = Map[String, String]
";
    let result = extract("types.scala", code);
    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert!(aliases.iter().any(|a| a.name == "UserId"));
    assert!(aliases.iter().any(|a| a.name == "UserMap"));
}
