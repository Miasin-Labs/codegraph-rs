//! Multi-agent installer targets (port of `src/installer/targets/`).
//!
//! Each MCP-capable agent implements [`types::AgentTarget`]; the
//! registry in [`registry`] lists them all. Adding a new agent = one
//! new file here + one entry in `registry.rs`.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod gemini;
pub mod hermes;
pub mod kiro;
pub mod opencode;
pub mod registry;
pub mod shared;
pub mod toml;
pub mod types;

pub use registry::{
    ALL_TARGETS,
    TargetDetection,
    detect_all,
    get_target,
    list_target_ids,
    resolve_target_flag,
};
pub use types::{
    AgentTarget,
    DetectionResult,
    FileAction,
    FileWrite,
    InstallOptions,
    Location,
    TargetId,
    WriteResult,
};
