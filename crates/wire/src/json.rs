pub use values::{Extra, JsonObject, Scrub, canonical_json, member_path};

#[must_use]
pub fn replace_lone_surrogates(bytes: &[u8]) -> Vec<u8> {
    if !bytes.windows(2).any(|window| window == b"\\u") {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(byte) = bytes.get(index).copied() {
        if is_escape_at(bytes, index) {
            let code = optional_escape_code(bytes, index);
            let paired = code.is_some_and(|high| (0xD800..=0xDBFF).contains(&high))
                && is_escape_at(bytes, index + 6)
                && optional_escape_code(bytes, index + 6)
                    .is_some_and(|low| (0xDC00..=0xDFFF).contains(&low));
            if paired {
                if let Some(pair) = bytes.get(index..index + 12) {
                    out.extend_from_slice(pair);
                }
                index += 12;
            } else if code.is_some_and(|unit| (0xD800..=0xDFFF).contains(&unit)) {
                out.extend_from_slice(b"\\ufffd");
                index += 6;
            } else {
                out.push(byte);
                index += 1;
            }
        } else {
            out.push(byte);
            index += 1;
        }
    }
    out
}

fn is_escape_at(bytes: &[u8], index: usize) -> bool {
    bytes.get(index) == Some(&b'\\')
        && bytes.get(index + 1) == Some(&b'u')
        && preceded_by_even_backslashes(bytes, index)
        && bytes
            .get(index + 2..index + 6)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
}

fn preceded_by_even_backslashes(bytes: &[u8], index: usize) -> bool {
    let mut count = 0;
    while let Some(previous) = index.checked_sub(count + 1).and_then(|at| bytes.get(at)) {
        if *previous != b'\\' {
            break;
        }
        count += 1;
    }
    count % 2 == 0
}

fn optional_escape_code(bytes: &[u8], index: usize) -> Option<u32> {
    let hex = bytes.get(index + 2..index + 6)?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS JSONBYTES: a non-hex escape has no surrogate code point"
        )
    )]
    let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok();
    code
}

#[cfg(test)]
mod surrogate_tests {
    use super::replace_lone_surrogates;

    #[test]
    fn escaped_backslashes_are_not_treated_as_escapes() {
        let sanitized = replace_lone_surrogates(br#"{"content":"a\\ud800"}"#);
        assert_eq!(&sanitized[..], br#"{"content":"a\\ud800"}"#);
        let sanitized = replace_lone_surrogates(br#""\ud83d\ude00""#);
        assert_eq!(&sanitized[..], br#""\ud83d\ude00""#);
        let sanitized = replace_lone_surrogates(br#""x\ud800y""#);
        assert_eq!(&sanitized[..], br#""x\ufffdy""#);
    }
}
