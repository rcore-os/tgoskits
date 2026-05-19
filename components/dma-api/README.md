# DMA API

用于 Rust 的 DMA（直接内存访问）抽象 API，提供安全的 DMA 内存操作接口，适用于嵌入式和裸机环境。

## 目录

- [快速开始](#快速开始)
- [核心概念](#核心概念)
- [使用场景指南](#使用场景指南)
- [API 方法目录](#api-方法目录)
- [完整示例](#完整示例)
- [缓存同步详解](#缓存同步详解)
- [完整 API 参考](#完整-api-参考)

---

## 快速开始

### 1. 实现 `DmaOp` trait

首先需要为你的平台实现 `DmaOp` trait，提供底层 DMA 操作支持：

```rust,ignore
use dma_api::{DmaOp, DmaDirection, DmaHandle, DmaError};
use core::{alloc::Layout, ptr::NonNull, num::NonZeroUsize};

struct MyDmaImpl;

impl DmaOp for MyDmaImpl {
    fn page_size(&self) -> usize {
        4096 // 返回系统页大小
    }

    unsafe fn map_single(
        &self,
        dma_mask: u64,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<DmaHandle, DmaError> {
        // 实现虚拟地址到 DMA 地址的映射
        // 返回 DmaHandle
        todo!()
    }

    unsafe fn unmap_single(&self, handle: DmaHandle) {
        // 解除 DMA 映射
        todo!()
    }

    unsafe fn alloc_coherent(
        &self,
        dma_mask: u64,
        layout: Layout,
    ) -> Option<DmaHandle> {
        // 分配 DMA 一致性内存
        todo!()
    }

    unsafe fn dealloc_coherent(&self, handle: DmaHandle) {
        // 释放 DMA 内存
        todo!()
    }
}
```

### 2. 选择合适的 DMA 容器

根据你的使用场景选择：

| 需求 | 推荐容器 | 特点 |
|------|---------|------|
| 需要数组类型的 DMA 缓冲区 | `DArray<T>` | 自动缓存同步，固定大小 |
| 需要单个结构体的 DMA 缓冲区 | `DBox<T>` | 自动缓存同步，单个值 |
| 映射现有缓冲区用于 DMA | `SArrayPtr<T>` | 手动缓存同步，单个连续内存区域映射 |

---

## 核心概念

### DMA 传输方向 (`DmaDirection`)

DMA 操作有三种方向，决定了缓存同步的行为：

```rust,ignore
pub enum DmaDirection {
    ToDevice,       // CPU → 设备：CPU 写数据，设备读
    FromDevice,     // 设备 → CPU：设备写数据，CPU 读
    Bidirectional,  // 双向：CPU 和设备都可能读写
}
```

**选择指南**：
- **ToDevice**: 用于发送数据到设备（如网卡发送缓冲区）
- **FromDevice**: 用于接收设备数据（如网卡接收缓冲区）
- **Bidirectional**: 用于双向通信（如驱动程序与设备共享内存）

### 缓存同步

DMA 操作需要处理 CPU 缓存和内存之间的数据一致性：

| 操作 | ToDevice | FromDevice | Bidirectional |
|------|----------|------------|---------------|
| **写数据前** (`confirm_write`) | ✅ 刷新缓存 | ❌ 无操作 | ✅ 刷新缓存 |
| **读数据前** (`prepare_read`) | ❌ 无操作 | ✅ 使缓存失效 | ✅ 使缓存失效 |

**自动同步 vs 手动同步**：
- `DArray<T>` 和 `DBox<T>`：每次访问（`read`/`set`/`write`/`modify`）**自动**同步对应元素的缓存
- `SingleMap`：**不自动**同步，用户必须手动调用 `prepare_read_all()` 和 `confirm_write_all()`

### DMA 地址掩码 (`dma_mask`)

`dma_mask` 指定设备可寻址的地址范围：

- `0xFFFFFFFF` (32 位设备，最多 4GB)
- `0xFFFFFFFFFFFFFFFF` (64 位设备，全地址空间)
- 其他值根据设备硬件限制

---

## 使用场景指南

### 场景 1: DMA 数组缓冲区

**用途**：需要固定大小的数组类型 DMA 缓冲区

**推荐**：`DArray<T>`

**特点**：
- ✅ 自动缓存同步（每次 `read`/`set` 时）
- ✅ 固定大小，随机访问
- ✅ 类型安全

**示例**：
```rust,ignore,ignore
let device = DeviceDma::new(0xFFFFFFFF, &DMA_IMPL);

// 创建 100 个 u32 的 DMA 数组
let mut dma_array = device.array_zero_with_align::<u32>(100, 64, DmaDirection::FromDevice)
    .expect("Failed to allocate");

dma_array.set(0, 0x12345678);  // 写入（自动刷新缓存）
let value = dma_array.read(0); // 读取（自动失效缓存）

let dma_addr = dma_array.dma_addr(); // 获取 DMA 地址给硬件
```

**适用场景**：
- 网卡数据包缓冲区
- 音频采样缓冲区
- 图像帧缓冲区

---

### 场景 2: DMA 单值容器

**用途**：需要单个结构体的 DMA 缓冲区

**推荐**：`DBox<T>`

**特点**：
- ✅ 自动缓存同步（每次 `read`/`write`/`modify` 时）
- ✅ 适合配置寄存器、描述符等
- ✅ 类型安全

**示例**：
```rust,ignore,ignore
#[derive(Default)]
struct Descriptor {
    addr: u64,
    length: u32,
    flags: u32,
}

let mut dma_desc = device.box_zero_with_align::<Descriptor>(64, DmaDirection::ToDevice)
    .expect("Failed to allocate");

dma_desc.modify(|d| d.length = 4096); // 修改（自动刷新缓存）
let desc = dma_desc.read();           // 读取（自动失效缓存）
```

**适用场景**：
- DMA 描述符
- 设备配置结构
- 状态寄存器

---

### 场景 3: 映射现有缓冲区

**用途**：将已存在的缓冲区映射用于 DMA

**推荐**：`SingleMap`

**特点**：
- ⚠️ **手动**缓存同步
- ✅ 临时映射，RAII 自动解映射
- ✅ 适用于栈分配或静态缓冲区

**示例**：
```rust,ignore,ignore
let mut buffer = [0u8; 4096];

// 映射现有缓冲区
let mapping = device.map_single_array(&buffer, 64, DmaDirection::ToDevice)
    .expect("Mapping failed");

// ⚠️ 重要：使用前必须手动同步缓存
mapping.confirm_write_all();  // 将 CPU 数据刷到内存
// ... DMA 传输 ...
mapping.prepare_read_all();   // 使 CPU 缓存失效，准备接收设备数据

let dma_addr = mapping.dma_addr();

// 映射在离开作用域时自动解除（不会自动同步缓存）
```

**适用场景**：
- 临时 DMA 操作
- 栈上的小缓冲区
- 重用已分配的内存

---

## API 方法目录

### 📦 创建方法

| 方法 | 用途 | 返回类型 | 缓存同步 |
|------|------|----------|----------|
| [`array_zero<T>(len, dir)`](#device-zeros) | 创建默认对齐的 DMA 数组 | `DArray<T>` | 自动 |
| [`array_zero_with_align<T>(len, align, dir)`](#device-zeros) | 创建指定对齐的 DMA 数组 | `DArray<T>` | 自动 |
| [`box_zero<T>(align, dir)`](#device-zeros) | 创建默认对齐的 DMA Box | `DBox<T>` | 自动 |
| [`box_zero_with_align<T>(align, dir)`](#device-zeros) | 创建指定对齐的 DMA Box | `DBox<T>` | 自动 |
| [`map_single_array<T>(buff, align, dir)`](#device-maps) | 映射现有缓冲区 | `SArrayPtr<T>` | **手动** |

### 🔍 访问方法（DArray）

| 方法 | 用途 | 缓存同步 | 返回值 |
|------|------|----------|--------|
| [`read(index)`](#darray-access) | 读取元素 | 自动失效 | `Option<T>` |
| [`set(index, value)`](#darray-access) | 写入元素 | 自动刷新 | `()` |
| [`copy_from_slice(slice)`](#darray-access) | 批量复制 | 自动刷新整个范围 | `()` |

### 🔍 访问方法（DBox）

| 方法 | 用途 | 缓存同步 | 返回值 |
|------|------|----------|--------|
| [`read()`](#dbox-access) | 读取值 | 自动失效 | `T` |
| [`write(value)`](#dbox-access) | 写入值 | 自动刷新 | `()` |
| [`modify(f)`](#dbox-access) | 修改值（read-modify-write） | 先失效后刷新 | `()` |

### 🔄 同步方法（SingleMap）

| 方法 | 用途 | 适用方向 |
|------|------|----------|
| [`prepare_read_all()`](#singlemap-sync) | 使 CPU 缓存失效 | `FromDevice`, `Bidirectional` |
| [`confirm_write_all()`](#singlemap-sync) | 刷新 CPU 缓存到内存 | `ToDevice`, `Bidirectional` |

### 📊 信息方法

| 方法 | 用途 | 返回值 |
|------|------|--------|
| [`dma_addr()`](#info-methods) | 获取 DMA 地址 | `DmaAddr` |
| [`len()`](#info-methods) | 获取数组长度 | `usize` |

---

## 完整示例

### 示例 1: 网卡接收缓冲区

```rust,ignore,ignore
use dma_api::{DeviceDma, Direction};

// 创建 DMA 设备
let device = DeviceDma::new(0xFFFFFFFF, &DMA_IMPL);

// 分配接收缓冲区（1500 字节数据包）
let mut rx_buffer = device.array_zero_with_align::<u8>(1500, 64, DmaDirection::FromDevice)
    .expect("Failed to allocate RX buffer");

// 配置网卡使用这个 DMA 地址
let dma_addr = rx_buffer.dma_addr();
nic.set_rx_address(dma_addr.as_u64());

// ... 网卡接收数据 ...

// 读取数据（自动使 CPU 缓存失效）
let data = rx_buffer.read(0).unwrap();
```

### 示例 2: DMA 描述符配置

```rust,ignore,ignore
#[derive(Default)]
struct DmaDescriptor {
    buffer_addr: u64,
    length: u32,
    control: u32,
}

// 分配描述符
let mut desc = device.box_zero_with_align::<DmaDescriptor>(64, DmaDirection::ToDevice)
    .expect("Failed to allocate descriptor");

// 配置描述符（自动刷新缓存）
desc.write(DmaDescriptor {
    buffer_addr: 0x12345000,
    length: 4096,
    control: 0x01,
});

// 修改描述符（自动失效 → 修改 → 刷新）
desc.modify(|d| d.length = 2048);

// 获取 DMA 地址给硬件
let desc_addr = desc.dma_addr();
```

### 示例 3: 临时映射栈缓冲区

```rust,ignore,ignore
// 栈上的临时缓冲区
let mut temp_buf = [0u8; 256];

// 映射用于 DMA 写入
let mapping = device.map_single_array(&temp_buf, 64, DmaDirection::ToDevice)
    .expect("Failed to map");

// 准备数据写入设备
temp_buf[0] = 0xAA;
temp_buf[1] = 0xBB;

// ⚠️ 必须手动刷新缓存
mapping.confirm_write_all();

// ... 启动 DMA 传输 ...

// 映射在作用域结束时自动解映射
```

---

## 缓存同步详解

### 自动同步（DArray 和 DBox）

`DArray<T>` 和 `DBox<T>` 在以下操作时**自动**处理缓存同步：

| 操作 | 缓存同步行为 |
|------|--------------|
| `DArray::set(i, v)` | 写入后刷新单个元素 |
| `DArray::read(i)` | 读取前失效单个元素 |
| `DArray::copy_from_slice(s)` | 写入后刷新整个范围 |
| `DBox::write(v)` | 写入后刷新 |
| `DBox::read()` | 读取前失效 |
| `DBox::modify(f)` | 失效 → 执行闭包 → 刷新 |

**优点**：使用简单，不会出错
**缺点**：频繁同步可能影响性能

### 手动同步（SingleMap）

`SingleMap` **不会**在 Drop 时自动同步缓存，必须显式调用：

```rust,ignore,ignore
let mapping = device.map_single_array(&buffer, 64, DmaDirection::ToDevice)?;

// 写入前准备
mapping.confirm_write_all();  // 将 CPU 数据刷到内存

// ... DMA 传输 ...

// 读取前准备
mapping.prepare_read_all();   // 使 CPU 缓存失效

// Drop 时只会解除映射，不会自动同步
```

**为什么这样设计**：
- 与 Linux DMA API 语义一致
- 让用户精确控制缓存同步时机
- 避免不必要的缓存操作

### 缓存同步规则

根据 DMA 方向选择同步操作：

| DMA 方向 | 写入设备前 | 读取设备后 |
|----------|-----------|-----------|
| `ToDevice` | `confirm_write_all()` ✅ | ❌ 无需操作 |
| `FromDevice` | ❌ 无需操作 | `prepare_read_all()` ✅ |
| `Bidirectional` | `confirm_write_all()` ✅ | `prepare_read_all()` ✅ |

---

## 完整 API 参考

### 核心类型

#### `DeviceDma`

DMA 设备操作接口。

**构造函数**：
```rust,ignore
pub fn new(dma_mask: u64, osal: &impl DmaOp) -> Self
```
- **用途**：创建 DMA 设备实例
- **参数**：
  - `dma_mask`: 设备可寻址的地址掩码（如 `0xFFFFFFFF`）
  - `osal`: 实现 `DmaOp` trait 的操作系统抽象层
- **示例**：
```rust,ignore
let device = DeviceDma::new(0xFFFFFFFF, &DMA_IMPL);
```

#### <a name="device-zeros"></a>数组创建方法

```rust,ignore
pub fn array_zero<T: Sized + Default>(
    &self,
    len: usize,
    direction: DmaDirection,
) -> Result<DArray<T>, DmaError>

pub fn array_zero_with_align<T: Sized + Default>(
    &self,
    len: usize,
    align: usize,
    direction: DmaDirection,
) -> Result<DArray<T>, DmaError>
```
- **用途**：创建 DMA 可访问的数组，初始化为零
- **参数**：
  - `len`: 数组长度（元素个数）
  - `align`: 对齐字节数（`array_zero` 默认为 `core::mem::align_of::<T>()`）
  - `direction`: DMA 传输方向
- **返回**：`DArray<T>` 容器
- **缓存同步**：自动
- **示例**：
```rust,ignore
let array = device.array_zero_with_align::<u32>(100, 64, DmaDirection::ToDevice)?;
```

#### <a name="device-zeros"></a>Box 创建方法

```rust,ignore
pub fn box_zero<T: Sized + Default>(
    &self,
    direction: DmaDirection,
) -> Result<DBox<T>, DmaError>

pub fn box_zero_with_align<T: Sized + Default>(
    &self,
    align: usize,
    direction: DmaDirection,
) -> Result<DBox<T>, DmaError>
```
- **用途**：创建 DMA 可访问的单值容器，初始化为零
- **参数**：
  - `align`: 对齐字节数（`box_zero` 默认为 `core::mem::align_of::<T>()`）
  - `direction`: DMA 传输方向
- **返回**：`DBox<T>` 容器
- **缓存同步**：自动
- **示例**：
```rust,ignore
let box_val = device.box_zero_with_align::<MyStruct>(64, DmaDirection::Bidirectional)?;
```

#### <a name="device-maps"></a>映射方法

```rust,ignore
pub fn map_single_array<T: Sized>(
    &self,
    buff: &[T],
    align: usize,
    direction: DmaDirection,
) -> Result<SArrayPtr<T>, DmaError>
```
- **用途**：将现有缓冲区映射为 DMA 可访问
- **参数**：
  - `buff`: 要映射的缓冲区切片
  - `align`: 对齐字节数
  - `direction`: DMA 传输方向
- **返回**：`SArrayPtr<T>` 映射句柄
- **缓存同步**：**手动**（必须使用 `to_vec()` 和 `copy_from_slice()`）
- **示例**：
```rust,ignore
let buf = [0u8; 4096];
let mapping = device.map_single_array(&buf, 64, DmaDirection::ToDevice)?;
```

---

#### `DArray<T>`

DMA 可访问的数组容器，支持自动缓存同步。

##### <a name="darray-access"></a>访问方法

```rust,ignore
pub fn read(&self, index: usize) -> Option<T>
```
- **用途**：读取数组中指定索引的元素
- **缓存同步**：读取前自动失效
- **返回**：`Some(T)` 如果索引有效，否则 `None`

```rust,ignore
pub fn set(&mut self, index: usize, value: T) -> Option<()>
```
- **用途**：写入数组中指定索引的元素
- **缓存同步**：写入后自动刷新
- **返回**：`Some(())` 如果索引有效，否则 `None`

```rust,ignore
pub fn copy_from_slice(&mut self, src: &[T]) where T: Copy
```
- **用途**：从 slice 复制数据到数组
- **缓存同步**：写入后自动刷新整个范围
- **要求**：`src.len() <= self.len()`

##### <a name="info-methods"></a>信息方法

```rust,ignore
pub fn dma_addr(&self) -> DmaAddr
```
- **用途**：获取 DMA 地址，用于配置硬件
- **返回**：DMA 地址

```rust,ignore
pub fn len(&self) -> usize
```
- **用途**：获取数组长度
- **返回**：元素个数

---

#### `DBox<T>`

DMA 可访问的单值容器，支持自动缓存同步。

##### <a name="dbox-access"></a>访问方法

```rust,ignore
pub fn read(&self) -> T
```
- **用途**：读取存储的值
- **缓存同步**：读取前自动失效
- **返回**：存储的值

```rust,ignore
pub fn write(&mut self, value: T)
```
- **用途**：写入新值
- **缓存同步**：写入后自动刷新

```rust,ignore
pub fn modify<F: FnOnce(&mut T)>(&mut self, f: F)
```
- **用途**：修改值（read-modify-write 模式）
- **缓存同步**：读取前失效，写入后刷新
- **示例**：
```rust,ignore
dma_box.modify(|v| v.field += 10);
```

##### 信息方法

```rust,ignore
pub fn dma_addr(&self) -> DmaAddr
```
- **用途**：获取 DMA 地址，用于配置硬件
- **返回**：DMA 地址

---

#### `SArrayPtr<T>`

映射单个连续内存区域的 DMA 数组，RAII 风格自动清理。

**注意**：此类型提供手动缓存同步控制，与 `DArray<T>` 不同，它不会在每次访问时自动同步缓存。

##### 访问方法

```rust,ignore
pub fn read(&self, index: usize) -> Option<T>
```
- **用途**：读取指定索引的元素（**不自动**同步缓存）
- **返回**：`Some(T)` 如果索引有效，否则 `None`
- **注意**：读取前需手动使用 `to_vec()` 方法

```rust,ignore
pub fn set(&mut self, index: usize, value: T)
```
- **用途**：写入指定索引的元素（**不自动**同步缓存）
- **注意**：写入后需使用 `copy_from_slice()` 刷新缓存

```rust,ignore
pub fn copy_from_slice(&mut self, src: &[T])
```
- **用途**：从 slice 复制数据到数组
- **缓存同步**：写入后自动刷新整个范围

```rust,ignore
pub fn to_vec(&self) -> Vec<T>
```
- **用途**：将整个数组转换为 Vec
- **缓存同步**：读取前自动失效整个范围

##### 信息方法

```rust,ignore
pub fn dma_addr(&self) -> DmaAddr
```
- **用途**：获取 DMA 地址，用于配置硬件
- **返回**：DMA 地址

```rust,ignore
pub fn len(&self) -> usize
```
- **用途**：获取数组长度（元素个数）
- **返回**：元素个数

```rust,ignore
pub fn is_empty(&self) -> bool
```
- **用途**：检查数组是否为空
- **返回**：如果长度为 0 返回 `true`

---

### 类型定义

#### `DmaDirection`

DMA 传输方向枚举：

```rust,ignore
pub enum DmaDirection {
    ToDevice,       // DMA_TO_DEVICE: CPU 写入，设备读取
    FromDevice,     // DMA_FROM_DEVICE: 设备写入，CPU 读取
    Bidirectional,  // DMA_BIDIRECTIONAL: 双向传输
}
```

#### `DmaError`

DMA 操作错误类型：

```rust,ignore
pub enum DmaError {
    NoMemory,                           // DMA 分配失败
    LayoutError(LayoutError),           // 无效的内存布局
    DmaMaskNotMatch { addr, mask },     // DMA 地址超出设备掩码
    AlignMismatch { required, address }, // 地址对齐不满足要求
    NullPointer,                        // 提供了空指针
    ZeroSizedBuffer,                    // 零大小缓冲区不能用于 DMA
}
```

#### `DmaAddr`

DMA 地址类型：

```rust,ignore
pub struct DmaAddr(u64);

impl DmaAddr {
    pub fn as_u64(&self) -> u64;  // 转换为 u64
    pub fn checked_add(&self, rhs: u64) -> Option<Self>;  // 安全加法
}
```

---

### Linux 等价 API

| Rust API | Linux Equivalent |
|----------|------------------|
| `DeviceDma::map_single_array()` | `dma_map_single()` |
| `SArrayPtr<T>::drop()` | `dma_unmap_single()` |
| `DmaOp::alloc_coherent()` | `dma_alloc_coherent()` |
| `DmaOp::dealloc_coherent()` | `dma_free_coherent()` |
| `DmaOp::flush()` | `dma_cache_sync()` (DMA_TO_DEVICE) |
| `DmaOp::invalidate()` | `dma_cache_sync()` (DMA_FROM_DEVICE) |

---

### 对齐要求

DMA 操作通常需要对齐到特定的边界：

- 常见对齐值：64、128、256、512、4096
- `array_zero_with_align()` / `box_zero_with_align()` 的 `align` 参数指定对齐字节数
- `map_single_array()` 的 `align` 参数也指定对齐要求
- 确保返回的 DMA 地址满足对齐要求，否则返回 `DmaError::AlignMismatch`

### DMA Mask

`DeviceDma::new()` 的 `dma_mask` 参数指定设备可寻址的地址范围：

- `0xFFFFFFFF` (32 位设备，最多 4GB)
- `0xFFFFFFFFFFFFFFFF` (64 位设备，全地址空间)
- 其他值根据设备硬件限制
- 如果分配的 DMA 地址超出掩码范围，返回 `DmaError::DmaMaskNotMatch`
