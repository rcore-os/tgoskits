//! Physical-terminal output normalization.

pub(crate) struct TerminalNewlineNormalizer {
    previous_was_cr: bool,
}

impl TerminalNewlineNormalizer {
    pub(crate) const fn new() -> Self {
        Self {
            previous_was_cr: false,
        }
    }

    pub(crate) fn write<E>(
        &mut self,
        bytes: &[u8],
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut chunk_start = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'\n' && !self.previous_was_cr {
                write(&bytes[chunk_start..index])?;
                write(b"\r\n")?;
                chunk_start = index + 1;
            }
            self.previous_was_cr = byte == b'\r';
        }
        write(&bytes[chunk_start..])
    }
}
