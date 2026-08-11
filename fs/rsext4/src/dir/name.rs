//! Validated raw ext4 directory-entry names.

use crate::{
    config::DIRNAME_LEN,
    error::{Ext4Error, Ext4Result},
};

/// A validated ext4 directory-entry name.
///
/// Names remain arbitrary bytes. The portable core does not require UTF-8 and
/// reserves Unicode normalization and encrypted-name policy for injected
/// runtime capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileName<'a>(&'a [u8]);

impl<'a> FileName<'a> {
    pub fn new(bytes: &'a [u8]) -> Ext4Result<Self> {
        if bytes.is_empty()
            || bytes.len() > DIRNAME_LEN
            || bytes.contains(&0)
            || bytes.contains(&b'/')
        {
            return Err(Ext4Error::invalid_input().with_operation("directory:name"));
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(self) -> &'a [u8] {
        self.0
    }

    pub fn is_dot(self) -> bool {
        self.0 == b"."
    }

    pub fn is_dotdot(self) -> bool {
        self.0 == b".."
    }

    pub fn is_reserved(self) -> bool {
        self.is_dot() || self.is_dotdot()
    }
}

#[cfg(test)]
mod tests {
    use super::FileName;

    #[test]
    fn validates_format_names_without_imposing_utf8() {
        assert!(FileName::new(&[0xff]).is_ok());
        assert!(FileName::new(b"").is_err());
        assert!(FileName::new(b"a/b").is_err());
        assert!(FileName::new(b"a\0b").is_err());
        assert!(FileName::new(&[b'a'; 255]).is_ok());
        assert!(FileName::new(&[b'a'; 256]).is_err());
    }
}
