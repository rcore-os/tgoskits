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
