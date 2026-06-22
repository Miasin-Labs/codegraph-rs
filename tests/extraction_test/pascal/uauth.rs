use crate::extraction_test::fixture::*;

// --- Full fixture: UAuth.pas ---

const UAUTH_PAS: &str = "unit UAuth;

interface

uses
  System.SysUtils,
  System.Classes;

type
  ITokenValidator = interface
    ['{11111111-1111-1111-1111-111111111111}']
    function Validate(const AToken: string): Boolean;
  end;

  TAuthService = class(TInterfacedObject, ITokenValidator)
  private
    FToken: string;
    FLoginCount: Integer;
    procedure IncLoginCount;
  protected
    function GetToken: string;
  public
    constructor Create;
    destructor Destroy; override;
    function Validate(const AToken: string): Boolean;
    function Login(const AUser, APass: string): string;
    property Token: string read GetToken;
    property LoginCount: Integer read FLoginCount;
  end;

implementation

constructor TAuthService.Create;
begin
  inherited Create;
  FToken := '';
  FLoginCount := 0;
end;

destructor TAuthService.Destroy;
begin
  FToken := '';
  inherited Destroy;
end;

procedure TAuthService.IncLoginCount;
begin
  Inc(FLoginCount);
end;

function TAuthService.GetToken: string;
begin
  Result := FToken;
end;

function TAuthService.Validate(const AToken: string): Boolean;
begin
  Result := AToken <> '';
end;

function TAuthService.Login(const AUser, APass: string): string;
begin
  IncLoginCount;
  if Validate(AUser + ':' + APass) then
  begin
    FToken := AUser;
    Result := 'ok';
  end
  else
    Result := '';
end;

end.";

#[test]
fn pascal_uauth_fixture_extracts_all_expected_nodes() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    assert_eq!(result.errors.len(), 0);

    // Module
    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "UAuth");

    // Imports
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);

    // Interface
    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "ITokenValidator");

    // Class
    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TAuthService");

    // Methods
    let methods = filter_kind(&result, NodeKind::Method);
    assert!(methods.len() >= 6);
    let method_names = names(&methods);
    assert!(method_names.contains(&"Create".to_string()));
    assert!(method_names.contains(&"Destroy".to_string()));
    assert!(method_names.contains(&"Login".to_string()));

    // Fields
    let fields = filter_kind(&result, NodeKind::Field);
    assert_eq!(fields.len(), 2);
    assert!(
        fields
            .iter()
            .all(|f| f.visibility == Some(Visibility::Private))
    );

    // Properties
    let props = filter_kind(&result, NodeKind::Property);
    assert_eq!(props.len(), 2);
    let prop_names = names(&props);
    assert!(prop_names.contains(&"Token".to_string()));
    assert!(prop_names.contains(&"LoginCount".to_string()));
}

#[test]
fn pascal_uauth_fixture_extracts_inheritance_and_interface_implementation() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TInterfacedObject")
    );

    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(
        implements_refs.first().map(|r| r.reference_name.as_str()),
        Some("ITokenValidator")
    );
}

#[test]
fn pascal_uauth_fixture_extracts_calls_from_implementation() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    let call_names = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    assert!(call_names.contains(&"Inc".to_string()));
    assert!(call_names.contains(&"Validate".to_string()));
}
