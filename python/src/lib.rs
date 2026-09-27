//! Python bindings for the codegraph-rs MCP engine.
//!
//! Wraps [`EngineHandle`] (the same engine the MCP server drives) so tools can
//! be called in-process from Python without spawning `codegraph serve`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use codegraph::mcp::engine::{EngineHandle, MCPEngineOptions};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("codegraph-py-rt")
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for codegraph")
    })
}

/// Low-level engine handle. Prefer the `codegraph_rs.CodeGraph` wrapper.
#[pyclass(module = "codegraph_rs._native", frozen)]
struct Engine {
    inner: Arc<EngineHandle>,
    fatal: Arc<Mutex<Option<String>>>,
    closed: AtomicBool,
}

impl Engine {
    fn check(&self) -> PyResult<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(PyRuntimeError::new_err("codegraph engine is closed"));
        }
        if let Some(reason) = self.fatal.lock().unwrap().clone() {
            return Err(PyRuntimeError::new_err(format!(
                "codegraph engine worker failed: {reason}"
            )));
        }
        Ok(())
    }
}

#[pymethods]
impl Engine {
    /// Create an engine. `project` (optional) is a path inside an indexed
    /// project (one containing `.codegraph/`); the engine searches upward.
    #[new]
    #[pyo3(signature = (project=None, watch=false))]
    fn new(py: Python<'_>, project: Option<String>, watch: bool) -> PyResult<Self> {
        let fatal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let fatal_cb = fatal.clone();
        let handle = runtime().handle().clone();
        let inner =
            EngineHandle::spawn_embedded(MCPEngineOptions { watch }, handle, move |reason| {
                *fatal_cb.lock().unwrap() = Some(reason.to_string());
            });
        let engine = Engine {
            inner: Arc::new(inner),
            fatal,
            closed: AtomicBool::new(false),
        };
        if let Some(p) = project {
            engine.open(py, p)?;
        }
        Ok(engine)
    }

    /// Point the engine at a project (searches upward from `path` for `.codegraph/`).
    fn open(&self, py: Python<'_>, path: String) -> PyResult<Option<String>> {
        self.check()?;
        let inner = self.inner.clone();
        let p = path.clone();
        py.detach(move || {
            inner.set_project_path_hint(&p);
            inner.ensure_initialized(&p);
        });
        self.check()?;
        if !self.inner.has_default_code_graph() {
            return Err(PyValueError::new_err(format!(
                "no codegraph index found at or above {path:?} (run `codegraph init` / `codegraph index` there first)"
            )));
        }
        Ok(self.inner.get_project_path())
    }

    /// Project root currently loaded, if any.
    #[getter]
    fn project_path(&self) -> Option<String> {
        self.inner.get_project_path()
    }

    /// True once a project index is loaded.
    #[getter]
    fn is_open(&self) -> bool {
        self.inner.has_default_code_graph()
    }

    /// Tool definitions as a JSON string (list of {name, description, inputSchema...}).
    fn tools_json(&self) -> PyResult<String> {
        self.check()?;
        serde_json::to_string(&self.inner.get_tools())
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    /// Execute a tool by name. `args_json` is a JSON object string.
    /// Returns the ToolResult as a JSON string. Ctrl-C cancels the call.
    fn call_json(&self, py: Python<'_>, name: String, args_json: String) -> PyResult<String> {
        self.check()?;
        let args: serde_json::Value = serde_json::from_str(&args_json)
            .map_err(|e| PyValueError::new_err(format!("invalid args JSON: {e}")))?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let inner = self.inner.clone();
        let cancel_w = cancel.clone();
        std::thread::Builder::new()
            .name("codegraph-py-call".into())
            .spawn(move || {
                let r = inner.execute_with_context(&name, args, None, Some(cancel_w));
                let _ = tx.send(r);
            })
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        loop {
            match py.detach(|| rx.recv_timeout(Duration::from_millis(100))) {
                Ok(result) => {
                    return serde_json::to_string(&result)
                        .map_err(|e| PyRuntimeError::new_err(e.to_string()));
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if let Err(e) = py.check_signals() {
                        cancel.store(true, Ordering::SeqCst);
                        return Err(e);
                    }
                    self.check()?;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    self.check()?;
                    return Err(PyRuntimeError::new_err("codegraph call thread died"));
                }
            }
        }
    }

    /// Stop the engine worker. Further calls raise.
    fn close(&self, py: Python<'_>) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let inner = self.inner.clone();
            py.detach(move || inner.stop());
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.inner.stop();
        }
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
