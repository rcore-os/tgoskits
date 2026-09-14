# StarryOS LTP 网络请求响应基准

## 1. 测量范围

这个手动应用直接运行 LTP 20260529 的 `netstress`，测量 StarryOS 回环网络上的请求响应与连接建立成本。`ltp-netstress.sh` 只负责启动上游服务端、等待就绪、运行客户端、保存结果和回收自己的进程组；不复制 LTP 的 socket 测试实现。

### 1.1 与已有测试的关系

PR #1417 的 TCP 单流、多流和反向吞吐场景与现有 [`iperf3`](../../iperf3/README.md) 重叠，继续使用该应用。板卡 `native-network-smoke` 和 LTP 系统调用套件继续承担功能回归。这里补充现有应用缺少的请求响应性能场景，默认应用发现通过 `apps/.ignore` 排除它，必须显式选择。

| 场景 | 复用入口 | 结果含义 |
| --- | --- | --- |
| TCP 连续流单向、双向、多流吞吐 | 现有 `apps/starry/iperf3` | 接收端 Mbps |
| TCP/UDP 小消息、并行请求响应 | 本应用的 `tcp-rr`、`udp-rr` 及 parallel 场景 | 固定请求数的总耗时 |
| TCP 大消息请求响应 | `tcp-large`，双方消息均为 16384 字节 | 固定工作负载耗时 |
| TCP 每次请求重新建立连接 | `tcp-connect-rr`，上游服务端 `-R 1` | 包含连接建立与关闭的耗时 |

这里的 UDP 是请求响应，不能替代 iperf 的定速 UDP 灌流、丢包率或线速 PPS。上游 UDP 客户端允许有界超时，因此不把请求数除以耗时包装成实际收包 PPS。回环测试也不经过物理网卡、TAP、vhost、SDIO 或 Wi-Fi，不能用来评价这些驱动的性能。

### 1.2 上游契约

固定源码为 [LTP 20260529 netstress.c](https://github.com/linux-test-project/ltp/blob/20260529/testcases/network/netstress/netstress.c)。`client_fn()` 发送请求并等待响应，`client_run()` 用 `CLOCK_MONOTONIC_RAW` 统计总毫秒数，`-c` 保存结果；`-a` 控制客户端线程数，`-n/-N` 控制消息大小，`-R` 控制每条连接的请求数。

PR 中独立的 `net_stats` 计数与其 `netmon` 重叠；常规接口收发统计已有 `/proc/net/dev`。本应用不引入这两套 loader。设备路径时延观测有独立诊断价值，但需要实际设备工作负载和明确探针契约，不能以 loopback 自测宣称已经验证设备路径。

## 2. 手动执行

目前仅提供 x86_64、四虚拟 CPU、512 MiB、MTTCG 配置。`prebuild.sh` 校验 LTP 发布包 SHA-256，在 Alpine 3.23 容器中只编译未修改的 `netstress` 及其 `libltp` 依赖，静态链接 musl，再通过应用 overlay 安装到 guest。

### 2.1 环境与冒烟

宿主机需要可访问的 Docker daemon、curl、sha256sum 和项目正常的 x86_64 QEMU/UEFI 环境。首次执行需要访问 GitHub release 和 Alpine 软件源。当前托管 rootfs 未包含 `netstress`，所以不能只依赖 `/opt/ltp/Version` 存在。源码及构建产物位于 `target/ltp-netstress`；guest 使用 `/usr/bin/ltp-netstress` 和独立版本文件。

```bash
cargo xtask starry app qemu -t qemu/ltp-netstress --arch x86_64
```

`smoke` 模式每个客户端发送 20 次请求，每场景执行一次，用于验证构建、安装、启动、六个工作负载和结果传递。短样本可能出现零毫秒，不应据此计算速率。

### 2.2 性能采样

性能配置每个客户端发送 1000 次请求，各场景先预热一次，再测量三次。上游毫秒数包含客户端线程创建、请求响应及线程回收；`run_case()` 输出原始样本和中位数，不设置机器无关的性能通过阈值。

```bash
set -o pipefail
cargo xtask starry app qemu -t qemu/ltp-netstress --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml \
  2>&1 | tee /tmp/ltp-netstress-benchmark.log
```

比较不同提交时保持 CPU 数、QEMU 版本、加速器和宿主负载一致，保留命令、提交散列值和日志。MTTCG 数据用于趋势比较；它不是板卡网卡性能或低噪声硬件延迟基线。

## 3. 结果与失败

`run_sample()` 同时要求客户端退出成功、出现 `TPASS: test completed`、没有 `TFAIL/TBROK/TCONF/TWARN`，且耗时文件有效。任一失败包括预热失败都会终止应用，输出 `LTP_NETSTRESS_APP_FAILED` 并使 xtask 失败。上游服务端在客户端发送终止消息后可能报告 `TBROK: Server closed`，该服务端退出协议不冒充客户端测试失败。

每次运行在 guest 的 `/tmp/ltp-netstress.XXXXXX` 保留原始日志和样本；目录路径出现在 `LTP_NETSTRESS_ENV` 中。默认磁盘写入会随 QEMU 退出丢弃，长期记录应保存宿主串口输出。`LTP_NETSTRESS_SAMPLE` 表示单次耗时，`LTP_NETSTRESS_RESULT` 表示中位数，所有场景完成后才打印唯一的 `LTP_NETSTRESS_APP_PASSED`。本应用维护者需在发现异常数据时先核对原始日志和环境，再定位对应 LTP 工作负载；成功标记仅表示工作负载完成，不表示性能达标。

## 4. 当前验证记录

2026-09-14，以 `dev` 提交 `51c2d5077938e535ea9b8a5ec468f33dbd3de765` 的内核运行本应用：x86_64 QEMU 六个 smoke 场景通过，同一静态程序和脚本在 Linux 容器中的 smoke 也通过。限制文件描述符的故障注入使脚本退出 1，输出失败标记且没有总成功标记。

完整 bench 尚未通过：前五项完成三轮测量，`udp-rr-parallel` 完成预热后出现 `ARCEOS_PANIC_EMERGENCY`，位置为 `components/ax-task/src/sync/mutex/mod.rs:586`，消息为 `validate PI mutex blocking context failed: operation requires a scheduler safe point`。xtask 非零退出。该记录定位了失败阶段，尚未确定触发的完整调用链；不能据此把所有 PI mutex 或 UDP 路径判为不正确。保留该工作负载用于后续定位，当前不能把完整 bench 当作已通过的性能基线。
