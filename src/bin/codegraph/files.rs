use super::{
    CodeGraph,
    FileRecord,
    OpenOptions,
    bold,
    cyan,
    dim,
    error_msg,
    get_glyphs,
    info,
    is_initialized,
    parse_int_js,
    process,
    resolve_project_path,
};

mod tree;

use tree::print_file_tree;

fn glob_to_regex_str(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                if chars.get(i) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 1;
                } else {
                    out.push_str(".*");
                }
            }
            '*' => {
                out.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            '{' => {
                if let Some(close) = chars[i + 1..].iter().position(|c| *c == '}') {
                    let end = i + 1 + close;
                    let body: String = chars[i + 1..end].iter().collect();
                    let alts: Vec<String> = body
                        .split(',')
                        .filter(|alt| !alt.is_empty())
                        .map(regex::escape)
                        .collect();
                    if alts.is_empty() {
                        out.push_str("\\{");
                    } else {
                        out.push_str("(?:");
                        out.push_str(&alts.join("|"));
                        out.push(')');
                        i = end + 1;
                        continue;
                    }
                } else {
                    out.push_str("\\{");
                }
                i += 1;
            }
            c => {
                if matches!(
                    c,
                    '.' | '+' | '^' | '$' | '(' | ')' | '|' | '[' | ']' | '\\' | '}'
                ) {
                    out.push('\\');
                }
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// codegraph files
pub(crate) fn cmd_files(
    path_arg: Option<&str>,
    filter: Option<&str>,
    pattern: Option<&str>,
    format: &str,
    max_depth_arg: Option<&str>,
    include_metadata: bool,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let mut files = cg.get_files().map_err(|e| e.to_string())?;

        if files.is_empty() {
            info("No files indexed. Run \"codegraph index\" first.");
            cg.close();
            return Ok(());
        }

        // Filter by path prefix
        if let Some(filter) = filter {
            let dotted = format!("./{filter}");
            files.retain(|f| f.path.starts_with(filter) || f.path.starts_with(&dotted));
        }

        // Filter by glob pattern
        if let Some(pattern) = pattern {
            let regex =
                regex::Regex::new(&glob_to_regex_str(pattern)).map_err(|e| e.to_string())?;
            files.retain(|f| regex.is_match(&f.path));
        }

        if files.is_empty() {
            info("No files found matching the criteria.");
            cg.close();
            return Ok(());
        }

        // JSON output
        if json {
            let output: Vec<serde_json::Value> = files
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "path": f.path,
                        "language": f.language.as_str(),
                        "nodeCount": f.node_count,
                        "size": f.size,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
            );
            cg.close();
            return Ok(());
        }

        let max_depth = max_depth_arg.and_then(parse_int_js);

        // Format output
        match format {
            "flat" => {
                println!("{}", bold(&format!("\nFiles ({}):\n", files.len())));
                let mut sorted = files.clone();
                sorted.sort_by(|a, b| a.path.cmp(&b.path));
                for file in &sorted {
                    if include_metadata {
                        println!(
                            "  {} {}",
                            file.path,
                            dim(&format!(
                                "({}, {} symbols)",
                                file.language.as_str(),
                                file.node_count
                            ))
                        );
                    } else {
                        println!("  {}", file.path);
                    }
                }
            }
            "grouped" => {
                println!(
                    "{}",
                    bold(&format!("\nFiles by Language ({} total):\n", files.len()))
                );
                // Insertion-ordered Map<lang, files> (TS `Map`).
                let mut by_lang: Vec<(String, Vec<&FileRecord>)> = Vec::new();
                for file in &files {
                    let lang = file.language.as_str().to_string();
                    match by_lang.iter_mut().find(|(l, _)| *l == lang) {
                        Some((_, list)) => list.push(file),
                        None => by_lang.push((lang, vec![file])),
                    }
                }
                by_lang.sort_by_key(|(_, list)| std::cmp::Reverse(list.len()));
                for (lang, mut lang_files) in by_lang {
                    println!("{}", cyan(&format!("{lang} ({}):", lang_files.len())));
                    lang_files.sort_by(|a, b| a.path.cmp(&b.path));
                    for file in lang_files {
                        if include_metadata {
                            println!(
                                "  {} {}",
                                file.path,
                                dim(&format!("({} symbols)", file.node_count))
                            );
                        } else {
                            println!("  {}", file.path);
                        }
                    }
                    println!();
                }
            }
            _ => {
                // "tree" and unknown formats fall through to tree (TS default)
                println!(
                    "{}",
                    bold(&format!("\nProject Structure ({} files):\n", files.len()))
                );
                print_file_tree(&files, include_metadata, max_depth);
            }
        }

        println!();
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to list files: {msg}"));
        process::exit(1);
    }
}
