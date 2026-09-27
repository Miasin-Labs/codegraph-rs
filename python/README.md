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

## Results

Every call returns a `Result`: `str(r)` is the human text, `r.data` the
structured payload (the tool's declared output schema), and iterating a
result yields its rows as dicts:

```python
for c in g.callers("EngineHandle::spawn"):      # {name, kind, file, line}
    print(c["file"], c["line"], c["name"])
hits = [row for row in g.impact("spawn") if row["file"].startswith("src/mcp/")]
g.callers("x").data["otherProjects"]           # cross-project sections
```

`r.rows` flattens every shape: file-grouped payloads (`impact`, `tests`, `diagnostics`,
`arch`) get each row's `file`, `xref` references get their `edgeKind` and
`target`, and `history`/`paths` yield their pairs/steps; `r.results` is the
raw `results` array of `search`/`callers`/`callees`. Where the budget cut a
list, `truncated` is set and the matching `…Omitted` field counts every row
left out, so listed + omitted always equals the total.
