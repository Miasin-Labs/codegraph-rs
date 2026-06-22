mod path_traversal_prevention {
    use super::*;

    fn setup(root: &Path) -> CodeGraph {
        write(
            &root.join("src/hello.ts"),
            "export function hello(): string { return \"hi\"; }\n",
        );
        let cg = CodeGraph::init_sync(root).unwrap();
        cg.index_all(&IndexOptions::default()).unwrap();
        cg
    }

    #[test]
    fn reads_code_for_valid_nodes_within_project() {
        let dir = TempDir::new().unwrap();
        let cg = setup(dir.path());

        let nodes = cg.get_nodes_by_kind(NodeKind::Function).unwrap();
        let hello = nodes
            .iter()
            .find(|n| n.name == "hello")
            .expect("hello should be indexed");

        let code = cg.get_code(&hello.id).unwrap();
        assert!(code.expect("code should be readable").contains("hello"));
    }

    #[test]
    fn returns_none_for_non_existent_node() {
        let dir = TempDir::new().unwrap();
        let cg = setup(dir.path());

        assert!(cg.get_code("does-not-exist").unwrap().is_none());
    }
}
