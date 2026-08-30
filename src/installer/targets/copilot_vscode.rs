use std::path::PathBuf;

use jsonc_parser::cst::CstInputValue;
use serde_json::{Value, json};

use super::shared::{
    cwd,
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

fn vscode_user_dir() -> PathBuf {
    if cfg!(windows) {
        return nonblank_env_path("APPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join("Code/User");
    }
    if cfg!(target_os = "macos") {
        return home_dir().join("Library/Application Support/Code/User");
    }
    nonblank_env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("Code/User")
}

fn mcp_json_path(loc: Location) -> PathBuf {
    match loc {
        Location::Global => vscode_user_dir().join("mcp.json"),
        Location::Local => cwd().join(".vscode/mcp.json"),
    }
}

fn server_entry(loc: Location) -> Value {
    let mut entry = get_mcp_server_config();
    if loc == Location::Local {
        if let Some(args) = entry.get_mut("args").and_then(Value::as_array_mut) {
            args.push(Value::String("--path".to_string()));
            args.push(Value::String(cwd().display().to_string()));
        }
    }
    entry
}

fn server_entry_input(loc: Location) -> CstInputValue {
    let mut args = vec![
        CstInputValue::String("serve".to_string()),
        CstInputValue::String("--mcp".to_string()),
    ];
    if loc == Location::Local {
        args.push(CstInputValue::String("--path".to_string()));
        args.push(CstInputValue::String(cwd().display().to_string()));
    }
    CstInputValue::Object(vec![
        (
            "type".to_string(),
            CstInputValue::String("stdio".to_string()),
        ),
        (
            "command".to_string(),
            CstInputValue::String("codegraph".to_string()),
        ),
        ("args".to_string(), CstInputValue::Array(args)),
    ])
}

pub struct CopilotVscodeTarget;

impl AgentTarget for CopilotVscodeTarget {
    fn id(&self) -> TargetId {
        TargetId::CopilotVscode
    }

    fn display_name(&self) -> &'static str {
        "VS Code (Copilot Chat)"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some("https://code.visualstudio.com/docs/copilot/customization/mcp-servers")
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, loc: Location) -> DetectionResult {
        let file = mcp_json_path(loc);
        let config = parse_jsonc_object(&read_jsonc_file(&file));
        let already_configured = config
            .get("servers")
            .and_then(|servers| servers.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        let installed = match loc {
            Location::Global => vscode_user_dir().exists() || home_dir().join(".vscode").exists(),
            Location::Local => cwd().join(".vscode").exists(),
        };
        DetectionResult {
            installed,
            already_configured,
            config_path: Some(file),
        }
    }

    fn install(&self, loc: Location, _opts: &InstallOptions) -> WriteResult {
        let file = mcp_json_path(loc);
        let entry = server_entry(loc);
        WriteResult {
            files: vec![upsert_jsonc_entry(
                &file,
                "servers",
                "codegraph",
                &entry,
                server_entry_input(loc),
            )],
            notes: vec!["Restart VS Code for MCP changes to take effect.".to_string()],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        WriteResult {
            files: vec![remove_jsonc_entry(
                &mcp_json_path(loc),
                "servers",
                "codegraph",
            )],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        let snippet = serde_json::to_string_pretty(&json!({
            "servers": { "codegraph": server_entry(loc) }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", mcp_json_path(loc).display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        vec![mcp_json_path(loc)]
    }
}

pub static COPILOT_VSCODE_TARGET: CopilotVscodeTarget = CopilotVscodeTarget;
