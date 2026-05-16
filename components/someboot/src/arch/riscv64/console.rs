pub struct Console;

struct SbiConsole;

impl crate::console::Con for SbiConsole {
    fn write_bytes(&self, bytes: &[u8]) -> usize {
        for &byte in bytes {
            #[allow(deprecated)]
            {
                sbi_rt::legacy::console_putchar(byte as usize);
            }
        }
        bytes.len()
    }
}

static SBI_CONSOLE: SbiConsole = SbiConsole;

impl crate::console::ArchConsoleOps for Console {
    fn init() -> bool {
        unsafe {
            crate::console::set_out(&SBI_CONSOLE);
        }
        true
    }

    fn read_byte() -> Option<u8> {
        #[allow(deprecated)]
        let ch = sbi_rt::legacy::console_getchar();
        if ch == usize::MAX {
            None
        } else {
            Some(ch as u8)
        }
    }
}
