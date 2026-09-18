//! A file's lines, for inference run once per reference.
//!
//! References are resolved file by file, so the same source is split into
//! lines again and again; each worker thread keeps the line boundaries of
//! the last file it split (keyed by the shared source it was handed, not
//! by path, so an edited file is never read with stale boundaries).

use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    /// The last source split on this thread and where its lines start.
    static LAST: RefCell<Option<(Arc<str>, Arc<[usize]>)>> = const { RefCell::new(None) };
}

/// The lines of `source` (no `\n`, no trailing `\r`).
pub(super) fn lines(source: &Arc<str>) -> Vec<&str> {
    let starts = starts(source);
    let text: &str = source;
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).map_or(text.len(), |next| next - 1);
            let line = &text[start..end];
            line.strip_suffix('\r').unwrap_or(line)
        })
        .collect()
}

fn starts(source: &Arc<str>) -> Arc<[usize]> {
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if let Some((seen, starts)) = last.as_ref() {
            if Arc::ptr_eq(seen, source) {
                return Arc::clone(starts);
            }
        }
        let starts: Arc<[usize]> = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        *last = Some((Arc::clone(source), Arc::clone(&starts)));
        starts
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::lines;

    #[test]
    fn splits_like_str_split_and_reuses_the_last_file() {
        let source: Arc<str> = Arc::from("fn a() {\r\n    b();\n}\n");
        let expected: Vec<&str> = source
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        assert_eq!(lines(&source), expected);
        assert_eq!(lines(&source), expected);
        let other: Arc<str> = Arc::from("x");
        assert_eq!(lines(&other), ["x"]);
    }
}
