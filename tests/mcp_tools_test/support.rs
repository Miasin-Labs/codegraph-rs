pub(crate) use std::fs;
pub(crate) use std::path::Path;
pub(crate) use std::rc::Rc;

pub(crate) use codegraph::mcp::tools::{
    ToolHandler,
    get_explore_budget,
    get_explore_output_budget,
    get_static_tools,
    tools,
};
pub(crate) use codegraph::{CodeGraph, EdgeKind, IndexOptions, NodeKind};
pub(crate) use serde_json::json;
pub(crate) use tempfile::TempDir;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static ENV_LOCK: RwLock<()> = RwLock::const_new(());

pub(crate) async fn env_read() -> RwLockReadGuard<'static, ()> {
    ENV_LOCK.read().await
}

pub(crate) async fn env_write() -> RwLockWriteGuard<'static, ()> {
    ENV_LOCK.write().await
}

pub(crate) struct EnvVarGuard {
    key: String,
    original: Option<String>,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }

    pub(crate) fn unset(key: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::remove_var(key);
        EnvVarGuard {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

pub(crate) fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub(crate) fn section_for(text: &str, basename: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines
        .iter()
        .position(|l| l.starts_with("#### ") && l.contains(basename))
    else {
        return String::new();
    };
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        if l.starts_with("### ") || l.starts_with("#### ") {
            end = i;
            break;
        }
    }
    lines[start..end].join("\n")
}
