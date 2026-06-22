use super::*;

// =============================================================================
// analyze capabilities
// =============================================================================

/// Every engine capability, in display order. The engine's own
/// `ALL_CAPABILITIES` array is private (see `notes/close-tier1-needs.md`),
/// so the list is mirrored here — `Capability` is `#[non_exhaustive]`-free
/// and six-variant today.
const ALL_CAPABILITIES: [(Capability, &str); 6] = [
    (Capability::CallGraph, "callGraph"),
    (Capability::TypeUsage, "typeUsage"),
    (Capability::PartialStruct, "partialStruct"),
    (Capability::VirtualValidation, "virtualValidation"),
    (Capability::Persistence, "persistence"),
    (Capability::SymbolEditing, "symbolEditing"),
];

/// One capability's resolved status.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub name: String,
    /// The `CODEGRAPH_ANALYSIS_CAP_*` kill-switch env var.
    pub env_var: String,
    /// Resolved state after env overrides and dependency cascading.
    pub enabled: bool,
    /// Raw env var value, when set in the current environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_value: Option<String>,
    /// Capabilities additionally disabled when this one is turned off
    /// (dependency cascade).
    pub disables: Vec<String>,
}

/// Result of [`capabilities_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesReport {
    pub capabilities: Vec<CapabilityStatus>,
    pub note: String,
}

fn capability_display_name(cap: Capability) -> &'static str {
    ALL_CAPABILITIES
        .iter()
        .find(|(c, _)| *c == cap)
        .map(|(_, name)| *name)
        .unwrap_or("unknown")
}

/// The engine's capability tree resolved against the current environment
/// (engine entry points: `CapabilityTree::from_env`, `Capability::env_name`).
/// Pure environment read — touches no index.
pub fn capabilities_report() -> CapabilitiesReport {
    let resolved = CapabilityTree::from_env();

    let capabilities: Vec<CapabilityStatus> = ALL_CAPABILITIES
        .iter()
        .map(|(cap, name)| {
            // Probe the dependency cascade against a fresh default tree:
            // `disable` returns exactly the dependents it switched off.
            let mut probe = CapabilityTree::new();
            let mut disables: Vec<String> = probe
                .disable(*cap)
                .into_iter()
                .map(|c| capability_display_name(c).to_string())
                .collect();
            disables.sort();

            CapabilityStatus {
                name: (*name).to_string(),
                env_var: cap.env_name().to_string(),
                enabled: resolved.is_enabled(*cap),
                env_value: std::env::var(cap.env_name()).ok(),
                disables,
            }
        })
        .collect();

    CapabilitiesReport {
        capabilities,
        note: "All capabilities are enabled by default. Set <ENV_VAR>=0|false|off|no|disabled \
               to disable one; disabling cascades to its dependents (listed in disables)."
            .to_string(),
    }
}
