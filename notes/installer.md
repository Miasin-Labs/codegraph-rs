# Installer module port notes

Port of `src/installer/**` (8 agent targets) → `rust/src/installer/**`.
All files compile clean; 80 integration tests (`tests/installer_targets_test.rs`)
plus 9 in-module unit tests pass.

## Public API surface (for the wiring wave)

`crate::installer` re-exports (mirrors TS `index.ts`):

- `run_installer() -> Result<()>`
- `run_installer_with_options(&RunInstallerOptions) -> Result<()>`
  - `RunInstallerOptions { target: Option<String>, location: Option<Location>, auto_allow: Option<bool>, yes: bool }`
- `run_uninstaller(&RunUninstallerOptions) -> Result<()>`
  - `RunUninstallerOptions { target: Option<String>, location: Option<Location>, yes: bool }`
- `uninstall_targets(&[&dyn AgentTarget], Location) -> Vec<UninstallReport>` (pure sweep, no prompts)
  - `UninstallReport { id: TargetId, display_name: String, status: UninstallStatus, removed_paths: Vec<PathBuf>, notes: Vec<String> }`
  - `UninstallStatus::{Removed, NotConfigured, Unsupported}`
- `offer_watch_fallback(&Path, yes: bool)` — **stub, see "Deferred" below**
- config-writer shims: `write_mcp_config(Location)`, `write_permissions(Location)`,
  `has_mcp_config(Location) -> bool`, `has_permissions(Location) -> bool`,
  `type InstallLocation = Location`

`crate::installer::targets` re-exports:

- trait `AgentTarget: Send + Sync` — `id() -> TargetId`, `display_name() -> &'static str`,
  `docs_url() -> Option<&'static str>`, `supports_location(Location) -> bool`,
  `detect(Location) -> DetectionResult`,
  `install(Location, &InstallOptions) -> WriteResult`,
  `uninstall(Location) -> WriteResult`,
  `print_config(Location) -> String`, `describe_paths(Location) -> Vec<PathBuf>`
- `ALL_TARGETS: [&'static dyn AgentTarget; 8]` (order: claude, cursor, codex,
  opencode, hermes, gemini, antigravity, kiro — keep stable)
- `get_target(&str) -> Option<&'static dyn AgentTarget>`
- `list_target_ids() -> Vec<TargetId>`
- `detect_all(Location) -> Vec<TargetDetection>`
- `resolve_target_flag(&str, Location) -> Result<Vec<&'static dyn AgentTarget>>`
  (errors with the exact TS message `Unknown --target id(s): … Known: …, plus 'auto' / 'all' / 'none'.`)
- types: `Location::{Global, Local}` (Display/FromStr "global"/"local"),
  `TargetId` (8-variant enum, `as_str()`), `DetectionResult`,
  `FileAction::{Created, Updated, Unchanged, Removed, NotFound, Kept}`
  (serde kebab-case → `"not-found"` wire parity), `FileWrite { path, action }`,
  `WriteResult { files, notes }`, `InstallOptions { auto_allow }`
- `targets::claude` additionally exports `write_mcp_entry`, `write_permissions_entry`,
  `cleanup_legacy_hooks`, `remove_instructions_entry` (used by the config-writer
  shims and tested directly, same as TS)
- `targets::shared`: `get_mcp_server_config()`, `get_codegraph_permissions()`,
  `read_json_file`, `atomic_write_file_sync`, `write_json_file`, `json_deep_equal`,
  `replace_or_append_marked_section`, `remove_marked_section`
- `targets::toml`: `serialize_toml_table_body`, `build_toml_table`,
  `upsert_toml_table`, `remove_toml_table`, `TomlValue::{String, Array}`,
  `TomlUpsertAction`, `TomlRemoveAction` — faithful port of the hand-rolled
  serializer (byte-equal output; siblings and `[[array-of-tables]]` preserved
  verbatim)
- `instructions_template`: `CODEGRAPH_SECTION_START` / `CODEGRAPH_SECTION_END`
  only (issue #529 — no instructions body; targets only STRIP legacy blocks)

## Deviations from TS

1. **TS `WriteResult['files'][].action` string union → `FileAction` enum**
   (serde kebab-case for wire parity). TS `Location`/`TargetId` unions → enums.
2. **`AgentTarget.install/uninstall` don't return `Result`** — like the TS
   versions, fs write failures inside targets are swallowed
   (`atomic_write_file_sync` returns `io::Result` but target call sites ignore
   it the way TS lets exceptions be the only signal; in practice the TS
   contract tests never exercise the throwing paths). If the wiring wave wants
   hard failures, change the call sites to propagate.
3. **`os.homedir()` parity** — `shared::home_dir()` reads `$HOME` (POSIX) /
   `%USERPROFILE%` (Windows) first, then `dirs::home_dir()`. This is what makes
   the test suite's HOME-redirect work, same trick as the TS suite.
4. **`readJsonFile` on valid-but-non-object JSON** (e.g. top-level array)
   returns `{}` instead of the parsed value. The TS version would return the
   array and then misbehave on property assignment; nothing downstream supports
   a non-object root.
5. **opencode JSONC**: TS used `jsonc-parser` `modify`/`applyEdits`; Rust uses
   the `jsonc-parser` crate's CST API (`CstRootNode`) — comments/formatting of
   untouched keys survive byte-for-byte (asserted in tests). Insert formatting
   of NEW keys may differ in whitespace details from the TS `modify`
   formatter, but re-runs are byte-identical (`unchanged` short-circuits before
   any write). The crate's `serde_json` feature is NOT enabled in Cargo.toml,
   so `parse_config` bridges `jsonc_parser::JsonValue` → `serde_json::Value` by
   hand (numbers go through f64, fine for config files).
   If opencode CST parse fails on a malformed non-empty file, the Rust port
   rebuilds from the minimal `$schema` seed (TS `modify` on garbage text has
   under-defined behavior; this is the defensive equivalent).
6. **`getVersion()`** — `env!("CARGO_PKG_VERSION")` instead of reading
   `package.json` at runtime.
7. **@clack/prompts UI → plain stdin/stdout prompts** in `install.rs`:
   - `confirm` → `message [Y/n]` line read; empty = default; EOF = cancel
     (mirrors clack's `isCancel` → "Installation cancelled." + exit 0).
   - `select` → numbered list + `Choice [n]:`; invalid input falls back to the
     initial value.
   - `multiselect` → checkbox-style listing + comma-separated numbers; empty
     input keeps the pre-checked (detected) defaults.
   - `spinner` → plain "Installing codegraph CLI..." line.
   - `log.success/info/warn` → `✔ / ℹ / ▲` prefixed println; `note()` → titled
     indented block. All message wording is preserved verbatim from TS.
8. **`tildify`** is string-prefix-based on `home_dir()` like the TS version.
9. **installer.test.ts "warn spy" assertion** can't be ported (no console spy
   in Rust) — the corrupted-JSON test instead asserts the `.backup` file +
   recovery behavior (the warning lines still go to stderr verbatim:
   `  Warning: Could not parse <basename>: <msg>` /
   `  A backup will be created before overwriting.`).
10. **Antigravity macOS `command -v` resolution** is runtime-gated with
    `cfg!(target_os = "macos")`; non-macOS always uses the bare `codegraph`,
    matching TS `process.platform !== 'darwin'`.

## Deferred / for the integrator (wiring wave)

1. **`initialize_local_project` (install.rs Step 6) is reduced**: TS loads the
   `CodeGraph` public API, runs `init()` + `indexAll()` with shimmer progress
   (`ui/shimmer-progress`, `ui/glyphs`), prints the indexed-file summary, then
   calls `offerWatchFallback`. Those modules are owned by other port waves and
   were stubs at port time. Current behavior: if `directory::is_initialized`
   → log "CodeGraph already initialized in this project"; else log
   `Skipping project initialization. Run "codegraph init -i" later.` (the TS
   fallback wording for the native-modules-missing path).
   Reconnect to: `CodeGraph::init`, `index_all`, shimmer progress, and
   `offer_watch_fallback` (TS `src/installer/index.ts` lines 446–493).
2. **`offer_watch_fallback` is an empty stub**: the full TS flow
   (`src/installer/index.ts` lines 504–564) needs
   `sync::watch_policy::watch_disabled_reason(&Path) -> Option<String>`,
   `sync::git_hooks::{is_git_repo, is_sync_hook_installed, install_git_sync_hook}`.
   All prompt wording is in the TS source; port the body verbatim once sync
   lands. (It's exported so the CLI can call it after `codegraph init`.)
3. **Step 2 "Install the codegraph CLI on your PATH?"** still shells out to
   `npm install -g @colbymchenry/codegraph` (faithful port). For a pure-Rust
   distribution the wiring wave may want to replace this with a self-install /
   copy-binary step — decide at CLI-integration time.
4. CLI flags mapping: `--target` → `RunInstallerOptions.target`,
   `--location` → `Location::from_str`, `--auto-allow` → `auto_allow`,
   `--yes`/`-y` → `yes`. `--print-config <id>` should call
   `get_target(id).print_config(loc)` (no fs writes — contract-tested).
5. `mcp/server_instructions.rs` is referenced in doc comments as the single
   source of truth for agent guidance (issue #529) — MCP wave owns it; nothing
   here writes instruction files anymore.

## Tests

- `tests/installer_targets_test.rs` — full port of
  `__tests__/installer-targets.test.ts` (all suites: 5 contract cases looped
  across all 8 targets × supported locations, partial-state idempotency,
  legacy hook cleanup, registry, TOML serializer, uninstall sweep, Cursor
  rules cleanup) + the 3 `__tests__/installer.test.ts` config-writer cases.
  **80 tests, all passing.**
- Env/cwd redirection is process-global → every test takes a shared
  `static ENV_MUTEX` (TS got the same serialization from vitest's
  per-file single thread). `TestEnv` RAII guard sets
  HOME/USERPROFILE/APPDATA/XDG_CONFIG_HOME, clears HERMES_HOME, chdirs into a
  canonicalized tempdir, and restores everything on drop.
  **If other waves add tests that mutate HOME or cwd, they must take the same
  serialization approach** (or these tests will race with theirs under the
  default multi-threaded test runner).
- TOML serializer + shared JSON helpers also have `#[cfg(test)]` unit tests
  in-module (9 tests).
