# K3 双客户机启动测试

本用例在 BananaPi-K3 上启动 Linux 和 StarryOS 两个虚拟化客户机，验证串口交互和双 vCPU 启动，再切回 Linux 确认其仍能响应。用例通过 `cargo xtask axvisor test board` 手动执行，不注册到 CI。

## 1. 板上资源

`linux.toml` 与 `starryos.toml` 引用 `/guest/linux/`、`/guest/starry/` 下已有的文件。K3 原生 Linux 在 UFS 上保存这些资源及 `/guest/axvisor-host.cpio.gz`；后者是包含相同目录的 AxVisor 内存根。当前 AxVisor 没有 K3 UFS 驱动，因此运行器使用该归档的本地缓存，将它放入 FIT 并交给宿主解包。客户机仍按配置中的绝对路径加载，测试不直通 UFS。

### 1.1 镜像路径

两份客户机各使用一个内核、一个设备树和一个初始内存文件系统。内核入口均为 `0x90200000`，由各自独立的 512 MiB 客户机内存承载，地址相同不会共享物理内存。

| 客户机 | 内核 | 设备树 | 初始内存文件系统 |
| --- | --- | --- | --- |
| Linux | `/guest/linux/Image` | `/guest/linux/guest.dtb` | `/guest/linux/initramfs.cpio.gz` |
| StarryOS | `/guest/starry/starryos.bin` | `/guest/starry/guest.dtb` | `/guest/starry/initramfs.cpio.gz` |

两份初始内存文件系统均须提供 `/init`、`/bin/busybox` 和已挂载的 `/sys`；宿主内存根另须提供 `/bin/sh`。检查步骤等待 Linux 的 `~ #`、StarryOS 的 `starry-bpi>` 提示符；使用不同初始化脚本时同步调整 `smoke/board-bananapi-k3.toml` 中的 `shell_prefix`。更新板上镜像后，应同步重打包 `/guest/axvisor-host.cpio.gz` 并刷新本地缓存。

### 1.2 CPU 与设备树

`build-riscv64gc-unknown-none-elf.toml` 允许 16 核宿主；CPU0 留给宿主，两个客户机仅绑定支持虚拟化的 X100 核。`phys_cpu_ids` 表达客户机 hart ID，`phys_cpu_sets` 表达宿主 CPU 亲和，两者与设备树须保持一致。

| 客户机 | VM ID | 客户机 hart ID | 宿主 CPU | 预期在线 CPU |
| --- | --- | --- | --- | --- |
| Linux | 1 | 1、2 | 1、2 | `0-1` |
| StarryOS | 2 | 3、4 | 3、4 | `0-1` |

客户机设备树使用虚拟 PLIC、NS16550 串口和 24 MHz 时基，串口须有 `serial0` 别名。PLIC 的 `interrupts-extended` 依照本客户机 hart 顺序，逐个配置机器上下文占位和监管者中断 9。两份配置的 `passthrough` 均为空，只使用虚拟设备。

## 2. 手动运行

`smoke/board-bananapi-k3.toml` 指定 `BananaPi-K3` 板卡类型，使用 `earlycon=sbi axvisor.vm_start=1,2` 同时启动两个客户机。`initramfs` 选择缓存的板上内存根，运行器在独立归档中追加本次客户机配置和宿主符号，不修改缓存输入。

### 2.1 命令入口

首次运行或板上资源更新后，将 `/guest/axvisor-host.cpio.gz` 缓存到仓库根目录下的 `target/axbuild/board-assets/bananapi-k3/host-root.cpio.gz`。这是预先准备的输入缓存，使用 `--target-dir` 或 `CARGO_TARGET_DIR` 切换构建输出目录时仍保留在此位置；`smoke/guest-builds.toml` 的校验与 `smoke/board-bananapi-k3.toml` 的 `initramfs` 均读取该文件。例如在 K3 原生 Linux 可通过 SSH 访问时，可以在仓库根目录执行下面的命令；传输后核对板上与本地 SHA-256 一致。

```bash
mkdir -p target/axbuild/board-assets/bananapi-k3
scp root@<K3_IP>:/guest/axvisor-host.cpio.gz \
  target/axbuild/board-assets/bananapi-k3/host-root.cpio.gz
sha256sum target/axbuild/board-assets/bananapi-k3/host-root.cpio.gz
```

缓存存在后，通过正式板卡入口运行。服务地址与端口作为命令参数提供，不写入测试配置；`smoke/guest-builds.toml` 只检查缓存是否存在，不下载镜像、构建客户机或部署文件，缺失时在宿主构建和板卡分配之前失败。

```bash
cargo xtask axvisor test board --board bananapi-k3 --list
cargo xtask axvisor test board --board bananapi-k3 \
  --server <板卡服务地址> --port <端口>
```

`board_type` 与 `--board` 含义不同：前者是服务端资源类型 `BananaPi-K3`，后者是测试配置名 `bananapi-k3`。

### 2.2 结果判定

运行器依次接入两个客户机控制台，检查 `uname -m` 为 `riscv64`，以及 `/sys/devices/system/cpu/online` 为 `0-1`。只有 Linux 和 StarryOS 分别输出 `K3_LINUX_READY`、`K3_STARRY_READY`，且重新接入 Linux 后输出 `K3_MULTI_GUEST_PASSED`，整项检查才通过；任何检查失败均输出 `K3_MULTI_GUEST_FAILED` 并使运行器失败。

本用例仅验证多客户机启动与串口交互，不进行物理设备直通、网络、磁盘读写或持久化验证。缺少板上资源或宿主无法访问这些路径时，应修复板卡环境后重跑，不能把配置发现成功当作实板运行通过。
