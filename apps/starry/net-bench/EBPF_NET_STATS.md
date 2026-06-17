# eBPF net_stats 观测说明

本文档说明 `apps/starry/ebpf/net_stats` 的实现目标、观测语义、当前实现程度、适用场景、稳定性和准确性限制。

`net_stats` 的实现目标由 StarryOS 网络性能测试驱动：`apps/starry/net-bench` 主要用 `iperf3` 统计吞吐、PPS、UDP loss、TCP retransmits 等性能结果，而 `net_stats` 提供内核侧 ax-net socket send/recv 路径的辅助观测，用来判断 benchmark 期间 Starry 内核协议栈是否确实发生了 TCP/UDP 收发活动，以及收发字节量是否随 workload 变化。

它不是完整网络性能 benchmark，也不是网卡层、IP 层或 wire-level 统计工具。当前定位是 net-bench 的诊断辅助信号。

## 背景目标

`net-bench` 关注 StarryOS 网络性能基线，覆盖：

- TCP 单流上行：guest -> host
- TCP 4 并发流上行
- TCP 单流下行：host -> guest
- UDP 大包吞吐
- UDP 64B 小包 PPS

这些测试的主指标来自 `iperf3` JSON 输出，由 `apps/starry/net-bench/summarize.py` 汇总 mean/stddev。

`net_stats` 补充的目标是：

- 在 Starry guest 内用 eBPF kprobe/kretprobe 观测 ax-net socket 层收发路径。
- 输出 TCP/UDP tx/rx 的调用计数和 payload byte 计数。
- 以 `NET_STATS_BEGIN/NET_STATS_END` 包裹输出，便于 host 侧日志解析。
- 在网络性能测试出现异常时，帮助区分“iperf3/脚本/host 网络问题”和“guest 内核协议栈实际没有走到预期路径”。

## 当前实现位置

主要代码：

- `apps/starry/ebpf/net_stats/net_stats/src/main.rs`
- `apps/starry/ebpf/net_stats/net_stats-ebpf/src/main.rs`
- `apps/starry/ebpf/net_stats/net_stats-common/src/lib.rs`
- `apps/starry/ebpf/net_stats/prebuild.sh`
- `apps/starry/ebpf/net_stats/qemu-x86_64.toml`

net-bench 解析集成：

- `apps/starry/net-bench/summarize.py`

net-bench host 侧采样尝试：

- `apps/starry/net-bench/run.sh`

## 输出格式

`net_stats` 输出如下：

```text
NET_STATS_BEGIN
tcp_tx_pkts=<N>  tcp_tx_bytes=<N>
tcp_rx_pkts=<N>  tcp_rx_bytes=<N>
udp_tx_pkts=<N>  udp_tx_bytes=<N>
udp_rx_pkts=<N>  udp_rx_bytes=<N>
NET_STATS_END
```

字段含义：

- `tcp_tx_pkts`：TCP send entry probe 命中次数。
- `tcp_tx_bytes`：TCP send 成功返回的 payload byte 累计。
- `tcp_rx_pkts`：TCP recv entry probe 命中次数。
- `tcp_rx_bytes`：TCP recv 成功返回的 payload byte 累计。
- `udp_tx_pkts`：UDP send entry probe 命中次数。
- `udp_tx_bytes`：UDP send 成功返回的 payload byte 累计。
- `udp_rx_pkts`：UDP recv entry probe 命中次数。
- `udp_rx_bytes`：UDP recv 成功返回的 payload byte 累计。

注意：当前 `*_pkts` 实际语义更接近 socket send/recv 调用次数，不等价于真实网络包数。这个名字是为了沿用网络统计习惯，但在解释结果时必须按“调用次数”理解。

## eBPF 探针设计

当前 eBPF 程序使用 kprobe/kretprobe：

- entry kprobe：统计一次 send/recv 调用。
- return kretprobe：读取函数返回结果中的 byte count。

探测目标是 ax-net 的 socket 操作：

- `ax_net::tcp::TcpSocket::send`
- `ax_net::tcp::TcpSocket::recv`
- `ax_net::udp::UdpSocket::send`
- `ax_net::udp::UdpSocket::recv`

由于 Rust 泛型和 monomorphization，同一类 send/recv 可能对应多个实际符号。userspace loader 会读取 `/proc/kallsyms`，用 Rust v0 mangled symbol fragment 匹配所有相关符号，然后把同一个 eBPF program attach 到所有匹配项。

当前使用的 fragment 包括：

- TCP send：`6ax_net3tcp`, `9TcpSocket`, `9SocketOps4send`
- TCP recv：`6ax_net3tcp`, `9TcpSocket`, `9SocketOps4recv`
- UDP send：`6ax_net3udp`, `9UdpSocket`, `9SocketOps4send`
- UDP recv：`6ax_net3udp`, `9UdpSocket`, `9SocketOps4recv`

这样做的原因是 Starry `/proc/kallsyms` 暴露的是 Rust v0 mangled symbol，而不是源码中的完整路径字符串。

## 字节数读取方式

当前实现基于 x86_64 Starry QEMU 中观察到的 ABI 行为。

send 路径：

- `SocketOps::send` 返回 `AxResult<usize>`。
- 当前观察到 send 返回值通过 sret pointer 返回。
- kretprobe return site 中，`rax` 是指向返回结构的指针。
- 内存布局按当前观察为：
  - offset `+0`：`u64` discriminant，`0` 表示 `Ok`。
  - offset `+8`：`u64` payload，`Ok(bytes)` 的 byte count。
- eBPF 通过 `bpf_probe_read_kernel` 读取该结构，只在 `Ok` 时累计 bytes。

recv 路径：

- 当前 x86_64 Starry QEMU 中观察到 recv 成功返回 byte count 位于 `rdx`。
- aya x86_64 pt_regs 映射中，`rdx` 可通过 `ProbeContext::arg::<u64>(2)` 读取。
- 实现增加了 `MAX_IO_BYTES = 1 << 30` 过滤，避免把指针样的大值误计为 byte count。

这个读取方式能让当前 x86_64 QEMU 自测产生正确的非零 TCP/UDP byte counter，但它不是跨架构通用 ABI 抽象。

## userspace loader 行为

`net_stats` userspace 侧负责：

- 初始化日志。
- 读取 `/proc/kallsyms`。
- 解析所有匹配的 ax-net TCP/UDP send/recv 符号。
- 加载嵌入的 eBPF bytecode。
- attach kprobe/kretprobe 到全部匹配符号。
- 读取 `NETSTATS` Array map。
- 按 `NET_STATS_BEGIN/END` 输出快照。

支持模式：

- `--once`：attach 后立即输出一次快照并退出。
- `--test`：attach 后在 guest 内产生 TCP/UDP loopback 流量，等待短时间后输出快照并退出。
- 默认周期模式：按 `--interval` 周期输出快照，直到收到 Ctrl-C。

`--test` 的目的不是性能测试，而是验证 kprobe attach 和 byte counter 是否能在当前 Starry/QEMU 环境工作。

## net-bench 集成状态

`summarize.py` 已经能解析 `NET_STATS_BEGIN/END` block，并在 summary 中输出：

- before snapshot
- after snapshot
- delta snapshot

这部分解析逻辑已经具备。

但 `run.sh` 当前的采样集成还不完整。当前脚本逻辑是在 host 侧检查 `command -v net_stats`，然后执行：

```sh
timeout 6 net_stats --once
```

这有两个问题：

- 如果该命令在 host 上执行，它观测的是 host 环境，不是 Starry guest 内核。
- before 采样写入 `result_file` 后，后续 QEMU 输出使用 `tee "$result_file"`，会覆盖之前写入的内容。

因此，当前 `net_stats` app 本身可用，但 net-bench 自动 before/after guest eBPF delta 还不能认为已经正确集成。

要达到完整 net-bench 集成，应该保证：

- `/usr/bin/net_stats --once` 在 Starry guest 内执行。
- before 和 after 采样都进入同一个 QEMU/run log。
- host 侧 `summarize.py` 解析的是 guest 输出，而不是 host 命令输出。
- result log 写入方式不能覆盖已有采样内容。

## 当前验证结果

已完成的验证：

```sh
cargo xtask starry app qemu -t ebpf/net_stats --arch x86_64
```

结果：

- QEMU 能启动 Starry。
- `/proc/kallsyms` 能提供目标符号。
- loader 能解析 Rust v0 mangled ax-net send/recv 符号。
- kprobe/kretprobe 能成功 attach。
- `net_stats --test` 能产生 TCP/UDP loopback 流量。
- 输出能匹配 `NET_STATS_END` success regex。
- TCP/UDP tx/rx byte counter 均非零。

一次已验证输出中的关键值：

```text
tcp_tx_bytes=192
tcp_rx_bytes=256
udp_tx_bytes=88
udp_rx_bytes=88
```

格式化和静态检查：

```sh
cargo fmt --manifest-path apps/starry/ebpf/net_stats/net_stats/Cargo.toml
cargo fmt --manifest-path apps/starry/ebpf/net_stats/net_stats-ebpf/Cargo.toml
cargo clippy --manifest-path apps/starry/ebpf/net_stats/net_stats/Cargo.toml --all-targets
```

`cargo xtask clippy --package net_stats` 当前不可用，因为 `net_stats` 不是 workspace package。

测试中出现过的非致命日志：

- `bpf: unsupported command BPF_BTF_LOAD`
- `bpf: unsupported command BPF_LINK_CREATE`
- `bpf map type BPF_MAP_TYPE_CPUMAP not implemented`
- `bpf map type BPF_MAP_TYPE_DEVMAP not implemented`
- `failed to initialize eBPF logger: AYA_LOGS not found`

这些日志未阻止当前 kprobe/kretprobe 统计工作。

## 达到的观测要求

当前已经达到：

- 能观测 Starry x86_64 QEMU 中 ax-net TCP/UDP send/recv 活动。
- 能区分 TCP tx/rx 和 UDP tx/rx。
- 能输出 payload bytes。
- 能以 parseable marker 输出。
- 能通过自包含 `--test` 验证探针是否生效。
- 能作为网络性能测试中的辅助诊断信号。

当前尚未达到：

- 不能作为真实 packet count 统计。
- 不能作为 wire-level byte 统计。
- 不能替代 iperf3 性能结果。
- 不能保证 SMP 并发下完全准确。
- 不能保证跨架构 byte decode 正确。
- 不能认为 net-bench host 脚本已完整采集 guest eBPF before/after delta。

## 适用场景

适合使用：

- x86_64 Starry QEMU 中验证 eBPF kprobe/kretprobe 能否工作。
- net-bench 调试时确认 guest 内核 TCP/UDP socket 路径是否被触发。
- 判断某次 benchmark 是否完全没有产生预期方向的 TCP/UDP 活动。
- 比较同一构建、同一架构、同一 workload 下 counter 是否大致随流量变化。
- 辅助定位“iperf3 有输出但内核路径计数异常”或“脚本跑了但 guest 没有实际收发”的问题。

谨慎使用：

- 多核 SMP、高并发、多 stream 场景。
- 用 bytes 与 iperf3 throughput 做严格数值对齐。
- 用 `*_pkts` 推断真实包数或 PPS。
- 长时间运行后期待 counter 无溢出或无丢增量。

不适合使用：

- 作为正式性能结论的唯一依据。
- 作为协议栈包级精确统计。
- 作为网卡/virtio queue 层统计。
- 跨架构对比前不做 ABI 验证。
- 对 TCP 重传、分片、offload、队列行为做直接推断。

## 准确性评估

比较可靠的部分：

- 当前 x86_64 QEMU 自测中，TCP/UDP send/recv 活动能被探测到。
- 当前 x86_64 QEMU 自测中，send/recv byte counter 能变为合理的非零值。
- `NET_STATS_BEGIN/END` 输出格式稳定，便于日志解析。

不够可靠的部分：

- `*_pkts` 是函数调用次数，不是真实网络包数。
- bytes 是 socket API payload bytes，不包含 TCP/IP/Ethernet header。
- TCP 重传不会按 wire bytes 反映在这里。
- 失败的 send/recv 调用会增加 entry count，但不会增加 bytes。
- 非原子 `*slot += delta` 在并发场景可能丢计数。
- `recv` byte decode 依赖当前 x86_64 ABI 观察，不应直接推广到其他架构。

## 稳定性评估

当前工作稳定性：

- x86_64 Starry QEMU `--test` 场景表现稳定。
- eBPF loader 能找到符号并 attach。
- QEMU success regex 能匹配 `NET_STATS_END`。
- clippy 和 fmt 已通过针对性检查。

潜在不稳定来源：

- Rust v0 mangled symbol 会受编译器版本、crate 路径、泛型实例、优化和内联影响。
- 如果目标函数被内联、重命名或 monomorphization 变化，fragment 匹配可能失效或漏 attach。
- 如果 Starry `/proc/kallsyms` 输出变化，loader 可能找不到目标符号。
- 如果 ax-net `AxResult<usize>` ABI 或返回布局变化，byte decode 可能失效。
- SMP 场景下 map 更新不是原子操作。

## 实现成熟度分级

当前成熟度可以评为：实验性可用。

分项评价：

- eBPF PoC：可用。
- x86_64 Starry QEMU 自测：可用。
- net-bench 诊断辅助：核心 app 可用，自动集成仍需修正。
- 正式性能指标：不建议。
- 跨架构通用工具：尚未完成。
- SMP 精确统计：尚未完成。

## 推荐后续工作

优先级较高：

- 修正 `net-bench/run.sh` 的 guest 侧采样方式，确保 `net_stats --once` 在 Starry guest 中执行。
- 修正 result log 写入方式，避免 before snapshot 被 QEMU `tee` 覆盖。
- 在文档或输出中明确 `*_pkts` 是调用次数，或者改名为 `*_calls`。
- 使用 atomic add 或 Per-CPU map，降低 SMP 并发丢计数风险。

优先级中等：

- 为 aarch64、riscv64、loongarch64 分别验证 send/recv return ABI。
- 增加真实 net-bench workload 下的 guest 内 `net_stats` before/after delta 验证。
- 在 summary 中区分 eBPF snapshot 来源，避免 host/guest 混淆。
- 增加 attach 到多少个符号的调试输出，便于判断是否漏 attach。

优先级较低：

- 增加更多协议栈内部观测点，例如 IP 层或 virtio-net 层。
- 输出 JSON 格式，减少文本解析歧义。
- 为长期运行增加 counter overflow 说明或处理。

## 使用建议

推荐把 `net_stats` 解释为：

```text
Starry ax-net socket 层 TCP/UDP send/recv 辅助计数器。
```

不推荐解释为：

```text
Starry 网络包数/网卡吞吐/协议栈精确性能计数器。
```

在性能报告中，应把 `iperf3` mean/stddev 作为主结果，把 `net_stats` delta 作为辅助证据。例如：

- `iperf3` 显示吞吐正常，`net_stats` TCP tx/rx delta 非零：说明 guest socket 路径与 benchmark 结果大体一致。
- `iperf3` 无结果，`net_stats` 没有对应方向 delta：更可能是连接未建立、脚本未跑到 workload、guest 网络配置异常。
- `iperf3` 有结果但 `net_stats` 没有 delta：优先怀疑 eBPF attach、符号匹配、采样位置或 host/guest 混淆。
- `net_stats` bytes 与 `iperf3` bytes 不完全一致：这是预期现象，二者统计层级不同。
