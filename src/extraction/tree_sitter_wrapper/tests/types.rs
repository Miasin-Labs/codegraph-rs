use super::fixture::extract_ts;
use crate::types::NodeKind;

#[test]
fn enum_members_are_extracted() {
    let source = "enum Color {\n  Red,\n  Green,\n}\n";
    let result = extract_ts("src/color.ts", source);

    let color = result
        .nodes
        .iter()
        .find(|n| n.name == "Color")
        .expect("enum node");
    assert_eq!(color.kind, NodeKind::Enum);
    let members: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumMember)
        .collect();
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|m| m.name == "Red"));
    assert!(members.iter().any(|m| m.name == "Green"));
    assert!(
        members
            .iter()
            .all(|m| m.qualified_name.starts_with("Color::"))
    );
}

#[test]
fn type_alias_members_surface_as_property_and_method() {
    let source = "type RecorderHandle = {\n  id: string;\n  stop: () => void;\n};\n";
    let result = extract_ts("src/recorder.ts", source);

    let alias = result
        .nodes
        .iter()
        .find(|n| n.name == "RecorderHandle")
        .expect("type alias");
    assert_eq!(alias.kind, NodeKind::TypeAlias);

    let id_member = result
        .nodes
        .iter()
        .find(|n| n.name == "id")
        .expect("id member");
    assert_eq!(id_member.kind, NodeKind::Property);
    assert_eq!(id_member.qualified_name, "RecorderHandle::id");

    let stop_member = result
        .nodes
        .iter()
        .find(|n| n.name == "stop")
        .expect("stop member");
    assert_eq!(stop_member.kind, NodeKind::Method);
}
