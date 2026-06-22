mod fuzzy;
mod method;
mod probe;
mod score;

use std::collections::HashSet;

fn cpu_flags(known: &HashSet<&str>, name: &str) -> u8 {
    if known.contains(name) {
        return 1;
    }
    if let Some(d) = name.find('.') {
        if d > 0 {
            let receiver = &name[..d];
            if known.contains(receiver) || known.contains(&name[d + 1..]) {
                return 1;
            }
            let mut cap = receiver.to_string();
            if let Some(f) = cap.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            if receiver.starts_with(|c: char| c.is_ascii_lowercase())
                && known.contains(cap.as_str())
            {
                return 1;
            }
            let ld = name.rfind('.').unwrap_or(0);
            if ld > d && !name[ld + 1..].is_empty() && known.contains(&name[ld + 1..]) {
                return 1;
            }
            if !receiver.is_ascii() && receiver.starts_with(|c: char| !c.is_ascii()) {
                return 0x80;
            }
        }
    }
    if let Some(c) = name.find("::") {
        if c > 0 && (known.contains(&name[..c]) || known.contains(&name[c + 2..])) {
            return 1;
        }
    }
    if let Some(sl) = name.rfind('/') {
        if sl > 0 && !name[sl + 1..].is_empty() && known.contains(&name[sl + 1..]) {
            return 1;
        }
    }
    0
}
