//! Library models for taint: which library calls are sources, sinks,
//! sanitizers (barriers and barrier guards), and how data moves through
//! them (summaries, neutrals) — imported from CodeQL's Models-as-Data
//! ([`mad`], MIT-licensed, `github/codeql`) instead of written by hand.
//!
//! A model names a callable (namespace — a Java package, C++ namespace,
//! Rust module path, Python/JS module — plus a type and a method or
//! function name, an optional arity) and positions on its calls
//! ([`Pos`]: the receiver, an argument, the result). The vendored data
//! (`data/*.tsv`, attribution in `data/NOTICE`) is a generated subset of
//! the upstream models; `codegraph models import-codeql <checkout>`
//! regenerates it, and `CODEGRAPH_MODELS_DIR` loads another import at run
//! time instead (e.g. with the generated summaries). `CODEGRAPH_MODELS=0`
//! turns every model off; `CODEGRAPH_MODEL_SUMMARIES=0` keeps the
//! sources/sinks but not the summaries.
//!
//! Rules use models by kind (`sinks: [{model: sql-injection}]`, see
//! [`kinds`]); calls are matched to models through the index's resolution
//! and, where it has none (library code the index holds no node for), the
//! file's imports and declared types ([`matcher`]).

pub mod kinds;
pub mod mad;
pub mod matcher;
mod report;
pub(super) mod site;
mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

pub use report::model_match_report;
pub use store::{parse_models, write_models};

use crate::types::Language;

/// The languages models exist for and taint lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelLanguage {
    Java,
    /// C and C++ (CodeQL's `cpp` models cover both).
    Cpp,
    Python,
    /// JavaScript and TypeScript.
    JavaScript,
    Rust,
}

impl ModelLanguage {
    pub const ALL: [ModelLanguage; 5] = [
        ModelLanguage::Java,
        ModelLanguage::Cpp,
        ModelLanguage::Python,
        ModelLanguage::JavaScript,
        ModelLanguage::Rust,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ModelLanguage::Java => "java",
            ModelLanguage::Cpp => "cpp",
            ModelLanguage::Python => "python",
            ModelLanguage::JavaScript => "javascript",
            ModelLanguage::Rust => "rust",
        }
    }

    /// CodeQL's directory name for the language (`github/codeql/<dir>`).
    pub fn from_codeql_dir(dir: &str) -> Option<Self> {
        Some(match dir {
            "java" => ModelLanguage::Java,
            "cpp" => ModelLanguage::Cpp,
            "python" => ModelLanguage::Python,
            "javascript" => ModelLanguage::JavaScript,
            "rust" => ModelLanguage::Rust,
            _ => return None,
        })
    }

    /// The models a file of `language` uses.
    pub fn of(language: Language) -> Option<Self> {
        Some(match language {
            Language::Java => ModelLanguage::Java,
            Language::C | Language::Cpp => ModelLanguage::Cpp,
            Language::Python => ModelLanguage::Python,
            Language::Javascript | Language::Typescript | Language::Tsx | Language::Jsx => {
                ModelLanguage::JavaScript
            }
            Language::Rust => ModelLanguage::Rust,
            _ => return None,
        })
    }

    /// Separator between namespace, type and name when written out.
    pub fn separator(self) -> &'static str {
        match self {
            ModelLanguage::Cpp | ModelLanguage::Rust => "::",
            _ => ".",
        }
    }
}

/// What a model says of a callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Its `output` is untrusted data of `kind` (a threat model).
    Source,
    /// Its `input` must not receive tainted data of vulnerability `kind`.
    Sink,
    /// Data flows from `input` to `output`.
    Summary,
    /// CodeQL needs no summary for it (a `neutralModel` of kind
    /// `summary`). Kept for listing only: CodeQL's built-in taint steps
    /// still run through such calls (`Integer.parseInt`), so codegraph
    /// keeps its default propagation for them.
    Neutral,
    /// Its `output` is clean for vulnerability `kind`.
    Barrier,
    /// Its result tests `input`: when the result is `accepting`, `input`
    /// is clean for vulnerability `kind`.
    Guard,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Source => "source",
            Role::Sink => "sink",
            Role::Summary => "summary",
            Role::Neutral => "neutral",
            Role::Barrier => "barrier",
            Role::Guard => "guard",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "source" => Role::Source,
            "sink" => Role::Sink,
            "summary" => Role::Summary,
            "neutral" => Role::Neutral,
            "barrier" => Role::Barrier,
            "guard" => Role::Guard,
            _ => return None,
        })
    }
}

/// A position on a call, as codegraph's taint represents it. CodeQL's
/// access paths map onto these ([`mad::access`]); content below a
/// position (an element, a field, a map value) is collapsed onto it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pos {
    /// The call's result.
    Ret,
    /// The receiver (`this`, `self`, a C++ qualifier).
    Recv,
    /// The `n`-th positional argument (0-based, receiver excluded), or
    /// the keyword argument named `keyword` (Python `path=`).
    Arg { n: u8, keyword: Option<String> },
    /// Every argument from the `n`-th on.
    ArgsFrom(u8),
    /// The member read itself (`os.environ`, `sys.argv`): a source that is
    /// no call.
    Read,
}

impl Pos {
    pub fn arg(n: u8) -> Self {
        Pos::Arg { n, keyword: None }
    }

    /// Written form: `ret`, `recv`, `arg0`, `arg0=path`, `arg1+`, `read`.
    pub fn render(&self) -> String {
        match self {
            Pos::Ret => "ret".into(),
            Pos::Recv => "recv".into(),
            Pos::Arg {
                n,
                keyword: Some(keyword),
            } => format!("arg{n}={keyword}"),
            Pos::Arg { n, keyword: None } => format!("arg{n}"),
            Pos::ArgsFrom(n) => format!("arg{n}+"),
            Pos::Read => "read".into(),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ret" => return Some(Pos::Ret),
            "recv" => return Some(Pos::Recv),
            "read" => return Some(Pos::Read),
            _ => {}
        }
        let rest = text.strip_prefix("arg")?;
        if let Some(n) = rest.strip_suffix('+') {
            return n.parse().ok().map(Pos::ArgsFrom);
        }
        let (n, keyword) = match rest.split_once('=') {
            Some((n, keyword)) => (n, Some(keyword.to_string())),
            None => (rest, None),
        };
        Some(Pos::Arg {
            n: n.parse().ok()?,
            keyword,
        })
    }
}

/// One library model.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Model {
    pub role: Role,
    /// Java package, C++ namespace, Rust module path (`std::process`),
    /// Python/JS module (`urllib.parse`, `fs`, `global`); empty for C
    /// functions.
    pub namespace: String,
    /// The type's simple name (`Statement`, `Command`); empty for a free
    /// function or a module member.
    pub type_name: String,
    /// It also holds for the type's subtypes (overrides, implementations).
    pub subtypes: bool,
    /// Method or function name; a constructor is named like its type
    /// (Java, C++, Python classes) or `new` (Rust).
    pub name: String,
    /// Parameters, when the model names a signature.
    pub arity: Option<u8>,
    /// Sinks, summaries, guards: the position read.
    pub input: Option<Pos>,
    /// Sources, summaries, barriers: the position written.
    pub output: Option<Pos>,
    /// Source threat model (`remote`, `environment`…), sink or barrier
    /// vulnerability kind (`sql-injection`…), summary `taint`/`value`.
    pub kind: String,
    /// Guards: the result that makes the input clean.
    pub accepting: Option<bool>,
    /// CodeQL generated it (`*-generated` provenance), not a person.
    pub generated: bool,
    /// Content below a position (an element, a field) was collapsed onto
    /// it: the model is coarser than CodeQL's.
    pub approximate: bool,
}

impl Model {
    /// `java.sql.Statement.executeQuery`, `std::process::Command::new`.
    pub fn callable(&self, language: ModelLanguage) -> String {
        let sep = language.separator();
        let mut out = String::new();
        for part in [&self.namespace, &self.type_name, &self.name] {
            if part.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push_str(sep);
            }
            out.push_str(part);
        }
        out
    }
}

/// One language's models, indexed by callable name.
#[derive(Debug, Default)]
pub struct LanguageModels {
    pub models: Vec<Model>,
    by_name: HashMap<String, Vec<usize>>,
}

impl LanguageModels {
    pub fn new(models: Vec<Model>) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, model) in models.iter().enumerate() {
            by_name.entry(model.name.clone()).or_default().push(index);
        }
        Self { models, by_name }
    }

    /// Every model of a callable named `name`.
    pub fn named(&self, name: &str) -> impl Iterator<Item = &Model> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&index| &self.models[index])
    }

    pub fn has_name(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }
}

/// Every language's models.
#[derive(Debug, Default)]
pub struct ModelDb {
    languages: HashMap<ModelLanguage, LanguageModels>,
    /// Where they were read from (`vendored` or a directory).
    pub origin: String,
}

/// The vendored import (`models import-codeql`), one file per language.
const VENDORED: &[(ModelLanguage, &str)] = &[
    (ModelLanguage::Java, include_str!("data/java.tsv")),
    (ModelLanguage::Cpp, include_str!("data/cpp.tsv")),
    (ModelLanguage::Python, include_str!("data/python.tsv")),
    (
        ModelLanguage::JavaScript,
        include_str!("data/javascript.tsv"),
    ),
    (ModelLanguage::Rust, include_str!("data/rust.tsv")),
];

/// Attribution of the vendored data: CodeQL's license and source commit.
pub const NOTICE: &str = include_str!("data/NOTICE");

impl ModelDb {
    /// The vendored models.
    pub fn vendored() -> Self {
        let mut db = ModelDb {
            origin: "vendored".into(),
            ..Default::default()
        };
        for (language, text) in VENDORED {
            db.languages
                .insert(*language, LanguageModels::new(parse_models(text)));
        }
        db
    }

    /// Models written by [`write_models`] into `dir` (`<language>.tsv`);
    /// a language without a file has none.
    pub fn from_dir(dir: &Path) -> std::io::Result<Self> {
        let mut db = ModelDb {
            origin: dir.display().to_string(),
            ..Default::default()
        };
        for language in ModelLanguage::ALL {
            let path = dir.join(format!("{}.tsv", language.as_str()));
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    db.languages
                        .insert(language, LanguageModels::new(parse_models(&text)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(db)
    }

    /// From explicit models (tests).
    pub fn from_models(models: impl IntoIterator<Item = (ModelLanguage, Vec<Model>)>) -> Self {
        let mut db = ModelDb {
            origin: "explicit".into(),
            ..Default::default()
        };
        for (language, list) in models {
            db.languages.insert(language, LanguageModels::new(list));
        }
        db
    }

    pub fn language(&self, language: ModelLanguage) -> Option<&LanguageModels> {
        self.languages.get(&language)
    }

    /// How many models each language has.
    pub fn counts(&self) -> Vec<(ModelLanguage, usize)> {
        let mut out: Vec<(ModelLanguage, usize)> = self
            .languages
            .iter()
            .map(|(language, models)| (*language, models.models.len()))
            .collect();
        out.sort();
        out
    }
}

/// Whether models are on (`CODEGRAPH_MODELS=0` turns them off).
pub fn enabled() -> bool {
    std::env::var("CODEGRAPH_MODELS").map_or(true, |v| v != "0")
}

/// Whether summaries apply (`CODEGRAPH_MODEL_SUMMARIES=0` keeps only the
/// sources, sinks and sanitizers).
pub fn summaries_enabled() -> bool {
    enabled() && std::env::var("CODEGRAPH_MODEL_SUMMARIES").map_or(true, |v| v != "0")
}

/// The process's models: `CODEGRAPH_MODELS_DIR` when set (an unreadable
/// directory falls back to the vendored ones, with a warning), else the
/// vendored import; `None` when models are off.
pub fn global() -> Option<&'static ModelDb> {
    static DB: OnceLock<ModelDb> = OnceLock::new();
    if !enabled() {
        return None;
    }
    Some(DB.get_or_init(|| {
        if let Some(dir) = std::env::var_os("CODEGRAPH_MODELS_DIR") {
            match ModelDb::from_dir(Path::new(&dir)) {
                Ok(db) => return db,
                Err(e) => eprintln!(
                    "warning: CODEGRAPH_MODELS_DIR {}: {e}; using the vendored models",
                    Path::new(&dir).display()
                ),
            }
        }
        ModelDb::vendored()
    }))
}
