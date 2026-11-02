# Zephyr 与 StarryOS IVC 传输基准

## 测试用途

本应用通过 `ivc-channel` 连接 Zephyr 统一 shell 与 StarryOS，完成四组各 100 次传输。它验证两端协议和完整传输计数，同时输出性能数据；普通 IVC 功能检查见 [IVC 功能检查](../functional/README.md)。

## 边界

本例只测 Zephyr 与 StarryOS 的 IVC 传输。手工入口不注入命令或自动判定；两端完成、四组计数及 StarryOS 退出码均满足要求才通过。单端日志或早先结果文件不能替代本轮完整结果。

## 运行前确认

宿主构建配置通过 `vm_configs` 打包两份客户机描述，运行时从板卡 `/guest` 读取发布镜像。内核、Zephyr DTB 和客户机用户态程序需要来自配套版本。

### 板端文件

在板卡原生 Linux 中部署并确认以下资源，替换前保留旧文件，替换后执行 `sync`，再释放原生 Linux 会话。

```text
/guest/
├── starry/orangepi-5-plus
├── zephyr/orangepi-5-plus
├── zephyr/orangepi-5-plus.dtb
└── rootfs-aarch64-orangepi-jammy-1.img
```

StarryOS 的独立 ext4 内必须有 `/usr/bin/ivc-starry-bench` 及其匹配的 IVC 用户态库。只替换客户机内核不能补齐 rootfs 中的程序。Zephyr 镜像必须提供 `ivc ivc_benchmark` 子命令。

### 客户机分配

[configs/starry-axivc-benchmark.toml](configs/starry-axivc-benchmark.toml) 定义 VM1，单 vCPU、1 GiB RAM、独立 VirtIO 磁盘；[configs/zephyr-axivc-benchmark.toml](configs/zephyr-axivc-benchmark.toml) 定义 VM2，单 vCPU、128 MiB RAM、PL011 虚拟控制台。启动参数 `axvisor.vm_start=1,2` 自动启动两台客户机。

## 启动与手工测量

以下命令在构建机仓库根目录执行。板卡类型、服务地址和端口通过 `--board-type <板卡类型> --server <服务地址> --port <端口>` 显式指定。

### 启动宿主

[configs/uboot-shell.toml](configs/uboot-shell.toml) 不注入命令、不自动判定，也不设置终端总超时，适合逐步观察两端结果。

以下命令从 TGOSKits 仓库根目录执行，配置文件直接使用仓库内路径。

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
cargo xtask axvisor board \
  --config apps/axvisor/normal/board-ballpick-robot/ivc/benchmark/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/ivc/benchmark/configs/uboot-shell.toml \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

看到 `axvisor:/$` 后执行 `vm list`，再用 `vm console 1` 进入 StarryOS。`Ctrl+X` 后按 `h` 可返回宿主控制台。

### 先启动 StarryOS 对端

在 StarryOS 的 `# ` 提示符下执行以下命令。每次运行创建新的结果目录，避免读取上次留下的成功日志；保留当前 shell，后面还要读取 `ivc_results`。

```sh
ivc_results=$(mktemp -d /tmp/ivc-benchmark.XXXXXX)
printf 'IVC_RESULTS=%s\n' "$ivc_results"
(
  /usr/bin/ivc-starry-bench
  printf '%s\n' "$?" > "$ivc_results/status"
) > "$ivc_results/benchmark.log" 2>&1 </dev/null &
```

返回 AxVisor 后执行 `vm console 2`，在 Zephyr 提示符下运行下一节命令。StarryOS 对端在后台等待，不需要关闭它。

### 启动 Zephyr 并核对两端

Zephyr 命令的参数是毫秒，总等待上限为 240 秒。执行期间不要重启或删除任一客户机。

```text
ivc ivc_benchmark 240000
```

Zephyr 必须输出独占行 `ORANGEPI_ZEPHYR_IVC_BENCHMARK_PASSED`。随后返回宿主并进入 VM1，在原 StarryOS shell 执行：

```sh
i=0
while [ ! -f "$ivc_results/status" ] && [ "$i" -lt 30 ]; do
  sleep 1
  i=$((i + 1))
done
cat "$ivc_results/benchmark.log"
cat "$ivc_results/status"
sync
```

## 判定

要求日志中的 `AXVISOR_IVC_BENCH_RESULT=PASS cases=4 testTime=100 bytes=1232076800 chunks=400` 与 status 文件中的 `0` 同时出现。状态文件缺失、任一端报错或传输计数不符均未通过。

## 结束

手工运行完成后先在 StarryOS 执行 `sync`，返回宿主，再按 `Ctrl+A` 后按 `x` 结束串口并释放会话。退出交互命令为 0 只表示会话正常结束，各项功能仍按两端日志判定。保存发布版本、文件摘要、本轮结果目录和两端日志；超时或取消后按板级复位流程重新运行。

## 自动执行（可选）

自动入口使用同一组客户机和工作量，由运行器完成对端启动、控制台切换和退出码检查。

### 正式命令

[board-ballpick-robot-ivc-benchmark.toml](board-ballpick-robot-ivc-benchmark.toml) 定义自动步骤和失败匹配，总超时为 300 秒。

```bash
cargo xtask axvisor app board -g normal \
  -t board-ballpick-robot/ivc/benchmark \
  --board-type <板卡类型> --server <服务地址> --port <端口>
```

除两端成功标记外，自动入口还要求 `IVC_BENCH_EXIT=0`，并且外层命令返回 0。CI 继续在 benchmark 阶段采集性能报告；目录位于 `apps` 不改变性能检查的归属。

原 [benchmarks 副本](../../../../../../benchmarks/axvisor/board-orangepi-5-plus/ivc-benchmark) 保留现有 CI 性能入口；本目录面向当前应用板卡，并增加统一 shell 的自动检查。修改 IVC 协议、四组传输工作量或客户机资源时，修改者须同步核对两份配置；板卡类型、相对资源路径和 shell 匹配按各自场景维护。
