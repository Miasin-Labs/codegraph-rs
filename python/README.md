# codegraph-rs (Python bindings)

In-process Python access to the codegraph-rs engine: the same tools the MCP server exposes, with no server or subprocess.

```python
import codegraph_rs as cg
g = cg.CodeGraph("/path/to/repo")      # opens/indexes the project
g.help()                                # list tools + args
g.search(query="EngineHandle")          # tool methods are generated from the live schema
g.call("codegraph_explore", {"query": "..."})   # generic escape hatch
```

## Build

```bash
cd python
RUSTUP_TOOLCHAIN=stable maturin develop --release         # into the active venv
RUSTUP_TOOLCHAIN=stable maturin build --release -o dist    # abi3 wheel (CPython >= 3.9)
```

`RUSTUP_TOOLCHAIN=stable` is needed on this machine because the rustup default points to an uninstalled `rustc-master` toolchain.

Engine worker failures raise a Python exception instead of calling `process::exit`, so they won't kill the host interpreter/REPL (uses `EngineHandle::spawn_embedded`).
