use std::collections::HashSet;
use std::rc::Rc;

use codegraph::mcp::ToolHandler;
use codegraph::mcp::daemon_registry::{
    DaemonRecord,
    StopOutcome,
    daemon_at,
    list_daemons,
    stop_all_daemons,
    stop_daemon_at,
};
use codegraph::telemetry::{TELEMETRY_DOCS, Telemetry, TelemetryDecision};
use codegraph::{CodeGraph, OpenOptions};
use serde_json::{Map, Value};

use super::{
    error_msg,
    format_duration,
    info,
    is_initialized,
    now_ms,
    parse_int_js,
    process,
    resolve_project_path,
    success,
};

fn open_handler(path_arg: Option<&str>) -> Result<(Rc<CodeGraph>, ToolHandler), String> {
    let project_path = resolve_project_path(path_arg);
    if !is_initialized(&project_path) {
        return Err(format!(
            "CodeGraph isn't available here - no .codegraph/ index exists in {}. Run `codegraph init` first.",
            project_path.display()
        ));
    }
    let graph = Rc::new(
        CodeGraph::open(&project_path, &OpenOptions::default())
            .map_err(|error| error.to_string())?,
    );
    let handler = ToolHandler::new(Some(Rc::clone(&graph)));
    Ok((graph, handler))
}

fn execute_and_print(tool: &str, args: Value, path_arg: Option<&str>) -> Result<bool, String> {
    let (graph, handler) = open_handler(path_arg)?;
    let result = handler.execute(tool, &args);
    println!("{}", result.text());
    let failed = result.is_error == Some(true);
    graph.close();
    Ok(!failed)
}

pub(crate) fn cmd_explore(query_parts: &[String], path_arg: Option<&str>, max_files: Option<&str>) {
    let mut args = Map::new();
    args.insert("query".to_string(), Value::String(query_parts.join(" ")));
    if let Some(max_files) = max_files.and_then(parse_int_js) {
        args.insert("maxFiles".to_string(), Value::from(max_files));
    }
    match execute_and_print("codegraph_explore", Value::Object(args), path_arg) {
        Ok(true) => {}
        Ok(false) => process::exit(1),
        Err(message) => {
            error_msg(&format!("Explore failed: {message}"));
            process::exit(1);
        }
    }
}

pub(crate) fn cmd_node(
    name: Option<&str>,
    path_arg: Option<&str>,
    file: Option<&str>,
    offset: Option<&str>,
    limit: Option<&str>,
    symbols_only: bool,
) {
    if name.is_none() && file.is_none() {
        error_msg(
            "Pass a symbol name (for example `codegraph node parseToken`) or a file (`codegraph node -f src/auth.ts`).",
        );
        process::exit(1);
    }

    let mut args = Map::new();
    match (name, file) {
        (Some(name), Some(file)) => {
            args.insert("file".to_string(), Value::String(file.to_string()));
            if name != file {
                args.insert("symbol".to_string(), Value::String(name.to_string()));
            }
        }
        (None, Some(file)) => {
            args.insert("file".to_string(), Value::String(file.to_string()));
        }
        (Some(name), None) if name.contains('/') || name.contains('\\') => {
            args.insert("file".to_string(), Value::String(name.replace('\\', "/")));
        }
        (Some(name), None) => {
            args.insert("symbol".to_string(), Value::String(name.to_string()));
            args.insert("includeCode".to_string(), Value::Bool(true));
        }
        (None, None) => unreachable!(),
    }
    if let Some(offset) = offset.and_then(parse_int_js) {
        args.insert("offset".to_string(), Value::from(offset));
    }
    if let Some(limit) = limit.and_then(parse_int_js) {
        args.insert("limit".to_string(), Value::from(limit));
    }
    if symbols_only {
        args.insert("symbolsOnly".to_string(), Value::Bool(true));
    }

    match execute_and_print("codegraph_node", Value::Object(args), path_arg) {
        Ok(true) => {}
        Ok(false) => process::exit(1),
        Err(message) => {
            error_msg(&format!("Node lookup failed: {message}"));
            process::exit(1);
        }
    }
}

fn discover_daemons(path_arg: Option<&str>) -> Vec<DaemonRecord> {
    let mut records = list_daemons(true);
    let root = resolve_project_path(path_arg);
    if let Some(current) = daemon_at(&root) {
        let existing = records.iter().any(|record| record.pid == current.pid);
        if !existing {
            records.push(current);
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    let mut seen = HashSet::new();
    records.retain(|record| seen.insert((record.pid, record.root.clone())));
    records
}

fn print_daemon(record: &DaemonRecord) {
    let uptime = now_ms().saturating_sub(record.started_at);
    println!(
        "pid {}  v{}  up {}  {}",
        record.pid,
        record.version,
        format_duration(uptime),
        record.root
    );
}

pub(crate) fn cmd_daemon(path_arg: Option<&str>, stop: bool, all: bool, json: bool) {
    if stop {
        let results = if all {
            stop_all_daemons()
        } else {
            vec![stop_daemon_at(&resolve_project_path(path_arg))]
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string())
            );
            return;
        }
        if results.is_empty() {
            info("No CodeGraph daemons running.");
            return;
        }
        for result in results {
            match result.outcome {
                StopOutcome::Term => success(&format!(
                    "Stopped daemon pid {} for {}",
                    result.pid.unwrap_or_default(),
                    result.root
                )),
                StopOutcome::Kill => success(&format!(
                    "Force-stopped daemon pid {} for {}",
                    result.pid.unwrap_or_default(),
                    result.root
                )),
                StopOutcome::NotRunning => {
                    info(&format!("Removed stale daemon state for {}", result.root))
                }
                StopOutcome::NoDaemon => info(&format!("No daemon running for {}", result.root)),
            }
        }
        return;
    }

    let records = discover_daemons(path_arg);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records).unwrap_or_else(|_| "[]".to_string())
        );
    } else if records.is_empty() {
        info("No CodeGraph daemons running.");
    } else {
        for record in &records {
            print_daemon(record);
        }
    }
}

pub(crate) fn cmd_telemetry(action: Option<&str>) {
    let telemetry = Telemetry::default();
    match action {
        Some("on") | Some("off") => {
            let enabled = action == Some("on");
            telemetry.set_enabled(enabled, "cli");
            if enabled {
                success(
                    "Telemetry enabled - anonymous usage stats only (no code, paths, or names).",
                );
            } else {
                success("Telemetry disabled. Buffered, unsent data was deleted.");
            }
            let status = telemetry.status();
            if matches!(
                status.decided_by,
                TelemetryDecision::DoNotTrack | TelemetryDecision::Environment
            ) {
                super::warn(&format!(
                    "The {} overrides this choice - effective state right now: {}.",
                    status.decided_by.as_str(),
                    if status.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));
            }
        }
        None | Some("status") => {
            let status = telemetry.status();
            println!(
                "\nTelemetry: {} ({})",
                if status.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                status.decided_by.as_str()
            );
            println!(
                "Machine ID: {}",
                status
                    .machine_id
                    .as_deref()
                    .unwrap_or("(random UUID, created on first use)")
            );
            println!("Config:     {}", status.config_path.display());
            println!("\nExactly what is collected (and never collected): {TELEMETRY_DOCS}\n");
        }
        Some(action) => {
            error_msg(&format!(
                "Unknown action: {action} (expected status, on, or off)"
            ));
            process::exit(1);
        }
    }
}

pub(crate) fn cmd_upgrade(version: Option<&str>, check: bool, force: bool) {
    process::exit(codegraph::upgrade::run_upgrade(version, check, force));
}

pub(crate) fn cmd_prompt_hook() {
    codegraph::prompt_hook::run_prompt_hook();
}
