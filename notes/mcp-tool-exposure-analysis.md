
# CodeGraph MCP Tool Exposure Analysis

## Current State

### TypeScript (Reference Implementation)
- **Tools exposed**: ALL 8 tools by default
- **No filtering**: Every tool in the registry is immediately available
- **User experience**: Works out of the box, matches documentation

### Rust (Current)
- **Tools implemented**: 12 tools in registry
  - Core 8: search, node, explore, callers, callees, impact, files, status
  - Advanced 3: arch, xref, paths  
  - Feature-gated 2: vuln, verify_roles (require --features vuln)
- **Tools exposed by default**: 1 (only explore)
- **Override mechanism**: CODEGRAPH_MCP_TOOLS env var (undocumented for users)
- **User experience**: Broken - AGENTS.md promises search/node but they don't work

## The Problems

1. **Documentation mismatch**: AGENTS.md explicitly instructs using codegraph_search and 
   codegraph_node, but they're not available without env var override

2. **Parity regression**: TypeScript → Rust port reduced surface from 8 tools to 1

3. **Hidden functionality**: All CLI commands work (search, node, callers, etc.) but 
   MCP hides them behind undocumented env var

4. **Discoverability failure**: Users see one tool in MCP, don't know others exist

5. **Workflow breaks**: Quick lookups need search/node, not just explore

## Proposed Solutions

### Option 1: Match TypeScript (Recommended)
Expose the same 8 core tools by default:
- search, node, explore, callers, callees, impact, files, status

Keep advanced tools (arch, xref, paths) behind opt-in for now since TS doesn't have them.
Keep feature-gated tools (vuln, verify_roles) gated as-is.

**File**: src/mcp/tools/registry/filters.rs
**Change**: 
```rust
const DEFAULT_MCP_TOOLS: &[&str] = &[
    "search", "node", "explore", 
    "callers", "callees", "impact",
    "files", "status"
];
```

### Option 2: Expose ALL Tools  
Remove filtering entirely - if it's in the catalog, expose it.

**Pros**: Maximum capability, simplest code
**Cons**: Exposes arch/xref/paths which are experimental/undocumented

### Option 3: Two-Tier Defaults
Core tier (auto): search, node, explore, callers, callees, impact, files, status
Advanced tier (opt-in): arch, xref, paths

**Implementation**: Check CODEGRAPH_MCP_TOOLS, if unset use core tier, if "all" use everything

## Recommendation

**Go with Option 1** - exact TypeScript parity for the 8 core tools.

Reasoning:
- Matches user expectations from TS version
- Fixes AGENTS.md documentation mismatch  
- Provides full workflow coverage (search → explore, node for signatures, etc.)
- Keeps experimental tools (arch/xref/paths) opt-in until they're documented
- Minimal risk - these are all tested, working tools with CLI equivalents

## Implementation

1. Change DEFAULT_MCP_TOOLS in src/mcp/tools/registry/filters.rs
2. Update AGENTS.md to document the env var override mechanism
3. Add note about advanced tools (arch/xref/paths) being opt-in
4. Test that all 8 tools work via MCP
