//! rustdoc's JSON — the compiler's own description of a crate's public API
//! — as the source of truth for a dependency's paths, re-exports and impls.
//!
//! * [`raw`] reads the part of a rustdoc JSON file the index needs; the
//!   file's `format_version` must be one of [`SUPPORTED_FORMAT_VERSIONS`],
//!   anything else is refused (fail closed: the shard keeps working from
//!   tree-sitter alone, as before).
//! * [`from_raw`] turns it into an [`ApiIndex`]: every public path (the
//!   module tree and its `pub use` re-exports, globs included) and every
//!   item's path of definition, each type's impls (inherent, trait, the
//!   blanket impls rustdoc found to apply), blanket impls as written, and
//!   traits' items — each item with where it is written.
//! * [`store`] keeps the indexes as versioned artifacts inside the shard
//!   directory (`api/<crate>.json`), written with the shard, read-only
//!   afterwards; [`load`] shares them per process.
//! * [`reconcile`] fixes the shard's nodes where the grammar lost an
//!   item rustdoc knows (a mangled `impl` header hoists its methods to bare
//!   functions; a nightly-only syntax loses a whole item).
//! * [`source`] finds the JSON: the toolchain's `rust-docs-json` component
//!   (for the toolchain shard, matched by release and commit) or
//!   `cargo rustdoc` run for a crate (opt-in, bounded, offline).
//!
//! Resolution reads the indexes in `resolution/external/rust/api.rs`.

pub mod attach;
mod from_raw;
pub(crate) use from_raw::owner_name;
mod model;
pub(crate) mod raw;
pub mod reconcile;
pub mod source;
pub mod store;

use std::path::Path;

pub use model::{ApiImpl, ApiIndex, ApiItem, ApiKind, ApiType, ExternalPath, PathStep, Target};
pub use store::{API_DIR, ApiMeta, load};

/// The rustdoc JSON `format_version`s this reader understands.
pub const SUPPORTED_FORMAT_VERSIONS: &[u32] = &[61];

/// The API index artifact's format: bump whenever what an index records (or
/// how it is built) changes — every shard holding an older one is rebuilt.
pub const API_FORMAT: u32 = 2;

/// Why a rustdoc JSON file was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Io(String),
    /// A `format_version` this build does not read.
    UnsupportedFormat(u32),
    /// No `format_version` at the end of the file.
    NotRustdocJson,
    Parse(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "read: {error}"),
            Self::UnsupportedFormat(version) => write!(
                f,
                "rustdoc JSON format_version {version} is not supported (supported: {SUPPORTED_FORMAT_VERSIONS:?})"
            ),
            Self::NotRustdocJson => f.write_str("not rustdoc JSON (no format_version)"),
            Self::Parse(error) => write!(f, "parse: {error}"),
        }
    }
}

/// The `format_version` of a rustdoc JSON file, read from its end (rustdoc
/// writes it last) without parsing the rest.
pub fn format_version(path: &Path) -> Result<u32, ReadError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| ReadError::Io(e.to_string()))?;
    let len = file
        .metadata()
        .map_err(|e| ReadError::Io(e.to_string()))?
        .len();
    let tail = len.min(512);
    file.seek(SeekFrom::Start(len - tail))
        .map_err(|e| ReadError::Io(e.to_string()))?;
    let mut text = String::new();
    file.take(tail)
        .read_to_string(&mut text)
        .map_err(|e| ReadError::Io(e.to_string()))?;
    format_version_in(&text).ok_or(ReadError::NotRustdocJson)
}

fn format_version_in(text: &str) -> Option<u32> {
    let at = text.rfind("\"format_version\"")?;
    let rest = text[at + "\"format_version\"".len()..].trim_start();
    let digits: String = rest
        .strip_prefix(':')?
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Read a rustdoc JSON file — refused unless its format is supported.
pub(crate) fn read_crate(path: &Path) -> Result<raw::RawCrate, ReadError> {
    let version = format_version(path)?;
    if !SUPPORTED_FORMAT_VERSIONS.contains(&version) {
        return Err(ReadError::UnsupportedFormat(version));
    }
    let bytes = std::fs::read(path).map_err(|e| ReadError::Io(e.to_string()))?;
    parse_crate(&bytes)
}

/// Parse rustdoc JSON text — refused unless its format is supported.
pub(crate) fn parse_crate(bytes: &[u8]) -> Result<raw::RawCrate, ReadError> {
    let tail = &bytes[bytes.len().saturating_sub(512)..];
    let version =
        format_version_in(&String::from_utf8_lossy(tail)).ok_or(ReadError::NotRustdocJson)?;
    if !SUPPORTED_FORMAT_VERSIONS.contains(&version) {
        return Err(ReadError::UnsupportedFormat(version));
    }
    let krate: raw::RawCrate =
        serde_json::from_slice(bytes).map_err(|e| ReadError::Parse(e.to_string()))?;
    if !SUPPORTED_FORMAT_VERSIONS.contains(&krate.format_version) {
        return Err(ReadError::UnsupportedFormat(krate.format_version));
    }
    Ok(krate)
}

#[cfg(test)]
mod tests {
    use super::format_version_in;

    /// `CODEGRAPH_RUSTDOC_JSON_DIR=<dir> cargo test --lib real_toolchain_json -- --ignored --nocapture`:
    /// index the installed toolchain JSON and print what came out.
    #[test]
    #[ignore]
    fn real_toolchain_json() {
        let dir = std::path::PathBuf::from(std::env::var("CODEGRAPH_RUSTDOC_JSON_DIR").unwrap());
        for krate in ["std", "core", "alloc"] {
            let started = std::time::Instant::now();
            let raw = super::read_crate(&dir.join(format!("{krate}.json"))).unwrap();
            let parsed = started.elapsed();
            let index = super::from_raw::build_index(&raw, krate, &|file| {
                super::source::toolchain_span_file(file)
            });
            let text = serde_json::to_vec(&index).unwrap();
            println!(
                "{krate}: parse {parsed:?} build {:?}; {} items {} paths {} defined {} globs {} impls {} types {} foreign {} blankets {} traits; {} KB",
                started.elapsed() - parsed,
                index.items.len(),
                index.paths.len(),
                index.defined.len(),
                index.globs.len(),
                index.impls.len(),
                index.types.len(),
                index.foreign_impls.len(),
                index.blankets.len(),
                index.traits.len(),
                text.len() / 1024
            );
            for probe in [
                "task::Poll",
                "cell::RefCell",
                "collections::HashMap",
                "vec::Vec",
                "io::Read",
                "option::Option",
                "string::String",
                "string::ToString",
                "prelude::rust_2021::Vec",
            ] {
                let segments: Vec<String> = probe.split("::").map(str::to_string).collect();
                println!("   {probe} → {:?}", index.resolve(&segments));
            }
            println!(
                "   globs: {:?}",
                index.globs.iter().take(8).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn format_version_is_read_from_the_tail() {
        assert_eq!(
            format_version_in(r#"…"target":{},"format_version":61}"#),
            Some(61)
        );
        assert_eq!(format_version_in(r#""format_version" : 7 }"#), Some(7));
        assert_eq!(format_version_in("{}"), None);
    }
}
