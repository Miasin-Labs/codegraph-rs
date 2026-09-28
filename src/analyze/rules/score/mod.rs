//! `codegraph analyze rules --score <corpus>` (and the MCP `rules` tool's
//! `score` action): run rules over a labeled corpus and say, per rule,
//! whether it beats chance — the check that lets a model keep the rules it
//! writes only when they find bugs.
//!
//! The corpus is split into units and each is staged and indexed the way
//! `tools/bugbench` does it ([`corpus`], [`stage`]); the rules run over each
//! unit's index in-process; findings are scored with bugbench's semantics,
//! ported so the numbers are identical on the same input: labeled rows
//! (function, line or file granularity — [`labeled`]) or vulnerable/fixed
//! pairs ([`differential`]). Each rule gets precision, recall, the corpus
//! base rate and a keep/discard [`verdict`].
//!
//! Bounded: `sample` limits the units, a deadline stops the run between
//! units (an index already running is finished, or — in the MCP server —
//! left to finish in the background; never thrown away), and every scored
//! unit is cached under the rules' hash, so the next call with the returned
//! cursor picks up where this one stopped and ends with the full numbers.

mod corpus;
mod differential;
mod gt;
mod labeled;
mod pyrandom;
mod stage;
#[cfg(test)]
mod tests;
mod verdict;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub use corpus::CorpusKind;
use corpus::{PairSide, Unit};
use serde::{Deserialize, Serialize};
use stage::{PrepareError, UnitCache, Workspace};
pub use verdict::{Decision, Policy};

use super::RuleSet;
use crate::analyze::bugs::{BugsOptions, Detector, Project};
use crate::codegraph::{CodeGraph, OpenOptions};
use crate::utils::sha256_hex;

/// Seconds one unit may take to index (bugbench's default).
const DEFAULT_INDEX_TIMEOUT: Duration = Duration::from_secs(900);
/// Unit failures listed by name.
const MAX_FAILURES_LISTED: usize = 20;

/// A finding as scoring sees it (bugbench's normalized `Finding`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScoredFinding {
    pub unit: String,
    pub rule_id: String,
    /// Ground-truth path: the unit's prefix + the project-relative path.
    pub file: String,
    /// Project-relative path, as reported.
    pub local_file: String,
    pub line: i64,
    pub function: Option<String>,
    pub message: String,
    pub confidence: f64,
}

/// `round(num / den, 4)`, `None` when `den` is 0 — Python's rounding (the
/// exact binary value rounded half-even), so ratios print identically.
pub(crate) fn ratio(num: usize, den: usize) -> Option<f64> {
    if den == 0 {
        return None;
    }
    format!("{:.4}", num as f64 / den as f64).parse().ok()
}

/// What to score and how far to go.
#[derive(Debug, Clone)]
pub struct ScoreOptions {
    /// The corpus directory (holding `ground_truth.jsonl`).
    pub corpus: PathBuf,
    /// Where units are staged and indexed (default: `rulescore-work`
    /// beside the corpus).
    pub work: Option<PathBuf>,
    /// Juliet: testcases per CWE (40); pairs: how many pairs (all).
    pub sample: Option<usize>,
    pub seed: u64,
    /// Line slack (default 3; pairs 5).
    pub slack: Option<i64>,
    /// Units staged and indexed at once.
    pub jobs: usize,
    /// Stop starting (and running) units after this long.
    pub deadline: Option<Duration>,
    /// Bound on one unit's index.
    pub index_timeout: Duration,
    /// A previous run's `nextCursor`.
    pub cursor: Option<String>,
    pub policy: Policy,
    /// The `codegraph` binary that indexes units (default: this one, or the
    /// one beside it; `CODEGRAPH_SCORE_BIN` overrides).
    pub binary: Option<PathBuf>,
    /// At the deadline, return at once and let running indexes finish in
    /// the background (this process lives on: the MCP server) rather than
    /// waiting for them. Either way no index is thrown away.
    pub detach: bool,
}

impl ScoreOptions {
    pub fn new(corpus: PathBuf) -> Self {
        Self {
            corpus,
            work: None,
            sample: None,
            seed: 1,
            slack: None,
            jobs: 4,
            deadline: None,
            index_timeout: DEFAULT_INDEX_TIMEOUT,
            cursor: None,
            policy: Policy::default(),
            binary: None,
            detach: false,
        }
    }
}

/// How far the run got.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitCounts {
    pub total: usize,
    /// Scored (from cache or run now).
    pub done: usize,
    /// Indexed by this run.
    pub indexed: usize,
    /// Could not be staged or indexed.
    pub failed: usize,
    /// Not reached before the deadline.
    pub pending: usize,
}

/// One rule's score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScore {
    pub id: String,
    pub findings: usize,
    pub tp: usize,
    /// Labeled: findings in a `good` row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fp: Option<usize>,
    /// Labeled: findings outside every row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlabeled: Option<usize>,
    /// Pairs: in the fix region but still there after the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_discriminating: Option<usize>,
    /// Pairs: gone after the fix, but away from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_fix: Option<usize>,
    /// Pairs: in both versions, away from the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<usize>,
    /// Labeled: TP / (TP + FP); pairs: TP / (TP + off-fix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<f64>,
    /// Labeled: bad rows hit / bad rows; pairs: pairs detected / pairs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall: Option<f64>,
    pub verdict: Decision,
    pub reason: String,
}

/// The result of [`score_rules`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreReport {
    pub corpus: String,
    /// `juliet-c`, `juliet-java`, `pairs` or `whole`.
    pub layout: &'static str,
    /// `labeled` or `differential`.
    pub scoring: &'static str,
    pub units: UnitCounts,
    /// Units that failed, by name → status.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, String>,
    /// Labeled: bad ÷ (bad + good) rows of the scored files; pairs: the
    /// share of the functions a fix changed (any `vuln/` row) that lie in
    /// its fix region — where a finding that vanishes with the fix lands
    /// by chance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_rate: Option<f64>,
    /// Labeled: bad rows; pairs: scoreable pairs.
    pub positives: usize,
    pub rules: Vec<RuleScore>,
    /// Every unit was scored: the verdicts are final.
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub elapsed_ms: u64,
    /// Where the units are staged (reused by later runs).
    pub work: String,
    /// bugbench's `metrics.json` shape, for comparison with `score.py`.
    #[serde(skip)]
    pub metrics: serde_json::Value,
}

/// The hash findings are cached under: the rules' texts and this binary.
fn rules_hash(texts: &[(String, String)]) -> String {
    let mut all = String::new();
    for (label, text) in texts {
        all.push_str(label);
        all.push('\0');
        all.push_str(text);
        all.push('\0');
    }
    let engine = std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::metadata(exe).ok())
        .map(|meta| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{mtime}", meta.len())
        })
        .unwrap_or_default();
    all.push_str(env!("CARGO_PKG_VERSION"));
    all.push_str(&engine);
    sha256_hex(all.as_bytes())[..16].to_string()
}

/// The `codegraph` binary to index with.
fn index_binary(options: &ScoreOptions) -> Result<PathBuf, String> {
    if let Some(binary) = &options.binary {
        return Ok(binary.clone());
    }
    if let Some(binary) = std::env::var_os("CODEGRAPH_SCORE_BIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(binary));
    }
    crate::sync::background::cli_binary().ok_or_else(|| {
        "no `codegraph` binary to index the corpus with (it must sit beside this executable, \
         or be named by CODEGRAPH_SCORE_BIN)"
            .to_string()
    })
}

/// `<index>:<rules hash>`.
fn parse_cursor(cursor: &str, hash: &str, total: usize) -> Result<usize, String> {
    let (index, owner) = cursor
        .split_once(':')
        .ok_or_else(|| format!("invalid cursor `{cursor}`"))?;
    if owner != hash {
        return Err(
            "the cursor belongs to a run of other rules (or another build); start over without it"
                .to_string(),
        );
    }
    index
        .parse::<usize>()
        .ok()
        .filter(|index| *index <= total)
        .ok_or_else(|| format!("invalid cursor `{cursor}`"))
}

/// A unit's outcome in this run.
enum Outcome {
    Done(UnitCache),
    Failed(String),
    Pending,
}

/// Score the rules in `texts` (`(label, yaml)`, as [`RuleSet::load`] takes
/// them) and `saved` (a project's saved rules, each shadowed by a rule of
/// its id in `texts`) on the corpus. `progress` hears one line per unit.
pub fn score_rules(
    texts: &[(String, String)],
    saved: &[(String, String)],
    options: &ScoreOptions,
    progress: &dyn Fn(&str),
) -> Result<ScoreReport, String> {
    let started = Instant::now();
    let deadline = options.deadline.map(|d| started + d);
    let corpus = options
        .corpus
        .canonicalize()
        .map_err(|e| format!("corpus {}: {e}", options.corpus.display()))?;
    if !corpus.join("ground_truth.jsonl").is_file() {
        return Err(format!(
            "{} has no ground_truth.jsonl (point --score at a bugbench corpus directory)",
            corpus.display()
        ));
    }
    let mut rules = RuleSet::load(&[], texts, false);
    rules.add_shadowed(saved);
    if !rules.errors.is_empty() {
        let list: Vec<String> = rules.errors.iter().map(ToString::to_string).collect();
        return Err(format!("the rules do not load:\n  {}", list.join("\n  ")));
    }
    if rules.rules.is_empty() {
        return Err("no rules to score".to_string());
    }
    let kind = CorpusKind::detect(&corpus);
    let units = corpus::enumerate_units(&corpus, kind, options.sample, options.seed)?;
    let hash = rules_hash(&[texts, saved].concat());
    if let Some(cursor) = &options.cursor {
        parse_cursor(cursor, &hash, units.len())?;
    }
    let work_root = match &options.work {
        Some(work) => work.clone(),
        None => corpus.parent().unwrap_or(&corpus).join("rulescore-work"),
    };
    std::fs::create_dir_all(&work_root)
        .map_err(|e| format!("cannot create {}: {e}", work_root.display()))?;
    let work_root = work_root.canonicalize().map_err(|e| e.to_string())?;
    let workspace = Workspace::new(&work_root, &corpus).map_err(|e| e.to_string())?;

    // Fingerprint every unit's sources once, in parallel.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    workspace.warm(&units, threads.max(options.jobs));
    // Cached units first; the rest are staged, indexed and run.
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(units.len());
    let mut pending = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        match workspace.cached(unit, &hash) {
            Some(cache) => outcomes.push(Outcome::Done(cache)),
            None => {
                outcomes.push(Outcome::Pending);
                pending.push(index);
            }
        }
    }
    // Units indexed before (by a cut run) first: they cost no index.
    pending.sort_by_key(|index| !workspace.is_fresh(&units[*index]));
    let cached = units.len() - pending.len();
    if cached > 0 {
        progress(&format!(
            "{cached} of {} units scored before (cached)",
            units.len()
        ));
    }
    let binary = if pending.is_empty() {
        PathBuf::new()
    } else {
        index_binary(options)?
    };
    let mut indexed = 0;
    run_pending(
        &units,
        &pending,
        &workspace,
        &binary,
        options,
        deadline,
        &rules,
        &hash,
        kind,
        &mut outcomes,
        &mut indexed,
        progress,
    );

    let mut counts = UnitCounts {
        total: units.len(),
        indexed,
        ..Default::default()
    };
    let mut failures = BTreeMap::new();
    let mut done: HashMap<&str, &UnitCache> = HashMap::new();
    for (unit, outcome) in units.iter().zip(&outcomes) {
        match outcome {
            Outcome::Done(cache) => {
                counts.done += 1;
                done.insert(unit.name.as_str(), cache);
            }
            Outcome::Failed(status) => {
                counts.failed += 1;
                if failures.len() < MAX_FAILURES_LISTED {
                    failures.insert(unit.name.clone(), status.clone());
                }
            }
            Outcome::Pending => counts.pending += 1,
        }
    }
    let next_cursor = outcomes
        .iter()
        .position(|o| matches!(o, Outcome::Pending))
        .map(|index| format!("{index}:{hash}"));
    let rule_ids: Vec<String> = rules.rules.iter().map(|rule| rule.id.clone()).collect();
    let slack = options
        .slack
        .unwrap_or(if kind.is_differential() { 5 } else { 3 });
    let (metrics, base_rate, positives, scores) = if kind.is_differential() {
        score_differential(&corpus, &units, &done, slack, &rule_ids, &options.policy)?
    } else {
        score_labeled_units(&corpus, &units, &done, slack, &rule_ids, &options.policy)?
    };
    Ok(ScoreReport {
        corpus: corpus.display().to_string(),
        layout: kind.as_str(),
        scoring: if kind.is_differential() {
            "differential"
        } else {
            "labeled"
        },
        complete: next_cursor.is_none(),
        units: counts,
        failures,
        base_rate,
        positives,
        rules: scores,
        next_cursor,
        elapsed_ms: started.elapsed().as_millis() as u64,
        work: workspace.root.display().to_string(),
        metrics,
    })
}

/// Stage and index the pending units on `options.jobs` workers while this
/// thread runs the rules over each unit as its index is ready.
#[allow(clippy::too_many_arguments)]
fn run_pending(
    units: &[Unit],
    pending: &[usize],
    workspace: &Workspace,
    binary: &Path,
    options: &ScoreOptions,
    deadline: Option<Instant>,
    rules: &RuleSet,
    hash: &str,
    kind: CorpusKind,
    outcomes: &mut [Outcome],
    indexed: &mut usize,
    progress: &dyn Fn(&str),
) {
    if pending.is_empty() {
        return;
    }
    let queue = Arc::new(Mutex::new(
        pending
            .iter()
            .copied()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let cancel = AtomicBool::new(false);
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);
    // Every call starts at least one unit, so a deadline shorter than one
    // index still makes progress.
    let started = AtomicBool::new(false);
    let total = units.len();
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, Result<bool, PrepareError>)>();
        for _ in 0..options.jobs.max(1).min(pending.len()) {
            let tx = tx.clone();
            let queue = Arc::clone(&queue);
            let cancel = &cancel;
            let started = &started;
            scope.spawn(move || {
                loop {
                    let first = !started.swap(true, std::sync::atomic::Ordering::SeqCst);
                    if (expired() && !first) || cancel.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let Some(index) = queue.lock().ok().and_then(|mut q| q.pop_front()) else {
                        break;
                    };
                    let result = workspace.prepare(
                        &units[index],
                        binary,
                        options.index_timeout,
                        deadline,
                        cancel,
                        options.detach,
                    );
                    if tx.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut scored_now = 0usize;
        for (index, result) in rx {
            let unit = &units[index];
            match result {
                Ok(fresh) => {
                    *indexed += usize::from(fresh);
                    if expired() && scored_now > 0 {
                        // Indexed (and stamped): the next run scores it.
                        continue;
                    }
                    match run_unit(workspace, unit, rules, kind) {
                        Ok(cache) => {
                            scored_now += 1;
                            if workspace
                                .store(
                                    unit,
                                    &UnitCache {
                                        rules: hash.to_string(),
                                        ..cache.clone()
                                    },
                                )
                                .is_err()
                            {
                                progress(&format!("{}: cannot cache its findings", unit.name));
                            }
                            progress(&format!(
                                "[{}/{total}] {}: {} findings{}",
                                index + 1,
                                unit.name,
                                cache.findings.len(),
                                if fresh { "" } else { " (index reused)" }
                            ));
                            outcomes[index] = Outcome::Done(UnitCache {
                                rules: hash.to_string(),
                                ..cache
                            });
                        }
                        Err(error) => {
                            progress(&format!("{}: rules-failed: {error}", unit.name));
                            outcomes[index] = Outcome::Failed(format!("rules-failed: {error}"));
                        }
                    }
                }
                Err(PrepareError::Interrupted) => {}
                Err(error) => {
                    let detail = match &error {
                        PrepareError::Stage(e) | PrepareError::IndexFailed(e) => format!(": {e}"),
                        _ => String::new(),
                    };
                    progress(&format!("{}: {}{detail}", unit.name, error.status()));
                    outcomes[index] = Outcome::Failed(error.status().to_string());
                }
            }
        }
    });
}

/// Run `rules` over one unit's index.
fn run_unit(
    workspace: &Workspace,
    unit: &Unit,
    rules: &RuleSet,
    kind: CorpusKind,
) -> Result<UnitCache, String> {
    let root = workspace.unit_dir(unit);
    let cg = CodeGraph::open(&root, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut project = Project::load(&cg, &root)?;
        let options = BugsOptions {
            detectors: vec![Detector::Rule],
            only_files: None,
            include_tests: true,
            taint_budget: None,
            dependency_summaries: None,
        };
        let findings = super::detect(&cg, &mut project, rules, &options)?;
        let findings = findings
            .into_iter()
            .map(|finding| ScoredFinding {
                unit: unit.name.clone(),
                rule_id: finding.rule.to_string(),
                file: format!("{}{}", unit.prefix, finding.file),
                local_file: finding.file,
                line: i64::from(finding.line),
                function: finding.function,
                message: finding.message,
                confidence: finding.confidence,
            })
            .collect();
        let vuln_side = kind.is_differential() && matches!(unit.pair, Some((_, PairSide::Vuln)));
        let functions = if vuln_side {
            project
                .files()
                .iter()
                .flat_map(|file| project.functions_in(file))
                .map(|span| {
                    (
                        format!("{}{}", unit.prefix, span.file),
                        i64::from(span.start_line),
                        i64::from(span.end_line.max(span.start_line)),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(UnitCache {
            rules: String::new(),
            findings,
            functions,
        })
    })();
    cg.close();
    result
}

type Scored = (serde_json::Value, Option<f64>, usize, Vec<RuleScore>);

fn score_labeled_units(
    corpus: &Path,
    units: &[Unit],
    done: &HashMap<&str, &UnitCache>,
    slack: i64,
    rule_ids: &[String],
    policy: &Policy,
) -> Result<Scored, String> {
    let whole = units.iter().any(|u| u.sources == ["."]);
    let staged: BTreeSet<&str> = units
        .iter()
        .filter(|u| done.contains_key(u.name.as_str()))
        .flat_map(|u| u.sources.iter().map(String::as_str))
        .collect();
    let keep = |file: &str| staged.contains(file);
    let rows = gt::load_rows(
        &corpus.join("ground_truth.jsonl"),
        if whole { None } else { Some(&keep) },
    )?;
    let findings: Vec<ScoredFinding> = units
        .iter()
        .filter_map(|u| done.get(u.name.as_str()))
        .flat_map(|cache| cache.findings.iter().cloned())
        .collect();
    let (metrics, _) = labeled::score_labeled(&findings, &rows, slack);
    let scores = rule_ids
        .iter()
        .map(|id| {
            let row = metrics.per_rule.get(id);
            let (findings, tp, fp) = row.map_or((0, 0, 0), |r| (r.findings, r.tp, r.fp));
            let (verdict, reason) = verdict::judge(
                findings,
                tp,
                tp + fp,
                metrics.base_rate,
                policy,
                "labeled findings",
            );
            RuleScore {
                id: id.clone(),
                findings,
                tp,
                fp: Some(fp),
                unlabeled: Some(row.map_or(0, |r| r.unlabeled)),
                not_discriminating: None,
                off_fix: None,
                background: None,
                precision: row.and_then(|r| r.precision),
                recall: row.and_then(|r| r.recall).or(Some(0.0)),
                verdict,
                reason,
            }
        })
        .collect();
    let mut json = serde_json::to_value(&metrics).map_err(|e| e.to_string())?;
    json["slack"] = serde_json::Value::from(slack);
    Ok((json, metrics.base_rate, metrics.rows.bad, scores))
}

fn score_differential(
    corpus: &Path,
    units: &[Unit],
    done: &HashMap<&str, &UnitCache>,
    slack: i64,
    rule_ids: &[String],
    policy: &Policy,
) -> Result<Scored, String> {
    let advisories: HashMap<String, corpus::Advisory> = corpus::load_advisories(corpus)?
        .into_iter()
        .map(|a| (a.id.clone(), a))
        .collect();
    let ids: BTreeSet<&str> = units
        .iter()
        .filter_map(|u| u.pair.as_ref().map(|(id, _)| id.as_str()))
        .collect();
    let prefixes: Vec<String> = ids.iter().map(|id| format!("{id}/")).collect();
    let keep = |file: &str| prefixes.iter().any(|p| file.starts_with(p.as_str()));
    let rows = gt::load_rows(&corpus.join("ground_truth.jsonl"), Some(&keep))?;
    let mut rows_by_adv: HashMap<&str, Vec<&gt::GtRow>> = HashMap::new();
    for row in &rows {
        let advisory = row.file.split('/').next().unwrap_or("");
        rows_by_adv.entry(advisory).or_default().push(row);
    }
    let mut pairs = Vec::new();
    let (mut region_functions, mut changed_functions) = (0usize, 0usize);
    for id in &ids {
        let Some(advisory) = advisories.get(*id) else {
            continue;
        };
        let (Some(vuln), Some(fixed)) = (
            done.get(advisory.vuln_dir.as_str()),
            done.get(advisory.fixed_dir.as_str()),
        ) else {
            continue;
        };
        let mut categories = if advisory.categories.is_empty() {
            vec!["(none)".to_string()]
        } else {
            advisory.categories.clone()
        };
        if advisory.unsound {
            categories.push("informational:unsound".to_string());
        }
        let advisory_rows = rows_by_adv.get(id).map_or(&[][..], Vec::as_slice);
        let region = differential::region_rows(id, advisory_rows);
        if !region.is_empty() {
            // A finding that vanishes with the fix sits in a function the
            // fix changed; the chance one lands in the fix region is the
            // share of changed functions that are in it.
            let vuln_prefix = format!("{id}/vuln/");
            let changed: Vec<&gt::GtRow> = advisory_rows
                .iter()
                .copied()
                .filter(|row| row.file.starts_with(&vuln_prefix))
                .collect();
            let touches = |rows: &[&gt::GtRow], (file, start, end): &(String, i64, i64)| {
                rows.iter()
                    .any(|row| row.file == *file && row.overlaps(*start, *end, slack))
            };
            let region_refs: Vec<&gt::GtRow> = region.iter().collect();
            for function in &vuln.functions {
                if touches(&changed, function) {
                    changed_functions += 1;
                    region_functions += usize::from(touches(&region_refs, function));
                }
            }
        }
        pairs.push(differential::PairInput {
            advisory: id.to_string(),
            categories,
            localization: advisory.localization.clone(),
            vuln: vuln.findings.iter().collect(),
            fixed: fixed.findings.iter().collect(),
            region,
        });
    }
    let (metrics, _) = differential::score_pairs(&pairs, slack);
    let base_rate = ratio(region_functions, changed_functions);
    let scores = rule_ids
        .iter()
        .map(|id| {
            let row = metrics.per_rule.get(id);
            let get = |f: fn(&differential::DiffRow) -> usize| row.map_or(0, f);
            let (tp, off) = (get(|r| r.tp), get(|r| r.vuln_only_elsewhere));
            let findings = get(|r| r.vuln_findings);
            let (verdict, reason) = verdict::judge(
                findings,
                tp,
                tp + off,
                base_rate,
                policy,
                "differential findings",
            );
            RuleScore {
                id: id.clone(),
                findings,
                tp,
                fp: None,
                unlabeled: None,
                not_discriminating: Some(get(|r| r.not_discriminating)),
                off_fix: Some(off),
                background: Some(get(|r| r.background)),
                precision: row.and_then(|r| r.precision_differential),
                recall: row.and_then(|r| r.recall_pairs).or(Some(0.0)),
                verdict,
                reason,
            }
        })
        .collect();
    let json = serde_json::to_value(&metrics).map_err(|e| e.to_string())?;
    Ok((json, base_rate, metrics.scoreable_pairs, scores))
}
