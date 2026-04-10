# `axdriver_block` 技术文档

> 路径：`components/axdriver_crates/axdriver_block`
> 类型：库 crate
> 分层：组件层 / 块设备类别接口层
> 版本：`0.1.4-preview.3`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`src/ramdisk.rs`、`src/ramdisk_static.rs`、`src/sdmmc.rs`、`src/bcm2835sdhci.rs`、`os/arceos/modules/axdriver/src/drivers.rs`、`platform/axplat-dyn/src/drivers/blk/mod.rs`

`axdriver_block` 不是文件系统，也不是块缓存层。它的真实定位是 ArceOS 驱动栈里的块设备类别接口 crate：一方面定义统一的 `BlockDriverOps`，另一方面在 feature 打开时提供少量叶子块设备实现，例如 `ramdisk`、`sdmmc`、`bcm2835-sdhci` 和 `ahci`。上层 `ax-driver` 负责探测与聚合，`ax-fs`/`ax-fs-ng` 才是消费块设备并组织文件系统语义的地方。

## 1. 架构设计分析
### 1.1 设计定位
这个 crate 同时承担两类职责：

- **类别接口层**：通过 `BlockDriverOps` 统一块设备的容量、块大小、读写和刷盘语义。
- **可选叶子实现层**：通过 feature 暴露若干具体块设备实现。

因此它不是纯粹的“trait-only crate”，但也不是系统级块设备管理器。当前真实模块如下：

| 模块 | 启用条件 | 作用 |
| --- | --- | --- |
| `ramdisk` | `ramdisk` | 基于堆内存的 RAM 盘 |
| `ramdisk_static` | `ramdisk-static` | 基于静态切片的 RAM 盘 |
| `sdmmc` | `sdmmc` | 基于 `simple_sdmmc::SdMmc` 的 SD/MMC 驱动 |
| `bcm2835sdhci` | `bcm2835-sdhci` | Raspberry Pi 侧的 BCM2835 SDHCI 驱动 |
| `ahci` | `ahci` | AHCI 控制器驱动实现入口 |

### 1.2 核心接口
`BlockDriverOps` 继承 `BaseDriverOps`，定义了五个关键方法：

- `num_blocks()`：返回逻辑块总数。
- `block_size()`：返回单块大小。
- `read_block()`：从给定块号开始读取，可跨多块。
- `write_block()`：从给定块号开始写入，可跨多块。
- `flush()`：把待刷写数据提交到底层介质。

这里最重要的设计点是：**读写接口以“逻辑块设备”视角工作，而不是以文件、分区或页缓存视角工作。**

### 1.3 具体实现的行为差异
#### `ramdisk`
`ramdisk::RamDisk` 用 512 字节对齐的堆内存作为后端：

- `new(size_hint)` 会向上按 512 字节对齐。
- `read_block()` / `write_block()` 要求缓冲区长度是块大小整数倍。
- 超界访问返回 `DevError::Io`。
- `flush()` 为空操作，因为数据本来就在内存中。

#### `sdmmc`
`sdmmc::SdMmcDriver` 是对 `simple_sdmmc::SdMmc` 的薄封装：

- 构造函数是 `unsafe fn new(base: usize)`。
- 多块读写通过 `as_chunks()` / `as_chunks_mut()` 分块循环完成。
- 若缓冲区长度不是块大小整数倍，返回 `DevError::InvalidParam`。

#### `bcm2835sdhci`
`bcm2835sdhci::SDHCIDriver` 通过 `try_new()` 初始化控制器：

- 初始化失败直接映射为 `DevError::Io`。
- 读写要求缓冲区至少覆盖一个块，且需满足 `u32` 对齐要求。
- 将外部 `SDHCIError` 映射回统一的 `DevError`。

### 1.4 与 `ax-driver` 聚合层的接线关系
在当前仓库中，真正把这些实现接进系统初始化流程的是 `os/arceos/modules/axdriver/src/drivers.rs`：

- `ramdisk` 通过 `RamDiskDriver::probe_global()` 创建固定大小 16 MiB RAM 盘。
- `sdmmc` 通过 `SdMmcDriver::new()` 使用 `ax_config::devices::SDMMC_PADDR` 对应寄存器基址。
- `bcm2835-sdhci` 通过 `SDHCIDriver::try_new()` 接入。

需要特别注意一个实现事实：**`axdriver_block` 虽然有 `ahci` feature 和 `ahci` 模块，但当前 `ax-driver::drivers.rs` 并没有把 AHCI 探测逻辑注册进去。** 也就是说，打开 feature 并不等于当前 ArceOS 探测路径就会自动发现 AHCI 设备。

### 1.5 与动态平台路径的关系
在 `platform/axplat-dyn/src/drivers/blk/mod.rs` 中，`rd_block::Block` 会被包装成实现 `BlockDriverOps` 的 `Block`。这说明：

- `axdriver_block` 的核心价值首先是 trait 契约。
- 具体实现不一定非要写在本 crate 内，也可以由其它 crate 适配后满足 `BlockDriverOps`。

## 2. 核心功能说明
### 2.1 主要能力
- 提供统一的块设备 trait `BlockDriverOps`。
- 为 RAM 盘、SD/MMC、BCM2835 SDHCI、AHCI 等设备提供可选实现入口。
- 复用 `ax-driver-base` 的名称、类别和错误模型。
- 作为 `ax_driver_virtio::VirtIoBlkDev` 与 `platform/axplat-dyn` 动态块设备包装的共同契约。

### 2.2 典型调用链
当前仓库里最典型的使用主线是：

1. `ax_runtime::init_drivers()` 调用 `ax-driver::init_drivers()`。
2. `ax-driver` 根据 feature 选择 `ramdisk`、`sdmmc`、`bcm2835-sdhci` 或 `virtio-blk` 路径。
3. 设备实例被包装成 `AxBlockDevice` 放入 `AllDevices.block`。
4. `ax-fs` 或 `ax-fs-ng` 再接手这些块设备。

也就是说，本 crate 输出的是“可供上层消费的块设备实例”，而不是最终文件系统能力。

### 2.3 当前实现限制
- `ahci` 在本 crate 中存在，但当前 ArceOS 静态探测主线尚未接入。
- `flush()` 在多个实现里目前为空操作或恒成功，语义上更接近“同步点占位接口”。
- 各实现对缓冲区对齐、长度和块大小的要求不同，调用方不能假设所有块驱动都能接受任意字节数。

## 3. 依赖关系图谱
### 3.1 直接依赖
| 依赖 | 作用 |
| --- | --- |
| `ax-driver-base` | 统一设备元信息和错误类型 |
| `simple-sdmmc` | `sdmmc` 模块的底层控制器实现 |
| `simple-ahci` | `ahci` 模块的底层控制器实现 |
| `bcm2835-sdhci` | `bcm2835sdhci` 模块的底层控制器实现 |
| `log` | 初始化与错误日志 |

### 3.2 主要消费者
- `os/arceos/modules/axdriver`
- `components/axdriver_crates/axdriver_virtio`
- `platform/axplat-dyn`
- `os/arceos/modules/axfs`
- `os/arceos/modules/axfs-ng`

### 3.3 分层关系总结
- 向下依赖具体控制器库或内存后端。
- 向上输出统一的 `BlockDriverOps` 语义。
- 由 `ax-driver` 聚合层决定哪些设备真正进入系统。

## 4. 开发指南
### 4.1 何时应在这里扩展
适合放进 `axdriver_block` 的内容有两类：

- 一个能代表“块设备通用语义”的 trait 或辅助类型。
- 一个已经被 ArceOS 驱动栈广泛使用、适合以 feature 方式内建的叶子块设备实现。

如果某个块设备只在单一平台实验使用，也可以不放进这里，而是直接在外部 crate 实现 `BlockDriverOps`。

### 4.2 新增实现时必须同步检查的地方
1. `Cargo.toml` 的 feature 和可选依赖。
2. `os/arceos/modules/axdriver/src/drivers.rs` 是否真的注册了 probe 路径。
3. `ax-feat` 顶层 feature 是否需要把该驱动能力暴露给整机配置。
4. 读写接口对块大小、缓冲区长度和对齐的约束是否写清楚。

### 4.3 常见坑
- 不要把本 crate 写成“块设备子系统”；队列调度、页缓存、文件系统挂载都不在这里。
- 仅在本 crate 中加入一个模块，并不会自动进入 `ax-driver` 探测流程。
- `flush()` 语义在不同实现里强弱不同，不能想当然地把它当作硬件缓存落盘保证。

## 5. 测试策略
### 5.1 当前有效验证面
该 crate 没有独立的 `tests/` 目录，验证主要依赖：

- `ramdisk` 的内存读写行为。
- `ax-driver` 初始化阶段是否能真正注册块设备。
- `ax-fs` / `ax-fs-ng` 是否能基于这些设备完成挂载和读写。

### 5.2 建议补充的单元测试
- `ramdisk` 的块对齐、越界与多块读写。
- `sdmmc` 与 `bcm2835sdhci` 的参数检查和错误映射。
- `BlockDriverOps` 约定下 `num_blocks() * block_size()` 的边界一致性。

### 5.3 集成测试重点
- `virtio-blk`、`ramdisk`、`sdmmc` 至少各保留一条整机启动路径。
- 文件系统挂载和基础 I/O 回归。
- 若补齐 `ahci` 接线，应新增对应的总线探测和 BAR 配置验证。

### 5.4 风险点
- Feature 已声明但探测主线未接线，是当前最容易引入误判的地方。
- 块大小和缓冲区约束一旦处理错误，问题通常会在更上层文件系统中以数据损坏形式出现。

## 6. 跨项目定位分析
### 6.1 ArceOS
这是当前仓库里的主消费方。ArceOS 通过 `ax-driver` 和文件系统模块把它作为块设备类别契约和部分内建驱动实现使用。

### 6.2 StarryOS
StarryOS 在当前仓库中没有把 `axdriver_block` 当成独立块子系统直接使用；它更多是通过共享的 ArceOS 底层模块栈间接受益。

### 6.3 Axvisor
当前 Axvisor 代码主线更偏向 `rd_block` 与自身驱动体系，而不是直接依赖 `axdriver_block`。因此不应把本 crate 写成 Axvisor 的通用块设备框架。
