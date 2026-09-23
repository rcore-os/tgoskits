# ArceOS、Zephyr、Linux 三客户机功能测例

本测例属于 AxVisor 标准板卡测试套件，位于 `test-suit/axvisor/normal/board-orangepi-5-plus/triple-vm/`，由 `cargo xtask axvisor test board` 按标准目录约定发现并运行。一个 AxVisor 同时启动三个客户机，AxVisor 持有 eMMC 和宿主文件系统，Linux 使用其中的独立 ext4 文件作为 VirtIO 块设备 `/dev/vda`。检查重点是三者持续运行时各自功能正常，且 Linux 网卡启动后宿主仍能访问 eMMC，为 Linux 的根盘提供读写。

| 客户机 | VM ID / 物理 CPU | 内存 | 功能检查 |
| --- | --- | --- | --- |
| Linux | 1 / `0x100` | 3 GiB | CPU、IPv4 ping、文件读写、APT 安装及运行 `hello` |
| ArceOS | 2 / `0x200` | 128 MiB | `help`、`uname` |
| Zephyr | 3 / `0x300` | 128 MiB | 内核版本、线程列表、设备状态 |

三个客户机均以 `image_location = "fs"` 加载启动文件。检查期间只切换控制台，不停止其他客户机。串口检查由项目通用的有序 `shell_check_steps` 框架执行，步骤定义在本测例自己的板卡配置里，不依赖其他测例脚本。

Linux 和 Zephyr 使用受控的 `passthrough` 地址空间，以保留其启动文件要求的 RK3588 板级 UART 身份；实际物理设备仍由各自的显式 `devices.passthrough` 清单决定，不会因此取得全部宿主设备。ArceOS 使用架构虚拟设备。Linux 清单不包含 eMMC 控制器，AxVisor 因而持续持有宿主文件系统并为 `/dev/vda` 提供文件后端。

## 目录内容

工作站上的构建和检查输入都在本测例目录中；板卡运行时文件按下文的独立目录准备，不与这些 TOML 一起部署。

| 文件 | 作用 |
| --- | --- |
| `build-aarch64-unknown-none-softfloat.toml` | 构建包装配置；声明 feature、target 及三个 VM 配置的路径 |
| `linux.toml`、`arceos.toml`、`zephyr.toml` | 三个客户机的 VM 配置 |
| `smoke/board-orangepi-5-plus-triple-vm.toml` | 独立板卡运行配置；固定 `board_type = "OrangePi-5-Plus"` 与全部有序检查步骤，不保存部署服务地址 |

## 首次准备

此处只说明必需输入和板上最终位置。文件的传送方式由部署环境决定。若目标目录已有文件，核对版本与内容即可，无需重新创建根镜像。

测例面向能够提供所需 CPU、eMMC 和直通网卡的 AArch64 RK3588 平台。板上需要可启动的原生 `6.1.99-rockchip-rk3588` Linux，包括 `/boot/vmlinuz-6.1.99-rockchip-rk3588`、同版本模块、完整用户态、正常的 SSH/网络以及可用的 APT/dpkg 数据库。Linux 自动检查要求 `orangepi@orangepi5plus:~$` 提示符及 `orangepi` sudo 密码；提示符已在板卡配置中固定，修改它只会改变提示符匹配，不会修改测例使用的 sudo 密码。

准备以下六个运行时文件，统一放在板上的 `/home/orangepi/axvisor-tests/triple-vm-test/`：

| 文件 | 来源及要求 |
| --- | --- |
| `linux-Image` | 原生 `/boot/vmlinuz-6.1.99-rockchip-rk3588` 的完整副本 |
| `linux-initrd.img` | 与该内核版本匹配的初始内存盘，须在挂载根盘前加载 `virtio_mmio`、`virtio_blk`，并包含 ext4 文件系统检查工具 |
| `linux-rootfs.ext4` | 一个独立、可写、完整的 ext4 根文件系统；含 Linux 用户态、用户目录、SSH 和 APT/dpkg 数据库，建议容量 8 GiB |
| `arceos.bin` | AArch64 ArceOS Shell，提供 `help`、`uname`、`exit`，提示符 `arceos:/$`，加载地址和入口均为 `0x80200000` |
| `zephyr.bin` | AArch64 Zephyr Shell，提供 `kernel version`、`kernel thread list`、`device list`，加载地址 `0x40000000`，入口 `0x4000100c` |
| `zephyr.dtb` | 与 `zephyr.bin` 同一构建产生、匹配虚拟 UART、中断控制器与内存布局的 DTB |

原生内核未内置所需的两个 VirtIO 驱动，因此专用 initrd 必须包含与该内核匹配的 `virtio_mmio`、`virtio_blk` 模块。ArceOS、Zephyr 的三个已构建文件也需要另行提供。运行测例前，上述六个文件必须实际存在，不能只建立空目录；CI 不会生成或覆盖它们。

### 为什么需要专用 initrd

板上原生启动使用 `/boot/initrd.img-6.1.99-rockchip-rk3588`，对应直接访问 eMMC 的原生根分区。本测例使用同一个原生内核的副本，但 Linux 的根设备变为 AxVisor 提供的 `/dev/vda`。这个内核的配置既未内置 `VIRTIO_MMIO`、`VIRTIO_BLK`，也未将它们编译为模块，所以必须另外编译与该内核匹配的两个模块，并确保在挂载根文件系统之前加载。

`linux-initrd.img` 是与上述原生内核匹配的专用 initrd：它必须在挂载 `/dev/vda` 前加载两个 VirtIO 模块，且包含 `fsck`、`fsck.ext4`。它不是把原生 initrd 换个文件名；板上原生 `/boot` 文件保持原样。

`cmdline` 中的 `root=/dev/vda` 仅指定要挂载哪个设备，不能创建 `/dev/vda` 或补齐缺失的驱动。因此，在当前内核配置下，保持原生 initrd 不变而只修改 `cmdline`，无法完成本测例的 VirtIO 根盘启动。若将这两个驱动内置到另一个内核，或给原生 initrd 加入匹配模块，启动方案可以改变，但那也需要相应内核或 initrd 的改动，不能只改 `cmdline`。

`linux.toml` 的 `kernel_path`、`ramdisk_path` 和虚拟块设备 `rootdisk.path` 分别指向目录中的 `linux-Image`、`linux-initrd.img`、`linux-rootfs.ext4`；ArceOS 和 Zephyr 的配置分别指向各自的内核文件，Zephyr 还指定 `zephyr.dtb`。本目录的 TOML 在工作站由构建工具读取，无需放到板上。若改变部署目录，须同步修改所有相关绝对路径并重新构建 AxVisor。

Linux 内核和 initrd 被加载到内存；8 GiB 根镜像仍保留在 eMMC，由 AxVisor 提供运行时 I/O。Linux 使用 `root=/dev/vda` 挂载该 ext4 文件，而不是直接访问宿主的 eMMC 分区。ArceOS 和 Zephyr 不使用磁盘或 initramfs；ArceOS 的 DTB 由 AxVisor 生成。

如果板卡网络不能访问软件源，根镜像的 `/var/cache/apt/archives/` 中还需要预置与 APT 索引版本和 SHA-256 一致的 `hello` 安装包。APT 会先使用缓存；缺包时会尝试联网，离线环境下检查将失败。本测例验证包的安装、运行和原有状态恢复，不将在线下载作为通过条件。

## 运行

### 标准自动路径

板上六个文件与配置中的路径一致后，可在 TGOSKits 仓库根目录用测试套件的标准入口运行本测例：

```bash
# 自动构建并启动三个客户机，依次执行板卡配置中的全部检查。
cargo xtask axvisor test board --board orangepi-5-plus-triple-vm --test-case smoke
```

测试工具按目录约定发现本测例：最近的构建包装 `build-aarch64-unknown-none-softfloat.toml` 提供构建输入，`smoke/board-orangepi-5-plus-triple-vm.toml` 提供板卡类型与检查步骤。CI 使用同一条命令，并按当前运行环境附加本地板卡服务参数（`--board-type`、`--server`、`--port`）；这类板型与服务地址属于运行环境，不写入测例文件。

### 可选环境变量

两个检查参数可通过环境变量覆盖，由 ostool 在运行时展开；未设置时展开为空字符串，命令按空值处理，默认行为不变：

```bash
# Linux 客户机 ping 的目标；改为客户机可达地址。不设置时使用默认网关。
export TRIPLE_PING_TARGET=192.168.1.1
# 仅供 Linux APT 步骤使用的 HTTP 代理；不设置时不使用代理。
export TRIPLE_APT_PROXY=http://proxy.example:8080
```

不设置这两个变量时，检查仍会验证五次 ping 全部到达、文件写入读回、以及 `hello` 的安装、运行与状态恢复，因此无需额外变量即可保持默认强度。

### 手动路径（不含自动检查）

若只想启动三个客户机并保留交互控制台，直接用 `axvisor board`：它只构建并启动，是否执行检查完全取决于所用板卡配置是否包含 `shell_check_steps`。

```bash
# 用一个只含 board_type（不含 shell_check_steps）的本地板卡配置启动，不跑自动检查。
cargo xtask axvisor board \
  --config test-suit/axvisor/normal/board-orangepi-5-plus/triple-vm/build-aarch64-unknown-none-softfloat.toml \
  --board-config /absolute/path/to/local-board.toml \
  --board-type OrangePi-5-Plus
```

本地板卡配置可放在仓库外，避免在测例目录保存部署环境信息；其内容只需 `board_type = "OrangePi-5-Plus"` 等启动设置。若省略 `--board-config`，测试工具会在工作区根目录使用默认板卡配置。

检查步骤先确认三个 VM 同时为 `running`，按 ArceOS、Zephyr、Linux 的顺序检查，再次确认三者仍在运行。期望 ArceOS 2/2、Zephyr 3/3、Linux 4/4。Linux 检查包含五次 ping、文件写入和读回、安装与运行 `hello`；成功路径中，若 `hello` 原先未安装，检查后卸载，若已安装则重装原版本并保留。单项失败、超时或 VM 状态不符时立即终止后续检查并返回非零。若 APT 检查中途失败，重新运行前须核对根镜像中的 `hello` 状态。CI 在命令成功返回后才输出 `TRIPLE_VM_TEST_PASS` 供日志识别，失败时不输出该标记。

## 检查结束

自动检查结束后串口会话退出；客户机是否继续运行由板卡后续上电和启动流程决定。手动启动时三个客户机持续运行，结束时先让 Linux 执行 `sync` 并正常关机，再停止其余客户机和宿主。板卡连接、上电及租约回收遵循项目通用流程。
