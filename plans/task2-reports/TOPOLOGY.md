# Task 2 网络拓扑（交卷）

> 对应赛题任务二 · Hypervisor 内生 L2 交换，不经宿主数据面。

## 拓扑图

```
┌─────────────────────────────────────────────────────────────┐
│                        AxVisor (Host)                        │
│  ┌──────────────┐    global vsw (MAC learn + ACL)    ┌──────┐│
│  │ Guest A      │◄────── VirtioNet port 1 ──────────►│Guest B│
│  │ Linux Alpine │         10.0.9.0/24                │initramfs│
│  │ 10.0.9.2     │                                    │10.0.9.3│
│  │ MAC ..:02    │                                    │MAC ..:03│
│  └──────────────┘                                    └──────┘│
└─────────────────────────────────────────────────────────────┘
         无 vsock / tap / 宿主转发（合规数据面）
```

## 地址与端口

| 项 | Guest A (Linux) | Guest B (Peer) |
|---|---|---|
| IP | 10.0.9.2/24 | 10.0.9.3/24 |
| MAC | 02:00:00:00:00:02 | 02:00:00:00:00:03 |
| vsw port | 1 | 2 |
| icpc UDP | 9527 | 9527 |
| ACL 白名单 | UDP 9527 + ARP | 同左 |
| ACL 拒绝示例 | UDP 12345 → Drop | sentinel 不应收到 |

## VM 配置

- Guest A：`os/axvisor/configs/vms/qemu/aarch64/linux-net-a.toml`（VirtioBlk + VirtioNet）
- Guest B：`linux-net-b.toml`（initramfs + VirtioNet）

## 数据路径

1. Guest A `sendto(10.0.9.3:9527)` → VirtioNet TX → vsw 学习 MAC → 转发至 Guest B 队列
2. cross-VM：`kick_peer_virtio_nets` 立即 peer RX DMA + GIC SPI 按 peer pCPU 投递
3. Guest B peer `recvfrom` → 业务处理 → `sendto` 响应
4. 非 icpc UDP（如 :12345）在 vsw `IcpcPortAcl` 处丢弃，不进入对端队列

## 验证用例映射

| 用例 | 验证点 |
|---|---|
| `virtio-net-loopback` | 单 Guest eth0 |
| `vsw-dual-guest` | 双 Guest L3 互通 + 明文 UDP |
| `icpc-smoke` | 三类 icpc 消息 |
| `icpc-bench` | RTT CSV + 吞吐 |
| `icpc-acl-deny` | 非授权 UDP 被拒 |
| `icpc-fault-inject` | ~50% icpc 丢包 + 停等重试 |
