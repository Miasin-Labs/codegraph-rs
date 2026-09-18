//! `go.mod`: `replace` directives whose target is a local directory
//! (`=> ./x`, `=> ../x`, `=> /abs/x`), single-line or in a `replace ( … )`
//! block. Module-path targets (`=> example.com/fork v1.2.3`) belong to
//! `deps/`.

use super::{Parsed, RawLink};
use crate::atlas::kinds::LinkKind;

pub(super) fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut in_block = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or_default().trim();
        let line_no = u32::try_from(index + 1).ok();
        if let Some(module) = line.strip_prefix("module ") {
            let module = module.trim().trim_matches('"');
            parsed.package = module.rsplit('/').next().map(str::to_owned);
            continue;
        }
        let directive = if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            line
        } else if let Some(rest) = line.strip_prefix("replace") {
            let rest = rest.trim();
            if rest.starts_with('(') {
                in_block = true;
                continue;
            }
            rest
        } else {
            continue;
        };
        let Some((from, to)) = directive.split_once("=>") else {
            continue;
        };
        let target = to
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches('"');
        if target.starts_with("./") || target.starts_with("../") || target.starts_with('/') {
            let module = from.split_whitespace().next().unwrap_or_default();
            parsed.links.push(RawLink {
                kind: LinkKind::GoReplace,
                path: target.to_owned(),
                line: line_no,
                detail: Some(module.trim_matches('"').to_owned()),
            });
        }
    }
    parsed
}
