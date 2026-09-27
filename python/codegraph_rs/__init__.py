"""In-process Python bindings for codegraph-rs.

    >>> import codegraph_rs as cg
    >>> g = cg.open(".")                      # finds .codegraph/ upward
    >>> print(g.search("EngineHandle"))       # text, same as the MCP tool
    >>> g.search("EngineHandle").data         # structuredContent dict
    >>> [c["name"] for c in g.callers("spawn", limit=5)]   # structured rows
    >>> for row in g.impact("spawn"): print(row["file"], row["name"])
    >>> g.explore("how does indexing work")
    >>> g.call("codegraph_grep", pattern="TODO")

Every MCP tool ``codegraph_<name>`` is exposed as ``g.<name>(...)``. Positional
arguments fill the tool's *required* parameters in schema order; everything
else is a keyword argument (``g.help("search")`` shows the schema).
"""
from __future__ import annotations

import json
from typing import Any, Dict, Iterator, List, Optional

from ._native import Engine, __version__

__all__ = ["CodeGraph", "Result", "CodeGraphError", "open", "__version__"]

_PREFIX = "codegraph_"


class CodeGraphError(RuntimeError):
    """A tool returned ``isError: true``. ``.result`` holds the full Result."""

    def __init__(self, result: "Result"):
        super().__init__(result.text or "codegraph tool error")
        self.result = result


class Result:
    """One tool result. ``str(r)`` gives the text; ``r.data`` the structured payload."""

    __slots__ = ("tool", "raw")

    def __init__(self, tool: str, raw: Dict[str, Any]):
        self.tool = tool
        self.raw = raw

    @property
    def text(self) -> str:
        return "\n".join(
            c.get("text", "") for c in self.raw.get("content", []) if c.get("type") == "text"
        )

    @property
    def data(self) -> Optional[Dict[str, Any]]:
        return self.raw.get("structuredContent")

    @property
    def is_error(self) -> bool:
        return bool(self.raw.get("isError"))

    @property
    def meta(self) -> Dict[str, Any]:
        return self.raw.get("_meta") or {}

    @property
    def notices(self) -> List[Dict[str, Any]]:
        return list(self.meta.get("notices", []))

    @property
    def results(self) -> List[Dict[str, Any]]:
        """Shortcut for ``data["results"]`` (search-like tools); ``[]`` otherwise."""
        d = self.data or {}
        r = d.get("results")
        return r if isinstance(r, list) else []

    @property
    def rows(self) -> List[Dict[str, Any]]:
        """The payload's rows as flat dicts, whatever its shape:

        - ``results`` as-is (search, callers, callees);
        - file groups (impact, tests, diagnostics, arch:
          ``files: [{file, symbols|tests|diagnostics: [...]}]``), each row
          carrying its ``file``;
        - xref's references, each with its ``edgeKind`` and the ``target``
          definition (``name``/``file``/``line``) it references;
        - history's co-change ``pairs`` and paths' ``steps``.

        ``[]`` for payloads without rows (status, a miss)."""
        d = self.data or {}
        r = d.get("results")
        if isinstance(r, list):
            return r
        kind = d.get("kind")
        if kind == "xref":
            out: List[Dict[str, Any]] = []
            for target in d.get("definitions") or []:
                ref_target = {k: target.get(k) for k in ("name", "kind", "file", "line")}
                for group in target.get("byKind") or []:
                    for ref in group.get("references") or []:
                        out.append({**ref, "edgeKind": group.get("edgeKind"), "target": ref_target})
            return out
        for key in ("pairs", "steps"):
            if isinstance(d.get(key), list):
                return d[key]
        out = []
        for group in d.get("files") or []:
            if not isinstance(group, dict):
                continue
            for key, items in group.items():
                if key == "file" or not isinstance(items, list):
                    continue
                out.extend({**item, "file": group.get("file")} for item in items if isinstance(item, dict))
        return out

    def __iter__(self) -> Iterator[Dict[str, Any]]:
        return iter(self.rows)

    def __len__(self) -> int:
        return len(self.rows)

    def __getitem__(self, key):
        if isinstance(key, str):
            return (self.data or {})[key]
        return self.rows[key]

    def __str__(self) -> str:
        return self.text

    def __repr__(self) -> str:
        head = self.text.splitlines()[0] if self.text else ""
        flag = " ERROR" if self.is_error else ""
        return f"<Result {self.tool}{flag}: {head[:80]!r}>"

    def _repr_pretty_(self, p, cycle):  # IPython
        p.text(self.text)


class CodeGraph:
    """An in-process codegraph engine bound to one indexed project."""

    def __init__(self, path: Optional[str] = ".", *, watch: bool = False, check: bool = True):
        """``path``: any directory inside a project that has ``.codegraph/``
        (``None`` = don't open yet; call ``.open(path)``). ``watch``: enable the
        live file watcher. ``check``: raise CodeGraphError on tool errors."""
        import os

        self.check = check
        self._engine = Engine(None, watch)
        self._tools: Optional[Dict[str, Dict[str, Any]]] = None
        if path is not None:
            self.open(os.path.abspath(os.path.expanduser(path)))

    # -- lifecycle -------------------------------------------------------
    def open(self, path: str) -> str:
        import os

        return self._engine.open(os.path.abspath(os.path.expanduser(path)))

    @property
    def root(self) -> Optional[str]:
        return self._engine.project_path

    def close(self) -> None:
        self._engine.close()

    def __enter__(self) -> "CodeGraph":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"<CodeGraph root={self.root!r}>"

    # -- tools -----------------------------------------------------------
    @property
    def tools(self) -> Dict[str, Dict[str, Any]]:
        """Tool definitions keyed by full name (``codegraph_search`` ...)."""
        if self._tools is None:
            self._tools = {t["name"]: t for t in json.loads(self._engine.tools_json())}
        return self._tools

    def _full(self, name: str) -> str:
        return name if name.startswith(_PREFIX) else _PREFIX + name

    def call(self, name: str, args: Optional[Dict[str, Any]] = None, /, **kwargs: Any) -> Result:
        """Call a tool by name (with or without the ``codegraph_`` prefix)."""
        full = self._full(name)
        payload = dict(args or {})
        payload.update({k: v for k, v in kwargs.items() if v is not None})
        res = Result(full, json.loads(self._engine.call_json(full, json.dumps(payload))))
        if res.is_error and self.check:
            raise CodeGraphError(res)
        return res

    def help(self, name: Optional[str] = None) -> None:
        """Print tool list, or one tool's description + parameters."""
        if name is None:
            out = "\n".join(
                f"{n[len(_PREFIX):]:<12} {t.get('description', '').splitlines()[0][:100]}"
                for n, t in self.tools.items()
            )
        else:
            t = self.tools[self._full(name)]
            schema = t.get("inputSchema", {})
            req = set(schema.get("required", []))
            lines = [t["name"], "", t.get("description", ""), "", "params:"]
            for p, s in schema.get("properties", {}).items():
                typ = s.get("type", "")
                if isinstance(typ, list):
                    typ = "|".join(typ)
                desc = (s.get("description") or "").splitlines()[0][:110] if s.get("description") else ""
                lines.append(f"  {p}{'*' if p in req else ''} ({typ}) {desc}")
            out = "\n".join(lines)
        print(out)

    def _make(self, full: str):
        schema = self.tools[full].get("inputSchema", {})
        required = list(schema.get("required", []))
        props = list(schema.get("properties", {}).keys())
        positional = required or props[:1]

        def method(*args: Any, **kwargs: Any) -> Result:
            if len(args) > len(positional):
                raise TypeError(
                    f"{full[len(_PREFIX):]}() takes at most {len(positional)} positional "
                    f"argument(s) ({', '.join(positional)}); use keywords for the rest"
                )
            for k, v in zip(positional, args):
                if k in kwargs:
                    raise TypeError(f"got multiple values for {k!r}")
                kwargs[k] = v
            return self.call(full, **kwargs)

        method.__name__ = full[len(_PREFIX):]
        method.__doc__ = self.tools[full].get("description", "")
        return method

    def __getattr__(self, attr: str):
        if attr.startswith("_"):
            raise AttributeError(attr)
        full = _PREFIX + attr
        if full in self.tools:
            m = self._make(full)
            object.__setattr__(self, attr, m)  # cache
            return m
        raise AttributeError(f"{type(self).__name__} has no tool {attr!r} (see .help())")

    def __dir__(self):
        return sorted(set(super().__dir__()) | {n[len(_PREFIX):] for n in self.tools})


def open(path: str = ".", **kwargs: Any) -> CodeGraph:  # noqa: A001 - intentional
    """``codegraph_rs.open(path)`` -> CodeGraph."""
    return CodeGraph(path, **kwargs)
