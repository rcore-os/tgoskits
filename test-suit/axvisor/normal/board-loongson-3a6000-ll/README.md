# 龙芯 3A6000 的 Axvisor Linux 板卡用例

## 1. 运行边界

本用例通过板卡服务的 `LoongSon-PC` 和原生 UEFI axloader 启动 Axvisor，
再运行一个 Linux 7.1 客户机。宿主使用单核，客户机使用一个 vCPU、独立 RAM、
BusyBox initramfs、虚拟串口与独立的只读 VirtIO Block 镜像，不使用板卡原系统
分区。启动和资源所有权见[设计文档](../../../../design/axvisor-loongson-3a6000-linux.md)。

### 1.1 宿主控制台

宿主命令行由 `smoke/board-loongson-3a6000-ll.toml` 完整提供，
`earlycon=ns16550,mmio,0x1fe001e0` 负责早期输出，
`console=uart8250,mmio,0x1fe001e0` 选择已注册的运行时串口，
`axvisor.vm_start=1` 选择客户机。地址参数不创建设备资源，也不重设线路或波特率；
IRQ 仍须来自真实 ACPI 描述和平台控制器。

已检查的固件没有 SPCR，但 DSDT 提供 COMA，MADT 提供其 LIOPIC 拓扑。
只有 earlycon 输出或 `runtime console active` 不代表中断输入可用。
客户机 `[kernel].cmdline` 的 `console=ttyS0,115200` 属于虚拟串口，
不能拿来替代宿主选择。

### 1.2 客户机资源

客户机内核复用镜像仓库的 `qemu-loongarch64/linux/linux-qemu`，BusyBox 复用
`initramfs-loongarch64-busybox.cpio.gz`。`linux.toml` 使用既有受管理镜像路径，
由 Axvisor 的镜像准备流程下载、校验 SHA-256 并打包；当前发布包为 v0.0.14，
Linux 版本为 `7.1.0-rc2+`。运行记录必须保留实际镜像版本、内核和归档摘要。

`resources/prepare-board.py` 从发布 initramfs 提取 BusyBox 和原 `/init`，去掉其中
重复携带的 Linux 镜像，通过 `cargo xtask image pack-initramfs` 生成
`busybox.cpio.gz`。`resources/init` 先挂载 devtmpfs、重新打开控制台，再执行
发布版初始化脚本，避免精简归档依赖构建机创建设备节点。这样宿主上传包可保持在
板卡服务的 64 MiB 限制内。

脚本同时准备独立的 ext4 块夹具，再通过同一打包入口装入宿主基底归档。
`board-manifest.json` 绑定发布 initramfs、夹具内容、脚本、ext4 配置、工具及
产物摘要；缓存中任一产物不匹配时重新生成。`SOURCE_DATE_EPOCH` 使用正整数，
缺省为 1，用于固定夹具时间戳。
运行时后端为 `/guest/loongson-block.img`，不是构建机路径；内核、BusyBox initrd
和 VM 配置随后由既有 builtin bundle 附加。块镜像仅包含测试文件
`/axvisor-board-fixture.txt`，内容为 `AXVISOR_LOONGSON_BLOCK_FIXTURE_V1`。

## 2. 准备与验证

推荐直接使用正式板卡入口，由 `smoke/guest-builds.toml` 的 `host_prepare`
准备资源，再构建和运行宿主。构建机需要项目固定的 Rust 工具链、LoongArch
编译工具，以及 `cpio`、`mke2fs`、`debugfs`；板卡服务应已配置且目标可租用。

### 2.1 执行入口

正式入口自动准备发布镜像与块夹具，不需要本地 Linux 源码。夹具产物保存在
`target/axbuild/loongson-3a6000/`，客户机发布镜像保存在 `target/axbuild/images/`。

```bash
cargo xtask board ls
cargo xtask axvisor test board --board loongson-3a6000-ll --list
AXBUILD_NO_SNAPSHOT=1 cargo xtask axvisor test board \
  --board loongson-3a6000-ll --test-case smoke
```

如果只需提前准备 initrd 和块夹具，可设置实际 Cargo target 根目录为 `AXBUILD_TARGET_DIR`，再运行
`python3 -B test-suit/axvisor/normal/board-loongson-3a6000-ll/resources/prepare-board.py`。
不要把缺少文件、准备失败或资源缓存命中当作客户机运行成功。

### 2.2 持续集成

`.github/ci/checks/axvisor.toml` 以 `axvisor-board` 注册本用例，使用 `board` runner
和空 `cache_key`，默认每次执行一次 Linux 完整验收。它替换了龙芯 ArceOS Hello
的默认实板 CI；原 `cargo xtask arceos test board --board loongson-3a6000-ll --test-case http-boot`
仍可用于手动检查加载器与早期控制台。QEMU 的两个 Linux 客户机检查独立保留。

镜像下载沿用 `Storage::pull_image()` 和 `download_file_verified_sha256()`：自托管
宿主的 `/tmp/tgosimages` 保留下载归档，每次以 registry 的 SHA-256 校验；内容未变
就复用缓存，仅缺失或摘要不匹配时下载。可用 `TGOS_IMAGE_DOWNLOAD_DIR` 指定持久
目录。checkout 清理后即使需要重新解包，也不会重新下载相同归档；不启用 Rust
cache，也不向板卡原系统分区部署文件。

### 2.3 成功判定

正式步骤先等待宿主管理提示符，再发送 `vm console 1` 接入客户机；
只有随后执行的命令产生成功行才计入结果。用例检查实际 Linux 7.1 与
LoongArch 架构、确定性计算、RAM 文件写读和定时等待，以及 `/dev/vda` 的只读
状态、ext4 挂载和预置内容，任何 panic、失败标记或超时都应使外层任务失败。

完整交付还要求同一最终版本第二次独立板级启动，并记录两轮上传镜像摘要、
完整日志和板卡租约释放。当前开发记录在
[#2798](https://github.com/rcore-os/tgoskits/issues/2798) 和
[PR #2799](https://github.com/rcore-os/tgoskits/pull/2799)；
其中的局部静态检查、Hello 或诊断采集结果不能替代这两轮 Linux 验收。

### 2.4 虚拟化入口回归

客户机启动依赖[LVZ 世界切换与缺页契约](../../../../design/loongarch-lvz-refill-exit.md)：
只在直映入口切换 `GTLBC.TGID`，以独立输入写入两侧根，目录只保存物理基址。
缺目录先填入无效条目，再由普通 page-invalid 异常报告真实指令，供虚拟串口
和其他 MMIO 解码使用。可信 `cpu/guest-entry` 验证这些入口行为，Linux 用例
继续验证完整装配；前者通过不能替代后者。

最小 BusyBox initramfs 不保证已有 `/tmp`。RAM 用例先创建测试目录，再执行
写入、内容校验和定时等待，任何准备或功能失败仍输出原失败标记。
