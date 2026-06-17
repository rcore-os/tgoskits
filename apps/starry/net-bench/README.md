# apps/starry/net-bench — StarryOS 网络 iperf3 smoke 基线

当前目录提供最小可复现的 StarryOS `iperf3` 网络 smoke/baseline workflow，
用于确认 guest 网络、rootfs 资源和 host runner 能跑通。这里的 SLIRP/TAP 数据不是
完整性能 benchmark 结论；正式性能对比仍需 QEMU+TAP+vhost、同拓扑 Linux 基线、
多次重复统计和环境指纹记录。

## 目录结构

```
apps/starry/net-bench/
├── README.md                         本文件
├── run.sh                            一键启动测试（host 侧 iperf3 server + xtask）
├── qemu-aarch64.toml                 usermode (SLIRP) smp=1 配置
├── qemu-aarch64-smp4.toml            usermode (SLIRP) smp=4 配置
├── qemu-aarch64-tap.toml             TAP 配置，使用 tap0 和固定 MAC
├── net-bench.sh                      guest 侧 iperf3 client 脚本（SLIRP，目标 10.0.2.2）
├── net-bench-tap.sh                  guest 侧 iperf3 client 脚本（TAP，目标 192.168.100.1）
├── prebuild.sh                       构建前置（拉取 iperf3 等资源）
├── build-aarch64-unknown-none-softfloat.toml
└── results/                          测试结果（.txt / .log）
```

## 前置条件

1. host 上需安装 `iperf3`：

   ```sh
   sudo apt-get install -y iperf3
   ```

2. SLIRP 场景下，iperf3 server 必须监听所有接口（`0.0.0.0`），不要用 `-B`
   绑定到单一网卡。QEMU usermode (SLIRP) 的 host 网关地址是 `10.0.2.2`，
   guest 通过该地址回连 host。若 server 只绑在某个具体 IP 上，guest 会报
   `iperf3 server unreachable`。TAP 场景则由 `run.sh tap` 绑定到
   `192.168.100.1:5201`。

## 快速开始

```sh
# 在仓库根目录运行默认 SLIRP/smp=1 流程：
bash apps/starry/net-bench/run.sh aarch64

# 显式指定场景：slirp, slirp-smp4, tap, all
bash apps/starry/net-bench/run.sh aarch64 slirp
```

脚本会：
1. 检查 host 上 iperf3 是否可用
2. 在后台启动 `iperf3 -s -p 5201`（默认监听 0.0.0.0）
3. 调用对应场景的 `cargo xtask starry app qemu ...`
4. 将结果保存到 `apps/starry/net-bench/results/`

测试通过的标志是 guest 输出 `NET_BENCH_PASSED`。

## 手动运行

```sh
# 1. host 侧启动 server（另一个终端），确保监听 0.0.0.0
iperf3 -s -p 5201

# 2. 运行 StarryOS guest（usermode / SLIRP，smp=1）
cargo xtask starry app qemu --test-case net-bench --arch aarch64

# smp=4 配置：
bash apps/starry/net-bench/run.sh aarch64 slirp-smp4

# 或手动运行：
cargo xtask starry app qemu --test-case net-bench --arch aarch64 \
    --qemu-config apps/starry/net-bench/qemu-aarch64-smp4.toml
```

## TAP 流程

TAP 流程用于绕过 QEMU usermode/SLIRP。脚本不会自动修改 host 网络配置，运行前需
准备 `tap0=192.168.100.1/24`：

```sh
sudo ip tuntap add dev tap0 mode tap user "$USER"
sudo ip addr add 192.168.100.1/24 dev tap0
sudo ip link set tap0 up

bash apps/starry/net-bench/run.sh aarch64 tap
```

TAP 场景仍使用 `apps/starry/net-bench/` 这个 test-case，但通过
`qemu-aarch64-tap.toml` 切换到 TAP QEMU 配置，并在 guest 内执行
`/usr/bin/net-bench-tap.sh` 连接 host `192.168.100.1:5201`。`run.sh tap`
会为该场景设置 `AX_IP=192.168.100.2`、`AX_GW=192.168.100.1`、
`AX_PREFIX_LEN=24`，通过 StarryOS 启动时的静态网络配置初始化 guest 地址。

## 全量基线流程

```sh
bash apps/starry/net-bench/run.sh aarch64 all
```

`all` 会按顺序运行：
1. `slirp`：usermode 网络，smp=1
2. `slirp-smp4`：usermode 网络，smp=4
3. `tap`：TAP 网络，smp=1

注意：`all` 里的 TAP 步骤仍要求 host 侧 tap0 已提前配置好。

## Linux 基线（WSL 本机，参考上限）

```sh
# 无需 QEMU，直接在 WSL 里跑 loopback 基线
iperf3 -s -p 5201 &
iperf3 -c 127.0.0.1 -p 5201 -t 10
iperf3 -c 127.0.0.1 -p 5201 -t 10 -P 4
iperf3 -c 127.0.0.1 -p 5201 -t 10 -u -b 1G
kill %1
```

## 指标

| 指标 | 工具 | 参数 |
|------|------|------|
| TCP 吞吐（单流） | iperf3 | `-t 10` |
| TCP 吞吐（4流） | iperf3 | `-t 10 -P 4` |
| UDP PPS | iperf3 | `-t 10 -u -b 1G` |

## 基线结果

最新基线汇总见 `results/baseline-all-2026-06-17.txt`。摘要：

| 场景 | TCP 1流 | TCP 4流 | UDP (target 1G) |
|------|---------|---------|-----------------|
| Starry smp=1 (SLIRP, smoke only) | 93.1 Mbit/s | 93.9 Mbit/s | 70.9 Mbit/s |
| Starry smp=4 (SLIRP, smoke only) | 70.1 Mbit/s | 80.1 Mbit/s | 62.9 Mbit/s |
| Starry smp=1 (TAP, no vhost)     | 96.3 Mbit/s | 91.0 Mbit/s | 62.7 Mbit/s |
| Linux WSL2 loopback reference    | ~174 Gbit/s | ~507 Gbit/s | 1000 Mbit/s |

初步观察：测试期间 host CPU 跑满（~100%）而 guest 仅用 3-4%，smp=4 没有
带来吞吐提升。该现象和 StarryOS 协议栈单核轮询、全局锁竞争的已知瓶颈一致，
但当前数据仍属于 smoke baseline；最终归因需要 QEMU+TAP+vhost 同拓扑 Linux 基线
和重复统计确认。详细分析与优化方向见 results 汇总文件。

## 常见问题

- `NET_BENCH_FAILED: iperf3 server unreachable after 10 retries`
  host 上 iperf3 server 未启动，或绑定到了非 `0.0.0.0` 的地址。
  确认 `ss -tlnp | grep 5201` 显示监听地址为 `*:5201` 或 `0.0.0.0:5201`。

- `error: TCP port 5201 is already listening on host`
  `run.sh` 检测到已有进程占用了 iperf3 端口。停止已有 server 后重跑，或确认
  该 server 监听地址正确后手动运行 xtask。

- `error: TAP interface tap0 does not exist`
  TAP 场景要求提前创建并配置 tap0。按 TAP 流程中的 `ip tuntap` 命令设置后重跑。

- `iperf3: command not found`（host）
  执行 `sudo apt-get install -y iperf3`。若提示 dpkg 被中断，先运行
  `sudo dpkg --configure -a`。

- `debugfs rdump` 输出大量 `Operation not permitted while changing ownership`
  `prebuild.sh` 会用 `debugfs -R "rdump / ..."` 将 rootfs 镜像导出到 host 上的
  staging 目录。`rdump` 会尽量还原 ext inode 元数据，包括 uid/gid owner，因此在
  普通用户下会尝试 `chown root:root` 并被 host 内核拒绝。当前测试只依赖导出的文件
  内容、路径和可执行权限，用于安装并复制 `iperf3` 及其依赖；owner 保真失败不影响
  guest 内运行 `net-bench.sh`，只会产生 warning。默认保留原始输出，避免隐藏真正的
  `debugfs` 错误。
