//! Port of `__tests__/lombok.test.ts` from the canonical TypeScript source.

use std::fs;
use std::path::Path;

use codegraph::{CodeGraph, EdgeKind, IndexOptions, Node, NodeKind};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, source: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, source).unwrap();
}

async fn index(root: &Path) -> CodeGraph {
    let graph = CodeGraph::init_sync(root).unwrap();
    graph.index_all(&IndexOptions::default()).await.unwrap();
    graph
}

fn lombok_node(graph: &CodeGraph, name: &str) -> Option<Node> {
    graph
        .get_nodes_by_name(name)
        .unwrap()
        .into_iter()
        .find(|node| {
            node.decorators
                .as_deref()
                .is_some_and(|decorators| decorators.iter().any(|item| item == "lombok"))
        })
}

#[tokio::test(flavor = "current_thread")]
async fn synthesizes_accessors_log_field_and_resolved_calls() {
    // Given
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "model/User.java",
        r#"package model;
import lombok.Data;
import lombok.Builder;
import lombok.extern.slf4j.Slf4j;

@Data
@Builder
@Slf4j
public class User {
    private String name;
    private boolean active;
    private static final int MAX = 10;
}
"#,
    );
    write(
        dir.path(),
        "svc/UserService.java",
        r#"package svc;
import model.User;

class UserService {
    String describe(User user) {
        user.setActive(true);
        return user.getName();
    }
    User make() {
        return User.builder();
    }
}
"#,
    );

    // When
    let graph = index(dir.path()).await;

    // Then
    for name in [
        "getName",
        "setName",
        "isActive",
        "setActive",
        "builder",
        "equals",
        "hashCode",
        "toString",
    ] {
        let node = lombok_node(&graph, name).unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(node.qualified_name, format!("model::User::{name}"));
    }
    let getter = lombok_node(&graph, "getName").unwrap();
    assert_eq!(getter.signature.as_deref(), Some("String getName()"));
    assert!(
        getter
            .docstring
            .as_deref()
            .is_some_and(|doc| doc.contains("Lombok-generated"))
    );
    assert_eq!(
        lombok_node(&graph, "isActive")
            .unwrap()
            .signature
            .as_deref(),
        Some("boolean isActive()")
    );
    assert!(
        lombok_node(&graph, "builder")
            .unwrap()
            .signature
            .as_deref()
            .is_some_and(|signature| signature.contains("static "))
    );
    assert_eq!(lombok_node(&graph, "log").unwrap().kind, NodeKind::Field);
    assert!(graph.get_nodes_by_name("getMAX").unwrap().is_empty());
    assert!(graph.get_nodes_by_name("getMax").unwrap().is_empty());

    for (target, caller) in [
        ("getName", "describe"),
        ("setActive", "describe"),
        ("builder", "make"),
    ] {
        let target = lombok_node(&graph, target).unwrap();
        let callers = graph.get_callers(&target.id, Some(1)).unwrap();
        assert!(callers.iter().any(|node_ref| {
            node_ref.node.name == caller && node_ref.edge.kind == EdgeKind::Calls
        }));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_accessor_is_not_overridden() {
    // Given
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "model/Account.java",
        r#"package model;
import lombok.Getter;

@Getter
public class Account {
    private int balance;
    private String owner;
    public int getBalance() { return balance < 0 ? 0 : balance; }
}
"#,
    );

    // When
    let graph = index(dir.path()).await;

    // Then
    let getters = graph.get_nodes_by_name("getBalance").unwrap();
    assert_eq!(getters.len(), 1);
    assert!(
        getters[0]
            .decorators
            .as_deref()
            .is_none_or(|decorators| !decorators.iter().any(|item| item == "lombok"))
    );
    assert!(lombok_node(&graph, "getOwner").is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn field_annotations_and_final_field_rules_are_preserved() {
    // Given
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "model/Box.java",
        r#"package model;
import lombok.Getter;
import lombok.Setter;

public class Box {
    @Getter @Setter private String label;
    @Getter private final long id;
    private int hidden;
}
"#,
    );

    // When
    let graph = index(dir.path()).await;

    // Then
    for name in ["getLabel", "setLabel", "getId"] {
        assert!(lombok_node(&graph, name).is_some(), "missing {name}");
    }
    assert!(graph.get_nodes_by_name("setId").unwrap().is_empty());
    assert!(graph.get_nodes_by_name("getHidden").unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn plain_class_has_no_lombok_members() {
    // Given
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "model/Plain.java",
        r#"package model;

public class Plain {
    private int value;
    public int getValue() { return value; }
    public void setValue(int v) { this.value = v; }
}
"#,
    );

    // When
    let graph = index(dir.path()).await;

    // Then
    assert!(
        graph
            .get_nodes_by_kind(NodeKind::Method)
            .unwrap()
            .into_iter()
            .chain(graph.get_nodes_by_kind(NodeKind::Field).unwrap())
            .all(|node| node
                .decorators
                .as_deref()
                .is_none_or(|decorators| !decorators.iter().any(|item| item == "lombok")))
    );
}
