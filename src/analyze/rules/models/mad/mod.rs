//! The CodeQL Models-as-Data importer: every `*.model.yml` of a
//! `github/codeql` checkout (tests left out) → codegraph [`Model`]s, with
//! a count of what was imported, approximated and dropped, and why.
//!
//! Each file holds `extensions:` entries adding rows to an extensible
//! predicate. Imported: `sourceModel`, `sinkModel`, `summaryModel`,
//! `neutralModel` (kind `summary` only: the others say nothing about
//! flow), `barrierModel`, `barrierGuardModel` of the languages codegraph's
//! taint lowers (Java, C/C++, Python, JavaScript/TypeScript, Rust). Row
//! formats per language are in [`callable`]; access paths in [`access`].
//! Generated summaries and neutrals (`*-generated` provenance) are left
//! out unless asked for ([`ImportOptions::generated`]) — they are the bulk
//! of the data (Java 43k, C++ 31k, Rust 15k rows) and codegraph's default
//! propagation already carries what most of them say.

pub mod access;
pub mod callable;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use self::access::Token;
use self::callable::Callable;
use super::{Model, ModelLanguage, Pos, Role};

/// What to import.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Keep generated summaries and neutrals too.
    pub generated: bool,
}

/// An import's models and accounting.
#[derive(Debug, Default)]
pub struct Import {
    pub models: BTreeMap<ModelLanguage, Vec<Model>>,
    /// Files read.
    pub files: usize,
    /// Rows seen, per (CodeQL language dir, extensible predicate).
    pub rows: BTreeMap<(String, String), usize>,
    /// Models made, per (language, role).
    pub imported: BTreeMap<(String, String), usize>,
    /// Rows (or row alternatives) left out, per (language, reason).
    pub dropped: BTreeMap<(String, String), usize>,
    /// Models made coarser than CodeQL's, per (language, what collapsed).
    pub approximated: BTreeMap<(String, String), usize>,
    /// The checkout's commit, when it has a `.git`.
    pub commit: Option<String>,
}

impl Import {
    fn drop(&mut self, language: &str, reason: impl Into<String>) {
        *self
            .dropped
            .entry((language.to_string(), reason.into()))
            .or_default() += 1;
    }

    /// The accounting as text: rows per predicate, models per role, the
    /// drops and approximations by reason.
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "CodeQL models{}: {} files\n",
            self.commit
                .as_ref()
                .map(|c| format!(" @ {c}"))
                .unwrap_or_default(),
            self.files
        ));
        out.push_str("\nRows read (language, predicate):\n");
        for ((language, predicate), n) in &self.rows {
            out.push_str(&format!("  {language:<12} {predicate:<24} {n:>7}\n"));
        }
        out.push_str("\nModels imported (language, role):\n");
        for ((language, role), n) in &self.imported {
            out.push_str(&format!("  {language:<12} {role:<24} {n:>7}\n"));
        }
        out.push_str("\nDropped (language, reason):\n");
        let mut dropped: Vec<_> = self.dropped.iter().collect();
        dropped.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(b.1.cmp(a.1)));
        for ((language, reason), n) in dropped {
            out.push_str(&format!("  {language:<12} {n:>7}  {reason}\n"));
        }
        out.push_str("\nApproximated (language, content collapsed onto its position):\n");
        for ((language, reason), n) in &self.approximated {
            out.push_str(&format!("  {language:<12} {reason:<24} {n:>7}\n"));
        }
        out
    }
}

/// Predicates imported; everything else is counted and dropped.
const IMPORTED: &[&str] = &[
    "sourceModel",
    "sinkModel",
    "summaryModel",
    "neutralModel",
    "barrierModel",
    "barrierGuardModel",
];

/// Every `*.model.yml` under `root`, test directories left out, sorted.
fn model_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if name.starts_with('.') || matches!(name.as_ref(), "test" | "tests") {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() && name.ends_with(".model.yml") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The commit `root`'s `.git` points at (loose or packed ref), if any.
pub fn checkout_commit(root: &Path) -> Option<String> {
    let git = root.join(".git");
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref: ") else {
        return Some(head.to_string());
    };
    if let Ok(commit) = std::fs::read_to_string(git.join(reference)) {
        return Some(commit.trim().to_string());
    }
    let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (commit, name) = line.split_once(' ')?;
        (name == reference).then(|| commit.to_string())
    })
}

/// A YAML scalar as text (`True` → `true`).
fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => serde_yaml_ng::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

/// Import every model file under `root` (a `github/codeql` checkout, or
/// any directory holding `<language>/…/*.model.yml`).
pub fn import_dir(root: &Path, options: &ImportOptions) -> std::io::Result<Import> {
    let mut import = Import {
        commit: checkout_commit(root),
        ..Default::default()
    };
    let mut models: BTreeMap<ModelLanguage, BTreeSet<Model>> = BTreeMap::new();
    for path in model_files(root)? {
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let dir = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path)?;
        import.files += 1;
        let Ok(doc) = serde_yaml_ng::from_str::<Value>(&text) else {
            import.drop(&dir, "file is not YAML");
            continue;
        };
        let extensions = doc
            .get("extensions")
            .and_then(Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        for extension in extensions {
            let predicate = extension
                .get("addsTo")
                .and_then(|a| a.get("extensible"))
                .map(scalar)
                .unwrap_or_default();
            let rows = extension
                .get("data")
                .and_then(Value::as_sequence)
                .cloned()
                .unwrap_or_default();
            *import
                .rows
                .entry((dir.clone(), predicate.clone()))
                .or_default() += rows.len();
            if rows.is_empty() {
                continue;
            }
            let Some(language) = ModelLanguage::from_codeql_dir(&dir) else {
                *import
                    .dropped
                    .entry((
                        dir.clone(),
                        "language codegraph's taint does not lower".into(),
                    ))
                    .or_default() += rows.len();
                continue;
            };
            if !IMPORTED.contains(&predicate.as_str()) {
                *import
                    .dropped
                    .entry((dir.clone(), format!("predicate `{predicate}` not imported")))
                    .or_default() += rows.len();
                continue;
            }
            for row in rows {
                let fields: Vec<String> = row
                    .as_sequence()
                    .map(|cells| cells.iter().map(scalar).collect())
                    .unwrap_or_default();
                match row_models(language, &predicate, &fields, options) {
                    Ok(made) => {
                        for (model, collapsed) in made {
                            for token in collapsed {
                                *import
                                    .approximated
                                    .entry((dir.clone(), token.to_string()))
                                    .or_default() += 1;
                            }
                            models.entry(language).or_default().insert(model);
                        }
                    }
                    Err(reason) => import.drop(&dir, reason),
                }
            }
        }
    }
    for (language, set) in models {
        for model in &set {
            *import
                .imported
                .entry((
                    language.as_str().to_string(),
                    model.role.as_str().to_string(),
                ))
                .or_default() += 1;
        }
        import.models.insert(language, set.into_iter().collect());
    }
    Ok(import)
}

/// One mapped row: the models it makes and, per model, what content it
/// collapsed.
type Made = Vec<(Model, Vec<&'static str>)>;

/// A row's parts, whatever the language's spelling.
struct Row {
    callables: Vec<Callable>,
    input: Option<String>,
    output: Option<String>,
    kind: String,
    accepting: Option<bool>,
    provenance: String,
}

/// The models of one row of `predicate`.
fn row_models(
    language: ModelLanguage,
    predicate: &str,
    fields: &[String],
    options: &ImportOptions,
) -> Result<Made, String> {
    let role = match predicate {
        "sourceModel" => Role::Source,
        "sinkModel" => Role::Sink,
        "summaryModel" => Role::Summary,
        "neutralModel" => Role::Neutral,
        "barrierModel" => Role::Barrier,
        "barrierGuardModel" => Role::Guard,
        _ => return Err(format!("predicate `{predicate}`")),
    };
    let row = match language {
        ModelLanguage::Java | ModelLanguage::Cpp => java_row(role, fields)?,
        ModelLanguage::Rust => rust_row(role, fields)?,
        ModelLanguage::Python | ModelLanguage::JavaScript => api_row(role, fields)?,
    };
    let generated = row.provenance.contains("generated");
    if generated && !options.generated && matches!(role, Role::Summary | Role::Neutral) {
        return Err("generated summary or neutral (import with --generated)".into());
    }
    if role == Role::Neutral && row.kind != "summary" {
        return Err(format!(
            "neutral of kind `{}` (says nothing about flow)",
            row.kind
        ));
    }
    let mut collapsed_all: Vec<&'static str> = Vec::new();
    let mut map = |path: &Option<String>| -> Result<Option<Vec<Pos>>, String> {
        match path {
            None => Ok(None),
            Some(path) if path == "read" => Ok(Some(vec![Pos::Read])),
            Some(path) => {
                let mapped = access::map(path)?;
                for token in mapped.collapsed {
                    if !collapsed_all.contains(&token) {
                        collapsed_all.push(token);
                    }
                }
                Ok(Some(mapped.positions))
            }
        }
    };
    let inputs = map(&row.input)?;
    let outputs = map(&row.output)?;
    let mut kind = row.kind.clone();
    if matches!(role, Role::Sink | Role::Barrier | Role::Guard) {
        // `path-injection[read]`, `regex-use[0]`: the base kind.
        if let Some((base, _)) = kind.split_once('[') {
            kind = base.to_string();
            collapsed_all.push("kind qualifier");
        }
    }
    let approximate = !collapsed_all.is_empty();
    let mut out: Made = Vec::new();
    let input_list: Vec<Option<Pos>> = match inputs {
        Some(list) => list.into_iter().map(Some).collect(),
        None => vec![None],
    };
    let output_list: Vec<Option<Pos>> = match outputs {
        Some(list) => list.into_iter().map(Some).collect(),
        None => vec![None],
    };
    let mut refused: Option<String> = None;
    for callable in &row.callables {
        for input in &input_list {
            for output in &output_list {
                if let Err(reason) = check_positions(role, input, output) {
                    refused.get_or_insert(reason);
                    continue;
                }
                out.push((
                    Model {
                        role,
                        namespace: callable.namespace.clone(),
                        type_name: callable.type_name.clone(),
                        subtypes: callable.subtypes,
                        name: callable.name.clone(),
                        arity: callable.arity,
                        input: input.clone(),
                        output: output.clone(),
                        kind: kind.clone(),
                        accepting: row.accepting,
                        generated,
                        approximate,
                    },
                    collapsed_all.clone(),
                ));
            }
        }
    }
    match (out.is_empty(), refused) {
        (true, Some(reason)) => Err(reason),
        _ => Ok(out),
    }
}

/// Whether the positions make sense for the role.
fn check_positions(role: Role, input: &Option<Pos>, output: &Option<Pos>) -> Result<(), String> {
    match role {
        Role::Sink | Role::Guard => match input {
            Some(Pos::Ret) => Err("a sink or guard on a call's result".into()),
            Some(Pos::Read) | None => Err("a sink or guard with no argument".into()),
            _ => Ok(()),
        },
        Role::Source | Role::Barrier => match output {
            None => Err("a source or barrier with no position".into()),
            _ => Ok(()),
        },
        Role::Summary => match (input, output) {
            (_, Some(Pos::ArgsFrom(_))) => Err("a summary writing every argument".into()),
            (Some(input), Some(output)) if input == output => {
                Err("a step within one position (content only)".into())
            }
            (Some(_), Some(_)) => Ok(()),
            _ => Err("a summary missing its input or output".into()),
        },
        Role::Neutral => Ok(()),
    }
}

fn field(fields: &[String], index: usize) -> Result<&str, String> {
    fields
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("row has {} columns", fields.len()))
}

/// Java/C++: `[package, type, subtypes, name, signature, ext, …]`.
fn java_row(role: Role, f: &[String]) -> Result<Row, String> {
    let callable = |name_at: usize| -> Result<Callable, String> {
        if role == Role::Neutral {
            // [package, type, name, signature, kind, provenance]
            callable::java_like(
                field(f, 0)?,
                field(f, 1)?,
                "false",
                field(f, 2)?,
                field(f, 3)?,
                "",
            )
        } else {
            callable::java_like(
                field(f, 0)?,
                field(f, 1)?,
                field(f, 2)?,
                field(f, name_at)?,
                field(f, 4)?,
                field(f, 5)?,
            )
        }
    };
    let text = |i: usize| field(f, i).map(str::to_string);
    Ok(match role {
        Role::Source | Role::Barrier => Row {
            callables: vec![callable(3)?],
            input: None,
            output: Some(text(6)?),
            kind: text(7)?,
            accepting: None,
            provenance: text(8)?,
        },
        Role::Sink => Row {
            callables: vec![callable(3)?],
            input: Some(text(6)?),
            output: None,
            kind: text(7)?,
            accepting: None,
            provenance: text(8)?,
        },
        Role::Summary => Row {
            callables: vec![callable(3)?],
            input: Some(text(6)?),
            output: Some(text(7)?),
            kind: text(8)?,
            accepting: None,
            provenance: text(9)?,
        },
        Role::Guard => Row {
            callables: vec![callable(3)?],
            input: Some(text(6)?),
            output: None,
            accepting: Some(text(7)? == "true"),
            kind: text(8)?,
            provenance: text(9)?,
        },
        Role::Neutral => Row {
            callables: vec![callable(2)?],
            input: None,
            output: None,
            kind: text(4)?,
            accepting: None,
            provenance: text(5)?,
        },
    })
}

/// Rust: `[path, …]`.
fn rust_row(role: Role, f: &[String]) -> Result<Row, String> {
    let callables = vec![callable::rust(field(f, 0)?)?];
    let text = |i: usize| field(f, i).map(str::to_string);
    Ok(match role {
        Role::Source | Role::Barrier => Row {
            callables,
            input: None,
            output: Some(text(1)?),
            kind: text(2)?,
            accepting: None,
            provenance: text(3)?,
        },
        Role::Sink => Row {
            callables,
            input: Some(text(1)?),
            output: None,
            kind: text(2)?,
            accepting: None,
            provenance: text(3)?,
        },
        Role::Summary => Row {
            callables,
            input: Some(text(1)?),
            output: Some(text(2)?),
            kind: text(3)?,
            accepting: None,
            provenance: text(4)?,
        },
        Role::Guard => Row {
            callables,
            input: Some(text(1)?),
            output: None,
            accepting: Some(text(2)? == "true"),
            kind: text(3)?,
            provenance: text(4)?,
        },
        Role::Neutral => Row {
            callables,
            input: None,
            output: None,
            kind: text(1)?,
            accepting: None,
            provenance: text(2)?,
        },
    })
}

/// Where an API-graph path stops naming the callable: the first
/// `Argument`/`ReturnValue`/`Parameter`.
fn split_terminal<'a>(tokens: &'a [Token<'a>]) -> (&'a [Token<'a>], &'a [Token<'a>]) {
    let at = tokens
        .iter()
        .position(|t| matches!(t.name, "Argument" | "ReturnValue" | "Parameter"))
        .unwrap_or(tokens.len());
    (&tokens[..at], &tokens[at..])
}

/// A terminal as an access path string (`read` when there is none: the
/// member read itself).
fn terminal_path(terminal: &[Token]) -> Result<String, String> {
    if terminal.is_empty() {
        return Ok("read".into());
    }
    // `ReturnValue.Member[then].Argument[0].Parameter[0]`: reached through
    // what a call returns, not a position of this call.
    let later_call = terminal[1..]
        .iter()
        .position(|t| matches!(t.name, "Argument" | "ReturnValue" | "Parameter"));
    let member_before = |end: usize| {
        terminal[1..=end]
            .iter()
            .any(|t| matches!(t.name, "Member" | "AnyMember"))
    };
    if let Some(i) = later_call {
        if member_before(i) {
            return Err("reached through another call's result (API-graph chain)".into());
        }
    }
    Ok(terminal
        .iter()
        .map(|t| match t.arg {
            Some(arg) => format!("{}[{arg}]", t.name),
            None => t.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join("."))
}

/// Python/JS: `[type, path, …]`.
fn api_row(role: Role, f: &[String]) -> Result<Row, String> {
    let root = field(f, 0)?;
    let path = field(f, 1)?;
    let text = |i: usize| field(f, i).map(str::to_string);
    let tokens = access::tokens(path);
    let (callable_tokens, terminal) = match role {
        Role::Summary | Role::Neutral => (&tokens[..], &[][..]),
        _ => split_terminal(&tokens),
    };
    let callables = callable::api_graph(root, callable_tokens)?;
    Ok(match role {
        Role::Source | Role::Barrier => Row {
            callables,
            input: None,
            output: Some(terminal_path(terminal)?),
            kind: text(2)?,
            accepting: None,
            provenance: "manual".into(),
        },
        Role::Sink => {
            if terminal.is_empty() {
                return Err("a sink with no argument".into());
            }
            Row {
                callables,
                input: Some(terminal_path(terminal)?),
                output: None,
                kind: text(2)?,
                accepting: None,
                provenance: "manual".into(),
            }
        }
        Role::Summary => Row {
            callables,
            input: Some(text(2)?),
            output: Some(text(3)?),
            kind: text(4)?,
            accepting: None,
            provenance: "manual".into(),
        },
        Role::Guard => Row {
            callables,
            input: Some(terminal_path(terminal)?),
            output: None,
            accepting: Some(text(2)? == "true"),
            kind: text(3)?,
            provenance: "manual".into(),
        },
        Role::Neutral => Row {
            callables,
            input: None,
            output: None,
            kind: text(2)?,
            accepting: None,
            provenance: "manual".into(),
        },
    })
}

/// The files an import writes into `dir`: `<language>.tsv` per language
/// and `NOTICE` (CodeQL's license, from the checkout's `LICENSE`, and the
/// commit). Returns the paths written.
pub fn write_import(import: &Import, root: &Path, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let commit = import.commit.as_deref().unwrap_or("unknown");
    let mut written = Vec::new();
    for language in ModelLanguage::ALL {
        let models = import
            .models
            .get(&language)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let preamble = format!(
            "CodeQL Models-as-Data for {}, imported by `codegraph models import-codeql`.\n\
             Source: https://github.com/github/codeql @ {commit}\n\
             Copyright (c) GitHub, Inc. — MIT License, see NOTICE.",
            language.as_str()
        );
        let path = dir.join(format!("{}.tsv", language.as_str()));
        std::fs::write(&path, super::write_models(&preamble, models))?;
        written.push(path);
    }
    let license = std::fs::read_to_string(root.join("LICENSE")).unwrap_or_default();
    let notice = format!(
        "The *.tsv files in this directory are derived from the Models-as-Data\n\
         files (*.model.yml) of GitHub CodeQL, https://github.com/github/codeql,\n\
         at commit {commit}, converted by `codegraph models import-codeql`.\n\
         They are distributed under CodeQL's license:\n\n{license}"
    );
    let path = dir.join("NOTICE");
    std::fs::write(&path, notice)?;
    written.push(path);
    Ok(written)
}
