//! Shared filesystem-aware source-file selection.
//!
//! Directory scanning and live watching must agree about which existing files
//! are indexable. Extension and project-override checks stay in `grammars`;
//! this module adds bounded shebang inspection and realpath-safe file access.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use super::grammars::{detect_shebang_language, is_source_file_with_overrides};
use crate::types::Language;
use crate::utils::resolve_existing_path_within_root_real;

const SHEBANG_PREFIX_BYTES: usize = 512;

/// Whether an existing project-relative path is a regular, safely readable
/// source file according to extension overrides or a supported shebang.
pub(crate) fn is_indexable_existing_file(
    project_root: &Path,
    relative_path: &str,
    overrides: &HashMap<String, Language>,
) -> bool {
    let Some(real_path) = resolve_existing_path_within_root_real(project_root, relative_path)
    else {
        return false;
    };
    if !fs::metadata(&real_path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return false;
    }

    is_source_file_with_overrides(relative_path, overrides)
        || file_has_supported_shebang(&real_path)
}

/// Inspect only a bounded prefix. `BufRead::read_line` is intentionally not
/// used because a newline-free binary or generated file could otherwise force
/// an unbounded allocation during directory discovery.
fn file_has_supported_shebang(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut prefix = [0u8; SHEBANG_PREFIX_BYTES];
    let Ok(read) = file.read(&mut prefix) else {
        return false;
    };
    let first_line_end = prefix[..read]
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(read);
    let first_line = String::from_utf8_lossy(&prefix[..first_line_end]);
    detect_shebang_language(&first_line).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_shebang_probe_accepts_supported_interpreters() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("script");
        fs::write(&script, "#!/usr/bin/env python3\nprint('ok')\n").unwrap();
        assert!(file_has_supported_shebang(&script));
    }

    #[test]
    fn bounded_shebang_probe_rejects_a_long_non_shebang_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blob");
        fs::write(&file, vec![b'x'; SHEBANG_PREFIX_BYTES * 4]).unwrap();
        assert!(!file_has_supported_shebang(&file));
    }
}
