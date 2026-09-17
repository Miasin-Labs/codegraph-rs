
# CodeGraph Multi-Tool Workflow Analysis

## What We Have Now (After Fix)

All 8 core tools exposed by default:
- codegraph_search
- codegraph_node  
- codegraph_explore
- codegraph_callers
- codegraph_callees
- codegraph_impact
- codegraph_files
- codegraph_status

Plus session state tracking and deduplication (CODEGRAPH_EXPLORE_DEDUP=true by default)

## The Token-Saving Workflow Patterns

### Pattern 1: Progressive Refinement
Instead of one massive explore call that returns everything:

```
1. codegraph_search "authenticate" 
   → 5 matches (User.authenticate, Session.authenticate, etc.)
   → 200 tokens
   
2. codegraph_node User.authenticate
   → Just the signature + location
   → 150 tokens
   
3. codegraph_explore User.authenticate
   → Full source + call graph + dependencies
   → 2000 tokens
   
Total: 2,350 tokens for targeted context
```

Without tools, reading files blindly:
```
1. grep -r "authenticate"
   → 50+ matches across 20 files  
   → Must read context for each
   
2. Read User.ts (entire file)
   → 3000 tokens (includes unrelated code)
   
3. Read Session.ts (entire file)
   → 2500 tokens
   
4. Read Auth.ts (entire file)
   → 4000 tokens
   
Total: 9,500+ tokens for unfocused context
```

**Savings: 75% fewer tokens**

### Pattern 2: Impact Analysis Before Refactoring

```
1. codegraph_impact changePassword
   → List of 12 affected files/symbols
   → 300 tokens
   
2. codegraph_explore on affected symbols
   → Only the changed regions
   → Session dedup prevents re-sending same source
   → 1500 tokens
   
Total: 1,800 tokens
```

Without tools:
```
1. grep -r "changePassword"
   → 25+ matches
   
2. Read all 25 files to understand usage
   → 15,000+ tokens
   
3. Manually trace dependencies
   → More reads, more tokens
   
Total: 20,000+ tokens
```

**Savings: 91% fewer tokens**

### Pattern 3: Call Graph Navigation

```
1. codegraph_callers handleRequest
   → 8 callers listed
   → 250 tokens
   
2. codegraph_callees handleRequest  
   → 15 callees listed
   → 400 tokens
   
3. codegraph_explore on specific path
   → Source for the interesting flow
   → Dedup skips already-shown files
   → 1200 tokens
   
Total: 1,850 tokens
```

Without tools:
```
1. Read handleRequest file
   → 2000 tokens
   
2. Search for all references
   → grep through codebase
   
3. Read each caller file
   → 8 files × 2000 = 16,000 tokens
   
4. Read each callee file
   → 15 files × 2000 = 30,000 tokens
   
Total: 48,000+ tokens (and still might miss dynamic calls)
```

**Savings: 96% fewer tokens**

### Pattern 4: Multi-Agent Fan-Out with Session State

Parent agent:
```
1. codegraph_search "payment"
   → 20 matches across modules
   
2. Spawn 3 child agents, each gets:
   - Child A: PaymentProcessor module (5 symbols)
   - Child B: PaymentValidator module (7 symbols)  
   - Child C: PaymentGateway module (8 symbols)
```

Each child:
```
1. codegraph_explore <their symbols>
   → First call: full source
   → Session dedup tracks what was sent
   
2. codegraph_impact <their changes>
   → Cross-module impacts
   
3. Second codegraph_explore call
   → ONLY new source (dedup skips already-sent)
   → Saves 60-80% on repeated context
```

Without tools + session state:
```
- Each child must re-read entire files
- No dedup = same source sent multiple times
- Parent must orchestrate manual grep/read loops
- 3x-5x more tokens for same work
```

**Savings: 70% fewer tokens across the workflow**

## Missing Workflow Features (TypeScript Has, Rust Needs)

### 1. Budget Decay (CG-19)
TypeScript tracks cumulative session budget and reduces explore output size as session grows.

**Why it matters**: Long sessions don't exhaust context by returning huge explore results every time

**Token impact**: Prevents context bloat in multi-turn workflows (20-30% savings in long sessions)

### 2. Cross-Call Deduplication Banner (CG-18)
TypeScript shows "Already sent earlier in this conversation" for files that haven't changed.

**Why it matters**: Agent knows to scroll up, not re-fetch

**Token impact**: Zero-cost reference to prior context (100% savings on duplicates)

### 3. Multi-Project Session Tracking
TypeScript tracks state PER project within a session (via projectPath).

**Why it matters**: Monorepo work or multi-repo sessions maintain separate dedup state

**Token impact**: Prevents cross-contamination of session state (correctness + efficiency)

## The Database Query Flow

Each tool queries the SQLite graph differently:

```
codegraph_search:
  SELECT * FROM nodes WHERE name LIKE ? LIMIT ?
  → Index scan, sub-millisecond
  → Returns: id, name, kind, file, line, signature

codegraph_node:
  SELECT * FROM nodes WHERE id = ?
  + Read source file for that location
  → Single row lookup + file read
  → Returns: Full source snippet

codegraph_callers:
  SELECT * FROM edges WHERE to_node_id = ? AND kind = 'call'
  JOIN nodes ON from_node_id = nodes.id
  → Index scan on edges
  → Returns: List of caller nodes

codegraph_callees:
  SELECT * FROM edges WHERE from_node_id = ? AND kind = 'call'
  JOIN nodes ON to_node_id = nodes.id
  → Index scan on edges
  → Returns: List of callee nodes

codegraph_impact:
  Recursive CTE: WITH RECURSIVE dependents AS (...)
  → Walks dependency graph
  → Returns: Affected nodes at all depths

codegraph_explore:
  1. Search query → seed nodes
  2. Expand via edges → related nodes
  3. Rank by relevance
  4. Read source files for top N
  5. Apply dedup filter (session state)
  6. Build call graph among shown symbols
  → Multi-step query + file reads + dedup
  → Returns: Structured source + relationships
```

## Agent Architecture Patterns

### Single-Agent Deep Dive
```
User: "Fix the authentication bug"

Agent flow:
1. codegraph_search "auth"          (broad discovery)
2. codegraph_impact "authenticate"  (scope the change)
3. codegraph_explore <affected>     (get source)
4. Edit files
5. codegraph_callers "authenticate" (verify no breaks)
```

**Token efficiency**: 5 focused queries vs 20+ file reads

### Fan-Out Parallel Investigation
```
User: "Audit the payment flow for security"

Parent:
1. codegraph_search "payment"
2. Spawn 4 children (one per concern):
   - Child A: Input validation
   - Child B: Database transactions
   - Child C: External API calls
   - Child D: Error handling

Each child:
1. codegraph_explore <their area>
2. codegraph_impact <their changes>
3. Report findings

Parent: Synthesize reports
```

**Token efficiency**: 4x parallelism + dedup = 10x faster than sequential file reads

### Incremental Refactor
```
User: "Refactor User model to use new auth system"

Agent:
1. codegraph_node User              (current structure)
2. codegraph_impact User            (blast radius)
3. codegraph_callers User.login     (who depends on old auth)
4. codegraph_explore User + callers (get full source)
   [Session dedup active]
5. Edit User model
6. codegraph_explore <callers>      (refresh changed files)
   [Dedup skips unchanged files, only shows new changes]
7. Edit caller files
```

**Token efficiency**: Step 6 only sends NEW source, not duplicates from step 4

## Summary: Why This Matters

### Before (explore-only, no session state)
- One tool to rule them all
- Every call returns everything
- No deduplication
- Agents over-fetch context
- 50-100k tokens per workflow

### After (8 tools + session state)
- Progressive refinement workflows
- Targeted queries save 70-95% tokens
- Session dedup prevents re-sending
- Multi-agent fan-out becomes efficient
- 5-20k tokens per workflow

### Real-World Impact
- **Search first, explore second**: Find what you need, then get its source (not everything)
- **Impact analysis**: Know the blast radius before changing (don't read 50 files)
- **Call graph navigation**: Follow edges, not grep (dynamic dispatch tracked)
- **Parallel agents**: Each child gets deduplicated context (no waste)
- **Long sessions**: Budget decay + dedup keeps context lean (100+ turn sessions viable)
