# StarryOS、Linux 与 Zephyr 客户机功能检查

## 测试用途

通过 AxVisor 分别启动 StarryOS、Linux（Ubuntu 用户空间）和 Zephyr，检查启动、CPU、网络、文件读写及客户机工具运行。每个子测例独立启动一个客户机，可单独执行。

| 子测例                | 客户机        | 检查内容                             |
| --------------------- | ------------- | ------------------------------------ |
| `starry/functional` | StarryOS，VM2 | CPU、文件读写、局域网 ping           |
| `starry/cyclictest` | StarryOS，VM2 | 1000000次定时唤醒和直方图            |
| `linux/functional`  | Ubuntu，VM1   | CPU、文件、网络、APT安装并运行 hello |
| `linux/lmbench`     | Ubuntu，VM1   | 六类固定 LMBench 子项                |
| `linux/iozone`      | Ubuntu，VM1   | 512 MiB 文件的顺序及随机读写         |
| `zephyr/functional` | Zephyr，VM3   | 版本、线程、设备列表                 |

cyclictest、LMBench 和 IOZone 记录实际结果，不设置性能数值门槛。

## 边界

每次只启动所选子测例的一个客户机，手工输入该客户机的检查命令。六个子测例独立判定；cyclictest、LMBench 和 IOZone 记录数值，不以某次历史性能作为固定门槛。

## 运行前确认

镜像和测试工具统一由 tgosimages 构建并部署到板卡，客户机资源和网络前提在使用前确认。

### 构建和部署镜像

在 tgosimages 仓库构建：

```bash
./build.sh platform orangepi-5-plus all
```

将同一轮构建的内核、Zephyr 配套 DTB 和两份独立 ext4 部署到板卡原生 Linux：

```text
/guest/
├── linux/orangepi-5-plus
├── starry/orangepi-5-plus
├── zephyr/
│   ├── orangepi-5-plus
│   └── orangepi-5-plus.dtb
├── rootfs-aarch64-orangepi-jammy-0.img  # Ubuntu
└── rootfs-aarch64-orangepi-jammy-1.img  # StarryOS
```

两份 ext4 的客户机内部应有：

```text
/guest-tests/
├── cyclictest/cyclictest
├── lmbench/bin/Linux/
└── iozone/iozone
```

StarryOS 使用支持 virtio-blk、virtio-net 的 Error 日志镜像。

### 客户机资源与前提

Linux 内的 `ip`、`dhclient`、`ping`、`curl`、`apt-get` 和 `timeout` 应可用。客户机需要局域网 DHCP；功能测试需要一个客户机可达的局域网 ping 目标，APT 还需软件源可达。

Linux、StarryOS 各使用1 vCPU和1 GiB内存，固定到 MPIDR `0x400`，分别独立启动；Zephyr 使用1 vCPU、128 MiB内存，MPIDR为 `0x100`。

IOZone 的目标目录 `/var/tmp` 必须位于可写 ext4，至少有576 MiB空闲空间。tgosimages 默认预留1 GiB客户机空闲空间；以客户机中的 `df -Pk /var/tmp` 为准。

## 启动

先用仓库中的交互配置启动选定客户机，再进入客户机准备运行环境。交互入口不注入测试命令，各功能项由测试者按输出判定。

### 选择配置并启动

先在构建机设置板卡服务地址，将主机和端口占位符替换为自己的环境值：

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
```

以下命令在构建机 TGOSKits 仓库根目录执行。用 `leaf` 选择上表中的一个子测例：

首次执行下面这组命令前，在本文档所在目录打开构建机终端；代码会记录应用路径并返回仓库根目录。后续构建机命令沿用 `APP_DIR` 和 `APP_CASE`。

```bash
APP_DIR="$PWD"
cd "$(git rev-parse --show-toplevel)"
APP_CASE=$(realpath --relative-to=apps/axvisor/normal "$APP_DIR")

export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
ostool board ls --server "$BOARD_SERVER"

app="$APP_DIR"
leaf=starry/functional

cargo xtask axvisor board \
  --config "$app/$leaf/build-aarch64-unknown-none-softfloat.toml" \
  --board-config "$app/configs/${leaf%%/*}-uboot-shell.toml" \
  --server "$BOARD_SERVER"
```

`leaf` 选择子测例的宿主构建配置，`${leaf%%/*}` 选择 `configs/` 中对应操作系统的交互配置。每换一个子测例，先结束前一次会话，再修改 `leaf` 并重新执行启动命令；同一操作系统的子测例共用一份交互配置。

### 进入客户机并准备环境

看到 `axvisor:/$` 后先执行 `vm list`，再进入相应控制台：

| 客户机   | AxVisor 命令     | 客户机提示符       |
| -------- | ---------------- | ------------------ |
| Ubuntu   | `vm console 1` | `root@ubuntu:~#` |
| StarryOS | `vm console 2` | `root@starry:~#` |
| Zephyr   | `vm console 3` | `zephyr:~$`      |

下面的命令均在对应客户机内执行。Linux、StarryOS 启动后先准备：

```bash
# PID 1 是交互 shell，需要手工挂载 proc 和 sysfs
mkdir -p /proc /sys /var/tmp
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
ip link set lo up
```

交互配置不设置终端总超时，空的 `shell_check_steps` 和 `fail_regex` 将输入与结果判定交给测试者。`Ctrl+X` 后按 `h` 返回 AxVisor；`Ctrl+A` 后按 `x` 释放板卡。外层命令返回 0 仅表示会话正常结束，不替代后续功能判定。

## 场景 1：StarryOS 功能

选择 `leaf=starry/functional`，启动后进入 VM2。依次检查系统信息、文件读写和局域网连通性。

### 系统信息

确认客户机内核版本，以及 `/proc/cpuinfo` 列出处理器条目，`CPU_EXIT=0` 才通过：

```bash
uname -a
cat /proc/cpuinfo
grep -q '^processor' /proc/cpuinfo
echo CPU_EXIT=$?
```

### 文件读写

在 ext4 的临时目录写入固定字符串并逐字读回，`FILE_EXIT=0` 表示客户机文件系统可写且内容一致：

```bash
results=$(mktemp -d /var/tmp/starry-functional.XXXXXX)
printf '%s\n' AXVISOR_GUEST_FILESYSTEM_RW >"$results/file.txt"
test "$(cat "$results/file.txt")" = AXVISOR_GUEST_FILESYSTEM_RW
echo FILE_EXIT=$?
sync
```

### 网络

从虚拟网卡 ping 局域网目标：

```bash
ip -brief address
ip route
PING_TARGET='<reachable-lan-ip>'
ping -n -c 5 -W 2 -w 15 "$PING_TARGET"
echo PING_EXIT=$?
```

ping返回0，5个回复、0%丢包才通过。如果接口尚未取得地址，先确认虚拟网卡和 DHCP 工作，不能把构建机能 ping 通当作客户机通过。

## 场景 2：StarryOS cyclictest

先运行测试取得时延直方图，再核对样本总数。

### 运行 cyclictest

选择 `leaf=starry/cyclictest`，进入 VM2：

```bash
# -S 单 vCPU 简单模式：唤醒间隔 10 us、优先级 90，共 100 万次唤醒
results=$(mktemp -d /var/tmp/starry-cyclictest.XXXXXX)
timeout 300 /guest-tests/cyclictest/cyclictest \
  --default-system -S -p 90 -i 10 -h 100 -l 1000000 -q \
  --histfile="$results/latency.log" >"$results/cyclictest.log" 2>&1
rc=$?
printf 'CYCLICTEST_EXIT=%s\n' "$rc"
cat "$results/cyclictest.log"
cat "$results/latency.log"
```

应正常返回0，并输出 Min/Avg/Max 和直方图。

### 核对样本总数

直方图桶内样本加上溢出行样本，应恰好等于 `-l` 指定的唤醒次数：

```bash
awk '
  /^[0-9]+[[:space:]]+[0-9]+$/ { buckets++; samples += $2 }
  /^# Histogram Overflows:/ { overflows++; samples += $4 }
  /^# (Min|Avg|Max) Latencies:/ { statistics++ }
  END {
    printf "CYCLICTEST_SAMPLES %.0f\n", samples
    exit !(buckets > 0 && overflows == 1 && statistics == 3 && samples == 1000000)
  }
' "$results/latency.log"
echo SAMPLES_EXIT=$?
sync
```

`SAMPLES_EXIT=0` 且总数1000000才表示完成。直方图溢出样本也计入总数。该命令不锁页，采用单 vCPU 的 -S 模式；结果描述当前条件下的定时唤醒时延。

## 场景 3：Linux 功能

选择 `leaf=linux/functional`，进入 VM1。依次检查系统信息、文件读写和局域网连通性，再安装并运行 hello。

### 系统信息

确认客户机内核版本、处理器条目和发行版，`CPU_EXIT=0` 且 `ID=ubuntu` 才通过：

```bash
uname -a
cat /proc/cpuinfo
grep -q '^processor' /proc/cpuinfo
echo CPU_EXIT=$?
cat /etc/os-release
```

### 文件读写

在 ext4 的临时目录写入固定字符串并逐字读回，`FILE_EXIT=0` 表示客户机文件系统可写且内容一致：

```bash
results=$(mktemp -d /var/tmp/linux-functional.XXXXXX)
printf '%s\n' AXVISOR_GUEST_FILESYSTEM_RW >"$results/file.txt"
test "$(cat "$results/file.txt")" = AXVISOR_GUEST_FILESYSTEM_RW
echo FILE_EXIT=$?
sync
```

### 网络

取第一个非 lo 接口作为虚拟网卡，有全局IPv4地址时保留，没有时申请 DHCP，然后 ping 局域网目标：

```bash
n=$(ls /sys/class/net | grep -v '^lo$' | head -n 1)
echo NET_DEVICE=$n
ip link set "$n" up
if ! ip -4 -o addr show dev "$n" scope global | grep -q ' inet '; then
  timeout 45 /sbin/dhclient -1 "$n"
  echo DHCP_EXIT=$?
fi
ip -brief address
ip route
PING_TARGET='<reachable-lan-ip>'
ping -n -c 5 -W 2 -w 15 "$PING_TARGET"
echo PING_EXIT=$?
```

接口名必须非空，ping为5发5收、0%丢包才通过。

### 安装并运行 hello

以下整段在 Ubuntu 客户机执行。它使用临时 APT 源和缓存，按安装前状态清理 hello；括号让错误退出只结束这一段检查，不退出客户机的 PID 1 shell。

```bash
# 整段在子 shell 中执行：任一步失败只结束本段检查，不影响 PID 1 shell
(
  set -eu
  results=$(mktemp -d /var/tmp/linux-apt.XXXXXX)
  temporary_hello=0
  # 退出时按安装前状态清理，并输出汇总结果
  cleanup() {
    if [ "$temporary_hello" = 1 ]; then
      timeout -k 5 180 apt-get -y purge hello >"$results/apt-cleanup.log" 2>&1 || return 1
    fi
    rm -rf "$results/apt/lists" "$results/apt/archives"
    sync
  }
  on_exit() {
    status=$?
    trap - EXIT
    cleanup || status=1
    printf 'APT_RESULT exit=%s results=%s\n' "$status" "$results"
    exit "$status"
  }
  trap on_exit EXIT

  # 从板卡服务的响应头取时间校准客户机时钟，避免软件源日期校验失败
  BOARD_SERVER='http://<board-server-host>:<board-server-port>'
  curl -fsSI --connect-timeout 5 --max-time 20 \
    "$BOARD_SERVER/" >"$results/headers.log"
  wall_time=$(sed -n 's/^date: *//Ip' "$results/headers.log" | tr -d '\r' | head -n 1)
  test -n "$wall_time"
  date -u -s "$wall_time"

  # 使用临时的源、状态和缓存目录，不改动客户机全局 APT 配置
  mkdir -p "$results/apt/lists/partial" "$results/apt/archives/partial"
  chmod 755 "$results"
  chown _apt:root "$results/apt/lists/partial" "$results/apt/archives/partial"
  printf '%s\n' 'deb [arch=arm64] http://repo.huaweicloud.com/ubuntu-ports jammy main' \
    >"$results/apt/sources.list"
  export APT_CONFIG="$results/apt/apt.conf"
  cat >"$APT_CONFIG" <<EOF
Dir::Etc::sourcelist "$results/apt/sources.list";
Dir::Etc::sourceparts "-";
Dir::State::lists "$results/apt/lists";
Dir::Cache::archives "$results/apt/archives";
Dir::Cache::pkgcache "";
Dir::Cache::srcpkgcache "";
Acquire::Languages "none";
EOF

  # hello 原本已安装时不清理，只在本次新装时移除
  hello_was_installed=0
  if dpkg-query -W -f='${Status}\n' hello 2>/dev/null | grep -qx 'install ok installed'; then
    hello_was_installed=1
  fi
  timeout -k 5 180 apt-get -o Acquire::Retries=0 -o Acquire::http::Timeout=20 update
  if [ "$hello_was_installed" = 0 ]; then temporary_hello=1; fi
  timeout -k 5 180 apt-get -y --reinstall install hello
  hello >"$results/hello.log"
  cat "$results/hello.log"
  # 输出应恰好是 Hello, world!
  grep -qx 'Hello, world!' "$results/hello.log"
)
echo APT_EXIT=$?
```

成功需要真实更新索引、安装、运行并完成清理，`APT_EXIT=0`。hello 输出应为 `Hello, world!`。脚本只校准客户机时间；软件源签名和日期校验保持启用。

## 场景 4：Linux LMBench

选择 `leaf=linux/lmbench`，进入 VM1。六个子项依次执行，每项都必须返回0并有对应数值结果；任一子项失败时先停止本段启动的 TCP 服务、保留日志再处理，不能只用其余成功项判定整个 LMBench 通过。先准备结果目录：

```bash
lm=/guest-tests/lmbench/bin/Linux
results=$(mktemp -d /var/tmp/linux-lmbench.XXXXXX)
df -Pk "$results"
df -Pi "$results"
```

### 运算时延

运行 `lat_ops`，输出 integer add 等运算时延：

```bash
timeout -k 5 180 "$lm/lat_ops" -N 5 >"$results/cpu.log" 2>&1
echo CPU_EXIT=$?
cat "$results/cpu.log"
```

### 内存带宽

运行 `bw_mem`，输出 8 MiB 块读取的内存带宽：

```bash
timeout -k 5 180 "$lm/bw_mem" -N 5 8m rd >"$results/memory.log" 2>&1
echo MEMORY_EXIT=$?
cat "$results/memory.log"
```

### 上下文切换

运行 `lat_ctx`，输出两进程上下文切换时延：

```bash
timeout -k 5 180 "$lm/lat_ctx" -N 5 -s 0 2 >"$results/context-switch.log" 2>&1
echo CONTEXT_EXIT=$?
cat "$results/context-switch.log"
```

### Pipe 时延

运行 `lat_pipe`，输出 Pipe latency：

```bash
timeout -k 5 180 "$lm/lat_pipe" -N 5 >"$results/ipc.log" 2>&1
echo IPC_EXIT=$?
cat "$results/ipc.log"
```

### 文件系统

运行 `lat_fs`，输出 0k、1k、4k、10k 等文件大小的统计行。该子项可能持续数分钟，600秒为完成上限：

```bash
mkdir "$results/lat-fs"
timeout -k 5 600 "$lm/lat_fs" -N 5 "$results/lat-fs" >"$results/filesystem.log" 2>&1
echo FILESYSTEM_EXIT=$?
cat "$results/filesystem.log"
rm -rf "$results/lat-fs"
```

### TCP 时延

先后台启动本机服务端，再测量 TCP latency。TCP 子项使用客户机回环地址；局域网连通性由功能测试验证：

```bash
"$lm/lat_tcp" -s
timeout -k 5 180 "$lm/lat_tcp" -N 5 127.0.0.1 >"$results/network.log" 2>&1
echo NETWORK_EXIT=$?
cat "$results/network.log"
# 测试后显式关闭本段启动的服务端
timeout 10 "$lm/lat_tcp" -S 127.0.0.1
sync
```

## 场景 5：Linux IOZone

先确认目标目录空间，再运行测试并核对报告。

### 检查磁盘空间

选择 `leaf=linux/iozone`，进入 VM1。测试文件 512 MiB 加 64 MiB 余量：

```bash
results=$(mktemp -d /var/tmp/linux-iozone.XXXXXX)
df -Pk "$results"
df -Pk "$results" | awk 'END {exit !($4 >= 589824)}'
echo SPACE_EXIT=$?
```

`SPACE_EXIT=0` 才满足空间要求。

### 运行测试

`-i 0/1/2` 分别对应顺序写、顺序读、随机读写：

```bash
timeout 1800 /guest-tests/iozone/iozone \
  -a -n 512m -g 512m -i 0 -i 1 -i 2 \
  -f "$results/iozone.tmp" -Rb "$results/test.xls" >"$results/iozone.log" 2>&1
echo IOZONE_EXIT=$?
cat "$results/iozone.log"
test -s "$results/test.xls"
echo REPORT_EXIT=$?
grep -i 'iozone test complete' "$results/iozone.log"
rm -f "$results/iozone.tmp"
sync
```

要求返回0，输出顺序写/重写、读/重读、随机读写的结果及 completion 标记，Excel报告非空。保留 `iozone.log` 和 `test.xls`，删除的只是512 MiB工作文件。

## 场景 6：Zephyr 功能

选择 `leaf=zephyr/functional`，进入 VM3。在 `zephyr:~$` 下依次执行：

```text
kernel version
kernel thread list
device list
```

要求输出实际 Zephyr 版本号、包含 shell_uart 和 idle 的线程列表、至少一个 READY 设备。三个命令全部成功才通过。

## 判定

各子测例按各自命令输出和退出码独立判定。Linux、StarryOS 的读写与网络操作须实际成功；cyclictest 须核对100万样本；LMBench、IOZone 须完成指定工作量；Zephyr 须完成版本、线程和设备三项查询。手动流程不会自动打印 `CASE_PASSED`，不能只凭退出串口成功认定通过。

## 结束

测试完成后在 Linux、StarryOS 中执行 `sync`，按 Ctrl+X、h 返回 AxVisor，按 Ctrl+A、x 退出会话。记录实际源码版本、镜像哈希、运行命令和本轮日志，作为结果依据。

## 自动执行与结果（可选）

自动执行使用与手动步骤相同的子测例配置，由运行器代替人工输入并判定结果。

### 执行子测例

在构建机 TGOSKits 仓库根目录执行需要的子测例，每个外层 Cargo 命令都必须返回0：

```bash
case_dir="$APP_CASE"
# StarryOS：CPU、文件读写和局域网 ping
cargo xtask axvisor app board -g normal -t "$case_dir/starry/functional" \
  --server "$BOARD_SERVER"
# StarryOS：100 万次定时唤醒和时延直方图
cargo xtask axvisor app board -g normal -t "$case_dir/starry/cyclictest" \
  --server "$BOARD_SERVER"
# Ubuntu：CPU、文件、网络、APT 安装并运行 hello
cargo xtask axvisor app board -g normal -t "$case_dir/linux/functional" \
  --server "$BOARD_SERVER"
# Ubuntu：六类 LMBench 子项
cargo xtask axvisor app board -g normal -t "$case_dir/linux/lmbench" \
  --server "$BOARD_SERVER"
# Ubuntu：512 MiB 文件的顺序及随机读写
cargo xtask axvisor app board -g normal -t "$case_dir/linux/iozone" \
  --server "$BOARD_SERVER"
# Zephyr：版本、线程和设备列表
cargo xtask axvisor app board -g normal -t "$case_dir/zephyr/functional" \
  --server "$BOARD_SERVER"
```

自动测例的 ping 目标及 APT 校时服务地址写在对应板卡配置中，更换目标时修改对应子测例的配置，手动执行则修改命令中的地址；构建机的 BOARD_SERVER 只用于连接板卡服务，不会覆盖这些客户机命令。

### 通过判定

Linux、StarryOS 自动用例必须输出与子测例名称对应的独占行：

```text
CASE_PASSED case=starry-functional results=...
```

其中名称随子测例变化。`CASE_FAILED`、超时、工具错误或结果缺失均为失败。Zephyr 由版本、线程和 READY 设备检查判定。

自动结果位于对应客户机的 `/var/tmp/kernel-types/`，具体目录随日志打印；这不是板卡宿主的同名目录。手动步骤使用前面各段生成的 `results` 目录，按各退出码及输出判定，不会自动产生 CASE_PASSED。


## 配置说明

[configs/linux.toml](configs/linux.toml)、[configs/starry.toml](configs/starry.toml)、[configs/zephyr.toml](configs/zephyr.toml) 定义客户机镜像、磁盘和设备。`configs/linux-uboot-shell.toml`、`configs/starry-uboot-shell.toml` 和 `configs/zephyr-uboot-shell.toml` 提供对应客户机的交互启动参数。每个子测例目录另包含：

- `build-aarch64-unknown-none-softfloat.toml`：构建 AxVisor，注册该客户机。
- `board-orangepi-5-plus.toml`：启动参数、客户机命令及结果判定。

客户机镜像和工具由 tgosimages 构建；这里的 Cargo 命令负责构建 AxVisor 并运行选定客户机。
