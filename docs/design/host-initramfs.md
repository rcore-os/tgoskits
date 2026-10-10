# 宿主 initramfs 启动契约

## 1. 范围与共享边界

宿主 initramfs 由 `ax-fs-ng::initramfs::unpack_sources()` 解包到未发布的
`MemoryFs`。ArceOS、StarryOS 和 Axvisor 通过 `ax-runtime::fs::block::init()` 共用
解包、根文件系统选择和块设备注册流程。Axvisor VM 配置里的 `ramdisk_path`
属于 Linux 客户机，不进入宿主 `InitramfsRange`。

行为对照本地 Linux v7.1 的 `init/initramfs.c` 与 `init/main.c`：内置归档先于
外部归档解包，后者同名条目按 Linux 规则覆盖前者；非空目录遇到异类条目时
保留目录并继续解包。支持 `newc`、`crc`、零填充、串接归档
和 gzip；格式错误、中途截断、路径越界与不支持的压缩格式均终止启动，
不改从磁盘根继续。宿主镜像不是磁盘镜像，也不要求包含分区表。

## 2. 镜像来源与交接

| 来源 | 建立时机 | 交接方式 |
| --- | --- | --- |
| 内置 | 编译 `ax-runtime` 时 | Cargo 构建环境 `AX_BUILTIN_INITRAMFS` 指向非空归档；`build.rs` 追踪文件变化并将其编入镜像 |
| QEMU 直启 | 运行内核前 | AArch64/RISC-V 使用 QEMU `-initrd`、FDT `linux,initrd-start/end`；x86 使用 UEFI ESP 相邻文件，不向裸 ELF 传 Linux x86 boot protocol 参数 |
| U-Boot FIT | `bootm` 前 | FIT ramdisk 节点和 `bootargs`；someboot 从 FDT 接收物理范围 |
| UEFI 本地 | `ExitBootServices` 前 | 从启动卷读取 `EFI/BOOT/initramfs.cpio` 和 `cmdline.txt` |
| axloader v5 HTTP 服务 | 调用方推送 kernel 后 | 调用方按清单向设备推送可选 initramfs；axloader 检查长度和 SHA-256，再以版本化 UEFI 配置表交接 |

UEFI 配置表由 `host-boot-abi::BootPayload` 定义。表自身使用
`RUNTIME_SERVICES_DATA` pool，归档使用 `LOADER_DATA` 页。someboot 在退出
Boot Services 前读取配置表或 ESP 文件并记录范围；退出后核对固件内存映射，
把归档页从可分配内存中预留。配置表和 ESP 的 cmdline 不允许内部 NUL，
避免内核按 C 字符串读取时静默截断。FDT 范围连同向外扩展的边界页都须处于 RAM，
并在初始化页分配器前预留。UEFI 启动以固件内存图为 RAM 来源，FDT 只补充保留区；
LoongArch 的 UEFI 入口不再次清零已暂存的交接状态。
`ax-runtime` 解包完成后只把确知归本次镜像所有的完整物理页提交给
全局分配器；已有固件预留不能提交。归档页使用 buddy-slab 的紧凑区域入口，
按页对齐堆起点，并报告扣除分配器元数据后的实际可用字节数。小到不足以放下
元数据和一页堆空间的归档不会伪报回收；普通新增内存仍使用 2 MiB 对齐策略。
可回收归档在平台物理 RAM 范围中可见，但启动时仍由保留区遮罩，解包前不会进入分配器。
UEFI 与 FDT 同时提供镜像时，优先使用 UEFI 交接。

运行配置统一复用 ostool 的 `BootPayloadConfig`。`.qemu.toml` 和
`.board.toml` 都在顶层填写两个可选字段，路径继续支持 `${workspace}`、
`${package}` 等变量：

```toml
cmdline = "loglevel=7 init=/bin/sh"
initramfs = "${workspace}/test-suit/host-initramfs.cpio"
```

两个字段可以分别省略；它们属于运行配置，不写入 `build-*.toml`。QEMU 直启
使用 `-append` 和 `-initrd`，x86 UEFI 本地启动使用 ESP 的 `cmdline.txt` 和
`initramfs.cpio`，U-Boot 使用 `bootargs` 和 FIT ramdisk。ostool v5 推送给
axloader 时，cmdline 通过 EFI LoadOptions 传给 someboot，initramfs 仍使用
`BootPayload` 配置表。someboot 的命令行优先级为 EFI LoadOptions、旧
`BootPayload.cmdline`、ESP `cmdline.txt`、FDT `/chosen/bootargs`、编译期命令行。

## 3. 根与 PID 1

`ax-fs-ng::root::init_root_with_memory()` 只选择和发布根，不选择进程。
块设备无论是否成为根都注册到块运行时。Starry 按 `rdinit=` 或默认 `/init`
的路径可访问性决定是否使用内存根，不提前要求执行位；不存在时按照 `root=`
选择磁盘根。内存根上执行早期 init 失败不会重新挂载磁盘，而是尝试 `init=`
或 Linux 默认
`/sbin/init`、`/etc/init`、`/bin/init`、`/bin/sh`。`--` 后的词只传给 PID 1；
未知的不带点号的键值参数成为环境变量，其余未知词成为参数。ArceOS 和
Axvisor 没有 PID 1：ArceOS 在应用启动前处理显式 `root=`；Axvisor 使用延迟切根，先安装自带资源再提交。没有块设备驱动或宿主块设备时，即使继承了 `root=`，也直接从 initramfs 运行客户机，不切根。没有请求磁盘根时同样保留内存根；已接入磁盘但显式选择错误、文件系统不可用或安装失败时明确报错。
Starry 的 `known_kernel_option()` 只过滤启动链路与兼容性名单中的参数，并非 Linux
完整的内核参数注册表；Starry 未识别的 Linux 参数仍按未知参数规则传给 PID 1，
例如 `memmap=exactmap` 会进入环境变量，不能据此认为 Starry 已实现该参数的内核语义。
axbuild 将 `disk0`、匿名及直连盘视为宿主根盘接线；明确命名为其他 ID 的
guest/data drive 不要求额外准备宿主根盘。显式 QEMU 配置未接宿主根盘且未传 `--rootfs` 时，axbuild 保持无盘启动；仅继承 `root=` 不会触发下载或补盘。显式准备磁盘镜像并设置 `root=` 时必须提供宿主根盘接线。axbuild 可改写 `-drive ...` 与
`-drive=...` 根盘，并识别两种 `-device` 写法。`replace_drive_arg()` 在没有
`disk0` 接线时也可改写唯一匿名文件后端；多个匿名后端不会猜测根盘，
须显式指定 `disk0`。
宿主根盘使用 `-blockdev` 时会明确报错，需改为 `-drive id=disk0`，
不会插入第二份根盘后继续启动。`-hda`、`-sd` 等直连盘别名同样不支持补盘，
会明确报错。

## 4. 验证入口与边界

`cargo xtask image pack-initramfs test-suit/host-initramfs target/axbuild/host-initramfs/host-test.cpio`
生成供 ArceOS smoke 使用的归档。下面的 QEMU 命令均从工作区根目录执行：

```bash
cargo xtask starry test qemu --arch aarch64 --test-case qemu/host-initramfs
cargo xtask starry test qemu --arch aarch64 --test-case qemu/host-initramfs-disk-fallback
cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case qemu-host-initramfs
FEATURES=initramfs-smoke cargo xtask arceos qemu -p arceos-helloworld --arch aarch64 --qemu-config apps/arceos/helloworld/qemu-host-initramfs-aarch64.toml
AX_BUILTIN_INITRAMFS="$PWD/target/axbuild/host-initramfs/host-test.cpio" FEATURES=initramfs-smoke cargo xtask arceos qemu -p arceos-helloworld --arch aarch64 --qemu-config apps/arceos/helloworld/qemu-initramfs-aarch64.toml
FEATURES=initramfs-smoke cargo xtask arceos qemu -p arceos-helloworld --arch x86_64 --qemu-config apps/arceos/helloworld/qemu-host-initramfs-x86_64.toml
AX_BUILTIN_INITRAMFS="$PWD/target/axbuild/host-initramfs/host-test.cpio" FEATURES=initramfs-smoke cargo xtask arceos qemu -p arceos-helloworld --arch x86_64 --qemu-config apps/arceos/helloworld/qemu-initramfs-x86_64.toml
```

Starry 和 Axvisor 用例的 `host-initramfs.toml` 在运行前由 axbuild 打包。
Starry 内存根用例编译独立 AArch64 `/init`，检查 `rdinit=`、环境变量和
`--` 后参数；磁盘回退用例的归档无 `/init`。Axvisor 用例不要求 `/init`，
且 QEMU 不接入块设备。ArceOS 的 AArch64 外部用例走 `-initrd`，x86_64
外部用例走 UEFI/axloader，不使用裸 ELF 的 Linux x86 initrd 参数。

解析器的宿主测试覆盖串接归档、硬链接、权限、错误边界和不支持的压缩格式；
只测试内置镜像不能证明外部传输。FIT 的 U-Boot 实机交接、UEFI 本地 ESP
读取、axloader HTTP 推送及实体板卡上的镜像页回收仍须按各自入口核对。没有实体板卡
运行证据时标为未验证，不能以 QEMU 成功代替。

Axvisor 自带资源安装、共享根切换与内存回收重构的实际命令、确定性回归和未运行目标，记录在 [根切换验收记录](axvisor-initramfs-validation.md)。

## 5. Axvisor 自带资源

`axbuild::axvisor::bundle` 复用 newc 打包器，生成 `/guest/builtin/configs` 和 `/guest/builtin/images`。内核统一从文件加载，五种启动资源包括 kernel、DTB、BIOS、UEFI firmware 和客户机 initrd；可写客户机磁盘维持原路径。`vm_configs` 不进入 Cargo 环境或内核编译依赖。

### 5.1 安装发布

`builtin::prepare_root()` 先调用 `prepare_block_root()`，通过独立 `PreparedRoot::context()` 安装资源，再调用 `PreparedRoot::commit()`。`bundle::install_directory()` 将整个包复制到 `builtin.new`，校验配置及镜像、刷盘后发布；旧包独有文件一并删除。Ext4 用 `RenameOptions::EXCHANGE`，FAT 用 `builtin.old` 备份、目录发布和失败回滚；FAT 不保证断电原子性。后续启动先处理遗留临时及备份目录。

源目录缺失时保留已安装版本，空包清空旧资源。只读目标、空间不足或安装失败时保持原根并停止 VM 启动。`selected_configs()` 优先有效非空 `/guest/vm_default`；空目录或缺失才回退到自带配置，无效用户配置明确报错。

没有显式磁盘根或没有宿主块设备时，`prepare_root()` 保留 initramfs 根，`validate_builtin()` 只校验包内配置及 `/guest/builtin/images/**` 的文件类型、存在性和非空长度。外部绝对路径仍须是合法配置路径，但在此阶段不解析；对应 VM 加载时才报告外部资源缺失。磁盘根安装由 `install_builtin()` 在准备好的目标根上同时校验包内与外部资源，失败仍保留原根及旧包。 本次调整只限定根准备的校验范围；默认 VM 配置实际加载失败时，`init_guest_vms()` 仍按既有策略停止默认 VM 集初始化，不承诺逐 VM 容错。

### 5.2 资源生命周期

`take_initramfs()` 一次领取并清除外部启动范围；解包借用结束后只回收确知归属的完整页。内置归档、边界共享页和固件保留页保留。原始归档页回收与解包 ramfs 释放分别记录，仍在使用的无盘内存根保留到正常生命周期结束。

`MemoryFs` 对自身及挂载登记使用弱引用；文件内容由 inode 所有，挂载 lease 在旧根最后引用释放时清理目录和页缓存。已打开文件和映射继续可用，最终关闭后才释放实际页面。挂载拓扑变化移出引用后在锁外执行析构。

## 6. 磁盘根提交

`MountNamespace` 保存不可变命名空间底座，实际根通过挂载替换。`FsContext::pivot_root()` 支持普通移动和 `pivot_root(".", ".")`，提交后更新同命名空间中仍指向旧根的 root/cwd，Canonical 根上下文用于后续任务初始化；命名空间复制基于当前有效根恢复路径。

### 6.1 用户态切根

Starry 早期 init 可挂载物理块设备的 Ext4，再切根并用 `umount2(MNT_DETACH)` 脱离旧根。`mount_ext4()` 复用共享块运行时、选盘命名和分区扫描；native filesystem 登记为弱引用，重复挂载同一块区域复用超级块。物理块节点的原始 read/write 仍不支持，文件系统 I/O 经共享 native handle 后端执行。

`render_mountinfo()` 与 `render_mounts()` 只输出进程根内可达的挂载。`Location::path_from()` 无法到达当前根时跳过条目，不能将其挂载点伪装成 `/`。这与 [Linux v6.12 的 `show_mountinfo()`、`show_vfsmnt()`](https://github.com/torvalds/linux/blob/v6.12/fs/proc_namespace.c) 调用 [`seq_path_root()`](https://github.com/torvalds/linux/blob/v6.12/fs/seq_file.c#L480-L504) 并以 `SEQ_SKIP` 隐藏根外挂载的行为一致。`qemu/system/test-mount-bind` 复用真实绑定挂载，改变子进程根目录后核对 `/proc/self/mountinfo` 和 `/proc/mounts` 只有可达根与 procfs 挂载。

### 6.2 验证入口

`qemu/host-initramfs-switch-root` 在 AArch64 直接执行 mount、chdir、pivot_root 和 umount2，读取磁盘根 `/etc/alpine-release`，再从磁盘 exec `/sbin/init`。ArceOS 的 `block-root-smoke` 显式启用 Ext4 和 NVMe，验证应用启动时已读取磁盘文件。

```bash
cargo xtask starry test qemu --arch aarch64 --test-case qemu/host-initramfs-switch-root
cargo xtask image pack-initramfs test-suit/host-initramfs target/axbuild/host-initramfs/host-test.cpio
cargo xtask image pull --arch aarch64
FEATURES=block-root-smoke cargo xtask arceos qemu -p arceos-helloworld --arch aarch64 --qemu-config apps/arceos/helloworld/qemu-host-initramfs-root-aarch64.toml
cargo xtask axvisor test qemu --arch aarch64 --test-case http-control-plane
```

ArceOS 配置通过 `-device nvme,drive=disk0` 接入下载后的 Alpine 根盘，并以 `root=/dev/nvme0n1 rw` 选择该盘。应用断言根类型为块设备、`/etc/alpine-release` 非空且 `/etc/issue` 不再来自测试归档，最后输出 `HOST_DISK_ROOT_PASSED`；该完整入口已纳入 `.github/ci/checks/arceos.toml` 的 AArch64 应用与套件作业。

Axvisor HTTP 用例显式切到 NVMe 根，删除、重建并再次启动 VM，创建请求使用打包后的自带配置和镜像路径；无需旧 initramfs 或内核内嵌镜像。

## 7. 板卡启动资源

`prepare_guest_payload()` 在构建机准备启动资源，交给既有 FIT、UEFI 或 HTTP Boot 流程。构建机可取得的启动镜像放入自带包；板卡 rootfs 已提供的绝对资源路径可保留，切根后从该文件系统加载。板卡上的可写客户机根盘和用户态工作负载保持原部署方式。

### 7.1 发布镜像

`ensure_guest_image_bundles()` 识别 `${workspace}/target/axbuild/images/<image-name>/...`，复用镜像 registry、SHA-256 校验和解包缓存。kernel、DTB、BIOS、UEFI firmware 和客户机 initrd 均检查对应文件。OrangePi IVC benchmark 使用 `orangepi/ivc/guest/zephyr/zephyr-ivc-benchmark.bin`；virtio-net-peer 使用 `qemu-aarch64` 和 `initramfs-aarch64-busybox.cpio.gz`；Phytium Pi 使用 `phytiumpi`。构建目录改名时通过 `resolve_axbuild_artifact_path()` 转到实际 target 目录。

### 7.2 定制镜像

配置中的 `${env:AXVISOR_GUEST_ASSETS}` 指向构建机资产目录，必须由相应发布或板卡 CI 环境提供。ROC-RK3568-PC、OrangePi 的定制 BSP/initrd、Zephyr 控制程序和 ROCK 4D DTB 沿用各自生产者，不用通用 BusyBox 镜像代替。机器人 Linux 6.1.99 AXIVC 配置保留板卡 rootfs 上的 `/guest/linux/...` 路径；机器人 Zephyr 使用 TGOSImages 的 `scripts/apps/aka-rk3588-zephyr.sh`，生成的二进制及 DTB 放到资产目录的 `zephyr/`。该环境变量是归档输入，不进入 Axvisor 内核构建依赖。

缺失环境变量和读取到的空镜像在上传前报错；保留的绝对外部路径由 `validate_builtin()` 在准备好的 rootfs 上检查文件存在性和长度，失败时不发布新包、不提交切根。自托管 board runner 可把启动资源同步到构建机并设置资产变量，也可继续维护已声明的 rootfs 绝对路径。客户机磁盘、模型、标定与用户态程序仍按各自部署流程维护。没有对应 BSP 或设备时，FIT/HTTP Boot 的实机交接和客户机运行均标为未验证。
