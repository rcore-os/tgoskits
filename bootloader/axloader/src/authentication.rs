//! Publisher authentication for ELF files transported by HTTP boot.
//!
//! The final 81 bytes are a 16-byte version/domain marker, one entry-mode
//! byte, and a 64-byte Ed25519 signature over all preceding file bytes.
//! The trust anchor must be provisioned independently of the boot response.

use ed25519_dalek::{Signature, VerifyingKey};

const MARKER: &[u8; 16] = b"AXLOADER-SIG-V1\0";
const SIGNATURE_LEN: usize = 64;
const TRAILER_LEN: usize = MARKER.len() + 1 + SIGNATURE_LEN;

/// A failure to authenticate the image or its requested entry mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthenticationError {
    #[error("no trusted public key was provisioned in the loader")]
    MissingTrustedKey,
    #[error("the provisioned public key is invalid")]
    InvalidTrustedKey,
    #[error("the image has no supported signature trailer")]
    MissingSignature,
    #[error("the image signature is invalid")]
    InvalidSignature,
    #[error("the requested entry mode is not authorized by the signature")]
    EntryMismatch,
}

/// Authenticates a transported file and returns only its original ELF bytes.
///
/// `trusted_key` is a preprovisioned, 64-character hexadecimal Ed25519 public
/// key, never a value obtained from discovery or HTTP. Both supported entry
/// choices (`None` and `Some("httpboot_entry")`) are bound by the signature.
/// No ELF parsing, allocation, or segment copying occurs here.
pub fn authenticate_image<'a>(
    image: &'a [u8],
    trusted_key: Option<&str>,
    entry_symbol: Option<&str>,
) -> Result<&'a [u8], AuthenticationError> {
    let key = trusted_key.ok_or(AuthenticationError::MissingTrustedKey)?;
    let key = parse_public_key(key)?;
    let elf_len = image
        .len()
        .checked_sub(TRAILER_LEN)
        .ok_or(AuthenticationError::MissingSignature)?;
    let (elf, trailer) = image.split_at(elf_len);
    if &trailer[..MARKER.len()] != MARKER {
        return Err(AuthenticationError::MissingSignature);
    }
    let signed_len = image.len() - SIGNATURE_LEN;
    let signature = Signature::from_slice(&image[signed_len..])
        .map_err(|_| AuthenticationError::InvalidSignature)?;
    key.verify_strict(&image[..signed_len], &signature)
        .map_err(|_| AuthenticationError::InvalidSignature)?;
    if entry_mode(entry_symbol) != Some(trailer[MARKER.len()]) {
        return Err(AuthenticationError::EntryMismatch);
    }
    Ok(elf)
}

fn entry_mode(entry_symbol: Option<&str>) -> Option<u8> {
    match entry_symbol {
        None => Some(0),
        Some("httpboot_entry") => Some(1),
        Some(_) => None,
    }
}

fn parse_public_key(hex: &str) -> Result<VerifyingKey, AuthenticationError> {
    if hex.len() != 64 {
        return Err(AuthenticationError::InvalidTrustedKey);
    }
    let mut bytes = [0; 32];
    for (byte, encoded) in bytes.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        let high = nibble(encoded[0]).ok_or(AuthenticationError::InvalidTrustedKey)?;
        let low = nibble(encoded[1]).ok_or(AuthenticationError::InvalidTrustedKey)?;
        *byte = high << 4 | low;
    }
    let key =
        VerifyingKey::from_bytes(&bytes).map_err(|_| AuthenticationError::InvalidTrustedKey)?;
    if key.is_weak() {
        return Err(AuthenticationError::InvalidTrustedKey);
    }
    Ok(key)
}

/// Signs the final, postprocessed ELF and returns the public key as hex.
///
/// The PKCS#8 PEM private key belongs only on the publishing host. The caller
/// must not transform the resulting file before upload. The input buffer is
/// reused, with only the fixed-size authentication trailer appended.
#[cfg(any(windows, unix))]
pub fn sign_image(
    image: &mut Vec<u8>,
    private_key_pem: &str,
    entry_symbol: Option<&str>,
) -> anyhow::Result<String> {
    use std::fmt::Write;

    use anyhow::{Context, bail};
    use ed25519_dalek::{Signer, SigningKey, pkcs8::DecodePrivateKey};

    if !image.starts_with(b"\x7fELF") {
        bail!("the input must be a final ELF image");
    }
    let mode = entry_mode(entry_symbol).context("unsupported entry symbol")?;
    let key = SigningKey::from_pkcs8_pem(private_key_pem)
        .context("failed to decode Ed25519 PKCS#8 private key")?;
    image.extend_from_slice(MARKER);
    image.push(mode);
    let signature = key.sign(image);
    image.extend_from_slice(&signature.to_bytes());
    let mut hex = String::with_capacity(64);
    for byte in key.verifying_key().as_bytes() {
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey};

    use super::*;

    #[test]
    fn only_authorized_bytes_and_entry_modes_are_accepted() {
        // Key material exists only in this deterministic component test.
        let key = SigningKey::from_bytes(&[42; 32]);
        let pem = key.to_pkcs8_pem(Default::default()).unwrap();
        let original = b"\x7fELFpublisher-authorized-image";
        for entry in [None, Some("httpboot_entry")] {
            let mut signed = original.to_vec();
            let public_key = sign_image(&mut signed, &pem, entry).unwrap();
            assert_eq!(
                authenticate_image(&signed, Some(&public_key), entry),
                Ok(original.as_slice())
            );
            assert_eq!(
                authenticate_image(&signed, None, entry),
                Err(AuthenticationError::MissingTrustedKey)
            );
            assert_eq!(
                authenticate_image(original, Some(&public_key), entry),
                Err(AuthenticationError::MissingSignature)
            );
            assert_eq!(
                authenticate_image(&signed, Some(&"00".repeat(32)), entry),
                Err(AuthenticationError::InvalidTrustedKey)
            );
            let other_entry = if entry.is_some() {
                None
            } else {
                Some("httpboot_entry")
            };
            assert_eq!(
                authenticate_image(&signed, Some(&public_key), other_entry),
                Err(AuthenticationError::EntryMismatch)
            );
            for index in 0..signed.len() {
                signed[index] ^= 1;
                assert!(
                    authenticate_image(&signed, Some(&public_key), entry).is_err(),
                    "accepted changed byte {index}"
                );
                signed[index] ^= 1;
            }
            let other_pem = SigningKey::from_bytes(&[43; 32])
                .to_pkcs8_pem(Default::default())
                .unwrap();
            let mut forged = original.to_vec();
            sign_image(&mut forged, &other_pem, entry).unwrap();
            assert_eq!(
                authenticate_image(&forged, Some(&public_key), entry),
                Err(AuthenticationError::InvalidSignature)
            );
        }
    }
}
