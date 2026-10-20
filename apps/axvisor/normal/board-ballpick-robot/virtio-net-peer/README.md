# ArceOS 多客户机虚拟网络通信与移除

## 测试用途

在目标板卡上一次启动64台 ArceOS 和1台 Linux。每台客户机各有一块 AxVisor 提供的 VirtIO 虚拟网卡，接到同一虚拟网络；本例在同一次启动中完成三件事：

- ArceOS 通过 VirtIO 网卡组成 TCP 通信链。
- Linux 逐个 ping ArceOS，验证每个 IP 对应的客户机。
- 保持同一次启动，停止并删除 ArceOS，再由 Linux 确认原 IP 不可达。

## 边界

手工流程不注入检查命令。必须在同一次启动中完成64台心跳、三轮网络检查和65台客户机清理，不能拆成不同启动的结果拼接通过。观察端和 ArceOS 共享调度，链尾地址准备顺序见操作步骤。

## 运行前确认

默认配置一次启动65台客户机，每台的资源和地址如下：

| 客户机             | 资源                        | 地址                 |
| ------------------ | --------------------------- | -------------------- |
| VM1～VM64，ArceOS  | 每台1 vCPU、64 MiB，共4 GiB | 10.0.2.15～10.0.2.78 |
| VM65，Linux 观察端 | 1 vCPU、1 GiB               | 10.0.2.2、10.0.2.79   |

ArceOS 轮流固定到 RK3588 的8个物理核，每核8台；Linux 固定到物理核7并共享调度。默认共65个 vCPU、5 GiB客户机内存，板卡至少需要8 GiB内存。

64台 ArceOS 共用一份镜像，由独立 MAC 确定身份：`52:54:00:20:01:N` 对应 VMN 和 `10.0.2.(14+N)`。每台有独立内存和虚拟网卡，不挂载磁盘。Linux 使用独立 MAC `52:54:00:20:02:01` 和独立客户机 ext4。

### 镜像准备

ArceOS 客户机随宿主打包，Linux 观察端使用板端内核和独立磁盘。两类资源的准备入口不同，应在启动前分别确认。

#### ArceOS 共享镜像

正式入口自动构建当前 TGOSKits 源码中的 `arceos-virtio-net-peer`，无需先运行独立的 ArceOS 构建命令，也无需向板端部署 ArceOS 镜像。`vcpus/smoke/guest-builds.toml` 声明配置生成命令和 `arceos_build_configs`，运行器在解析宿主构建配置前生成64份 VM 配置、构建共享客户机，然后由 axbuild 将镜像打入宿主 initramfs 的 `/guest/builtin/images`，随 AxVisor 一起上传。

`configs/arceos-guest-build.toml` 声明客户机软件包、网络特性和 `[env].AXVIRTIO_IDENTITY_FROM_MAC = "1"`。64台客户机共用这次构建的 BIN，通过 `PeerIdentity::for_mac()` 得到各自身份。构建机需要 AArch64 musl 交叉工具链；正式运行命令见「启动」节。

#### Linux 观察端

板卡原生 Linux 中需有：

```text
/guest/
├── linux/orangepi-5-plus
└── rootfs-aarch64-orangepi-jammy-0.img
```

这两项需提前构建并部署到板卡。客户机 ext4 内需有 `ip`、`ping` 和基本 shell 工具。`-0.img` 只供本例 Linux 使用，不能同时挂给其他客户机。

### VM 配置准备

正式 `app board` 入口会自动生成配置。仅查看配置时，可在构建机 TGOSKits 仓库根目录执行：

首次执行下面这组命令前，在本文档所在目录打开构建机终端；代码会记录应用路径并返回仓库根目录。后续构建机命令沿用 `APP_DIR` 和 `APP_CASE`。

```bash
APP_DIR="$PWD"
cd "$(git rev-parse --show-toplevel)"
APP_CASE=$(realpath --relative-to=apps/axvisor/normal "$APP_DIR")

app="$APP_DIR"/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" gen
```

`gen_configs()` 固定生成64台 ArceOS 的配置，每台64 MiB，共用本轮自动构建的 ArceOS 镜像。输出文件为 `target/axvisor-arceos-net-scale/vm1.toml` 至 `vm64.toml`；宿主构建配置 `vcpus/build-aarch64-unknown-none-softfloat.toml` 引用这些文件及固定的 `configs/linux-observer.toml`。

## 启动

在构建机 TGOSKits 仓库根目录执行以下命令。交互流程先显式构建 ArceOS、生成64份 VM 描述，再用仓库内的 [configs/uboot-shell.toml](configs/uboot-shell.toml) 启动 AxVisor；无需向板端部署 ArceOS BIN。

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
app="$APP_DIR"
ostool board ls --server "$BOARD_SERVER"
cargo xtask arceos build --config "$app/configs/arceos-guest-build.toml"
bash "$app/vcpus/orangepi-arceos-virtio-net-test.sh" gen
cargo xtask axvisor board \
  --config "$app/vcpus/build-aarch64-unknown-none-softfloat.toml" \
  --board-config "$app/configs/uboot-shell.toml" \
  --server "$BOARD_SERVER"
```

该配置只通过 `axvisor.vm_start=65` 自动启动 Linux 观察端，不注入检查命令、不设置终端总超时。看到 `axvisor:/$` 后先按「配置观察端并启动 ArceOS」节配置观察端，再启动 ArceOS。交互命令退出为0只表示会话结束，各功能项按后面的检查判定；需要自动准备和运行完整用例时使用「自动执行与判定（可选）」节命令。

## 场景 1：虚拟网络通信

按观察端准备、ArceOS 启动、TCP 心跳和逐台 ping 的顺序执行，避免链尾地址尚未就绪时开始通信。

### 配置观察端并启动 ArceOS

返回 AxVisor，输入 `vm console 65`。PID 1 是交互 shell，先手工挂载 proc 和 sysfs，再配置观察地址和链尾对端地址：

```bash
mkdir -p /proc /sys
mount -t proc proc /proc 2>/dev/null || grep ' /proc proc ' /proc/mounts
test -d /sys/class/net || mount -t sysfs sysfs /sys
ip link set lo up
ip link set eth0 up
ip addr replace 10.0.2.2/24 dev eth0
ip addr replace 10.0.2.79/24 dev eth0
cat /sys/class/net/eth0/carrier
```

carrier 应为1。

正式 smoke 配置在 `SCALE_BEFORE`、`SCALE_PARTIAL` 和 `SCALE_AFTER` 检查前配置 `10.0.2.79/24`，避免观察窗口内向无人持有的链尾地址反复发送 ARP。自动用例先用 `axvisor.vm_start` 启动 Linux 观察端，配置两个地址并确认链路，再通过管理 shell 按 VM64→VM1 的顺序启动 ArceOS，缩短下游尚未启动的时间。`vm start` 返回不代表客户机网络服务已就绪，短暂重试仍可能发生；链尾地址则在 ArceOS 启动前已由观察端持有。手工启动全部客户机时，观察端完成配置之前仍可能发生链尾 ARP 重试。

配置完成后按 `Ctrl+X`、`h` 返回 AxVisor。在构建机另一个终端生成启动清单，并将输出逐行输入 AxVisor，每条确认成功后继续：

```bash
for n in $(seq 64 -1 1); do
  printf 'vm start %s\n' "$n"
done
```

先启动链尾再启动链首，随后按「查看 ArceOS TCP 心跳」节确认心跳。以上循环在构建机执行，AxVisor shell 本身不执行 shell 循环。

### 查看 ArceOS TCP 心跳

在 AxVisor 输入：

```text
vm console 1
```

VM1 必须打印带 `ack="ACK VM2 seq=..."` 的 `VM1_VIRTIO_NET_HB_TX`。

按 Ctrl+X、h 返回 AxVisor，依次用 `vm console 2` 至 `vm console 64` 检查：VMN 必须打印 `VMN_VIRTIO_NET_HB_RX`，payload 中为前一台 `HB VM(N-1) seq=...`。

ArceOS 每台同时运行 TCP 服务端和客户端，形成 VM1→VM2→…→VM64。`PeerIdentity::for_mac()` 推导本机身份，`peer_ip_of()` 将本机地址末字节加一作为对端，因此 VM64 拨向 `10.0.2.79:5001`。Linux 观察端额外持有该地址，使链尾的 ARP 请求得到应答；观察端不运行心跳服务，TCP 连接重试不参与成功判定。

### 检查全部 ArceOS 可达

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

## 场景 2：移除检查

保留「场景 1：虚拟网络通信」节的 Linux shell 和 `check_peers()` 函数，在同一次启动中依次移除前63台、最后一台 ArceOS 和 Linux。每个阶段都有网络结果和 VM 列表作为判据。

### 移除前63台 ArceOS

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

### 移除最后一台 ArceOS 并复查

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

### 移除 Linux 并确认空列表

返回 AxVisor，执行：

```text
vm stop 65
vm delete 65
vm list
```

必须输出 `No virtual machines found.`。完成以上步骤后才退出板卡会话。

## 判定

要求64台的 TCP 心跳存在、移除前64个 IP/MAC 均正确、移除前63台后失联且 VM64 仍可达、全部移除后64个原 IP 均无回复，最后 VM 列表为空。手工检查函数的各退出码均须为0；只看到部分心跳、少量 ping 成功或最终空列表，不足以证明全部阶段通过。

## 结束

确认 `vm list` 为空后按 `Ctrl+A`、`x` 结束串口并释放会话。若中途停止，记录未完成的客户机和检查阶段；Linux 仍在运行时先执行 `sync`，重新开始时使用新的板卡启动，不能沿用部分移除状态。

## 自动执行与判定（可选）

正式入口通过 `vcpus/smoke/guest-builds.toml` 自动生成 VM 配置并构建共享 ArceOS 镜像，再打包宿主并运行完整检查；Linux 的板端资源仍按「镜像准备」节准备。自动运行时不要同时向串口输入手工命令。

### 执行完整用例

在构建机仓库根目录执行，服务地址使用「启动」节的环境变量。CI 在 `.github/ci/checks/axvisor-nightly.toml` 中调用同一应用入口。

```bash
cargo xtask axvisor app board -g normal \
  -t "$APP_CASE"/vcpus \
  --server "$BOARD_SERVER"
```

运行器依次检查64台心跳、全部地址与 MAC 对应关系、移除前63台后的失联及 VM64 正对照、全部移除后的失联，以及移除 Linux 后的空列表。成功必须包含：

```text
SCALE_BEFORE_PASS
SCALE_PARTIAL_PASS
SCALE_AFTER_PASS
No virtual machines found.
```

上述标记与外层 Cargo 返回0必须同时成立。自动用例完成65台的停止和删除后释放板卡；交互流程则由测试者在「场景 2：移除检查」节完成清理，再按 `Ctrl+A`、`x` 结束串口。

### 失败定位与记录

客户机尚未启动或网络未就绪时，先检查 Linux 观察端是否同时持有 `10.0.2.2/24`、`10.0.2.79/24`，以及客户机 BIN 是否由当前源码生成。ping 返回1才表示无回复，返回2是执行或网络错误，不能用作移除成功的证据。

只出现部分心跳、停在早期内存映射输出或管理命令没有响应时，本轮未完成。保留卡住的 VM ID、最后一条控制台输出和镜像摘要，不通过跳过客户机或降低数量制造通过。历史上曾出现早期启动停滞，后续通过不能单独证明其根因已修复。

也可用下面的包装脚本调用同一个自动入口。它把日志和退出状态写入 `target/axvisor-arceos-net-scale/board.log`、`result.json`；直接 Cargo 运行时自行保存终端输出。

```bash
app="$APP_DIR"/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" run --server "$BOARD_SERVER"
```

## 配置说明

自动测试入口位于本应用的 `vcpus/` 子目录，其余配置在应用目录：

| 文件                                                                                        | 作用                                                |
| ------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| [orangepi-arceos-virtio-net-test.sh](vcpus/orangepi-arceos-virtio-net-test.sh)               | 生成配置、通过 Cargo 运行测试                      |
| [build-aarch64-unknown-none-softfloat.toml](vcpus/build-aarch64-unknown-none-softfloat.toml) | 默认64台 ArceOS 加 Linux 的正式宿主构建配置         |
| [configs/uboot-shell.toml](configs/uboot-shell.toml) | 交互启动 Linux 观察端，客户机检查由人工执行 |
| [configs/linux-observer.toml](configs/linux-observer.toml)                                                   | Linux 观察客户机、独立 ext4 和虚拟网卡              |
| [configs/arceos-guest-build.toml](configs/arceos-guest-build.toml)                                           | ArceOS 网络程序的构建配置                           |
| [board-orangepi-5-plus-arceos-net.toml](vcpus/smoke/board-orangepi-5-plus-arceos-net.toml)   | 默认64台 ArceOS 加 Linux 的板卡启动、通信和移除验证 |
