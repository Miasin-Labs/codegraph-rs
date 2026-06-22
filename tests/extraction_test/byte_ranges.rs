use crate::extraction_test::fixture::*;

// =============================================================================
// Byte ranges (schema v5) — tree-sitter start_byte()/end_byte() stored on nodes
// =============================================================================

#[test]
fn byte_ranges_match_source_slices_for_typescript() {
    let code = "export function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport class Greeter {\n  greet(name: string): string {\n    return `hi ${name}`;\n  }\n}\n";
    let result = extract("src/bytes.ts", code);

    // Every tree-sitter-extracted node carries a well-formed byte range.
    for node in &result.nodes {
        let range = node
            .byte_range()
            .unwrap_or_else(|| panic!("node {} ({:?}) missing byte range", node.name, node.kind));
        assert!(
            range.end <= code.len(),
            "range {range:?} exceeds source for {}",
            node.name
        );
    }

    // The file node spans the whole source.
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));

    // Function/class/method ranges anchor exactly at their tree-sitter nodes
    // and slice back to the original declaration text.
    let add = find_named(&result, NodeKind::Function, "add").expect("add");
    let add_range = add.byte_range().unwrap();
    assert_eq!(add_range.start, code.find("function add").unwrap());
    let add_src = &code[add_range];
    assert!(add_src.starts_with("function add"));
    assert!(add_src.ends_with("return a + b;\n}"));

    let greeter = find_named(&result, NodeKind::Class, "Greeter").expect("Greeter");
    let greeter_src = &code[greeter.byte_range().unwrap()];
    assert!(greeter_src.starts_with("class Greeter"));
    assert!(greeter_src.ends_with('}'));

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    let greet_src = &code[greet.byte_range().unwrap()];
    assert!(greet_src.starts_with("greet(name: string)"));
    assert!(greet_src.contains("return `hi ${name}`;"));
}

#[test]
fn byte_ranges_round_trip_through_full_indexing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    let code = "export function add(a: number, b: number): number {\n  return a + b;\n}\n";
    fs::write(src_dir.join("utils.ts"), code).unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");
    assert!(result.success);

    // Byte offsets survive SQLite storage and slice the on-disk source back
    // to the declaration.
    let nodes = queries.get_nodes_by_file("src/utils.ts").unwrap();
    let add = nodes.iter().find(|n| n.name == "add").expect("add");
    let range = add.byte_range().expect("byte range stored");
    let on_disk = fs::read_to_string(src_dir.join("utils.ts")).unwrap();
    assert!(on_disk[range].starts_with("function add"));

    let file_node = nodes
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));
}

#[test]
fn byte_ranges_svelte_script_nodes_are_whole_file_offsets() {
    let code = "<script>\n  function bump(n) {\n    return n + 1;\n  }\n</script>\n\n<button on:click={() => bump(1)}>+</button>\n";
    let result = extract("src/Counter.svelte", code);

    // The component node spans the whole .svelte file.
    let component = find_kind(&result, NodeKind::Component).expect("component node");
    assert_eq!(component.byte_range(), Some(0..code.len()));

    // Script-block nodes are remapped from slice-relative to whole-file
    // offsets, so slicing the full source recovers the declaration.
    let bump = find_named(&result, NodeKind::Function, "bump").expect("bump");
    let range = bump.byte_range().expect("script node byte range");
    assert_eq!(range.start, code.find("function bump").unwrap());
    assert!(code[range].starts_with("function bump"));
}

#[test]
fn byte_ranges_vue_script_nodes_are_whole_file_offsets() {
    let code = "<template>\n  <button @click=\"bump\">+</button>\n</template>\n\n<script setup>\nfunction bump(n) {\n  return n + 1;\n}\n</script>\n";
    let result = extract("src/Counter.vue", code);

    let component = find_kind(&result, NodeKind::Component).expect("component node");
    assert_eq!(component.byte_range(), Some(0..code.len()));

    let bump = find_named(&result, NodeKind::Function, "bump").expect("bump");
    let range = bump.byte_range().expect("script node byte range");
    assert_eq!(range.start, code.find("function bump").unwrap());
    assert!(code[range].starts_with("function bump"));
}

#[test]
fn byte_ranges_absent_for_extractors_without_offsets() {
    // The IDA-C extractor tracks line/column only — its function nodes keep
    // NULL byte offsets (honest absence), while its file node spans the file.
    let code = "//----- (00000001800012F0) ----------------------------------------------------\n// Function: sub_1800012F0\n__int64 __fastcall sub_1800012F0(__int64 a1)\n{\n  return a1 + 1;\n}\n";
    assert!(is_ida_generated_c("sub_1800012F0.c", code));
    let result = extract("sub_1800012F0.c", code);

    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));

    let func = find_kind(&result, NodeKind::Function).expect("function node");
    assert_eq!(func.start_byte, None);
    assert_eq!(func.end_byte, None);
    assert_eq!(func.byte_range(), None);
}
