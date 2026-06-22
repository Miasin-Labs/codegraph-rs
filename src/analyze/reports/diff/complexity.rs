use super::{
    ANodeId,
    ANodeKind,
    AnalysisGraph,
    HashMap,
    LangRules,
    Path,
    StoredComplexity,
    Tree,
    complexity_lang_id,
    compute_complexity,
    create_parser,
    detect_language,
    is_placeholder,
    locate_function_node,
};

/// Measure cyclomatic/cognitive complexity for every non-placeholder
/// `Function` node by re-parsing the on-disk sources (the `analyze
/// complexity` anchor pattern). Functions whose language has no complexity
/// rules, whose file is unreadable, or whose body cannot be located are
/// simply absent from the map — `analyze diff` reports those as
/// "complexity unavailable", never as zero.
pub fn measure_complexity_map(
    graph: &AnalysisGraph,
    workspace_root: &Path,
) -> HashMap<ANodeId, StoredComplexity> {
    struct ParsedFile {
        tree: Tree,
        source: String,
        lang_id: &'static str,
    }
    let mut cache: HashMap<String, Option<ParsedFile>> = HashMap::new();
    let mut measured: HashMap<ANodeId, StoredComplexity> = HashMap::new();

    for node in graph.nodes_by_kind(ANodeKind::Function) {
        if is_placeholder(node) {
            continue;
        }
        let key = node.file_path.display().to_string();
        let parsed = cache.entry(key).or_insert_with(|| {
            let language = detect_language(&node.file_path.to_string_lossy(), None);
            let lang_id = complexity_lang_id(language)?;
            let source = std::fs::read_to_string(workspace_root.join(&node.file_path)).ok()?;
            let mut parser = create_parser(language)?;
            let tree = parser.parse(&source, None)?;
            Some(ParsedFile {
                tree,
                source,
                lang_id,
            })
        });
        let Some(parsed) = parsed.as_ref() else {
            continue;
        };
        let Some(rules) = LangRules::for_language(parsed.lang_id) else {
            continue;
        };
        let Some(fn_node) = locate_function_node(
            parsed.tree.root_node(),
            node.span.start_line,
            node.span.start_col,
            rules,
        ) else {
            continue;
        };
        let Some(metrics) = compute_complexity(fn_node, parsed.source.as_bytes(), parsed.lang_id)
        else {
            continue;
        };
        measured.insert(
            node.id.clone(),
            StoredComplexity {
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
                max_nesting: metrics.max_nesting,
            },
        );
    }
    measured
}
