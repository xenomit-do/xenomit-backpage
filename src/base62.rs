const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Port of `utils/Base62.java` — note `encode(0)` returns `"a"`.
pub fn encode(mut i: i64) -> String {
    if i == 0 {
        return "a".to_string();
    }
    let mut sb = Vec::new();
    while i > 0 {
        sb.push(ALPHABET[(i % 62) as usize]);
        i /= 62;
    }
    sb.reverse();
    String::from_utf8(sb).unwrap()
}

/// Port of `Base62.decode` — most significant digit first.
/// Note: `decode(encode(0))` is `36` upstream too (`encode(0)` returns `"a"`).
pub fn decode(s: &str) -> i64 {
    let mut n: i64 = 0;
    for c in s.chars() {
        let digit = ALPHABET.iter().position(|&a| a == c as u8).unwrap_or(0) as i64;
        n = n * 62 + digit;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for i in [1, 61, 62, 63, 12345, 987654321] {
            assert_eq!(decode(&encode(i)), i, "roundtrip failed for {i}");
        }
    }

    #[test]
    fn known_values() {
        assert_eq!(encode(0), "a");
        assert_eq!(encode(10), "A");
        assert_eq!(encode(62), "10");
        assert_eq!(decode("10"), 62);
        assert_eq!(decode("z"), 61);
    }
}
