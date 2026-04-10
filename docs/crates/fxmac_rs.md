# `fxmac_rs` 技术文档

> 路径：`components/fxmac_rs`
> 类型：库 crate
> 分层：组件层 / 可复用基础组件
> 版本：`0.4.1`
> 文档依据：当前仓库源码、`Cargo.toml` 与 `components/fxmac_rs/README.md`

`fxmac_rs` 的核心定位是：FXMAC Ethernet driver in Rust for PhytiumPi (Phytium Pi) board, supporting DMA-based packet transmission and reception.

## 1. 架构设计分析
- 目录角色：可复用基础组件
- crate 形态：库 crate
- 工作区位置：根工作区
- feature 视角：主要通过 `debug` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `FXmac`、`FXmacConfig`、`FXmacQueue`、`FXmacBdRing`、`FXmacNetifBuffer`、`FXmacPhyInterface`、`FXmacBd`、`FXmacIntrHandler`、`FXMAC_HANDLER_DMASEND`、`FXMAC_HANDLER_DMARECV` 等（另有 1 个关键类型/对象）。
- 设计重心：该 crate 通常作为多个内核子系统共享的底层构件，重点在接口边界、数据结构和被上层复用的方式。

### 1.1 内部模块划分
- `fxmac_const`：FXMAC hardware register offsets and bit definitions. This module mirrors the low-level register layout from the FXMAC hardware specification and is primarily intended for internal…
- `fxmac`：Core FXMAC Ethernet controller functionality. This module provides the main data structures and functions for controlling the FXMAC Ethernet MAC controller
- `fxmac_dma`：DMA buffer descriptor management for FXMAC Ethernet. This module handles DMA-based packet transmission and reception, including buffer descriptor ring management
- `fxmac_intr`：Interrupt handling for FXMAC Ethernet controller. This module provides interrupt handlers and ISR setup functions for handling TX/RX completion, errors, and link status changes
- `fxmac_phy`：PHY management for FXMAC Ethernet controller. This module provides functions for PHY initialization, configuration, and management through the MDIO interface
- `utils`：Architecture helpers for FXMAC on supported targets. This module provides low-level helpers (CPU ID, barriers, cache ops) used by the driver on aarch64 platforms

### 1.2 核心算法/机制
- DMA 缓冲分配与地址映射

## 2. 核心功能说明
- 功能定位：FXMAC Ethernet driver in Rust for PhytiumPi (Phytium Pi) board, supporting DMA-based packet transmission and reception.
- 对外接口：从源码可见的主要公开入口包括 `FXMAC_RXBUFQX_SIZE_OFFSET`、`FXMAC_INTQX_IER_SIZE_OFFSET`、`FXMAC_INTQX_IDR_SIZE_OFFSET`、`FXMAC_QUEUE_REGISTER_OFFSET`、`BIT`、`GENMASK`、`read_reg`、`write_reg`、`FXmac`、`FXmacConfig` 等（另有 6 个公开入口）。
- 典型使用场景：作为共享基础设施被多个 OS 子系统复用，常见场景包括同步、内存管理、设备抽象、接口桥接和虚拟化基础能力。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `dma_alloc_coherent()` -> `xmac_init()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["fxmac_rs"]
    current --> ax_crate_interface["ax-crate-interface"]
    ax_driver_net["ax-driver-net"] --> current
```

### 3.1 直接与间接依赖
- `ax-crate-interface`

### 3.2 间接本地依赖
- 未检测到额外的间接本地依赖，或依赖深度主要停留在第一层。

### 3.3 被依赖情况
- `ax-driver-net`

### 3.4 间接被依赖情况
- `arceos-affinity`
- `arceos-display`
- `arceos-exception`
- `arceos-fs-shell`
- `arceos-irq`
- `arceos-memtest`
- `arceos-net-echoserver`
- `arceos-net-httpclient`
- `arceos-net-httpserver`
- `arceos-net-udpserver`
- `arceos-parallel`
- `arceos-priority`
- 另外还有 `34` 个同类项未在此展开

### 3.5 关键外部依赖
- `aarch64-cpu`
- `log`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
fxmac_rs = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# fxmac_rs = { path = "components/fxmac_rs" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`FXMAC_RXBUFQX_SIZE_OFFSET`、`FXMAC_INTQX_IER_SIZE_OFFSET`、`FXMAC_INTQX_IDR_SIZE_OFFSET`、`FXMAC_QUEUE_REGISTER_OFFSET`、`BIT`、`GENMASK`、`read_reg`、`write_reg` 等（另有 48 项）。
- 上下文/对象类型通常从 `FXmac`、`FXmacConfig`、`FXmacQueue`、`macb_dma_desc`、`FXmacBdRing`、`FXmacNetifBuffer` 等（另有 1 项） 等结构开始。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 存在单元测试/`#[cfg(test)]` 场景：`src/lib.rs`。

### 5.2 单元测试重点
- 建议用单元测试覆盖公开 API、错误分支、边界条件以及并发/内存安全相关不变量。

### 5.3 集成测试重点
- 建议补充被 ArceOS/StarryOS/Axvisor 消费时的最小集成路径，确保接口语义与 feature 组合稳定。

### 5.4 覆盖率要求
- 覆盖率建议：核心算法与错误路径达到高覆盖，关键数据结构和边界条件应实现接近完整覆盖。

## 6. 跨项目定位分析
### 6.1 ArceOS
`fxmac_rs` 主要通过 `arceos-affinity`、`arceos-display`、`arceos-exception`、`arceos-fs-shell`、`arceos-irq`、`arceos-memtest` 等（另有 34 项） 等上层 crate 被 ArceOS 间接复用，通常处于更底层的公共依赖层。

### 6.2 StarryOS
`fxmac_rs` 主要通过 `starry-kernel`、`starryos`、`starryos-test` 等上层 crate 被 StarryOS 间接复用，通常处于更底层的公共依赖层。

### 6.3 Axvisor
`fxmac_rs` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
