use crate::extraction_test::fixture::*;

// --- Full fixture: UTypes.pas ---

const UTYPES_PAS: &str = "unit UTypes;

interface

uses
  System.SysUtils;

const
  C_MAX_RETRIES = 3;
  C_DEFAULT_NAME = 'Guest';

type
  TUserRole = (urAdmin, urEditor, urViewer);

  TPoint2D = record
    X: Double;
    Y: Double;
  end;

  TUserName = string;

  TUserInfo = class
  public
    type
      TAddress = record
        Street: string;
        City: string;
        Zip: string;
      end;
  private
    FName: TUserName;
    FRole: TUserRole;
    FAddress: TAddress;
  public
    constructor Create(const AName: TUserName; ARole: TUserRole);
    function GetDisplayName: string;
    class function CreateAdmin(const AName: TUserName): TUserInfo; static;
    property Name: TUserName read FName write FName;
    property Role: TUserRole read FRole;
    property Address: TAddress read FAddress write FAddress;
  end;

implementation

constructor TUserInfo.Create(const AName: TUserName; ARole: TUserRole);
begin
  FName := AName;
  FRole := ARole;
end;

function TUserInfo.GetDisplayName: string;
begin
  if FRole = urAdmin then
    Result := '[Admin] ' + FName
  else
    Result := FName;
end;

class function TUserInfo.CreateAdmin(const AName: TUserName): TUserInfo;
begin
  Result := TUserInfo.Create(AName, urAdmin);
end;

end.";

#[test]
fn pascal_utypes_fixture_extracts_enums_with_members() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "TUserRole");

    let members = filter_kind(&result, NodeKind::EnumMember);
    assert_eq!(members.len(), 3);
    assert_eq!(names(&members), vec!["urAdmin", "urEditor", "urViewer"]);
}

#[test]
fn pascal_utypes_fixture_extracts_constants() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let constants = filter_kind(&result, NodeKind::Constant);
    assert_eq!(constants.len(), 2);
    let constant_names = names(&constants);
    assert!(constant_names.contains(&"C_MAX_RETRIES".to_string()));
    assert!(constant_names.contains(&"C_DEFAULT_NAME".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_type_aliases() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert!(names(&aliases).contains(&"TUserName".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_records_as_classes_with_fields() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let classes = filter_kind(&result, NodeKind::Class);
    assert!(names(&classes).contains(&"TPoint2D".to_string()));

    // TPoint2D fields
    let fields = filter_kind(&result, NodeKind::Field);
    let field_names = names(&fields);
    assert!(field_names.contains(&"X".to_string()));
    assert!(field_names.contains(&"Y".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_static_class_methods() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let methods = filter_kind(&result, NodeKind::Method);
    let static_method = methods
        .iter()
        .find(|m| m.name == "CreateAdmin")
        .expect("CreateAdmin");
    assert_eq!(static_method.is_static, Some(true));
}

#[test]
fn pascal_utypes_fixture_extracts_nested_types() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let classes = filter_kind(&result, NodeKind::Class);
    assert!(names(&classes).contains(&"TAddress".to_string()));
}

// =============================================================================
