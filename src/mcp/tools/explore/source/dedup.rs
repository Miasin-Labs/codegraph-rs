use super::super::super::format::number_source_lines;
use super::super::types::{ExploreBackReference, RenderedFile, StructuredSourceFile};
use crate::mcp::explore_session::{LineRange, ProjectState, file_fingerprint, served_ranges};

const MIN_COVERED_LINES: usize = 8;

pub(super) struct DedupRequest<'a> {
    pub root: &'a std::path::Path,
    pub path: &'a str,
    pub prior: Option<&'a ProjectState>,
    pub line_numbers: bool,
}

pub(super) struct DedupResult {
    pub rendered: Option<RenderedFile>,
    pub covered: Vec<LineRange>,
    pub symbols: Vec<String>,
}

pub(super) fn apply(mut rendered: RenderedFile, req: DedupRequest<'_>) -> DedupResult {
    let Some(prior) = req.prior else {
        return DedupResult {
            rendered: Some(rendered),
            covered: Vec::new(),
            symbols: Vec::new(),
        };
    };
    let Some(fingerprint) = file_fingerprint(req.root, req.path) else {
        return DedupResult {
            rendered: Some(rendered),
            covered: Vec::new(),
            symbols: Vec::new(),
        };
    };
    let served = served_ranges(prior, req.path, &fingerprint);
    let mut covered = Vec::new();
    let mut symbols = Vec::new();
    rendered.chunks.retain(|chunk| {
        let held = chunk.end_line - chunk.start_line + 1 >= MIN_COVERED_LINES
            && served
                .iter()
                .any(|range| range.start <= chunk.start_line && range.end >= chunk.end_line);
        if held {
            covered.push(LineRange {
                start: chunk.start_line,
                end: chunk.end_line,
            });
            symbols.extend(chunk.symbols.iter().cloned());
        }
        !held
    });
    if covered.is_empty() {
        return DedupResult {
            rendered: Some(rendered),
            covered,
            symbols,
        };
    }
    if rendered.chunks.is_empty() {
        return DedupResult {
            rendered: None,
            covered,
            symbols,
        };
    }
    rendered.body = rendered
        .chunks
        .iter()
        .map(|chunk| {
            if req.line_numbers {
                number_source_lines(&chunk.source, chunk.start_line)
            } else {
                chunk.source.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n...\n");
    rendered.cost = rendered.body.len() + 200;
    DedupResult {
        rendered: Some(rendered),
        covered,
        symbols,
    }
}

pub(super) struct Context<'a> {
    pub root: &'a std::path::Path,
    pub prior: Option<&'a ProjectState>,
    pub line_numbers: bool,
}

pub(super) struct Appender<'a> {
    lines: &'a mut Vec<String>,
    context: Context<'a>,
    rendered_files: Vec<StructuredSourceFile>,
    back_references: Vec<ExploreBackReference>,
    files_included: usize,
    total_chars: usize,
    fallback: Option<(String, RenderedFile)>,
}

impl<'a> Appender<'a> {
    pub fn new(lines: &'a mut Vec<String>, context: Context<'a>, initial_chars: usize) -> Self {
        Self {
            lines,
            context,
            rendered_files: Vec::new(),
            back_references: Vec::new(),
            files_included: 0,
            total_chars: initial_chars,
            fallback: None,
        }
    }

    pub fn files_included(&self) -> usize {
        self.files_included
    }

    pub fn total_chars(&self) -> usize {
        self.total_chars
    }

    pub fn add_back_reference(&mut self, reference: ExploreBackReference) {
        self.lines.push(format!(
            "> **Already sent earlier in this conversation:** `{}` — unchanged on disk; source is not repeated.",
            reference.path
        ));
        self.lines.push(String::new());
        self.back_references.push(reference);
    }

    pub fn append_notice(&mut self, notice: String) {
        self.total_chars += notice.len() + 2;
        self.lines.push(notice);
        self.lines.push(String::new());
    }

    pub fn append(&mut self, file_path: &str, rendered: RenderedFile) -> usize {
        let original = rendered.clone();
        let result = apply(
            rendered,
            DedupRequest {
                root: self.context.root,
                path: file_path,
                prior: self.context.prior,
                line_numbers: self.context.line_numbers,
            },
        );
        if !result.covered.is_empty() {
            self.back_references.push(ExploreBackReference {
                path: file_path.to_string(),
                ranges: result.covered,
                symbols: result.symbols,
            });
        }
        let Some(rendered) = result.rendered else {
            self.lines.push(format!(
            "> **Already sent earlier in this conversation:** `{}` — unchanged on disk; source is not repeated.",
            file_path
        ));
            self.lines.push(String::new());
            if self.fallback.is_none() {
                self.fallback = Some((file_path.to_string(), original));
            }
            return 0;
        };
        let (source_file, cost) = append_rendered(self.lines, file_path, rendered);
        self.rendered_files.push(source_file);
        self.total_chars += cost;
        self.files_included += 1;
        cost
    }

    pub fn finish(mut self) -> (usize, Vec<StructuredSourceFile>, Vec<ExploreBackReference>) {
        if self.rendered_files.is_empty() {
            if let Some((path, rendered)) = self.fallback.take() {
                let (source_file, _) = append_rendered(self.lines, &path, rendered);
                self.rendered_files.push(source_file);
                self.files_included = 1;
                self.back_references
                    .retain(|reference| reference.path != path);
            }
        }
        (
            self.files_included,
            self.rendered_files,
            self.back_references,
        )
    }
}

#[cfg(test)]
mod tests;

pub(super) fn append_rendered(
    lines: &mut Vec<String>,
    file_path: &str,
    rendered: RenderedFile,
) -> (StructuredSourceFile, usize) {
    let cost = rendered.cost;
    lines.push(rendered.header.clone());
    lines.push(String::new());
    lines.push(format!("```{}", rendered.language));
    lines.push(rendered.body.clone());
    lines.push("```".to_string());
    lines.push(String::new());
    (rendered.into_structured(file_path), cost)
}
