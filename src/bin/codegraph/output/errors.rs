use super::{ExtractionError, Path, Severity, iso_from_epoch_ms, now_ms};

pub(crate) fn write_error_log(project_path: &Path, errors: &[ExtractionError]) {
    let cg_dir = project_path.join(".codegraph");
    if !cg_dir.exists() {
        return;
    }

    let log_path = cg_dir.join("errors.log");

    // Group errors by file path (insertion order, TS `Map`).
    let mut errors_by_file: Vec<(String, Vec<String>)> = Vec::new();
    let mut no_file_errors: Vec<String> = Vec::new();

    for err in errors {
        if err.severity != Severity::Error {
            continue;
        }
        match &err.file_path {
            Some(fp) => match errors_by_file.iter_mut().find(|(f, _)| f == fp) {
                Some((_, list)) => list.push(err.message.clone()),
                None => errors_by_file.push((fp.clone(), vec![err.message.clone()])),
            },
            None => no_file_errors.push(err.message.clone()),
        }
    }

    let mut lines: Vec<String> = vec![
        format!("CodeGraph Error Log - {}", iso_from_epoch_ms(now_ms())),
        format!("{} files with errors", errors_by_file.len()),
        String::new(),
    ];

    for (file_path, file_errors) in &errors_by_file {
        for message in file_errors {
            lines.push(format!("{file_path}: {message}"));
        }
    }

    for message in &no_file_errors {
        lines.push(message.clone());
    }

    let _ = std::fs::write(&log_path, lines.join("\n") + "\n");
}
