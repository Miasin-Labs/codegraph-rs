# CodeGraph telemetry

CodeGraph records anonymous aggregate usage by default. The first long-running
command that can send data prints a notice. Disable it at any time with:

```bash
codegraph telemetry off
```

`DO_NOT_TRACK=1` has highest priority. `CODEGRAPH_TELEMETRY=0` or `1` overrides
the saved choice; `codegraph telemetry status` shows the effective setting and
why it was selected.

Collected data is limited to:

- a random installation UUID;
- CodeGraph version, operating system, and CPU architecture;
- sanitized command/tool or lifecycle identifiers;
- daily aggregate invocation and error counts;
- lifecycle properties containing only bounded identifiers, booleans, numbers,
  or null values.

CodeGraph does not collect source code, prompts, file or project paths, symbol
names, repository names, command arguments, environment values, or query text.
Identifiers containing path separators or free-form text are rejected before
they enter the local queue.

Unsent events are stored in user-private files under `~/.codegraph/`. Turning
telemetry off deletes the buffered queue. Network errors never fail a CodeGraph
command; unsent events remain local for a later maintenance command.
