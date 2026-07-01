# CodeGraph Vulnerability-Research Roadmap

Status: design (2026-06). Owner mission task: `t1782505017303500001`.
Scope: turn CodeGraph from a name-lexicon "find vulns" black box into a
substrate of **graph-proven, composable primitives** an LLM harness (Explore-style)
drives to discover and **validate** novel, cross-file, *chained* vulnerabilities —
plus a local **vulnerability-knowledge base** that grounds the reasoning.

This doc sits next to `PLAN.md` (shipped work) and `DESIGN_FUTURE.md` (scaling infra).
It is the security/vuln roadmap specifically.

---

## 0. Guiding principle — "the LLM proposes, the graph proves"

Three layers, and the LLM is **never** in the proof path:

1. **Deterministic semantic substrate** (the *prover*) — sound-ish, witness-producing
   analyses in `codegraph-analysis`. Must emit a concrete **witness** for every claim.
2. **LLM explorer loop** (the *proposer*) — lives in the *harness*, not the engine.
   It queries the MCP primitives, proposes hypotheses/specs (which fn is a sink, what
   an opaque libcall does, which chains to try), and reads back proof/refutation.
3. **Hard validation gate** — no graph-proven witness ⇒ no finding emitted. A
   reproducer/PoC, where the target is buildable, is the strongest gate.

This *extends* the existing `vuln/classify.rs::GraphVerifier` ("model proposes,
graph proves"). The fix is to make the **prover stronger and the surface composable**,
not to make the proposer smarter.

---

## 1. Verdict on today's engine

### Keep (these are the defensible core)
- `vuln/classify.rs::GraphVerifier` — verifies LLM role proposals against call-graph
  facts (in-degree, precedence). This is the prove-boundary; keep as a library + the
  `verify_roles` MCP tool.
- `vuln/learned.rs::LearnedStore` — noisy-OR evidence fusion. Reuse for ranking.
- `taint_seed.rs::scan_unsanitized_flows` — *real* interprocedural taint over
  `taint_v2` + `PointsToOracle`. Keep the engine; replace the **seeding**.

### Fix / demote
- `vuln/mining.rs::mine_missing_guards` (`MissingDominatorCheck`) uses **call-site line
  ordering**, not the real dominator tree — even though `dominators.rs` /
  `analysis/algorithms.rs::DominatorTree` exist. **This is the most corrosive defect:
  it fakes the proof substrate.** Wire it to real dominance / control-dependence. (P0)
- Name-lexicon seeding (`taint_naming.rs`, `STRONG_SINK_LEXICON`, `infer_class` string
  matching) is load-bearing today. Demote it to **one weighted signal** fused via
  `LearnedStore`, not the gate.

### Reposition (the MCP surface)
The monolithic `vuln` tool that emits a ranked findings list is the **wrong
abstraction for the MCP** — it collapses orchestration the agent should own into a
black box it cannot refine. Replace it with **composable primitive tools** (§4) the
Explore harness composes. Keep `verify_roles`. (Decision: `t1782505203149438002`.)

---

## 2. What CodeGraph already has (strong base — do not rebuild)

| Capability | Where | Notes |
|---|---|---|
| Cross-file symbol/call/type graph, 12+ langs | `graph.rs`, `src/extraction/` | incl. C/C++ |
| Interprocedural taint | `taint_v2.rs` | over a points-to oracle |
| Points-to / IR | `points_to.rs`, `ir/`, `ir_map.rs` | precision = the lever (§3) |
| Dominators + tree | `dominators.rs`, `analysis/algorithms.rs` | **not** wired into mining yet |
| Per-fn CFG + dataflow | `cfg.rs`, `dataflow.rs`, `NodeData.cfg/.dataflow` | per-language rules tables |
| Program slicing (fwd/back) | `slicing.rs`, `analysis_tools.rs::program_slice` | |
| Label-constrained reachability (RLC) | `label_reachability.rs` | regular path constraints over edge labels |
| Path *predicate text* | `predicates.rs` | **text only — no solving** (§3.3) |
| IDA / Hex-Rays decompiled-C ingestion | `src/extraction/ida_c_extractor/`, `ida_manifest.rs` | pseudo-C bodies |
| Role verifier ("graph proves") | `vuln/classify.rs::GraphVerifier` | keep |
| Evidence fusion | `vuln/learned.rs::LearnedStore` | noisy-OR |
| Concurrency lint | `concurrency.rs` | TOCTOU-adjacent |

> Naming hazards: `capabilities.rs` is a **feature-toggle tree** (CallGraph/TypeUsage
> on/off), NOT security capabilities. `cascade.rs` is **edit-propagation** for
> signature changes, NOT exploit chaining. `validation.rs` is **edit-safety**, NOT bug
> validation. New work must namespace away from these (use `exploit_primitive`,
> `PrimitiveKind`, etc.).

---

## 3. Missing analysis primitives (the prover) — prioritized

### P0 — Make the existing proof real (foundation)
- Wire `MissingDominatorCheck` to the real dominator/post-dominator/control-dependence.
- Define a canonical **`Witness` object** now: the concrete CFG/call-graph path +
  the missing guard/free/overflow site. Every later primitive emits into this one shape
  so the gate and PoC generator are uniform. (Task `t1782505203262359005`.)

### 3.1 Semantic event layer (keystone)
A canonical, language-agnostic per-function/per-path **event stream** lowered from IR:
`alloc · free · use/deref · copy/move · cast · arith · branch · check/guard · source ·
sink · capability-grant`. C/C++/decompiled-C all lower to the same events; taint,
typestate, interval all consume it. This decouples the 12-language front end from the
analyses and is the prerequisite for §3.2–§3.4.

### Tier 1 — highest signal per unit effort
- **(3.2) Typestate / heap-object lifecycle** — per-allocation state machine
  `Uninit→Allocated→Freed→(Use=UAF)/(Free=DoubleFree)`; flow- & path-sensitive over the
  event stream, interprocedural via call graph + points-to. Emits alloc→free→use
  witness. Highest-value *new* capability for low-level C (CWE-416/415/476).
- **(3.3) Exploit-primitive / sanitizer-aware taint** — annotate each
  source/sink/primitive with the **capability it yields** (read N bytes / write
  controlled value / control free / control alloc-size / control branch-target) and
  which sanitizer category would fire. Turns single-hop findings into **composable
  chains** (info-leak → arbitrary-write → control-flow). Cheap: an annotation +
  composition layer over `taint_v2`. (Namespace: `PrimitiveKind`, not `Capability`.)

### Tier 2 — high value, more effort
- **(3.4) Value-range / interval analysis**, scoped to size/index/length SSA values →
  CWE-190 integer-overflow → undersized-alloc → CWE-787 OOB write. *Not* a general
  abstract interpreter.
- **(3.5) SMT / path-feasibility (Z3)** as a **pruner**, never a search strategy: take a
  candidate witness, slice the path (`slicing.rs`), collect the path condition (upgrade
  `predicates.rs` from condition *text* to a real constraint IR), ask Z3 for
  (a) feasibility and (b) guard-evasion (∃ input s.t. guard passes ∧ bug still fires).
- **(3.6) Context-sensitive (CFL) reachability** — matched call/return brackets, the
  next precision lever already flagged in `label_reachability.rs`'s own module doc.
- **(3.7) Datalog rule engine** (`DESIGN_FUTURE.md` P13) — expose declarative pattern
  queries (`unsafe_path(X) :- reachable(X,Y), sink(Y), !sanitized(X).`); let the LLM
  author rules. Strong fit for variant analysis.

### Tier 3 — orchestration & gate (mostly harness-side)
- **(3.8) Attack-surface / entry-point enumeration** — exported fns, parsers,
  syscall/IPC/network boundaries, fuzzable entry points. The natural seed set.
- **(3.9) Patch-diff / variant analysis** — "find structurally similar sinks to this
  confirmed bug"; "what does this diff change in reachability/taint" (n-day + variant).
- **(3.10) Reproducer / PoC generation** — the hard gate, harness-side: drive a
  fuzz/concolic harness from the SMT model's satisfying assignment. For
  non-buildable/decompiled-only targets, fall back to "proven-static, not-reproduced".

---

## 4. Proposed MCP surface change

Replace the `vuln` black box with **primitive tools** the harness composes (keep the
original 13, add these; route in `src/mcp/tools/`, register schemas in `registry/`):

- `typestate(symbol|file)` → heap-lifecycle facts + UAF/DF witnesses
- `taint_path(source?, sink?)` → flow with a **witness** + capability annotations
- `feasible(path|witness)` → SAT/UNSAT/UNKNOWN + model (SMT pruner)
- `attack_surface()` → ranked entry points
- `variants(finding)` → structurally similar sinks
- `slice(symbol, dir)` → already exists (`program_slice`); expose as a tool
- keep `verify_roles` (the prove-boundary)
- **KB tools** (§5): `cve_lookup`, `cwe_classify`, `cvss_enrich`, `dep_audit`

Two *named but transparent* convenience compositions encode the proven pipeline:
`source→sink→feasibility→witness` (find/confirm) and `confirmed→variants` (spread).

---

## 5. Vulnerability-knowledge base (the "baseline primitives" layer)

A **local mirror** + an MCP query surface. Two jobs: (a) **CWE taxonomy = the primitive
library** that grounds the LLM's reasoning and labels findings; (b) **CVE/advisory data**
to enrich, prioritize, and do SCA (dependency / cross-library chaining).

### 5.1 Authoritative sources to mirror (verified 2026-06)

| Source | What | Endpoint / mirror | Format | Cadence | License |
|---|---|---|---|---|---|
| **NVD CVE API 2.0** | CVE + CVSS + CPE | `https://services.nvd.nist.gov/rest/json/cves/2.0` (API key); feeds `https://nvd.nist.gov/vuln/data-feeds` | JSON 2.0 | feeds: year=daily, recent/modified=2h | US Gov public domain |
| **CVE List V5** | canonical CVE records | `github.com/CVEProject/cvelistV5` (clone / daily release) | CVE JSON 5.x | continuous | CVE Program terms (free) |
| **MITRE CWE** | weakness taxonomy | `https://cwe.mitre.org/data/xml/cwec_latest.xml.zip`; views CWE-1000 (research), CWE-699 (dev), CWE-1003 (NVD map) | XML | ~2–3 / yr | free w/ attribution |
| **CISA KEV** | actively-exploited CVEs | `https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json` (+ CSV) | JSON/CSV | as added | US Gov public domain |
| **EPSS (FIRST)** | exploit probability (30d) | `https://www.first.org/epss/api`; daily `epss_scores-YYYY-MM-DD.csv.gz` | CSV/JSON | **daily** | free, cite FIRST |
| **CVSS (FIRST)** | v3.1 + v4.0 spec/calc | `https://www.first.org/cvss/` | spec + JS calc | per-version | free |
| **OSV.dev** | OSS pkg advisories w/ version ranges | API `https://api.osv.dev`; bulk `gs://osv-vulnerabilities` (per-ecosystem zips) | OSV JSON | continuous | CC-BY (per source) |
| **GitHub Advisory DB (GHSA)** | OSS advisories | `github.com/github/advisory-database` (OSV format); GraphQL `securityAdvisories` | OSV JSON | continuous | CC-BY 4.0 |
| **ExploitDB** | public exploits/PoCs | `gitlab.com/exploit-database/exploitdb` (`searchsploit`) | CSV + files | continuous | mixed/GPL |
| **HackerOne** | disclosed reports (hacktivity) | Hacker API `https://api.hackerone.com/` (filters: cwe, cve_ids, severity_rating, disclosed_at) | JSON | continuous | ToS-bound |
| **Bug-bounty scopes** | program scopes (H1/Bugcrowd/Intigriti/YWH) | `github.com/arkadiyt/bounty-targets-data` (hourly); `bbscope` | JSON | hourly | per-repo |

> Note: NVD/CVE-List/KEV/OSV overlap. Pragmatic split — **CVE-List V5 + NVD** for CVE
> core, **CWE XML** for taxonomy, **KEV + EPSS** for prioritization, **OSV/GHSA** for
> package-version-range advisories (NVD's CPE model handles ranges poorly). Bug-bounty
> data (H1 hacktivity, disclosed reports) is *training/grounding* corpus for the
> proposer, not a fact source for the prover.

### 5.1b National / government vulnerability databases
Useful for coverage gaps (each enriches CVEs the others miss) and for non-US scope.

| DB | Org / country | Endpoint | Notes |
|---|---|---|---|
| **CVE.org** | MITRE / CVE Program | `https://www.cve.org` (records via CVE-List V5) | canonical CVE source of record |
| **EUVD** | ENISA (EU) | `https://euvd.enisa.europa.eu/` (+ API) | EU database, EU-CRA driven; own enrichment |
| **JVN / JVNDB** | JPCERT/IPA (Japan) | `https://jvndb.jvn.jp/en/` | Japanese vendor status; MyJVN API |
| **CNNVD** | CNITSEC (China) | `http://www.cnnvd.org.cn/` | national DB; often earlier/later than NVD |
| **CNVD** | CNCERT (China) | `https://www.cnvd.org.cn/` | second Chinese national DB |
| **BDU** | FSTEC (Russia) | `https://bdu.fstec.ru/` | Russian national DB |
| **CERT-In** | India | `https://www.cert-in.org.in/` | advisories |
| **KISA / KrCERT** | Korea | `https://knvd.krcert.or.kr/` | Korean national DB |
| **GSD** | Cloud Security Alliance | `github.com/cloudsecurityalliance/gsd-database` | Global Security Database (open) |
| **VRDX vuln-DB catalog** | FIRST VRDX-SIG | `https://www.first.org/global/sigs/vrdx/vdb-catalog` | **meta-catalog of the world's vuln DBs** — start here for "more DBs" |
| **(community list)** | — | `github.com/haxdoggy/vulnerability-databases` | curated global list |

### 5.1c OSS ecosystem advisory databases (SCA / dependency chaining)
Most are OSV-format and already aggregated by OSV.dev, but the upstreams carry richer
context. SCA over an SBOM (CycloneDX/SPDX) against these powers cross-library chaining.

| DB | Ecosystem | Endpoint |
|---|---|---|
| **GitHub Advisory DB** | multi (npm, PyPI, Go, Maven, NuGet, Composer, Cargo, …) | `github.com/github/advisory-database` |
| **PyPA Advisory DB** | Python | `github.com/pypa/advisory-database` |
| **RustSec** | Rust/Cargo | `github.com/rustsec/advisory-db`, `rustsec.org` |
| **Go Vuln DB** | Go | `github.com/golang/vulndb`, `vuln.go.dev` |
| **Ruby advisory-db** | Ruby | `github.com/rubysec/ruby-advisory-db` |
| **FriendsOfPHP** | PHP/Composer | `github.com/FriendsOfPHP/security-advisories` |
| **Debian Security Tracker** | Debian | `security-tracker.debian.org` |
| **Ubuntu USN** | Ubuntu | `ubuntu.com/security/notices` |
| **Red Hat Security Data API** | RHEL | `access.redhat.com/hydra/rest/securitydata` |
| **Alpine SecDB** | Alpine | `secdb.alpinelinux.org` |
| **Sonatype OSS Index** | multi | `ossindex.sonatype.org` |
| **GitLab Advisory DB** | multi | `gitlab.com/gitlab-org/security-products/gemnasium-db` |

### 5.1d Exploit / PoC corpora (grounding the proposer; reproducer references)
| Source | What | Endpoint |
|---|---|---|
| **ExploitDB** | curated public exploits | `gitlab.com/exploit-database/exploitdb` |
| **Metasploit Framework** | exploit modules | `github.com/rapid7/metasploit-framework` |
| **Nuclei templates** | detection/PoC templates | `github.com/projectdiscovery/nuclei-templates` |
| **PoC-in-GitHub** | auto-collected CVE PoCs | `github.com/nomi-sec/PoC-in-GitHub` |
| **trickest/cve** | CVE→PoC index | `github.com/trickest/cve` |
| **Packet Storm** | exploits/advisories | `packetstormsecurity.com` |
| **Vulners** | aggregator + API | `vulners.com/api` |
| **VulnCheck KEV / XDB** | enriched KEV + exploit intel | `vulncheck.com` (API) |

### 5.1e Research venues, write-ups & datasets (proposer corpus, not facts)
- **Top-4 security confs** (where novel-bug + analysis techniques are published):
  USENIX Security, IEEE S&P (Oakland), ACM CCS, NDSS — indexes:
  `github.com/prncoprs/best-papers-in-computer-security`, `oaklandsok.github.io`,
  `wcventure.github.io/FuzzingPaper`. Also arXiv `cs.CR`.
- **Project Zero**: blog `googleprojectzero.blogspot.com`, bug tracker
  `bugs.chromium.org/p/project-zero`, repo `github.com/googleprojectzero`.
- **Google Security Research** (PoCs/advisories): `github.com/google/security-research`.
- **OSS-Fuzz / OSS-Fuzz-Gen** (harnessing corpus): `github.com/google/oss-fuzz`,
  `github.com/google/oss-fuzz-gen`.
- **Vulnerability/code datasets** (for detector calibration): BigVul, DiverseVul,
  Devign, CVEfixes, PrimeVul — sourced via the papers above.

> Ingestion discipline: §5.1/5.1b/5.1c are **facts** (mirror → query). §5.1d/5.1e are
> **proposer grounding** (corpus the LLM/heuristics learn from) — they must NOT directly
> emit findings; they feed `LearnedStore` priors and reproducer references, and every
> resulting hypothesis still passes the §0 hard graph-proof gate.

### 5.2 CWE as the primitive library
Walk **CWE-1000** (ChildOf/ParentOf, **CanPrecede**, PeerOf) to go from a specific
weakness up to class/pillar, and to model **precursor chains** (CWE-190 *CanPrecede*
CWE-787). Memory-safety lineage under CWE-664/119: 787 OOB-write, 125 OOB-read, 416
UAF, 415 double-free, 476 NULL-deref, 190 int-overflow. Injection under CWE-74: 78 cmd,
89 SQLi, 22 path-traversal (CWE-706/668). Concurrency: 362 race, 367 TOCTOU.
Each detector/typestate/interval rule carries a **CWE tag** (deterministic
detector→CWE table) → walk the hierarchy for grouping + LLM grounding.

### 5.3 Finding → CWE → CVSS pipeline
1. detector rule → CWE-ID (deterministic table).
2. CWE-1000 walk → class/pillar context.
3. derive a CVSS-like **severity prior** from CWE class + dataflow facts (AV/PR/UI set
   from taint reachability + attack surface). Emit a **severity band + assumptions**,
   never false-precision.
4. if the finding matches a known CVE/advisory, **override** with real CVSS/EPSS/KEV.

### 5.4 Storage
Local SQLite KB, **separate** from the per-project graph DB (mirror the `src/history.rs`
flywheel pattern — global, not per-project schema). Refresh job pulls the feeds above.

---

## 6. Over-engineering traps (do NOT build)
- Full general-purpose abstract interpretation (scope intervals to size/index/length).
- Whole-program symbolic execution as a *discovery* strategy (path explosion). SMT only
  validates/prunes pre-found candidates on *sliced* paths.
- Sound + complete pointer analysis (accept unsoundness; let the PoC gate catch FPs).
- LLM in the proof loop. Ever.

## 7. Soundness / false-positive traps
- Line-ordering masquerading as dominance (today's bug — P0).
- Points-to imprecision → spurious alias/UAF (decompiled C is worst).
- Sanitizer-aware-but-wrong (a "sanitizer" that doesn't sanitize *this* taint kind).
- SMT timeout reported as UNSAT (must be UNKNOWN).
- Auto-CVSS false precision (emit bands).

---

## 8. Phasing → tasks

- **Phase 1 (foundation):** P0 dominators+`Witness`; vuln-engine test backfill;
  mermaid module diagram (independent, ship now). Tasks `...004`, `...005`, `...003`.
- **Phase 2 (prover primitives):** semantic event layer → typestate → exploit-primitive
  taint → MCP primitive tools (§4). Then interval + SMT pruner.
- **Phase 3 (knowledge base):** local KB mirror + CWE primitive library + KB MCP tools
  (§5); detector→CWE tagging; CVSS/EPSS/KEV enrichment; OSV SCA.
- **Phase 4 (orchestration):** LLM explorer loop in the harness; attack-surface +
  variants; reproducer/PoC hard gate. Task `...006`.

Each new analysis follows repo invariants: per-language rules tables (not branches),
`ensure_sufficient_stack` at recursion heads, versioned SQLite migrations, count/size
pin tests, and the CI gates (`fmt`/`clippy -D warnings`/`build`/`test`).
