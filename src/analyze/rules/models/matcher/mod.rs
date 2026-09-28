//! Calls matched to models.
//!
//! A call matches the models of the callable it runs, found — best first —
//! by:
//!
//! 1. **resolution**: a qualified name the index resolved the call to
//!    (dependency and std graphs through `external_edges`; Rust mostly —
//!    the other languages' library calls have no node in any graph);
//! 2. **type**: the receiver's declared type (a Java local, parameter or
//!    field; a Rust `let x: T` / `let x = T::new(…)` / parameter; a Python
//!    or JS variable assigned a constructed object) or the type a
//!    constructor builds, qualified through the file's imports — with,
//!    for Java, a model of a supertype (`subtypes`) matching a type whose
//!    simple name ends with it in a related package
//!    (`HttpServletRequest` → `ServletRequest`, `PreparedStatement` →
//!    `Statement`);
//! 3. **import**: a static or module path (`DriverManager.getConnection`,
//!    `os.system`, `std::env::var`, a C function) qualified through the
//!    imports;
//! 4. **name**: the callee's name alone, only when the receiver's type is
//!    unknown and every model of that name belongs to one type.
//!
//! A call the index resolves to project code runs that code: no model
//! applies. Summaries (which rewrite how data moves) apply only from the
//! first three; sources, sinks and sanitizers from all four.

pub mod facts;
pub mod shape;

use std::collections::HashSet;

use serde::Serialize;
use tree_sitter::Node;

use self::facts::{FileFacts, bare_type};
use self::shape::{CallShape, last_segment, plain_path};
use super::{LanguageModels, Model, ModelLanguage, Pos};
use crate::types::Language;

/// How a call was matched, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    Resolved,
    Typed,
    Imported,
    Name,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Resolved => "resolved",
            Tier::Typed => "typed",
            Tier::Imported => "imported",
            Tier::Name => "name",
        }
    }

    /// Precise enough to rewrite how data moves through the call.
    pub fn exact(self) -> bool {
        self != Tier::Name
    }
}

/// What the index says of a call.
#[derive(Debug, Clone, Default)]
pub struct CallFacts {
    /// Qualified names of what it resolves to.
    pub names: Vec<String>,
    /// It runs project code.
    pub in_project: bool,
    /// The file defines a function of the callee's name.
    pub defined_here: bool,
}

/// A call's models.
#[derive(Debug, Clone)]
pub struct Matched<'a, 't> {
    pub shape: CallShape<'t>,
    pub models: Vec<&'a Model>,
    pub tier: Tier,
    /// The callable matched, written out (`java.sql.Statement.execute`).
    pub callable: String,
}

/// Everything matching needs about one file.
pub struct FileContext<'a, 'f> {
    pub models: &'a LanguageModels,
    pub model_language: ModelLanguage,
    pub language: Language,
    pub source: &'f str,
    pub facts: &'f FileFacts,
}

/// What a candidate callable is.
#[derive(Debug, Clone)]
enum Key {
    /// A fully qualified callable (`java.sql.DriverManager.getConnection`,
    /// `os.system`, `std::env::var`; a constructor: its type's path).
    Path(String),
    /// The receiver's type: its possible namespaces and simple name.
    Typed {
        namespaces: Vec<String>,
        type_name: String,
    },
    /// The name alone.
    Name,
}

/// JavaScript globals a path may start with.
const JS_GLOBALS: &[&str] = &[
    "process",
    "console",
    "window",
    "document",
    "globalThis",
    "JSON",
    "Buffer",
    "location",
];

/// Rust crates the toolchain's library is spread over.
const RUST_STD: &[&str] = &["std", "core", "alloc"];

impl<'a, 'f> FileContext<'a, 'f> {
    /// The models `call` matches, how, and its parts; `None` when it is no
    /// call, runs project code, or matches nothing.
    pub fn match_call<'t>(&self, call: Node<'t>, facts: &CallFacts) -> Option<Matched<'a, 't>> {
        let shape = shape::shape(call, self.source)?;
        if facts.in_project {
            return None;
        }
        // Constructors are looked up by their type's name, and a JS module
        // called as a function by its module: neither by the local name.
        if !shape.constructor
            && self.model_language != ModelLanguage::JavaScript
            && !self.models.has_name(&shape.name)
        {
            return None;
        }
        let mut keys: Vec<(Key, Tier)> = Vec::new();
        for name in &facts.names {
            keys.push((Key::Path(name.clone()), Tier::Resolved));
        }
        self.keys(&shape, facts, &mut keys);
        for (key, tier) in keys {
            let found = self.lookup(&shape, &key);
            if !found.is_empty() {
                let callable = match &key {
                    Key::Path(path) => path.clone(),
                    Key::Typed { type_name, .. } => format!(
                        "{type_name}{}{}",
                        self.model_language.separator(),
                        shape.name
                    ),
                    Key::Name => shape.name.clone(),
                };
                return Some(Matched {
                    shape,
                    models: found,
                    tier,
                    callable,
                });
            }
        }
        None
    }

    /// Source models of a member read (`os.environ`, `sys.argv`): the
    /// models whose output is [`Pos::Read`] for the path `node` reads.
    pub fn match_read(&self, node: Node) -> Vec<&'a Model> {
        let Some(path) = plain_path(node, self.source) else {
            return Vec::new();
        };
        let name = last_segment(&path);
        if !self.models.has_name(name) {
            return Vec::new();
        }
        let Some(qualified) = self.qualify_module_path(&path, node) else {
            return Vec::new();
        };
        self.models
            .named(name)
            .filter(|m| m.output == Some(Pos::Read) && m.callable(self.model_language) == qualified)
            .collect()
    }

    /// Candidate keys for the call, best first.
    fn keys(&self, shape: &CallShape, facts: &CallFacts, keys: &mut Vec<(Key, Tier)>) {
        match self.model_language {
            ModelLanguage::Java => self.java_keys(shape, keys),
            ModelLanguage::Python | ModelLanguage::JavaScript => {
                self.module_keys(shape, facts, keys)
            }
            ModelLanguage::Rust => self.rust_keys(shape, facts, keys),
            ModelLanguage::Cpp => self.c_keys(shape, facts, keys),
        }
        if shape.constructor || facts.defined_here {
            return;
        }
        // The name alone, when nothing typed the receiver.
        let typed = keys.iter().any(|(key, _)| matches!(key, Key::Typed { .. }));
        if !typed && self.unambiguous(&shape.name) {
            keys.push((Key::Name, Tier::Name));
        }
    }

    /// Every model named `name` belongs to one type (by simple name: the
    /// same type in several crates or packages — `std` and `tokio`'s
    /// `Command` — counts as one), and a method of one: a free function's
    /// name alone says nothing of which module it is from.
    fn unambiguous(&self, name: &str) -> bool {
        let mut owners: HashSet<&str> = HashSet::new();
        for model in self.models.named(name) {
            if model.type_name.is_empty() {
                return false;
            }
            owners.insert(&model.type_name);
            if owners.len() > 1 {
                return false;
            }
        }
        owners.len() == 1
    }

    /// Java: constructors, typed receivers, static and imported calls.
    fn java_keys(&self, shape: &CallShape, keys: &mut Vec<(Key, Tier)>) {
        if shape.constructor {
            let Some(path) = &shape.path else { return };
            for (namespace, simple) in self.java_types(path) {
                keys.push((Key::Path(format!("{namespace}.{simple}")), Tier::Typed));
            }
            return;
        }
        let Some(receiver) = shape.receiver else {
            // `m(…)`: a static import, else the class's own method.
            if let Some(owner) = self.facts.static_imports.get(&shape.name) {
                keys.push((Key::Path(format!("{owner}.{}", shape.name)), Tier::Imported));
            }
            return;
        };
        let Some(receiver_path) = &shape.receiver_path else {
            return;
        };
        // `this.x` / `x`: a declared field or variable.
        let variable = receiver_path.strip_prefix("this.").unwrap_or(receiver_path);
        if !variable.contains('.') {
            if let Some(declared) = self.facts.declared_type(receiver, variable) {
                let types = self.java_types(declared);
                if let Some((_, simple)) = types.first() {
                    keys.push((
                        Key::Typed {
                            namespaces: types.iter().map(|(n, _)| n.clone()).collect(),
                            type_name: simple.clone(),
                        },
                        Tier::Typed,
                    ));
                }
                return;
            }
        }
        // `Type.m(…)`, `pkg.Type.m(…)`: a static call.
        let simple = last_segment(receiver_path);
        if simple.chars().next().is_some_and(char::is_uppercase) {
            for (namespace, simple) in self.java_types(receiver_path) {
                keys.push((
                    Key::Path(format!("{namespace}.{simple}.{}", shape.name)),
                    Tier::Imported,
                ));
            }
        }
    }

    /// The (package, simple name) a Java type as written may be: its
    /// qualified form, its import, else a wildcard import's, the file's
    /// package's or `java.lang`'s.
    fn java_types(&self, written: &str) -> Vec<(String, String)> {
        let bare = bare_type(written);
        if bare.is_empty() {
            return Vec::new();
        }
        if let Some((package, simple)) = bare.rsplit_once('.') {
            // `Map.Entry` (an imported outer type) or a qualified name.
            if package.chars().next().is_some_and(char::is_lowercase) {
                return vec![(package.to_string(), simple.to_string())];
            }
        }
        let simple = last_segment(&bare).to_string();
        let head = bare.split('.').next().unwrap_or(&bare);
        if let Some(import) = self.facts.imports.get(head) {
            let package = import.rsplit_once('.').map_or("", |(p, _)| p);
            return vec![(package.to_string(), simple)];
        }
        let mut out: Vec<(String, String)> = self
            .facts
            .wildcards
            .iter()
            .map(|w| (w.clone(), simple.clone()))
            .collect();
        if let Some(package) = &self.facts.package {
            out.push((package.clone(), simple.clone()));
        }
        out.push(("java.lang".into(), simple));
        out
    }

    /// A dotted path qualified through the module imports (Python, JS):
    /// `np.x` → `numpy.x`; a bare builtin → `builtins.x`; a JS global →
    /// `global.x`.
    fn qualify_module_path(&self, path: &str, at: Node) -> Option<String> {
        let (head, rest) = match path.split_once('.') {
            Some((head, rest)) => (head, Some(rest)),
            None => (path, None),
        };
        let join = |base: &str| match rest {
            Some(rest) => format!("{base}.{rest}"),
            None => base.to_string(),
        };
        if let Some(import) = self.facts.imports.get(head) {
            return Some(join(import));
        }
        if self.facts.declared_type(at, head).is_some() {
            return None;
        }
        match self.model_language {
            ModelLanguage::JavaScript if JS_GLOBALS.contains(&head) => {
                Some(format!("global.{path}"))
            }
            ModelLanguage::Python if rest.is_none() => Some(format!("builtins.{path}")),
            _ => None,
        }
    }

    /// Python/JS: module paths through the imports, and receivers
    /// assigned a constructed object.
    fn module_keys(&self, shape: &CallShape, facts: &CallFacts, keys: &mut Vec<(Key, Tier)>) {
        if let (Some(receiver), Some(receiver_path)) = (shape.receiver, &shape.receiver_path) {
            if !receiver_path.contains('.') {
                if let Some(declared) = self.facts.declared_type(receiver, receiver_path) {
                    let declared = declared.trim_start_matches("new ").to_string();
                    if let Some(class) = self.qualify_module_path(&declared, receiver) {
                        let (namespace, type_name) = class
                            .rsplit_once('.')
                            .map_or((String::new(), class.clone()), |(n, t)| {
                                (n.to_string(), t.to_string())
                            });
                        keys.push((
                            Key::Typed {
                                namespaces: vec![namespace],
                                type_name,
                            },
                            Tier::Typed,
                        ));
                    }
                    return;
                }
            }
        }
        let Some(path) = &shape.path else { return };
        if shape.receiver.is_none() && facts.defined_here {
            return;
        }
        if let Some(qualified) = self.qualify_module_path(path, shape.call) {
            keys.push((Key::Path(qualified), Tier::Imported));
        }
    }

    /// Rust: `use`-qualified paths and typed receivers.
    fn rust_keys(&self, shape: &CallShape, facts: &CallFacts, keys: &mut Vec<(Key, Tier)>) {
        if let (Some(receiver), Some(receiver_path)) = (shape.receiver, &shape.receiver_path) {
            if !receiver_path.contains(['.', ':']) {
                if let Some(declared) = self.facts.declared_type(receiver, receiver_path) {
                    let declared = bare_type(declared);
                    let qualified = self.qualify_rust_path(&declared);
                    let (namespace, type_name) = match qualified.rsplit_once("::") {
                        Some((n, t)) => (vec![n.to_string()], t.to_string()),
                        None => (Vec::new(), qualified.clone()),
                    };
                    keys.push((
                        Key::Typed {
                            namespaces: namespace,
                            type_name,
                        },
                        Tier::Typed,
                    ));
                }
            }
            return;
        }
        let Some(path) = &shape.path else { return };
        if !path.contains("::") && facts.defined_here {
            return;
        }
        let qualified = self.qualify_rust_path(path);
        if qualified.contains("::") {
            keys.push((Key::Path(qualified), Tier::Imported));
        }
    }

    /// A Rust path's first segment through the `use`s (`Command::new` →
    /// `std::process::Command::new`).
    fn qualify_rust_path(&self, path: &str) -> String {
        let (head, rest) = match path.split_once("::") {
            Some((head, rest)) => (head, Some(rest)),
            None => (path, None),
        };
        let base = match self.facts.imports.get(head) {
            Some(import) => import.clone(),
            None => head.to_string(),
        };
        match rest {
            Some(rest) => format!("{base}::{rest}"),
            None => base,
        }
    }

    /// C/C++: a free function not defined in the project, or a qualified
    /// one (`std::getenv`).
    fn c_keys(&self, shape: &CallShape, facts: &CallFacts, keys: &mut Vec<(Key, Tier)>) {
        if shape.receiver.is_some() || facts.defined_here {
            return;
        }
        if let Some(path) = &shape.path {
            keys.push((Key::Path(path.clone()), Tier::Imported));
        }
    }

    /// The models of `shape`'s callee that `key` names.
    fn lookup(&self, shape: &CallShape, key: &Key) -> Vec<&'a Model> {
        let language = self.model_language;
        let arity_fits = |model: &Model| {
            model
                .arity
                .is_none_or(|n| n as usize == shape.args.len() + shape.keywords.len())
        };
        match key {
            Key::Path(path) => {
                let name = if shape.constructor {
                    last_segment(path).to_string()
                } else {
                    shape.name.clone()
                };
                let mut out: Vec<&Model> = self
                    .models
                    .named(&name)
                    .filter(|m| arity_fits(m) && self.path_matches(m, path, shape.constructor))
                    .collect();
                // A JS module that is itself a function.
                if out.is_empty() && language == ModelLanguage::JavaScript {
                    out = self
                        .models
                        .named("")
                        .filter(|m| m.namespace == *path)
                        .collect();
                }
                out
            }
            Key::Typed {
                namespaces,
                type_name,
            } => {
                let exact: Vec<&Model> = self
                    .models
                    .named(&shape.name)
                    .filter(|m| {
                        arity_fits(m)
                            && m.type_name == *type_name
                            && (namespaces.is_empty()
                                || namespaces
                                    .iter()
                                    .any(|n| self.namespace_matches(&m.namespace, n)))
                    })
                    .collect();
                if !exact.is_empty() || language != ModelLanguage::Java {
                    return exact;
                }
                // A model of a supertype, by Java's naming: `HttpServletRequest`
                // is a `ServletRequest`, `PreparedStatement` a `Statement`.
                // (Whatever the model's `subtypes`: that is about overrides,
                // and a subtype that inherits the method calls the model's.)
                self.models
                    .named(&shape.name)
                    .filter(|m| {
                        arity_fits(m)
                            && !m.type_name.is_empty()
                            && type_name.len() > m.type_name.len()
                            && type_name.ends_with(&m.type_name)
                            && namespaces.iter().any(|n| related_packages(n, &m.namespace))
                    })
                    .collect()
            }
            Key::Name => self
                .models
                .named(&shape.name)
                .filter(|m| arity_fits(m) && self.in_scope(m))
                .collect(),
        }
    }

    /// Whether the file can reach the model's library at all: its package
    /// (Java: imported, wildcard-imported, the file's own or `java.lang`),
    /// crate (Rust: a `use` of it, or std) or module (Python/JS: imported).
    /// A name alone is only evidence within that reach — rusqlite's
    /// `Statement::query_map` is not mysql's `Queryable::query_map`.
    fn in_scope(&self, model: &Model) -> bool {
        let facts = self.facts;
        match self.model_language {
            ModelLanguage::Cpp => true,
            ModelLanguage::Java => {
                let package = model.namespace.as_str();
                package == "java.lang"
                    || facts.package.as_deref() == Some(package)
                    || facts.wildcards.iter().any(|w| w == package)
                    || facts
                        .imports
                        .values()
                        .any(|path| path.rsplit_once('.').is_some_and(|(p, _)| p == package))
            }
            ModelLanguage::Rust => {
                let krate = model.namespace.split("::").next().unwrap_or_default();
                RUST_STD.contains(&krate)
                    || facts
                        .imports
                        .values()
                        .chain(facts.wildcards.iter())
                        .any(|path| path.split("::").next() == Some(krate))
            }
            ModelLanguage::Python | ModelLanguage::JavaScript => {
                let root = model.namespace.split('.').next().unwrap_or_default();
                facts
                    .imports
                    .values()
                    .any(|path| path.split('.').next() == Some(root))
            }
        }
    }

    /// Whether `model` is the callable `path` names (a constructor: its
    /// type's path).
    fn path_matches(&self, model: &Model, path: &str, constructor: bool) -> bool {
        let language = self.model_language;
        if constructor {
            let sep = language.separator();
            let type_path = if model.namespace.is_empty() {
                model.type_name.clone()
            } else {
                format!("{}{sep}{}", model.namespace, model.type_name)
            };
            return model.name == model.type_name && type_path == path;
        }
        if language != ModelLanguage::Rust {
            if model.callable(language) == path {
                return true;
            }
            // A Python class called (`zipfile.ZipFile(f)`): the model is
            // named like its type.
            return model.name == model.type_name
                && !model.type_name.is_empty()
                && format!("{}.{}", model.namespace, model.type_name) == path;
        }
        // Rust: `std`/`core`/`alloc` are one library; a module path may be
        // the defining (`std::io::error`) or the public (`std::io`) one.
        let mut segments: Vec<&str> = path.split("::").collect();
        let Some(name) = segments.pop() else {
            return false;
        };
        if name != model.name {
            return false;
        }
        if !model.type_name.is_empty() {
            match segments.pop() {
                Some(type_name) if type_name == model.type_name => {}
                _ => return false,
            }
        }
        let namespace = segments.join("::");
        self.namespace_matches(&model.namespace, &namespace)
    }

    /// Whether a model's namespace is the one a call names: equal, or —
    /// Rust — the same crate (std/core/alloc as one) with one module path
    /// a prefix of the other.
    fn namespace_matches(&self, model: &str, called: &str) -> bool {
        if model == called {
            return true;
        }
        if self.model_language != ModelLanguage::Rust {
            return false;
        }
        let normalize = |ns: &str| -> Vec<String> {
            let mut parts: Vec<String> = ns.split("::").map(str::to_string).collect();
            if let Some(first) = parts.first_mut() {
                if RUST_STD.contains(&first.as_str()) {
                    *first = "std".into();
                }
            }
            parts
        };
        let (a, b) = (normalize(model), normalize(called));
        if a.first() != b.first() || a.is_empty() {
            return false;
        }
        let shorter = a.len().min(b.len());
        a[..shorter] == b[..shorter]
    }
}

/// Java packages close enough for a subtype by name: one a prefix of the
/// other (`javax.servlet` / `javax.servlet.http`) or the same.
fn related_packages(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
}

/// The nodes a model position names on a call.
pub fn position_nodes<'t>(shape: &CallShape<'t>, pos: &Pos) -> Vec<Node<'t>> {
    match pos {
        Pos::Ret | Pos::Read => vec![shape.call],
        Pos::Recv => shape.receiver.into_iter().collect(),
        Pos::Arg { n, keyword } => {
            if let Some(arg) = shape.args.get(*n as usize) {
                return vec![*arg];
            }
            keyword
                .as_ref()
                .and_then(|k| shape.keywords.iter().find(|(name, _)| name == k))
                .map(|(_, value)| vec![*value])
                .unwrap_or_default()
        }
        Pos::ArgsFrom(n) => shape.args.iter().skip(*n as usize).copied().collect(),
    }
}

/// How a project's calls matched models, per language and tier.
#[derive(Debug, Default, Serialize)]
pub struct MatchReport {
    pub root: String,
    pub models: String,
    pub languages: Vec<LanguageMatch>,
}

/// One language's calls.
#[derive(Debug, Default, Serialize)]
pub struct LanguageMatch {
    pub language: String,
    pub files: usize,
    /// Every call node.
    pub calls: usize,
    /// Calls the index resolved to project code (models never apply).
    pub project_calls: usize,
    /// The rest: library (or unknown) calls.
    pub library_calls: usize,
    /// Library calls whose callee name some model has.
    pub named_like_a_model: usize,
    /// Library calls matched, per tier.
    pub matched: std::collections::BTreeMap<String, usize>,
    /// Matched calls per role (a call may count under several).
    pub by_role: std::collections::BTreeMap<String, usize>,
    /// The most frequent matched callables.
    pub top: Vec<(String, usize)>,
    /// The most frequent library callee names with models that no key
    /// matched (what a better typing would find).
    pub unmatched_named: Vec<(String, usize)>,
}

impl MatchReport {
    pub fn text(&self) -> String {
        let mut out = format!("models ({}) against {}\n", self.models, self.root);
        for l in &self.languages {
            let matched: usize = l.matched.values().sum();
            let pct = |n: usize, d: usize| {
                if d == 0 {
                    0.0
                } else {
                    100.0 * n as f64 / d as f64
                }
            };
            out.push_str(&format!(
                "\n{}: {} files, {} calls ({} project, {} library)\n  \
                 named like a model: {} ({:.1}% of library calls)\n  \
                 matched: {} ({:.1}% of library calls, {:.1}% of those named like a model)\n",
                l.language,
                l.files,
                l.calls,
                l.project_calls,
                l.library_calls,
                l.named_like_a_model,
                pct(l.named_like_a_model, l.library_calls),
                matched,
                pct(matched, l.library_calls),
                pct(matched, l.named_like_a_model),
            ));
            for (tier, n) in &l.matched {
                out.push_str(&format!("    by {tier:<9} {n:>7}\n"));
            }
            for (role, n) in &l.by_role {
                out.push_str(&format!("    {role:<12} {n:>7}\n"));
            }
            if !l.top.is_empty() {
                out.push_str("  top matched:\n");
                for (callable, n) in l.top.iter().take(15) {
                    out.push_str(&format!("    {n:>6}  {callable}\n"));
                }
            }
            if !l.unmatched_named.is_empty() {
                out.push_str("  named like a model, unmatched:\n");
                for (name, n) in l.unmatched_named.iter().take(15) {
                    out.push_str(&format!("    {n:>6}  {name}\n"));
                }
            }
        }
        out
    }
}
