use crate::extraction_test::fixture::*;

// describe('Swift Extraction')
// =============================================================================

#[test]
fn swift_extracts_class_declarations() {
    let code = r#"
public class NetworkManager {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func fetchData(from url: URL) async throws -> Data {
        let (data, _) = try await session.data(from: url)
        return data
    }
}
"#;
    let result = extract("NetworkManager.swift", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "NetworkManager");
}

#[test]
fn swift_extracts_function_declarations() {
    let code = r#"
func calculateSum(_ numbers: [Int]) -> Int {
    return numbers.reduce(0, +)
}

public func formatCurrency(amount: Double) -> String {
    return String(format: "$%.2f", amount)
}
"#;
    let result = extract("utils.swift", code);

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(!functions.is_empty());
}

#[test]
fn swift_extracts_struct_declarations() {
    let code = r#"
public struct User {
    let id: UUID
    var name: String
    var email: String

    func displayName() -> String {
        return name
    }
}
"#;
    let result = extract("User.swift", code);

    let struct_node = find_kind(&result, NodeKind::Struct).expect("struct");
    assert_eq!(struct_node.name, "User");
}

#[test]
fn swift_extracts_protocol_declarations() {
    let code = r#"
public protocol Repository {
    associatedtype Entity

    func find(id: String) async throws -> Entity?
    func save(_ entity: Entity) async throws
}
"#;
    let result = extract("Repository.swift", code);

    let protocol_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(protocol_node.name, "Repository");
}

#[test]
fn swift_extracts_class_inheritance_and_protocol_conformance() {
    let code = r#"
class DataRequest: Request {
    func validate() {}
}

class UploadRequest: DataRequest, Sendable {
    func upload() {}
}

enum AFError: Error {
    case invalidURL
}

struct HTTPMethod: RawRepresentable {
    let rawValue: String
}

protocol UploadConvertible: URLRequestConvertible {
    func asURLRequest() throws -> URLRequest
}
"#;
    let result = extract("Inheritance.swift", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    let extends_names = ref_names(&extends_refs);

    // DataRequest extends Request
    assert!(extends_names.contains(&"Request".to_string()));
    // UploadRequest extends DataRequest and Sendable
    assert!(extends_names.contains(&"DataRequest".to_string()));
    assert!(extends_names.contains(&"Sendable".to_string()));
    // AFError extends Error
    assert!(extends_names.contains(&"Error".to_string()));
    // HTTPMethod extends RawRepresentable
    assert!(extends_names.contains(&"RawRepresentable".to_string()));
    // UploadConvertible extends URLRequestConvertible
    assert!(extends_names.contains(&"URLRequestConvertible".to_string()));
}

// =============================================================================
