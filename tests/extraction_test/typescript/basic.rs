use crate::extraction_test::fixture::*;

// =============================================================================
// describe('TypeScript Extraction')
// =============================================================================

#[test]
fn typescript_extracts_function_declarations() {
    let code = r#"
export function processPayment(amount: number): Promise<Receipt> {
  return stripe.charge(amount);
}
"#;
    let result = extract("payment.ts", code);

    // File node + function node
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.name, "payment.ts");

    let func_node = find_kind(&result, NodeKind::Function).expect("function node");
    assert_eq!(func_node.name, "processPayment");
    assert_eq!(func_node.language, Language::Typescript);
    assert_eq!(func_node.is_exported, Some(true));
    assert!(
        func_node
            .signature
            .as_deref()
            .unwrap_or_default()
            .contains("amount: number")
    );
}

#[test]
fn typescript_extracts_class_declarations() {
    let code = r#"
export class PaymentService {
  private stripe: StripeClient;

  constructor(apiKey: string) {
    this.stripe = new StripeClient(apiKey);
  }

  async charge(amount: number): Promise<Receipt> {
    return this.stripe.charge(amount);
  }
}
"#;
    let result = extract("service.ts", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class node");
    let method_nodes = filter_kind(&result, NodeKind::Method);

    assert_eq!(class_node.name, "PaymentService");
    assert_eq!(class_node.is_exported, Some(true));

    assert!(!method_nodes.is_empty());
    assert!(method_nodes.iter().any(|m| m.name == "charge"));
}

#[test]
fn typescript_extracts_interfaces() {
    let code = r#"
export interface User {
  id: string;
  name: string;
  email: string;
}
"#;
    let result = extract("types.ts", code);

    assert!(find_kind(&result, NodeKind::File).is_some());

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface node");
    assert_eq!(iface_node.name, "User");
    assert_eq!(iface_node.is_exported, Some(true));
}

#[test]
fn typescript_extracts_type_refs_from_interface_property_signatures() {
    let code = r#"
import type { IPage } from '../PromoterList';
import type { IOrderField } from '../types';

interface Hprops {
  value?: Partial<IPage> & Partial<IOrderField>;
}
"#;
    let result = extract("HeaderFilter.ts", code);

    let refs = refs_of_kind(&result, EdgeKind::References);
    assert!(refs.iter().any(|r| r.reference_name == "IPage"));
    assert!(refs.iter().any(|r| r.reference_name == "IOrderField"));
}

#[test]
fn typescript_extracts_type_refs_from_interface_method_signatures() {
    let code = r#"
import type { IPage } from '../PromoterList';
import type { IOrderField } from '../types';

interface MethodForm {
  fetchPage(arg: IPage): IOrderField;
}
"#;
    let result = extract("MethodForm.ts", code);

    let refs = refs_of_kind(&result, EdgeKind::References);
    assert!(refs.iter().any(|r| r.reference_name == "IPage"));
    assert!(refs.iter().any(|r| r.reference_name == "IOrderField"));
}

#[test]
fn typescript_tracks_function_calls() {
    let code = r#"
function main() {
  const result = processData();
  console.log(result);
}
"#;
    let result = extract("main.ts", code);

    assert!(!result.unresolved_references.is_empty());
    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(calls.iter().any(|c| c.reference_name == "processData"));
}
