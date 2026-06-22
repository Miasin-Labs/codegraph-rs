use crate::extraction_test::fixture::*;

// describe('DFM/FMX Extraction')
// =============================================================================

#[test]
fn dfm_extracts_components() {
    let code = "object Form1: TForm1
  Left = 0
  Top = 0
  Caption = 'My Form'
  object Button1: TButton
    Left = 10
    Top = 10
    Caption = 'Click Me'
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
    let component_names = names(&components);
    assert!(component_names.contains(&"Form1".to_string()));
    assert!(component_names.contains(&"Button1".to_string()));

    let button = components
        .iter()
        .find(|c| c.name == "Button1")
        .expect("Button1");
    assert_eq!(button.signature.as_deref(), Some("TButton"));
}

#[test]
fn dfm_extracts_nested_component_hierarchy() {
    let code = "object Form1: TForm1
  object Panel1: TPanel
    object Label1: TLabel
      Caption = 'Hello'
    end
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 3);

    // Check nesting: Panel1 contains Label1
    let panel = components
        .iter()
        .find(|c| c.name == "Panel1")
        .expect("Panel1");
    let label = components
        .iter()
        .find(|c| c.name == "Label1")
        .expect("Label1");
    let contains_edge = result
        .edges
        .iter()
        .find(|e| e.source == panel.id && e.target == label.id && e.kind == EdgeKind::Contains);
    assert!(contains_edge.is_some());
}

#[test]
fn dfm_extracts_event_handler_references() {
    let code = "object Form1: TForm1
  OnCreate = FormCreate
  OnDestroy = FormDestroy
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 3);
    let all_names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert!(all_names.contains(&"FormCreate"));
    assert!(all_names.contains(&"FormDestroy"));
    assert!(all_names.contains(&"Button1Click"));
    assert!(
        refs.iter()
            .all(|r| r.reference_kind == EdgeKind::References)
    );
}

#[test]
fn dfm_handles_multi_line_properties() {
    let code = "object Form1: TForm1
  SQL.Strings = (
    'SELECT * FROM users'
    'WHERE active = 1')
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].reference_name, "Button1Click");
}

#[test]
fn dfm_handles_inherited_keyword() {
    let code = "inherited Form1: TForm1
  Caption = 'Inherited Form'
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
    assert!(names(&components).contains(&"Form1".to_string()));
}

#[test]
fn dfm_handles_item_collection_properties() {
    let code = "object Form1: TForm1
  object StatusBar1: TStatusBar
    Panels = <
      item
        Width = 200
      end
      item
        Width = 200
      end>
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
}

const MAINFORM_DFM: &str = "object frmMain: TfrmMain
  Left = 0
  Top = 0
  Caption = 'CodeGraph DFM Fixture'
  ClientHeight = 480
  ClientWidth = 640
  OnCreate = FormCreate
  OnDestroy = FormDestroy
  object pnlTop: TPanel
    Left = 0
    Top = 0
    Width = 640
    Height = 50
    object lblTitle: TLabel
      Left = 16
      Top = 16
      Caption = 'Authentication Service'
    end
    object btnLogin: TButton
      Left = 540
      Top = 12
      OnClick = btnLoginClick
    end
  end
  object pnlContent: TPanel
    Left = 0
    Top = 50
    object edtUsername: TEdit
      Left = 16
      Top = 16
      OnChange = edtUsernameChange
    end
    object edtPassword: TEdit
      Left = 16
      Top = 48
      OnKeyPress = edtPasswordKeyPress
    end
    object mmoLog: TMemo
      Left = 16
      Top = 88
    end
  end
  object pnlStatus: TStatusBar
    Left = 0
    Top = 440
    Panels = <
      item
        Width = 200
      end
      item
        Width = 200
      end>
  end
end";

#[test]
fn dfm_mainform_fixture_extracts_all_components() {
    let result = extract("MainForm.dfm", MAINFORM_DFM);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 9);
    let component_names = names(&components);
    for expected in [
        "frmMain",
        "pnlTop",
        "lblTitle",
        "btnLogin",
        "pnlContent",
        "edtUsername",
        "edtPassword",
        "mmoLog",
        "pnlStatus",
    ] {
        assert!(
            component_names.contains(&expected.to_string()),
            "missing {expected}"
        );
    }
}

#[test]
fn dfm_mainform_fixture_extracts_all_event_handlers() {
    let result = extract("MainForm.dfm", MAINFORM_DFM);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 5);
    let all_names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
    for expected in [
        "FormCreate",
        "FormDestroy",
        "btnLoginClick",
        "edtUsernameChange",
        "edtPasswordKeyPress",
    ] {
        assert!(all_names.contains(&expected), "missing {expected}");
    }
}
