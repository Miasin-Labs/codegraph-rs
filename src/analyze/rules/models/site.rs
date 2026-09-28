//! Models in the rules engine: what a `model:` pattern matches in a file,
//! and the library summaries the taint program applies to calls.
//!
//! Each file's calls are matched once ([`FileModels`], cached on the
//! file): every call node against the models of its language, with the
//! index's word on the call ([`Semantics::call_at`]). Patterns then only
//! filter by role and kind.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use codegraph_analysis::propagation_rules::PropagationRules;
use codegraph_analysis::taint_flow::{Access, Output, Path, Slot, Summary};
use tree_sitter::Node;

use super::matcher::shape::{self, CallShape};
use super::matcher::{CallFacts, FileContext, Tier, position_nodes};
use super::{Model, ModelLanguage, Pos, Role, kinds};
use crate::analyze::rules::compile::Backend;
use crate::analyze::rules::engine::{FileInput, Hit};
use crate::analyze::rules::lang;
use crate::analyze::rules::semantics::Semantics;
use crate::types::Language;

/// The capture a model pattern marks (the source's value, the sink's
/// argument, the sanitized value, the guarded value).
pub const MODEL_CAPTURE: &str = "model";
/// The capture holding the modeled call (a guard's check).
pub const CALL_CAPTURE: &str = "call";

/// A compiled `model:` pattern.
#[derive(Debug, Clone)]
pub struct ModelPattern {
    pub role: Role,
    /// The kinds its selectors stand for ([`kinds::expand`]).
    pub kinds: Vec<String>,
    /// Guards: the check result that makes the value safe (the rule's
    /// `safe`), so only models accepting that result apply.
    pub accepting: bool,
}

/// Compile a `model:` pattern for `languages`: one backend per language
/// models exist for, and its captures.
pub fn compile(
    selectors: &[String],
    role: Role,
    languages: &[Language],
) -> Result<(Vec<(Language, Backend)>, BTreeSet<String>), String> {
    if selectors.is_empty() {
        return Err("`model` names no kind".into());
    }
    let kind_role = if role == Role::Source {
        Role::Source
    } else {
        Role::Sink
    };
    let mut kinds_list: Vec<String> = Vec::new();
    for selector in selectors {
        for kind in kinds::expand(kind_role, selector) {
            if !kinds_list.contains(&kind) {
                kinds_list.push(kind);
            }
        }
    }
    let with_models: Vec<Language> = languages
        .iter()
        .copied()
        .filter(|l| ModelLanguage::of(*l).is_some())
        .collect();
    if with_models.is_empty() {
        return Err(format!(
            "no library models for {}; models exist for java, c, cpp, python, javascript, \
             typescript and rust",
            languages
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // A kind no model of these languages has is a typo, or a kind of
    // another role (a threat model as a sink…).
    if let Some(db) = super::global() {
        let model_role = |m: &Model| match role {
            Role::Sink => m.role == Role::Sink,
            Role::Source => m.role == Role::Source,
            Role::Barrier => matches!(m.role, Role::Barrier | Role::Sink),
            Role::Guard => matches!(m.role, Role::Guard | Role::Sink),
            _ => false,
        };
        let known = with_models.iter().any(|l| {
            ModelLanguage::of(*l)
                .and_then(|ml| db.language(ml))
                .is_some_and(|models| {
                    models
                        .models
                        .iter()
                        .any(|m| model_role(m) && kinds::matches(&kinds_list, &m.kind))
                })
        });
        if !known {
            return Err(format!(
                "no {} model of kind {} for {} (aliases: {}; `codegraph models stats` lists \
                 the kinds)",
                role.as_str(),
                selectors.join(", "),
                with_models
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                kinds::known_selectors(kind_role).join(", ")
            ));
        }
    }
    let pattern = ModelPattern {
        role,
        kinds: kinds_list,
        accepting: true,
    };
    let backends = with_models
        .into_iter()
        .map(|l| (l, Backend::Model(pattern.clone())))
        .collect();
    let captures = [MODEL_CAPTURE, CALL_CAPTURE]
        .into_iter()
        .map(str::to_string)
        .collect();
    Ok((backends, captures))
}

/// One matched call of a file.
#[derive(Debug, Clone)]
pub struct ModelCall {
    pub call: Range<usize>,
    pub kind: &'static str,
    pub models: Vec<&'static Model>,
    pub tier: Tier,
    pub callable: String,
}

/// One matched member read (`os.environ`).
#[derive(Debug, Clone)]
pub struct ModelRead {
    pub node: Range<usize>,
    pub models: Vec<&'static Model>,
}

/// A file's calls matched to models, once.
#[derive(Debug, Default)]
pub struct FileModels {
    pub calls: Vec<ModelCall>,
    pub reads: Vec<ModelRead>,
    /// Call start byte → indices into `calls`.
    by_start: HashMap<usize, Vec<usize>>,
    /// Calls examined, and the ones the index put in the project.
    pub examined: usize,
    pub in_project: usize,
    /// Library calls whose callee name some model has.
    pub named_like_a_model: usize,
    /// Those that matched nothing, by name.
    pub unmatched: HashMap<String, usize>,
}

/// Member-read node kinds (sources that are no call).
const READ_KINDS: &[&str] = &["attribute", "member_expression"];

/// What the index (or an example's `resolves` map) says of a call node.
pub(in crate::analyze::rules) fn call_facts(
    file: &FileInput,
    semantics: &dyn Semantics,
    call: Node,
) -> CallFacts {
    let rules = lang::for_language(file.language);
    let (text, name) = lang::callee(rules, call, file.source);
    let start = call.start_position();
    let resolution = semantics.call_at(file, start.row as u32 + 1, start.column as u32, &text);
    CallFacts {
        names: resolution.names,
        in_project: resolution.in_project,
        defined_here: !name.is_empty() && file.defines(rules, &name),
    }
}

/// The file's matched calls (built on first use; `None` without models
/// for its language).
pub(in crate::analyze::rules) fn file_models<'f>(
    file: &'f FileInput,
    semantics: &dyn Semantics,
) -> Option<&'f FileModels> {
    let db = super::global()?;
    let model_language = ModelLanguage::of(file.language)?;
    let models = db.language(model_language)?;
    Some(file.model_calls.get_or_init(|| {
        let context = FileContext {
            models,
            model_language,
            language: file.language,
            source: file.source,
            facts: file.model_facts(),
        };
        let has_reads = models.models.iter().any(|m| m.output == Some(Pos::Read));
        let mut out = FileModels::default();
        let mut stack = vec![file.tree.root_node()];
        while let Some(node) = stack.pop() {
            let kind = node.kind();
            if shape::is_call(kind) {
                out.examined += 1;
                let facts = call_facts(file, semantics, node);
                if facts.in_project {
                    out.in_project += 1;
                } else if let Some(matched) = context.match_call(node, &facts) {
                    out.named_like_a_model += 1;
                    out.by_start
                        .entry(node.start_byte())
                        .or_default()
                        .push(out.calls.len());
                    out.calls.push(ModelCall {
                        call: node.byte_range(),
                        kind,
                        models: matched.models,
                        tier: matched.tier,
                        callable: matched.callable,
                    });
                } else if let Some(s) = shape::shape(node, file.source) {
                    if models.has_name(&s.name) {
                        out.named_like_a_model += 1;
                        *out.unmatched.entry(s.name).or_default() += 1;
                    }
                }
            } else if has_reads && READ_KINDS.contains(&kind) {
                let found = context.match_read(node);
                if !found.is_empty() {
                    out.reads.push(ModelRead {
                        node: node.byte_range(),
                        models: found,
                    });
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        out
    }))
}

/// The call node `call` spans, with its kind.
fn call_node<'t>(file: &'t FileInput, call: &ModelCall) -> Option<Node<'t>> {
    let mut node = file
        .tree
        .root_node()
        .descendant_for_byte_range(call.call.start, call.call.end)?;
    while node.byte_range() != call.call || node.kind() != call.kind {
        node = node.parent()?;
    }
    Some(node)
}

/// Whether a model plays `role` for a pattern of that role.
fn plays(model: &Model, pattern: &ModelPattern) -> bool {
    let role_fits = match pattern.role {
        Role::Guard => model.role == Role::Guard && model.accepting == Some(pattern.accepting),
        role => model.role == role,
    };
    role_fits && kinds::matches(&pattern.kinds, &model.kind)
}

/// A `model:` pattern's raw matches in `file`.
pub(in crate::analyze::rules) fn hits(
    pattern: &ModelPattern,
    index: usize,
    file: &FileInput,
    semantics: &dyn Semantics,
) -> Vec<Hit> {
    let Some(found) = file_models(file, semantics) else {
        return Vec::new();
    };
    let model_language = ModelLanguage::of(file.language);
    let mut out = Vec::new();
    for call in &found.calls {
        let models: Vec<&Model> = call
            .models
            .iter()
            .copied()
            .filter(|m| plays(m, pattern))
            .collect();
        if models.is_empty() {
            continue;
        }
        let Some(node) = call_node(file, call) else {
            continue;
        };
        let Some(shape) = shape::shape(node, file.source) else {
            continue;
        };
        let mut marked: Vec<Range<usize>> = Vec::new();
        for model in &models {
            let position = match pattern.role {
                Role::Sink | Role::Guard => model.input.as_ref(),
                _ => model.output.as_ref(),
            };
            let Some(position) = position else { continue };
            for target in positions(&shape, model, position) {
                if marked.contains(&target) {
                    continue;
                }
                marked.push(target.clone());
                let model = *model;
                out.push(Hit {
                    pattern: index,
                    captures: vec![
                        (MODEL_CAPTURE.to_string(), target.clone()),
                        (CALL_CAPTURE.to_string(), call.call.clone()),
                    ],
                    span: call.call.clone(),
                    at: target,
                    notes: vec![format!(
                        "CodeQL {} model `{}`{} ({}), matched by {}",
                        model.role.as_str(),
                        model_language.map_or(call.callable.clone(), |l| model.callable(l)),
                        model
                            .input
                            .as_ref()
                            .or(model.output.as_ref())
                            .map(|p| format!(" {}", p.render()))
                            .unwrap_or_default(),
                        model.kind,
                        call.tier.as_str()
                    )],
                    path: Vec::new(),
                    confidence: None,
                    flow: None,
                });
            }
        }
    }
    if pattern.role == Role::Source {
        for read in &found.reads {
            if !read.models.iter().any(|m| plays(m, pattern)) {
                continue;
            }
            out.push(Hit {
                pattern: index,
                captures: vec![
                    (MODEL_CAPTURE.to_string(), read.node.clone()),
                    (CALL_CAPTURE.to_string(), read.node.clone()),
                ],
                span: read.node.clone(),
                at: read.node.clone(),
                notes: vec!["CodeQL source model (a member read)".into()],
                path: Vec::new(),
                confidence: None,
                flow: None,
            });
        }
    }
    out
}

/// The byte ranges a model's position names on a call. A Rust path call
/// of a method (`Type::m(x, a)`) passes the receiver first.
fn positions(shape: &CallShape, model: &Model, position: &Pos) -> Vec<Range<usize>> {
    let ufcs = shape.receiver.is_none()
        && !shape.constructor
        && shape.path.as_deref().is_some_and(|p| p.contains("::"))
        && [&model.input, &model.output]
            .iter()
            .any(|p| **p == Some(Pos::Recv));
    let position = match (ufcs, position) {
        // A constructor's `this` is the object it makes: its result.
        (_, Pos::Recv) if shape.constructor => Pos::Ret,
        (true, Pos::Recv) => Pos::arg(0),
        (true, Pos::Arg { n, keyword }) => Pos::Arg {
            n: n + 1,
            keyword: keyword.clone(),
        },
        (true, Pos::ArgsFrom(n)) => Pos::ArgsFrom(n + 1),
        (_, other) => other.clone(),
    };
    position_nodes(shape, &position)
        .into_iter()
        .map(|node| node.byte_range())
        .collect()
}

/// The library summary for the call at 1-based `line`, 0-based `col`
/// written `callee` with `args` arguments: what its summary models say
/// moves through it. `None` when no summary model matches it exactly (by
/// resolution, type or import), or
/// when the language's propagation table already models its name (the
/// table is more precise in codegraph's terms: keyed maps, lists,
/// builders).
pub(in crate::analyze::rules) fn summary_at(
    file: &FileInput,
    semantics: &dyn Semantics,
    ir_language: &str,
    line: u32,
    col: u32,
    callee: &str,
    args: usize,
) -> Option<Arc<Summary>> {
    if !super::summaries_enabled() {
        return None;
    }
    let found = file_models(file, semantics)?;
    let name = lang::last_name(callee);
    let table = PropagationRules::for_language(ir_language);
    if table.models(name) {
        return None;
    }
    let point = tree_sitter::Point {
        row: line.saturating_sub(1) as usize,
        column: col as usize,
    };
    let mut node = file
        .tree
        .root_node()
        .descendant_for_point_range(point, point)?;
    // The innermost matched call starting here named like the op.
    let call = loop {
        if node.start_position() != point {
            node = node.parent()?;
            continue;
        }
        let here = found.by_start.get(&node.start_byte()).and_then(|calls| {
            calls.iter().map(|&i| &found.calls[i]).find(|c| {
                c.call == node.byte_range()
                    && shape::shape(node, file.source).is_some_and(|s| s.name == name)
            })
        });
        if let Some(call) = here {
            break call;
        }
        node = node.parent()?;
    };
    if !call.tier.exact() {
        return None;
    }
    let shape = shape::shape(node, file.source)?;
    // Neutrals are left to the default: CodeQL's own taint steps still
    // run through them (`Integer.parseInt` is neutral, yet its result is
    // the untrusted number an arithmetic rule tracks).
    let models: Vec<&Model> = call
        .models
        .iter()
        .copied()
        .filter(|m| m.role == Role::Summary)
        .collect();
    if models.is_empty() {
        return None;
    }
    let path: Path = Arc::from(Vec::new());
    let ufcs = shape.receiver.is_none() && callee.contains("::");
    let access = |pos: &Pos, model: &Model| -> Vec<Access> {
        let shift = usize::from(
            ufcs && [&model.input, &model.output]
                .iter()
                .any(|p| **p == Some(Pos::Recv)),
        );
        let slot = |n: usize| {
            if shift == 1 && n == 0 {
                Slot::Receiver
            } else {
                Slot::Param(n)
            }
        };
        match pos {
            Pos::Recv if shape.constructor => Vec::new(),
            Pos::Recv if shift == 1 => vec![Access::slot(Slot::Param(0))],
            Pos::Recv => vec![Access::slot(Slot::Receiver)],
            Pos::Arg { n, .. } => vec![Access::slot(slot(*n as usize + shift))],
            Pos::ArgsFrom(n) => (*n as usize + shift..args.max(*n as usize + shift + 1))
                .map(|i| Access::slot(Slot::Param(i)))
                .collect(),
            Pos::Ret | Pos::Read => Vec::new(),
        }
    };
    let mut summary = Summary::default();
    for model in models.iter().filter(|m| m.role == Role::Summary) {
        let (Some(input), Some(output)) = (&model.input, &model.output) else {
            continue;
        };
        let inputs = access(input, model);
        let add = |out: &mut Output| {
            for input in &inputs {
                if !out.inputs.iter().any(|(a, _)| a == input) {
                    out.inputs.push((input.clone(), path.clone()));
                }
            }
        };
        // A constructor's `this` is the object it makes: its result.
        if *output == Pos::Ret || (shape.constructor && *output == Pos::Recv) {
            add(&mut summary.returns);
            continue;
        }
        for target in access(output, model) {
            match summary.writes.iter_mut().find(|(t, _)| *t == target) {
                Some((_, out)) => add(out),
                None => {
                    let mut out = Output::default();
                    add(&mut out);
                    summary.writes.push((target, out));
                }
            }
        }
    }
    Some(Arc::new(summary))
}

/// The models `call` (a call node of `file`) matched, described for a
/// person or model writing a rule: `sink sql-injection arg0
/// (java.sql.Statement.executeQuery, by typed)`.
pub(in crate::analyze::rules) fn describe(
    file: &FileInput,
    semantics: &dyn Semantics,
    call: Node,
) -> Vec<String> {
    let Some(found) = file_models(file, semantics) else {
        return Vec::new();
    };
    let Some(language) = ModelLanguage::of(file.language) else {
        return Vec::new();
    };
    let Some(matched) = found
        .by_start
        .get(&call.start_byte())
        .into_iter()
        .flatten()
        .map(|&i| &found.calls[i])
        .find(|c| c.call == call.byte_range())
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = matched
        .models
        .iter()
        .filter(|m| m.role != Role::Neutral)
        .map(|m| {
            let position = match m.role {
                Role::Summary => format!(
                    "{} -> {}",
                    m.input.as_ref().map_or("-".into(), Pos::render),
                    m.output.as_ref().map_or("-".into(), Pos::render)
                ),
                Role::Sink | Role::Guard => m.input.as_ref().map_or("-".into(), Pos::render),
                _ => m.output.as_ref().map_or("-".into(), Pos::render),
            };
            format!(
                "{} {} {position} ({}, by {})",
                m.role.as_str(),
                m.kind,
                m.callable(language),
                matched.tier.as_str()
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}
