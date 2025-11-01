use acpi::{AcpiTables, Handler};
use core::ffi::c_void;

pub mod dbg2;

/// 串口类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SerialPortType {
    /// 完全16550兼容
    Full16550Compatible,
    /// ARM SBSA UART
    ArmSbsaUart,
    /// RISC-V SBI控制台
    RiscVSbiConsole,
    /// 带通用地址结构的16550
    Generic16550,
    /// 其他类型
    Generic,
}

/// 串口信息结构
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SerialPortInfo {
    /// 基地址
    pub base_address: u64,
    /// 串口类型
    pub port_type: SerialPortType,
    /// 波特率（如果已知）
    pub baud_rate: Option<u32>,
    /// 寄存器宽度（字节）
    pub register_width: u8,
    /// 是否使用内存映射I/O
    pub memory_mapped: bool,
    /// 中断号（如果适用）
    pub interrupt: Option<u8>,
}

/// 默认串口地址列表（按优先级排序）
#[allow(dead_code)]
const DEFAULT_SERIAL_ADDRESSES: &[u64] = &[
    0x3F8, // COM1
    0x2F8, // COM2
    0x3E8, // COM3
    0x2E8, // COM4
];

/// RSDP存储
#[unsafe(link_section = ".data")]
static mut RSDP: usize = 0;

/// 设置RSDP地址
pub(crate) fn set_rsdp(addr: *const c_void) {
    unsafe {
        RSDP = addr as usize;
    }
}

/// 获取RSDP地址
fn rsdp() -> *const c_void {
    unsafe { RSDP as _ }
}

pub fn tables<T: Handler>(h: T) -> Result<AcpiTables<T>, acpi::AcpiError> {
    unsafe { ::acpi::AcpiTables::from_rsdp(h, rsdp() as usize) }
}




/// DBG2表头结构（从SDT头之后开始）
#[repr(C, packed)]
struct Dbg2TableHeader {
    /// 调试设备信息结构的偏移量（从表开始）
    info_offset: u32,
    /// 调试设备信息结构的数量
    info_count: u32,
}

/// 调试设备信息结构
#[repr(C, packed)]
struct Dbg2DeviceInfo {
    /// 版本号
    revision: u8,
    /// 结构长度
    length: u16,
    /// 寄存器数量
    number_of_registers: u8,
    /// 命名空间字符串长度
    namespace_string_length: u16,
    /// 命名空间字符串偏移量（从此结构开始）
    namespace_string_offset: u16,
    /// OEM数据长度
    oem_data_length: u16,
    /// OEM数据偏移量（从此结构开始）
    oem_data_offset: u16,
    /// 端口类型
    port_type: u16,
    /// 端口子类型
    port_subtype: u16,
    /// 保留字段
    reserved: u16,
    /// 基地址寄存器偏移量（从此结构开始）
    base_address_offset: u16,
    /// 地址大小偏移量（从此结构开始）
    address_size_offset: u16,
}

/// 通用地址结构 (Generic Address Structure - GAS)
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GenericAddress {
    /// 地址空间ID (0=内存, 1=I/O)
    address_space_id: u8,
    /// 寄存器位宽度
    register_bit_width: u8,
    /// 寄存器位偏移
    register_bit_offset: u8,
    /// 访问大小 (1=byte, 2=word, 3=dword, 4=qword)
    access_size: u8,
    /// 物理地址
    address: u64,
}

/// 从ACPI DBG2表获取串口信息
#[allow(dead_code)]
pub fn get_serial_from_dbg2<T: Handler>(_tb: &AcpiTables<T>) -> Option<SerialPortInfo> {
    // 读取RSDP来获取XSDT或RSDT地址
    let rsdp_addr = rsdp() as usize;
    if rsdp_addr == 0 {
        log::debug!("RSDP not set");
        return None;
    }

    unsafe {
        // 假设ACPI表已经被恒等映射，直接访问
        let rsdp = &*(rsdp_addr as *const acpi::rsdp::Rsdp);

        // 检查RSDP版本并获取XSDT或RSDT地址
        let (sdt_addr, is_xsdt) = if rsdp.revision() >= 2 {
            // ACPI 2.0+，使用XSDT
            (rsdp.xsdt_address() as usize, true)
        } else {
            // ACPI 1.0，使用RSDT
            (rsdp.rsdt_address() as usize, false)
        };

        if sdt_addr == 0 {
            log::debug!("No XSDT/RSDT found");
            return None;
        }

        // 读取SDT头获取长度
        let sdt_header = &*(sdt_addr as *const acpi::sdt::SdtHeader);
        let sdt_length = sdt_header.length as usize;

        // 计算表指针数组
        let entry_size = if is_xsdt { 8 } else { 4 };
        let entry_count = (sdt_length - core::mem::size_of::<acpi::sdt::SdtHeader>()) / entry_size;

        let entries_ptr = (sdt_addr + core::mem::size_of::<acpi::sdt::SdtHeader>()) as *const u8;

        // 遍历查找DBG2表
        let mut dbg2_addr: Option<usize> = None;
        let mut dbg2_len: usize = 0;

        for i in 0..entry_count {
            let table_addr = if is_xsdt {
                // 64位指针
                *((entries_ptr as *const u64).add(i)) as usize
            } else {
                // 32位指针
                *((entries_ptr as *const u32).add(i)) as usize
            };

            // 读取表头检查签名
            let header = &*(table_addr as *const acpi::sdt::SdtHeader);

            if header.signature == acpi::sdt::Signature::DBG2 {
                dbg2_addr = Some(table_addr);
                dbg2_len = header.length as usize;
                break;
            }
        }

        let (table_ptr, table_len) = match dbg2_addr {
            Some(addr) => (addr, dbg2_len),
            None => {
                log::debug!("DBG2 table not found in RSDT/XSDT");
                return None;
            }
        };

        parse_dbg2_table(table_ptr, table_len)
    }
}

/// 解析DBG2表内容
unsafe fn parse_dbg2_table(table_ptr: usize, table_len: usize) -> Option<SerialPortInfo> {
    // 安全检查：确保表至少包含SDT头和DBG2头
    if table_len
        < core::mem::size_of::<acpi::sdt::SdtHeader>() + core::mem::size_of::<Dbg2TableHeader>()
    {
        log::warn!("DBG2 table too small");
        return None;
    }

    unsafe {
        // 跳过SDT头，读取DBG2特定的头
        let header_offset = core::mem::size_of::<acpi::sdt::SdtHeader>();
        let dbg2_header_ptr = (table_ptr + header_offset) as *const Dbg2TableHeader;

        // 使用 read_unaligned 读取 packed 结构
        let info_offset =
            core::ptr::addr_of!((*dbg2_header_ptr).info_offset).read_unaligned() as usize;
        let info_count = core::ptr::addr_of!((*dbg2_header_ptr).info_count).read_unaligned();

        log::debug!("DBG2: Found {info_count} debug device(s) at offset 0x{info_offset:X}"); // 遍历所有调试设备
        for _i in 0..info_count {
            let device_ptr = (table_ptr + info_offset) as *const Dbg2DeviceInfo;

            if (device_ptr as usize) + core::mem::size_of::<Dbg2DeviceInfo>()
                > table_ptr + table_len
            {
                log::warn!("DBG2: Device info {_i} out of bounds");
                break;
            }

            // 使用 read_unaligned 读取所有字段
            let port_type = core::ptr::addr_of!((*device_ptr).port_type).read_unaligned();
            let port_subtype = core::ptr::addr_of!((*device_ptr).port_subtype).read_unaligned();
            let number_of_registers =
                core::ptr::addr_of!((*device_ptr).number_of_registers).read_unaligned();
            let base_address_offset =
                core::ptr::addr_of!((*device_ptr).base_address_offset).read_unaligned();

            log::debug!(
                "DBG2: Device {_i} - type: 0x{port_type:04X}, subtype: 0x{port_subtype:04X}, registers: {number_of_registers}"
            );

            // 检查是否是串口设备 (0x8000 = Serial)
            if port_type != 0x8000 {
                continue;
            }

            // 获取基地址寄存器
            if number_of_registers == 0 {
                log::warn!("DBG2: No base address register");
                continue;
            }

            let gas_ptr =
                (device_ptr as usize + base_address_offset as usize) as *const GenericAddress;

            if (gas_ptr as usize) + core::mem::size_of::<GenericAddress>() > table_ptr + table_len {
                log::warn!("DBG2: Base address out of bounds");
                continue;
            }

            // 使用 read_unaligned 读取 GAS 字段
            let address_space_id =
                core::ptr::addr_of!((*gas_ptr).address_space_id).read_unaligned();
            let access_size = core::ptr::addr_of!((*gas_ptr).access_size).read_unaligned();
            let address = core::ptr::addr_of!((*gas_ptr).address).read_unaligned();

            // 确定串口类型
            let serial_type = match port_subtype {
                0x0000 => SerialPortType::Full16550Compatible, // 完全16550兼容
                0x0001 => SerialPortType::Generic16550,        // 16550子集
                0x000E | 0x000F => SerialPortType::ArmSbsaUart, // ARM SBSA UART
                0x0012 => SerialPortType::RiscVSbiConsole,     // RISC-V SBI Console
                _ => SerialPortType::Generic,
            };

            let memory_mapped = address_space_id == 0; // 0 = System Memory
            let register_width = match access_size {
                1 => 1, // byte
                2 => 2, // word
                3 => 4, // dword
                4 => 8, // qword
                _ => 1,
            };

            log::info!(
                "DBG2: Serial port found - address: 0x{address:X}, type: {serial_type:?}, memory_mapped: {memory_mapped}, width: {register_width}"
            );

            return Some(SerialPortInfo {
                base_address: address,
                port_type: serial_type,
                baud_rate: None,
                register_width,
                memory_mapped,
                interrupt: None,
            });
        }

        log::debug!("DBG2: No serial port found");
        None
    }
}
