//! Timestamps as the history store keeps them: integer milliseconds since
//! the Unix epoch (UTC). Agent stores write RFC 3339 text (Claude Code, JFC)
//! or integer milliseconds (opencode); both land here.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the epoch of an RFC 3339 timestamp
/// (`2026-09-17T12:34:56.789Z`, `…+02:00`, fractional seconds optional).
/// `None` for anything else.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        std::str::from_utf8(b.get(from..to)?).ok()?.parse().ok()
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if b[13] != b':' || b[16] != b':' || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut i = 19;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 3 {
                millis = millis * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        for _ in (i - start)..3 {
            millis *= 10;
        }
    }
    let offset_min = match b.get(i) {
        None | Some(b'Z' | b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let hh = num(i + 1, i + 3)?;
            let mm = if b.get(i + 3) == Some(&b':') {
                num(i + 4, i + 6)?
            } else {
                num(i + 3, i + 5).unwrap_or(0)
            };
            let total = hh * 60 + mm;
            if sign == b'+' { total } else { -total }
        }
        Some(_) => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_min * 60;
    Some(secs * 1_000 + millis)
}

/// RFC 3339 UTC text (millisecond precision) for epoch milliseconds.
pub(crate) fn format_rfc3339_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1_000);
    let millis = ms.rem_euclid(1_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Current wall-clock time in epoch milliseconds.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Compact age of `ms` relative to `now` (`45s`, `12m`, `3h`, `2d`, `5w`).
pub(crate) fn ago(ms: i64, now: i64) -> String {
    let secs = (now - ms).max(0) / 1_000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 14 * 86_400 => format!("{}d", s / 86_400),
        s => format!("{}w", s / (7 * 86_400)),
    }
}

/// A duration like `30m`, `12h`, `7d`, `2w` (a bare number is days) in ms.
pub(crate) fn parse_duration_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: i64 = s[..split].parse().ok()?;
    let unit = match &s[split..] {
        "" | "d" | "day" | "days" => 86_400_000,
        "s" => 1_000,
        "m" | "min" => 60_000,
        "h" | "hour" | "hours" => 3_600_000,
        "w" | "week" | "weeks" => 7 * 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_and_honours_offsets() {
        let ms = parse_rfc3339_ms("2026-09-17T12:34:56.789Z").unwrap();
        assert_eq!(format_rfc3339_ms(ms), "2026-09-17T12:34:56.789Z");
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-09-17T14:34:56.789+02:00"),
            Some(ms),
            "offset folds to UTC"
        );
        // Microseconds (JFC) keep millisecond precision.
        assert_eq!(
            parse_rfc3339_ms("2026-06-01T10:00:00.000009Z"),
            parse_rfc3339_ms("2026-06-01T10:00:00Z")
        );
        assert_eq!(parse_rfc3339_ms("2026-06-01"), None);
        assert_eq!(parse_rfc3339_ms("not a time at all!!"), None);
    }

    #[test]
    fn durations_and_ages() {
        assert_eq!(parse_duration_ms("7d"), Some(7 * 86_400_000));
        assert_eq!(parse_duration_ms("30"), Some(30 * 86_400_000));
        assert_eq!(parse_duration_ms("12h"), Some(12 * 3_600_000));
        assert_eq!(parse_duration_ms("2x"), None);
        assert_eq!(ago(0, 90_000), "1m");
        assert_eq!(ago(0, 3 * 86_400_000), "3d");
        assert_eq!(ago(0, 30 * 86_400_000), "4w");
    }
}
