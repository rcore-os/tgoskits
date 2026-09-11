use sha2::{Digest, Sha256};

pub fn sha256_matches(bytes: &[u8], expected: &str) -> bool {
    if expected.len() != 64 {
        return false;
    }
    Sha256::digest(bytes)
        .iter()
        .enumerate()
        .all(|(index, actual)| {
            let offset = index * 2;
            decode_hex_byte(&expected.as_bytes()[offset..offset + 2]) == Some(*actual)
        })
}

fn decode_hex_byte(encoded: &[u8]) -> Option<u8> {
    Some(hex_nibble(*encoded.first()?)? << 4 | hex_nibble(*encoded.get(1)?)?)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::sha256_matches;

    #[test]
    fn rejects_a_kernel_whose_sha256_does_not_match() {
        let expected = "6923dd1bc0460082c5d55a831908c24a282860b7f1cd6c2b79cf1bc8857c639c";
        assert!(sha256_matches(b"kernel", expected));
        assert!(!sha256_matches(b"corrupted kernel", expected));
        assert!(!sha256_matches(b"kernel", "not-a-sha256"));
    }
}
