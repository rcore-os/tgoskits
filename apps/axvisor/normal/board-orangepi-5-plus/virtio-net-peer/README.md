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
| VM65，Linux 观察端 | 1 vCPU、1 GiB               | 10.0.2.2、10.0.2.79   |

ArceOS 轮流固定到 RK3588 的8个物理核，每核8台；Linux 固定到物理核7并共享调度。默认共65个 vCPU、5 GiB客户机内存，使用8 GiB板卡。

64台 ArceOS 共用一份镜像，由独立 MAC 确定身份：`52:54:00:20:01:N` 对应 VMN 和 `10.0.2.(14+N)`。每台有独立内存和虚拟网卡，不挂载磁盘。Linux 使用独立 MAC `52:54:00:20:02:01` 和独立客户机 ext4。

## 3. 准备镜像

### 3.1 ArceOS 共享镜像

正式入口自动构建当前 TGOSKits 源码中的 `arceos-virtio-net-peer`，无需先运行独立的 ArceOS 构建命令，也无需向板端部署 ArceOS 镜像。`vcpus/smoke/guest-builds.toml` 声明配置生成命令和 `arceos_build_configs`，运行器在解析宿主构建配置前生成64份 VM 配置、构建共享客户机，然后由 axbuild 将镜像打入宿主 initramfs 的 `/guest/builtin/images`，随 AxVisor 一起上传。

`arceos-guest-build.toml` 声明客户机软件包、网络特性和 `[env].AXVIRTIO_IDENTITY_FROM_MAC = "1"`。64台客户机共用这次构建的 BIN，通过 `PeerIdentity::for_mac()` 得到各自身份。构建机需要 AArch64 musl 交叉工具链；正式运行命令见第5节。

### 3.2 准备 Linux 观察端

板卡原生 Linux 中需有：

```text
/guest/
├── linux/orangepi-5-plus
└── rootfs-aarch64-orangepi-jammy-0.img
```

这两项需提前构建并部署到板卡。客户机 ext4 内需有 `ip`、`ping` 和基本 shell 工具。`-0.img` 只供本例 Linux 使用，不能同时挂给其他客户机。

## 4. 生成配置

正式 `app board` 入口会自动生成配置。仅查看配置时，可在构建机 TGOSKits 仓库根目录执行：

```bash
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" gen
```

`gen_configs()` 固定生成64台 ArceOS 的配置，每台64 MiB，共用本轮自动构建的 ArceOS 镜像。输出文件为 `target/axvisor-arceos-net-scale/vm1.toml` 至 `vm64.toml`；宿主构建配置 `vcpus/build-aarch64-unknown-none-softfloat.toml` 引用这些文件及固定的 `linux-observer.toml`。

## 5. 使用 Cargo 启动 AxVisor

先在构建机设置板卡服务地址，将主机和端口占位符替换为自己的环境值：

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
```

以下命令在构建机 TGOSKits 仓库根目录执行。运行器依次准备客户机、构建宿主并运行完整检查，无需单独调用 `arceos build`：

```bash
export PATH=/opt/aarch64-linux-musl-cross/bin:$PATH
ostool board ls --server "$BOARD_SERVER"
cargo xtask axvisor app board -g normal \
  -t board-orangepi-5-plus/virtio-net-peer/vcpus \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

运行器自动启动 Linux 观察端、配置地址，再从 VM64 到 VM1 启动 ArceOS。第6节列出客户机内的检查命令，供理解和排查自动步骤；自动运行时不要同时手工输入这些命令。

## 6. 在客户机中验证通信

验证分两步：先确认 ArceOS 间的 TCP 心跳链，再由 Linux 观察端逐台确认可达。

### 6.1 查看 ArceOS TCP 心跳

在 AxVisor 输入：

```text
vm console 1
```

VM1 必须打印带 `ack="ACK VM2 seq=..."` 的 `VM1_VIRTIO_NET_HB_TX`。

按 Ctrl+X、h 返回 AxVisor，依次用 `vm console 2` 至 `vm console 64` 检查：VMN 必须打印 `VMN_VIRTIO_NET_HB_RX`，payload 中为前一台 `HB VM(N-1) seq=...`。

ArceOS 每台同时运行 TCP 服务端和客户端，形成 VM1→VM2→…→VM64。`PeerIdentity::for_mac()` 推导本机身份，`peer_ip_of()` 将本机地址末字节加一作为对端，因此 VM64 拨向 `10.0.2.79:5001`。Linux 观察端额外持有该地址，使链尾的 ARP 请求得到应答；观察端不运行心跳服务，TCP 连接重试不参与成功判定。

### 6.2 配置 Linux 观察端

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
nightly 通过 `app board` 读取 `guest-builds.toml`，由 `host_prepare` 调用 `orangepi-arceos-virtio-net-test.sh gen` 生成配置，再按 `arceos_build_configs` 构建共享镜像，最后打包宿主并运行完整 smoke 检查。Linux 观察端仍使用上述预先部署的板端 `/guest` 资源。

本目录提供默认 64 台 ArceOS 加一台 Linux 观察端的通信与移除检查。
`test-suit/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus/`
仍保留原有 24 客户机测例及其独立构建、运行入口。

在构建机 TGOSKits 仓库根目录执行，运行器通过 `guest-builds.toml` 自动生成配置并构建 ArceOS。只有板端 Linux 资源需提前准备。

```bash
export BOARD_SERVER='http://<board-server-host>:<board-server-port>'
cargo xtask axvisor app board -g normal \
  -t board-orangepi-5-plus/virtio-net-peer/vcpus \
  -b BallPick-Robot-Rk3588 --server "$BOARD_SERVER"
```

`app board` 按应用目录选择 smoke 配置，在解析宿主构建配置前生成配置并构建 ArceOS，再将其打入 AxVisor 并自动验证。此应用固定为64台 ArceOS 加 Linux。`cargo xtask axvisor app list` 可查看全部应用。

用例依次检查全部 ArceOS 的 TCP 心跳、移除前的 IP/MAC 对应关系、移除前63台后的失联及 VM64 仍可达、全部移除后的失联，以及 Linux 移除后的空 VM 列表。必须依次看到：

```text
SCALE_BEFORE_PASS
SCALE_PARTIAL_PASS
SCALE_AFTER_PASS
No virtual machines found.
```

所有通信检查和65台停止、删除命令均需成功，外层 Cargo 命令返回0。

2026-10-19 在 `origin/dev` 的 `96cfc036f0` 基线上，通过 `BOARD_SERVER` 指定的板卡服务在 `BallPick-Robot-Rk3588` 上复验。原控制台循环在输出持续到来时跳过输入处理，完整64台用例在首个步骤等待300秒后超时；两台客户机用例通过。验证期间曾试验每批最多回放128条记录、批量通知受背压客户机及每100毫秒刷新 VM 状态。该组控制台改动现已按要求撤回；本节后续通过记录均对应撤回前的实现，不能作为当前控制台实现的实板通过证明。

提前配置观察端并倒序启动后的首次完整运行，在 VM64 心跳步骤等待300秒后超时：其启动输出缺少 RAM 映射，停在 `MMU Entry point`。原始失败日志为 `target/axvisor-arceos-net-scale/tail-address-verification-observer-ready.log`。随后对原始 DTB、保存副本和 per-CPU 分配阶段取证，并用宿主侧只读端口检查普通镜像的 FDT；这些运行均未复现故障。采样中副本内容完整，`/memory@80000000` 描述64 MiB RAM，FDT 地址为 `0x80600000`。恢复源码后重新构建的普通客户机镜像与失败时 SHA-256 一致。该偶发启动故障的根因仍未确定，现有证据不能归因于 ARP、FDT 覆盖或缓存，也不能把后续通过当作该故障已修复。

清除全部临时诊断代码后，通过当时的 `cargo xtask axvisor test board` 入口复跑，64台心跳、三轮 IP/MAC 与移除检查、65台停止和删除、最终空 VM 列表全部通过，外层 Cargo 返回0。日志为 `target/axvisor-arceos-net-scale/tail-address-verification-repeat-clean.log`，汇总为同目录的 `tail-address-verification-result.json`，FDT 取证记录为 `fdt-investigation.json`。最终控制台代码的 QEMU `interleave` 回归通过（1/1）；`cargo xtask clippy --package axvisor` 明确跳过该软件包，按板卡构建配置执行的定向 Clippy 被基线 `builtin.rs:262` 的 `collapsible_if` 阻断。

也可通过脚本运行同一个固定用例：

```bash
app=apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus
bash "$app/orangepi-arceos-virtio-net-test.sh" run --server "$BOARD_SERVER"
```

`run` 始终调用上述 `cargo xtask axvisor app board` 入口，使用固定的64台配置及完整通信、移除检查。脚本只接受板卡服务地址 `--server`，也可通过环境变量 `BOARD_SERVER` 指定。日志和汇总保存在 `target/axvisor-arceos-net-scale/board.log`、`result.json` 中。

## 9. 配置文件

本组位于 `apps/axvisor/normal/board-orangepi-5-plus/virtio-net-peer/vcpus/`，其余配置在上级目录：

| 文件                                                                                        | 作用                                                |
| ------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| [orangepi-arceos-virtio-net-test.sh](vcpus/orangepi-arceos-virtio-net-test.sh)               | 生成配置、通过 Cargo 运行测试                      |
| [build-aarch64-unknown-none-softfloat.toml](vcpus/build-aarch64-unknown-none-softfloat.toml) | 默认64台 ArceOS 加 Linux 的正式宿主构建配置         |
| [linux-observer.toml](linux-observer.toml)                                                   | Linux 观察客户机、独立 ext4 和虚拟网卡              |
| [arceos-guest-build.toml](arceos-guest-build.toml)                                           | ArceOS 网络程序的构建配置                           |
| [board-orangepi-5-plus-arceos-net.toml](vcpus/smoke/board-orangepi-5-plus-arceos-net.toml)   | 默认64台 ArceOS 加 Linux 的板卡启动、通信和移除验证 |

以下为此前实现的历史运行记录，当时入口会构建共享镜像；当前入口按第3节通过声明式客户机构建配置自动构建并打包共享镜像。本次自动准备流程的实板验证见文末记录。

补齐独立应用入口后，使用 `cargo xtask axvisor app board -t board-orangepi-5-plus/virtio-net-peer --board-type BallPick-Robot-Rk3588 --server "$BOARD_SERVER"` 再次完成实板验收。运行器自动构建共享客户机镜像并生成配置；64台心跳、三轮观察与移除检查、65台停止和删除及最终空列表均通过，外层退出码为0，板卡运行耗时668.24秒。日志为 `target/axvisor-arceos-net-scale/app-entry-board.log`，板卡随后恢复可用。

撤回上述控制台改动后，再以相同 `app board` 命令执行实板用例。观察端配置成功，VM64至VM27启动成功，随后 `vm start 26` 对应的 `shell_check_steps[41]` 等待180秒超时，外层退出码为1；本轮未进入心跳、ping及移除检查。日志为 `target/axvisor-app-verification/console-reverted-board/board.log`，精确源码和镜像摘要及结果为同目录 `result.json`。板卡已释放，控制台源码保持撤回后的实现。

后续临时计数诊断定位到管理控制台输入饥饿：`console_init()` 每次读到有序输出记录后直接 `continue`，因此持续输出时不会走到 `read_host_byte()`。诊断运行的第95至265秒内，输出消费增加10,271条，输入读取次数和已消费字节数均不变，RX订阅持续报告可读；同一时段发出412,921次设备重试通知。已消费的1,328个输入字节恰好等于截至 `vm start 1` 的全部命令，下一条 `vm console 1` 未被消费。临时探针改变了时序，本轮启动完64台后才停滞，不能把原失败归为VM26特有的启动故障。取证充分后主动结束诊断租约，不作为完整 smoke 通过；全部临时源码和接口已撤除，普通客户机镜像已恢复，未保留控制台修复。样本与分析位于 `target/axvisor-app-verification/console-probe/samples.json` 和同目录 `analysis.json`。


针对已确认的输入饥饿，首版修复将 `console_init()` 的服务策略提取到 `ConsolePump::turn()`：每轮最多回放 `OUTPUT_BUDGET = 128` 条有序输出，再读取一个输入字节，两侧都有处理机会。输出仍经过原有 mux 的 FIFO 和后端代次检查；`notify_retained_tx_retries()` 改为每批结束后调用一次，并在输入交给可能阻塞的管理命令之前完成。`STATE_REFRESH = 100ms` 控制繁忙时的 VM 状态扫描，空闲时使用 `TaskConsoleInput::wait_event_timeout()` 等待输入、日志或超时，避免静默停止的前台客户机一直不能退出。

当前实现进一步移除 `STATE_REFRESH` 及状态扫描超时：AxVM 在生命周期快照变化或实例移除后通知 `VmLifecycleSubscription`，控制台通过 `wait_event_until()` 同时等待输入、日志和待对账标志。只有初始化或收到变化时才对账；输入限额、输出顺序和批量背压通知保持原有规则。所有权、丢失唤醒防护及验证边界见[控制台生命周期通知](../../../../../docs/docs/build/axvisor/console-lifecycle.md)。以下运行记录属于首版修复，不能作为本次事件通知改动的实板验证。

确定性回归直接调用正式服务策略：持续输出下，原策略返回 `None`，修复后每轮都能消费输入且输出继续前进。QEMU 内核测试同时验证真实日志事件等待、正常超时、批次通知与陈旧后端过滤，125项全部通过；`interleave` 控制台回归通过。`ax-runtime` 的26个 Clippy 组合及隔离源码快照中由 `cargo xtask test --since` 选出的15个受影响标准库测试包均通过。Axvisor 的项目 Clippy 入口明确跳过该包，按实板参数补查仍被未改动的 `builtin.rs:262` 的 `collapsible_if` 阻断，不能记作通过。

正式修复的串行实板验收通过：通过 `cargo xtask axvisor app board -t board-orangepi-5-plus/virtio-net-peer --board-type BallPick-Robot-Rk3588 --server "$BOARD_SERVER"` 启动当前源码，64台心跳、三轮各64个地址的连通性与移除检查、65台停止和删除、最终空列表全部完成，外层退出码0，板卡运行638.34秒。日志、源码指纹、镜像摘要和汇总保存在 `target/axvisor-app-verification/console-fixed-board-final/`。此前并行 QEMU 构建曾覆盖共享的宿主产物，造成板卡错误使用缺少存储驱动的镜像；该轮已保留为无效镜像运行记录，最终验收采用串行构建。

随后变基到最新 `dev` 的 `355289e335`，全部整理结果无冲突保留。变基后重新通过125项 QEMU 内核测试、53项 CI 计划测试、4个软件包的35个 Clippy 组合，以及隔离源码快照中16个受影响标准库测试包；PCI 网络实板定向复跑通过。上述10项完整应用运行记录与逐项日志位于 `target/axvisor-app-verification/orangepi-all-apps/`，变基后补充记录位于 `target/axvisor-app-verification/post-rebase/`。

2026-10-20 使用声明式自动构建流程复验：运行前移走构建机旧 ArceOS BIN，只执行第5节的 `cargo xtask axvisor app board` 命令。运行器按 `guest-builds.toml` 生成配置，并按 `arceos-guest-build.toml` 自动构建启用 MAC 身份推导的共享镜像，再随宿主 initramfs 打包上传。64台心跳、三轮各64个地址检查、65台停止删除及最终空列表全部通过，外层退出码0，总耗时671.09秒（板卡运行647.40秒）。镜像 SHA256 为 `71117296646b442bc66cb6ad003b74c0e3497744bd94d1add2f6a9d8651624e6`，日志、源码指纹和结果位于 `target/axvisor-app-verification/auto-build-board-case/`。此流程不读取板端预装的 ArceOS 镜像；Linux 观察端仍使用既有板端资源。
