# OrangePi 5 Plus 上的 StarryOS 自举编译

这个应用从 `macos-selfbuild` 的 AArch64 编译闭环演化而来，但使用真实的
OrangePi 5 Plus、直连串口和 Debian 12 arm64 glibc chroot。物理板启动不经过
OSTool/`cargo xtask ... board`，也不抢 U-Boot 的 0 秒输入窗口；主机先打开串口
监控，再由板载 Linux 临时选择已经校验的一次性 StarryOS 启动脚本。
它是 `apps/starry/orangepi-5-plus-selfbuild` 下的独立应用，不替换也不删除
macOS AArch64 或 x86_64 适配，不使用 KVM/HVF。

当前正确性目标是“编译闭环”：StarryOS 内完整编译 `starryos`，产出 AArch64
ELF 和 raw binary，回到 Linux 后取回产物并验证 SHA-256。暂不把第二代内核
自动设为下一次启动内核。

## 恢复模型

chroot 里的编译命令由 `timeout` 限制为 21,600 秒；正常结束后主动 reboot。
本应用不启用硬件自动复位。如果内核死锁导致 `timeout` 无法运行，串口监控会在
22,800 秒后报告失败，但板卡仍需人工复位。U-Boot 必须默认
进入 Linux：若当前 `/boot/boot.scr` 是已知的
Starry 脚本且存在经过内容检查的 `boot.scr.tgoskits-backup`，准备脚本会先保留
当前脚本再恢复 Linux 备份；遇到未知 `bootcmd` 或无法验证的备份时会停止，
绝不猜测和覆盖。

`stage_starry_boot.sh` 从板端当前 Linux 的 `/sys/firmware/fdt` 取得实际 DTB，
与主机编译的内核组成带 SHA-256 节点的 FIT，并反向提取 FIT 中的 kernel/DTB
与输入逐字节比较。部署先写 `.new`、校验、`sync`，再在相同文件系统原子改名为
`/image.fit`、`/boot/boot-starryos-emmc.scr` 和 `/boot/starryEnv.txt`；该阶段不修改
`/boot/boot.scr`。完整入口在串口监控就绪后才临时替换 `/boot/boot.scr` 并重启，
StarryOS shell 出现后执行的第一步是恢复并校验 Linux 备份，然后才开始编译。
`stage_starry_boot.sh` 将 Starry 脚本补零到 Linux 脚本的文件长度；镜像头中的
有效负载长度和校验保持不变。`restore_linux_boot.sh` 核对已知脚本与相同长度，
再用 `dd conv=notrunc` 覆盖原文件、逐字节比较并同步，保留已有 inode 尺寸和
extent 映射。U-Boot 不重放 ext4 日志，因此恢复不能依赖截断、扩展或目录改名
产生的日志内元数据。验收须包含无编译写入的短运行，并实际重启回 Linux；
Starry 中两个 SHA-256 相同只证明当前可见内容一致。

一次性环境使用 Linux 根分区的 GPT `PARTUUID` 作为 StarryOS 的 `root=`；不能
复用 Linux 的 `/dev/mmcblkN` 编号，因为本板上 Linux、StarryOS 与 U-Boot 的
MMC 枚举顺序不同。OrangePi 5 Plus 的 eMMC 在当前 U-Boot 中实测为 `mmc 1`。

## 手动连接串口

在主机交互终端运行：

```bash
apps/starry/orangepi-5-plus-selfbuild/connect_serial.sh
```

入口直接使用 picocom，默认设备为 `/dev/ttyACM0`、波特率为 1,500,000；
可以用一个位置参数指定其他串口设备。连接前先关闭主机终端遗留的括号粘贴模式，
避免粘贴时把 `ESC[200~` 和 `ESC[201~` 一起发到板端，被 shell 当成命令。
退出时先按 Ctrl+A，再按 Ctrl+X。运行自动板卡流程前先退出该串口会话。

板端默认 `/bin/sh` 可能是没有命令行编辑功能的 dash。需要退格、方向键和
括号粘贴处理时，在 StarryOS 控制台运行：

```sh
bash --noprofile --norc -i
```

这会启动交互 Bash，两个启动文件参数用于避免执行板载 Linux 的初始化脚本；
`exit` 返回原来的 shell。Linux Bash 开启的终端粘贴模式也可能在重启进入
StarryOS 的 dash 后遗留，因此同一串口会话跨系统启动时要留意 shell 类型。

`tests/script_smoke.sh` 包含使用真实 picocom 和两对 PTY 的粘贴回归测试；
运行该项检查需要主机安装 picocom。

## 手动启动 SSH

使用带 OpenSSH 和既有用户公钥的 Linux rootfs 时，在 StarryOS 的 root
串口控制台执行：

```sh
ip -brief address
mkdir -p /run/sshd
/usr/sbin/sshd -t
/usr/sbin/sshd -4
```

第一条查看板端实际地址，后续命令创建隔离目录、检查配置并启动 IPv4 SSH 服务。
然后在主机运行 `ssh orangepi@<BOARD_IP>`。StarryOS 与 Linux 的 DHCP 地址可能
不同；主机 `enp3s0` 的地址也不是板子的地址。

## 一次性准备可复用 glibc rootfs

先让板卡进入默认 Linux，并从 Linux 控制台得到 IP。自动入口需要独占直连串口，
因此运行前只检查设备存在，不能让 `picocom`、`tio`、`minicom` 或其他进程保持
打开：

```bash
test -c /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0
```

另一个终端执行：

```bash
apps/starry/orangepi-5-plus-selfbuild/provision_rootfs.sh \
  --host <BOARD_IP>
```

脚本在板端创建并复用
`/opt/starry-orangepi5plus-selfbuild/rootfs`，其中包含：

- Debian 12 arm64 glibc 用户态；
- 仓库锁定的 Rust nightly、`rust-src`、LLVM tools、`cargo-binutils` 和
  `gen_ksym`；
- C/C++、Clang/LLVM、CMake、binutils、U-Boot tools、`perf` 等构建工具；
- `lwprintf-rs` 构建脚本所需的 `aarch64-linux-musl-gcc` 兼容命令（在该 glibc
  rootfs 中指向 `aarch64-linux-gnu-gcc`，用于查询并使用同一 AArch64 sysroot）；
- Cargo registry/git cache，以及 `-Z build-std=core,alloc` 所需的 nightly
  sysroot crate；
- 当前工作区精确快照及 commit/ref/dirty/toolchain/SHA-256 元数据；
- `/work/targets` 和 `/output/runs` 持久目录。

源码按 archive SHA-256 存在版本目录中，rootfs 和旧架构应用都不会被删除。
相同 rootfs 后续只更新工具链、Cargo cache 和源码快照。

## 跑完整正确性闭环

```bash
apps/starry/orangepi-5-plus-selfbuild/run_selfbuild.sh \
  --host <BOARD_IP> \
  --skip-provision
```

也可以省略 `--skip-provision`，让入口先幂等检查 rootfs。主运行命令会独占打开
1,500,000 波特率串口，原子切换一次启动项、自动发送 guest 命令、等待 Linux
恢复并取回产物。如果板载 Linux 安装了一次性启动脚本却没有实际进入重启，主机
会立即恢复 Linux 默认启动脚本并失败退出。

入口默认通过 `stage_starry_boot.sh` 调用 `cargo xtask starry build` 构建
StarryOS 种子内核并生成 raw AArch64 boot image；它只是启动板端自举的 bootstrap。已有
经过验证的种子内核时可加 `--skip-boot-build`。

板端 guest 每轮冷 target 先执行 `cargo build -p tg-xtask`，再直接调用生成的
`$CARGO_TARGET_DIR/debug/tg-xtask starry build --config ...` 编译 StarryOS。Linux
和 StarryOS 都继承系统默认 CPU affinity、Cargo jobs、rustc 与 Rayon 线程策略，
不绑核也不设置并行度上限。日志每 60 秒输出当前阶段耗时、阶段/累计编译单元数和
不同 crate 数，便于查看两侧大概进度。内核日志为 Warn；FAIL/PASS 等验收信号
仍可见。完整成功需要先在串口看到 guest PASS：

```text
===STARRY-ORANGEPI5PLUS-SELFBUILD-PASS run=... parallelism=system-default elapsed=...===
```

板卡随后自动回到 Linux；完整入口会自行取回并验证产物。也可以单独复核：

```bash
apps/starry/orangepi-5-plus-selfbuild/fetch_artifacts.sh \
  --host <BOARD_IP> --run-id <RUN_ID>
```

主机最后应输出：

```text
===STARRY-ORANGEPI5PLUS-SELFBUILD-HOST-PASS run=...===
```

取回目录为 `target/starry-orangepi5plus-selfbuild/artifacts/<run-id>/`，包含
`starryos.elf`、`starryos.bin`、`SHA256SUMS`、源码元数据、耗时和完整日志。

## 内核冷编译与限时采样

只计量 `starryos` 编译时，`init-kernel-selfbuild.sh` 先恢复 Linux 启动入口，
再进入已有 glibc chroot；`guest-kernel-selfbuild.sh` 调用已经准备好的
`/usr/local/bin/tg-xtask`。入口要求源码目录没有 `target`，并在计时前验证
匹配工具链的 `llvm-objcopy`。源码快照、Rust 工具链和任务工具散列都记录在
日志中；成功必须同时产出 ELF 和 BIN，不能以 Cargo 的 `Finished` 代替闭环。
已有编译产物应先在 Linux 中改名保留，每轮使用不同的 `run-id`。

在 StarryOS 的外层 shell 执行一次入口，参数用于区分本轮日志和产物：

```sh
sh /opt/starry-orangepi5plus-selfbuild/init-kernel-selfbuild.sh kernel-cold-01
```

当前版本不包含内核采样配置；上述内核冷编译仍记录时间、ELF/BIN 散列与完整日志。

## Linux 基线、三轮中位数和 profiling

Linux 单次基线使用同一 chroot、源码、工具链和 cache，并与 StarryOS 一样继承
系统默认 CPU affinity、Cargo jobs、rustc 与 Rayon 线程策略：

```bash
apps/starry/orangepi-5-plus-selfbuild/run_linux_baseline.sh \
  --host <BOARD_IP> --skip-provision
```

在正确性和 Linux 启动恢复均通过后，交替执行三轮 Linux/StarryOS 冷构建并取
中位数。每轮 StarryOS 构建复用上述自动串口和 Linux 恢复流程：

```bash
apps/starry/orangepi-5-plus-selfbuild/benchmark.sh --host <BOARD_IP>
```

结果位于 `target/starry-orangepi5plus-selfbuild/benchmarks/`。每轮使用新的
Cargo target 目录；Linux 轮先 sync/drop_caches，StarryOS 轮通过重新启动清空
内核缓存。成功轮在复制 ELF/bin 后清理其 app 专属 target 目录，失败轮保留
target 便于诊断，避免六轮构建耗尽板端存储。

不需要等完整自举编译结束才做 profiling。profiling 只包住第一条命令
`cargo build -p tg-xtask`，最多运行 300 秒；到时由 `timeout` 发送 SIGINT，让
`perf` 正常落盘。第二条 `tg-xtask starry build ...` 不参与 profiling，也不会在
profiling 模式下启动。

先在 Linux 采一份，再用同一源码快照在 StarryOS 采一份：

```bash
# Linux：低开销硬件计数
apps/starry/orangepi-5-plus-selfbuild/run_linux_baseline.sh \
  --host <BOARD_IP> --skip-provision --profile stat

# StarryOS：相同的硬件计数窗口
apps/starry/orangepi-5-plus-selfbuild/run_selfbuild.sh \
  --host <BOARD_IP> --skip-provision --profile stat

# 需要定位热点函数时，两侧分别改为 --profile record
apps/starry/orangepi-5-plus-selfbuild/run_selfbuild.sh \
  --host <BOARD_IP> --skip-provision --profile record
```

`stat` 保存 cycles、instructions、cache references/misses、branches/misses 和实际
耗时；`record` 以 49 Hz 保存 cycle 采样，并在板载 Linux 恢复后生成
`perf-report.txt`。当前 StarryOS perf ABI 不支持 `PERF_SAMPLE_CALLCHAIN`，因此
这里有意不加 `-g`：它能给出平坦的用户态指令热点，不能给出完整调用链，也不能
直接回答 off-CPU 等待、锁竞争、调度延迟、缺页原因或块 I/O 延迟。先用 Linux/
StarryOS 的 IPC、cache/branch miss 比例和热点差异确定最大方向，再按证据补充
MOSS BuildStorm 风格的缺页、锁、调度、page cache 或 ext4 区间埋点。

`perf-stat.txt` 或 `perf.data`、`profile.meta`、源码元数据、SHA-256 和完整日志保存
在同一 run 目录，并取回到
`target/starry-orangepi5plus-selfbuild/artifacts/<run-id>/`。profiling PASS 只表示
受控窗口及数据落盘成功，不表示两阶段 StarryOS 自举已经完成。
