use super::{
    Location,
    RunInstallerOptions,
    RunUninstallerOptions,
    Write,
    error_msg,
    get_target,
    io,
    list_target_ids,
    process,
    run_installer_with_options,
    run_uninstaller,
};

/// codegraph install
pub(crate) fn cmd_install(
    target: Option<String>,
    location: Option<String>,
    yes: bool,
    no_permissions: bool,
    print_config: Option<String>,
) {
    if let Some(id) = print_config {
        let Some(target) = get_target(&id) else {
            let known = list_target_ids()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            error_msg(&format!("Unknown target \"{id}\". Known: {known}."));
            process::exit(1);
        };
        let loc = if location.as_deref() == Some("local") {
            Location::Local
        } else {
            Location::Global
        };
        print!("{}", target.print_config(loc));
        let _ = io::stdout().flush();
        return;
    }

    if let Some(loc) = &location {
        if loc != "global" && loc != "local" {
            error_msg(&format!(
                "--location must be \"global\" or \"local\" (got \"{loc}\")."
            ));
            process::exit(1);
        }
    }

    // Commander's `--no-permissions` makes `opts.permissions === false`;
    // omitting the flag leaves it `true` (the positive-form default).
    // We MUST treat the default-true as "user did not override — let
    // the orchestrator prompt" and only forward an explicit `false`
    // (or `true` when --yes implies it). Otherwise the auto-allow
    // prompt is silently skipped on every interactive run.
    let auto_allow: Option<bool> = if no_permissions {
        Some(false)
    } else if yes {
        Some(true)
    } else {
        None
    };

    let opts = RunInstallerOptions {
        target,
        location: location.as_deref().map(|l| {
            if l == "local" {
                Location::Local
            } else {
                Location::Global
            }
        }),
        auto_allow,
        prompt_hook: None,
        yes,
    };

    if let Err(err) = run_installer_with_options(&opts) {
        error_msg(&err.to_string());
        process::exit(1);
    }
}

/// codegraph uninstall
///
/// Inverse of `install`. Removes the codegraph MCP server entry,
/// instructions block, and permissions from every agent (or a
/// `--target` subset). Prompts global-vs-local when not given. Does NOT
/// delete the `.codegraph/` index — that's `codegraph uninit`.
pub(crate) fn cmd_uninstall(target: Option<String>, location: Option<String>, yes: bool) {
    if let Some(loc) = &location {
        if loc != "global" && loc != "local" {
            error_msg(&format!(
                "--location must be \"global\" or \"local\" (got \"{loc}\")."
            ));
            process::exit(1);
        }
    }

    let opts = RunUninstallerOptions {
        target,
        location: location.as_deref().map(|l| {
            if l == "local" {
                Location::Local
            } else {
                Location::Global
            }
        }),
        yes,
    };

    if let Err(err) = run_uninstaller(&opts) {
        error_msg(&err.to_string());
        process::exit(1);
    }
}
