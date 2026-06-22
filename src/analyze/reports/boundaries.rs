use super::*;

// =============================================================================
// analyze boundaries
// =============================================================================

/// An HTTP route provider.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRouteBoundary {
    pub method: String,
    pub path: String,
    pub provider: SymbolRef,
}

/// A C-ABI export.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfiBoundary {
    pub symbol_name: String,
    pub provider: SymbolRef,
}

/// A WASM export or import.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmBoundary {
    /// `export` or `import`.
    pub direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub name: String,
    pub provider: SymbolRef,
}

/// Cross-language stitching counters from the engine's
/// `resolve_cross_language_calls`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossLanguageStitching {
    pub boundaries_seen: usize,
    pub clients_seen: usize,
    pub edges_emitted: usize,
    pub edge_errors: usize,
}

/// Result of [`boundaries_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundariesReport {
    pub boundary_count: usize,
    pub http_routes: Vec<HttpRouteBoundary>,
    pub ffi_exports: Vec<FfiBoundary>,
    pub wasm_boundaries: Vec<WasmBoundary>,
    pub cross_language_calls: CrossLanguageStitching,
    pub note: String,
}

/// Polyglot boundary detection + cross-language call stitching (engine entry
/// points: `polyglot::{detect_http_routes, detect_ffi_exports,
/// detect_wasm_exports, resolve_cross_language_calls}`). Stitched
/// `ExternalCall` edges land in the in-memory graph only — the SQLite index
/// is never mutated.
pub fn boundaries_report(graph: &mut AnalysisGraph) -> BoundariesReport {
    let mut boundaries = detect_http_routes(graph);
    boundaries.extend(detect_ffi_exports(graph));
    boundaries.extend(detect_wasm_exports(graph));

    let mut http_routes: Vec<HttpRouteBoundary> = Vec::new();
    let mut ffi_exports: Vec<FfiBoundary> = Vec::new();
    let mut wasm_boundaries: Vec<WasmBoundary> = Vec::new();
    for boundary in &boundaries {
        let Some(provider) = graph.get_node(&boundary.provider_node).map(symbol_ref) else {
            continue;
        };
        match &boundary.kind {
            BoundaryKind::HttpRoute { method, path } => http_routes.push(HttpRouteBoundary {
                method: method.clone(),
                path: path.clone(),
                provider,
            }),
            BoundaryKind::FfiExport { symbol } => ffi_exports.push(FfiBoundary {
                symbol_name: symbol.clone(),
                provider,
            }),
            BoundaryKind::WasmExport { name } => wasm_boundaries.push(WasmBoundary {
                direction: "export".to_string(),
                module: None,
                name: name.clone(),
                provider,
            }),
            BoundaryKind::WasmImport { module, name } => wasm_boundaries.push(WasmBoundary {
                direction: "import".to_string(),
                module: Some(module.clone()),
                name: name.clone(),
                provider,
            }),
            BoundaryKind::GrpcService { .. } => {}
        }
    }
    http_routes.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    ffi_exports.sort_by(|a, b| a.symbol_name.cmp(&b.symbol_name));
    wasm_boundaries.sort_by(|a, b| (&a.direction, &a.name).cmp(&(&b.direction, &b.name)));

    let report = resolve_cross_language_calls(graph, &boundaries);
    let boundary_count = http_routes.len() + ffi_exports.len() + wasm_boundaries.len();

    let note = if boundary_count == 0 {
        "No cross-language boundaries detected. Detection reads the engine's metadata \
         contract on Function nodes (http_route/http_method, http_client_target, ffi_export, \
         wasm_export, wasm_import_module/name); the SQLite bridge does not populate these \
         keys yet — host route nodes are dropped by the 5-kind projection and extern/wasm \
         qualifiers are not carried in signatures — so a bridged index reports no boundaries \
         until that enrichment lands."
            .to_string()
    } else {
        "Stitched cross-language call edges (ExternalCall) connect HTTP clients to matching \
         route providers in the in-memory analysis graph only; the index is unchanged."
            .to_string()
    };

    BoundariesReport {
        boundary_count,
        http_routes,
        ffi_exports,
        wasm_boundaries,
        cross_language_calls: CrossLanguageStitching {
            boundaries_seen: report.boundaries_seen,
            clients_seen: report.clients_seen,
            edges_emitted: report.edges_emitted,
            edge_errors: report.edge_errors,
        },
        note,
    }
}
