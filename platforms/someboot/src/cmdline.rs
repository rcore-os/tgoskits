use kernutil::StaticCell;

static CMDLINE: StaticCell<[u8; 0x1000]> = StaticCell::new([0; 0x1000]);

const BUILDIN_CMDLINE: Option<&str> = option_env!("KERNEL_BUILTIN_CMDLINE");

pub fn set_cmdline(cmdline: &str) {
    let bytes = cmdline.as_bytes();
    let len = bytes.len().min(CMDLINE.len() - 1);

    unsafe {
        CMDLINE.update(|cmd| {
            cmd[..len].copy_from_slice(&bytes[..len]);
            cmd[len] = 0;
        });
    }
}

pub fn cmdline() -> Option<&'static str> {
    if CMDLINE[0] == 0 {
        return BUILDIN_CMDLINE;
    }

    let len = CMDLINE
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(CMDLINE.len());
    Some(unsafe { core::str::from_utf8_unchecked(&CMDLINE[..len]) })
}

pub(crate) fn has_handoff_cmdline() -> bool {
    CMDLINE[0] != 0
}

pub fn var(key: &str) -> Option<&'static str> {
    let cmdline = cmdline()?;

    for pair in cmdline.split_whitespace() {
        if let Some(pos) = pair.find('=') {
            let (k, v) = pair.split_at(pos);
            if k == key {
                return Some(&v[1..]);
            }
        }
    }
    None
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct EarlyconConfig {
    pub uart_type: &'static str,
    pub io_type: &'static str,
    pub base_addr: Option<usize>,
    pub options: Option<&'static str>,
}

fn parse_earlycon_argument(val: &'static str) -> Option<EarlyconConfig> {
    // Accept <drv>, <drv>,<addr>[,<opts>] and <drv>,<io>,<addr>[,<opts>].
    // An omitted I/O mode defaults to byte MMIO for RISC-V NS16550,
    // port I/O on x86_64, and 32-bit MMIO otherwise.

    // Parsed fields borrow from the static kernel command line.
    let s: &'static str = val.trim();
    if s.is_empty() {
        return None;
    }

    let mut parts = s.split(',');
    let first_raw = parts.next()?;
    let first = first_raw.trim();

    // Normalize 8250 aliases. Keep `xscale` for the MMIO variant that sets IER.UUE.
    let uart_type: &'static str = match first {
        "uart" | "uart8250" | "8250" | "ns16550" | "ns16550a" => "ns16550",
        _ => first,
    };

    #[cfg(not(target_arch = "x86_64"))]
    let mut io_type = if cfg!(target_arch = "riscv64") && uart_type == "ns16550" {
        "mmio"
    } else {
        "mmio32"
    };
    #[cfg(target_arch = "x86_64")]
    let mut io_type = "io";

    let mut base_addr: Option<usize> = None;
    let mut options: Option<&'static str> = None;

    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.contains("io") {
            io_type = part;
            continue;
        }
        if part.starts_with("0x") {
            base_addr = parse_usize(part);
            continue;
        }
        options = Some(part);
    }

    Some(EarlyconConfig {
        uart_type,
        io_type,
        base_addr,
        options,
    })
}

pub fn earlycon() -> Option<EarlyconConfig> {
    let val = crate::cmdline::var("earlycon")?;

    let config = parse_earlycon_argument(val)?;
    Some(config)
}

/// 解析十进制或 0x 开头的十六进制 usize
fn parse_usize(s: &str) -> Option<usize> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<usize>().ok()
    }
}

#[cfg(all(test, target_arch = "riscv64"))]
mod tests {
    use super::parse_earlycon_argument;

    #[test]
    fn ns16550_shorthand_uses_byte_mmio() {
        let config = parse_earlycon_argument("ns16550,0x10000000").unwrap();
        assert_eq!(config.io_type, "mmio");
        assert_eq!(config.base_addr, Some(0x1000_0000));

        let config = parse_earlycon_argument("ns16550,mmio32,0x10000000").unwrap();
        assert_eq!(config.io_type, "mmio32");
    }
}
