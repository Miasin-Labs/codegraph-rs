use std::path::PathBuf;

use jsonc_parser::cst::CstInputValue;
use serde_json::json;

use super::shared::{
    get_mcp_server_config,
    home_dir,
    is_truthy,
    nonblank_env_path,
    parse_jsonc_object,
    read_jsonc_file,
    remove_jsonc_entry,
    upsert_jsonc_entry,
};
use super::types::{AgentTarget, DetectionResult, InstallOptions, Location, TargetId, WriteResult};

fn copilot_config_root() -> PathBuf {
    if let Some(xdg) = nonblank_env_path("XDG_CONFIG_HOME") {
        return xdg.join("github-copilot");
    }
    if cfg!(windows) {
        return nonblank_env_path("LOCALAPPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Local"))
            .join("github-copilot");
    }
    home_dir().join(".config/github-copilot")
}

fn intellij_dir() -> PathBuf {
    copilot_config_root().join("intellij")
}

fn mcp_json_path() -> PathBuf {
    intellij_dir().join("mcp.json")
}

fn jetbrains_config_dir_exists() -> bool {
    if cfg!(target_os = "macos") {
        return home_dir()
            .join("Library/Application Support/JetBrains")
            .exists();
    }
    if cfg!(windows) {
        return nonblank_env_path("APPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join("JetBrains")
            .exists();
    }
    nonblank_env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("JetBrains")
        .exists()
}

fn server_entry_input() -> CstInputValue {
    CstInputValue::Object(vec![
        (
            "type".to_string(),
            CstInputValue::String("stdio".to_string()),
        ),
        (
            "command".to_string(),
            CstInputValue::String("codegraph".to_string()),
        ),
        (
            "args".to_string(),
            CstInputValue::Array(vec![
                CstInputValue::String("serve".to_string()),
                CstInputValue::String("--mcp".to_string()),
            ]),
        ),
    ])
}

pub struct CopilotJetbrainsTarget;

impl AgentTarget for CopilotJetbrainsTarget {
    fn id(&self) -> TargetId {
        TargetId::CopilotJetbrains
    }

    fn display_name(&self) -> &'static str {
        "JetBrains IDEs (Copilot plugin)"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some(
            "https://docs.github.com/en/copilot/how-tos/provide-context/use-mcp/extend-copilot-chat-with-mcp",
        )
    }

    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        if loc == Location::Local {
            return DetectionResult {
                installed: false,
                already_configured: false,
                config_path: None,
            };
        }
        let file = mcp_json_path();
        let config = parse_jsonc_object(&read_jsonc_file(&file));
        DetectionResult {
            installed: intellij_dir().exists() || jetbrains_config_dir_exists(),
            already_configured: config
                .get("servers")
                .and_then(|servers| servers.get("codegraph"))
                .map(is_truthy)
                .unwrap_or(false),
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        if loc == Location::Local {
            return WriteResult {
                files: Vec::new(),
                notes: vec!["The JetBrains Copilot plugin has no project-local MCP config — re-run with --location=global to install.".to_string()],
            };
        }
        let file = mcp_json_path();
        let entry = get_mcp_server_config();
        WriteResult {
            files: vec![upsert_jsonc_entry(
                &file,
                "servers",
                "codegraph",
                &entry,
                server_entry_input(),
            )],
            notes: vec![
                "Restart your JetBrains IDE — the Copilot plugin only reads mcp.json on startup."
                    .to_string(),
            ],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        if loc == Location::Local {
            return WriteResult::default();
        }
        WriteResult {
            files: vec![remove_jsonc_entry(&mcp_json_path(), "servers", "codegraph")],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        if loc == Location::Local {
            return "# The JetBrains Copilot plugin has no project-local MCP config — use --location=global.\n".to_string();
        }
        let snippet = serde_json::to_string_pretty(&json!({
            "servers": { "codegraph": get_mcp_server_config() }
        }))
        .unwrap_or_default();
        format!(
            "# Add to {}\n# (Settings → Tools → GitHub Copilot → Model Context Protocol → Configure)\n\n{}\n",
            mcp_json_path().display(),
            snippet
        )
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        match loc {
            Location::Global => vec![mcp_json_path()],
            Location::Local => Vec::new(),
        }
    }
}

pub static COPILOT_JETBRAINS_TARGET: CopilotJetbrainsTarget = CopilotJetbrainsTarget;
