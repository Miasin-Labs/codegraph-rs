//! IDA dump manifest ingestion (`all_funcs.txt` / `failed_addrs.txt`).
//!
//! IDA dump scripts emit two side files next to the per-function `.c` tree:
//!
//! - `all_funcs.txt` — one `0xADDR <name>` row per function in the binary,
//!   INCLUDING bodiless imports and functions Hex-Rays failed to decompile (so
//!   it is the authoritative addr→name symbol table; C++ names are mangled).
//! - `failed_addrs.txt` — one `0xADDR` per function that produced no `.c` file.
//!
//! Most cross-file references already resolve by name (the filename *is* the
//! addr→name map), so this module's job is the gap the per-file tree can't
//! cover: minting address-bearing stub Function nodes for symbols that are
//! *referenced but never defined* (failed decompilations + import-only
//! targets), so their call edges resolve to a real node instead of dangling.

use crate::extraction::ida_c_extractor::demangle_name;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{Language, Node, NodeKind};

/// Parsed `all_funcs.txt`: an ordered address→name table.
#[derive(Debug, Default, Clone)]
pub struct FuncManifest {
    /// (address, raw name) rows in file order.
    entries: Vec<(u64, String)>,
}

impl FuncManifest {
    /// Parse `all_funcs.txt` content. Each non-blank line is
    /// `0x<hex> <name>`; malformed lines are skipped.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let Some(addr_tok) = parts.next() else {
                continue;
            };
            let Some(name) = parts.next().map(str::trim).filter(|n| !n.is_empty()) else {
                continue;
            };
            let hex = addr_tok.strip_prefix("0x").unwrap_or(addr_tok);
            if let Ok(addr) = u64::from_str_radix(hex, 16) {
                entries.push((addr, name.to_string()));
            }
        }
        FuncManifest { entries }
    }

    /// The raw (possibly mangled) name recorded for `addr`.
    pub fn name_for(&self, addr: u64) -> Option<&str> {
        self.entries
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, n)| n.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All `(address, name)` rows.
    pub fn entries(&self) -> &[(u64, String)] {
        &self.entries
    }
}

/// Parse `failed_addrs.txt`: one `0x<hex>` virtual address per line.
pub fn parse_failed_addrs(text: &str) -> Vec<u64> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            let hex = t.strip_prefix("0x").unwrap_or(t);
            u64::from_str_radix(hex, 16).ok()
        })
        .collect()
}

/// Mint stub Function nodes for `addrs` (e.g. `failed_addrs.txt` entries, or
/// any referenced-but-fileless target), naming them from the manifest and
/// demangling C++ names. The node id is the same address-named form the
/// per-file extractor would produce, so a real `.c` file for the address — if
/// it ever appears — collapses onto the same row. `synthetic` file path marks
/// these as not backed by a source file.
pub fn synthesize_stub_nodes(
    manifest: &FuncManifest,
    addrs: &[u64],
    language: Language,
    synthetic_path: &str,
) -> Vec<Node> {
    let mut out = Vec::with_capacity(addrs.len());
    for &addr in addrs {
        let raw = manifest
            .name_for(addr)
            .map(str::to_string)
            .unwrap_or_else(|| format!("sub_{addr:X}"));
        let demangled = demangle_name(&raw);
        let kind = match &demangled {
            Some(d) if d.contains("::") => NodeKind::Method,
            _ => NodeKind::Function,
        };
        let qualified = demangled.unwrap_or_else(|| raw.clone());
        let name = qualified
            .rsplit("::")
            .next()
            .unwrap_or(&qualified)
            .to_string();
        let id = generate_node_id(synthetic_path, kind, &qualified, 1);
        let mut node = Node::new(id, kind, name, qualified, synthetic_path, language, 1, 1);
        node.address = Some(addr);
        node.signature = Some("/* stub: no decompiled body */".to_string());
        out.push(node);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "0x111000 .init_proc\n0x111020 sub_111020\n0x111030 ._ZN2QT13QStandardItemC1ERKNS_5QIconERKNS_7QStringE\n0x1A27F0 sub_1A27F0\n\n   \nxyzzy not a hex address\n";

    #[test]
    fn parses_addr_name_rows_and_skips_garbage() {
        let m = FuncManifest::parse(MANIFEST);
        // 4 valid rows; the blank and the leading-word line are skipped.
        assert_eq!(m.len(), 4);
        assert_eq!(m.name_for(0x111020), Some("sub_111020"));
        assert_eq!(m.name_for(0x111000), Some(".init_proc"));
        assert_eq!(m.name_for(0xDEAD), None);
    }

    #[test]
    fn parses_failed_addrs() {
        let addrs = parse_failed_addrs("0x1A27F0\n0x1BC780\n\ngarbage\n0x258FC0\n");
        assert_eq!(addrs, vec![0x1A27F0, 0x1BC780, 0x258FC0]);
    }

    #[test]
    fn synthesizes_demangled_address_bearing_stubs() {
        let m = FuncManifest::parse(MANIFEST);
        let stubs = synthesize_stub_nodes(
            &m,
            &[0x111030, 0x1A27F0, 0xCAFE],
            Language::C,
            "<ida-manifest>",
        );
        assert_eq!(stubs.len(), 3);

        // C++ mangled name → demangled Method stub with its address.
        let ctor = stubs.iter().find(|n| n.address == Some(0x111030)).unwrap();
        assert_eq!(ctor.kind, NodeKind::Method);
        assert_eq!(ctor.qualified_name, "QT::QStandardItem::QStandardItem");
        assert_eq!(ctor.name, "QStandardItem");

        // Plain sub_ name from the manifest.
        let sub = stubs.iter().find(|n| n.address == Some(0x1A27F0)).unwrap();
        assert_eq!(sub.kind, NodeKind::Function);
        assert_eq!(sub.name, "sub_1A27F0");

        // Address absent from the manifest falls back to a synthesized name.
        let unknown = stubs.iter().find(|n| n.address == Some(0xCAFE)).unwrap();
        assert_eq!(unknown.name, "sub_CAFE");
    }
}
