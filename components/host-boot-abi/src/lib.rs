#![no_std]

/// UEFI configuration-table contract between axloader and someboot.
pub const BOOT_PAYLOAD_GUID: uefi::Guid = uefi::guid!("13f4e5f5-456a-4fe6-a040-378b9cf86471");
pub const BOOT_PAYLOAD_MAGIC: u64 = 0x5447_4f53_494e_4954;
pub const BOOT_PAYLOAD_VERSION: u32 = 1;
pub const MAX_CMDLINE: usize = 4095;

#[repr(C)]
pub struct BootPayload {
    pub magic: u64,
    pub version: u32,
    pub size: u32,
    pub archive_start: u64,
    pub archive_len: u64,
    pub cmdline_len: u32,
    pub cmdline: [u8; MAX_CMDLINE],
}

impl BootPayload {
    pub const fn empty() -> Self {
        Self {
            magic: BOOT_PAYLOAD_MAGIC,
            version: BOOT_PAYLOAD_VERSION,
            size: size_of::<Self>() as u32,
            archive_start: 0,
            archive_len: 0,
            cmdline_len: 0,
            cmdline: [0; MAX_CMDLINE],
        }
    }

    pub fn set_cmdline(&mut self, cmdline: &str) -> Result<(), &'static str> {
        if cmdline.len() > MAX_CMDLINE {
            return Err("host command line is too long");
        }
        if cmdline.contains('\0') {
            return Err("host command line contains NUL");
        }
        self.cmdline[..cmdline.len()].copy_from_slice(cmdline.as_bytes());
        self.cmdline_len = cmdline.len() as u32;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.magic != BOOT_PAYLOAD_MAGIC
            || self.version != BOOT_PAYLOAD_VERSION
            || self.size as usize != size_of::<Self>()
        {
            return Err("invalid host boot payload version");
        }
        if self.cmdline_len as usize > MAX_CMDLINE
            || self.cmdline[..self.cmdline_len as usize].contains(&0)
            || core::str::from_utf8(&self.cmdline[..self.cmdline_len as usize]).is_err()
        {
            return Err("invalid host command line");
        }
        if (self.archive_start == 0) != (self.archive_len == 0)
            || self.archive_start.checked_add(self.archive_len).is_none()
        {
            return Err("invalid host archive range");
        }
        Ok(())
    }

    pub fn cmdline(&self) -> &str {
        core::str::from_utf8(&self.cmdline[..self.cmdline_len as usize])
            .expect("validated host command line")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_cannot_be_silently_truncated_at_nul() {
        let mut payload = BootPayload::empty();
        let command = "root=/dev/sda\0rdinit=/init";
        assert!(payload.set_cmdline(command).is_err());

        payload.cmdline[..command.len()].copy_from_slice(command.as_bytes());
        payload.cmdline_len = command.len() as u32;
        assert!(payload.validate().is_err());
    }
}
