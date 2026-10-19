//! Command-line bounds and ASCII rules shared by producers and EFI consumers.
use super::{HandoffError, MAX_CMDLINE};

fn valid_byte(byte: u8) -> bool {
    byte == b' ' || byte.is_ascii_graphic()
}

pub(crate) fn validate(cmdline: &str) -> Result<(), HandoffError> {
    if cmdline.len() > MAX_CMDLINE || !cmdline.bytes().all(valid_byte) {
        return Err(HandoffError::InvalidCommandLine);
    }
    Ok(())
}

#[cfg(any(test, all(efi, not(target_os = "uefi"))))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EfiCmdlineError {
    TooLong,
    InvalidCharacter,
}

#[cfg(any(test, all(efi, not(target_os = "uefi"))))]
pub(crate) fn decode_efi_cmdline<'a>(
    words: &[u16],
    bytes: &'a mut [u8; MAX_CMDLINE],
) -> core::result::Result<&'a str, EfiCmdlineError> {
    if words.len() > bytes.len() {
        return Err(EfiCmdlineError::TooLong);
    }
    for (index, word) in words.iter().copied().enumerate() {
        let byte = u8::try_from(word).map_err(|_| EfiCmdlineError::InvalidCharacter)?;
        if !valid_byte(byte) {
            return Err(EfiCmdlineError::InvalidCharacter);
        }
        bytes[index] = byte;
    }
    core::str::from_utf8(&bytes[..words.len()]).map_err(|_| EfiCmdlineError::InvalidCharacter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_valid_ascii_efi_command_lines() {
        let mut bytes = [0_u8; MAX_CMDLINE];
        assert_eq!(
            decode_efi_cmdline(&[b'a' as u16; MAX_CMDLINE], &mut bytes).unwrap(),
            "a".repeat(MAX_CMDLINE)
        );
        assert_eq!(decode_efi_cmdline(&[], &mut bytes).unwrap(), "");
        assert_eq!(
            decode_efi_cmdline(&[b'a' as u16, b' ' as u16, b'=' as u16], &mut bytes).unwrap(),
            "a ="
        );
    }

    #[test]
    fn rejects_unsupported_efi_command_lines() {
        let mut bytes = [0_u8; MAX_CMDLINE];
        assert_eq!(
            decode_efi_cmdline(&vec![b'a' as u16; MAX_CMDLINE + 1], &mut bytes),
            Err(EfiCmdlineError::TooLong)
        );
        for words in [&[0x100_u16][..], &[b'\t' as u16][..], &[b'\n' as u16][..]] {
            assert_eq!(
                decode_efi_cmdline(words, &mut bytes),
                Err(EfiCmdlineError::InvalidCharacter)
            );
        }
    }
}
