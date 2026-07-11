<h1 align="center">ax-printk</h1>

<p align="center">用于内核日志的无锁多写者记录环形缓冲区</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-printk.svg)](https://crates.io/crates/ax-printk)
[![Docs.rs](https://docs.rs/ax-printk/badge.svg)](https://docs.rs/ax-printk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

[English](README.md) | 中文

# 简介

`ax-printk` 是一个面向变长日志记录的无锁、多写者环形缓冲区，作为 TGOSKits
组件集的一部分维护，供与 ArceOS、AxVisor 及相关底层系统软件集成的 Rust 项目使用。

它采用两个环：

- **描述符环**：定长记录数组，每条记录是一个 `state_var` 状态机加元数据（序列号、
  时间戳、优先级，以及数据块位置）；
- **文本数据环**：存放变长消息，写入时零拷贝。

全程无锁：写者通过 CAS 推进环头来预留记录，并经由描述符状态机发布；读者在拷贝后
重读 `state_var` 保持一致。这使得日志在 IRQ、NMI、panic 上下文中依然可用——这些
场景下用锁可能死锁。

每个实例自带几何参数（`count_bits` / `size_bits`），因此同一份代码可服务任意大小的
缓冲区，且多个独立实例可以并存。可定义静态实例以便在分配器就绪前使用；也可基于
调用方提供的缓冲区构造运行时实例。

> **注意：** 本 crate 使用 `unsafe` 与无锁内存序，投入使用前必须经过 SMP 压力测试
> 验证。

# 许可证

采用 [Apache-2.0](./LICENSE) 许可证。
