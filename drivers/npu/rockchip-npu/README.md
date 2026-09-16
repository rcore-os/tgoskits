# 运行测试

安装 `ostool`

```bash
cargo install ostool
```

运行测试

```bash
cargo test --test test -- tests --show-output uboot
```

## RKNPU Minimal Device Layer

基于orangepi-build内核驱动实现的最小化RKNPU设备层，使用OSAL接口抽象系统依赖，专注于硬件操作逻辑。

## 特性

- **OSAL抽象层**: 操作系统抽象层，支持不同平台的移植
- **硬件抽象层**: 直接的硬件操作接口，支持任务提交、中断处理
- **内存管理**: 统一的内存分配和管理接口，支持SRAM、NBUF、IOMMU
- **设备接口**: 高层设备接口，兼容原有驱动的IOCTL语义
- **中断处理**: 通过irq_handle接口处理中断，不包含中断注册

## 模块结构

```tree
src/
├── lib.rs          # 主入口，对外API
├── osal.rs         # 操作系统抽象层
├── hal.rs          # 硬件抽象层
├── memory.rs       # 内存管理器
├── device.rs       # 设备接口层
├── config/         # 配置管理
├── registers/      # 寄存器定义
└── err.rs          # 错误类型定义
```

## 使用示例

### 1. 实现OSAL接口

```rust
use rknpu::*;


struct MyOsal {
    // 平台相关的实现
}


impl Osal for MyOsal {
    fn dma_alloc(&self, size: usize, flags: MemoryFlags) -> Result<MemoryBuffer, OsalError> {
        // 实现DMA内存分配
        todo!()
    }
    
    fn dma_free(&self, buffer: MemoryBuffer) -> Result<(), OsalError> {
        // 实现DMA内存释放
        todo!()
    }
    
    fn get_time_us(&self) -> TimeStamp {
        // 返回当前时间戳（微秒）
        todo!()
    }
    
    fn msleep(&self, ms: u32) {
        // 毫秒级睡眠
        todo!()
    }
    
    fn log_info(&self, msg: &str) {
        println!("[INFO] {}", msg);
    }
    
    // ... 其他OSAL接口实现
}

```

### 2. 初始化设备

```rust
use core::ptr::NonNull;
use alloc::vec;


// 创建OSAL实例
let osal = MyOsal::new();

// 配置RKNPU
let config = RknpuConfig::new(RknpuType::Rk3588);

// MMIO基地址（需要平台提供）
let base_addrs = vec![
    NonNull::new(0xfda40000 as *mut u8).unwrap(), // Core 0
    NonNull::new(0xfda50000 as *mut u8).unwrap(), // Core 1
    NonNull::new(0xfda60000 as *mut u8).unwrap(), // Core 2
];

// 创建设备实例
let mut device = RknpuDevice::new(base_addrs, config, osal)?;

// 初始化设备
device.initialize()?;
```

### 3. 内存管理

```rust
// 分配内存
let flags = NpuMemoryFlags {
    base_flags: MemoryFlags {
        cacheable: true,
        contiguous: true,
        zeroing: true,
        dma32: false,
    },
    iommu: true,
    sram: false,
    nbuf: false,
    secure: false,
    kernel_mapping: true,
    iova_alignment: false,
};

let mem_handle = device.memory_create(1024 * 1024, flags)?; // 1MB

// 获取内存地址
let virt_addr = device.get_memory_vaddr(mem_handle)?;
let dma_addr = device.get_memory_dma_addr(mem_handle)?;

// 同步内存
device.memory_sync(mem_handle, DmaSyncDirection::ToDevice)?;

// 释放内存
device.memory_destroy(mem_handle)?;

```

### 4. 任务提交

```rust
// 创建任务缓冲区
let task_buffer_handle = device.memory_create(4096, flags)?;

// 填充任务数据
let task_vaddr = device.get_memory_vaddr(task_buffer_handle)?;
unsafe {
    let task_ptr = task_vaddr.as_ptr() as *mut RknpuTask;
    (*task_ptr).regcmd_addr = 0x12345678;
    (*task_ptr).regcfg_amount = 100;
    (*task_ptr).int_mask = 0x1;
    // ... 其他任务参数
}

// 创建任务提交
let task_flags = TaskFlags {
    pc_mode: true,
    non_block: false,
    ping_pong: false,
};

let submission = device.create_task_submission(
    task_buffer_handle,
    0,     // task_start
    1,     // task_number
    5000,  // timeout_ms
    RKNPU_CORE0_MASK, // core_mask
    task_flags,
)?;

// 提交任务
let job_id = device.submit_task(submission)?;
println!("Task submitted with job ID: {}", job_id);

```

### 5. 中断处理

```rust
// 在中断服务程序中调用（由平台提供）
device.irq_handle(0)?; // 处理Core 0的中断

```

### 6. 设备控制

```rust
// 获取硬件版本
let mut hw_version = 0;
device.execute_action(DeviceAction::GetHwVersion, &mut hw_version)?;
println!("Hardware version: 0x{:x}", hw_version);

// 软件重置
let mut value = 0;
device.execute_action(DeviceAction::Reset, &mut value)?;

// 获取SRAM使用情况
let mut total_sram = 0;
let mut free_sram = 0;
device.execute_action(DeviceAction::GetTotalSramSize, &mut total_sram)?;
device.execute_action(DeviceAction::GetFreeSramSize, &mut free_sram)?;
println!("SRAM: {} KB total, {} KB free", total_sram / 1024, free_sram / 1024);

```

## 平台集成要点

### OSAL实现要求

1. **内存管理**: 实现DMA一致性内存分配/释放
2. **时间服务**: 提供微秒级时间戳和睡眠功能
3. **同步操作**: 实现内存同步（cache操作）
4. **日志输出**: 提供不同级别的日志输出

### 中断处理集成

```rust
// 在平台的中断服务程序中
extern "C" fn npu_irq_handler(core_index: usize) {
    // 获取设备实例（全局或通过参数传递）
    if let Some(ref mut device) = get_device_instance() {
        if let Err(e) = device.irq_handle(core_index) {
            // 处理错误
        }
    }
}

```

### 内存映射

平台需要提供：

- RKNPU寄存器的MMIO映射
- DMA一致性内存分配器
- 可选的SRAM和NBUF区域映射

## 特性对比

| 特性 | 内核驱动 | 最小化设备层 |
|------|----------|-------------|
| 设备管理 | Linux设备模型 | 直接硬件操作 |
| 内存管理 | DRM GEM/DMA Heap | OSAL抽象分配器 |
| 中断处理 | 内核IRQ子系统 | irq_handle接口 |
| 同步机制 | 内核等待队列 | 轮询+OSAL睡眠 |
| 错误处理 | Linux错误码 | 自定义错误类型 |
| 平台依赖 | Linux内核API | OSAL抽象接口 |

## 注意事项

1. **线程安全**: 设备实例需要外部同步保护
2. **内存对齐**: DMA内存需要满足硬件对齐要求
3. **中断时序**: 确保中断处理的及时性
4. **错误恢复**: 实现适当的错误恢复机制
5. **资源清理**: 确保资源的正确释放

## 移植指南

1. 实现目标平台的OSAL接口
2. 提供MMIO基地址映射
3. 集成中断处理机制
4. 测试内存分配和任务执行
5. 优化性能和稳定性

## GEM 分配配额

`GemPool::create()` 对 `MemCreate` 分配的连续 DMA 内存执行配额检查，用于处理 [#1762](https://github.com/rcore-os/tgoskits/issues/1762)。`Card1File` 已有独立打开文件的句柄表和关闭清理，本次在该边界保存 `GemOwner`，不增加进程级登记表。Rust 调用方创建独立的 `GemOwner::default()`，并将其传给 `create(&owner, &mut args)`；Starry 的 `dup` 和 `fork` 通过共享 `Card1File` 继续使用同一个账户。

### 分配限制

[src/gem.rs](src/gem.rs) 中的常量集中定义当前资源策略。大小按 DMA 页向上取整后计费，因此超过剩余页预算一个字节也会被拒绝。

| 常量 | 上限 | 约束对象 |
| --- | --- | --- |
| `MAX_ALLOCATION_BYTES` | 64 MiB | 单次连续分配 |
| `MAX_OWNER_BYTES` | 256 MiB | 一个打开文件创建且仍然存活的 DMA backing |
| `MAX_OWNER_OBJECTS` | 1024 | 一个打开文件创建且仍然存活的对象 |
| `MAX_DEVICE_BYTES` | 512 MiB | 一个 `GemPool` 的全部存活 DMA backing |
| `MAX_DEVICE_OBJECTS` | 4096 | 一个 `GemPool` 的全部存活对象 |

这些值是软件资源上限，不是 RK3588 的硬件限制或可分配内存保证。单次上限限制连续大块请求；文件上限允许一个任务组合多个缓冲区；设备上限限制反复打开设备后的总占用。调整时应一并评估模型工作集和平台共享 DMA 内存容量。即使尚未触及上限，底层分配器仍可拒绝请求。零长度和对齐溢出返回参数错误；超限和底层内存不足经 `ax-driver::rknpu::Error::NoMemory` 映射为 `ENOMEM`。ioctl 结构和正常请求的返回字段不变。

### 保活与回收

`OwnedGem` 先持有 `ContiguousArray`，随后持有 owner 和设备两份 `GemCharge`；字段析构顺序保证实际 DMA 内存先释放，再归还配额。`GemUsage::reserve()` 在调用分配器前取得额度，设备配额拒绝或 DMA 分配失败通过 RAII 回滚。原子计数只维护独立资源上限，对象发布仍由池的排他借用和既有设备锁保证；保守的并发拒绝不会造成超额分配。

只在 ioctl 入口检查大小无法阻止重复分配；只按句柄表长度计费则可通过 mmap 或 PRIME 导出后销毁句柄绕过。当前设计让 `buffer_retainer()` 返回整个 `OwnedGem` 的 `Arc`，映射、导出或驱动层导入保留 backing 时也保留原始账户。外部导入不创建新的 DMA backing，不重复收取 `MemCreate` 额度；本配额不承担外部 dma-heap 或导入句柄表的资源政策。Starry 现有 PRIME fd 解析范围不因此扩大。

`MemDestroy`、最后一次关闭文件及复制输出失败仍走原有清理流程。设备忙时的延迟销毁继续占用额度；复位失败后进入隔离状态的设备继续保留 backing 和额度，不能为了回收配额释放设备可能仍在访问的内存。映射和导出 fd 可比源文件活得更久，故关闭源文件并不保证此类内存立即回收。

### 验证入口

`gem::tests` 用可计数的 DMA 分配后端和较小账户上限验证真实 `GemPool` 的字节边界、对象边界、失败回滚及最后 retainer 回收，不需要耗尽实体内存。AArch64 用户态执行命令为 `cargo xtask cross-test --arch aarch64 --package rockchip-npu --lib gem::tests::`；标准库检查使用 `cargo xtask test`。

真实文件生命周期回归位于 [rknpu-resources](../../../test-suit/starryos/board-orangepi-5-plus/rknpu-resources/c/main.c)，通过 `cargo xtask starry test board --board orangepi-5-plus -c rknpu-resources` 执行。它以 4 KiB 分配达到对象配额，正常实现最多占用 16 MiB DMA，验证独立打开、共享描述符、映射及 PRIME 导出保活和最后关闭后的额度恢复。该检查需要空闲的 OrangePi-5-Plus，不能以宿主测试或 C 编译结果代替。
