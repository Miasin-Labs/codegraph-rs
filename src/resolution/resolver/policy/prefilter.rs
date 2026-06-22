use std::collections::HashSet;

/// JS `charAt(0).toUpperCase() + slice(1)` (scalar-based; differs from
/// UTF-16 slicing only for astral-plane first chars).
pub(in crate::resolution::resolver) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(in crate::resolution::resolver) fn has_any_possible_match_in(
    known: &HashSet<String>,
    name: &str,
) -> bool {
    if known.contains(name) {
        return true;
    }

    if let Some(dot_idx) = name.find('.') {
        if dot_idx > 0 {
            let receiver = &name[..dot_idx];
            let member = &name[dot_idx + 1..];
            if known.contains(receiver) || known.contains(member) {
                return true;
            }
            let capitalized = capitalize_first(receiver);
            if known.contains(&capitalized) {
                return true;
            }
            let last_dot = name.rfind('.').unwrap_or(0);
            if last_dot > dot_idx {
                let tail = &name[last_dot + 1..];
                if !tail.is_empty() && known.contains(tail) {
                    return true;
                }
            }
        }
    }
    if let Some(colon_idx) = name.find("::") {
        if colon_idx > 0 {
            let receiver = &name[..colon_idx];
            let member = &name[colon_idx + 2..];
            if known.contains(receiver) || known.contains(member) {
                return true;
            }
        }
    }

    if let Some(slash_idx) = name.rfind('/') {
        if slash_idx > 0 {
            let file_name = &name[slash_idx + 1..];
            if known.contains(file_name) {
                return true;
            }
        }
    }

    false
}
