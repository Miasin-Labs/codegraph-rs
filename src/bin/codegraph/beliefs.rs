//! `codegraph deps beliefs …` — ecosystem beliefs mined from the cargo cache.

use std::time::Duration;

use codegraph::deps::DepsHome;
use codegraph::deps::beliefs::build::{BuildOptions, Progress, build};
use codegraph::deps::beliefs::model::{Belief, BeliefKind, api_path};
use codegraph::deps::beliefs::{load, profile, spawn_background_build};
use codegraph::deps::locate::SourceRoots;

use super::cli::BeliefsCommands;
use super::{bold, dim, error_msg, format_duration, format_number, green, info, process, red};

pub(crate) fn cmd_beliefs(command: BeliefsCommands) {
    let result = match command {
        BeliefsCommands::Build {
            max_crates,
            budget_ms,
            crate_budget_ms,
            jobs,
            crates,
            force,
            detach,
            background,
            quiet,
            json,
        } => {
            if detach {
                let mut args = vec![
                    "--max-crates".to_string(),
                    max_crates.to_string(),
                    "--budget-ms".to_string(),
                    budget_ms.to_string(),
                    "--crate-budget-ms".to_string(),
                    crate_budget_ms.to_string(),
                ];
                if let Some(jobs) = jobs {
                    args.extend(["--jobs".to_string(), jobs.to_string()]);
                }
                for krate in &crates {
                    args.extend(["--crate".to_string(), krate.clone()]);
                }
                if force {
                    args.push("--force".to_string());
                }
                let started = spawn_background_build(&args);
                info(&format!("Beliefs build: {started:?}"));
                Ok(())
            } else {
                let defaults = BuildOptions::default();
                let options = BuildOptions {
                    max_crates,
                    budget: (budget_ms > 0).then(|| Duration::from_millis(budget_ms)),
                    crate_budget: Duration::from_millis(crate_budget_ms.max(1)),
                    jobs: jobs.unwrap_or(defaults.jobs),
                    only: crates,
                    force,
                    ..defaults
                };
                cmd_build(&options, quiet || background || json, json, background)
            }
        }
        BeliefsCommands::Show {
            api,
            rule,
            profile,
            top,
            json,
        } => cmd_show(api.as_deref(), rule.as_deref(), profile, top, json),
    };
    if let Err(message) = result {
        error_msg(&message);
        process::exit(1);
    }
}

fn cmd_build(
    options: &BuildOptions,
    quiet: bool,
    json: bool,
    background: bool,
) -> Result<(), String> {
    let home = DepsHome::from_env();
    let roots = SourceRoots::from_env();
    let print = |progress: &Progress| {
        if quiet {
            return;
        }
        match progress {
            Progress::Toolchain(Some(rustc)) => println!("  {} {rustc}", dim("toolchain")),
            Progress::Toolchain(None) => println!(
                "  {}",
                dim("no rust-src found: std/core/alloc calls are not observed")
            ),
            Progress::Observed { krate, sites, ms } => println!(
                "  {:<44} {} {}",
                krate,
                green("observed"),
                dim(&format!(
                    "{sites} library call sites in {}",
                    format_duration(*ms as i64)
                ))
            ),
            Progress::Failed { krate, error } => {
                println!("  {:<44} {} {}", krate, red("failed"), dim(error));
            }
            Progress::Mined { crates, beliefs } => println!(
                "  {} {} beliefs from {} crates",
                dim("mined"),
                format_number(*beliefs as u64),
                format_number(*crates as u64)
            ),
        }
    };
    // The build drives its own runtimes on worker threads; keep it off
    // this async one.
    let report = std::thread::scope(|scope| {
        scope
            .spawn(|| build(&home, &roots, options, &print))
            .join()
            .unwrap_or_else(|_| Err("the beliefs build panicked".to_string()))
    });
    let report = match report {
        Err(message) if background && message.contains("is running") => return Ok(()),
        other => other?,
    };
    if json {
        let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        println!("{text}");
        return Ok(());
    }
    if background {
        return Ok(());
    }
    println!(
        "{} {} crates in the cache: {} observed now, {} cached, {} left for a later run, {} \
         failed{}",
        bold("Beliefs"),
        format_number(report.population as u64),
        report.observed,
        report.cached,
        report.remaining,
        report.failed.len(),
        if report.partial > 0 {
            format!(", {} cut short by the per-crate budget", report.partial)
        } else {
            String::new()
        }
    );
    println!(
        "  mined {} crates, {} APIs, {} call sites → {} beliefs {}",
        format_number(report.crates_mined as u64),
        format_number(report.apis as u64),
        format_number(report.sites as u64),
        format_number(report.beliefs as u64),
        dim(&format!("in {}", format_duration(report.elapsed_ms as i64)))
    );
    for (rule, count) in &report.by_rule {
        println!("  {:<34} {}", rule, count);
    }
    println!("  {}", dim(&report.artifact));
    Ok(())
}

fn cmd_show(
    api: Option<&str>,
    rule: Option<&str>,
    want_profile: bool,
    top: usize,
    json: bool,
) -> Result<(), String> {
    let home = DepsHome::from_env();
    if want_profile {
        let api = api.ok_or("--profile needs the API key (`std@1::File::create`)")?;
        let found = profile(&home, api).ok_or_else(|| format!("no crate calls {api}"))?;
        if json {
            let text = serde_json::to_string_pretty(&found).map_err(|e| e.to_string())?;
            println!("{text}");
            return Ok(());
        }
        println!(
            "{} {} — {} sites in {} crates",
            bold("Profile"),
            found.api,
            found.sites,
            found.crates
        );
        println!(
            "  result: {} used, {} discarded, {} handled, {} checked",
            found.used, found.discarded, found.handled, found.checked
        );
        println!(
            "  held across .await: {} of {} async bindings",
            found.await_held, found.await_sites
        );
        for (title, rows) in [
            ("followed by", &found.followed_by),
            ("preceded by", &found.preceded_by),
        ] {
            println!(
                "  {title} (on the same local object, {} sites):",
                found.local_object_sites
            );
            for (other, share, lift) in rows {
                println!(
                    "    {:<52} {:>5.1}%  lift {:.1}",
                    other,
                    share * 100.0,
                    lift
                );
            }
        }
        return Ok(());
    }
    let loaded = load(&home, None).ok_or("no beliefs yet — run `codegraph deps beliefs build`")?;
    let selected: Vec<&Belief> = loaded
        .set
        .beliefs
        .iter()
        .filter(|belief| api.is_none_or(|api| belief.api.contains(api)))
        .filter(|belief| rule.is_none_or(|rule| belief.kind.rule() == rule))
        .take(top)
        .collect();
    if json {
        let text = serde_json::to_string_pretty(&selected).map_err(|e| e.to_string())?;
        println!("{text}");
        return Ok(());
    }
    println!(
        "{} {} beliefs from {} crates ({} APIs, {} sites){}",
        bold("Ecosystem beliefs"),
        loaded.set.beliefs.len(),
        loaded.set.crates,
        loaded.set.apis,
        loaded.set.sites,
        loaded
            .set
            .toolchain
            .as_deref()
            .map(|t| dim(&format!(" — {t}")))
            .unwrap_or_default()
    );
    for belief in selected {
        let what = match &belief.kind {
            BeliefKind::ResultUsed => "result used".to_string(),
            BeliefKind::FollowedBy { others } => format!(
                "followed by {}",
                others
                    .iter()
                    .map(|o| api_path(o))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            BeliefKind::PrecededBy { others } => format!(
                "preceded by {}",
                others
                    .iter()
                    .map(|o| api_path(o))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            BeliefKind::PairedWith { other } => format!("paired with {}", api_path(other)),
            BeliefKind::NotHeldAcrossAwait => "not held across .await".to_string(),
        };
        println!(
            "  {:<46} {:<44} {}/{} crates, {}/{} sites, z {:.1}{}",
            belief.api,
            what,
            belief.support.agree_crates,
            belief.support.total_crates,
            belief.support.agree_sites,
            belief.support.total_sites,
            belief.z,
            belief
                .lift
                .map(|lift| format!(", lift {lift:.1}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}
