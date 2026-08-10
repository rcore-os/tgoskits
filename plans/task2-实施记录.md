# Task 2 实施进度记录

> 智能化工控虚拟化擂台赛 · 任务二：基于 IP 网络的客户机间通信  
> 主文档：[os/axvisor/doc/task2-network.md](../os/axvisor/doc/task2-network.md)  
> 交卷状态：[plans/task2-reports/SUBMISSION-STATUS.md](task2-reports/SUBMISSION-STATUS.md)

---

## 进度总览

| 里程碑 | 状态 | 说明 |
|---|---|---|
| M2.0 基线调研 | ✅ | VirtioNet/VirtioBlk 工厂原均未实现 |
| M2.1 VirtioNet MMIO + 工厂 | ✅ | `axdevice/src/virtio_net/` + builtin 注册 |
| M2.2 单口 loopback 冒烟 | ✅ | `virtio-net-loopback` PASS |
| M2.3 vsw + MAC/ACL | ✅ | `SwitchPortBackend` + `IcpcPortAcl` |
| M2.4 双 Guest UDP 互通 | ✅ | `vsw-dual-guest` PASS |
| M2.5 icpc 协议库 | ✅ | `components/icpc` 单测 PASS |
| M2.6 Guest icpc 三类消息 | ✅ | `icpc-smoke` PASS |
| M2.7 可靠性 + 故障注入 | ✅ | `icpc-fault-inject` PASS |
| M2.8 交卷材料 | ✅ | bench / ACL / 拓扑 / 抓包说明 |

---

## 2026-07-21 阶段五（Guest icpc 三类消息）

### 交付

| 组件 | 路径 | 行为 |
|---|---|---|
| C wire 格式 | `scripts/task2/icpc-wire.{h,c}` | 与 `components/icpc` 24B 头 + CRC32 对齐 |
| Guest B peer | `scripts/task2/icpc-peer-server.c` | CTRL→STATE / ERROR→ACK / HEARTBEAT 回显；明文 echo 兼容 |
| Guest A client | `scripts/task2/icpc-smoke-client.c` | 三类业务 + 心跳 smoke |
| 测试 | `test-suit/axvisor/normal/icpc-smoke/` | ping 暖机后跑 `/usr/local/bin/icpc-smoke` |

### 验证

```text
icpc-smoke PASS — ICPC_CTRL_OK / ICPC_ERROR_OK / ICPC_HEARTBEAT_OK
vsw-dual-guest PASS（peer 明文 echo 兼容仍可用）
vsw-peer-initramfs PASS
```

### 命令

```bash
./scripts/task2/setup-icpc-guests.sh
cargo xtask axvisor test qemu --arch aarch64 -c icpc-smoke
```

---

## 2026-07-17 阶段四（双 Guest PASS）

Passthrough 下 idle Guest 不 VM-exit → cross-VM kick 立即 peer RX DMA + `set_pending_spi_on_cpu` 按 peer CPU 路由 SPI。详见上文历史。

---

## 2026-07-24 阶段六（可靠性 + 故障注入）

### 交付

| 组件 | 路径 | 行为 |
|---|---|---|
| Rust 可靠性 | `components/icpc/src/reliability.rs` | 停等 ACK、指数退避、心跳、dedup |
| NEED_ACK 标志 | `components/icpc/src/flags.rs` | `ICPC_FLAG_NEED_ACK = 0x01` |
| vsw 故障注入 | `virtualization/axdevice/src/virtio_net/vsw.rs` | 仅 icpc UDP 按 hash ~50% 丢包；ARP 放行 |
| axvisor feature | `os/axvisor` `vsw-fault-inject` | 启动时 `configure_vsw_fault_inject(2)` |
| Guest A 客户端 | `scripts/task2/icpc-reliability-client.c` | 停等 + 最多 20 次重试 |
| Guest B dedup | `scripts/task2/icpc-peer-server.c` | 32 槽 seq 去重，重复仍回包 |
| 测试 | `test-suit/axvisor/stress/icpc-fault-inject/` | 50% icpc 丢包下三类消息 PASS |

### 根因与修复

1. **全局帧计数器**：请求/响应交替计数导致响应 100% 被丢 → 改为按帧 hash 独立判定。
2. **ARP 被丢**：L2 无法建立 → 故障注入仅作用于 icpc UDP（9527）。
3. **recv_expect 无总超时**：错误包可导致 select 循环 → 加入 monotonic deadline。
4. **ping 暖机**：50% 全帧丢包下 ping 易失败 → 去掉 ping 门禁，由 icpc 重试承担。

### 验证

```text
icpc-fault-inject PASS — ICPC_RELIABILITY_CTRL/ERROR/HEARTBEAT ok retries=N
icpc-smoke PASS
vsw-dual-guest PASS
cargo test -p icpc PASS
cargo test -p axdevice fault_inject PASS
```

### 命令

```bash
./scripts/task2/setup-icpc-reliability.sh
./scripts/task2/run-icpc-fault-inject.sh
# 或
cargo xtask axvisor test qemu --arch aarch64 -g stress -c icpc-fault-inject
```

---

---

## 2026-07-24 阶段七（交卷收尾）

### 交付

| 组件 | 路径 | 行为 |
|---|---|---|
| icpc-bench | `scripts/task2/icpc-bench-client.c` | 20× HEARTBEAT RTT → CSV + P50/P99/吞吐 |
| ACL 拒绝探测 | `scripts/task2/icpc-acl-deny-client.c` | :12345 探测 + :9527 心跳对照 |
| Peer ACL sentinel | `icpc-peer-server.c` | :12345 收到则 `ICPC_ACL_LEAK` |
| vsw ACL 单测 | `vsw.rs` | 非 icpc UDP 不转发 |
| 测试 | `icpc-bench` / `icpc-acl-deny` | normal 组 PASS |
| 交卷文档 | `plans/task2-reports/` | TOPOLOGY / BENCH-SAMPLE / CAPTURE-NOTES |

### 验证

```text
icpc-bench PASS — ok=20 fail=0 p50_us=2122 msg_per_s≈332
icpc-acl-deny PASS — 无 ICPC_ACL_LEAK，icpc 心跳 OK
```

### 命令

```bash
./scripts/task2/run-icpc-bench.sh
./scripts/task2/run-icpc-acl-deny.sh
```
