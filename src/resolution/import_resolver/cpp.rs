//! C/C++ include path and compile database support.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use super::paths::extension_resolution;
use crate::resolution::path_aliases::relative_lexical;
use crate::resolution::types::ResolutionContext;
use crate::types::Language;
use crate::utils::lexical_resolve;

/// C/C++ include directory cache (keyed by project root).
/// Loaded once per resolver instance, shared across calls.
static CPP_INCLUDE_DIR_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear the C/C++ include directory cache (call between indexing runs)
pub fn clear_cpp_include_dir_cache() {
    CPP_INCLUDE_DIR_CACHE.lock().unwrap().clear();
}

/// Discover C/C++ include search directories for a project.
///
/// Strategy:
/// 1. Look for compile_commands.json (Clang compilation database) in the
///    project root and common build subdirectories. Parse -I and -isystem
///    flags from compiler commands.
/// 2. If no compilation database is found, probe for common convention
///    directories (include/, src/, lib/, api/) and top-level directories
///    containing .h/.hpp files.
///
/// Returns paths relative to projectRoot.
pub fn load_cpp_include_dirs(project_root: &str) -> Vec<String> {
    if let Some(cached) = CPP_INCLUDE_DIR_CACHE.lock().unwrap().get(project_root) {
        return cached.clone();
    }

    let dirs = load_cpp_include_dirs_from_compile_db(project_root)
        .unwrap_or_else(|| load_cpp_include_dirs_heuristic(project_root));

    CPP_INCLUDE_DIR_CACHE
        .lock()
        .unwrap()
        .insert(project_root.to_string(), dirs.clone());
    dirs
}

/// One entry of a Clang compilation database. Type mismatches fail the
/// whole parse — mirroring the TS version, where a malformed entry threw
/// inside the `try` and the function returned `null`.
#[derive(serde::Deserialize)]
struct CompileDbEntry {
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    arguments: Option<Vec<String>>,
}

/// Try to load include directories from compile_commands.json.
/// Returns `None` if no compilation database is found (so the heuristic
/// fallback can run). Returns a vec (possibly empty) otherwise.
fn load_cpp_include_dirs_from_compile_db(project_root: &str) -> Option<Vec<String>> {
    let root = Path::new(project_root);
    let candidates = [
        root.join("compile_commands.json"),
        root.join("build").join("compile_commands.json"),
        root.join("cmake-build-debug").join("compile_commands.json"),
        root.join("cmake-build-release")
            .join("compile_commands.json"),
        root.join("out").join("compile_commands.json"),
    ];

    let db_path = candidates.iter().find(|c| c.exists())?;

    let content = std::fs::read_to_string(db_path).ok()?;
    let entries: Vec<CompileDbEntry> = serde_json::from_str(&content).ok()?;

    // Insertion-ordered set (TS used `Set` + `Array.from`).
    let mut dir_set: Vec<String> = Vec::new();
    for entry in &entries {
        let dir = match entry.directory.as_deref() {
            Some(d) if !d.is_empty() => d,
            _ => project_root,
        };
        let split;
        let args: &[String] = match (&entry.arguments, &entry.command) {
            (Some(args), _) => args,
            (None, Some(cmd)) => {
                split = shlex_split(cmd);
                &split
            }
            (None, None) => &[],
        };
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            let mut include_dir: Option<&str> = None;
            // -I<dir> (no space)
            if arg.starts_with("-I") && arg.len() > 2 {
                include_dir = Some(&arg[2..]);
            }
            // -isystem <dir> (space-separated)
            else if (arg == "-isystem" || arg == "-I") && i + 1 < args.len() {
                include_dir = Some(&args[i + 1]);
                i += 1; // skip next arg
            }
            if let Some(include_dir) = include_dir {
                // Normalize: resolve relative to the compilation directory
                let abs_path = if Path::new(include_dir).is_absolute() {
                    lexical_resolve(Path::new(""), include_dir)
                } else {
                    lexical_resolve(Path::new(dir), include_dir)
                };
                let rel_path =
                    relative_lexical(&lexical_resolve(Path::new(""), project_root), &abs_path)
                        .replace('\\', "/");
                // Skip system directories and paths outside the project
                // (relative paths starting with .. or absolute paths like
                // /usr/include or C:\usr on Windows)
                if !rel_path.starts_with("..")
                    && !rel_path.is_empty()
                    && !Path::new(&rel_path).is_absolute()
                    && !dir_set.contains(&rel_path)
                {
                    dir_set.push(rel_path);
                }
            }
            i += 1;
        }
    }
    Some(dir_set)
}

/// Minimal shlex-style split for compiler command strings.
/// Handles double-quoted and single-quoted arguments.
fn shlex_split(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // Skip whitespace
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let ch = chars[i];
        if ch == '"' {
            i += 1;
            let mut arg = String::new();
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    arg.push(chars[i]);
                } else {
                    arg.push(chars[i]);
                }
                i += 1;
            }
            i += 1; // closing quote
            result.push(arg);
        } else if ch == '\'' {
            i += 1;
            let mut arg = String::new();
            while i < chars.len() && chars[i] != '\'' {
                arg.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote
            result.push(arg);
        } else {
            let mut arg = String::new();
            while i < chars.len() && !chars[i].is_whitespace() {
                arg.push(chars[i]);
                i += 1;
            }
            result.push(arg);
        }
    }
    result
}

/// Heuristic include directory discovery when no compile_commands.json exists.
/// Checks common convention directories and scans top-level dirs for headers.
fn load_cpp_include_dirs_heuristic(project_root: &str) -> Vec<String> {
    static HEADER_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\.(h|hpp|hxx|hh)$").expect("valid regex"));
    let mut dirs: Vec<String> = Vec::new();
    let convention_dirs = ["include", "src", "lib", "api", "inc"];

    let entries = match std::fs::read_dir(project_root) {
        Ok(rd) => rd,
        Err(_) => return dirs,
    };
    for entry in entries.flatten() {
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Convention directories
        if convention_dirs.contains(&name.to_lowercase().as_str()) {
            dirs.push(name);
            continue;
        }
        // Any top-level directory containing .h or .hpp files
        if let Ok(sub) = std::fs::read_dir(Path::new(project_root).join(&name)) {
            let has_header = sub
                .flatten()
                .any(|f| HEADER_RE.is_match(f.file_name().to_string_lossy().as_ref()));
            if has_header {
                dirs.push(name);
            }
        }
        // ignore permission errors
    }

    dirs
}

/// Resolve a C/C++ include path by searching include directories.
/// Called as a fallback after relative and aliased resolution fail.
pub(super) fn resolve_cpp_include_path(
    import_path: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let include_dirs = context.get_cpp_include_dirs();
    let extensions = extension_resolution(language);

    for dir in &include_dirs {
        let normalized_dir = dir.replace('\\', "/");
        for ext in extensions {
            let candidate = format!("{normalized_dir}/{import_path}{ext}");
            if context.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        // Try as-is (already has extension)
        let candidate = format!("{normalized_dir}/{import_path}");
        if context.file_exists(&candidate) {
            return Some(candidate);
        }
    }

    None
}
