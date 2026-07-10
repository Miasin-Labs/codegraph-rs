//! CodeGraph Interactive Installer (port of `src/installer/index.ts`).
//!
//! Multi-target: writes MCP server config + instructions for the
//! agents the user picks (Claude Code, Cursor, Codex CLI, opencode,
//! Hermes Agent, Gemini CLI, Antigravity IDE, Kiro).
//! Defaults to the Claude-only behavior for backwards compatibility
//! when no targets are explicitly chosen and nothing else is detected.
//!
//! The TS version used `@clack/prompts` for the interactive UI; this
//! port uses simple stdin/stdout prompts with the same flow and
//! wording. `run_installer_with_options` is the non-interactive entry
//! point used by the `--target` / `--print-config` CLI flags.

use std::io::{self, BufRead, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::targets::registry::{ALL_TARGETS, detect_all, resolve_target_flag};
use super::targets::shared::{cwd, home_dir};
use super::targets::types::{AgentTarget, FileAction, InstallOptions, Location, TargetId};
use crate::error::Result;

fn get_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---------------------------------------------------------------------------
// Minimal prompt UI (replaces @clack/prompts; same flow & wording)
// ---------------------------------------------------------------------------

fn log_success(msg: &str) {
    println!("✔ {msg}");
}

fn log_info(msg: &str) {
    println!("ℹ {msg}");
}

fn log_warn(msg: &str) {
    println!("▲ {msg}");
}

fn intro(msg: &str) {
    println!("{msg}");
}

fn outro(msg: &str) {
    println!("{msg}");
}

fn note(body: &str, title: &str) {
    println!("{title}:");
    for line in body.lines() {
        println!("  {line}");
    }
}

fn cancel(msg: &str) -> ! {
    println!("{msg}");
    std::process::exit(0);
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) => None, // EOF — treated as cancel, same as clack's isCancel
        Ok(_) => Some(line.trim().to_string()),
        Err(_) => None,
    }
}

/// y/n confirm. `None` = cancelled (EOF / ctrl-d).
fn prompt_confirm(message: &str, initial: bool) -> Option<bool> {
    let hint = if initial { "Y/n" } else { "y/N" };
    print!("{message} [{hint}] ");
    let _ = io::stdout().flush();
    let answer = read_line()?;
    if answer.is_empty() {
        return Some(initial);
    }
    match answer.to_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => Some(initial),
    }
}

/// Numbered single-select. Returns the chosen index; `None` = cancelled.
fn prompt_select(message: &str, options: &[(String, String)], initial: usize) -> Option<usize> {
    println!("{message}");
    for (i, (label, hint)) in options.iter().enumerate() {
        let marker = if i == initial { ">" } else { " " };
        if hint.is_empty() {
            println!("  {marker} {}. {label}", i + 1);
        } else {
            println!("  {marker} {}. {label} ({hint})", i + 1);
        }
    }
    print!("Choice [{}]: ", initial + 1);
    let _ = io::stdout().flush();
    let answer = read_line()?;
    if answer.is_empty() {
        return Some(initial);
    }
    match answer.parse::<usize>() {
        Ok(n) if n >= 1 && n <= options.len() => Some(n - 1),
        _ => Some(initial),
    }
}

/// Numbered multi-select (comma-separated). Empty input keeps the
/// pre-checked defaults. `None` = cancelled.
fn prompt_multiselect(
    message: &str,
    options: &[(String, String)],
    initial: &[usize],
) -> Option<Vec<usize>> {
    println!("{message}");
    for (i, (label, _)) in options.iter().enumerate() {
        let marker = if initial.contains(&i) { "[x]" } else { "[ ]" };
        println!("  {marker} {}. {label}", i + 1);
    }
    print!("Comma-separated numbers (empty keeps the checked defaults): ");
    let _ = io::stdout().flush();
    let answer = read_line()?;
    if answer.is_empty() {
        return Some(initial.to_vec());
    }
    let picked: Vec<usize> = answer
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1 && *n <= options.len())
        .map(|n| n - 1)
        .collect();
    Some(picked)
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct RunInstallerOptions {
    /// Comma-separated target list, or `auto` / `all` / `none`.
    pub target: Option<String>,
    /// Skip the location prompt; use this value directly.
    pub location: Option<Location>,
    /// Skip the auto-allow prompt; use this value directly.
    pub auto_allow: Option<bool>,
    /// Skip the Claude prompt-hook prompt; install/remove when set.
    /// `None` asks interactively when Claude is selected, except `--yes`
    /// which enables the hook by default.
    pub prompt_hook: Option<bool>,
    /// Skip every confirm and use defaults: location=global,
    /// auto_allow=true, prompt_hook=true, target=auto. For scripting / CI.
    pub yes: bool,
}

/// Interactive entry point — preserves the historical UX (`codegraph
/// install` with no args goes through the prompts), but now starts
/// the targets multi-select pre-populated with detected agents.
pub fn run_installer() -> Result<()> {
    run_installer_with_options(&RunInstallerOptions::default())
}

pub fn run_installer_with_options(opts: &RunInstallerOptions) -> Result<()> {
    intro(&format!("CodeGraph v{}", get_version()));

    // --yes implies all defaults; explicit flags still win.
    let use_defaults = opts.yes;

    // Step 1: which agent targets? Asked FIRST so the user knows what
    // they're committing to before we touch npm or disk. Detection
    // probes the user-provided location if known, else 'global' as the
    // most common default — labels are a hint, not load-bearing.
    let detection_location = opts.location.unwrap_or(Location::Global);
    let targets = resolve_targets(opts, detection_location, use_defaults)?;
    if targets.is_empty() {
        outro("No agent targets selected — nothing to do.");
        return Ok(());
    }

    // Step 2: install the codegraph npm package on PATH (always offered;
    // matches existing behavior). Skipped when --yes (assume present).
    if !use_defaults {
        let should_install_globally = prompt_confirm(
            "Install the codegraph CLI on your PATH? (Required so agents can launch the MCP server)",
            true,
        );
        let should_install_globally = match should_install_globally {
            None => cancel("Installation cancelled."),
            Some(v) => v,
        };
        if should_install_globally {
            println!("Installing codegraph CLI...");
            let result = Command::new("npm")
                .args(["install", "-g", "@colbymchenry/codegraph"])
                .output();
            match result {
                Ok(out) if out.status.success() => {
                    log_success("Installed codegraph CLI on PATH");
                }
                _ => {
                    println!("Could not install (permission denied)");
                    log_warn("Try: sudo npm install -g @colbymchenry/codegraph");
                }
            }
        } else {
            log_info(
                "Skipped CLI install — agents will not be able to launch the MCP server without it",
            );
        }
    }

    // Step 3: where the per-agent config files should land.
    let location: Location = if let Some(loc) = opts.location {
        loc
    } else if use_defaults {
        Location::Global
    } else {
        // If every selected target is global-only (e.g. Codex), skip the
        // prompt and force user-wide — project-local would just produce
        // skip warnings.
        let all_global_only = targets
            .iter()
            .all(|t| !t.supports_location(Location::Local));
        if all_global_only {
            log_info("Writing user-wide configs (selected agents have no project-local config).");
            Location::Global
        } else {
            let sel = prompt_select(
                "Apply agent configs to all your projects, or just this one?",
                &[
                    (
                        "All projects".to_string(),
                        "~/.claude, ~/.cursor, etc.".to_string(),
                    ),
                    (
                        "Just this project".to_string(),
                        "./.claude, ./.cursor, etc.".to_string(),
                    ),
                ],
                0,
            );
            match sel {
                None => cancel("Installation cancelled."),
                Some(0) => Location::Global,
                Some(_) => Location::Local,
            }
        }
    };

    // Step 4: auto-allow permissions (only meaningful for Claude;
    // skipped silently by other targets).
    let auto_allow: bool = if let Some(v) = opts.auto_allow {
        v
    } else if use_defaults {
        true
    } else if targets.iter().any(|t| t.id() == TargetId::Claude) {
        let ans = prompt_confirm(
            "Auto-allow CodeGraph commands? (Skips permission prompts in Claude Code)",
            true,
        );
        match ans {
            None => cancel("Installation cancelled."),
            Some(v) => v,
        }
    } else {
        false
    };

    // Step 4b: front-load CodeGraph for structural Claude prompts. Claude's
    // UserPromptSubmit hook runs `codegraph prompt-hook`; other targets ignore
    // the option. An explicit false removes a hook written by a prior install.
    let prompt_hook: Option<bool> = if let Some(v) = opts.prompt_hook {
        Some(v)
    } else if targets.iter().any(|t| t.id() == TargetId::Claude) {
        if use_defaults {
            Some(true)
        } else {
            let ans = prompt_confirm(
                "Front-load CodeGraph on how / where / trace prompts? Auto-injects structural context so answers need fewer steps (adds a moment to those prompts; Claude Code only).",
                true,
            );
            match ans {
                None => cancel("Installation cancelled."),
                Some(v) => Some(v),
            }
        }
    } else {
        None
    };

    // Step 5: per-target install loop.
    for target in &targets {
        if !target.supports_location(location) {
            log_warn(&format!(
                "{}: skipped — does not support --location={location}.",
                target.display_name(),
            ));
            continue;
        }
        let result = target.install(
            location,
            &InstallOptions {
                auto_allow,
                prompt_hook,
            },
        );
        for file in &result.files {
            let verb = match file.action {
                FileAction::Unchanged => "Unchanged",
                FileAction::Created => "Created",
                FileAction::Removed => "Removed",
                _ => "Updated",
            };
            log_success(&format!(
                "{}: {verb} {}",
                target.display_name(),
                tildify(&file.path),
            ));
        }
        for note_line in &result.notes {
            log_info(&format!("{}: {note_line}", target.display_name()));
        }
    }

    // Step 6: for local install, initialize the project.
    if location == Location::Local {
        initialize_local_project(use_defaults);
    }

    if location == Location::Global {
        note("cd your-project\ncodegraph init", "Quick start");
    }

    let final_note = if !targets.is_empty() {
        format!(
            "Done! Restart your agent{} to use CodeGraph.",
            if targets.len() > 1 { "s" } else { "" }
        )
    } else {
        "Done!".to_string()
    };
    outro(&final_note);
    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct RunUninstallerOptions {
    /// Comma-separated target list, or `auto` / `all` / `none`. Defaults
    /// to `all` — uninstall sweeps every known agent and reports which
    /// ones it actually touched, so the user doesn't have to know where
    /// they configured it.
    pub target: Option<String>,
    /// Skip the location prompt; use this value directly.
    pub location: Option<Location>,
    /// Non-interactive: location=global, target=all, no prompts.
    pub yes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UninstallStatus {
    Removed,
    NotConfigured,
    Unsupported,
}

/// Per-target outcome of an uninstall sweep. `Removed` means we deleted
/// at least one thing; `NotConfigured` means the agent had no codegraph
/// config at this location (nothing to do); `Unsupported` means the
/// agent has no config concept for this location (e.g. Codex is
/// global-only, so a `local` uninstall skips it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallReport {
    pub id: TargetId,
    pub display_name: String,
    pub status: UninstallStatus,
    /// Absolute paths we actually edited/removed (action == Removed).
    pub removed_paths: Vec<PathBuf>,
    /// Verbatim notes from the target (rare for uninstall).
    pub notes: Vec<String>,
}

/// Pure uninstall sweep — no prompts, no I/O beyond the targets' own
/// file edits. Exposed (and unit-tested) separately from the prompt UI in
/// `run_uninstaller` so the aggregation logic can be asserted directly.
///
/// Each target's `uninstall()` is already safe to call when nothing was
/// installed (it returns `not-found` actions), so this is safe to run
/// across every target unconditionally.
pub fn uninstall_targets(targets: &[&dyn AgentTarget], location: Location) -> Vec<UninstallReport> {
    targets
        .iter()
        .map(|target| {
            if !target.supports_location(location) {
                let only = match location {
                    Location::Local => Location::Global,
                    Location::Global => Location::Local,
                };
                return UninstallReport {
                    id: target.id(),
                    display_name: target.display_name().to_string(),
                    status: UninstallStatus::Unsupported,
                    removed_paths: Vec::new(),
                    notes: vec![format!("no {location} config — this agent is {only}-only")],
                };
            }
            let result = target.uninstall(location);
            let removed_paths: Vec<PathBuf> = result
                .files
                .iter()
                .filter(|f| f.action == FileAction::Removed)
                .map(|f| f.path.clone())
                .collect();
            UninstallReport {
                id: target.id(),
                display_name: target.display_name().to_string(),
                status: if !removed_paths.is_empty() {
                    UninstallStatus::Removed
                } else {
                    UninstallStatus::NotConfigured
                },
                removed_paths,
                notes: result.notes,
            }
        })
        .collect()
}

/// Interactive uninstaller — the inverse of `run_installer_with_options`.
/// Asks global-vs-local first (unless `--location`/`--yes` is given),
/// then sweeps every agent target (or the `--target` subset) and prints
/// one block per agent so the user sees exactly which providers it hit.
///
/// Removes only what install wrote (MCP server entry, instructions
/// block, permissions) — never the `.codegraph/` index, which `codegraph
/// uninit` owns.
pub fn run_uninstaller(opts: &RunUninstallerOptions) -> Result<()> {
    intro(&format!("CodeGraph v{} — uninstall", get_version()));

    let use_defaults = opts.yes;

    // Step 1: which location — asked FIRST, the one decision the user
    // must make. Global sweeps ~/.claude, ~/.codex, etc.; local sweeps
    // the configs in this project directory.
    let location: Location = if let Some(loc) = opts.location {
        loc
    } else if use_defaults {
        Location::Global
    } else {
        let sel = prompt_select(
            "Remove CodeGraph from all your projects, or just this one?",
            &[
                (
                    "All projects (global)".to_string(),
                    "~/.claude, ~/.cursor, ~/.codex, ~/.config/opencode, ~/.hermes, ~/.gemini, ~/.kiro"
                        .to_string(),
                ),
                (
                    "Just this project (local)".to_string(),
                    "./.claude, ./.cursor, ./opencode.jsonc, ./.gemini, ./.kiro".to_string(),
                ),
            ],
            0,
        );
        match sel {
            None => cancel("Uninstall cancelled."),
            Some(0) => Location::Global,
            Some(_) => Location::Local,
        }
    };

    // Step 2: which agents. Default is every agent, so the user doesn't
    // have to remember where they installed it — unconfigured agents are
    // reported as "nothing to remove" and left untouched. An explicit
    // --target subsets this.
    let targets: Vec<&'static dyn AgentTarget> = match &opts.target {
        Some(value) => resolve_target_flag(value, location)?,
        None => ALL_TARGETS.to_vec(),
    };
    if targets.is_empty() {
        outro("No agent targets selected — nothing to do.");
        return Ok(());
    }

    // Step 3: sweep + per-agent feedback.
    let reports = uninstall_targets(&targets, location);
    let removed: Vec<&UninstallReport> = reports
        .iter()
        .filter(|r| r.status == UninstallStatus::Removed)
        .collect();

    for r in &reports {
        match r.status {
            UninstallStatus::Removed => {
                for p in &r.removed_paths {
                    log_success(&format!("{}: removed {}", r.display_name, tildify(p)));
                }
            }
            UninstallStatus::NotConfigured => {
                log_info(&format!(
                    "{}: not configured — nothing to remove",
                    r.display_name
                ));
            }
            UninstallStatus::Unsupported => {
                log_info(&format!(
                    "{}: skipped — {}",
                    r.display_name,
                    r.notes
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("unsupported location"),
                ));
            }
        }
    }

    // Step 4: for local uninstall, the index dir is separate — point at
    // `uninit` so the user knows it's still there (and how to remove it).
    let data_dir = crate::directory::codegraph_dir_name();
    if location == Location::Local && cwd().join(&data_dir).exists() {
        log_info(&format!(
            "The {data_dir}/ index for this project is still here. Run `codegraph uninit` to delete it."
        ));
    }

    // Step 5: summary.
    if !removed.is_empty() {
        let names = removed
            .iter()
            .map(|r| r.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        outro(&format!(
            "Removed CodeGraph from {} agent{}: {}. Restart {} to apply.",
            removed.len(),
            if removed.len() > 1 { "s" } else { "" },
            names,
            if removed.len() > 1 { "them" } else { "it" },
        ));
    } else {
        outro(&format!(
            "CodeGraph was not configured in any {location} agent — nothing to remove."
        ));
    }
    Ok(())
}

/// Replace home-directory prefix in a path with `~/` for cleaner log
/// lines. Pure cosmetic.
fn tildify(p: &Path) -> String {
    let home = home_dir();
    let p_str = p.display().to_string();
    let home_prefix = format!("{}{}", home.display(), std::path::MAIN_SEPARATOR);
    if p_str.starts_with(&home_prefix) {
        format!("~{}", &p_str[home.display().to_string().len()..])
    } else {
        p_str
    }
}

fn resolve_targets(
    opts: &RunInstallerOptions,
    location: Location,
    use_defaults: bool,
) -> Result<Vec<&'static dyn AgentTarget>> {
    // Explicit --target flag wins.
    if let Some(value) = &opts.target {
        return resolve_target_flag(value, location);
    }

    // --yes implies auto-detect.
    if use_defaults {
        return resolve_target_flag("auto", location);
    }

    // Interactive multi-select.
    let detected = detect_all(location);
    let initial_values: Vec<usize> = detected
        .iter()
        .enumerate()
        .filter(|(_, d)| d.detection.installed)
        .map(|(i, _)| i)
        .collect();
    // If nothing detected, default to Claude alone (matches the
    // historical default and the smallest-surprise outcome).
    let initial: Vec<usize> = if !initial_values.is_empty() {
        initial_values
    } else {
        ALL_TARGETS
            .iter()
            .enumerate()
            .filter(|(_, t)| t.id() == TargetId::Claude)
            .map(|(i, _)| i)
            .collect()
    };

    let options: Vec<(String, String)> = ALL_TARGETS
        .iter()
        .map(|t| {
            let det = detected
                .iter()
                .find(|d| d.target.id() == t.id())
                .map(|d| d.detection.installed)
                .unwrap_or(false);
            let flag = if det { "(detected)" } else { "(not found)" };
            let global_only = if !t.supports_location(Location::Local) {
                " — global only"
            } else {
                ""
            };
            (
                format!("{} {flag}{global_only}", t.display_name()),
                String::new(),
            )
        })
        .collect();

    let choice = prompt_multiselect(
        "Which agents should CodeGraph configure?",
        &options,
        &initial,
    );
    let choice = match choice {
        None => cancel("Installation cancelled."),
        Some(c) => c,
    };

    Ok(choice
        .into_iter()
        .filter_map(|i| ALL_TARGETS.get(i).copied())
        .collect())
}

/// Initialize CodeGraph in the current project (for local installs).
///
/// NOTE (port): the TS version loads the `CodeGraph` public API to
/// `init()` + `indexAll()` with shimmer progress, then offers the
/// watch fallback (git sync hooks) when the live watcher is disabled
/// (`offerWatchFallback` in `src/installer/index.ts`). Those modules
/// (`CodeGraph` public API, `sync/watch-policy`, `sync/git-hooks`,
/// `ui/shimmer-progress`) are owned by other port waves — the wiring
/// task must reconnect this. Until then we point the user at
/// `codegraph init`, mirroring the TS fallback when native modules
/// can't load.
fn initialize_local_project(_use_defaults: bool) {
    let project_path = cwd();
    if crate::directory::is_initialized(&project_path) {
        log_info("CodeGraph already initialized in this project");
        return;
    }
    // TODO(wiring): CodeGraph::init + index_all + offer_watch_fallback.
    log_info("Skipping project initialization. Run \"codegraph init\" later.");
}

/// Stale-index fallback for environments where the live file watcher is
/// disabled (WSL2 /mnt drives, CODEGRAPH_NO_WATCH).
///
/// NOT YET PORTED — depends on `sync::watch_policy::watch_disabled_reason`,
/// `sync::git_hooks::{is_git_repo, is_sync_hook_installed,
/// install_git_sync_hook}`, owned by the sync port wave. See
/// `notes/installer.md` for the full TS flow to reconnect
/// (`offerWatchFallback`, `src/installer/index.ts` lines 504-564).
pub fn offer_watch_fallback(_project_path: &Path, _yes: bool) {
    // TODO(wiring): port body once sync module lands.
}
