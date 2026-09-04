# Task 2：基于 IP 网络的客户机间通信 — 实施指南

> 对应赛题任务二（25 分）与 `plans/技术方案.md` §3.2、阶段三。
> 进度跟踪见 [`plans/task2-实施记录.md`](../../plans/task2-实施记录.md)。

---

## 1. 目标与阶段划分

| 阶段 | 内容 | 退出判据 |
|---|---|---|
| **阶段一** | VirtioNet MMIO 模拟 + 工厂注册 + 单口 loopback | Guest 探测 `virtio_net` / eth0；单机冒烟 PASS |
| **阶段二** | Hypervisor 内 `vsw`（MAC 学习 + 有界队列 + ACL） | 双 Guest UDP 互通；非 icpc 端口被拒 |
| **阶段三** | `components/icpc` 协议 + 双端适配 | CTRL_CMD / STATE_REPORT / ERROR_NOTIFY 可用 |
| **阶段四** | 可靠性（ACK/重传/心跳）+ 故障注入 + bench | `icpc-fault-inject` 有数据 |

**合规边界**：交付主通道为 Guest VirtioNet + Hypervisor 内 L2 交换；**不得**以 vsock / IVC / 宿主 tap 转发作为交付数据面。

---

## 2. 网络拓扑

```
Guest A (Linux)  10.0.9.2 / MAC 02:00:00:00:00:02
Guest B (RTOS)   10.0.9.3 / MAC 02:00:00:00:00:03
        \                    /
         \____ AxVisor vsw ____/
              (无宿主数据面)
```

| 项 | Guest A | Guest B |
|---|---|---|
| VM 配置 | `linux-smp2-net.toml`（规划） | `arceos-rt-smp1-net.toml` / RT-Thread |
| VirtioNet GPA | `0xa000000`（DTB 预留） | 独立 MMIO 窗口 |
| irq | DTB SPI（如 0x10） | 配置 `irq_id` |

---

## 3. 代码交付映射

| 交付物 | 路径 |
|---|---|
| VirtioNet 模拟 | `os/axvisor/src/virtio_net.rs` + `virtualization/axvirtio-net/` |
| 虚拟交换 | `virtualization/axvirtio-net/src/switch.rs` |
| 故障注入 | `axvirtio_net::switch::configure_fault_inject`，由 `vsw-fault-inject` 在 `main()` 打开 |
| icpc 协议库 | `components/icpc/` |
| 测试用例 | `virtio-net-loopback/`、`vsw-dual-guest/`、`icpc-smoke/`、`icpc-bench/`、`icpc-acl-deny/`、`stress/icpc-fault-inject/` |

启动、QEMU 命令和 RK3588 实板步骤见 [`operations-guide.md`](operations-guide.md)。

---

## 4. VirtioNet 设备约定

客户机网卡不再使用已删除的 `emu_devices` 表。当前 schema 在 `[devices]` 下声明虚拟设备，MAC 必须是非零单播地址；交换机端口号由运行时分配，不再写进 TOML。

```toml
[[devices.virtual]]
id = "virtnet0"
model = "virtio-net"
guest_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02]
```

`os/axvisor/src/virtio_net.rs` 把该设备接到全局 `VirtualSwitch`。MMIO 窗口固定在 `0x0a00_0000`、SPI 48；FDT 直通路径会排除与该 4K 页重叠的宿主 virtio-mmio 节点，避免虚拟网卡和宿主槽抢同一条 IRQ。

---

## 5. icpc 协议摘要

- 传输：UDP（主）；应用层 ACK/重传/心跳见技术方案 §3.2.3
- 头 24 字节：`ver|type|flags|rsvd|seq|timestamp_ns|payload_len|err_code|crc32`
- 类型：`CTRL_CMD(0x01)` / `STATE_REPORT(0x02)` / `ERROR_NOTIFY(0x03)` / `ACK(0x04)` / `HEARTBEAT(0x05)`

---

## 6. 验证命令（目标态）

```bash
cargo xtask axvisor test qemu --arch aarch64 -c virtio-net-loopback
cargo test -p icpc
./scripts/task2/setup-icpc-guests.sh
cargo xtask axvisor test qemu --arch aarch64 -c icpc-smoke
cargo xtask axvisor test qemu --arch aarch64 -c icpc-bench
cargo xtask axvisor test qemu --arch aarch64 -c icpc-acl-deny
cargo xtask axvisor test qemu --arch aarch64 -c vsw-dual-guest
cargo xtask axvisor test qemu --arch aarch64 -g stress -c icpc-fault-inject
```

---

## 7. 评分对照（自评跟踪）

| 评分项 | 分值 | 状态 |
|---|---:|---|
| IP 链路建立且配置清楚 | 4 | ✅ |
| 应用层协议字段完整 | 5 | ✅ |
| 三类业务消息可用 | 5 | ✅ |
| 可靠性/超时/重连 | 4 | ✅ |
| 自动化测试数据充分 | 4 | ✅ |
| 网络隔离与访问控制 | 3 | ✅ |
