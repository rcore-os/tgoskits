use std::{
    alloc::{self, Layout},
    collections::HashSet,
    fmt::Debug,
    mem,
    sync::{Arc, Mutex},
};

use page_table_generic::*;
use tock_registers::{interfaces::*, register_bitfields, registers::*};

register_bitfields! [
    u64,
    PTE64 [
        PA OFFSET(0) NUMBITS(48) [
        ],
        READ OFFSET(48) NUMBITS(1) [
        ],
        WRITE OFFSET(49) NUMBITS(1) [
        ],
        USER_EXECUTE OFFSET(50) NUMBITS(1) [
        ],
        USER_ACCESS OFFSET(51) NUMBITS(1) [
        ],
        PRIVILEGE_EXECUTE OFFSET(52) NUMBITS(1) [
        ],
        BLOCK OFFSET(53) NUMBITS(1) [
        ],
        CACHE OFFSET(54) NUMBITS(2) [
            NonCache = 0,
            Normal = 0b01,
            Device = 0b10,
        ],
        VALID OFFSET(63) NUMBITS(1) [

        ]
    ],
];

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PteImpl(pub u64);

impl PteImpl {
    fn reg(&self) -> &ReadWrite<u64, PTE64::Register> {
        unsafe { mem::transmute(self) }
    }
}

impl Debug for PteImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let config = self.to_config(false);
        if !config.valid {
            return write!(f, "invalid");
        }

        write!(f, "PTE PA: {:?} Block: {:?}", config.paddr, config.huge)
    }
}

impl PageTableEntry for PteImpl {
    fn from_config(config: PteConfig) -> Self {
        let mut pte = Self(0);

        // 设置物理地址
        let paddr = config.paddr.raw() >> 12;
        pte.reg().modify(PTE64::PA.val(paddr as _));

        // 设置标志位
        if config.valid {
            pte.reg().modify(PTE64::VALID::SET);
        }
        if config.read {
            pte.reg().modify(PTE64::READ::SET);
        }
        if config.writable {
            pte.reg().modify(PTE64::WRITE::SET);
        }
        if config.executable {
            pte.reg().modify(PTE64::PRIVILEGE_EXECUTE::SET);
        }
        if config.lower {
            pte.reg().modify(PTE64::USER_ACCESS::SET);
        }
        if config.huge {
            pte.reg().modify(PTE64::BLOCK::SET);
        }

        // 设置内存属性
        let cache = match config.mem_attr {
            MemAttributes::Device => 2,
            MemAttributes::Uncached => 0,
            MemAttributes::Normal | MemAttributes::PerCpu => 1,
        };
        pte.reg().modify(PTE64::CACHE.val(cache));

        // 注意：Mock 实现不支持 global, accessed, dirty

        pte
    }

    fn to_config(&self, _is_dir: bool) -> PteConfig {
        PteConfig {
            paddr: ((self.reg().read(PTE64::PA) << 12) as usize).into(),
            valid: self.reg().is_set(PTE64::VALID),
            read: self.reg().is_set(PTE64::READ),
            writable: self.reg().is_set(PTE64::WRITE),
            executable: self.reg().is_set(PTE64::PRIVILEGE_EXECUTE),
            lower: self.reg().is_set(PTE64::USER_ACCESS),
            dirty: false,  // Mock 不支持
            global: false, // Mock 不支持
            is_dir: _is_dir,
            huge: self.reg().is_set(PTE64::BLOCK),
            mem_attr: match self.reg().read(PTE64::CACHE) {
                1 => MemAttributes::Normal,
                2 => MemAttributes::Device,
                _ => MemAttributes::Uncached,
            },
        }
    }

    fn valid(&self) -> bool {
        self.reg().is_set(PTE64::VALID)
    }
}

// Flag 构造和操作方法
impl PteImpl {
    /// 创建带有指定flags的PTE
    pub fn new_with_flags(
        read: bool,
        write: bool,
        user_execute: bool,
        user_access: bool,
        privilege_execute: bool,
        cache: u64, // 0: NonCache, 1: Normal, 2: Device
        valid: bool,
        is_block: bool,
    ) -> Self {
        let pte = PteImpl(0);

        if read {
            pte.reg().modify(PTE64::READ::SET);
        }
        if write {
            pte.reg().modify(PTE64::WRITE::SET);
        }
        if user_execute {
            pte.reg().modify(PTE64::USER_EXECUTE::SET);
        }
        if user_access {
            pte.reg().modify(PTE64::USER_ACCESS::SET);
        }
        if privilege_execute {
            pte.reg().modify(PTE64::PRIVILEGE_EXECUTE::SET);
        }
        pte.reg().modify(PTE64::CACHE.val(cache));
        if valid {
            pte.reg().modify(PTE64::VALID::SET);
        }
        if is_block {
            pte.reg().modify(PTE64::BLOCK::SET);
        }

        pte
    }

    /// 用户权限模式：可读、可写、可执行、用户可访问
    pub fn user_mode() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            true,  // user_execute
            true,  // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 内核权限模式：可读、可写、特权执行
    pub fn kernel_mode() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 用户权限模式的 PteConfig
    pub fn user_mode_config() -> PteConfig {
        PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: true,
            lower: true,
            mem_attr: MemAttributes::Normal,
            ..Default::default()
        }
    }

    /// 内核权限模式的 PteConfig
    pub fn kernel_mode_config() -> PteConfig {
        PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: true,
            lower: false,
            mem_attr: MemAttributes::Normal,
            ..Default::default()
        }
    }

    /// 只读数据模式：只读、普通缓存
    pub fn read_only() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 读写模式：可读、可写、普通缓存
    pub fn read_write() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 设备寄存器模式：读写、设备缓存、大页
    pub fn device_memory() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            2,     // device cache
            true,  // valid
            true,  // block (大页)
        )
    }

    /// 内存映射I/O模式：用户可访问、只读、设备缓存
    pub fn mmap_io() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            true,  // user_access
            false, // privilege_execute
            2,     // device cache
            true,  // valid
            false, // not block
        )
    }

    // Flag 查询方法
    pub fn is_readable(&self) -> bool {
        self.reg().is_set(PTE64::READ)
    }

    pub fn is_writable(&self) -> bool {
        self.reg().is_set(PTE64::WRITE)
    }

    pub fn is_user_executable(&self) -> bool {
        self.reg().is_set(PTE64::USER_EXECUTE)
    }

    pub fn is_user_accessible(&self) -> bool {
        self.reg().is_set(PTE64::USER_ACCESS)
    }

    pub fn is_privilege_executable(&self) -> bool {
        self.reg().is_set(PTE64::PRIVILEGE_EXECUTE)
    }

    pub fn cache_mode(&self) -> u64 {
        self.reg().read(PTE64::CACHE)
    }

    /// 创建一个新的空PTE
    pub fn new() -> Self {
        Self(0)
    }

    /// 可读可执行模式：只读、用户可执行
    pub fn read_execute() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            true,  // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 全部权限模式：可读、可写、用户可执行、用户可访问、特权执行
    pub fn all_permissions() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            true,  // user_execute
            true,  // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 用户执行模式：只读、用户可执行
    pub fn user_execute() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            true,  // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 特权执行模式：只读、特权执行
    pub fn privilege_execute() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 非缓存模式：只读、非缓存
    pub fn non_cache() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            0,     // non cache
            true,  // valid
            false, // not block
        )
    }

    /// 普通缓存模式：只读、普通缓存
    pub fn normal_cache() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 设备缓存模式：只读、设备缓存
    pub fn device_cache() -> Self {
        Self::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            2,     // device cache
            true,  // valid
            false, // not block
        )
    }

    /// 复杂用户映射：全部权限 + 大页
    pub fn complex_user_mapping() -> Self {
        Self::new_with_flags(
            true, // read
            true, // write
            true, // user_execute
            true, // user_access
            true, // privilege_execute
            1,    // normal cache
            true, // valid
            true, // block (大页)
        )
    }

    /// 复杂用户映射配置：全部权限 + 大页
    pub fn complex_user_mapping_config() -> PteConfig {
        PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: true,
            lower: true,
            huge: true,
            mem_attr: MemAttributes::Normal,
            ..Default::default()
        }
    }

    /// 复杂内核映射：读写 + 特权执行，非大页
    pub fn complex_kernel_mapping() -> Self {
        Self::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        )
    }

    /// 复杂内核映射配置：读写 + 特权执行，非大页
    pub fn complex_kernel_mapping_config() -> PteConfig {
        PteConfig {
            valid: true,
            read: true,
            writable: true,
            executable: true,
            lower: false,
            huge: false,
            mem_attr: MemAttributes::Normal,
            ..Default::default()
        }
    }

    /// 获取页表项的内存配置
    ///
    /// 这是一个便捷方法，将细粒度的 PageTableEntry trait 方法
    /// 组合成高级别的 MemConfig 结构。
    pub fn mem_config(&self) -> MemConfig {
        let config = self.to_config(false);
        let mut access = AccessFlags::empty();

        // 根据页表项状态设置访问权限
        if config.writable {
            access |= AccessFlags::WRITE;
        }
        if config.executable {
            access |= AccessFlags::EXECUTE;
        }
        if config.lower {
            access |= AccessFlags::LOWER;
        }

        // 假设所有有效的页表项都是可读的
        // （如果架构不支持不可读的页，则总是设置此位）
        if config.valid {
            access |= AccessFlags::READ;
        }

        MemConfig {
            access,
            attrs: config.mem_attr,
        }
    }

    /// 设置页表项的内存配置
    ///
    /// 这是一个便捷方法，从高级别的 MemConfig 结构中提取配置
    /// 并调用相应的 PageTableEntry trait 方法。
    pub fn set_mem_config(&mut self, config: MemConfig) {
        let current_config = self.to_config(false);

        // 创建新的配置
        let new_config = PteConfig {
            writable: config.access.contains(AccessFlags::WRITE),
            executable: config.access.contains(AccessFlags::EXECUTE),
            lower: config.access.contains(AccessFlags::LOWER),
            mem_attr: config.attrs,
            ..current_config
        };

        *self = Self::from_config(new_config);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct T4kL3;

impl TableMeta for T4kL3 {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;

    const MAX_BLOCK_LEVEL: usize = 2;

    fn flush(vaddr: Option<VirtAddr>) {
        let _ = vaddr;
    }

    const LEVEL_BITS: &[usize] = &[9, 9, 9];
}

#[derive(Debug, Clone, Copy)]
pub struct T4kL4;

impl TableMeta for T4kL4 {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<VirtAddr>) {
        let _ = vaddr;
    }

    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
}

#[derive(Debug, Clone, Copy)]
pub struct T4kL5;

impl TableMeta for T4kL5 {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;

    const MAX_BLOCK_LEVEL: usize = 4;

    fn flush(vaddr: Option<VirtAddr>) {
        let _ = vaddr;
    }

    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9, 9];
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Fram4k;

impl FrameAllocator for Fram4k {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        let ptr = unsafe { alloc::alloc(layout) };
        if ptr.is_null() {
            None
        } else {
            Some(PhysAddr::new(ptr as usize))
        }
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        unsafe {
            alloc::dealloc(frame.raw() as *mut u8, layout);
        }
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.raw() as *mut u8
    }
}

/// 跟踪分配器，用于测试内存泄漏
#[derive(Debug, Clone, Copy)]
pub struct TrackedFram4k {
    allocated_frames: *const Mutex<HashSet<usize>>,
}

impl Default for TrackedFram4k {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackedFram4k {
    /// 创建新的跟踪分配器
    pub fn new() -> Self {
        let frames = Arc::new(Mutex::new(HashSet::new()));
        let ptr = Arc::into_raw(frames);
        Self {
            allocated_frames: ptr,
        }
    }

    /// 获取当前分配的帧数量
    pub fn allocated_count(&self) -> usize {
        unsafe {
            let frames = &*self.allocated_frames;
            frames.lock().unwrap().len()
        }
    }

    /// 获取所有已分配的帧地址
    #[allow(dead_code)]
    pub fn allocated_frames(&self) -> Vec<usize> {
        unsafe {
            let frames = &*self.allocated_frames;
            frames.lock().unwrap().iter().copied().collect()
        }
    }

    /// 检查是否有内存泄漏
    pub fn has_leaks(&self) -> bool {
        unsafe {
            let frames = &*self.allocated_frames;
            !frames.lock().unwrap().is_empty()
        }
    }

    /// 打印分配统计信息
    pub fn print_stats(&self) {
        unsafe {
            let frames = &*self.allocated_frames;
            let frames = frames.lock().unwrap();
            println!("分配器统计: {} 个帧已分配", frames.len());
            if !frames.is_empty() {
                println!("未释放的帧地址:");
                for addr in frames.iter() {
                    println!("  - {:#x}", addr);
                }
            }
        }
    }
}

unsafe impl Send for TrackedFram4k {}
unsafe impl Sync for TrackedFram4k {}

impl FrameAllocator for TrackedFram4k {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        let ptr = unsafe { alloc::alloc(layout) };
        if ptr.is_null() {
            None
        } else {
            let addr = ptr as usize;
            // 记录分配的地址
            unsafe {
                let frames = &*self.allocated_frames;
                frames.lock().unwrap().insert(addr);
            }
            Some(PhysAddr::new(addr))
        }
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        let addr = frame.raw();

        // 从跟踪记录中移除
        unsafe {
            let frames = &*self.allocated_frames;
            let removed = frames.lock().unwrap().remove(&addr);
            if !removed {
                panic!("尝试释放未跟踪的帧地址: {:#x}", addr);
            }
        }

        let layout = Layout::from_size_align(4096, 4096).unwrap();
        unsafe {
            alloc::dealloc(addr as *mut u8, layout);
        }
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.raw() as *mut u8
    }
}
