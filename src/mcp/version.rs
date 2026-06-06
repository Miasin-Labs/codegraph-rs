//! Resolved package version, computed once at module load.
//!
//! The version string is the rendezvous datum between cooperating daemon and
//! proxy processes: the daemon advertises its version in the hello line, and
//! the proxy refuses to share IPC across a mismatch (falls back to direct
//! mode). Keeping the resolution in one place avoids drift between the CLI
//! `--version` output and the daemon handshake.
//!
//! Port note: the TS implementation reads the bundled `package.json` at
//! runtime (two levels up from `dist/mcp/`), falling back to the sentinel
//! `"0.0.0-unknown"` when the package was unpacked oddly. In Rust the version
//! is baked in at compile time via `CARGO_PKG_VERSION`, so the read can never
//! fail and the sentinel is unreachable — same observable semantics for every
//! correctly-built binary: daemon and proxy built from the same crate version
//! always match, and a differently-versioned binary always mismatches.

/// The package version advertised in the daemon hello line and compared by
/// the proxy (TS: `CodeGraphPackageVersion`).
pub const CODEGRAPH_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
