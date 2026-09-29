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
Axvisor 没有 PID 1：显式 `root=` 选择磁盘，否则存在 initramfs 就选择内存根。
Starry 的 `known_kernel_option()` 只过滤启动链路与兼容性名单中的参数，并非 Linux
完整的内核参数注册表；Starry 未识别的 Linux 参数仍按未知参数规则传给 PID 1，
例如 `memmap=exactmap` 会进入环境变量，不能据此认为 Starry 已实现该参数的内核语义。
axbuild 将 `disk0`、匿名及直连盘视为宿主根盘接线；明确命名为其他 ID 的
guest/data drive 不要求额外准备宿主根盘。Axvisor 显式 `root=` 但未接入
可识别的宿主根盘会在配置阶段报错。axbuild 可改写 `-drive ...` 与
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
