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
    // 参考 Linux earlycon 语法，支持以下常见形式：
    // 1) earlycon=<drv>
    // 2) earlycon=<drv>,<io>,<addr>[,<opts>]
    //    其中 <io> ∈ { io, mmio, mmio32 }，<addr> 支持 0xHEX 或十进制
    // 3) earlycon=<drv>,<addr>[,<opts>] （省略 io，默认按 mmio 处理）
    // 4) 兼容同义词：uart|uart8250|8250|ns16550|ns16550a 统一视作 ns16550

    // 注意：cmdline::var 返回 &'static str，故从中切片得到的 &str 仍为 'static
    // 保证后续所有派生切片均来源于 &'static str
    let s: &'static str = val.trim();
    if s.is_empty() {
        return None;
    }

    // 拆分逗号，但也保留原字符串用于 args 切片
    let mut parts = s.split(',');
    let first_raw = parts.next()?; // 来源于 s (&'static str)
    let first = first_raw.trim();

    // 规范化驱动名
    // 驱动名用原始 first（保持 'static 切片），若是同义词替换为固定字面量
    let uart_type: &'static str = match first {
        "uart" | "uart8250" | "8250" | "ns16550" | "ns16550a" => "ns16550",
        _ => first,
    };

    // 预设输出字段
    #[cfg(not(target_arch = "x86_64"))]
    let mut io_type = "mmio32";
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
