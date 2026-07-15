<h1 align="center">axivc</h1>

<p align="center">AxVisor 客户机间通信的共享内存协议辅助库</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axivc.svg)](https://crates.io/crates/axivc)
[![Docs.rs](https://docs.rs/axivc/badge.svg)](https://docs.rs/axivc)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 介绍

`axivc` 是一个 `no_std` crate，提供 AxVisor 客户机间通信所需的共享内存协议辅助实现。它用于 AxVisor 已经把同一个 IVC channel 映射到两个客户机之后。

该 crate 负责客户机可见的内存协议和共享内存操作：

- 固定的共享内存区域头；
- 两个单生产者、单消费者消息环；
- 固定大小的请求和确认消息；
- 用于“IRQ 唤醒 + fallback polling”的对端事件计数器。

`axivc` 不直接发起 hypercall，不注册 IRQ handler，也不映射客户机物理地址。这些能力分别属于底层 ABI crate 和客户机 OS 集成层。

# 分层边界

IVC 栈按职责分为三层：

- `axhvc`：原始 guest-hypervisor ABI，包括 hypercall 编号、寄存器参数顺序、架构相关 trap 指令，以及 publish、subscribe、notify、unpublish 等低层封装。
- `axivc`：channel 已映射之后的架构无关共享内存协议布局和读写操作。
- 客户机 OS glue：hypercall 输出槽的虚实地址转换、GPA 映射、IRQ 注册、调度唤醒和应用策略。

完整客户机流程通常先用 `axhvc` 发布或订阅 channel，再通过客户机 OS 映射返回的 GPA，最后把映射后的内存作为 `axivc::IvcRegion` 使用。

# 协议布局

当前协议使用紧凑的单页格式：

- 前两个 `u64` 字段与 AxVisor host 侧 `IVCChannelHeader` 保持一致，分别是 publisher VM ID 和 channel key。
- `IvcRegion` 记录 magic、version、区域大小、feature flags 和 ring 偏移。
- 提供两个固定 slot 的 ring：publisher-to-subscriber 和 subscriber-to-publisher。
- 每个 slot 包含消息类型、序列号、payload 长度和固定 payload 缓冲区。

ring 协议使用 Release/Acquire 内存序。生产者写入 slot payload 后 release `tail`；消费者 acquire `tail` 后复制 slot，并 release `head` 归还所有权。

# 客户机使用流程

publisher 通常执行：

1. 调用 `axhvc::ivc::publish_channel`。
2. 映射返回的共享内存 GPA。
3. 使用 `IvcRegion::initialize` 初始化映射区域。
4. 使用 `IvcRegion::send_request` 发送请求。
5. 可选使用 `IvcRegion::try_recv_ack` 接收确认。
6. 可选通过 `axhvc` 通知对端。

subscriber 通常执行：

1. 调用 `axhvc::ivc::subscribe_channel`。
2. 映射返回的共享内存 GPA。
3. 校验 `channel_header_matches` 和 `protocol_header_matches`。
4. 使用 `IvcRegion::try_recv_request` 接收请求。
5. 可选使用 `IvcRegion::send_ack` 回复确认。
6. 可选通过 `axhvc` 通知对端。

对于需要等待消息的路径，客户机 IRQ 代码可以在 AxVisor 注入 notify IRQ 时调用 `record_peer_event`。接收路径再使用 `IvcPeerEventWaiter` 和 `fallback_poll`，组合 IRQ 唤醒和有界轮询，以覆盖中断丢失或 IRQ 尚未接好时的场景。

# 当前限制

- 区域布局面向当前 4 KiB AxVisor IVC channel。
- ring 是单生产者、单消费者。
- payload slot 大小固定。
- OS IRQ 注册和 hypervisor notify hypercall 不属于本 crate。
- 访问控制、配额和 channel 生命周期仍由 AxVisor 或客户机策略负责。

# 开发验证

使用 workspace 的 `xtask` 流程进行验证：

```bash
cargo fmt
cargo xtask clippy --package axivc
```

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
