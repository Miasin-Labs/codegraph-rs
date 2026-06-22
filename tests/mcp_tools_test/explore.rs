fn budget_fixture(root: &Path) -> CodeGraph {
    let src_dir = root.join("src");
    let mut fat_lines: Vec<String> = vec!["export class Session {".to_string()];
    for i in 0..30 {
        fat_lines.push(format!("  method{i}(arg: string): string {{"));
        fat_lines.push(format!("    return this.helper{i}(arg) + \"{i}\";"));
        fat_lines.push("  }".to_string());
        fat_lines.push(format!("  private helper{i}(arg: string): string {{"));
        fat_lines.push(format!("    return arg.repeat({});", i + 1));
        fat_lines.push("  }".to_string());
    }
    fat_lines.push("}".to_string());
    write(&src_dir.join("session.ts"), &fat_lines.join("\n"));

    for i in 0..5 {
        write(
            &src_dir.join(format!("support{i}.ts")),
            &format!(
                "import {{ Session }} from './session';\nexport function callSession{i}(s: Session) {{ return s.method{i}('hi'); }}\n"
            ),
        );
    }

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn explore(handler: &ToolHandler, query: &str) -> String {
    let res = handler.execute("codegraph_explore", &json!({ "query": query }));
    assert_ne!(res.is_error, Some(true), "explore errored: {}", res.text());
    res.text().to_string()
}

include!("explore_graph.rs");
include!("explore_output.rs");
