use std::path::PathBuf;

pub(super) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let (scheme, remainder) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let location = remainder.strip_prefix("//")?;
    let encoded = if location.starts_with('/') {
        location
    } else {
        let separator = location.find('/')?;
        if !location[..separator].eq_ignore_ascii_case("localhost") {
            return None;
        }
        &location[separator..]
    };
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    #[cfg(windows)]
    let path = path
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&path)
        .to_string();
    Some(PathBuf::from(path))
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_spaces_and_utf8_without_treating_plus_as_space() {
        assert_eq!(
            file_uri_to_path("file:///tmp/a%20b-%CE%BB+c"),
            Some(PathBuf::from("/tmp/a b-λ+c"))
        );
        assert_eq!(
            file_uri_to_path("FILE://LOCALHOST/tmp/a%20b"),
            Some(PathBuf::from("/tmp/a b"))
        );
    }

    #[test]
    fn rejects_non_file_authorities_and_malformed_escapes() {
        assert_eq!(file_uri_to_path("file://server/share"), None);
        assert_eq!(file_uri_to_path("file:///tmp/%zz"), None);
    }
}
