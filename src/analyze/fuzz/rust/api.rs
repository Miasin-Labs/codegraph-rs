//! Which items a crate exposes, and by what path: what a fuzz target — an
//! outside crate — can call.
//!
//! Read from the index: `module` nodes (their declarations), `import` nodes
//! whose text is a `pub use` (re-exports, globs included, one level), and
//! type nodes. The one thing the index does not keep is a *restricted*
//! visibility (`pub(crate)` is stored as public), so a declaration's own
//! line is read to tell `pub` from `pub(crate)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use toml::de::{DeTable, DeValue};

use super::super::sites::FnSyntax;
use crate::analyze::bugs::FnSpan;

/// A library crate in the project.
#[derive(Debug, Clone)]
pub struct CrateInfo {
    /// Project-relative directory holding `Cargo.toml` (`""` at the root).
    pub dir: String,
    /// `[package] name`.
    pub package: String,
    /// The name code uses (`[lib] name`, or the package with `-` → `_`).
    pub lib_name: String,
    /// Project-relative directory of the library root file (`src`).
    pub src_dir: String,
    /// Project-relative library root file (`src/lib.rs`).
    pub lib_root: String,
}

#[derive(Debug, Clone)]
struct ModuleNode {
    name: String,
    start_line: u32,
    end_line: u32,
}

#[derive(Debug, Clone)]
struct TypeNode {
    name: String,
    kind: String,
    file: String,
    start_line: u32,
}

#[derive(Debug, Clone)]
struct ReExport {
    /// Module (absolute, within the crate) the item comes from.
    source: Vec<String>,
    /// The item's name there; `None` for a glob.
    item: Option<String>,
    /// The module re-exporting it.
    module: Vec<String>,
    /// `as` name, if renamed.
    alias: Option<String>,
}

/// The crate API as the index sees it.
pub struct RustApi {
    root: PathBuf,
    crates: Vec<CrateInfo>,
    modules: HashMap<String, Vec<ModuleNode>>,
    types: Vec<TypeNode>,
    reexports: HashMap<String, Vec<ReExport>>,
    lines: std::cell::RefCell<HashMap<String, Option<Vec<String>>>>,
}

impl RustApi {
    pub fn load(conn: &Connection, root: &Path, files: &[String]) -> Result<Self, String> {
        let err = |e: rusqlite::Error| e.to_string();
        let crates = discover_crates(root, files);
        let mut modules: HashMap<String, Vec<ModuleNode>> = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT file_path, name, start_line, end_line FROM nodes \
                     WHERE kind = 'module' AND language = 'rust'",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        ModuleNode {
                            name: row.get(1)?,
                            start_line: row.get(2)?,
                            end_line: row.get(3)?,
                        },
                    ))
                })
                .map_err(err)?;
            for row in rows {
                let (file, module) = row.map_err(err)?;
                modules.entry(file).or_default().push(module);
            }
        }
        let types = {
            let mut stmt = conn
                .prepare(
                    "SELECT name, kind, file_path, start_line FROM nodes \
                     WHERE kind IN ('struct', 'enum', 'trait', 'type_alias', 'union') \
                       AND language = 'rust'",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(TypeNode {
                        name: row.get(0)?,
                        kind: row.get(1)?,
                        file: row.get(2)?,
                        start_line: row.get(3)?,
                    })
                })
                .map_err(err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(err)?
        };
        let uses: Vec<(String, u32, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT file_path, start_line, signature FROM nodes \
                     WHERE kind = 'import' AND language = 'rust' AND signature LIKE 'pub use%'",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(err)?
        };
        let mut api = Self {
            root: root.to_path_buf(),
            crates,
            modules,
            types,
            reexports: HashMap::new(),
            lines: std::cell::RefCell::new(HashMap::new()),
        };
        api.reexports = api.collect_reexports(&uses);
        Ok(api)
    }

    pub fn crates(&self) -> &[CrateInfo] {
        &self.crates
    }

    /// The library crate `file` belongs to (`None` for binaries, examples,
    /// tests, build scripts and files outside any crate).
    pub fn crate_of(&self, file: &str) -> Option<&CrateInfo> {
        let krate = self
            .crates
            .iter()
            .filter(|krate| krate.dir.is_empty() || file.starts_with(&format!("{}/", krate.dir)))
            .max_by_key(|krate| krate.dir.len())?;
        let inside_src = file.starts_with(&format!("{}/", krate.src_dir));
        let binary = file.contains("/bin/") || file.ends_with("/main.rs");
        (inside_src && !binary).then_some(krate)
    }

    /// The module path of `file` within its crate (`src/a/b.rs` → `[a, b]`).
    pub fn file_module(&self, krate: &CrateInfo, file: &str) -> Vec<String> {
        if file == krate.lib_root {
            return Vec::new();
        }
        let relative = file
            .strip_prefix(&format!("{}/", krate.src_dir))
            .unwrap_or(file)
            .trim_end_matches(".rs");
        let mut segments: Vec<String> = relative.split('/').map(str::to_string).collect();
        if segments.last().is_some_and(|last| last == "mod") {
            segments.pop();
        }
        segments
    }

    /// The module path at `line` of `file`: the file's module plus the
    /// inline `mod x { … }` blocks enclosing the line.
    pub fn module_at(&self, krate: &CrateInfo, file: &str, line: u32) -> Vec<String> {
        let mut path = self.file_module(krate, file);
        let mut inline: Vec<&ModuleNode> = self
            .modules
            .get(file)
            .map(|modules| {
                modules
                    .iter()
                    .filter(|m| {
                        m.end_line > m.start_line && m.start_line < line && line <= m.end_line
                    })
                    .collect()
            })
            .unwrap_or_default();
        inline.sort_by_key(|m| m.start_line);
        path.extend(inline.into_iter().map(|m| m.name.clone()));
        path
    }

    /// Every module on `path` is declared `pub` (not `pub(crate)`).
    pub fn module_is_public(&self, krate: &CrateInfo, path: &[String]) -> bool {
        (0..path.len()).all(|depth| self.module_decl_is_pub(krate, &path[..depth], &path[depth]))
    }

    fn module_decl_is_pub(&self, krate: &CrateInfo, parent: &[String], name: &str) -> bool {
        // An inline module lives in its parent's file; a file module is
        // declared there too (`mod name;`).
        for file in self.module_files(krate, parent) {
            let Some(modules) = self.modules.get(&file) else {
                continue;
            };
            for module in modules.iter().filter(|m| m.name == name) {
                if self.module_at(krate, &file, module.start_line) == parent {
                    return self.line_is_pub(&file, module.start_line, "mod");
                }
            }
        }
        // Inline modules nest deeper in one file: find a parent file up the
        // path whose inline chain leads here.
        for cut in (0..parent.len()).rev() {
            for file in self.module_files(krate, &parent[..cut]) {
                if let Some(modules) = self.modules.get(&file) {
                    for module in modules.iter().filter(|m| m.name == name) {
                        if self.module_at(krate, &file, module.start_line) == parent {
                            return self.line_is_pub(&file, module.start_line, "mod");
                        }
                    }
                }
            }
        }
        false
    }

    /// Files that can hold module `path` (`src/a.rs`, `src/a/mod.rs`).
    fn module_files(&self, krate: &CrateInfo, path: &[String]) -> Vec<String> {
        if path.is_empty() {
            return vec![krate.lib_root.clone()];
        }
        let joined = path.join("/");
        vec![
            format!("{}/{joined}.rs", krate.src_dir),
            format!("{}/{joined}/mod.rs", krate.src_dir),
        ]
    }

    /// The declaration at `line` of `file` is plain `pub` (read from the
    /// source: the index stores `pub(crate)` as public).
    pub fn line_is_pub(&self, file: &str, line: u32, _keyword: &str) -> bool {
        self.line(file, line)
            .is_some_and(|text| visibility_is_pub(text.trim_start()))
    }

    fn line(&self, file: &str, line: u32) -> Option<String> {
        let mut cache = self.lines.borrow_mut();
        let lines = cache.entry(file.to_string()).or_insert_with(|| {
            std::fs::read_to_string(self.root.join(file))
                .ok()
                .map(|text| text.lines().map(str::to_string).collect())
        });
        lines
            .as_ref()
            .and_then(|lines| lines.get(line.saturating_sub(1) as usize).cloned())
    }

    /// Up to `count` lines just above `line` of `file` (attributes).
    pub fn lines_above(&self, file: &str, line: u32, count: u32) -> Vec<String> {
        (line.saturating_sub(count).max(1)..line)
            .filter_map(|l| self.line(file, l))
            .collect()
    }

    fn collect_reexports(&self, uses: &[(String, u32, String)]) -> HashMap<String, Vec<ReExport>> {
        let mut out: HashMap<String, Vec<ReExport>> = HashMap::new();
        for (file, line, text) in uses {
            let Some(krate) = self.crate_of(file) else {
                continue;
            };
            let here = self.module_at(krate, file, *line);
            let Some(tree) = text
                .trim()
                .strip_prefix("pub use")
                .map(|rest| rest.trim().trim_end_matches(';').trim())
            else {
                continue;
            };
            for (segments, alias) in flatten_use_tree(tree) {
                let Some((last, module)) = segments.split_last() else {
                    continue;
                };
                let Some(source) = resolve_module(&here, module, |first| {
                    self.module_exists(krate, &[here.as_slice(), &[first.to_string()]].concat())
                }) else {
                    continue;
                };
                out.entry(krate.dir.clone()).or_default().push(ReExport {
                    source,
                    item: (last != "*").then(|| last.clone()),
                    module: here.clone(),
                    alias,
                });
            }
        }
        out
    }

    fn module_exists(&self, krate: &CrateInfo, path: &[String]) -> bool {
        let Some((name, parent)) = path.split_last() else {
            return true;
        };
        self.module_files(krate, parent).iter().any(|file| {
            self.modules
                .get(file)
                .is_some_and(|modules| modules.iter().any(|m| &m.name == name))
        })
    }

    /// Crate-relative public paths of item `name` declared in `module`,
    /// shortest first: its own path when every module on it is `pub`, and
    /// every `pub use` chain (named or glob, a few hops) that lands in a
    /// public module. An item not declared `pub` has none.
    pub fn public_paths(
        &self,
        krate: &CrateInfo,
        module: &[String],
        name: &str,
        declared_pub: bool,
    ) -> Vec<Vec<String>> {
        if !declared_pub {
            return Vec::new();
        }
        let reexports = self
            .reexports
            .get(&krate.dir)
            .map_or(&[][..], Vec::as_slice);
        let mut locations: Vec<(Vec<String>, String)> = vec![(module.to_vec(), name.to_string())];
        let mut next = 0;
        while next < locations.len() && locations.len() < 64 {
            let (at, item) = locations[next].clone();
            next += 1;
            for reexport in reexports.iter().filter(|r| r.source == at) {
                let exported = match (&reexport.item, &reexport.alias) {
                    (Some(named), alias) if *named == item => {
                        alias.clone().unwrap_or_else(|| item.clone())
                    }
                    (None, _) => item.clone(),
                    _ => continue,
                };
                let location = (reexport.module.clone(), exported);
                if !locations.contains(&location) {
                    locations.push(location);
                }
            }
        }
        let mut paths: Vec<Vec<String>> = locations
            .into_iter()
            .filter(|(at, _)| self.module_is_public(krate, at))
            .map(|(mut at, item)| {
                at.push(item);
                at
            })
            .collect();
        paths.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        paths.dedup();
        paths
    }

    /// The public call path of a free function, `None` if unreachable.
    pub fn function_path(
        &self,
        krate: &CrateInfo,
        span: &FnSpan,
        syntax: &FnSyntax,
    ) -> Option<Vec<String>> {
        let module = self.module_at(krate, &span.file, span.start_line);
        let declared_pub = visibility_is_pub(&syntax.visibility);
        self.public_paths(krate, &module, &span.name, declared_pub)
            .into_iter()
            .next()
    }

    /// The owner type of a method named `owner`, preferring its own file.
    pub fn owner_type(&self, krate: &CrateInfo, owner: &str, file: &str) -> Option<OwnerType> {
        let mut candidates: Vec<&TypeNode> = self
            .types
            .iter()
            .filter(|t| {
                t.name == owner && self.crate_of(&t.file).is_some_and(|k| k.dir == krate.dir)
            })
            .collect();
        candidates.sort_by_key(|t| (t.file != file, t.file.clone(), t.start_line));
        let node = candidates.first()?;
        let module = self.module_at(krate, &node.file, node.start_line);
        let declared_pub = self.line_is_pub(&node.file, node.start_line, &node.kind);
        let path = self
            .public_paths(krate, &module, owner, declared_pub)
            .into_iter()
            .next();
        let derives_default = self
            .lines_above(&node.file, node.start_line, 6)
            .iter()
            .any(|line| line.contains("derive(") && line.contains("Default"));
        let generic = self
            .line(&node.file, node.start_line)
            .is_some_and(|line| type_declares_generics(&line, owner));
        // `pub type Multihash = MultihashGeneric<Code>;` names the generic
        // owner with its parameters fixed: call through the alias.
        let alias = if generic {
            self.concrete_alias(krate, owner)
        } else {
            None
        };
        let (path, generic) = match alias {
            Some(alias) => (Some(alias), false),
            None => (path, generic),
        };
        Some(OwnerType {
            name: owner.to_string(),
            kind: node.kind.clone(),
            path,
            derives_default,
            generic,
        })
    }
}

impl RustApi {
    /// The public path of a type alias in `krate` that instantiates `owner`
    /// (`type X = Owner<Concrete>;`, `type X<'a> = Owner<'a, C>;`), taking
    /// no type parameters itself. The shortest such path.
    fn concrete_alias(&self, krate: &CrateInfo, owner: &str) -> Option<Vec<String>> {
        let mut best: Option<Vec<String>> = None;
        for alias in self.types.iter().filter(|t| {
            t.kind == "type_alias" && self.crate_of(&t.file).is_some_and(|k| k.dir == krate.dir)
        }) {
            let Some(line) = self.line(&alias.file, alias.start_line) else {
                continue;
            };
            let Some((head, rhs)) = line.split_once('=') else {
                continue;
            };
            if type_declares_generics(head, &alias.name) {
                continue;
            }
            let rhs = rhs.trim();
            let names_owner = rhs
                .strip_prefix(owner)
                .is_some_and(|rest| rest.trim_start().starts_with('<'));
            if !names_owner || !self.line_is_pub(&alias.file, alias.start_line, "type") {
                continue;
            }
            let module = self.module_at(krate, &alias.file, alias.start_line);
            let path = self
                .public_paths(krate, &module, &alias.name, true)
                .into_iter()
                .next();
            if let Some(path) = path.filter(|p| best.as_ref().is_none_or(|b| p.len() < b.len())) {
                best = Some(path);
            }
        }
        best
    }
}

/// A method's owner type as the harness needs it.
#[derive(Debug, Clone)]
pub struct OwnerType {
    pub name: String,
    pub kind: String,
    /// Crate-relative public path, if exposed.
    pub path: Option<Vec<String>>,
    pub derives_default: bool,
    /// Declared with type parameters (`struct Foo<T>`).
    pub generic: bool,
}

/// `struct Name<T>` (not `struct Name<'a>`: lifetimes need no naming).
fn type_declares_generics(line: &str, name: &str) -> bool {
    let Some(at) = line.find(name) else {
        return false;
    };
    let Some(list) = line[at + name.len()..].trim_start().strip_prefix('<') else {
        return false;
    };
    let list = list.split('>').next().unwrap_or(list);
    list.split(',')
        .map(str::trim)
        .any(|param| !param.is_empty() && !param.starts_with('\''))
}

/// `pub` exactly — not `pub(crate)`, `pub(super)`, `pub(in …)`.
pub fn visibility_is_pub(text: &str) -> bool {
    let text = text.trim_start();
    text == "pub" || text.starts_with("pub ")
}

/// `a::b::{C, D as E, f::*}` → `([a, b, C], None)`, `([a, b, D], Some(E))`,
/// `([a, b, f, *], None)`.
pub fn flatten_use_tree(tree: &str) -> Vec<(Vec<String>, Option<String>)> {
    let compact: String = tree.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    flatten(&compact, &[], &mut out);
    out
}

fn flatten(tree: &str, prefix: &[String], out: &mut Vec<(Vec<String>, Option<String>)>) {
    let tree = tree.trim().trim_start_matches("::");
    if let Some(open) = tree.find('{') {
        let head: Vec<String> = tree[..open]
            .split("::")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let body = tree[open + 1..].trim_end().trim_end_matches('}');
        let base = [prefix, &head].concat();
        for part in super::types::split_top_level(body, ',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if part == "self" {
                if let Some(last) = base.last() {
                    out.push((base.clone(), Some(last.clone())));
                }
                continue;
            }
            flatten(part, &base, out);
        }
        return;
    }
    let (path, alias) = match tree.split_once(" as ") {
        Some((path, alias)) => (path, Some(alias.trim().to_string())),
        None => (tree, None),
    };
    let mut segments: Vec<String> = prefix.to_vec();
    segments.extend(
        path.split("::")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    if alias.as_deref() == Some("_") {
        return;
    }
    out.push((segments, alias));
}

/// The crate-absolute module a `use` path's module part names, from module
/// `here`: `crate::a` → `[a]`, `self::a` → here+`[a]`, `super::a`, or a
/// child module named without a prefix (2018 paths). External crates → None.
fn resolve_module(
    here: &[String],
    module: &[String],
    is_child: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let Some(first) = module.first() else {
        // `pub use Item;` names something in scope here — treat as local.
        return Some(here.to_vec());
    };
    let mut base: Vec<String> = match first.as_str() {
        "crate" => Vec::new(),
        "self" => here.to_vec(),
        "super" => here[..here.len().saturating_sub(1)].to_vec(),
        name if is_child(name) => {
            let mut path = here.to_vec();
            path.push(name.to_string());
            path
        }
        _ => return None,
    };
    for segment in &module[1..] {
        if segment == "super" {
            base.pop();
        } else if segment != "self" {
            base.push(segment.clone());
        }
    }
    Some(base)
}

/// Library crates under `root`: each directory with a `Cargo.toml` that has
/// a `[package]` and a library root holding indexed files.
fn discover_crates(root: &Path, files: &[String]) -> Vec<CrateInfo> {
    let mut dirs: Vec<String> = Vec::new();
    for file in files.iter().filter(|f| f.ends_with(".rs")) {
        let mut dir = Path::new(file).parent();
        while let Some(current) = dir {
            let text = current.to_string_lossy().to_string();
            if dirs.contains(&text) {
                break;
            }
            if root.join(current).join("Cargo.toml").is_file() {
                dirs.push(text);
                break;
            }
            dir = current.parent();
        }
    }
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| read_crate(root, &dir))
        .collect()
}

fn read_crate(root: &Path, dir: &str) -> Option<CrateInfo> {
    let manifest = std::fs::read_to_string(root.join(dir).join("Cargo.toml")).ok()?;
    let doc = DeTable::parse(&manifest).ok()?;
    let doc = doc.get_ref();
    let package = toml_string(toml_table(doc, "package")?, "name")?.to_string();
    let lib = toml_table(doc, "lib");
    let lib_name = lib
        .and_then(|lib| toml_string(lib, "name"))
        .map_or_else(|| package.replace('-', "_"), str::to_string);
    let lib_path = lib
        .and_then(|lib| toml_string(lib, "path"))
        .unwrap_or("src/lib.rs")
        .trim_start_matches("./")
        .to_string();
    let join = |rel: &str| {
        if dir.is_empty() {
            rel.to_string()
        } else {
            format!("{dir}/{rel}")
        }
    };
    let lib_root = join(&lib_path);
    if !root.join(&lib_root).is_file() {
        return None;
    }
    let src_dir = Path::new(&lib_root)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Some(CrateInfo {
        dir: dir.to_string(),
        package,
        lib_name,
        src_dir,
        lib_root,
    })
}

fn toml_table<'a, 'i>(parent: &'a DeTable<'i>, key: &str) -> Option<&'a DeTable<'i>> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::Table(t) if k.get_ref() == key => Some(t),
        _ => None,
    })
}

fn toml_string<'a>(parent: &'a DeTable<'_>, key: &str) -> Option<&'a str> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::String(s) if k.get_ref() == key => Some(s.as_ref()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_trees_flatten_with_aliases_and_globs() {
        let flat = flatten_use_tree("self::bit::{BitString, Iter as BitIter, inner::*}");
        let as_text: Vec<String> = flat
            .iter()
            .map(|(path, alias)| {
                format!(
                    "{}{}",
                    path.join("::"),
                    alias
                        .as_deref()
                        .map(|a| format!(" as {a}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            as_text,
            vec![
                "self::bit::BitString",
                "self::bit::Iter as BitIter",
                "self::bit::inner::*"
            ]
        );
        assert_eq!(
            flatten_use_tree("crate::a::B"),
            vec![(vec!["crate".into(), "a".into(), "B".into()], None)]
        );
    }

    #[test]
    fn use_paths_resolve_from_the_declaring_module() {
        let here = vec!["string".to_string()];
        let seg = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolve_module(&here, &seg(&["self", "bit"]), |_| false),
            Some(seg(&["string", "bit"]))
        );
        assert_eq!(
            resolve_module(&here, &seg(&["crate", "a"]), |_| false),
            Some(seg(&["a"]))
        );
        assert_eq!(
            resolve_module(&here, &seg(&["super", "a"]), |_| false),
            Some(seg(&["a"]))
        );
        assert_eq!(
            resolve_module(&here, &seg(&["bit"]), |name| name == "bit"),
            Some(seg(&["string", "bit"]))
        );
        assert_eq!(resolve_module(&here, &seg(&["serde"]), |_| false), None);
    }

    #[test]
    fn only_plain_pub_is_public() {
        assert!(visibility_is_pub("pub fn decode("));
        assert!(visibility_is_pub("pub"));
        assert!(!visibility_is_pub("pub(crate) fn decode("));
        assert!(!visibility_is_pub("fn decode("));
        assert!(!visibility_is_pub("pub(super)"));
    }
}
