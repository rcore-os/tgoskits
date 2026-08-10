# Task 2 抓包说明（交卷）

## 为何无宿主侧 pcap

Task 2 数据面完全在 AxVisor 内 VirtioNet + vsw 完成，**不经过宿主 tap/bridge**，因此无法在 Linux 宿主上直接 tcpdump 双 Guest 间流量。

## 可提交的观测证据

### 1. Hypervisor 日志

启用 `Info`/`Debug` 时可看到：

- `vsw forward UDP …`（`axdevice::virtio_net::vsw`）
- `vsw fault inject: drop …`（`vsw-fault-inject` feature）
- cross-VM kick / SPI 路由相关日志（`axvm`）

### 2. Guest 串口输出（QEMU `-nographic`）

| 标记 | 含义 |
|---|---|
| `ICPC_PEER_CTRL` / `ICPC_PEER_STATE` | B 端收到 CTRL 并回 STATE |
| `ICPC_BENCH_CSV` / `ICPC_BENCH_SUMMARY` | A 端压测 CSV 与汇总 |
| `ICPC_ACL_DENY_SENT` | A 端向 :12345 发探测 |
| `ICPC_ACL_LEAK` | **不应出现**（ACL 失效） |
| `ICPC_RELIABILITY_* ok retries=N` | 故障注入下重试成功 |

### 3. 复现命令（保存完整串口日志）

```bash
./scripts/task2/run-icpc-smoke.sh 2>&1 | tee plans/task2-reports/logs/icpc-smoke.log
./scripts/task2/run-icpc-bench.sh 2>&1 | tee plans/task2-reports/logs/icpc-bench.log
./scripts/task2/run-icpc-acl-deny.sh 2>&1 | tee plans/task2-reports/logs/icpc-acl-deny.log
./scripts/task2/run-icpc-fault-inject.sh 2>&1 | tee plans/task2-reports/logs/icpc-fault-inject.log
```

### 4. icpc 线格式（等价于“协议抓包”）

24 字节小端头 + payload + CRC32，见 `components/icpc/src/header.rs` 与 `scripts/task2/icpc-wire.h`。

字段：`version|type|flags|seq|timestamp_ns|payload_len|err_code|crc32`

## 若需 Guest 内 tcpdump

Alpine rootfs 可在 shell 中：

```sh
apk add tcpdump  # 若镜像含 apk
tcpdump -i eth0 -n udp port 9527 -c 10
```

仅抓取本 Guest 网卡可见帧；cross-VM 交换发生在 hypervisor，Guest 内 dump 仍可用于验证本端收发时序。
