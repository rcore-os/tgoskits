# StarryOS macOS HVF 自举编译修复报告

## 背景

目标是在 Apple Silicon macOS 上用 QEMU HVF 启动 AArch64 StarryOS guest，
然后在 StarryOS guest 内运行 Cargo，重新编译 StarryOS，并把 guest 编译出的
kernel 提取出来，再按普通 `cargo xtask starry qemu --arch aarch64` 路径启动。

最终目标不是让 macOS 宿主机交叉编译成功，而是验证 StarryOS 作为运行环境时，
能支撑 Cargo、rustc、链接工具、文件系统写入、进程/pipe/wait、kallsyms 以及
最终 kernel 启动这一整条链路。

## 最初遇到的问题

### 1. self-build 稳定卡死

最初按下面的流程执行：

```bash
apps/starry/macos-selfbuild/build_rootfs.sh
apps/starry/macos-selfbuild/build_kernel.sh

KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 JOBS=8 RAYON_NUM_THREADS=1 RUSTC_THREADS=2 SOURCE_TMPFS=1 \
QEMU_TIMEOUT_SEC=10800 \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

现象是 QEMU 仍然存活，但 Cargo 进度长时间不前进：

```text
SMP=8 JOBS=8: Building ... 254/318
SMP=4 JOBS=4: Building ... 178/318
```

`host-heartbeat` 还能持续输出，说明宿主机 runner 没死，QEMU 进程也没退出。
但是 guest 内 Cargo 没有继续产生有效构建进度。

### 2. CPU 占用不符合预期

QEMU 暴露给 guest 的 vCPU 数是 `SMP=8`，Cargo job 数是 `JOBS=8`，但 macOS
活动监视器看到的总 CPU 占用并没有打满 8 核。这个现象本身不是单独的 bug，
它说明 guest 内部存在串行瓶颈或阻塞点，vCPU 数量并不等于宿主机会持续满载。

后续从日志看，主要瓶颈不是 QEMU 没分配 CPU，而是 guest 里的文件系统写入、
元数据同步、kallsyms 处理、以及部分 kernel/runtime 逻辑问题。

### 3. 编译 profile 和启动 profile 不一致

一开始尝试过较“瘦”的 defplat/静态 profile，但它和普通
`cargo xtask starry qemu --arch aarch64` 使用的 qemu-aarch64 动态平台路径不一致。

真正要验证的 kernel 应该按 qemu-aarch64 board 的动态平台特性构建，也就是：

```text
plat-dyn,
ax-driver/virtio-blk,
ax-driver/virtio-net,
ax-driver/virtio-gpu,
ax-driver/virtio-input,
ax-driver/virtio-socket,
starry-kernel/input,
starry-kernel/vsock
```

同时 AArch64 这条路径应走 PIE JSON target spec，而不是旧的静态 `defplat`
linker script 路径。

### 4. guest 内缺少 kallsyms 所需工具

guest 编译出 `starryos` 后，还需要跑 `starry-kallsyms.sh`，这一步依赖：

- `rust-nm`
- `rust-objdump`
- `rust-objcopy`
- `gen_ksym`

guest rootfs 内不一定直接有这些 Rust binutils 工具。之前即使 Cargo build 过了，
也会在 kallsyms/objcopy 阶段暴露工具缺失或执行路径问题。

### 5. kallsyms padding 极慢

`starry-kallsyms.sh` 原先用类似 `dd bs=1` 的方式补齐 `.kallsyms` section。
在 StarryOS guest 文件系统里，这种逐字节写入会触发大量小写入和元数据更新，
非常慢，容易表现成“编译已经差不多完成但后处理卡住”。

### 6. ext4 写入路径元数据同步过重

排查过程中确认 StarryOS 的 ext4/rsext4 路径对普通文件写入、append、`set_len`
做了过重的同步。Cargo/rustc 会产生大量小文件、临时文件、rename、truncate，
如果每次普通写入都同步整套文件系统元数据，guest 内构建会被严重拖慢。

另外，extent 文件 truncate 扩展时不应该为了逻辑增长提前分配并清零所有块；
Linux 语义下增长出来的新区域应当按 sparse/zero-read 处理。

### 7. 自举编译出的 kernel 还需要能按普通 xtask 路径启动

自举编译完成只是第一步，还要证明 guest 生成的 kernel 不是只能“编出来”，而是能
按普通 StarryOS qemu-aarch64 路径启动并进入 shell。

这个过程中又暴露出几类 runtime 问题：

- FD table 初始化时会在 AArch64 kernel stack 上形成巨大临时对象；
- kprobe/kretprobe selftest 存在递归锁和 kernel task fallback 问题；
- `membarrier` 常量使用了“命令编号”而不是 Linux UAPI bitmask，测试期望也随之错误。

## 排查方式

### 1. 增加 host heartbeat

`run_selfbuild.sh` 会周期性输出：

```text
host-heartbeat elapsed=<seconds> qemu_pid=<pid> ...
```

这个输出用于确认：

- 宿主机 runner 仍在运行；
- QEMU 进程仍存在；
- Cargo 最近一条进度是什么；
- 是否命中 timeout、panic、trap、segfault 等 fail pattern。

它帮助区分“QEMU 退出了”和“guest 内部卡住了”。

### 2. 增加 target heartbeat

guest runner 增加 `TARGET_HEARTBEAT_SEC`，周期性统计 target 目录：

```text
===STARRY-MACOS-SELFBUILD-TARGET-HEARTBEAT
dir=/tmp/starryos-selfbuild-target
kib=<size>
files=<count>
changed=<changed_count>
sample=<recent_files>
===
```

最早卡住时，target 目录文件数长期不再变化。例如旧日志里 `254/318` 附近，
heartbeat 的 files 数停住，说明不是单纯 Cargo UI 没刷新，而是构建产物确实没有继续生成。

### 3. 降低变量组合，分别测试 SMP/JOBS/source tmpfs

分别尝试过：

```text
SMP=8 JOBS=8
SMP=4 JOBS=4
SOURCE_TMPFS=0
SOURCE_TMPFS=1
RUSTC_THREADS=1/2
RAYON_NUM_THREADS=1
```

结论是：

- `SMP/JOBS` 会影响并发，但不能解决 guest 内部串行卡点；
- source/target 放到 `/tmp` 能显著减少 ext4 写入压力；
- `RUSTC_THREADS=2` 是当前本地复现较稳定的设置；
- 单纯加大 `SMP` 或 `JOBS` 不会绕过 FS/runtime 问题。

### 4. 单独编译 crate 收敛问题

排查时尝试过把完整构建拆成单 crate 或更小依赖组来观察是否复现卡死。

如果单独 crate 也卡住，问题更偏向 StarryOS syscall/FS/runtime；如果单独能过，
再回到完整依赖图看是哪个组合触发。这个方式帮助把“源码问题”和“OS 环境问题”
区分开来：同一份源码在 macOS 宿主机能编译，但在 StarryOS guest 卡住，说明
核心问题还是 StarryOS 作为构建 OS 时暴露出的系统能力问题。

### 5. 对齐 xtask qemu-aarch64 配置

用户明确要求最终按：

```bash
cargo xtask starry qemu --arch aarch64
```

对应的普通 StarryOS 启动路径来验证，而不是在这个命令里再次跑自举编译。

因此排查时检查了 xtask 的 qemu-aarch64 board 配置，确认默认 dynamic platform
features、rootfs、内存和启动方式，并新增 `--kernel-elf` 支持，使 xtask 可以直接
启动已经编译好的外部 kernel ELF。

### 6. 使用 debugfs/提取脚本验证 rootfs 产物

最终 guest 会把产物复制到 rootfs copy 内：

```text
/opt/starryos-selfbuild-artifacts/starryos-aarch64-unknown-none-softfloat
/opt/starryos-selfbuild-artifacts/starryos-aarch64-unknown-none-softfloat.bin
```

先用 `debugfs` 手动提取验证，后续整理成脚本：

```bash
eval "$(apps/starry/macos-selfbuild/extract_kernel.sh)"
```

脚本默认从最新 rootfs copy 中提取 ELF 和 `.bin`，并输出：

```text
rootfs_copy=...
kernel_elf=...
kernel_bin=...
```

## 按优先级排序的问题与修复

下面按“先解除哪个阻塞，才能继续推进下一阶段”的顺序写。每一项只对应一个问题和
一个修复方向。

### P0-1. 问题：membarrier UAPI 语义错误，单模块阶段无法跑通

排查依据：

- 单模块验证在 `membarrier` 修复后才能继续跑通；
- 旧实现把 Linux UAPI 的 bitmask 命令值当成连续命令编号；
- 真实用户态按 Linux UAPI 调用 query/register/private expedited 时，StarryOS
  返回的能力 mask 和实际命令匹配不上。

修复：

- `membarrier` command 常量改为使用 `linux_raw_sys::general::membarrier_cmd`；
- `SUPPORTED_COMMANDS` 改为 Linux UAPI bitmask 语义；
- 测试期望从 `62` 改成 `31`；
- 测试里的 `query_advertises()` 改成直接按 mask 判断，而不是 `1 << cmd`。

涉及文件：

- `os/StarryOS/kernel/src/syscall/sync/membarrier.rs`
- `test-suit/starryos/normal/qemu-smp1/test-membarrier/c/src/main.c`

### P0-2. 问题：构建 profile 和普通 qemu-aarch64 启动路径不一致

排查依据：

- 较瘦的 defplat/静态 profile 可以减少构建量，但它不是普通
  `cargo xtask starry qemu --arch aarch64` 对应的 qemu-aarch64 dynamic platform；
- 如果 profile 不一致，即使 guest 内能编译，也不能证明产物能按目标启动路径工作；
- crate 总数和 feature set 可以用来识别是否跑到了错误 profile。

修复：

- self-build 默认使用 `TARGET_SPEC_MODE=pie`；
- Cargo target 使用 `scripts/targets/pie/aarch64-unknown-none-softfloat.json`；
- 默认 features 改为 qemu-aarch64 dynamic platform 所需设备特性；
- `EXPECTED_MAX_CRATES` 调整为 `420`，最终实际为 `386`；
- `BUILD_STD` 调整为 `core,alloc`。

涉及文件：

- `apps/starry/macos-selfbuild/guest-selfbuild.sh`
- `apps/starry/macos-selfbuild/reproduce.sh`
- `apps/starry/macos-selfbuild/prebuild.sh`

### P0-3. 问题：rootfs/source 可能过期，导致长时间跑在错误输入上

排查依据：

- self-build 一次运行耗时很长，如果 rootfs 里嵌入的是旧源码，会浪费大量时间；
- 分支切换、`git pull`、本地修复后，rootfs 内 `/opt/tgoskits-src.tar` 必须同步刷新；
- 旧 rootfs 会造成“明明改了源码但 guest 里仍在跑旧代码”的误判。

修复：

- `prepare_rootfs.sh` 注入当前源码 tarball 和 `/opt/tgoskits-src.meta`；
- `run_selfbuild.sh` 默认检查 rootfs commit 是否和当前 checkout 匹配；
- runner 复制输入 rootfs 到 `target/starry-macos-selfbuild/rootfs/` 后写入临时副本；
- guest 编译成功后把 ELF 和 `.bin` 复制到 `/opt/starryos-selfbuild-artifacts/`；
- 增加 crate count guard，发现异常 crate total 时提前停止。

涉及文件：

- `apps/starry/macos-selfbuild/prepare_rootfs.sh`
- `apps/starry/macos-selfbuild/run_selfbuild.sh`
- `apps/starry/macos-selfbuild/guest-selfbuild.sh`
- `apps/starry/macos-selfbuild/reproduce.sh`

### P0-4. 问题：guest 内构建进度停止，target 目录不再增长

排查依据：

- 旧日志中 Cargo 长时间停在 `178/318`、`254/318` 等位置；
- host heartbeat 证明 QEMU 仍在；
- target heartbeat 显示卡住时 target 文件数不再增长；
- Cargo/rustc 在 guest 内会大量创建、写入、truncate 临时文件，StarryOS ext4 路径
  每次普通写入都同步元数据，会放大成严重性能问题。

修复：

- source/target 尽量放到 `/tmp`，减少 ext4 写入压力；
- 普通文件 `write_at`、`append`、`set_len` 后不再立即 `sync_to_disk()`；
- extent truncate 扩展时不再提前分配所有新增逻辑块，改为 sparse 增长；
- truncate 收缩时清理 partial tail block，保证读回符合 zero-fill 预期；
- 更新 `test_file_truncate`，把增长后的新区域期望从旧数据改为零。

涉及文件：

- `os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/inode.rs`
- `components/rsext4/src/file/io.rs`
- `components/rsext4/tests/file_operations.rs`

### P0-5. 问题：guest 内缺少 kallsyms 所需工具

排查依据：

- Cargo build 成功后还要运行 `starry-kallsyms.sh`；
- guest rootfs 内不一定存在 `rust-nm`、`rust-objdump`、`rust-objcopy`、`gen_ksym`；
- 这些工具缺失会导致最终 kernel artifact 不能完成后处理。

修复：

- 如果 guest 没有 `rust-nm`，用 `llvm-nm` 包装；
- 如果 guest 没有 `rust-objdump`，用 `llvm-objdump` 包装；
- 如果 guest 没有 `rust-objcopy`，用 `llvm-objcopy` 包装；
- 从离线 Cargo cache 中安装 `ksym` crate 的 `gen_ksym`；
- Cargo build 成功后自动运行 `starry-kallsyms.sh`，再复制最终 ELF 和 `.bin`。

涉及文件：

- `apps/starry/macos-selfbuild/guest-selfbuild.sh`

### P0-6. 问题：kallsyms padding 写入极慢

排查依据：

- 旧 `starry-kallsyms.sh` 使用逐字节 padding；
- StarryOS guest 文件系统里大量小写入会触发严重元数据开销；
- 这会让“Cargo 已经编完，但后处理还卡住”的现象更加明显。

修复：

- `starry-kallsyms.sh` 优先使用 `truncate -s` 补齐 `.kallsyms`；
- 无 `truncate` 时使用 MiB 级大块 `dd`，避免 `dd bs=1`；
- linker script 支持 `STARRY_KALLSYMS_RESERVED`；
- guest self-build 默认使用 `STARRY_KALLSYMS_RESERVED=64M`。

涉及文件：

- `scripts/axbuild/scripts/starry-kallsyms.sh`
- `os/StarryOS/starryos/build.rs`
- `os/StarryOS/starryos/linker.ld`

### P0-7. 问题：FD table 初始化形成 AArch64 kernel stack 巨大临时对象

排查依据：

- self-built kernel 还必须能启动；
- `FlattenObjects<FileDescriptor, AX_FILE_LIMIT>` 作为 scope-local 默认值初始化时，
  容易在 AArch64 kernel stack 上形成巨大临时对象；
- 这属于 kernel runtime 启动阶段风险。

修复：

- `FD_TABLE` 从 `Arc::default()` 改为直接在 heap 上初始化；
- 避免大对象先落到 kernel stack；
- 降低 self-built kernel 启动阶段的栈风险。

涉及文件：

- `os/StarryOS/kernel/src/file/mod.rs`

### P0-8. 问题：kprobe/kretprobe selftest 可能递归锁住或缺少 kernel task fallback

排查依据：

- self-built kernel 启动时会经过 kprobe/kretprobe selftest；
- 旧 selftest 路径在持有 manager lock 时执行可能再次进入 probe 管理逻辑；
- kretprobe instance stack 原先假设当前任务一定是 thread，对 kernel task 不稳。

修复：

- kprobe selftest 不再在持有 manager lock 时执行会再次进入 probe 管理逻辑的路径；
- kretprobe instance stack 支持 kernel task fallback；
- 避免 self-built kernel 启动时 kprobe/kretprobe selftest 死锁或栈路径异常。

涉及文件：

- `os/StarryOS/kernel/src/kprobe.rs`

### P1-1. 问题：guest-built kernel 提取和启动流程不标准

排查依据：

- 手写 debugfs dump 命令容易写错路径；
- xtask 原先不能直接启动一个已经编译好的外部 kernel ELF；
- 最终验证需要按普通 qemu-aarch64 xtask 路径启动，而不是在 xtask 里重新自举编译。

修复：

- 新增 `extract_kernel.sh`，默认从最新 rootfs copy 中提取 ELF 和 `.bin`；
- 输出 `rootfs_copy`、`kernel_elf`、`kernel_bin` shell 变量，方便直接接 xtask；
- xtask 增加 `--kernel-elf`，支持启动外部 ELF。

涉及文件：

- `apps/starry/macos-selfbuild/extract_kernel.sh`
- `scripts/axbuild/src/starry/mod.rs`
- `scripts/axbuild/src/starry/rootfs.rs`

### P2-1. 问题：复现命令和排查结论不成体系

排查依据：

- 排查过程中有多套命令、多个 rootfs copy、多个中间 kernel artifact；
- 如果文档不收敛，后续很容易混用旧参数或旧 rootfs。

修复：

- `README.md` 和 `README_CN.md` 整理成一套端到端命令；
- `RESULTS.md` 记录最终 verified run；
- `fix_report.md` 记录问题、排查方式、优先级、修复和复现方式。

涉及文件：

- `apps/starry/macos-selfbuild/README.md`
- `apps/starry/macos-selfbuild/README_CN.md`
- `apps/starry/macos-selfbuild/RESULTS.md`
- `fix_report.md`

## 最终验证结果

最终成功 self-build 命令：

```bash
KERNEL=target/aarch64-unknown-none-softfloat/release/starryos.bin \
ROOTFS=tmp/axbuild/rootfs/rootfs-aarch64-hvf-selfbuild.img \
SMP=8 JOBS=8 MEM=4096M \
RAYON_NUM_THREADS=1 RUSTC_THREADS=2 SOURCE_TMPFS=1 \
TARGET_HEARTBEAT_SEC=60 \
QEMU_TIMEOUT_SEC=10800 \
EXPECTED_MAX_CRATES=420 \
CASE_NAME=smp8-j8-final-fd-io-fix \
apps/starry/macos-selfbuild/run_selfbuild.sh
```

成功日志：

```text
target/starry-macos-selfbuild/logs/smp8-j8-final-fd-io-fix-20260610T065926.log
```

关键结果：

```text
Cargo total: 386 crates
guest build elapsed: 634s
ELF artifact bytes: 100170488
BIN artifact bytes: 77656064
pass marker: ===STARRY-MACOS-SELFBUILD-PASS jobs=8 elapsed=634===
```

提取结果：

```bash
eval "$(apps/starry/macos-selfbuild/extract_kernel.sh)"
```

输出变量形态：

```text
rootfs_copy=target/starry-macos-selfbuild/rootfs/rootfs-...
kernel_elf=target/starry-macos-selfbuild/extracted/starryos-selfbuilt-...
kernel_bin=target/starry-macos-selfbuild/extracted/starryos-selfbuilt-....bin
```

按普通 xtask qemu-aarch64 路径启动：

```bash
cargo xtask starry qemu \
  --arch aarch64 \
  -c os/StarryOS/configs/board/qemu-aarch64.toml \
  --rootfs "$rootfs_copy" \
  --kernel-elf "$kernel_elf"
```

已验证进入 shell：

```text
root@starry:/root #
```

## 当前推荐复现流程

从全新 clone 开始：

```bash
brew install qemu e2fsprogs zig llvm

git clone https://github.com/yks23/tgoskits.git
cd tgoskits
git checkout app/starry-macos-selfbuild

RUST_DIST_SERVER=https://rsproxy.cn \
STARRY_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/ \
apps/starry/macos-selfbuild/reproduce.sh

eval "$(apps/starry/macos-selfbuild/extract_kernel.sh)"

cargo xtask starry qemu \
  --arch aarch64 \
  -c os/StarryOS/configs/board/qemu-aarch64.toml \
  --rootfs "$rootfs_copy" \
  --kernel-elf "$kernel_elf"
```

如果只是更新源码，不需要重建完整工具链 rootfs，可以用：

```bash
ROOTFS_MODE=prepare-rootfs \
SMP=8 JOBS=8 MEM=4096M \
RUST_DIST_SERVER=https://rsproxy.cn \
STARRY_CARGO_REGISTRY_INDEX=sparse+https://rsproxy.cn/index/ \
apps/starry/macos-selfbuild/reproduce.sh
```

## 已执行的本地验证

已执行并通过：

```bash
cargo fmt
git diff --check
bash -n apps/starry/macos-selfbuild/*.sh scripts/axbuild/scripts/starry-kallsyms.sh
cargo clippy --no-deps -p rsext4 -- -D warnings
docker run --rm --privileged --platform linux/amd64 \
  -v "$PWD":/workspace -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest \
  cargo test -p rsext4 test_file_truncate
cargo xtask clippy --package tg-xtask
```

`cargo xtask clippy --package starry-kernel` 仍然会被既有 host-side
`components/percpu/percpu/src/imp.rs` 符号宏问题阻塞，错误形态是
`cannot subtract ! from !`、`usize + !`。这个问题不属于本次 self-build
修复引入的变化。

## 结论

这次问题不是单纯“源码不能编译”。同一份源码在 macOS 宿主机可以编译，但在
StarryOS guest 里跑 Cargo/rustc 时，会触发 StarryOS 的文件系统、系统调用、
kernel runtime 和构建后处理链路上的真实问题。

最终修复后的状态是：

- StarryOS guest 内可以完整编译 StarryOS；
- guest 内完成 kallsyms 和 `.bin` 生成；
- 编译产物能持久化到 rootfs copy；
- 宿主机可以用脚本提取 guest-built kernel；
- 提取出的 kernel 可以按普通 xtask qemu-aarch64 路径启动到 shell。
