# ArceOS 多客户机虚拟网络通信与移除

## 1. 用途

在 Orange Pi 5 Plus 上一次启动64台 ArceOS 和1台 Linux。每台客户机各有一块 AxVisor 提供的 VirtIO 虚拟网卡，接到同一虚拟网络；本例在同一次启动中完成三件事：

- ArceOS 通过 VirtIO 网卡组成 TCP 通信链。
- Linux 逐个 ping ArceOS，验证每个 IP 对应的客户机。
- 保持同一次启动，停止并删除 ArceOS，再由 Linux 确认原 IP 不可达。

## 2. 客户机与镜像

默认配置一次启动65台客户机，每台的资源和地址如下：

| 客户机             | 资源                        | 地址                 |
| ------------------ | --------------------------- | -------------------- |
| VM1～VM64，ArceOS  | 每台1 vCPU、64 MiB，共4 GiB | 10.0.2.15～10.0.2.78 |
| VM65，Linux 观察端 | 1 vCPU、1 GiB               | 10.0.2.2             |

ArceOS 轮流固定到 RK3588 的8个物理核，每核8台；Linux 固定到物理核7并共享调度。默认共65个 vCPU、5 GiB客户机内存，使用8 GiB板卡。

64台 ArceOS 共用一份镜像，由独立 MAC 确定身份：`52:54:00:20:01:N` 对应 VMN 和 `10.0.2.(14+N)`。每台有独立内存和虚拟网卡，不挂载磁盘。Linux 使用独立 MAC `52:54:00:20:02:01` 和独立客户机 ext4。

## 3. 准备镜像

### 3.1 构建 ArceOS

以下命令在构建机 TGOSKits 仓库根目录执行。tgosimages 默认位于 `$HOME/tgosimages`，可用 `--tgosimages` 指定：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
# 通过 tgosimages 的 Orange Pi 平台入口构建当前 TGOSKits 源码
bash "$app/orangepi-arceos-virtio-net-test.sh" build-images \
  --tgosimages "$HOME/tgosimages"
```

该命令构建当前 TGOSKits 源码，产物为：

```text
tgosimages/IMAGES/orangepi/arceos/
├── orangepi-5-plus
└── manifest.json
```

tgosimages 的 `./build.sh platform orangepi-5-plus all` 同样构建该网络镜像，并打包到外层宿主文件系统的 `/guest/arceos/orangepi-5-plus`。本测例从构建机正式产物读取镜像，由 axbuild 随 AxVisor 打包，确保使用本轮选定的镜像。

### 3.2 准备 Linux 观察端

板卡原生 Linux 中需有：

```text
/guest/
├── linux/orangepi-5-plus
└── rootfs-aarch64-orangepi-jammy-0.img
```

这两项也由 tgosimages 构建并部署。客户机 ext4 内需有 `ip`、`ping` 和基本 shell 工具。`-0.img` 只供本例 Linux 使用，不能同时挂给其他客户机。

## 4. 生成配置

在构建机 TGOSKits 仓库根目录执行：

```bash
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" gen
```

默认生成：

```text
target/axvisor-arceos-net-scale/
├── vm1.toml … vm64.toml
├── linux-observer.toml
├── build.toml
└── board.toml
```

`--count` 调整 ArceOS 数量，Linux 始终另加一台，ID 为 count+1；`--memory-mib` 调整每台 ArceOS 内存。修改数量无需重建共享镜像，重新 gen 即可。

例如两台 ArceOS 加 Linux：

```bash
bash "$app/orangepi-arceos-virtio-net-test.sh" gen \
  --count 2 --output target/axvisor-arceos-net-small
```

自定义数量或输出目录时，用该输出目录中的 `build.toml`。仓库内的正式构建文件固定为默认64台 ArceOS 加 Linux。

## 5. 使用 Cargo 启动 AxVisor

先在构建机设置板卡服务地址，将主机和端口占位符替换为自己的环境值：

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
```

以下命令在构建机 TGOSKits 仓库根目录执行。宿主构建配置使用仓库内的正式文件，板卡配置从默认生成目录裁剪，保留全部客户机启动参数，移除自动输入：

```bash
ostool board ls --server "$BOARD_SERVER"
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
# 去掉自动输入步骤和自动测试总超时，生成交互版板卡配置
sed '/^\[\[shell_check_steps\]\]/,$d' \
  target/axvisor-arceos-net-scale/board.toml \
  | sed '/^timeout[[:space:]]*=/d' > /tmp/arceos-net-manual.toml

cargo xtask axvisor board \
  --config "$app/build-aarch64-unknown-none-softfloat.toml" \
  --board-config /tmp/arceos-net-manual.toml \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

看到 `axvisor:/$` 后执行 `vm list`，默认应有 VM1～VM65。以下全过程保持本次板卡会话，不退出、不复位。

## 6. 在客户机中验证通信

验证分两步：先确认 ArceOS 间的 TCP 心跳链，再由 Linux 观察端逐台确认可达。

### 6.1 查看 ArceOS TCP 心跳

在 AxVisor 输入：

```text
vm console 1
```

VM1 必须打印带 `ack="ACK VM2 seq=..."` 的 `VM1_VIRTIO_NET_HB_TX`。

按 Ctrl+X、h 返回 AxVisor，依次用 `vm console 2` 至 `vm console 64` 检查：VMN 必须打印 `VMN_VIRTIO_NET_HB_RX`，payload 中为前一台 `HB VM(N-1) seq=...`。

ArceOS 每台同时运行 TCP 服务端和客户端，形成 VM1→VM2→…→VM64。最后一台尝试连接未部署的下一地址，其连接重试不参与成功判定。

### 6.2 配置 Linux 观察端

返回 AxVisor，输入 `vm console 65`。PID 1 是交互 shell，先手工挂载 proc 和 sysfs，再把观察端配置到与全部 ArceOS 同网段的 10.0.2.2/24：

```bash
mkdir -p /proc /sys
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
ip link set lo up
ip link set eth0 up
ip addr replace 10.0.2.2/24 dev eth0
cat /sys/class/net/eth0/carrier
```

carrier 应为1。

### 6.3 检查全部 ArceOS 可达

在同一 Linux shell 中定义以下检查函数，供删除前后重复使用：

```bash
# 对区间内的 VM 逐台 ping：expected=up 要求可达且邻居 MAC 与 VM 对应，
# expected=down 要求以“无回复”退出，即 ping 返回 1
check_peers() (
  set -eu
  expected=$1
  first=$2
  last=$3
  # 检查前后虚拟链路都应保持载波
  test "$(cat /sys/class/net/eth0/carrier)" = 1
  n=$first
  while [ "$n" -le "$last" ]; do
    # VMN 的地址和 MAC 都由 N 推导
    address=10.0.2.$((14 + n))
    if ping -n -c 2 -W 1 "$address" >/dev/null 2>&1; then
      rc=0
    else
      rc=$?
    fi
    printf 'VM%s ip=%s expected=%s ping_exit=%s\n' "$n" "$address" "$expected" "$rc"
    if [ "$expected" = up ]; then
      test "$rc" -eq 0
      # 邻居表中的 MAC 必须是 52:54:00:20:01:N，确认地址属于这台 VM
      mac=$(printf '52:54:00:20:01:%02x' "$n")
      ip neigh show "$address" dev eth0 | grep -qi "lladdr $mac "
    else
      # 退出码 2 是执行或网络错误，不能当作“客户机已删除”
      test "$rc" -eq 1
    fi
    n=$((n + 1))
  done
  test "$(cat /sys/class/net/eth0/carrier)" = 1
)
check_peers up 1 64
echo CHECK_EXIT=$?
```

`CHECK_EXIT=0` 表示64个地址均可达且邻居 MAC 与 VM 对应。ping 的退出码2表示执行或网络错误，不能作为“客户机已删除”的证据。

## 7. 保持同次启动验证移除

### 7.1 移除前63台 ArceOS

按 Ctrl+X、h 返回 AxVisor。对 VM1～VM63 逐台执行以下两条命令，每条都确认成功后再继续：

```text
vm stop 1
vm delete 1
vm stop 2
vm delete 2
...
vm stop 63
vm delete 63
```

这里的省略号表示对中间每个 VM 重复操作，不是 AxVisor 命令。可以在构建机另一个终端生成完整命令清单，再逐行输入 AxVisor：

```bash
# 生成完整的 stop/delete 命令清单
for n in $(seq 1 63); do
  printf 'vm stop %s\nvm delete %s\n' "$n" "$n"
done
```

进入 `vm console 65`，在之前的 Linux shell 中执行：

```bash
# 前63台应不可达，保留的 VM64 作为正对照
check_peers down 1 63
echo REMOVED_EXIT=$?
check_peers up 64 64
echo CONTROL_EXIT=$?
```

两项均应为0：前63个 IP 不可达，保留的 VM64 仍可达。VM64 是这一阶段的虚拟网络正对照。

### 7.2 移除最后一台 ArceOS 并复查

返回 AxVisor，执行：

```text
vm stop 64
vm delete 64
vm console 65
```

在 Linux 中执行：

```bash
# 全部64个原 IP 均应无回复
check_peers down 1 64
echo FINAL_CHECK_EXIT=$?
sync
```

`FINAL_CHECK_EXIT=0` 表示全部64个原 IP 均无回复。

### 7.3 移除 Linux 并确认空列表

返回 AxVisor，执行：

```text
vm stop 65
vm delete 65
vm list
```

必须输出 `No virtual machines found.`。完成以上步骤后才退出板卡会话。

## 8. 自动执行

默认 64 台 ArceOS 加一台 Linux 的板卡用例固定在
`vcpus/smoke/board-orangepi-5-plus-arceos-net.toml`，
通过标准测例入口运行，并注册在 `.github/ci/checks/axvisor-nightly.toml` 的
`test-axvisor-self-hosted-board-orangepi-5-plus-arceos-net` 检查中。
nightly 使用当前检出的源码和 `arceos-guest-build.toml`，通过 `cargo xtask arceos build`
与 `AXVIRTIO_IDENTITY_FROM_MAC=1` 构建共享镜像，再生成配置、运行完整 smoke 检查；
不依赖额外的 tgosimages 检出。Linux 观察端仍需上述板端 `/guest` 资源。

本目录提供默认 64 台 ArceOS 加一台 Linux 观察端的通信与移除检查。
`test-suit/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus/`
仍保留原有 24 客户机测例及其独立构建、运行入口。

先完成镜像准备，再在构建机 TGOSKits 仓库根目录执行：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" gen

cargo xtask axvisor test board -g normal \
  --board orangepi-5-plus-arceos-net -c smoke \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

Cargo 从上级 vcpus 目录读取正式构建配置，构建 AxVisor，并使用 smoke 中的板卡配置自动启动和验证。此入口固定为64台 ArceOS 加 Linux；gen 需要使用默认数量和输出目录。

用例依次检查全部 ArceOS 的 TCP 心跳、移除前的 IP/MAC 对应关系、移除前63台后的失联及 VM64 仍可达、全部移除后的失联，以及 Linux 移除后的空 VM 列表。必须依次看到：

```text
SCALE_BEFORE_PASS
SCALE_PARTIAL_PASS
SCALE_AFTER_PASS
No virtual machines found.
```

所有通信检查和65台停止、删除命令均需成功，外层 Cargo 命令返回0。

自定义客户机数量、内存或输出目录时，仍可使用脚本生成配置并运行：

```bash
bash "$app/orangepi-arceos-virtio-net-test.sh" run \
  --count 2 --output target/axvisor-arceos-net-small \
  --server "$BOARD_SERVER"
```

默认64台、默认输出目录的 run 同样调用上述标准 smoke 入口；自定义数量或输出目录时调用 cargo xtask axvisor board。日志和汇总分别保存在指定输出目录的 board.log、result.json 中。仅自动验证通信可使用普通 gen 生成的 board.toml；完整移除使用 run，或 gen --with-removal 后自行运行 Cargo。

## 9. 配置文件

本组位于 `apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus/`，其余配置在上级目录：

| 文件                                                                                        | 作用                                                |
| ------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| [orangepi-arceos-virtio-net-test.sh](vcpus/orangepi-arceos-virtio-net-test.sh)               | 生成配置、调用 tgosimages 构建、通过 Cargo 运行测试 |
| [build-aarch64-unknown-none-softfloat.toml](vcpus/build-aarch64-unknown-none-softfloat.toml) | 默认64台 ArceOS 加 Linux 的正式宿主构建配置         |
| [linux-observer.toml](linux-observer.toml)                                                   | Linux 观察客户机、独立 ext4 和虚拟网卡              |
| [arceos-guest-build.toml](arceos-guest-build.toml)                                           | ArceOS 网络程序的构建配置                           |
| [board-orangepi-5-plus-arceos-net.toml](vcpus/smoke/board-orangepi-5-plus-arceos-net.toml)   | 默认64台 ArceOS 加 Linux 的板卡启动、通信和移除验证 |
