use super::*;

// --- TypeScript/JavaScript imports ---

#[test]
fn ts_imports_default() {
    let result = extract("app.tsx", "import React from 'react';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert_eq!(sig(import_node), "import React from 'react';");
}

#[test]
fn ts_imports_named() {
    let result = extract(
        "icons.tsx",
        "import { Bug, Database } from '@phosphor-icons/react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "@phosphor-icons/react");
    assert!(sig(import_node).contains("Bug"));
    assert!(sig(import_node).contains("Database"));
}

#[test]
fn ts_imports_namespace() {
    let result = extract(
        "icons.tsx",
        "import * as Icons from '@phosphor-icons/react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "@phosphor-icons/react");
    assert!(sig(import_node).contains("* as Icons"));
}

#[test]
fn ts_imports_side_effect() {
    let result = extract("app.tsx", "import './styles.css';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "./styles.css");
}

#[test]
fn ts_imports_mixed_default_plus_named() {
    let result = extract(
        "app.tsx",
        "import React, { useState, useEffect } from 'react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("React"));
    assert!(sig(import_node).contains("useState"));
    assert!(sig(import_node).contains("useEffect"));
}

#[test]
fn ts_imports_multiple_statements() {
    let code = r#"
import React from 'react';
import { Button } from './components';
import './styles.css';
"#;
    let result = extract("app.tsx", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"react".to_string()));
    assert!(import_names.contains(&"./components".to_string()));
    assert!(import_names.contains(&"./styles.css".to_string()));
}

#[test]
fn ts_imports_type_imports() {
    let result = extract("types.ts", "import type { FC, ReactNode } from 'react';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("type"));
    assert!(sig(import_node).contains("FC"));
}

#[test]
fn ts_imports_aliased_named() {
    let result = extract(
        "hooks.ts",
        "import { useState as useStateAlias } from 'react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("useState"));
    assert!(sig(import_node).contains("useStateAlias"));
}

#[test]
fn ts_imports_relative_path() {
    let result = extract(
        "components/Button.tsx",
        "import { helper } from '../utils/helper';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../utils/helper");
    assert!(sig(import_node).contains("helper"));
}

// --- Python imports ---
