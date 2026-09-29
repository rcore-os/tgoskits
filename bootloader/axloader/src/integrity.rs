use sha2::{Digest, Sha256};

pub fn sha256_matches(bytes: &[u8], expected: &str) -> bool {
    decode_sha256(expected).as_ref() == Some(&Sha256::digest(bytes).into())
}

pub fn decode_sha256(encoded: &str) -> Option<[u8; 32]> {
    if encoded.len() != 64 {
        return None;
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = decode_hex_byte(&encoded.as_bytes()[offset..offset + 2])?;
    }
    Some(result)
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
