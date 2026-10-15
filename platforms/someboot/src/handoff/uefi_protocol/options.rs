//! UCS-2 LoadOptions encoding without firmware calls.
use alloc::vec::Vec;

use crate::{boot_resources::ResourceError, handoff::HandoffError};

#[derive(Debug)]
pub(in crate::handoff) struct PreparedLoadOptions {
    pub(in crate::handoff) words: Option<Vec<u16>>,
}

impl PreparedLoadOptions {
    pub(in crate::handoff) fn new(cmdline: Option<&str>) -> Result<Self, HandoffError> {
        let words = cmdline
            .filter(|cmdline| !cmdline.is_empty())
            .map(|cmdline| {
                if super::super::validate_cmdline(cmdline).is_err() {
                    return Err(HandoffError::InvalidCommandLine);
                }
                let mut words = Vec::new();
                words
                    .try_reserve_exact(cmdline.len() + 1)
                    .map_err(|_| HandoffError::Resource(ResourceError::Capacity))?;
                words.extend(cmdline.bytes().map(u16::from));
                words.push(0);
                Ok(words)
            })
            .transpose()?;
        Ok(Self { words })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{HandoffError, PreparedLoadOptions};

    #[test]
    fn prepares_optional_nul_terminated_ucs2_load_options() {
        assert!(PreparedLoadOptions::new(None).unwrap().words.is_none());
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0"))
                .unwrap()
                .words
                .unwrap(),
            "console=ttyS0"
                .bytes()
                .map(u16::from)
                .chain([0])
                .collect::<Vec<_>>()
        );
        assert!(PreparedLoadOptions::new(Some("")).unwrap().words.is_none());
    }

    #[test]
    fn rejects_invalid_load_options_before_installing_protocol_pointer() {
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0\nreset")).unwrap_err(),
            HandoffError::InvalidCommandLine
        );
        assert_eq!(
            PreparedLoadOptions::new(Some(&"x".repeat(4096))).unwrap_err(),
            HandoffError::InvalidCommandLine
        );
    }
}
