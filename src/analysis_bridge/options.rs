use std::ffi::OsString;

/// Environment variable that turns on field carrying for the CLI bridge
/// path ([`crate::analysis_bridge::build_analysis_graph_cached`]):
/// `CODEGRAPH_ANALYSIS_FIELDS=1` (or `true`). See the module docs for what
/// it does and what it costs.
pub const ANALYSIS_FIELDS_ENV: &str = "CODEGRAPH_ANALYSIS_FIELDS";

/// Behavior switches for the bridge. Off-by-default flags keep the default
/// graph byte-identical to what pre-options builds produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BridgeOptions {
    /// Carry `field`/`property` rows through the bridge as the analysis
    /// engine's typed partial-struct metadata (see module docs: "Field
    /// carrying"). Off by default; adds metadata weight most analyses
    /// never read. Node count is unchanged either way.
    pub include_fields: bool,
}

impl BridgeOptions {
    /// Read the flag gate from the process environment
    /// ([`ANALYSIS_FIELDS_ENV`]). Anything other than `1`/`true`
    /// (case-insensitive) - including unset - is off.
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var_os(ANALYSIS_FIELDS_ENV))
    }

    /// Env-free core of [`Self::from_env`] so tests can exercise the gate
    /// without process-global env mutation.
    pub(super) fn from_env_value(value: Option<OsString>) -> Self {
        let include_fields = value
            .map(|v| {
                let v = v.to_string_lossy().trim().to_ascii_lowercase();
                v == "1" || v == "true"
            })
            .unwrap_or(false);
        Self { include_fields }
    }
}
