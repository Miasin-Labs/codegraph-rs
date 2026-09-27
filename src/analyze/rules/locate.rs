//! Lines in a rule file, for errors found after the YAML is read (a regex
//! that does not compile, a pattern that does not parse, a capture no
//! pattern defines). `serde_yaml_ng` keeps no spans, so these are found in
//! the text by the block structure rules are written in: the rule's `id:`
//! line, its `check-patterns:` items, their `where:` items and keys. Flow
//! style (`[…]`, `{…}`) falls back to the nearest enclosing line.

pub(super) struct Locator<'a> {
    lines: Vec<&'a str>,
}

/// Where a value sits: the 1-based line of its key and whether the value
/// is a block scalar (`|`/`>`) whose text starts on the next line.
#[derive(Debug, Clone, Copy)]
pub(super) struct KeyAt {
    pub line: usize,
    pub block: bool,
}

impl KeyAt {
    /// The file line of line `n` (1-based) of the value's text.
    pub fn value_line(self, n: usize) -> usize {
        if self.block {
            self.line + n
        } else {
            self.line + n.saturating_sub(1)
        }
    }
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// `key` at the start of `line`'s content (after an optional `- `).
fn key_of(line: &str) -> Option<(&str, &str)> {
    let content = line.trim_start();
    let content = content.strip_prefix("- ").unwrap_or(content).trim_start();
    let (key, rest) = content.split_once(':')?;
    let key = key.trim().trim_matches(|c| c == '"' || c == '\'');
    ((!key.is_empty() && !key.contains(' ')) || key.contains("pattern")).then_some((key, rest))
}

impl<'a> Locator<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines().collect(),
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 1-based lines of every `id:` key, in order.
    pub fn rule_lines(&self, id: &str) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                key_of(line).is_some_and(|(key, value)| {
                    key == "id" && value.trim().trim_matches(|c| c == '"' || c == '\'') == id
                })
            })
            .map(|(i, _)| i + 1)
            .collect()
    }

    /// The line ranges (1-based, start inclusive, end exclusive) of the
    /// rules whose `id:` keys sit at `id_lines` (ascending): from the start
    /// of each rule's mapping (keys may come before `id:`) to the next one.
    pub fn rule_ranges(&self, id_lines: &[usize]) -> Vec<(usize, usize)> {
        let mut starts = Vec::with_capacity(id_lines.len());
        let mut floor = 1;
        for &id_line in id_lines {
            let line = self.lines[id_line - 1];
            let own = indent(line);
            let mut start = id_line;
            if !line.trim_start().starts_with("- ") {
                while start > floor {
                    let prev = self.lines[start - 2];
                    if prev.trim_start().starts_with("---") {
                        break;
                    }
                    if is_blank(prev) {
                        start -= 1;
                        continue;
                    }
                    let depth = indent(prev);
                    if prev.trim_start().starts_with("- ") && depth + 2 == own {
                        start -= 1;
                        break;
                    }
                    if depth < own {
                        break;
                    }
                    start -= 1;
                }
            }
            starts.push(start);
            floor = id_line + 1;
        }
        let mut ranges = Vec::with_capacity(starts.len());
        for (k, &start) in starts.iter().enumerate() {
            let end = starts.get(k + 1).copied().unwrap_or(self.lines.len() + 1);
            ranges.push((start, end.max(start + 1)));
        }
        ranges
    }

    /// The first key named one of `keys` within `from..to` at the shallowest
    /// indentation found.
    pub fn key(&self, from: usize, to: usize, keys: &[&str]) -> Option<KeyAt> {
        let mut best: Option<(usize, KeyAt)> = None;
        for n in from..to.min(self.lines.len() + 1) {
            let line = self.lines[n - 1];
            let Some((key, rest)) = key_of(line) else {
                continue;
            };
            if keys.contains(&key) {
                let depth = indent(line);
                if best.is_none_or(|(d, _)| depth < d) {
                    let value = rest.trim_start();
                    best = Some((
                        depth,
                        KeyAt {
                            line: n,
                            block: value.starts_with('|') || value.starts_with('>'),
                        },
                    ));
                }
            }
        }
        best.map(|(_, at)| at)
    }

    /// Lines of the items of the block sequence under the key at `key_line`,
    /// each with the end of its range. Empty for a flow sequence.
    pub fn items(&self, key_line: usize, to: usize) -> Vec<(usize, usize)> {
        let key_indent = indent(self.lines[key_line - 1]);
        let mut item_indent = None;
        let mut items: Vec<usize> = Vec::new();
        let mut end = to.min(self.lines.len() + 1);
        for n in key_line + 1..end {
            let line = self.lines[n - 1];
            if is_blank(line) {
                continue;
            }
            let depth = indent(line);
            let dash = line.trim_start().starts_with("- ") || line.trim() == "-";
            match item_indent {
                None if dash && depth >= key_indent => {
                    item_indent = Some(depth);
                    items.push(n);
                }
                None => {
                    end = n;
                    break;
                }
                Some(i) if dash && depth == i => items.push(n),
                Some(i) if depth > i => {}
                Some(_) => {
                    end = n;
                    break;
                }
            }
        }
        let mut ranges = Vec::with_capacity(items.len());
        for (k, &line) in items.iter().enumerate() {
            let next = items.get(k + 1).copied().unwrap_or(end);
            ranges.push((line, next));
        }
        ranges
    }
}
