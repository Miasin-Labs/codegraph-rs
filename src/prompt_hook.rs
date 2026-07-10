//! Claude `UserPromptSubmit` front-load hook.
//!
//! Every failure path is intentionally silent. A hook must never block or alter
//! delivery of the user's prompt; it only adds context when the local graph can
//! verify that the prompt is structural.

use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use walkdir::{DirEntry, WalkDir};

use crate::codegraph::{CodeGraph, is_initialized};
use crate::mcp::tools::ToolHandler;
use crate::telemetry::Telemetry;

const MAX_INPUT_BYTES: u64 = 1_048_576;
const MAX_INJECTION_BYTES: usize = 16_000;
const MAX_PROSE_CANDIDATES: usize = 16;

#[derive(Debug, Deserialize)]
struct PromptInput {
    #[serde(default)]
    prompt: String,
    cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrontloadPlan {
    explore_root: Option<PathBuf>,
    nudge_projects: Vec<PathBuf>,
    via_sub_scan: bool,
}

fn gate(outcome: &str) {
    Telemetry::default().record_usage("cli_command", &format!("prompt-hook-gate-{outcome}"), true);
}

fn hook_disabled() -> bool {
    std::env::var("CODEGRAPH_NO_PROMPT_HOOK").as_deref() == Ok("1")
        || std::env::var("CODEGRAPH_PROMPT_HOOK").as_deref() == Ok("0")
}

fn has_structural_keyword(prompt: &str) -> bool {
    const STEMS: &[&str] = &[
        "architect",
        "structur",
        "depend",
        "implement",
        "impact",
        "explain",
        "trace",
        "affect",
        "connect",
        "caller",
        "callee",
        "dispatch",
        "reference",
        "defined",
        "definition",
        "flow",
        "path",
        "where",
        "how",
        "appel",
        "dépend",
        "llamad",
        "arquitec",
        "struktur",
        "вызыва",
        "завис",
        "архитектур",
        "wywoł",
        "залеж",
        "εξαρτ",
        "समझा",
    ];
    const UNSEGMENTED: &[&str] = &[
        "如何",
        "怎么",
        "怎麼",
        "在哪",
        "哪里",
        "哪裡",
        "流程",
        "路径",
        "路徑",
        "调用",
        "調用",
        "依赖",
        "依賴",
        "影响",
        "影響",
        "架构",
        "架構",
        "结构",
        "結構",
        "どうやって",
        "どのように",
        "呼び出",
        "依存",
        "アーキテクチャ",
        "어떻게",
        "어디",
        "호출",
        "흐름",
        "의존",
        "아키텍처",
        "كيف",
        "أين",
        "استدعاء",
        "يعتمد",
        "معماري",
        "چگونه",
        "کجا",
        "فراخوان",
        "معماری",
        "איך",
        "איפה",
        "קורא",
        "ארכיטקטור",
        "อย่างไร",
        "ที่ไหน",
        "เรียกใช้",
        "สถาปัตยกรรม",
    ];
    let lower = prompt.to_lowercase();
    if UNSEGMENTED.iter().any(|word| lower.contains(word)) {
        return true;
    }
    lower
        .split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|word| !word.is_empty())
        .any(|word| STEMS.iter().any(|stem| word.starts_with(stem)))
}

fn code_token_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").expect("valid token regex"))
}

fn call_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\(").expect("valid call regex"))
}

fn member_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\.([A-Za-z_$][A-Za-z0-9_$]*)")
            .expect("valid member regex")
    })
}

fn extract_code_tokens(prompt: &str) -> Vec<String> {
    const DOC_EXTENSIONS: &[&str] = &[
        "md", "markdown", "txt", "rst", "json", "yaml", "yml", "toml", "lock", "csv", "tsv", "log",
        "ini", "cfg", "conf", "env", "xml", "html", "htm", "png", "jpg", "jpeg", "gif", "svg",
        "pdf",
    ];
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |token: &str| {
        if seen.insert(token.to_string()) {
            output.push(token.to_string());
        }
    };
    for matched in code_token_regex().find_iter(prompt) {
        let word = matched.as_str();
        if word.contains('_')
            || word
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
        {
            push(word);
        }
    }
    for capture in call_regex().captures_iter(prompt) {
        push(&capture[1]);
    }
    for capture in member_regex().captures_iter(prompt) {
        if !DOC_EXTENSIONS
            .iter()
            .any(|extension| capture[2].eq_ignore_ascii_case(extension))
        {
            push(&capture[1]);
            push(&capture[2]);
        }
    }
    output
}

fn extract_prose_candidates(prompt: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "about",
        "after",
        "again",
        "also",
        "because",
        "before",
        "change",
        "changes",
        "check",
        "class",
        "code",
        "could",
        "details",
        "directory",
        "does",
        "error",
        "example",
        "file",
        "files",
        "folder",
        "from",
        "function",
        "have",
        "help",
        "here",
        "issue",
        "just",
        "like",
        "line",
        "make",
        "method",
        "more",
        "name",
        "need",
        "only",
        "other",
        "please",
        "problem",
        "project",
        "question",
        "really",
        "rename",
        "should",
        "show",
        "some",
        "something",
        "still",
        "test",
        "tests",
        "than",
        "thank",
        "thanks",
        "that",
        "their",
        "then",
        "there",
        "these",
        "they",
        "thing",
        "this",
        "those",
        "type",
        "update",
        "value",
        "want",
        "warning",
        "what",
        "when",
        "which",
        "while",
        "will",
        "with",
        "work",
        "working",
        "would",
        "write",
        "your",
    ];
    let stopwords = STOPWORDS.iter().copied().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for run in prompt.split(|character: char| !character.is_alphanumeric()) {
        if output.len() == MAX_PROSE_CANDIDATES {
            break;
        }
        let word = run.to_lowercase();
        let length = word.chars().count();
        if !(4..=24).contains(&length)
            || word.chars().all(|character| character.is_numeric())
            || stopwords.contains(word.as_str())
            || !seen.insert(word.clone())
        {
            continue;
        }
        output.push(word);
    }
    output
}

fn looks_like_project_root(path: &Path) -> bool {
    [
        ".git",
        "Cargo.toml",
        "package.json",
        "pnpm-workspace.yaml",
        "go.work",
        "go.mod",
        "pyproject.toml",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

fn descend(entry: &DirEntry) -> bool {
    !matches!(
        entry.file_name().to_string_lossy().as_ref(),
        ".git" | ".codegraph" | "node_modules" | "target" | "dist" | "build" | ".venv"
    )
}

fn indexed_subprojects(base: &Path) -> Vec<PathBuf> {
    let mut roots = WalkDir::new(base)
        .min_depth(1)
        .max_depth(5)
        .follow_links(false)
        .into_iter()
        .filter_entry(descend)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir() && is_initialized(entry.path()))
        .map(|entry| {
            entry
                .path()
                .canonicalize()
                .unwrap_or_else(|_| entry.into_path())
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

fn plan_frontload(cwd: &Path, prompt: &str) -> FrontloadPlan {
    let none = FrontloadPlan {
        explore_root: None,
        nudge_projects: Vec::new(),
        via_sub_scan: false,
    };
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    for ancestor in cwd.ancestors().take(6) {
        if is_initialized(ancestor) {
            return FrontloadPlan {
                explore_root: Some(ancestor.to_path_buf()),
                nudge_projects: Vec::new(),
                via_sub_scan: false,
            };
        }
    }
    if !looks_like_project_root(&cwd) {
        return none;
    }
    let projects = indexed_subprojects(&cwd);
    if projects.is_empty() {
        return none;
    }
    if projects.len() == 1 {
        return FrontloadPlan {
            explore_root: projects.into_iter().next(),
            nudge_projects: Vec::new(),
            via_sub_scan: true,
        };
    }
    let lower = prompt.to_lowercase();
    let selected = projects
        .iter()
        .find(|project| {
            let relative = project.strip_prefix(&cwd).unwrap_or(project);
            let relative = relative.to_string_lossy().replace('\\', "/").to_lowercase();
            let basename = project
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            (!relative.is_empty() && lower.contains(&relative))
                || (basename.len() >= 3
                    && lower
                        .split(|character: char| !character.is_alphanumeric())
                        .any(|word| word == basename))
        })
        .cloned();
    if let Some(selected) = selected {
        FrontloadPlan {
            explore_root: Some(selected.clone()),
            nudge_projects: projects
                .into_iter()
                .filter(|project| project != &selected)
                .collect(),
            via_sub_scan: true,
        }
    } else {
        FrontloadPlan {
            explore_root: None,
            nudge_projects: projects,
            via_sub_scan: true,
        }
    }
}

fn nudge(projects: &[PathBuf], lead: &str) -> String {
    let mut text = format!("{lead}\n");
    for project in projects {
        text.push_str(&format!("  - projectPath: \"{}\"\n", project.display()));
    }
    text
}

fn cap_text(text: &str) -> String {
    if text.len() <= MAX_INJECTION_BYTES {
        return text.to_string();
    }
    let mut end = MAX_INJECTION_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n...(truncated; call codegraph_explore for the rest)",
        &text[..end]
    )
}

fn process_input(input: PromptInput, output: &mut impl Write) -> Result<(), ()> {
    let prompt = input.prompt;
    let keyworded = has_structural_keyword(&prompt);
    let code_tokens = if keyworded {
        Vec::new()
    } else {
        extract_code_tokens(&prompt)
    };
    let prose_words = if keyworded {
        Vec::new()
    } else {
        extract_prose_candidates(&prompt)
    };
    if !keyworded && code_tokens.is_empty() && prose_words.is_empty() {
        gate("noop-shape");
        return Ok(());
    }
    let cwd = input
        .cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let plan = plan_frontload(&cwd, &prompt);
    if plan.explore_root.is_none() && plan.nudge_projects.is_empty() {
        gate("noop-no-index");
        return Ok(());
    }

    let Some(root) = plan.explore_root else {
        write!(
            output,
            "<codegraph_context note=\"CodeGraph is available for this workspace's indexed sub-projects; query one with projectPath.\">\n{}</codegraph_context>\n",
            nudge(
                &plan.nudge_projects,
                "This workspace's CodeGraph indexes live in sub-projects:"
            )
        )
        .map_err(|_| ())?;
        gate("nudge-projects");
        return Ok(());
    };

    let graph = Rc::new(CodeGraph::open_sync(&root).map_err(|_| ())?);
    let token_verified = !keyworded
        && code_tokens.iter().any(|token| {
            graph
                .get_nodes_by_name(token)
                .is_ok_and(|nodes| !nodes.is_empty())
        });
    let others = (!plan.nudge_projects.is_empty()).then(|| {
        nudge(
            &plan.nudge_projects,
            "Other indexed projects in this workspace; pass projectPath to query them:",
        )
    });
    if keyworded || token_verified {
        let handler = ToolHandler::new(Some(graph));
        let result = handler.execute("codegraph_explore", &json!({ "query": prompt }));
        let text = result
            .content
            .first()
            .map(|content| content.text.trim())
            .unwrap_or_default();
        if result.is_error != Some(true) && !text.is_empty() {
            let more = if plan.via_sub_scan {
                format!(
                    "call codegraph_explore with projectPath: &quot;{}&quot; for more",
                    root.display()
                )
            } else {
                "call codegraph_explore for more".to_string()
            };
            write!(
                output,
                "<codegraph_context note=\"Structural context from CodeGraph for this prompt; treat returned source as already read; {more}.\">\n{}{}\n</codegraph_context>\n",
                cap_text(text),
                others.as_deref().unwrap_or_default()
            )
            .map_err(|_| ())?;
            gate(if keyworded {
                "high-keyword"
            } else {
                "high-token"
            });
        } else {
            gate(if keyworded {
                "noop-explore-keyword"
            } else {
                "noop-explore-token"
            });
        }
        return Ok(());
    }

    if !graph.heal_segment_vocab_if_empty().unwrap_or(false) {
        gate("noop-vocab-empty");
        return Ok(());
    }
    let related = graph.get_segment_matches(&prose_words, 6).map_err(|_| ())?;
    if related.is_empty() {
        gate("noop-unverified");
        return Ok(());
    }
    let lines = related
        .iter()
        .map(|matched| {
            format!(
                "  - {} ({} - {}:{})",
                matched.name,
                matched.kind.as_str(),
                matched.file_path,
                matched.start_line
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let example = related
        .iter()
        .take(3)
        .map(|matched| matched.name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let project_hint = if plan.via_sub_scan {
        format!(" with projectPath: \"{}\"", root.display())
    } else {
        String::new()
    };
    write!(
        output,
        "<codegraph_context note=\"CodeGraph found indexed symbols matching this prompt; query the graph before searching files.\">\nThis project's CodeGraph index contains symbols matching this request:\n{lines}\nCall codegraph_explore ONCE{project_hint} with the relevant names in one query (for example, \"{example}\") to get source and call paths.\n{}</codegraph_context>\n",
        others.as_deref().unwrap_or_default()
    )
    .map_err(|_| ())?;
    gate("medium-segment");
    Ok(())
}

/// Run the prompt hook on standard input/output. Always returns success.
pub fn run_prompt_hook() {
    if hook_disabled() || std::io::stdin().is_terminal() {
        return;
    }
    let mut raw = String::new();
    if std::io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut raw)
        .is_err()
    {
        return;
    }
    let Ok(input) = serde_json::from_str::<PromptInput>(&raw) else {
        return;
    };
    let _ = process_input(input, &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_and_code_token_gates_are_precise() {
        assert!(has_structural_keyword("trace the dependency flow"));
        assert_eq!(
            extract_code_tokens("where does user.login call parseToken("),
            ["parseToken", "user", "login"]
        );
        assert!(extract_code_tokens("update README.md").is_empty());
        assert!(!has_structural_keyword("fix this typo"));
    }

    #[test]
    fn prose_candidates_drop_generic_prompt_words() {
        assert_eq!(
            extract_prose_candidates("please fix the checkout state machine tests"),
            ["checkout", "state", "machine"]
        );
    }

    #[test]
    fn capped_output_stays_on_a_utf8_boundary() {
        let input = "é".repeat(MAX_INJECTION_BYTES);
        let output = cap_text(&input);
        assert!(output.is_char_boundary(output.len()));
        assert!(output.contains("truncated"));
    }
}
