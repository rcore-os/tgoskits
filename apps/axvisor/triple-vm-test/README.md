# ArceOS、Zephyr、Linux 三客户机功能测例

本测例由一个 AxVisor 同时启动三个客户机。AxVisor 持有 eMMC 和宿主文件系统，Linux 使用其中的独立 ext4 文件作为 VirtIO 块设备 `/dev/vda`。检查重点是三者持续运行时各自功能正常，且 Linux 网卡启动后，宿主仍能访问 eMMC，为 Linux 的根盘提供读写。

| 客户机 | VM ID / 物理 CPU | 内存 | 功能检查 |
| --- | --- | --- | --- |
| Linux | 1 / `0x100` | 3 GiB | CPU、IPv4 ping、文件读写、APT 安装及运行 `hello` |
| ArceOS | 2 / `0x200` | 128 MiB | `help`、`uname` |
| Zephyr | 3 / `0x300` | 128 MiB | 内核版本、线程列表、设备状态 |

三个客户机均以 `image_location = "fs"` 加载启动文件。检查期间只切换控制台，不停止其他客户机。启动、配置、串口通信和判定程序都位于本测例目录，不依赖其他测例脚本。

Linux 和 Zephyr 使用受控的 `passthrough` 地址空间，以保留其启动文件要求的 RK3588 板级 UART 身份；实际物理设备仍由各自的显式 `devices.passthrough` 清单决定，不会因此取得全部宿主设备。ArceOS 使用架构虚拟设备。Linux 清单不包含 eMMC 控制器，AxVisor 因而持续持有宿主文件系统并为 `/dev/vda` 提供文件后端。

## 首次准备

此处只说明必需输入和板上最终位置。文件的传送方式由部署环境决定。若目标目录已有文件，核对版本与内容即可，无需重新创建根镜像。

测例面向能够提供所需 CPU、eMMC 和直通网卡的 AArch64 RK3588 平台。板上需要可启动的原生 `6.1.99-rockchip-rk3588` Linux，包括 `/boot/vmlinuz-6.1.99-rockchip-rk3588`、同版本模块、完整用户态、正常的 SSH/网络以及可用的 APT/dpkg 数据库。Linux 自动检查当前要求 `orangepi@orangepi5plus:~` 提示符及 `orangepi` sudo 密码；修改 `--linux-prompt` 只会改变提示符匹配，不会修改运行器使用的 sudo 密码。

准备以下六个文件，统一放在板上的 `/home/orangepi/axvisor-tests/triple-vm-test/`：

| 文件 | 来源及要求 |
| --- | --- |
| `linux-Image` | 原生 `/boot/vmlinuz-6.1.99-rockchip-rk3588` 的完整副本 |
| `linux-initrd.img` | 与该内核版本匹配的初始内存盘，须在挂载根盘前加载 `virtio_mmio`、`virtio_blk`，并包含 ext4 文件系统检查工具 |
| `linux-rootfs.ext4` | 一个独立、可写、完整的 ext4 根文件系统；含 Linux 用户态、用户目录、SSH 和 APT/dpkg 数据库，默认容量 8 GiB |
| `arceos.bin` | AArch64 ArceOS Shell，提供 `help`、`uname`、`exit`，提示符 `arceos:/$`，加载地址和入口均为 `0x80200000` |
| `zephyr.bin` | AArch64 Zephyr Shell，提供 `kernel version`、`kernel thread list`、`device list`，加载地址 `0x40000000`，入口 `0x4000100c` |
| `zephyr.dtb` | 与 `zephyr.bin` 同一构建产生、匹配虚拟 UART、中断控制器与内存布局的 DTB |

原生内核未内置所需的两个 VirtIO 驱动，因此还需要与该内核匹配的 `virtio_mmio.ko`、`virtio_blk.ko`，以及具有该内核配置、`Module.symvers` 和生成版本头的构建树。ArceOS、Zephyr 的三个已构建文件也需要另行提供。准备完成后，上述六个运行时文件必须实际存在，不能只建立空目录。

### 为什么需要专用 initrd

板上原生启动使用 `/boot/initrd.img-6.1.99-rockchip-rk3588`，对应直接访问 eMMC 的原生根分区。本测例使用同一个原生内核的副本，但 Linux 的根设备变为 AxVisor 提供的 `/dev/vda`。这个内核的配置既未内置 `VIRTIO_MMIO`、`VIRTIO_BLK`，也未将它们编译为模块，所以必须另外编译与该内核匹配的两个模块，并确保在挂载根文件系统之前加载。

`linux-initrd.img` 是本测例从完整 Linux 根镜像的用户态重新生成的 initrd：镜像内安装两个模块并运行 `depmod`，将模块加入 initramfs 的启动加载清单，明确指定 ext4 文件系统类型，使生成的 initrd 包含 `fsck`、`fsck.ext4`。它不是把原生 initrd 换个文件名；板上原生 `/boot` 文件保持原样。

`cmdline` 中的 `root=/dev/vda` 仅指定要挂载哪个设备，不能创建 `/dev/vda` 或补齐缺失的驱动。因此，在当前内核配置下，保持原生 initrd 不变而只修改 `cmdline`，无法完成本测例的 VirtIO 根盘启动。若将这两个驱动内置到另一个内核，或给原生 initrd 加入匹配模块，启动方案可以改变，但那也需要相应内核或 initrd 的改动，不能只改 `cmdline`。

`configs/linux.toml` 的 `kernel_path`、`ramdisk_path` 和虚拟块设备 `rootdisk.path` 分别指向目录中的 `linux-Image`、`linux-initrd.img`、`linux-rootfs.ext4`；ArceOS 和 Zephyr 的配置分别指向各自的内核文件，Zephyr 还指定 `zephyr.dtb`。`configs/` 在工作站由构建工具读取，无需放到板上。若改变部署目录，须同步修改所有相关绝对路径并重新构建 AxVisor。

Linux 内核和 initrd 被加载到内存；8 GiB 根镜像仍保留在 eMMC，由 AxVisor 提供运行时 I/O。Linux 使用 `root=/dev/vda` 挂载该 ext4 文件，而不是直接访问宿主的 eMMC 分区。ArceOS 和 Zephyr 不使用磁盘或 initramfs；ArceOS 的 DTB 由 AxVisor 生成。

## 脚本用法

`build-virtio-modules.sh` 在具备 AArch64 交叉编译器的工作站执行，输入是与客户机内核匹配、已生成 `Module.symvers` 和 `include/generated/utsrelease.h` 的 Linux 构建树，输出两个 `.ko` 文件：

```bash
cd apps/axvisor/triple-vm-test
LOCALVERSION=-rockchip-rk3588 ./build-virtio-modules.sh \
  /absolute/path/to/prepared-linux-build /absolute/path/to/module-output
```

默认使用 `aarch64-linux-gnu-` 工具链前缀，可用 `CROSS_COMPILE` 指定其他前缀。`LOCALVERSION` 必须与准备构建树时采用的版本设置一致；用 `modinfo -F vermagic` 核对两个产物都匹配 `6.1.99-rockchip-rk3588`。仅匹配版本字符串不足以证明模块可加载，还要由目标原生内核实际验证。

`prepare-rootfs.sh` 在板上的**原生 Linux** 中以 root 执行，输入是尚不存在的目标目录、原生内核文件、内核版本及包含上述两个模块的目录。本测例需要第四个模块目录参数；不能在 Linux 客户机或 AxVisor Shell 中运行：

```bash
sudo ./prepare-rootfs.sh \
  /home/orangepi/axvisor-tests/triple-vm-test \
  /boot/vmlinuz-6.1.99-rockchip-rk3588 \
  6.1.99-rockchip-rk3588 \
  /absolute/path/to/module-output
```

脚本创建目标目录，将原生内核复制为 `linux-Image`，建立默认 8 GiB 的稀疏 ext4 根镜像，复制原生系统用户态，仅在镜像内设置 `/dev/vda` 根挂载并安装模块，然后运行 `depmod`、`mkinitramfs` 生成 `linux-initrd.img`。结束前会卸载准备阶段的挂载、执行 `e2fsck -fn` 并比较内核副本与原文件。需要调整根镜像容量时，在首次执行时通过 `ROOTFS_SIZE` 环境变量指定。已有的目标目录不会被覆盖；脚本不生成 ArceOS、Zephyr 文件，也不修改原生 `/boot`、模块树或 `/etc/fstab`。

如果板卡网络不能访问软件源，准备镜像时还必须确保 `hello` 的匹配版本安装包位于镜像的 `/var/cache/apt/archives/`。APT 会先使用缓存；缺包时它会尝试联网，离线环境下检查将失败。预置的 `.deb` 应与镜像中的 APT 索引所列版本和 SHA-256 一致。此测例验证包的安装、运行和原有状态恢复，不将在线下载作为通过条件。

`run.sh` 是本测例入口：它调用 `cargo xtask axvisor board`，并由本目录的 `scripts/runner.py` 组织检查；`scripts/console.py` 负责串口通信，`scripts/shell_checks.py`、`scripts/linux_checks.py` 负责各客户机判定。`scripts/` 中的模块由入口调用，无需分别执行。具体调用方式和参数见下节。

## 运行

板上六个文件与配置中的路径一致后，可在 TGOSKits 仓库根目录调用本测例入口：

```bash
# 默认：启动三个客户机，依次执行全部自动检查。
apps/axvisor/triple-vm-test/run.sh --board-type <board-type>

# CI：完成相同检查后关闭客户机、退出 AxVisor 并释放板卡。
apps/axvisor/triple-vm-test/run.sh --ci --board-type <board-type>

# 只启动客户机，保留交互控制台，不执行自动检查。
apps/axvisor/triple-vm-test/run.sh --interactive --board-type <board-type>

# 查看完整参数列表；不会申请板卡。
apps/axvisor/triple-vm-test/run.sh --help
```

`run.sh` 使用本目录的 `configs/build.toml`。板型必须由命令行指定；运行器会为当前进程生成最小的临时板卡配置，结束后自动删除，不在测例目录保存部署环境信息。

| 参数 | 作用 |
| --- | --- |
| `--board-type` | 必填；指定 ostool-server 中的板卡类型 |
| `--server`、`--port` | 可选；覆盖 ostool 全局配置中的板卡服务地址和端口 |
| `--ping-target` | Linux 客户机可达的 IPv4 地址；省略时 ping 其默认网关 |
| `--apt-proxy` | 仅为当次 Linux APT 命令指定 HTTP 代理，不影响 ping |
| `--linux-prompt` | Linux Shell 提示符；默认 `orangepi@orangepi5plus:~`，不改变 sudo 密码 |
| `--timeout` | Linux 单条命令的超时秒数；默认 600 |
| `--ci` | 自动检查后关闭三个客户机并退出；释放板卡后才输出最终结果并返回 |
| `--interactive` | 只启动三个客户机，跳过自动检查，不输出测试通过标记 |
| `--help` | 打印参数帮助并退出，不启动测例 |

直接运行底层 `cargo xtask axvisor board` 只负责构建和启动，不会执行 `run.sh` 的自动检查。

运行器先确认三个 VM 同时为 `running`，按 ArceOS、Zephyr、Linux 的顺序检查，再次确认三者仍在运行。期望 ArceOS 2/2、Zephyr 3/3、Linux 4/4，最后输出 `TRIPLE_VM_TEST_PASS`。Linux 检查包含五次 ping、文件写入和读回、安装与运行 `hello`；若 `hello` 原先未安装，检查后卸载，若已安装则重装原版本并保留。命令失败、超时或 VM 状态不符时输出 `TRIPLE_VM_TEST_FAIL` 并返回非零。默认模式检查结束后客户机继续运行，键盘恢复手工输入。

CI 使用同一组功能检查，不增加额外的客户机测试。检查成功后，运行器先让 Linux 执行 `sync` 并正常关机，再停止 ArceOS 和 Zephyr；只有确认三个 VM 均为 `stopped`、AxVisor 正常退出且板卡会话已经释放，才输出最终的 `TRIPLE_VM_TEST_PASS` 并返回零。CI 运行前应按“首次准备”提供六个运行时文件；每次运行不会重新制作或覆盖根镜像。

SSH 文件读写由工作站单独核对：在 Linux 客户机中用 `ip -brief addr` 确认 IP，使用 SSH 登录，向家目录临时文件写入、读回比较、`sync` 并删除。该结果不包含在 `TRIPLE_VM_TEST_PASS` 中。

## 检查结束

检查结束后三个客户机仍运行。Linux 持续写入根镜像；结束运行时先在 Linux 中执行 `sync` 并正常关机，确认磁盘 I/O 已结束，再停止其余客户机和宿主。板卡连接、上电及租约回收遵循项目通用流程。
