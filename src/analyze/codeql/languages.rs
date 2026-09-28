//! Which CodeQL extractor analyses which indexed files, and how its
//! database is built.
//!
//! One row per CodeQL language (its `--language` id): the codegraph
//! languages whose files it extracts. The build mode is read from the
//! extractor's own `codeql-extractor.yml` (`build_modes:`) in the CodeQL
//! distribution: `none` (no build: Java, C#, C/C++, and the interpreted
//! languages) wherever the extractor supports it, else `autobuild` (Go,
//! Swift), which runs the project's own build.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::extraction::detect_language;
use crate::types::Language;

/// A CodeQL language and the codegraph languages it covers.
#[derive(Debug)]
pub struct CodeqlLanguage {
    /// `--language` id, and the query pack's prefix (`codeql/<id>-queries`).
    pub id: &'static str,
    pub covers: &'static [Language],
    /// Build modes when the distribution does not say (its
    /// `codeql-extractor.yml` is authoritative).
    pub fallback_modes: &'static [BuildMode],
}

/// How `codeql database create` gets the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    /// Extract the sources without building.
    None,
    /// CodeQL runs the build it detects (it must succeed).
    Autobuild,
}

impl BuildMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Autobuild => "autobuild",
        }
    }
}

use BuildMode::{Autobuild, None as NoBuild};

/// Every language CodeQL's bundle analyses. Kotlin shares the Java
/// extractor, but only a build extracts it (`none` reads Java sources
/// alone); Swift builds only on macOS.
pub const LANGUAGES: &[CodeqlLanguage] = &[
    CodeqlLanguage {
        id: "java",
        covers: &[Language::Java, Language::Kotlin],
        fallback_modes: &[NoBuild, Autobuild],
    },
    CodeqlLanguage {
        id: "javascript",
        covers: &[
            Language::Javascript,
            Language::Typescript,
            Language::Tsx,
            Language::Jsx,
            Language::Vue,
        ],
        fallback_modes: &[NoBuild],
    },
    CodeqlLanguage {
        id: "python",
        covers: &[Language::Python],
        fallback_modes: &[NoBuild],
    },
    CodeqlLanguage {
        id: "ruby",
        covers: &[Language::Ruby],
        fallback_modes: &[NoBuild],
    },
    CodeqlLanguage {
        id: "go",
        covers: &[Language::Go],
        fallback_modes: &[Autobuild],
    },
    CodeqlLanguage {
        id: "cpp",
        covers: &[Language::C, Language::Cpp],
        fallback_modes: &[NoBuild, Autobuild],
    },
    CodeqlLanguage {
        id: "csharp",
        covers: &[Language::Csharp],
        fallback_modes: &[NoBuild, Autobuild],
    },
    CodeqlLanguage {
        id: "rust",
        covers: &[Language::Rust],
        fallback_modes: &[NoBuild],
    },
    CodeqlLanguage {
        id: "swift",
        covers: &[Language::Swift],
        fallback_modes: &[Autobuild],
    },
];

/// The CodeQL language named `id`, accepting CodeQL's aliases
/// (`java-kotlin`, `javascript-typescript`, `c-cpp`, `c`, `kotlin`,
/// `typescript`).
pub fn by_id(id: &str) -> Option<&'static CodeqlLanguage> {
    let id = match id.trim().to_ascii_lowercase().as_str() {
        "java-kotlin" | "kotlin" => "java",
        "javascript-typescript" | "typescript" | "js" | "ts" => "javascript",
        "c-cpp" | "c" | "c++" => "cpp",
        "c#" | "cs" => "csharp",
        other => return LANGUAGES.iter().find(|lang| lang.id == other),
    };
    LANGUAGES.iter().find(|lang| lang.id == id)
}

/// The CodeQL languages of `files` (project-relative, the index's), with
/// the files each covers, most files first.
pub fn detect(files: &[String]) -> Vec<(&'static CodeqlLanguage, Vec<String>)> {
    let mut by_lang: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for file in files {
        let language = detect_language(file, None);
        if let Some(lang) = LANGUAGES.iter().find(|l| l.covers.contains(&language)) {
            by_lang.entry(lang.id).or_default().push(file.clone());
        }
    }
    let mut out: Vec<(&'static CodeqlLanguage, Vec<String>)> = by_lang
        .into_iter()
        .filter_map(|(id, files)| Some((by_id(id)?, files)))
        .collect();
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.id.cmp(b.0.id)));
    out
}

/// The files of `files` a CodeQL language covers.
pub fn files_of(lang: &CodeqlLanguage, files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|file| lang.covers.contains(&detect_language(file, None)))
        .cloned()
        .collect()
}

/// The build mode for `lang` in the distribution at `dist`: `none` when its
/// extractor lists it, else `autobuild` when listed; the fallback table
/// when the distribution has no extractor file for it.
pub fn build_mode(dist: &Path, lang: &CodeqlLanguage) -> Option<BuildMode> {
    let modes = std::fs::read_to_string(dist.join(lang.id).join("codeql-extractor.yml"))
        .ok()
        .map(|text| extractor_build_modes(&text))
        .filter(|modes| !modes.is_empty())
        .unwrap_or_else(|| lang.fallback_modes.to_vec());
    [NoBuild, Autobuild]
        .into_iter()
        .find(|mode| modes.contains(mode))
}

/// The `build_modes:` list of a `codeql-extractor.yml`.
fn extractor_build_modes(text: &str) -> Vec<BuildMode> {
    let mut modes = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("build_modes:") {
            inside = true;
            continue;
        }
        if inside {
            match line.trim().strip_prefix("- ") {
                Some("none") => modes.push(NoBuild),
                Some("autobuild") => modes.push(Autobuild),
                Some(_) => {}
                None if line.trim().is_empty() => {}
                None => break,
            }
        }
    }
    modes
}

/// A `--suite` value for `lang`: a bare suite name (`security-extended`,
/// `code-scanning`, `security-and-quality`) becomes the language's
/// built-in suite; a pack, `pack:path`, or query/suite file is passed on
/// (a file that exists relative to the working directory made absolute).
pub fn suite_spec(lang: &CodeqlLanguage, suite: &str) -> String {
    let suite = suite.trim();
    let bare = !suite.is_empty()
        && suite
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !suite.ends_with(".ql")
        && !suite.ends_with(".qls");
    if bare {
        let name = suite
            .strip_prefix(&format!("{}-", lang.id))
            .unwrap_or(suite);
        return format!("codeql/{0}-queries:codeql-suites/{0}-{name}.qls", lang.id);
    }
    let path = Path::new(suite);
    if !suite.contains(':') && path.exists() {
        if let Ok(absolute) = path.canonicalize() {
            return absolute.to_string_lossy().into_owned();
        }
    }
    suite.to_string()
}

/// The default suite: `<lang>-security-and-quality`.
pub const DEFAULT_SUITE: &str = "security-and-quality";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_files_name_the_build_modes() {
        let java = "name: \"java\"\nbuild_modes:\n  - autobuild\n  - manual\n  - none\n\
                    default_queries:\n  - codeql/java-queries\n";
        assert_eq!(extractor_build_modes(java), [Autobuild, NoBuild]);
        let go = "build_modes:\n  - autobuild\n  - manual\ndefault_queries:\n";
        assert_eq!(extractor_build_modes(go), [Autobuild]);
    }

    #[test]
    fn a_missing_distribution_falls_back_to_the_table() {
        let dist = Path::new("/nonexistent-codeql");
        assert_eq!(build_mode(dist, by_id("java").unwrap()), Some(NoBuild));
        assert_eq!(build_mode(dist, by_id("go").unwrap()), Some(Autobuild));
    }

    #[test]
    fn languages_are_detected_from_the_indexed_files() {
        let files: Vec<String> = ["a/A.java", "a/B.java", "web/x.ts", "tool.py", "README.md"]
            .into_iter()
            .map(String::from)
            .collect();
        let found: Vec<(&str, usize)> = detect(&files)
            .iter()
            .map(|(lang, files)| (lang.id, files.len()))
            .collect();
        assert_eq!(found, [("java", 2), ("javascript", 1), ("python", 1)]);
        assert_eq!(by_id("java-kotlin").unwrap().id, "java");
        assert_eq!(by_id("c-cpp").unwrap().id, "cpp");
        assert!(by_id("cobol").is_none());
    }

    #[test]
    fn bare_suite_names_expand_per_language() {
        let java = by_id("java").unwrap();
        assert_eq!(
            suite_spec(java, "security-extended"),
            "codeql/java-queries:codeql-suites/java-security-extended.qls"
        );
        assert_eq!(
            suite_spec(java, "java-code-scanning"),
            "codeql/java-queries:codeql-suites/java-code-scanning.qls"
        );
        assert_eq!(
            suite_spec(java, "codeql/java-queries"),
            "codeql/java-queries"
        );
        assert_eq!(
            suite_spec(java, "codeql/java-queries:Security/CWE/CWE-089"),
            "codeql/java-queries:Security/CWE/CWE-089"
        );
    }
}
