//! Server-level instructions emitted in the MCP `initialize` response.

pub const SERVER_INSTRUCTIONS: &str = r##"# Codegraph — code intelligence over an indexed knowledge graph

Codegraph is a SQLite knowledge graph of every symbol, edge, and file in
the workspace — pre-computed structure you would otherwise re-derive by
reading files (cached intelligence: thousands of parse/trace decisions you
don't pay to re-reason each run). It indexes 30+ languages
(TypeScript/JavaScript, Python, Go, Rust, Java, C#, C/C++, PHP, Ruby, Swift,
Kotlin, and more) — don't assume a language here isn't covered. Reads are
sub-millisecond; the index lags writes by ~1s through the file watcher. Reach for it BEFORE *and* while
writing or editing code — not just for questions: one call returns the
verbatim source PLUS who calls it and what it affects, so you edit with the
blast radius in view. More accurate context, in far fewer tokens and
round-trips than reading files yourself.

## Primary tool: codegraph_explore — Read-equivalent with structure

`codegraph_explore` is the primary tool and is Read-equivalent. It takes
either a natural-language question or a bag of symbol/file names and returns
the **verbatim, line-numbered source** of the relevant symbols grouped by
file — the same `<n>\t<line>` shape `Read` gives you, safe to `Edit` from —
PLUS the call path among them (including dynamic-dispatch hops like callbacks,
React re-render, and JSX children that grep can't follow) and a blast-radius
summary of what depends on them.

Whether you're answering "how does X work" or implementing a change (fixing a
bug, adding a feature), call `codegraph_explore` before you Read. ONE call
usually answers the whole question. Codegraph IS the pre-built search index —
so running your own grep + read loop, or delegating the lookup to a separate
file-reading sub-task/agent, repeats work codegraph already did and costs more
for the same answer. A direct codegraph answer is typically one to a few
calls; a grep/read exploration is dozens.

## How to query

- **Almost any question — "how does X work", architecture, a bug, "what/where is X", or surveying an area** → `codegraph_explore` with a natural-language question or the relevant names. ONE capped call returns the verbatim source grouped by file; most often the ONLY call you need.
- **"How does X reach/become Y? / the flow / the path from X to Y"** → `codegraph_explore`, naming the symbols that span the flow (e.g. `mutateElement renderScene`) — it surfaces the call path among them, riding dynamic-dispatch hops, and returns their source.
- **Reading or editing a file/symbol you can name** → put its name or file path in the `codegraph_explore` query — it returns that current line-numbered source (safe to `Edit` from) with the call path and blast radius attached, so you don't Read it separately. For an overloaded name it returns every matching definition's body in one call.
- **Need more?** Call `codegraph_explore` again with more specific names — treat the source it returns as already Read.

## Available tools

Eleven core tools are available by default:

- **`codegraph_explore`** — Primary context tool: natural-language question or symbol/file names → verbatim source + call paths + dependencies (Read-equivalent)
- **`codegraph_search`** — Quick symbol lookup: name → locations/kinds/signatures (no source)
- **`codegraph_node`** — Get one symbol's definition: name → source/signature/location
- **`codegraph_callers`** — Find who calls a symbol: name → list of callers
- **`codegraph_callees`** — Find what a symbol calls: name → list of callees  
- **`codegraph_impact`** — Refactor impact analysis: symbol → affected code
- **`codegraph_files`** — Project file structure from the index
- **`codegraph_status`** — Index status and statistics
- **`codegraph_history`** — What changes together with a symbol, from git history (instead of git log/blame)
- **`codegraph_tests`** — Which tests exercise a symbol, via its callers (pick the tests to run after a change)
- **`codegraph_diagnostics`** — Compiler errors/warnings (cargo check/clippy or the project's tsc), each placed on its symbol; a long build keeps running between calls

Batch lookups: `codegraph_search` and `codegraph_node` take a `symbols` array
for several names in one call — use it instead of a grep alternation `a|b|c`.
`codegraph_search` also takes `projectPaths` to search several indexed projects
at once, each hit tagged with its `project`.
Re-reading a range already sent this session returns `alreadySent` instead of
the source again.

Most questions are answered by `codegraph_explore` alone. Use the specific
tools (search/node/callers/etc.) when you need ONLY that focused information
without full context.

Advanced tools (arch, xref, paths) can be enabled via the `CODEGRAPH_MCP_TOOLS`
environment variable — see project documentation.

## Anti-patterns

- **Trust codegraph's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep or Read first** to find or understand indexed code — ONE `codegraph_explore` returns the relevant symbols' source together in a single round-trip. Reach for raw `Read`/`Grep` only to confirm a specific detail codegraph didn't cover, or for what codegraph doesn't index (configs, docs).
- **Don't reconstruct a flow by hand** — name the endpoints in one `codegraph_explore` and it surfaces the path between them, dynamic-dispatch hops included.
- **After editing, check the result's notices.** When something may make a result differ from the code on disk, the result says so: a JSON result carries a `notices` array (`{kind, message, files?, filesOmitted?}`, right after `kind`), and a plain-text result starts with one "⚠️ …" line per notice. No notices means none apply.
  - `stale_index` — the listed `files` changed after the last index sync (just edited and pending re-index, or drifted on disk — most common on `projectPath` projects, which have no live watcher). Codegraph never serves a possibly-mis-sliced body from such a file: any source it shows for one is the full CURRENT content (trust it as a Read); otherwise Read that file, and treat its line numbers and edges here as possibly shifted. Every file not listed is in sync, so still trust codegraph for the rest (`filesOmitted` counts changed files the list left out).
  - `auto_sync_disabled` — live watching stopped entirely: the whole index is frozen, not just a few files. Until it's resolved, Read files directly to confirm anything that may have changed.
  - `worktree_mismatch` — the index belongs to a different git worktree (often another branch): symbols changed only in yours are missing.
  - `stale_extraction` — the index was built by an older extractor, so symbols and call edges may be missing or wrong until the user runs `codegraph index`; mention it rather than treating an empty callers/impact answer as proof.

- **"Already sent earlier in this conversation" is a pointer, not a gap.** When a result says so (or carries `alreadySent`) instead of source, an earlier `codegraph_explore` or `codegraph_node` in THIS conversation already returned those exact lines and the file has not changed since — so the copy already in your context is current and exact. Scroll back to it; don't re-fetch it and don't Read the file. The bytes it freed went into source you have not seen yet, elsewhere in the same response.

## Limitations

- If a tool reports a project isn't indexed (no `.codegraph/`), stop calling codegraph tools for that project for the rest of the session and use your built-in tools there instead. Indexing is the user's decision — mention they can run `codegraph init` if it comes up, but don't run it yourself.
- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- The graph itself does no type checking: `codegraph_diagnostics` asks the project's own compiler (Rust and TypeScript only). Other languages' checkers, the test suite, and linters are still yours to run.
"##;

pub const SERVER_INSTRUCTIONS_NO_ROOT_INDEX: &str = r##"# Codegraph — available (per-project; pass projectPath)

Codegraph is a SQLite knowledge graph of a codebase's symbols, edges, and
files (30+ languages): one `codegraph_explore` call returns the verbatim, line-numbered source
of the relevant symbols PLUS the call paths between them and a blast-radius
summary — replacing a grep + Read loop with one round-trip.

This server started somewhere with no `.codegraph/` of its own, so there is no
default project — but the tools are available and work **per project**:

- To query a project that HAS a `.codegraph/` index (e.g. a service inside a
  monorepo, or a second repo), pass its path as `projectPath` to
  `codegraph_explore` (and any other codegraph tool). Codegraph resolves the
  nearest `.codegraph/` at or above that path and answers from it — for as many
  projects as you like in one session.
- For a project with no `.codegraph/`, use your built-in tools (Read/Grep/Glob)
  for that project. Indexing is the user's decision — don't run it yourself, but
  if it comes up they can run `codegraph init` in a project to enable codegraph
  there (a new index is picked up live, no restart).
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_describe_the_live_default_surfaces() {
        assert!(SERVER_INSTRUCTIONS.contains("## Primary tool: codegraph_explore"));
        for tool in [
            "search",
            "node",
            "explore",
            "callers",
            "callees",
            "impact",
            "files",
            "status",
            "history",
            "tests",
            "diagnostics",
        ] {
            assert!(
                SERVER_INSTRUCTIONS.contains(&format!("**`codegraph_{tool}`**")),
                "default tool codegraph_{tool} is undocumented"
            );
        }
        assert!(SERVER_INSTRUCTIONS.contains("Advanced tools (arch, xref, paths)"));
        assert!(SERVER_INSTRUCTIONS_NO_ROOT_INDEX.contains("pass projectPath"));
    }

    /// Agents are told what each notice means, under the name it arrives with.
    #[test]
    fn instructions_explain_every_notice_kind() {
        assert!(SERVER_INSTRUCTIONS.contains("a `notices` array"));
        for kind in crate::mcp::tools::NoticeKind::ALL {
            assert!(
                SERVER_INSTRUCTIONS.contains(&format!("`{}` —", kind.as_str())),
                "notice kind {} is unexplained",
                kind.as_str()
            );
        }
    }
}
