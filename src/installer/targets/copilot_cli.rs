use std::path::PathBuf;
use std::{env, fs};

use serde_json::{Map, Value, json};

use super::shared::{
    get_mcp_server_config,
    home_dir,
    is_truthy,
    json_deep_equal,
    read_json_file,
    write_json_file,
};
use super::types::{
    AgentTarget,
    DetectionResult,
    FileAction,
    FileWrite,
    InstallOptions,
    Location,
    TargetId,
    WriteResult,
};

fn config_dir() -> PathBuf {
    env::var_os("COPILOT_HOME")
        .filter(|value| !value.is_empty() && !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".copilot"))
}

fn mcp_config_path() -> PathBuf {
    config_dir().join("mcp-config.json")
}

fn cli_config_dir_present() -> bool {
    fs::read_dir(config_dir()).ok().is_some_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name() != "ide")
    })
}

fn copilot_on_path() -> bool {
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ".ps1"]
    } else {
        &[""]
    };
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .any(|directory| {
            extensions
                .iter()
                .any(|extension| directory.join(format!("copilot{extension}")).exists())
        })
}

fn server_entry() -> Value {
    let mut entry = get_mcp_server_config();
    if let Some(object) = entry.as_object_mut() {
        object.insert("tools".to_string(), json!(["*"]));
    }
    entry
}

pub struct CopilotCliTarget;

impl AgentTarget for CopilotCliTarget {
    fn id(&self) -> TargetId {
        TargetId::CopilotCli
    }

    fn display_name(&self) -> &'static str {
        "GitHub Copilot CLI"
    }

    fn docs_url(&self) -> Option<&'static str> {
        Some(
            "https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers",
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
        let file = mcp_config_path();
        let config = read_json_file(&file);
        DetectionResult {
            installed: cli_config_dir_present() || copilot_on_path(),
            already_configured: config
                .get("mcpServers")
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
                notes: vec!["Copilot CLI has no project-local config — re-run with --location=global to install.".to_string()],
            };
        }
        WriteResult {
            files: vec![write_mcp_entry()],
            notes: vec![
                "Restart any running Copilot CLI session to pick up the MCP server.".to_string(),
            ],
        }
    }

    fn uninstall(&self, loc: Location) -> WriteResult {
        if loc == Location::Local {
            return WriteResult::default();
        }
        let file = mcp_config_path();
        let mut config = read_json_file(&file);
        let has_codegraph = config
            .get("mcpServers")
            .and_then(|servers| servers.get("codegraph"))
            .map(is_truthy)
            .unwrap_or(false);
        if !file.exists() || !has_codegraph {
            return WriteResult {
                files: vec![FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                }],
                notes: Vec::new(),
            };
        }
        if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
            servers.remove("codegraph");
            if servers.is_empty() {
                config.remove("mcpServers");
            }
        }
        if config.is_empty() {
            let _ = fs::remove_file(&file);
        } else {
            write_json_file(&file, &config);
        }
        WriteResult {
            files: vec![FileWrite {
                path: file,
                action: FileAction::Removed,
            }],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, loc: Location) -> String {
        if loc == Location::Local {
            return "# Copilot CLI has no project-local config — use --location=global.\n"
                .to_string();
        }
        let snippet = serde_json::to_string_pretty(&json!({
            "mcpServers": { "codegraph": server_entry() }
        }))
        .unwrap_or_default();
        format!("# Add to {}\n\n{}\n", mcp_config_path().display(), snippet)
    }

    fn describe_paths(&self, loc: Location) -> Vec<PathBuf> {
        match loc {
            Location::Global => vec![mcp_config_path()],
            Location::Local => Vec::new(),
        }
    }
}

fn write_mcp_entry() -> FileWrite {
    let file = mcp_config_path();
    let existed = file.exists();
    let mut config = read_json_file(&file);
    let after = server_entry();
    if config
        .get("mcpServers")
        .and_then(|servers| servers.get("codegraph"))
        .is_some_and(|before| json_deep_equal(before, &after))
    {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    if !matches!(config.get("mcpServers"), Some(Value::Object(_))) {
        config.insert("mcpServers".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(servers)) = config.get_mut("mcpServers") {
        servers.insert("codegraph".to_string(), after);
    }
    write_json_file(&file, &config);
    FileWrite {
        path: file,
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

pub static COPILOT_CLI_TARGET: CopilotCliTarget = CopilotCliTarget;
