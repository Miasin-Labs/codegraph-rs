use super::*;

#[test]
fn liquid_imports_render_tag() {
    let result = extract("template.liquid", "{% render 'loading-spinner' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "loading-spinner");
    assert!(sig(import_node).contains("render"));
}

#[test]
fn liquid_imports_section_tag() {
    let result = extract("layout/theme.liquid", "{% section 'header' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "header");
    assert!(sig(import_node).contains("section"));
}

#[test]
fn liquid_imports_include_tag() {
    let result = extract("snippets/header.liquid", "{% include 'icon-cart' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "icon-cart");
    assert!(sig(import_node).contains("include"));
}

#[test]
fn liquid_imports_render_with_whitespace_control() {
    let result = extract("snippets/product.liquid", "{%- render 'price' -%}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "price");
}

#[test]
fn liquid_imports_multiple() {
    let code = "
{% section 'header' %}
{% render 'loading-spinner' %}
{% render 'cart-drawer' %}
";
    let result = extract("layout/theme.liquid", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"header".to_string()));
    assert!(import_names.contains(&"loading-spinner".to_string()));
    assert!(import_names.contains(&"cart-drawer".to_string()));
}
