use crate::extraction_test::fixture::*;

#[test]
fn scala_extracts_import_declarations() {
    let code = "
import scala.collection.mutable.ListBuffer
import scala.concurrent.Future
";
    let result = extract("imports.scala", code);
    let imports = import_nodes(&result);
    assert!(imports.len() >= 2);
}

#[test]
fn scala_extracts_private_visibility() {
    let code = "
class Service {
  private val secret: String = \"abc\"
  private def helper(): Unit = {}
}
";
    let result = extract("Service.scala", code);
    let secret_field = result
        .nodes
        .iter()
        .find(|n| n.name == "secret")
        .expect("secret");
    assert_eq!(secret_field.visibility, Some(Visibility::Private));
    let helper_method = result
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper");
    assert_eq!(helper_method.visibility, Some(Visibility::Private));
}

#[test]
fn scala_extracts_protected_visibility() {
    let code = "
class Base {
  protected def helperMethod(): Unit = {}
}
";
    let result = extract("Base.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "helperMethod")
        .expect("helperMethod");
    assert_eq!(method.visibility, Some(Visibility::Protected));
}

#[test]
fn scala_defaults_to_public_visibility() {
    let code = "
class Greeter {
  def hello(): Unit = {}
}
";
    let result = extract("Greeter.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "hello")
        .expect("hello");
    assert_eq!(method.visibility, Some(Visibility::Public));
}

#[test]
fn scala_extracts_extends_relationships() {
    let code = "
class AdminUser extends User {
  def adminAction(): Unit = {}
}
";
    let result = extract("AdminUser.scala", code);
    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert!(extends_refs.iter().any(|r| r.reference_name == "User"));
}

#[test]
fn scala_extracts_function_call_expressions() {
    let code = "
def processData(): Unit = {
  val result = computeResult()
  println(result)
}
";
    let result = extract("processor.scala", code);
    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(!calls.is_empty());
}

// =============================================================================
