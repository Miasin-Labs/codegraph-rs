use super::*;

// =============================================================================
// affected command
// =============================================================================

/// Convert the `--filter` glob to a regex (TS inline converter:
/// `** → .+`, `* → [^/]*`, `. → \.`).
fn affected_filter_to_regex_str(filter: &str) -> String {
    // .replace(/[+[\]{}()^$|\\]/g, '\\$&')
    let mut escaped = String::new();
    for c in filter.chars() {
        if "+[]{}()^$|\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let escaped = escaped.replace('.', "\\.");
    let escaped = escaped.replace("**", ".+");
    escaped.replace('*', "[^/]*")
}

/// codegraph affected [files...]
///
/// Find test files affected by the given source files.
/// Traces dependency edges transitively to find test files that depend on
/// changed code.
///
/// Usage:
///   git diff --name-only | codegraph affected --stdin
///   codegraph affected src/lib/components/Editor.svelte src/routes/+page.svelte
pub(crate) fn cmd_affected(
    file_args: Vec<String>,
    path_arg: Option<&str>,
    stdin: bool,
    depth_arg: &str,
    filter: Option<&str>,
    json: bool,
    quiet: bool,
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

        // Collect changed files from args or stdin
        let mut changed_files: Vec<String> = file_args.clone();

        if stdin {
            let stdin_data = io::read_to_string(io::stdin()).map_err(|e| e.to_string())?;
            changed_files.extend(
                stdin_data
                    .split('\n')
                    .map(|f| f.trim().to_string())
                    .filter(|f| !f.is_empty()),
            );
        }

        if changed_files.is_empty() {
            if !quiet {
                info("No files provided. Use file arguments or --stdin.");
            }
            process::exit(0);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let max_depth = parse_int_js(depth_arg).unwrap_or(5);

        // Common test file patterns
        let default_test_patterns: Vec<regex::Regex> = [
            r"\.spec\.",
            r"\.test\.",
            r"/__tests__/",
            r"/tests?/",
            r"/e2e/",
            r"/spec/",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("static pattern"))
        .collect();

        // Custom filter pattern
        let custom_filter: Option<regex::Regex> = match filter {
            Some(f) => Some(
                regex::Regex::new(&affected_filter_to_regex_str(f)).map_err(|e| e.to_string())?,
            ),
            None => None,
        };

        let is_test_file = |file_path: &str| -> bool {
            if let Some(cf) = &custom_filter {
                return cf.is_match(file_path);
            }
            default_test_patterns.iter().any(|p| p.is_match(file_path))
        };

        // BFS to find all transitive dependents of changed files, filtered to
        // test files
        let mut affected_tests: HashSet<String> = HashSet::new();
        let mut all_dependents: HashSet<String> = HashSet::new();

        for file in &changed_files {
            // If the changed file is itself a test file, include it
            if is_test_file(file) {
                affected_tests.insert(file.clone());
                continue;
            }

            // BFS through dependents
            let mut queue: VecDeque<(String, i64)> = VecDeque::new();
            queue.push_back((file.clone(), 0));
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(file.clone());

            while let Some((current, depth)) = queue.pop_front() {
                if depth >= max_depth {
                    continue;
                }

                let dependents = cg
                    .get_file_dependents(&current)
                    .map_err(|e| e.to_string())?;
                for dep in dependents {
                    if visited.contains(&dep) {
                        continue;
                    }
                    visited.insert(dep.clone());
                    all_dependents.insert(dep.clone());

                    if is_test_file(&dep) {
                        affected_tests.insert(dep);
                    } else {
                        queue.push_back((dep, depth + 1));
                    }
                }
            }
        }

        let mut sorted_tests: Vec<String> = affected_tests.into_iter().collect();
        sorted_tests.sort();

        // Output
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changedFiles": changed_files,
                    "affectedTests": sorted_tests,
                    "totalDependentsTraversed": all_dependents.len(),
                }))
                .map_err(|e| e.to_string())?
            );
        } else if quiet {
            for t in &sorted_tests {
                println!("{t}");
            }
        } else if sorted_tests.is_empty() {
            info("No test files affected by the changed files.");
        } else {
            println!(
                "{}",
                bold(&format!(
                    "\nAffected test files ({}):\n",
                    sorted_tests.len()
                ))
            );
            for t in &sorted_tests {
                println!("  {}", cyan(t));
            }
            println!();
        }

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Affected analysis failed: {msg}"));
        process::exit(1);
    }
}
