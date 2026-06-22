use super::*;

pub(crate) fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// JS `Number.prototype.toFixed` approximation (round half away from zero).
pub(crate) fn js_to_fixed(value: f64, digits: u32) -> String {
    let factor = 10f64.powi(digits as i32);
    let rounded = (value * factor).round() / factor;
    format!("{:.*}", digits as usize, rounded)
}

/// Format duration in milliseconds to human readable.
pub(crate) fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let seconds = ms as f64 / 1000.0;
    if seconds < 60.0 {
        return format!("{}s", js_to_fixed(seconds, 1));
    }
    let minutes = (seconds / 60.0).floor() as i64;
    let remaining_seconds = seconds % 60.0;
    format!("{minutes}m {}s", js_to_fixed(remaining_seconds, 0))
}

/// `parseInt(s, 10)` parity: optional sign + leading digit run; None == NaN.
pub(crate) fn parse_int_js(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (negative, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()
        .map(|v| if negative { -v } else { v })
}

/// Epoch milliseconds (`Date.now()` parity).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Howard Hinnant's `civil_from_days` — days since 1970-01-01 → (y, m, d).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `new Date(ms).toISOString()` parity: `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC).
pub(crate) fn iso_from_epoch_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}
