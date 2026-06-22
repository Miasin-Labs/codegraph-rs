use super::*;

// =============================================================================
// analyze complexity
// =============================================================================

/// Per-function complexity metrics for one analyzed function.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionComplexity {
    pub symbol: SymbolRef,
    pub language: String,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loc_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loc_source: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintainability_index: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halstead_volume: Option<f64>,
}

/// Result of [`complexity_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityReport {
    /// All `Function` nodes in the bridged graph (placeholders included in
    /// `skipped`, not here).
    pub functions_total: usize,
    /// Functions whose source was parsed and measured.
    pub functions_analyzed: usize,
    /// Skip counts keyed by reason (`placeholder`, `unsupportedLanguage`,
    /// `fileUnreadable`, `bodyNotLocated`, `noMetrics`).
    pub skipped: BTreeMap<String, usize>,
    /// Most complex functions, cyclomatic desc / cognitive desc.
    pub functions: Vec<FunctionComplexity>,
}

/// Map a detected host language onto the analysis crate's complexity-rule id.
pub(crate) fn complexity_lang_id(language: Language) -> Option<&'static str> {
    Some(match language {
        Language::Rust => "rust",
        Language::Typescript | Language::Tsx | Language::Javascript | Language::Jsx => "typescript",
        Language::Python => "python",
        Language::Go => "go",
        Language::Java => "java",
        Language::C => "c",
        Language::Cpp => "cpp",
        Language::Php => "php",
        Language::Kotlin => "kotlin",
        Language::Swift => "swift",
        Language::Csharp => "csharp",
        Language::Ruby => "ruby",
        _ => return None,
    })
}

/// Walk up from the node at the function's recorded start position to the
/// nearest ancestor with a function body per the language rules. The bridge
/// carries line/column spans (no byte ranges), so location is point-based.
pub(crate) fn locate_function_node<'t>(
    root: TsNode<'t>,
    start_line: u32,
    start_col: u32,
    rules: &LangRules,
) -> Option<TsNode<'t>> {
    let point = Point {
        row: start_line.saturating_sub(1) as usize,
        column: start_col as usize,
    };
    let mut node = root.named_descendant_for_point_range(point, point)?;
    loop {
        if rules
            .body_field_names
            .iter()
            .any(|f| node.child_by_field_name(f).is_some())
        {
            return Some(node);
        }
        node = node.parent()?;
    }
}

/// Compute the analysis crate's complexity metrics for every function in the
/// bridged graph by re-parsing the on-disk sources under `workspace_root`,
/// keeping the `top` most complex.
pub fn complexity_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    top: usize,
) -> ComplexityReport {
    struct ParsedFile {
        tree: Tree,
        source: String,
        lang_id: &'static str,
        language: Language,
    }

    fn parse_file(workspace_root: &Path, rel_path: &Path) -> Option<ParsedFile> {
        let language = detect_language(&rel_path.to_string_lossy(), None);
        let lang_id = complexity_lang_id(language)?;
        let source = std::fs::read_to_string(workspace_root.join(rel_path)).ok()?;
        let mut parser = create_parser(language)?;
        let tree = parser.parse(&source, None)?;
        Some(ParsedFile {
            tree,
            source,
            lang_id,
            language,
        })
    }

    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    let mut skip = |reason: &str| {
        *skipped.entry(reason.to_string()).or_default() += 1;
    };

    let mut functions = graph.nodes_by_kind(ANodeKind::Function);
    functions.sort_by(|a, b| {
        (&a.file_path, a.span.start_line, &a.qualified_name).cmp(&(
            &b.file_path,
            b.span.start_line,
            &b.qualified_name,
        ))
    });

    let mut cache: HashMap<String, Option<ParsedFile>> = HashMap::new();
    let mut measured: Vec<FunctionComplexity> = Vec::new();
    let mut functions_total = 0usize;

    for node in functions {
        if is_placeholder(node) {
            skip("placeholder");
            continue;
        }
        functions_total += 1;

        let key = node.file_path.display().to_string();
        let parsed = cache
            .entry(key)
            .or_insert_with(|| parse_file(workspace_root, &node.file_path));
        let Some(parsed) = parsed.as_ref() else {
            // Either no complexity rules for the language or the file is
            // gone/unreadable — distinguish the two for the report.
            if complexity_lang_id(detect_language(&node.file_path.to_string_lossy(), None))
                .is_none()
            {
                skip("unsupportedLanguage");
            } else {
                skip("fileUnreadable");
            }
            continue;
        };

        let Some(rules) = LangRules::for_language(parsed.lang_id) else {
            skip("unsupportedLanguage");
            continue;
        };
        let Some(fn_node) = locate_function_node(
            parsed.tree.root_node(),
            node.span.start_line,
            node.span.start_col,
            rules,
        ) else {
            skip("bodyNotLocated");
            continue;
        };
        let Some(metrics) = compute_complexity(fn_node, parsed.source.as_bytes(), parsed.lang_id)
        else {
            skip("noMetrics");
            continue;
        };

        measured.push(FunctionComplexity {
            symbol: symbol_ref(node),
            language: parsed.language.as_str().to_string(),
            cyclomatic: metrics.cyclomatic,
            cognitive: metrics.cognitive,
            max_nesting: metrics.max_nesting,
            loc_total: metrics.loc.as_ref().map(|l| l.total),
            loc_source: metrics.loc.as_ref().map(|l| l.source),
            maintainability_index: metrics.maintainability_index,
            halstead_volume: metrics.halstead.as_ref().map(|h| h.volume),
        });
    }

    let functions_analyzed = measured.len();
    measured.sort_by(|a, b| {
        b.cyclomatic
            .cmp(&a.cyclomatic)
            .then_with(|| b.cognitive.cmp(&a.cognitive))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    measured.truncate(top);

    ComplexityReport {
        functions_total,
        functions_analyzed,
        skipped,
        functions: measured,
    }
}
