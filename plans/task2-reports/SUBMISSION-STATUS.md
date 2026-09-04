# Task 2 交卷状态摘要

> 最后更新：2026-07-24  
> 主文档：[`os/axvisor/doc/task2-network.md`](../../os/axvisor/doc/task2-network.md)  
> 实施记录：[`../task2-实施记录.md`](../task2-实施记录.md)

## 已完成（可复现）

| 项 | 证据 |
|---|---|
| VirtioNet + vsw + ACL | `virtio-net-loopback` / `vsw-dual-guest` **PASS** |
| icpc 协议库 | `cargo test -p icpc` PASS |
| Guest 三类 icpc 消息 | `icpc-smoke` **PASS** |
| 可靠性 + 故障注入 | `icpc-fault-inject` **PASS** |
| 压测 CSV | `icpc-bench` **PASS**（见 [`BENCH-SAMPLE.md`](BENCH-SAMPLE.md)） |
| ACL 拒绝非授权 UDP | `icpc-acl-deny` **PASS** |
| 拓扑 / 抓包说明 | [`TOPOLOGY.md`](TOPOLOGY.md)、[`CAPTURE-NOTES.md`](CAPTURE-NOTES.md) |
| Peer initramfs | `vsw-peer-initramfs` **PASS** |
| cross-VM RX + SPI affinity | kick 路径 + `set_pending_spi_on_cpu` |

```bash
./scripts/task2/setup-icpc-guests.sh
./scripts/task2/setup-icpc-bench.sh
./scripts/task2/setup-icpc-acl-deny.sh
cargo test -p icpc
cargo xtask axvisor test qemu --arch aarch64 -c virtio-net-loopback
cargo xtask axvisor test qemu --arch aarch64 -c vsw-dual-guest
cargo xtask axvisor test qemu --arch aarch64 -c icpc-smoke
cargo xtask axvisor test qemu --arch aarch64 -c icpc-bench
cargo xtask axvisor test qemu --arch aarch64 -c icpc-acl-deny
cargo xtask axvisor test qemu --arch aarch64 -g stress -c icpc-fault-inject
```

## 评分对照（自评）

| 评分项 | 自评 |
|---|---|
| IP 链路建立且配置清楚 | ✅ |
| 应用层协议字段完整 | ✅ |
| 三类业务消息可用 | ✅ |
| 可靠性/超时/重连 | ✅ |
| 自动化测试数据充分 | ✅ 7 项 axvisor + icpc/axdevice 单测 |
| 网络隔离与访问控制 | ✅ ACL 单测 + `icpc-acl-deny` |

## 交卷材料索引

| 文档 | 路径 |
|---|---|
| 拓扑 | [`TOPOLOGY.md`](TOPOLOGY.md) |
| 压测样例 | [`BENCH-SAMPLE.md`](BENCH-SAMPLE.md) |
| 抓包/观测说明 | [`CAPTURE-NOTES.md`](CAPTURE-NOTES.md) |
