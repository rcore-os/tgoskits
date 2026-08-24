# Task 2 双 Guest 网络最终设计与验收

> 本文是官方 `dev` 骨架上 Task 1–3 集成分支的 Task 2 canonical 摘要。
> 历史分支只作为证据来源；评审应以本文、
> `scripts/test/net-dual-guest/README.md` 和当前结果目录为准。

## 1. 目标与边界

Linux/Starry 侧与 RTOS 侧通过 UDP/IPv4 双向通信。共享内存、HyperCall、裸
MMIO 和 vsock 不作为主数据通道；QMP 只负责故障注入和退出。当前交付使用
AxVisor 内部 VirtIO-net L2 switch，QEMU 仅提供 AArch64 运行环境。

## 2. 当前拓扑

| 项目 | Linux/Starry 控制 Guest | RT-Thread 或 Zephyr Guest |
|---|---|---|
| VirtIO-mmio endpoint | `0x0a00_0000`，IRQ 48 | `0x0a00_0000`，IRQ 48（独立 VM stage-2） |
| MAC | `52:54:00:12:34:01` | `52:54:00:12:34:02` |
| IPv4 | `10.0.42.15/24` | `10.0.42.2/24` |
| UDP 服务 | `4242` | `4242` |
| 数据路径 | VM[1] VirtIO → AxVisor switch | VM[2] VirtIO → AxVisor switch |

每个 Guest 只拥有自己的 VirtIO 队列、DMA carveout、stage-2 映射和 vIRQ
路由。`manifest.toml`、`verify_fdt_devices.py` 和 `verify_isolation.py`
负责检查资源不相交及 route 证据。

## 3. T2N1 协议

每个 UDP datagram 使用固定 28 字节头：

```text
magic[4] version[1] kind[1] flags[2] session[4]
sequence[4] acknowledgement[4] payload_len[2] error_code[2] crc32[4]
```

支持五类消息：`CONTROL`、`STATUS`、`ACK`、`ERROR`、`HEARTBEAT`。CONTROL 和
STATUS 使用 stop-and-wait ACK、超时重传、重复抑制和乱序检测；Heartbeat
超时或重试耗尽进入 Safe，恢复后从 sequence 1 重新同步。

控制 payload 包含动作、参数和 request id；状态 payload 包含状态、标志、当前
值和最近 request id；错误 payload 使用明确的 error code，不把失败伪装成成功。

## 4. 验收证据

### 自动化检查

```bash
cargo test -p task2-net-protocol
cargo clippy -p task2-net-protocol --all-targets -- -D warnings
python3 -m unittest discover -s scripts/test/net-dual-guest -p 'test_*.py'
python3 scripts/test/net-dual-guest/validate_manifest.py \
  scripts/test/net-dual-guest/manifest.toml
```

当前集成工作树实测：协议 21 个 Rust 测试、Starry 控制端 11 个 Rust 测试和
网络工具 33 个 Python 回归全部通过。RT-Thread 与 Zephyr 端还共享以下约束：
陌生来源不刷新 liveness，合法 `ERROR` 进入 Safe，发送失败记录 Safe 后
fail-stop，`Stop/Reset` 返回对应的 `Stopped/Active STATUS`。

### 运行时检查

```bash
python3 scripts/test/net-dual-guest/verify_pcap.py \
  <linux.pcap> <rtos.pcap> --port 4242 --require-task2
python3 scripts/test/net-dual-guest/verify_isolation.py \
  <axvisor.log> scripts/test/net-dual-guest/manifest.toml
```

Task 3 当前 switch 结果目录保存了双侧 pcap、run.log、manifest 和完整 T2N1
ledger；故障目录保存了 blackout、Safe、恢复及续跑证据。历史 Task 2 长稳运行
记录显示约 1 小时运行中双向 heartbeat、CONTROL/STATUS/ACK 账本一致，未出现
非预期 Safe、协议错误或发送错误。

## 5. 当前限制与交付措辞

- 当前主数据面是 UDP/IP；不能写成“共享内存通信”。
- QEMU TCG 的吞吐和延迟是 SIL 相对数据，不是物理板硬实时保证。
- session-mismatch ERROR 已定义为语义层拒绝而非 malformed：陌生 session 的
  Heartbeat 返回本地 session 的 `ERROR(SessionMismatch)`，其
  `acknowledgement=0`；陌生可靠帧则关联被拒绝的 sequence。Rust、Python
  responder 和 Zephyr parser 共用该语义，正常 CONTROL/STATUS、ACK、重传、
  responder、RT-Thread 和 Zephyr parser 共用该语义，正常 CONTROL/STATUS、
  ACK、重传、乱序和链路恢复已经闭环。
- 协议/controller contract 已由 `scripts/test/net-dual-guest/run-ci-regression.sh`
  接入默认 CI；完整双 Guest QEMU 流程仍由显式脚本驱动，提交材料中明确给出
  命令、成功标记和失败退出码。
