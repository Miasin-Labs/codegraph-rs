//! `codegraph models match`: how many of a project's library calls the
//! models match, and by which evidence ([`super::matcher::Tier`]).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::matcher::{LanguageMatch, MatchReport};
use super::{ModelLanguage, Role, site};
use crate::analyze::bugs::Project;
use crate::analyze::rules::engine::FileInput;
use crate::analyze::rules::semantics::IndexSemantics;
use crate::codegraph::{CodeGraph, OpenOptions};
use crate::extraction::{create_parser, detect_language};

/// Files larger than this are not read (as the rules sweep).
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// Match every call of the project indexed at `root` against the models.
pub fn model_match_report(root: &Path) -> Result<MatchReport, String> {
    let db = super::global().ok_or("models are off (CODEGRAPH_MODELS=0)")?;
    let cg = CodeGraph::open(root, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let result = (|| {
        let project = Project::load(&cg, root)?;
        let semantics = IndexSemantics::new(&cg, &project)?;
        type Counts = HashMap<String, usize>;
        let mut per: BTreeMap<ModelLanguage, (LanguageMatch, Counts, Counts)> = BTreeMap::new();
        for file in project.files() {
            let language = detect_language(file, None);
            let Some(model_language) = ModelLanguage::of(language) else {
                continue;
            };
            let Ok(source) = std::fs::read_to_string(project.root().join(file)) else {
                continue;
            };
            if source.len() > MAX_FILE_BYTES {
                continue;
            }
            let language = detect_language(file, Some(&source));
            let Some(tree) = create_parser(language).and_then(|mut p| p.parse(&source, None))
            else {
                continue;
            };
            let input = FileInput::new(file, language, &source, &tree);
            let Some(found) = site::file_models(&input, &semantics) else {
                continue;
            };
            let (entry, top, unmatched) = per.entry(model_language).or_insert_with(|| {
                (
                    LanguageMatch {
                        language: model_language.as_str().to_string(),
                        ..Default::default()
                    },
                    HashMap::new(),
                    HashMap::new(),
                )
            });
            entry.files += 1;
            entry.calls += found.examined;
            entry.project_calls += found.in_project;
            entry.library_calls += found.examined - found.in_project;
            entry.named_like_a_model += found.named_like_a_model;
            for call in &found.calls {
                *entry
                    .matched
                    .entry(call.tier.as_str().to_string())
                    .or_default() += 1;
                let mut roles: Vec<Role> = call.models.iter().map(|m| m.role).collect();
                roles.sort();
                roles.dedup();
                for role in roles {
                    *entry.by_role.entry(role.as_str().to_string()).or_default() += 1;
                }
                *top.entry(call.callable.clone()).or_default() += 1;
            }
            for (name, n) in &found.unmatched {
                *unmatched.entry(name.clone()).or_default() += n;
            }
        }
        let languages = per
            .into_values()
            .map(|(mut entry, top, unmatched)| {
                entry.top = most_frequent(top);
                entry.unmatched_named = most_frequent(unmatched);
                entry
            })
            .collect();
        Ok(MatchReport {
            root: root.display().to_string(),
            models: db.origin.clone(),
            languages,
        })
    })();
    cg.close();
    result
}

/// The 40 largest counts, largest first.
fn most_frequent(counts: HashMap<String, usize>) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out.truncate(40);
    out
}
