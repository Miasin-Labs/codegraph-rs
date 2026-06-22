use super::*;

#[test]
fn kotlin_imports_simple() {
    let result = extract("Main.kt", "import java.io.IOException");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.io.IOException");
    assert_eq!(sig(import_node), "import java.io.IOException");
}

#[test]
fn kotlin_imports_aliased() {
    let result = extract(
        "Utils.kt",
        "import okhttp3.Request.Builder as RequestBuilder",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "okhttp3.Request.Builder");
    assert!(sig(import_node).contains("as RequestBuilder"));
}

#[test]
fn kotlin_imports_wildcard() {
    let result = extract("Time.kt", "import java.util.concurrent.TimeUnit.*");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.concurrent.TimeUnit");
    assert!(sig(import_node).contains(".*"));
}

#[test]
fn kotlin_imports_multiple() {
    let code = "
import java.io.IOException
import kotlin.test.assertFailsWith
import okhttp3.OkHttpClient
";
    let result = extract("Test.kt", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"java.io.IOException".to_string()));
    assert!(import_names.contains(&"kotlin.test.assertFailsWith".to_string()));
    assert!(import_names.contains(&"okhttp3.OkHttpClient".to_string()));
}

// --- Java imports ---
