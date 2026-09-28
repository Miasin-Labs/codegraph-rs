//! Departures from ecosystem beliefs: a project call to an API the cargo
//! cache agrees about, made the way the agreeing crates do not.

use std::collections::{HashMap, HashSet};

use super::observe::{Observed, ObservedSite};
use crate::analyze::bugs::{Detector, Evidence, Finding};
use crate::deps::beliefs::model::{Belief, BeliefKind, BeliefSet, UseClass, api_name, api_path};

/// Agreeing crates cited per finding.
const MAX_EVIDENCE: usize = 3;

/// The findings `observed` (the project's external calls) gives against
/// `beliefs`, `is_status` gating result beliefs as when mined.
pub(crate) fn findings(
    beliefs: &BeliefSet,
    observed: &Observed,
    is_status: &dyn Fn(&str) -> bool,
) -> Vec<Finding> {
    let mut about: HashMap<&str, Vec<&Belief>> = HashMap::new();
    for belief in &beliefs.beliefs {
        about.entry(belief.api.as_str()).or_default().push(belief);
    }
    let called_apis: HashSet<&str> = observed.apis.iter().map(String::as_str).collect();
    let mut paired_reported: HashSet<(&str, &str)> = HashSet::new();
    let mut out = Vec::new();
    for site in &observed.sites {
        let api = observed.apis[site.obs.api as usize].as_str();
        let Some(beliefs) = about.get(api) else {
            continue;
        };
        let names = |ids: &[u32]| -> Vec<&str> {
            ids.iter()
                .map(|&id| observed.apis[id as usize].as_str())
                .collect()
        };
        for belief in beliefs {
            let message = match &belief.kind {
                BeliefKind::ResultUsed => {
                    let status = belief.returns.as_deref().is_some_and(is_status);
                    if site.obs.use_class != UseClass::Discarded || !status {
                        continue;
                    }
                    format!(
                        "the result of `{}` (`{}`) is dropped here, while {} of {} call sites \
                         in {} of {} crates of the cargo cache use it",
                        api_path(api),
                        belief.returns.as_deref().unwrap_or("?"),
                        belief.support.agree_sites,
                        belief.support.total_sites,
                        belief.support.agree_crates,
                        belief.support.total_crates,
                    )
                }
                BeliefKind::FollowedBy { others } | BeliefKind::PrecededBy { others } => {
                    let after = matches!(belief.kind, BeliefKind::FollowedBy { .. });
                    // A departure is an object left alone: nothing called on
                    // it after (`Command::args(..)` then `.status()` took
                    // another way to finish), or nothing before but its
                    // constructor, which must be in view.
                    let abandoned = if after {
                        site.obs.methods_after == 0
                    } else {
                        site.obs.constructed && site.obs.methods_before == 0
                    };
                    if site.obs.object.is_none() || site.obs.escapes || !abandoned {
                        continue;
                    }
                    let seen = names(if after {
                        &site.obs.after
                    } else {
                        &site.obs.before
                    });
                    if others.iter().any(|other| seen.contains(&other.as_str())) {
                        continue;
                    }
                    format!(
                        "`{}` here is {} on the same object by {}, while {} of {} local uses in \
                         {} of {} crates of the cargo cache are (lift {:.1})",
                        api_path(api),
                        if after {
                            "not followed"
                        } else {
                            "not preceded"
                        },
                        either(others),
                        belief.support.agree_sites,
                        belief.support.total_sites,
                        belief.support.agree_crates,
                        belief.support.total_crates,
                        belief.lift.unwrap_or(0.0),
                    )
                }
                BeliefKind::PairedWith { other } => {
                    if called_apis.contains(other.as_str())
                        || observed.called_names.contains(api_name(other))
                        || !paired_reported.insert((api, other.as_str()))
                    {
                        continue;
                    }
                    format!(
                        "the project calls `{}` but never `{}`, which {} of {} crates of the \
                         cargo cache calling it also call",
                        api_path(api),
                        api_path(other),
                        belief.support.agree_crates,
                        belief.support.total_crates,
                    )
                }
                BeliefKind::NotHeldAcrossAwait => {
                    if site.obs.await_held != Some(true) {
                        continue;
                    }
                    format!(
                        "the value `{}` returns is still alive at a later `.await` in its block, \
                         while {} of {} such bindings in {} of {} crates of the cargo cache are \
                         dropped first",
                        api_path(api),
                        belief.support.agree_sites,
                        belief.support.total_sites,
                        belief.support.agree_crates,
                        belief.support.total_crates,
                    )
                }
            };
            out.push(finding(site, belief, message));
        }
    }
    out
}

fn finding(site: &ObservedSite, belief: &Belief, message: String) -> Finding {
    Finding {
        detector: Detector::Deviance,
        rule: belief.kind.rule().into(),
        file: site.obs.file.clone(),
        line: site.obs.line,
        col: site.col,
        function: site.function.clone(),
        message,
        confidence: belief.confidence,
        evidence: belief
            .evidence
            .iter()
            .take(MAX_EVIDENCE)
            .map(|example| Evidence {
                file: format!("{}/{}", example.krate, example.file),
                line: example.line,
                note: format!("`{}` agrees", example.krate),
            })
            .collect(),
    }
}

/// `` `a` `` / `` `a` or `b` `` / `` `a`, `b` or `c` ``, by qualified name.
fn either(apis: &[String]) -> String {
    let names: Vec<String> = apis
        .iter()
        .map(|api| format!("`{}`", api_path(api)))
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}
