# Orange Pi 5 Plus 三客户机全虚拟化功能用例复现指南

本目录的板卡用例在 Orange Pi 5 Plus 上用 AxVisor 同时运行 Linux、StarryOS 与 Zephyr 三个客户机。只有 Zephyr 使用专用镜像和专用身份 `zephyr-triple-guest-functional`；Linux 与 StarryOS 使用通用身份 `linux`、`starry`，分别复用板卡已有的 6.1.99 虚拟客体内核与本 checkout 构建的 StarryOS 内核。用例覆盖文件后端 VirtIO 根盘、经物理网卡上行的客户机网络、由 tgosimages 构建并部署的 Zephyr Shell 客户机，以及在两个磁盘后端客户机里各完成一次真实的离线 `apt-get` 安装、运行与还原。三个都是全虚拟化客户机：AxVisor 保留板卡 eMMC 根文件系统和物理 RTL8125 网卡的所有权，客户机只使用 AxVisor 仿真的设备窗口，不接收物理直通。

## 1. 测试目标与架构

三个客户机由 `build-aarch64-unknown-none-softfloat.toml` 的 `vm_configs` 按顺序装载，分别由 `linux-virtio-blk.toml`、`starry-virtio-blk.toml` 和 `zephyr-triple-guest-functional.toml` 描述，在一次运行中同时启动并全程保持运行。三者都是 `guest_type = "virtualized"`：Linux 与 StarryOS 从各自独立的文件后端 VirtIO 块设备启动（guest 侧 Linux 为 `/dev/vda`，StarryOS 用整盘 `root=/dev/sda`），Zephyr 不挂磁盘，只运行 tgosimages 发布的 `zephyr-triple-guest-functional` Shell 镜像。

Linux 与 StarryOS 的客户机 FDT 都由 AxVisor 从宿主固件 FDT 按虚拟化规则生成（两者都没有 `dtb_path`），Zephyr 使用与镜像同批构建的 DTB。三个客户机的 `passthrough` 与 `disabled` 列表都为空，它们看到的中断控制器、控制台、块设备和网卡全部是 AxVisor 的仿真窗口。

### 1.1 客户机网络

Linux 与 StarryOS 的 `virtio-net` 接在 AxVisor 内部二层交换机上，各自拥有互不相同的 MAC，默认只能彼此互访。用例的地址与 ping 检查要求出到局域网，因此宿主侧经唯一物理 RTL8125 打开二层上行：最新 AxVisor 默认网络路径负责宿主网络，`ax-driver/realtek-rtl8125` 提供物理网卡，`net/ax-net/src/uplink.rs` 提供出口和接收分发边界，`virtualization/axvm/src/configured/devices/virtio_net.rs` 只拥有虚机侧交换机与 guest 端口；宿主继续运行自己的 DHCP 客户端和控制面，这块网卡始终只有一个所有者。缺少物理上行时地址与 ping 步骤失败关闭，不会只凭内部交换机通过。

### 1.2 判定原则

这套检查是失败关闭的。板卡入口 `functional/board-orangepi-5-plus-triple-guest-functional.toml` 按顺序注入 `shell_check_steps`，每一步只在输出中匹配约定标记；看到客户机登录提示符、内核启动日志或某个中间标记都不算通过，只有完整标记序列成立才算通过。三个客户机的检查相互独立，某一客户机失败不会因为另一客户机通过而被掩盖。网络检查把枚举、接口名、链路状态、地址、路由和 ping 拆成独立步骤，各自计时分开，停顿归到导致停顿的那一步。Zephyr 的 benchmark 或 sample 镜像会跑完并打印 `PROJECT EXECUTION SUCCESSFUL`、永远不给 `zephyr:~$` 提示符，因此该 banner 被列入顶层 `fail_regex` 立即判失败，避免错误部署一直挂到整体超时。

## 2. 客户机与资源划分

三个客户机的身份、内核来源、根盘与关键设备如下表，其中 AxVisor 宿主一行用于对照它保留的物理资源。所有客户机路径均为宿主文件系统位置，缺失时用例会在启动客户机之前失败。

| 客户机 | `[base] name` | VM id | 内核来源 | 根文件系统 | 关键设备 |
| --- | --- | --- | --- | --- | --- |
| Linux | `linux` | 1 | 板卡已部署的 6.1.99 虚拟客体内核 `/guest/linux/orangepi-5-plus-6.1.99-virtualized`（绝对板卡路径，保持外部），不由本 checkout 构建 | `/guest/rootfs-aarch64-orangepi-jammy-0.img`，`virtio-blk`，guest 侧 `/dev/vda` | 显式虚拟 16550 控制台（`console0`，`uart16550-mmio`，`0xfeb50000`，guest 侧 `ttyS0`）、内部 `virtio-net`（MAC `52:54:00:54:01:01`） |
| StarryOS | `starry` | 2 | 由本 checkout 构建，作为内置资源打包进宿主 initramfs（`/guest/builtin/images`） | `/guest/rootfs-aarch64-orangepi-jammy-1.img`，`virtio-blk`，整盘 `root=/dev/sda` | 内部 `virtio-net`（MAC `52:54:00:54:01:02`） |
| Zephyr | `zephyr-triple-guest-functional` | 3 | 由 tgosimages 经 `scripts/apps/zephyr-triple-guest-functional.sh` 构建并部署（绝对板卡路径，保持外部） | 无磁盘 | 跟随宿主板级 GIC 地址的合成 vGIC、虚拟 UART |
| AxVisor | - | - | 由本 checkout 构建的宿主内核 | 板卡 eMMC 根文件系统 | 两块物理 RTL8125、板卡根存储 |

两张 rootfs 是各自独立的文件后端设备，拥有各自独立的 ext4 文件系统和 UUID：`-0` 归 Linux，`-1` 归 StarryOS，两者都必须在准备阶段各自修复干净并拥有不同 UUID。AxVisor 在宿主 eMMC 上保留自己的根文件系统，不会把它让渡给任何客户机。

Linux 客户机身份名 `linux` 保持不变，外部内核路径改用板卡已部署的 6.1.99 虚拟客体内核 `/guest/linux/orangepi-5-plus-6.1.99-virtualized`，为全虚拟化 VirtIO 客体准备，并显式声明一个 16550（`uart16550-mmio`）虚拟控制台，guest 侧以 `console=ttyS0` 使用 `0xfeb50000` 窗口，不复制任何板卡 BSP 内核，也没有任何物理直通。该镜像是 6.1.99 的虚拟客体变体，控制台走 8250/ttyS0；复用该内核路径不等于已验证运行兼容，只有板卡运行走到约定标记才能证明，驱动缺失时客户机直接启动失败，不会退回物理直通。

StarryOS 不识别 Linux 的 `/dev/vd*` 名称：它的 `root=` 只接受 `PARTUUID=`、`PARTLABEL=`、`/dev/sd*`、`/dev/nvme*n1` 与 `/dev/mmcblk*`（见 `fs/ax-fs-ng/src/root.rs`），其它取值会直接 panic，所以 StarryOS 客户机用整盘 `/dev/sda` 选择它唯一的那个文件后端根盘。

## 3. 板上文件需求与放置路径

下表列出板卡上必须预先存在的客户机资产及其来源，这些路径随构建配置一起提交，缺失时用例会在启动客户机之前失败。Zephyr 二进制与 DTB 必须来自同一次构建：板卡检查断言 `device list` 里出现 `interrupt-controller@fe600000` 与 `serial@feb50000`，只有同批 DTB 才带有这两个节点。StarryOS 客户机内核由本 checkout 构建，由 `cargo xtask axvisor test board` 打包进宿主 initramfs 的内置资源包（`/guest/builtin/images`）并改写配置引用，板卡侧不需要另放该文件；其构建命令见第 4.1 节。

| 文件 | 来源 | 说明 |
| --- | --- | --- |
| `/guest/linux/orangepi-5-plus-6.1.99-virtualized` | 板卡已有（发布/部署流程提供） | 板卡已部署的 6.1.99 虚拟客体内核（6.1.99 虚拟客体变体，控制台 8250/ttyS0），不由本 checkout 构建。作为本用例的全虚拟化 Linux 客户机内核，它必须实际带有 virtio-mmio/virtio-blk/virtio-net 与 ext4 支持，否则客户机无法启动 |
| `/guest/rootfs-aarch64-orangepi-jammy-0.img` | 准备生成 | `linux` 客户机根盘，原始 ext4，会被用例改写 |
| `/guest/rootfs-aarch64-orangepi-jammy-1.img` | 准备生成 | `starry` 客户机根盘，原始 ext4，会被用例改写，UUID 与 `-0` 不同 |
| `/guest/zephyr/zephyr-triple-guest-functional` | tgosimages `scripts/apps/zephyr-triple-guest-functional.sh` | Zephyr Shell 二进制，部署到 `zephyr-triple-guest-functional.toml` 的 `kernel_path` |
| `/guest/zephyr/zephyr-triple-guest-functional.dtb` | 同一次 Zephyr 构建 | 与二进制同批构建的 DTB，部署到 `zephyr-triple-guest-functional.toml` 的 `dtb_path` |

## 4. 准备脚本、构建命令与执行环境

本节按依赖顺序给出 StarryOS 客户机内核构建、客户机根盘与本地 APT 仓库准备、Zephyr 镜像获取与部署，以及 AxVisor 宿主构建与标准板卡测试入口。

### 4.1 StarryOS 客户机内核

`starry-virtio-blk.toml` 的 `kernel_path` 引用 `target/aarch64-unknown-none-softfloat/release/starryos.bin`；`cargo xtask axvisor test board` 在构建宿主时把该文件作为宿主 initramfs 的内置资源打包（安装到 `/guest/builtin/images`）并改写配置引用。`cargo xtask axvisor test board` 只构建并启动 AxVisor，**不会**替你重建 StarryOS 客户机内核，因此在跑标准板卡测试之前必须先在标准 `tgoskits` 仓库根目录用本用例的 `starry-guest-build.toml`（`max_cpu_num = 1`）显式构建一次，否则打包进去的会是旧产物或根本不存在的产物。CI 项把这条构建命令放在同一条 `command` 的前置步骤里并用 `&&` 串起来，只有客户机构建成功才会启动板卡；`pull_request_command` 保持同一条带前置构建的命令。

```bash
cargo xtask starry build \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/triple-guest-functional/starry-guest-build.toml \
  --smp 1
```

该配置只为这个客户机启用 VirtIO 块设备与网卡驱动特性。`starryos/legacy-board-init` 已经被删除，本用例不再通过构建特性选择 init 路径：`starry-virtio-blk.toml` 的 cmdline 直接使用主线板卡 guest 的 `init=/bin/sh` 方案，`HOME=/root`、`USER=root`、`HOSTNAME=starry` 与 `init=/bin/sh` 一样位于内核 cmdline（`--` 之前），`--` 之后才是传给 shell 的参数 `-c "cd /root; export PS1=$USER@$HOSTNAME:~#; exec /bin/sh -i"`，所以 StarryOS 客户机自身的 shell 提示符仍是板卡检查等待的 `root@starry:~#`，Jammy 根文件系统里缺失的 OpenRC 资产不再参与启动。

### 4.2 客户机根盘与本地 APT 仓库准备

两个 rootfs 都是会在测试中被改写的可变工作镜像。准备与运行都在**板卡原生 Linux 上以 root 执行**：先备份原始镜像并做 `e2fsck`，再把 `hello` 包离线暂存进每个镜像的 `/var/cache/apt/archives/`，然后运行 `prepare-guest-apt-repo.sh`。

```bash
# 在板卡原生 Linux 上以 root 执行，两个 guest 必须已停止
sh test-suit/axvisor/normal/board-orangepi-5-plus/triple-guest-functional/prepare-guest-apt-repo.sh \
  /guest/rootfs-aarch64-orangepi-jammy-0.img \
  /guest/rootfs-aarch64-orangepi-jammy-1.img
```

脚本拒绝已挂到 loop 设备上的镜像以及 `e2fsck -n` 不干净的镜像，用板卡自带的 `dpkg-deb` 重新校验缓存包（默认期望 `hello` `2.10-2ubuntu4`），然后在镜像内建立隔离的 `file:` 仓库 `/usr/local/share/axvisor-guest-apt/repo`、唯一源 `/etc/apt/axvisor-guest.sources.list` 和隔离配置 `/etc/apt/axvisor-guest.conf`，并预热该配置自己的索引与缓存。原有的 `/etc/apt/sources.list`、`/etc/apt/sources.list.d`、`/var/lib/apt/lists`、dpkg 数据库、已缓存 `.deb` 与文件系统 UUID 都只读或保持不变；脚本不创建也不恢复备份、不安装/删除/清除任何包、不重写 dpkg 数据库，跑完后复核文件系统并断言 UUID 未改变。它只在镜像内写 `/usr/local/share/axvisor-guest-apt/**`、`/etc/apt/axvisor-guest.*`、`/var/lib/apt/lists-axvisor-guest/**` 与 `/var/cache/apt/axvisor-guest-*-cache.bin`。脚本是幂等的，重复执行得到相同状态；可用 `HELLO_PACKAGE` 改包名、`HELLO_VERSION` 改期望版本（置空表示接受镜像里已有的版本）。

本地 APT 检查证明 guest 能在受控输入下完成真实的 `apt-get`/`dpkg` 解析、安装、运行与还原，但它不包含任何联网下载，因此“ping 通”不等于“能在线装包”。

### 4.3 Zephyr 客户机镜像

Zephyr 客户机运行 `zephyr-triple-guest-functional` Shell 镜像。该镜像**不在 tgoskits 中构建**：tgoskits 不再携带 Zephyr 应用源码或构建脚本，镜像由独立的 [tgosimages](https://github.com/rcore-os/tgosimages) 仓库构建并发布。构建入口是 tgosimages 的 `scripts/apps/zephyr-triple-guest-functional.sh`（在 tgosimages 仓库根目录执行，可加 `clean` 先清掉该 profile 自己的构建树与产物）。它复用 tgosimages 的 `orangepi-5-plus` Zephyr target，在同一次构建里产出二进制、ELF 和 DTB。

```bash
# 在 tgosimages 仓库根目录执行
./scripts/apps/zephyr-triple-guest-functional.sh
```

产物是 tgosimages 标准 Orange Pi Zephyr 镜像目录 `IMAGES/orangepi/zephyr/` 下的 `zephyr-triple-guest-functional`、`zephyr-triple-guest-functional.elf` 和 `zephyr-triple-guest-functional.dtb`。部署时把二进制与**同一次构建**的 DTB 一起安装到第 3 节列出的板卡路径，使 ELF 入口与 DTB 里的 GIC/UART/`sram0`（128 MiB 客户机 RAM）身份同 `zephyr-triple-guest-functional.toml` 的 `entry_point` 与 `memory_regions` 保持一致：

```bash
install -m 0644 IMAGES/orangepi/zephyr/zephyr-triple-guest-functional \
  /guest/zephyr/zephyr-triple-guest-functional
install -m 0644 IMAGES/orangepi/zephyr/zephyr-triple-guest-functional.dtb \
  /guest/zephyr/zephyr-triple-guest-functional.dtb
sync
sha256sum /guest/zephyr/zephyr-triple-guest-functional /guest/zephyr/zephyr-triple-guest-functional.dtb
```

tgoskits 的 CI 只运行板卡入口，不会构建 Zephyr 镜像，也不会假设当前机器存在 tgosimages 同级 checkout：**板卡上必须已经由发布/部署流程按上面的路径放好这批资产**。

### 4.4 AxVisor 宿主与标准板卡测试入口

`build-aarch64-unknown-none-softfloat.toml` 声明宿主的 `features` 和三个 `vm_configs`，其中显式启用 `ax-driver/rk3588-pcie`、`ax-driver/realtek-rtl8125`；最新 AxVisor 默认启用宿主网络和物理 uplink。板卡运行使用标准入口，它按该配置构建 AxVisor，经 ostool 板卡服务把 FIT 上传到板卡（U-Boot 通过 DHCP/TFTP 取得），再按 `shell_check_steps` 注入命令。本用例的板卡入口没有 `guest-builds.toml`，不会通过该侧车准备源码客户机，第 4.1 节的 StarryOS 构建必须先单独执行。板卡服务地址与端口属于运行环境配置，用占位符传入，不写进仓库。

三种入口对客户机开机资源的打包要求不同，必须区分清楚。本机完整打包构建（`cargo xtask axvisor build`，`run_build_request`）以 `prepare_guest_payload(..., false)` 打包一个自包含的宿主 initramfs：`vm_configs` 引用的每一个开机资源都必须在构建机上真实存在，缺一即报错。板卡/uboot 入口（`cargo xtask axvisor test board`、`cargo xtask axvisor uboot`）以 `prepare_guest_payload(..., true)` 打包：指向板卡根文件系统的绝对路径在构建机上缺失时被保留为外部镜像，由板卡运行前在已准备的根文件系统上校验；本机存在的资源（例如本 checkout 构建出的 `starryos.bin`）仍会被打包并改写为 `/guest/builtin/images` 下的内置路径。本用例的 `/guest/linux/orangepi-5-plus-6.1.99-virtualized` 与 `/guest/zephyr/zephyr-triple-guest-functional{,.dtb}` 就是这种板卡外部资产。

因此本用例只通过板卡入口运行：不需要为了本机 `cargo xtask axvisor build` 去填充或模拟板卡外部资产，也不改公共 `axbuild` 行为。只要板卡外部资产缺失或本 checkout 尚未按第 4.1 节构建 StarryOS 内核，板卡入口就会在启动客户机之前失败。

```bash
# 发现
cargo xtask axvisor test board --list \
  --test-group normal \
  --test-case functional \
  --board orangepi-5-plus-triple-guest-functional

# 执行
cargo xtask axvisor test board \
  --test-group normal \
  --test-case functional \
  --board orangepi-5-plus-triple-guest-functional \
  --server <board-service> --port <port>
```

板卡服务参数（`--server`、`--port`、`--board-type`）按运行环境传入。直接运行 `cargo xtask axvisor test board` 不会自建客户机：运行前必须已按第 4.1 节构建好 StarryOS 客户机内核，并确认两个 rootfs 都已按第 4.2 节准备并跑过 `prepare-guest-apt-repo.sh`，且 `/guest/linux/orangepi-5-plus-6.1.99-virtualized` 与部署后的 `zephyr-triple-guest-functional` 资产都存在。

ping 默认目标是 guest 自己的默认网关；需要打一个明确的外网地址时用宿主的 `TRIPLE_PING_TARGET` 覆盖。对没有可用 DHCP 路径的板卡，可以用 `TRIPLE_LINUX_IP`/`TRIPLE_LINUX_GW` 与 `TRIPLE_STARRY_IP`/`TRIPLE_STARRY_GW` 传入静态地址与网关，不设置它们即继续使用 DHCP。`${env:...}` 由运行器在注入前展开，`${p}` 这类变量仍由 guest shell 自己解释。

## 5. 各客户机检查内容

用例一次性启动三个客户机并在整个序列里保持运行，先完整检查 StarryOS，再切换检查 Linux，最后检查 Zephyr。

1. 读取 `vm list`，确认 Linux、StarryOS、Zephyr 三个 VM 均为 `running`。
2. 进入 StarryOS（`vm console 2`）：CPU（`/proc/cpuinfo` 含 `processor`）、文件读写、`IFLIST`/`IFNAME`/`LINKUP`、可选静态地址、DHCP、`IP`、`ROUTE` 和 5 次 ping；随后导出 `APT_CONFIG=/etc/apt/axvisor-guest.conf`，用 `apt-cache policy hello` 证明候选来自本地仓库（`APTSRC`），记录安装前状态并执行真实 `apt-get install`、运行 `hello` 与还原，最后 `sync`。
3. 切到 Linux（`Ctrl+X`、`h`、`vm console 1`）：做同一组独立的 mounts、CPU、文件、网络和本地 APT 检查，并以一次 `sync` 结束。
4. 切到 Zephyr（`Ctrl+X`、`h`、`vm console 3`）：执行 `kernel version`、`kernel thread list`、`device list`；随后再次 `vm list` 确认三个客户机仍在运行。

每一步只接受约定的独占标记，例如 `STARRY_IFLIST_PASS`、`STARRY_LINKUP_READY`、`STARRY_NET_PASS`、`STARRY_APTSRC_PASS`、`STARRY_INSTALL_PASS`、`STARRY_HELLO_PASS`、`STARRY_SYNC_PASS`，Linux 的 `LINUX_MOUNTS_PASS` 及对应标记，以及 Zephyr 的 `Zephyr version <major>.<minor>.<patch>`、含 `shell_uart` 与 `idle` 的线程列表和 `serial@feb50000 (READY)`。ping 必须退出码为 0、摘要为 `0% packet loss`，且抓取到的输出里恰有 5 行来自目标地址的回包。任一步失败都会让整个用例失败，不能用重试或放宽断言来掩盖。

## 6. 运行后的清理与还原

每一次板卡运行结束后都要做清理，即使用例本身通过也一样；这些操作都不需要修改板卡 U-Boot 环境，全部在板卡原生 Linux 上以 root 进行。

1. 先停止 VM，确认没有 guest 正在使用根盘；运行期间尽量避免在客户机仍挂载时对其发送 `SIGINT` 或直接结束进程。
2. 对两个镜像先 `e2fsck -fn`：AxVisor 是在客户机仍挂载根盘的情况下退出的，通常会报告待恢复日志、位图或孤儿 inode；再用 `e2fsck -fy` 各自修复，并再次 `e2fsck -fn` 确认返回 0。
3. `e2fsck` 干净只说明文件系统结构正常，不能代替包状态检查：把两个镜像分别只读挂载到宿主并 chroot，运行 `dpkg --audit` 与 `dpkg-query -W hello`。
4. 若长时间运行或异常结束之后 APT 解析报错，可从验证过的原始备份里只恢复受影响的文件（例如 `/var/lib/apt/extended_states`），而不是整盘覆盖。
5. 清理完再按第 4.2 节重新运行 `prepare-guest-apt-repo.sh` 重建本地仓库（幂等），使下一次运行从同一状态开始。

需要完全回滚时，用准备阶段使用的同一个备份目录把两个镜像恢复回原始大小、UUID 与哈希；恢复前先确认没有 guest 正在使用根盘。

```bash
BACKUP_DIR=/guest/guest-tests-backup      # 与准备阶段使用的备份目录一致
cp -a --sparse=always "$BACKUP_DIR"/rootfs-aarch64-orangepi-jammy-0.img /guest/rootfs-aarch64-orangepi-jammy-0.img
cp -a --sparse=always "$BACKUP_DIR"/rootfs-aarch64-orangepi-jammy-1.img /guest/rootfs-aarch64-orangepi-jammy-1.img
sync
```

## 7. 文件职责

下表汇总本目录文件在用例中的角色。板卡入口 `functional/board-orangepi-5-plus-triple-guest-functional.toml` 是唯一的判定来源，放在 `functional/` 子目录里，使 `--test-case functional` 在 AxVisor 板卡发现和 CI 规划器里得到同一个用例名。

| 文件 | 角色 |
| --- | --- |
| `build-aarch64-unknown-none-softfloat.toml` | AxVisor 宿主构建组：`features` 与按顺序装载的三个 `vm_configs` |
| `linux-virtio-blk.toml` | VM1 全虚拟化 Linux（`linux`）：板卡已部署的 6.1.99 虚拟客体内核 `/guest/linux/orangepi-5-plus-6.1.99-virtualized`、显式 16550 控制台、`-0` 文件后端根盘、`virtio-net` |
| `starry-virtio-blk.toml` | VM2 全虚拟化 StarryOS（`starry`）：由本 checkout 构建的内核（打包为宿主 initramfs 内置资源）、`-1` 文件后端根盘、`virtio-net` |
| `zephyr-triple-guest-functional.toml` | VM3 全虚拟化 Zephyr（`zephyr-triple-guest-functional`）：部署的 `zephyr-triple-guest-functional` 二进制与同批 DTB、合成 vGIC 跟随宿主板级 GIC 地址 |
| `starry-guest-build.toml` | 板卡测试前手工构建 StarryOS 客户机内核的配置（`cargo xtask starry build`，`max_cpu_num = 1`） |
| `prepare-guest-apt-repo.sh` | 在板卡原生 Linux 上把缓存的 `hello` 包变成隔离 `file:` 仓库，预热客户机 APT |
| `functional/board-orangepi-5-plus-triple-guest-functional.toml` | 板卡入口：按顺序注入 `shell_check_steps`，是唯一的判定来源 |

## 8. CI 注册

本目录的用例已注册为 `.github/ci/checks/axvisor.toml` 中 `normal` 分组的自托管板卡持续集成检查 `test-axvisor-self-hosted-board-orangepi-5-plus-triple-guest-functional`，与直接运行 `cargo xtask axvisor test board` 使用同一套 `shell_check_steps` 判定。CI 项在同一个 `command` 里先用 `cargo xtask starry build --config test-suit/axvisor/normal/board-orangepi-5-plus/triple-guest-functional/starry-guest-build.toml --smp 1` 构建客户机内核，成功（`&&`）后才运行板卡入口；`pull_request_command` 是同一条带前置构建的命令，供增量 PR 与套件精确路由使用。
