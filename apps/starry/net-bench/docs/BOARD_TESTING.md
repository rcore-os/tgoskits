# SG2002 板载 net-bench 测试指南

## 概述

`board/` 目录提供了在真实的 LicheeRV-Nano-SG2002 开发板上运行 net-bench 网络性能测试的工具。与 QEMU 虚拟化路径不同，板载测试使用板子的 WiFi AP 模式作为数据面，PC 通过 paramiko SSH 作为控制面，iperf3 C/S 方向与 QEMU 路径相反：

```
QEMU 路径：  Host iperf3 -s  ←─ virtio ─→  Guest iperf3 -c (StarryOS)
板载路径：  PC   iperf3 -c  ←─ WiFi  ──→  Board iperf3 -s (StarryOS)
```

## 前置条件

### 硬件

- LicheeRV-Nano-SG2002 开发板（已烧录 StarryOS，含 AIC8800 WiFi 固件）
- PC 带 WiFi 网卡（用于连接板子 AP）
- USB-TTL 串口线（用于 U-Boot 加载 + 串口监控）

### 软件

| 组件 | 说明 |
|------|------|
| Python 3.11+ | 含 `tomllib` 标准库 |
| paramiko | `pip install paramiko` |
| iperf3 (host) | PC 侧 `iperf3 -c -J` |
| riscv64 静态 iperf3 | 部署到板子 `/tmp/iperf3`（需 iperf3 >= 3.10 以支持 `-s -1`） |

### 网络

- 板子上电后默认进入 WiFi AP 模式，SSID 和密码由 StarryOS WiFi 配置决定
- 板子 AP 默认 IP：`192.168.50.1`
- PC 连接板子 WiFi 后通常通过 DHCP 获取 `192.168.50.2`
- PC 防火墙需放行到 `192.168.50.0/24` 的流量

## 快速开始

### 1. 构建 StarryOS 内核

```bash
# 使用 net-bench 的 WiFi 构建配置
cargo xtask starry app board --test-case net-bench --board-config \
    apps/starry/net-bench/board-licheerv-nano-sg2002-wifi.toml
```

产出 FIT uImage，通过 U-Boot 串口加载到板子。

### 2. 连接板子 WiFi

板子启动后 PC 连接到板子的 WiFi AP，确认连通性：

```bash
ping 192.168.50.1
ssh root@192.168.50.1 uptime
```

### 3. 准备板子侧资产

将 riscv64 静态 iperf3 和 board-server.sh 部署到板子：

```bash
# 从 PC 侧
scp board/board-server.sh root@192.168.50.1:/tmp/
scp /path/to/iperf3-riscv64-static root@192.168.50.1:/tmp/iperf3
ssh root@192.168.50.1 chmod +x /tmp/iperf3 /tmp/board-server.sh
```

也可以让控制器自动部署（使用 `--deploy` 参数）。

### 4. 运行测试

```bash
cd apps/starry/net-bench

# 完整测试矩阵
python3 board/board-controller.py

# 仅单个测试
python3 board/board-controller.py --test tcp1

# 部署并运行（自动上传 board-server.sh）
python3 board/board-controller.py --deploy

# 跳过结果汇总
python3 board/board-controller.py --no-summary
```

### 5. 查看结果

```bash
# 原始输出
cat results/board-sg2002-*.txt

# 汇总报告（控制器默认自动运行）
python3 core/summarize.py results/board-sg2002-*.txt
```

## 配置文件

`board/board-config.toml` 控制所有板载测试参数：

```toml
[board]
ip = "192.168.50.1"          # 板子 IP
ssh_port = 22
ssh_user = "root"
# 认证方式二选一：
# ssh_password = "your-password"
# ssh_key_path = "~/.ssh/id_rsa"

[test]
iperf3_port = 5201           # iperf3 端口
duration = 5                 # 每迭代时长（秒）
warmup = 1                   # warmup 迭代数
iters = 5                    # 测量迭代数
window = "256K"              # TCP socket buffer（256MB 内存约束）
server_script = "/tmp/board-server.sh"

[test.matrix]
tcp1   = ""                  # TCP 单流 PC→Board
tcp4   = "-P 4 -w 128K"      # TCP 4 并发流
tcp1r  = "-R"                # TCP 单流 Board→PC
udp1g  = "-u -b 1G"          # UDP 大包 1Gbps
udp64  = "-u -l 64 -b 100M"  # UDP 64B 小包 PPS
```

## 目录结构

```
apps/starry/net-bench/
├── board/
│   ├── board-controller.py      # PC 侧 Python 主控
│   ├── board-config.toml        # 板子连接与测试参数
│   ├── board-server.sh          # 板子侧脚本（部署到 /tmp/）
│   └── summarize.py -> ../core/summarize.py
├── board-licheerv-nano-sg2002-wifi.toml  # xtask board config
├── build-riscv64gc-unknown-none-elf.toml # riscv64 build config
├── init.sh                      # 板子启动入口
└── core/
    └── summarize.py             # 结果汇总（板载输出格式兼容）
```

## 同步机制

板子侧 `iperf3 -s -1`（单次服务模式）是同步的核心原语。详情见 `www/net-bench-sg2002-board-final-plan.md` §2。

每次测试迭代：
1. PC 通过 SSH channel 启动 `board-server.sh`
2. 板子输出 `/proc/net/dev` before snapshot，然后 `iperf3 -s -1` 监听
3. 板子输出 `SERVER_READY` 标记
4. PC 收到标记后启动本地 `iperf3 -c`
5. 板子 server 处理完连接后自动退出，输出 `/proc/net/dev` after snapshot
6. PC 收集两端数据，拼装 NET_BENCH marker 块

## 内存约束

SG2002 仅有 256MB RAM。测试参数已做适配：

- 默认 TCP window 256KB（`-w 256K`），WiFi BDP 下足够
- 4 并发流每流限 128KB window（`-w 128K`），总计约 1MB socket buffer
- iperf3 server 模式内存远小于 client（无需多流 send buffer）

如果测试中遇到 OOM，可进一步降低 window 或减少并发流数。

## 结果格式兼容性

板载测试的输出与 QEMU 路径使用相同的 marker 协议：

```
NET_BENCH_BEGIN test=tcp1 iter=0 warmup=0
NET_STATS_BEGIN warmup=0
  wlan0: ...
NET_STATS_END
{...iperf3 -J JSON...}
NET_STATS_BEGIN warmup=0
  wlan0: ...
NET_STATS_END
NET_BENCH_END test=tcp1 iter=0
NET_BENCH_PASSED
```

`core/summarize.py` 可直接解析，L2 delta、协议开销分析、error/drop 报告均可用。

## 故障排查

| 症状 | 可能原因 | 检查方法 |
|------|---------|---------|
| SSH 连接失败 | 板子 WiFi AP 未就绪 | 串口检查 StarryOS boot log，确认 AIC8800 probe 成功 |
| `SERVER_READY` 超时 | iperf3 未部署或版本过旧 | `ssh root@192.168.50.1 "/tmp/iperf3 --version"` |
| iperf3 client 连接被拒绝 | 板子防火墙 / server 未监听 | `ssh root@192.168.50.1 "netstat -tlnp | grep 5201"` |
| L2 delta 全零 | StarryOS `/proc/net/dev` 计数器未更新 | 先验证：连续两次 `cat /proc/net/dev`，检查计数器是否递增 |
| 高方差 / NOISY 标记 | WiFi 环境干扰 | 记录 RSSI；增加 `iters`；在多时间段重复测试 |
| OOM / 进程被杀 | 内存不足 | 降低 `window` 值；只跑 tcp1；检查 `free` 输出 |
