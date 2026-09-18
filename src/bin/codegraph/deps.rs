//! `codegraph deps …` — the shared dependency-graph store.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use codegraph::deps::builder::{BuildOptions, BuildReport, ShardResult, build_pending};
use codegraph::deps::gc::{GcPolicy, gc};
use codegraph::deps::locate::SourceRoots;
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::{PackageRow, PendingScope};
use codegraph::deps::scope::ShardLimits;
use codegraph::deps::store::StoreLock;
use codegraph::deps::trigger::DEFAULT_BACKGROUND_BUDGET_MS;
use codegraph::deps::{
    DepKey,
    DepSource,
    DepsHome,
    Ecosystem,
    Registry,
    ShardHandle,
    ShardMeta,
    ShardState,
    lockfile,
};
use codegraph::directory::is_git_checkout_root;
use serde::Serialize;

use super::{
    DepsCommands,
    bold,
    dim,
    error_msg,
    format_duration,
    format_number,
    green,
    is_initialized,
    now_ms,
    process,
    red,
    resolve_absolute,
    yellow,
};

pub(crate) async fn cmd_deps(command: DepsCommands) {
    let result = match command {
        DepsCommands::List {
            project,
            direct,
            ecosystem,
            json,
        } => cmd_list(project.as_deref(), direct, ecosystem.as_deref(), json),
        DepsCommands::Status { json } => cmd_status(json),
        DepsCommands::Record {
            project,
            force,
            json,
        } => cmd_record(project.as_deref(), force, json),
        DepsCommands::Build {
            project,
            all,
            direct_only,
            budget_ms,
            shard_budget_ms,
            max_files,
            max_mb,
            max_db_mb,
            force,
            background,
            quiet,
            json,
        } => {
            let defaults = ShardLimits::default();
            let limits = ShardLimits {
                max_files: max_files.unwrap_or(defaults.max_files),
                max_bytes: max_mb.map_or(defaults.max_bytes, |mb| mb * 1024 * 1024),
                max_file_bytes: defaults.max_file_bytes,
                time_ms: shard_budget_ms.unwrap_or(defaults.time_ms),
                max_db_bytes: max_db_mb.map_or(defaults.max_db_bytes, |mb| mb * 1024 * 1024),
            };
            let budget = match budget_ms {
                Some(0) => None,
                Some(ms) => Some(Duration::from_millis(ms)),
                None => background.then_some(Duration::from_millis(DEFAULT_BACKGROUND_BUDGET_MS)),
            };
            let request = BuildRequestArgs {
                project,
                all,
                background,
                quiet: quiet || background,
                json,
                options: BuildOptions {
                    limits,
                    budget,
                    force,
                    direct_only,
                },
            };
            cmd_build(request).await
        }
        DepsCommands::Gc {
            max_age_days,
            max_size_mb,
            dry_run,
            json,
        } => cmd_gc(
            &GcPolicy {
                max_age: Duration::from_secs(max_age_days * 24 * 3600),
                max_total_bytes: (max_size_mb > 0).then(|| max_size_mb * 1024 * 1024),
                dry_run,
            },
            json,
        ),
        DepsCommands::Show {
            spec,
            ecosystem,
            symbol,
            json,
        } => cmd_show(&spec, ecosystem.as_deref(), symbol.as_deref(), json),
    };
    if let Err(message) = result {
        error_msg(&message);
        process::exit(1);
    }
}

type CmdResult = Result<(), String>;

/// The project a command means: `--project` as given, else the nearest
/// indexed or git-checkout directory at or above the current one.
fn project_root(arg: Option<&str>) -> PathBuf {
    let start = resolve_absolute(arg);
    if arg.is_some() {
        return start;
    }
    start
        .ancestors()
        .find(|dir| is_initialized(dir) || is_git_checkout_root(dir))
        .unwrap_or(&start)
        .to_path_buf()
}

fn parse_ecosystem(value: Option<&str>) -> Result<Option<Ecosystem>, String> {
    value.map(str::parse::<Ecosystem>).transpose()
}

fn print_json<T: Serialize>(value: &T) -> CmdResult {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn state_label(state: ShardState) -> String {
    match state {
        ShardState::Ready => green(state.as_str()),
        ShardState::Partial | ShardState::Building => yellow(state.as_str()),
        ShardState::Failed => red(state.as_str()),
        ShardState::Missing | ShardState::Unavailable => dim(state.as_str()),
    }
}

fn direct_label(direct: Option<bool>) -> &'static str {
    match direct {
        Some(true) => "direct",
        Some(false) => "transitive",
        None => "-",
    }
}

// ---------------------------------------------------------------------------
// list

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListRow {
    key: DepKey,
    lock_version: String,
    source: DepSource,
    direct: Option<bool>,
    lockfile: String,
    source_dir: Option<String>,
    state: ShardState,
    shard: Option<ShardSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShardSummary {
    files: usize,
    nodes: u64,
    edges: u64,
    db_bytes: u64,
    partial_reasons: Vec<codegraph::deps::scope::PartialReason>,
    stale: bool,
}

impl ShardSummary {
    fn of(meta: &ShardMeta) -> Self {
        Self {
            files: meta.counts.indexed_files,
            nodes: meta.counts.nodes,
            edges: meta.counts.edges,
            db_bytes: meta.db_bytes,
            partial_reasons: meta.partial_reasons.clone(),
            stale: !meta.is_current(),
        }
    }
}

fn cmd_list(
    project: Option<&str>,
    direct_only: bool,
    ecosystem: Option<&str>,
    json: bool,
) -> CmdResult {
    let ecosystem = parse_ecosystem(ecosystem)?;
    let root = PathBuf::from(canonical_root(&project_root(project)));
    let home = DepsHome::from_env();
    let registry = Registry::open_read_only(&home.registry_path()).map_err(|e| e.to_string())?;
    let roots = SourceRoots::from_env();
    let manifest = lockfile::read_project(&root);
    let mut rows: Vec<ListRow> = Vec::new();
    for dep in manifest.deps {
        if ecosystem.is_some_and(|e| e != dep.key.ecosystem)
            || (direct_only && dep.direct != Some(true))
        {
            continue;
        }
        let source_dir = roots.locate(&dep, &root);
        let meta = ShardMeta::read(&home.shard_dir(&dep.key)).filter(|m| m.key() == dep.key);
        let state = match (&meta, &dep.source, &source_dir) {
            (Some(meta), _, _) => meta.state,
            (None, DepSource::Path { .. }, _) | (None, _, None) => ShardState::Unavailable,
            (None, _, Some(_)) => registry
                .as_ref()
                .and_then(|r| r.package(&dep.key).ok().flatten())
                .map(|row| effective_state(&home, &row))
                .filter(|s| !s.has_shard())
                .unwrap_or(ShardState::Missing),
        };
        rows.push(ListRow {
            shard: meta.as_ref().map(ShardSummary::of),
            key: dep.key,
            lock_version: dep.lock_version,
            source: dep.source,
            direct: dep.direct,
            lockfile: dep.lockfile,
            source_dir: source_dir.map(|d| d.to_string_lossy().into_owned()),
            state,
        });
    }
    rows.sort_by(|a, b| {
        (
            a.key.ecosystem,
            a.direct != Some(true),
            &a.key.name,
            &a.key.version,
        )
            .cmp(&(
                b.key.ecosystem,
                b.direct != Some(true),
                &b.key.name,
                &b.key.version,
            ))
    });
    if json {
        return print_json(&serde_json::json!({
            "root": root,
            "lockfiles": manifest.lockfiles,
            "errors": manifest.errors,
            "dependencies": rows,
        }));
    }
    println!("{} {}", bold("Dependencies of"), root.display());
    if manifest.lockfiles.is_empty() {
        println!("{}", dim("  no lockfiles found"));
        return Ok(());
    }
    for lock in &manifest.lockfiles {
        println!("  {} {}", dim("lockfile"), lock.rel_path);
    }
    for error in &manifest.errors {
        println!("  {} {error}", red("error"));
    }
    for row in &rows {
        let shard = row.shard.as_ref().map_or(String::new(), |s| {
            format!(
                "  {} files, {} nodes, {}{}",
                format_number(s.files as u64),
                format_number(s.nodes),
                fmt_bytes(s.db_bytes),
                if s.stale { ", stale" } else { "" }
            )
        });
        println!(
            "  {:<6} {:<40} {:<10} {}{}",
            row.key.ecosystem.as_str(),
            format!("{} {}", row.key.name, row.key.version),
            direct_label(row.direct),
            state_label(row.state),
            dim(&shard)
        );
    }
    let count = |s: ShardState| rows.iter().filter(|r| r.state == s).count();
    println!(
        "{} {} dependencies: {} ready, {} partial, {} missing, {} unavailable, {} failed",
        dim("·"),
        rows.len(),
        count(ShardState::Ready),
        count(ShardState::Partial),
        count(ShardState::Missing) + count(ShardState::Building),
        count(ShardState::Unavailable),
        count(ShardState::Failed)
    );
    Ok(())
}

/// A registry row's state, with `building` read as `missing` when no
/// builder actually holds the shard's lock (a build that died).
fn effective_state(home: &DepsHome, row: &PackageRow) -> ShardState {
    if row.state == ShardState::Building && !StoreLock::is_held(&home.shard_lock_path(&row.key())) {
        ShardState::Missing
    } else {
        row.state
    }
}

// ---------------------------------------------------------------------------
// status

fn cmd_status(json: bool) -> CmdResult {
    let home = DepsHome::from_env();
    let registry = Registry::open_read_only(&home.registry_path()).map_err(|e| e.to_string())?;
    let disk = dir_size(home.root());
    let builder_running = StoreLock::is_held(&home.builder_lock_path());
    let Some(registry) = registry else {
        if json {
            return print_json(
                &serde_json::json!({ "home": home.root(), "recorded": false, "diskBytes": disk }),
            );
        }
        println!(
            "{}",
            dim(&format!(
                "No dependency registry yet at {}",
                home.root().display()
            ))
        );
        return Ok(());
    };
    let status = registry.status().map_err(|e| e.to_string())?;
    if json {
        return print_json(&serde_json::json!({
            "home": home.root(),
            "recorded": true,
            "diskBytes": disk,
            "builderRunning": builder_running,
            "status": status,
        }));
    }
    println!("{} {}", bold("Dependency store"), home.root().display());
    println!(
        "  {} projects, {} dependency versions, {} usages",
        format_number(status.projects),
        format_number(status.versions),
        format_number(status.usages)
    );
    for row in &status.by_state {
        println!(
            "  {:<6} {:<12} {:>6}{}",
            row.ecosystem.as_str(),
            state_label(row.state),
            format_number(row.count),
            if row.shard_bytes > 0 {
                format!("  {}", fmt_bytes(row.shard_bytes))
            } else {
                String::new()
            }
        );
    }
    println!(
        "  shards {}, store on disk {}{}",
        fmt_bytes(status.shard_bytes),
        fmt_bytes(disk),
        if builder_running {
            ", builder running"
        } else {
            ""
        }
    );
    Ok(())
}

fn dir_size(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

// ---------------------------------------------------------------------------
// record

fn open_registry(home: &DepsHome) -> Result<Registry, String> {
    home.ensure().map_err(|e| e.to_string())?;
    Registry::open(&home.registry_path()).map_err(|e| e.to_string())
}

fn cmd_record(project: Option<&str>, force: bool, json: bool) -> CmdResult {
    let home = DepsHome::from_env();
    let mut registry = open_registry(&home)?;
    let root = project_root(project);
    let report = record_project(
        &mut registry,
        &root,
        &SourceRoots::from_env(),
        force,
        now_ms(),
    )
    .map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    print_record(&report);
    Ok(())
}

fn print_record(report: &codegraph::deps::project::RecordReport) {
    if report.unchanged {
        println!(
            "{} {} {}",
            bold("Recorded"),
            report.root,
            dim("(lockfiles unchanged)")
        );
        return;
    }
    println!(
        "{} {} {}",
        bold("Recorded"),
        report.root,
        dim(&format!("({} lockfiles)", report.lockfiles.len()))
    );
    for (ecosystem, counts) in &report.by_ecosystem {
        println!(
            "  {:<6} {} dependencies ({} direct): {} sources found, {} unavailable, {} local path",
            ecosystem.as_str(),
            counts.dependencies,
            counts.direct,
            counts.located,
            counts.unavailable,
            counts.path
        );
    }
    for error in &report.errors {
        println!("  {} {error}", red("error"));
    }
}

// ---------------------------------------------------------------------------
// build

struct BuildRequestArgs {
    project: Option<String>,
    all: bool,
    background: bool,
    quiet: bool,
    json: bool,
    options: BuildOptions,
}

async fn cmd_build(args: BuildRequestArgs) -> CmdResult {
    let home = DepsHome::from_env();
    home.ensure().map_err(|e| e.to_string())?;
    // One background builder at a time; it drains every project's queue.
    let _builder = if args.background {
        match StoreLock::try_acquire(&home.builder_lock_path()) {
            Ok(Some(lock)) => Some(lock),
            _ => return Ok(()),
        }
    } else {
        None
    };
    let mut registry = open_registry(&home)?;
    let roots = SourceRoots::from_env();
    let root = (!args.all || args.project.is_some()).then(|| project_root(args.project.as_deref()));
    if let Some(root) = &root {
        let report = record_project(&mut registry, root, &roots, true, now_ms())
            .map_err(|e| e.to_string())?;
        if !args.quiet && !args.json {
            print_record(&report);
        }
    }
    let canonical = root.as_deref().map(canonical_root);
    let scope = match (&canonical, args.all) {
        (Some(root), false) => PendingScope::Project(root),
        _ => PendingScope::All,
    };
    let quiet = args.quiet || args.json;
    let mut print = |result: &ShardResult| {
        if !quiet {
            print_shard_result(result);
        }
    };
    let started = std::time::Instant::now();
    let mut report = build_pending(&home, &registry, scope, &args.options, &mut print)
        .await
        .map_err(|e| e.to_string())?;
    if args.background && matches!(scope, PendingScope::Project(_)) {
        let left = args
            .options
            .budget
            .map(|b| b.saturating_sub(started.elapsed()));
        if left.is_none_or(|l| !l.is_zero()) {
            let options = BuildOptions {
                budget: left,
                ..args.options
            };
            let rest = build_pending(&home, &registry, PendingScope::All, &options, &mut print)
                .await
                .map_err(|e| e.to_string())?;
            merge(&mut report, rest);
        }
    }
    if args.json {
        return print_json(&report);
    }
    if !quiet {
        print_build_totals(&report);
    }
    Ok(())
}

fn merge(into: &mut BuildReport, more: BuildReport) {
    into.results.extend(more.results);
    into.remaining = more.remaining;
    into.elapsed_ms += more.elapsed_ms;
}

fn print_shard_result(result: &ShardResult) {
    match result {
        ShardResult::Built { meta } | ShardResult::UpToDate { meta } => {
            let verb = if matches!(result, ShardResult::Built { .. }) {
                "built"
            } else {
                "up to date"
            };
            println!(
                "  {:<6} {:<40} {} {}",
                meta.ecosystem.as_str(),
                format!("{} {}", meta.name, meta.version),
                state_label(meta.state),
                dim(&format!(
                    "{verb}: {}/{} files, {} nodes, {} edges, {} in {}{}",
                    format_number(meta.counts.indexed_files as u64),
                    format_number(meta.counts.candidate_files as u64),
                    format_number(meta.counts.nodes),
                    format_number(meta.counts.edges),
                    fmt_bytes(meta.db_bytes),
                    format_duration(meta.build_ms as i64),
                    if meta.partial_reasons.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (partial: {})",
                            meta.partial_reasons
                                .iter()
                                .map(|r| serde_json::to_string(r)
                                    .unwrap_or_default()
                                    .trim_matches('"')
                                    .to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ))
            );
        }
        ShardResult::Locked { key } => println!(
            "  {:<6} {:<40} {}",
            key.ecosystem.as_str(),
            format!("{} {}", key.name, key.version),
            yellow("locked (another builder)")
        ),
        ShardResult::Failed { key, error } => println!(
            "  {:<6} {:<40} {} {}",
            key.ecosystem.as_str(),
            format!("{} {}", key.name, key.version),
            red("failed"),
            dim(error)
        ),
        ShardResult::NoSources { key } => println!(
            "  {:<6} {:<40} {}",
            key.ecosystem.as_str(),
            format!("{} {}", key.name, key.version),
            dim("unavailable (no library sources)")
        ),
        ShardResult::SourceGone { key } => println!(
            "  {:<6} {:<40} {}",
            key.ecosystem.as_str(),
            format!("{} {}", key.name, key.version),
            dim("source gone")
        ),
    }
}

fn print_build_totals(report: &BuildReport) {
    let bytes: u64 = report
        .results
        .iter()
        .filter_map(|r| match r {
            ShardResult::Built { meta } => Some(meta.db_bytes),
            _ => None,
        })
        .sum();
    println!(
        "{} built {} shards ({}), {} failed{} in {}",
        dim("·"),
        report.built(),
        fmt_bytes(bytes),
        report.failed(),
        if report.remaining > 0 {
            format!(", {} left for the next run (budget)", report.remaining)
        } else {
            String::new()
        },
        format_duration(report.elapsed_ms as i64)
    );
}

// ---------------------------------------------------------------------------
// gc

fn cmd_gc(policy: &GcPolicy, json: bool) -> CmdResult {
    let home = DepsHome::from_env();
    let Some(registry) = (if policy.dry_run {
        Registry::open_read_only(&home.registry_path()).map_err(|e| e.to_string())?
    } else if home.registry_path().is_file() {
        Some(open_registry(&home)?)
    } else {
        None
    }) else {
        println!("{}", dim("No dependency registry yet"));
        return Ok(());
    };
    let report = gc(&home, &registry, policy, SystemTime::now()).map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    let verb = if policy.dry_run {
        "Would remove"
    } else {
        "Removed"
    };
    for project in &report.projects_forgotten {
        println!("  {} {project}", dim("forgot vanished project"));
    }
    for removed in &report.removed {
        println!(
            "  {verb} {} {} {} {}",
            removed.key.ecosystem.as_str(),
            removed.key.name,
            removed.key.version,
            dim(&format!(
                "({}, {})",
                fmt_bytes(removed.bytes),
                serde_json::to_string(&removed.reason)
                    .unwrap_or_default()
                    .trim_matches('"')
            ))
        );
    }
    println!(
        "{} {verb} {} shards ({}), {} build leftovers; {} kept",
        dim("·"),
        report.removed.len(),
        fmt_bytes(report.bytes_freed),
        report.leftovers_removed,
        fmt_bytes(report.bytes_kept)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// show

/// `name@version` → (name, Some(version)); a scoped npm name's leading `@`
/// is part of the name.
fn split_spec(spec: &str) -> (&str, Option<&str>) {
    match spec.rfind('@') {
        Some(at) if at > 0 => (&spec[..at], Some(&spec[at + 1..])),
        _ => (spec, None),
    }
}

fn cmd_show(spec: &str, ecosystem: Option<&str>, symbol: Option<&str>, json: bool) -> CmdResult {
    let ecosystem = parse_ecosystem(ecosystem)?;
    let (name, version) = split_spec(spec);
    let home = DepsHome::from_env();
    let registry = Registry::open_read_only(&home.registry_path()).map_err(|e| e.to_string())?;
    let mut rows: Vec<PackageRow> = match &registry {
        Some(r) => r
            .packages_named(ecosystem, name)
            .map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    if let Some(version) = version {
        rows.retain(|r| r.version == version || r.version.starts_with(&format!("{version}+")));
    }
    if rows.is_empty() {
        return Err(format!(
            "{spec} is not recorded for any project (try `codegraph deps record`)"
        ));
    }
    let mut out = Vec::new();
    for row in &rows {
        let key = row.key();
        let users = registry
            .as_ref()
            .map(|r| r.users_of(&key))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let handle = ShardHandle::open(&home, &key);
        let symbols = match (&handle, symbol) {
            (Some(handle), Some(query)) => handle
                .lookup(query)
                .map_err(|e| e.to_string())?
                .into_iter()
                .take(50)
                .map(|n| {
                    serde_json::json!({
                        "kind": n.kind,
                        "name": n.name,
                        "qualifiedName": n.qualified_name,
                        "file": n.file_path,
                        "line": n.start_line,
                        "signature": n.signature,
                    })
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        out.push(serde_json::json!({
            "package": row,
            "state": effective_state(&home, row),
            "shardDir": home.shard_dir(&key),
            "meta": handle.as_ref().map(ShardHandle::meta),
            "users": users,
            "symbols": symbols,
        }));
        if !json {
            println!(
                "{} {} {} {}",
                bold(&format!("{} {}", row.name, row.version)),
                dim(row.ecosystem.as_str()),
                state_label(effective_state(&home, row)),
                dim(&format!("({} projects)", users.len()))
            );
            if let Some(handle) = &handle {
                let meta = handle.meta();
                println!("  shard   {}", handle.dir().display());
                println!("  source  {}", meta.source_dir);
                println!(
                    "  {} of {} library files, {} nodes, {} edges, {}, built in {}{}",
                    format_number(meta.counts.indexed_files as u64),
                    format_number(meta.counts.candidate_files as u64),
                    format_number(meta.counts.nodes),
                    format_number(meta.counts.edges),
                    fmt_bytes(meta.db_bytes),
                    format_duration(meta.build_ms as i64),
                    if handle.is_stale() {
                        " (stale extractor)"
                    } else {
                        ""
                    }
                );
            } else if let Some(error) = &row.error {
                println!("  {} {error}", red("error"));
            }
            for user in users.iter().take(10) {
                println!("  {} {user}", dim("used by"));
            }
            if users.len() > 10 {
                println!("  {}", dim(&format!("… and {} more", users.len() - 10)));
            }
            for s in &symbols {
                println!(
                    "  {} {} {}:{}",
                    s["kind"].as_str().unwrap_or(""),
                    s["qualifiedName"].as_str().unwrap_or(""),
                    s["file"].as_str().unwrap_or(""),
                    s["line"]
                );
            }
            if symbol.is_some() && handle.is_some() && symbols.is_empty() {
                println!("  {}", dim("no matching symbols"));
            }
        }
    }
    if json {
        return print_json(&out);
    }
    Ok(())
}
