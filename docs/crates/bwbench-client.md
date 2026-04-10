# `bwbench-client` 技术文档

> 路径：`os/arceos/tools/bwbench_client`
> 类型：二进制 crate
> 分层：ArceOS 层 / 宿主机配套工具
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`README.md`、`src/main.rs`、`src/device.rs`、`os/arceos/modules/ax-net/src/smoltcp_impl/bench.rs`

`bwbench-client` 是一个运行在 Linux 宿主机上的原始以太网带宽对测工具。它通过 `AF_PACKET` raw socket 直接向指定网卡或 tap 设备发送/接收帧，并按秒输出吞吐统计。它不是 ArceOS 镜像里的应用，也不是可复用网络库；它的真实定位是给 ArceOS 网络基准路径提供一个宿主机侧对端。

最关键的边界是：`bwbench-client` 只负责在宿主机侧制造或接收原始以太网流量，用来观察链路吞吐。它不是 ArceOS 网络栈的一部分，也不是通用 benchmark 框架。

## 1. 架构设计分析

### 1.1 设计定位

从目录和源码看，`bwbench-client` 与仓库里的 HTTP 示例完全不同：

- 它运行在宿主机 `std` 环境，而不是 `no_std` ArceOS 应用环境
- 它不走 `ax-std`，也不直接依赖仓库内的网络栈
- 它直接使用 Linux raw socket 与 `ioctl`

因此，它不是“ArceOS 的一个网络示例”，而是“ArceOS 网络基准的宿主机配套工具”。

### 1.2 模块划分

| 模块 | 作用 |
| --- | --- |
| `src/main.rs` | 命令行入口、模式选择、吞吐统计与主循环 |
| `src/device.rs` | Linux raw socket 封装、接口绑定、MTU/MAC 查询、收发接口 |

整个工具的结构非常直接，重点在于以最薄的封装把宿主网卡原始收发能力暴露出来。

### 1.3 设备接入模型

`device.rs` 展示了它的真实工作方式：

- 使用 `libc::socket(AF_PACKET, SOCK_RAW | SOCK_NONBLOCK, ETH_P_ALL)`
- 通过 `SIOCGIFHWADDR` 读取接口 MAC
- 通过 `SIOCGIFINDEX` 获取接口索引并完成 `bind`
- 通过 `SIOCGIFMTU` 读取 MTU
- 使用 `send` / `recv` 直接收发原始帧

这说明它测量的不是 TCP/UDP 应用吞吐，而是更低层的二层帧收发吞吐。

### 1.4 发送与接收两种工作模式

`main.rs` 只提供两种模式：

- `Sender`：持续发送固定长度以太网帧
- `Receiver`：持续接收帧并累计字节数

两边都以每秒为窗口输出：

- 累计传输量（GBytes）
- 当前窗口带宽（Gbits/sec）

并在达到 `MAX_BYTES = 10 * GB` 后结束。

## 2. 核心功能说明

### 2.1 发送路径

发送模式会：

1. 创建设备对象并绑定接口
2. 构造一个 `STANDARD_MTU = 1500` 字节的固定缓冲区
3. 将第 `12..14` 字节写成 `0x0800`，即以太网 IPv4 EtherType
4. 持续调用 `dev.send(&tx_buf)`，直到累计发送 10GB 数据

这里没有构造真实的 IP/TCP/UDP 报文。它只是在发“带 IPv4 EtherType 的固定负载帧”，目的是测链路吞吐，而不是验证协议正确性。

### 2.2 接收路径

接收模式持续调用 `dev.recv(&mut rx_buffer)`，按秒打印吞吐统计。它同样不解析上层协议，只统计收到的总字节数。

### 2.3 与 ArceOS 侧基准的关系

`README.md` 明确把它描述成与 ArceOS 带宽基准配对使用的宿主工具，并给出了 tap 设备示例。当前仓库中，能直接对上的客体侧入口是：

- `ax-net::bench_transmit()`
- `ax-net::bench_receive()`

也就是说，这个工具真正的使命是充当这些基准入口的宿主机对端。

需要特别说明的是：`README.md` 里提到的 `make A=apps/net/bwbench ...` 来自更早期的应用组织方式；在当前仓库快照里，直接能对应上的仍是 `ax-net` 中的基准函数，而不是独立存在于同目录的 ArceOS 应用源码。

### 2.4 关键边界

- `bwbench-client` 不验证 HTTP、TCP、UDP 应用协议语义
- `bwbench-client` 不参与 ArceOS 镜像运行时装配
- `bwbench-client` 的统计结果主要反映原始链路吞吐，而不是完整应用栈吞吐

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `libc` | 调用 Linux raw socket、`ioctl`、`bind`、`send`、`recv` |
| `chrono` | 用于按秒统计吞吐 |

### 3.2 与仓库内其他模块的关系

`bwbench-client` 没有直接依赖仓库内其他 crate。它与仓库的关系主要体现在“测试配对”上：

- 宿主机侧：`bwbench-client`
- 客体侧：`ax-net::bench_transmit()` / `bench_receive()`

### 3.3 跨层关系

| 层次 | 角色 |
| --- | --- |
| `bwbench-client` | 宿主机 raw socket 基准工具 |
| Linux raw socket | 提供二层帧收发能力 |
| ArceOS `ax-net` 基准入口 | 提供客体侧发送/接收对端 |

## 4. 开发指南

### 4.1 运行方式

根据 `README.md`，最常见的运行方式是：

```bash
cargo build --release --manifest-path os/arceos/tools/bwbench_client/Cargo.toml
sudo ./target/release/bwbench_client [sender|receiver] <interface>
```

它通常需要 root 权限，因为 `AF_PACKET` raw socket 不是普通用户可直接使用的接口。

### 4.2 修改时的建议

1. 如果目标只是测链路吞吐，不要把它扩展成高层协议 benchmark。
2. 如果要改发送帧内容，先明确自己是在测 raw frame 吞吐，还是试图混入更高层协议开销。
3. 如果要改 `MAX_BYTES`、MTU 或时间统计逻辑，最好同步关注与 ArceOS 侧基准输出口径是否仍一致。

### 4.3 高风险点

- `WouldBlock` 被当作正常重试路径，其他错误则直接 panic；这说明它更像实验工具而不是健壮 CLI
- 设备实现只针对 Linux，其他平台直接返回 `"Not supported"`
- `ifreq`、`sockaddr_ll` 这类底层结构依赖 `unsafe` 与平台 ABI，修改时必须逐项核对

## 5. 测试策略

### 5.1 当前测试形态

没有独立单元测试。这个工具天然依赖真实宿主网络环境，因此验证方式主要是手工或脚本化端到端测试。

### 5.2 建议重点

- 至少分别跑一次 `sender` 和 `receiver` 模式
- 至少验证一条 tap 设备路径或真实网卡路径
- 若修改 `device.rs`，要重点检查 MAC、MTU、接口绑定与 `WouldBlock` 行为

### 5.3 更适合它的验证方式

最合理的验证组合是：

1. 宿主机启动 `bwbench-client`
2. 客体侧启动 ArceOS 的网络基准入口
3. 对照两边统计结果，观察收发是否匹配、吞吐是否稳定

## 6. 跨项目定位

### 6.1 ArceOS

虽然路径位于 `os/arceos/tools/` 下，但 `bwbench-client` 并不是跑在 ArceOS 里的程序，而是 ArceOS 网络基准的宿主机侧对端工具。

### 6.2 StarryOS

当前没有看到 StarryOS 直接使用这个工具的证据。它的 README、命名和对接方式都明显围绕 ArceOS 网络基准场景。

### 6.3 Axvisor

同样没有看到 Axvisor 与它的直接关系。它本质上是原始链路吞吐测试的配套工具，而不是通用虚拟化测试组件。
