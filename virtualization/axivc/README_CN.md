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

`axivc` 是一个 `#![no_std]` 共享内存协议 crate，用于 AxVisor 已把同一
IVC channel 映射给 publisher 和 subscriber 之后。它提供：

- 两个互相独立的单生产者、单消费者方向；
- 通过 Release/Acquire 发布的固定大小 opaque cell ring；
- Message V1 编码、校验、分片、重组和中止；
- 支持消息大于整个 ring 的非阻塞 partial-progress API；
- 用于 IRQ 唤醒和有界 fallback polling 的对端事件辅助类型。

Hypercall、GPA 映射、IRQ 注册、阻塞等待、通知和应用协议仍属于客户机
OS glue。Request/Ack 类型和应用 sequence 应编码在 payload 中，不是传输层字段。

# 分层

```text
应用 payload（RPC、Request/Ack、文件块等）
IvcMessageSender / IvcMessageReceiver（Message V1 cells）
IvcRegion 中的 opaque SPSC rings
AxVisor HVC 映射和客户机 notify glue
```

ring 不解释 cell；Message frame 编解码也不依赖 hypercall 或具体 runtime。

# Message V1

每个 64 字节 cell 以手工 little-endian 编码的 24 字节 header 开始：

| Offset | 大小 | 字段 |
|---:|---:|---|
| `0x00` | 1 | version（`1`） |
| `0x01` | 1 | `FIRST`、`LAST`、`ABORT` flags |
| `0x02` | 2 | header 长度（`24`） |
| `0x04` | 4 | fragment 长度 |
| `0x08` | 8 | 传输层 message ID |
| `0x10` | 8 | 完整 payload 长度 |
| `0x18` | 最多 40 | fragment bytes |

同一消息的 frames 必须连续。sender 自动分配非零传输 ID、填写 flags 和长度，
并禁止消息交错。receiver 会拒绝未知 version/flags、非法长度、变化的 ID 或总长度，
以及不正确的 `LAST` 边界。

共享 region layout 版本为 **3**，与旧 v2 固定 Request/Ack cell 格式不兼容。
v2/v3 peer 会明确互拒；publish/subscribe/notify HVC ABI 保持不变。

# 非阻塞 API

先开始消息，再反复传入尚未消费的 input 后缀：

```rust
# use axivc::{IvcMessageError, IvcMessageSender};
fn send_step(
    sender: &mut IvcMessageSender<'_>,
    payload: &[u8],
    consumed: &mut usize,
) -> Result<bool, IvcMessageError> {
    let progress = sender.try_write(&payload[*consumed..])?;
    *consumed += progress.consumed();
    // published_cells() 非零时，客户机 glue 可以批量 notify 一次。
    Ok(progress.is_complete())
}
```

ring 满时，`try_write` 返回零进度并保留发送状态，调用方可以等待后重试。因此一条
消息可以同时大于单个 cell 和全部 16 个在途 cells。

receiver 可以在不消费 `FIRST` 的情况下检查不可信 metadata：

```rust
# use axivc::{IvcMessageError, IvcMessageReceiver};
fn receive_step(
    receiver: &mut IvcMessageReceiver<'_>,
    output: &mut [u8],
) -> Result<bool, IvcMessageError> {
    let progress = receiver.try_read(output)?;
    // 只把 output[..progress.written()] 追加到应用 sink。
    Ok(progress.is_complete())
}
```

一个 cell fragment 不会被拆开消费。如果第一个可用 fragment 放不进 output，API
返回 `BufferTooSmall`，且不推进 ring head。应用因资源策略拒绝消息时，可用
`try_discard` 释放其 cells。需要应用级原子可见性的调用方，应自行暂存流式输出，
直到观察到 `LAST` 再提交。

# 客户机流程

1. 通过 `axhvc` publish/subscribe，并映射返回的 GPA。
2. publisher 调用 `IvcRegion::initialize`。
3. subscriber 校验 `channel_header_matches` 和 `protocol_header_matches`。
4. 每个角色只调用一次 `publisher_endpoints` 或 `subscriber_endpoints`。
5. 用 `IvcEndpoints::into_parts` 拆分，并把 sender/receiver 移交给各自任务。
6. 发布 cells 或释放 ring capacity 后通知对端；无进度时等待或轮询。

unsafe attach 契约负责阻止重复 producer/consumer 在 `UnsafeCell` cell bytes 上发生竞争。

# 兼容性与限制

- 当前每个 HVC channel 只允许一个 publisher 和一个 subscriber，因为两个
  ring 都是单生产者、单消费者。多 peer 支持由
  [tgoskits#1238](https://github.com/rcore-os/tgoskits/issues/1238) 跟踪，后续需要
  引入版本化的 per-peer 内存布局。
- cell 为 64 字节，fragment capacity 为 40，ring capacity 为 16。
- Message V1 不支持消息交错、重传、乱序重排，也不进行分配。
- API 预留 `PeerReset`；当前 HVC backend 没有 queue generation，暂不会产生该错误。
- 外部 Linux `/root/axvisor.ko` companion 必须升级到 region v3 后，ArceOS↔Linux
  IVC 才兼容；该模块不在本仓库内。
- ivshmem、PCI BAR、doorbell、MSI-X 和 owner-RW/peer-RO section 不属于本次协议。

# 开发验证

使用 workspace 的 `xtask` 流程进行验证：

```bash
cargo fmt
cargo test -p axivc
cargo xtask clippy --package axivc
cargo xtask axvisor test qemu --arch aarch64 --test-case ivc-arceos2arceos
```

# 许可证

本项目采用 Apache License 2.0 许可证。详情见 [LICENSE](./LICENSE)。
