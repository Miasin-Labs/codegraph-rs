use crate::extraction_test::fixture::*;

#[test]
fn type_alias_extracts_exported_type_aliases_in_typescript() {
    let code = r#"
export type AuthContextValue = {
  user: User | null;
  login: () => void;
  logout: () => void;
};
"#;
    let result = extract("types.ts", code);

    let type_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(type_node.name, "AuthContextValue");
    assert_eq!(type_node.is_exported, Some(true));
}

#[test]
fn type_alias_extracts_non_exported_type_aliases() {
    let code = r#"
type InternalState = {
  loading: boolean;
  error: string | null;
};
"#;
    let result = extract("internal.ts", code);

    let type_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(type_node.name, "InternalState");
    assert_eq!(type_node.is_exported, Some(false));
}

#[test]
fn type_alias_extracts_multiple_type_aliases_from_the_same_file() {
    let code = r#"
export type UnitSystem = 'metric' | 'imperial';
export type DateFormat = 'ISO' | 'US' | 'EU';
type Internal = string;
"#;
    let result = extract("config.ts", code);

    let type_aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert_eq!(type_aliases.len(), 3);

    let exported: Vec<&&Node> = type_aliases
        .iter()
        .filter(|n| n.is_exported == Some(true))
        .collect();
    assert_eq!(exported.len(), 2);
    let mut exported_names: Vec<String> = exported.iter().map(|n| n.name.clone()).collect();
    exported_names.sort();
    assert_eq!(exported_names, vec!["DateFormat", "UnitSystem"]);
}
