use std::collections::BTreeMap;
use std::fmt::Write;

pub(crate) fn trim_trailing_slashes(value: &str) -> &str {
    value.trim_end_matches('/')
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub(crate) fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Append `query`, sorted by key, to a URL that may already carry one.
pub(crate) fn append_query(base_url: &str, query: &BTreeMap<String, String>) -> String {
    if query.is_empty() {
        return base_url.to_owned();
    }
    let mut out = String::from(base_url);
    out.push(if base_url.contains('?') { '&' } else { '?' });
    for (index, (key, value)) in query.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&url_encode(key));
        out.push('=');
        out.push_str(&url_encode(value));
    }
    out
}

/// Decode percent-escapes, and `+` as a space, so a filename embedded in a
/// content-disposition query parameter is readable. Bytes that do not form UTF-8
/// become U+FFFD.
pub(crate) fn url_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(if byte == b'+' { b' ' } else { byte });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters_and_sorts_the_query() {
        assert_eq!(url_encode("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
        let query = BTreeMap::from([
            ("platform".to_owned(), "Mac".to_owned()),
            ("format".to_owned(), "JWT".to_owned()),
            ("meta[suite]".to_owned(), "rs".to_owned()),
        ]);
        assert_eq!(append_query("https://x/api", &query), "https://x/api?format=JWT&meta%5Bsuite%5D=rs&platform=Mac");
        assert_eq!(append_query("https://x/api?a=1", &query).matches('?').count(), 1);
    }

    #[test]
    fn decodes_escapes_and_plus() {
        assert_eq!(url_decode("a%20b+c%2"), "a b c%2");
        assert_eq!(url_decode("%E2%9C%93"), "✓");
    }
}
