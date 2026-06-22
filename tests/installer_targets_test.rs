//! Multi-target installer tests (port of `__tests__/installer-targets.test.ts`
//! plus `__tests__/installer.test.ts`).
//!
//! Each `AgentTarget` is exercised against the same contract:
//!   - `install` writes the expected files
//!   - re-running `install` is byte-identical (idempotent)
//!   - sibling MCP servers / unrelated config is preserved
//!   - `uninstall` reverses `install`
//!   - `print_config` returns parseable, non-empty content
//!
//! For agent-config destinations we redirect HOME to a tmpdir via the
//! `$HOME` (POSIX) / `%USERPROFILE%` (Windows) env vars, and CWD via
//! `std::env::set_current_dir` — same pattern as the TS suite. No real
//! `~/.claude/` etc. is ever touched.
//!
//! Env + cwd are process-global, so every test acquires a shared mutex
//! (the TS suite got the same serialization from vitest's
//! single-threaded per-file execution).

#[path = "installer_targets_test/support.rs"]
mod support;

#[path = "installer_targets_test/antigravity.rs"]
mod antigravity;
#[path = "installer_targets_test/claude.rs"]
mod claude;
#[path = "installer_targets_test/codex.rs"]
mod codex;
#[path = "installer_targets_test/config_writer.rs"]
mod config_writer;
#[path = "installer_targets_test/contract.rs"]
mod contract;
#[path = "installer_targets_test/cursor.rs"]
mod cursor;
#[path = "installer_targets_test/gemini.rs"]
mod gemini;
#[path = "installer_targets_test/hermes.rs"]
mod hermes;
#[path = "installer_targets_test/kiro.rs"]
mod kiro;
#[path = "installer_targets_test/legacy_hooks.rs"]
mod legacy_hooks;
#[path = "installer_targets_test/opencode.rs"]
mod opencode;
#[path = "installer_targets_test/registry.rs"]
mod registry;
#[path = "installer_targets_test/sweep.rs"]
mod sweep;
#[path = "installer_targets_test/toml.rs"]
mod toml;
