use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Pascal / Delphi Extraction')
// =============================================================================

#[test]
fn pascal_detects_pascal_files() {
    assert_eq!(detect_language("UAuth.pas", None), Language::Pascal);
    assert_eq!(detect_language("App.dpr", None), Language::Pascal);
    assert_eq!(detect_language("Package.dpk", None), Language::Pascal);
    assert_eq!(detect_language("App.lpr", None), Language::Pascal);
    assert_eq!(detect_language("MainForm.dfm", None), Language::Pascal);
    assert_eq!(detect_language("MainForm.fmx", None), Language::Pascal);
}

#[test]
fn pascal_is_reported_as_supported() {
    assert!(is_language_supported(Language::Pascal));
    assert!(get_supported_languages().contains(&Language::Pascal));
}

#[test]
fn pascal_extracts_unit_as_module() {
    let code = "unit MyUnit;\ninterface\nimplementation\nend.";
    let result = extract("MyUnit.pas", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "MyUnit");
    assert_eq!(module_node.language, Language::Pascal);
}

#[test]
fn pascal_extracts_program_as_module() {
    let code = "program MyApp;\nbegin\nend.";
    let result = extract("MyApp.dpr", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "MyApp");
}

#[test]
fn pascal_falls_back_to_filename_when_module_name_is_empty() {
    // Some .dpr templates use "program;" without a name
    let code = "program;\nuses SysUtils;\nbegin\nend.";
    let result = extract("Console.dpr", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "Console");
}

#[test]
fn pascal_extracts_uses_as_individual_imports() {
    let code =
        "unit Test;\ninterface\nuses\n  System.SysUtils,\n  System.Classes;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"System.SysUtils".to_string()));
    assert!(import_names.contains(&"System.Classes".to_string()));
}

#[test]
fn pascal_creates_unresolved_references_for_imports() {
    let code = "unit Test;\ninterface\nuses\n  UAuth;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let import_ref = refs_of_kind(&result, EdgeKind::Imports);
    assert_eq!(
        import_ref.first().map(|r| r.reference_name.as_str()),
        Some("UAuth")
    );
}

#[test]
fn pascal_extracts_class_declarations() {
    let code = "unit Test;\ninterface\ntype\n  TMyClass = class\n  public\n    procedure DoSomething;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TMyClass");
}

#[test]
fn pascal_extracts_class_with_inheritance() {
    let code =
        "unit Test;\ninterface\ntype\n  TChild = class(TParent)\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TParent")
    );
}

#[test]
fn pascal_extracts_class_with_interface_implementation() {
    let code = "unit Test;\ninterface\ntype\n  TService = class(TInterfacedObject, ILogger)\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TInterfacedObject")
    );
    assert_eq!(
        implements_refs.first().map(|r| r.reference_name.as_str()),
        Some("ILogger")
    );
}

#[test]
fn pascal_extracts_records_as_class_nodes() {
    let code = "unit Test;\ninterface\ntype\n  TPoint = record\n    X: Double;\n    Y: Double;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TPoint");

    let fields = filter_kind(&result, NodeKind::Field);
    assert_eq!(fields.len(), 2);
    let field_names = names(&fields);
    assert!(field_names.contains(&"X".to_string()));
    assert!(field_names.contains(&"Y".to_string()));
}

#[test]
fn pascal_extracts_interface_declarations() {
    let code = "unit Test;\ninterface\ntype\n  ILogger = interface\n    procedure Log(const AMsg: string);\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "ILogger");
}

#[test]
fn pascal_extracts_methods_with_visibility() {
    let code = "unit Test;\ninterface\ntype\n  TMyClass = class\n  private\n    FValue: Integer;\n  public\n    constructor Create;\n    function GetValue: Integer;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let methods = filter_kind(&result, NodeKind::Method);
    assert_eq!(methods.len(), 2);

    let create_method = methods.iter().find(|m| m.name == "Create").expect("Create");
    assert_eq!(create_method.visibility, Some(Visibility::Public));

    let get_value = methods
        .iter()
        .find(|m| m.name == "GetValue")
        .expect("GetValue");
    assert_eq!(get_value.visibility, Some(Visibility::Public));

    let fields = filter_kind(&result, NodeKind::Field);
    let f_value = fields.iter().find(|f| f.name == "FValue").expect("FValue");
    assert_eq!(f_value.visibility, Some(Visibility::Private));
}

#[test]
fn pascal_detects_static_methods_class_methods() {
    let code = "unit Test;\ninterface\ntype\n  THelper = class\n  public\n    class function Create: THelper; static;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let methods = filter_kind(&result, NodeKind::Method);
    let static_method = methods.iter().find(|m| m.name == "Create").expect("Create");
    assert_eq!(static_method.is_static, Some(true));
}

#[test]
fn pascal_extracts_enums_with_members() {
    let code =
        "unit Test;\ninterface\ntype\n  TColor = (clRed, clGreen, clBlue);\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "TColor");

    let members = filter_kind(&result, NodeKind::EnumMember);
    assert_eq!(members.len(), 3);
    assert_eq!(names(&members), vec!["clRed", "clGreen", "clBlue"]);
}

#[test]
fn pascal_extracts_properties() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    property Name: string read FName write FName;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let prop_node = find_kind(&result, NodeKind::Property).expect("property");
    assert_eq!(prop_node.name, "Name");
    assert_eq!(prop_node.visibility, Some(Visibility::Public));
}

#[test]
fn pascal_extracts_constants() {
    let code = "unit Test;\ninterface\nconst\n  MAX_RETRIES = 3;\n  APP_NAME = 'MyApp';\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let constants = filter_kind(&result, NodeKind::Constant);
    assert_eq!(constants.len(), 2);
    let constant_names = names(&constants);
    assert!(constant_names.contains(&"MAX_RETRIES".to_string()));
    assert!(constant_names.contains(&"APP_NAME".to_string()));
}

#[test]
fn pascal_extracts_type_aliases() {
    let code = "unit Test;\ninterface\ntype\n  TUserName = string;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let alias_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(alias_node.name, "TUserName");
}

#[test]
fn pascal_extracts_calls_from_implementation_bodies() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    procedure DoWork;\n  end;\nimplementation\nprocedure TObj.DoWork;\nbegin\n  WriteLn('hello');\nend;\nend.";
    let result = extract("Test.pas", code);

    let call_refs = refs_of_kind(&result, EdgeKind::Calls);
    assert_eq!(
        call_refs.first().map(|r| r.reference_name.as_str()),
        Some("WriteLn")
    );
}

#[test]
fn pascal_creates_contains_edges_for_class_members() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    procedure Foo;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    let method_node = find_kind(&result, NodeKind::Method).expect("method");

    let contains_edge = result.edges.iter().find(|e| {
        e.source == class_node.id && e.target == method_node.id && e.kind == EdgeKind::Contains
    });
    assert!(contains_edge.is_some());
}
