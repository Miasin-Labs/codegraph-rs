//! `codegraph models …` — library models for taint, imported from CodeQL's
//! Models-as-Data.

use std::path::{Path, PathBuf};

use codegraph::analyze::rules::models::mad::{ImportOptions, import_dir, write_import};
use codegraph::analyze::rules::models::{self, ModelLanguage, Role, kinds};

use super::{ModelsCommands, process, resolve_absolute};

pub(crate) fn cmd_models(command: ModelsCommands) {
    let result = match command {
        ModelsCommands::ImportCodeql {
            dir,
            out,
            generated,
            dry_run,
        } => cmd_import(&dir, out.as_deref(), generated, dry_run),
        ModelsCommands::List {
            language,
            role,
            kind,
            name,
            limit,
        } => cmd_list(
            language.as_deref(),
            role.as_deref(),
            kind.as_deref(),
            name.as_deref(),
            limit,
        ),
        ModelsCommands::Stats => cmd_stats(),
        ModelsCommands::Match { project, json } => cmd_match(project.as_deref(), json),
    };
    if let Err(message) = result {
        eprintln!("error: {message}");
        process::exit(1);
    }
}

fn cmd_import(dir: &str, out: Option<&str>, generated: bool, dry_run: bool) -> Result<(), String> {
    let root = PathBuf::from(dir);
    if !root.is_dir() {
        return Err(format!("{dir} is not a directory"));
    }
    let import = import_dir(&root, &ImportOptions { generated }).map_err(|e| e.to_string())?;
    print!("{}", import.report());
    if dry_run {
        return Ok(());
    }
    let out = out.map_or_else(
        || {
            codegraph::directory::codegraph_home()
                .join("models")
                .join("codeql")
        },
        PathBuf::from,
    );
    let written = write_import(&import, &root, &out).map_err(|e| e.to_string())?;
    println!();
    for path in &written {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        println!("wrote {} ({size} bytes)", path.display());
    }
    println!(
        "use it with CODEGRAPH_MODELS_DIR={}",
        out.canonicalize().unwrap_or(out.clone()).display()
    );
    Ok(())
}

fn parse_language(text: &str) -> Result<ModelLanguage, String> {
    let lower = text.to_ascii_lowercase();
    let wanted = match lower.as_str() {
        "c" | "c++" | "cc" => "cpp",
        "js" | "ts" | "typescript" => "javascript",
        "py" => "python",
        "rs" => "rust",
        other => other,
    };
    ModelLanguage::ALL
        .into_iter()
        .find(|l| l.as_str() == wanted)
        .ok_or_else(|| format!("no models for language `{text}`"))
}

fn cmd_list(
    language: Option<&str>,
    role: Option<&str>,
    kind: Option<&str>,
    name: Option<&str>,
    limit: usize,
) -> Result<(), String> {
    let db = models::global().ok_or("models are off (CODEGRAPH_MODELS=0)")?;
    let languages: Vec<ModelLanguage> = match language {
        Some(l) => vec![parse_language(l)?],
        None => ModelLanguage::ALL.to_vec(),
    };
    let role = role
        .map(|r| Role::parse(r).ok_or_else(|| format!("unknown role `{r}`")))
        .transpose()?;
    let mut shown = 0usize;
    let mut total = 0usize;
    for language in languages {
        let Some(models) = db.language(language) else {
            continue;
        };
        for model in &models.models {
            if role.is_some_and(|r| r != model.role)
                || name.is_some_and(|n| n != model.name)
                || kind.is_some_and(|k| !kinds::matches(&kinds::expand(model.role, k), &model.kind))
            {
                continue;
            }
            total += 1;
            if shown >= limit {
                continue;
            }
            shown += 1;
            let position =
                |pos: &Option<models::Pos>| pos.as_ref().map_or(String::new(), models::Pos::render);
            let callable = format!(
                "{}{}",
                model.callable(language),
                model.arity.map(|n| format!("/{n}")).unwrap_or_default()
            );
            println!(
                "{:<10} {:<8} {callable:<60} {:>6} -> {:<6} {}{}",
                language.as_str(),
                model.role.as_str(),
                position(&model.input),
                position(&model.output),
                model.kind,
                if model.approximate { " ~" } else { "" }
            );
        }
    }
    if total > shown {
        println!("… {} more (--limit)", total - shown);
    }
    Ok(())
}

fn cmd_stats() -> Result<(), String> {
    let db = models::global().ok_or("models are off (CODEGRAPH_MODELS=0)")?;
    println!("models: {}", db.origin);
    for (language, count) in db.counts() {
        println!("{:<12} {count:>7}", language.as_str());
        let Some(models) = db.language(language) else {
            continue;
        };
        let mut by: std::collections::BTreeMap<(String, String), usize> = Default::default();
        for model in &models.models {
            let kind = match model.role {
                Role::Summary | Role::Neutral => String::new(),
                _ => model.kind.clone(),
            };
            *by.entry((model.role.as_str().to_string(), kind))
                .or_default() += 1;
        }
        for ((role, kind), n) in by {
            println!("  {role:<8} {kind:<32} {n:>7}");
        }
    }
    Ok(())
}

fn cmd_match(project: Option<&str>, json: bool) -> Result<(), String> {
    let root = resolve_absolute(project);
    let report = codegraph::analyze::rules::model_match_report(Path::new(&root))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.text());
    }
    Ok(())
}
