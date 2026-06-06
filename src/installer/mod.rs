//! CodeGraph installer (port of `src/installer/`).
//!
//! Multi-target: writes MCP server config + permissions for the agents
//! the user picks. The public surface mirrors the TS `index.ts`
//! re-exports.

pub mod config_writer;
pub mod install;
pub mod instructions_template;
pub mod targets;

// Backwards-compat: keep these named exports — downstream code may
// import them. The shim in `config_writer.rs` continues to re-export
// them too.
pub use config_writer::{
    InstallLocation,
    has_mcp_config,
    has_permissions,
    write_mcp_config,
    write_permissions,
};
pub use install::{
    RunInstallerOptions,
    RunUninstallerOptions,
    UninstallReport,
    UninstallStatus,
    offer_watch_fallback,
    run_installer,
    run_installer_with_options,
    run_uninstaller,
    uninstall_targets,
};
