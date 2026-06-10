# StarryOS 测试套件维护指南

本文档说明 `test-suit/starryos/` 的当前目录约定，以及
`scripts/axbuild/src/starry/test.rs` 和 `scripts/axbuild/src/test/` 如何发现、构建和运行这些用例。

## 发现规则

StarryOS test-suit 不再使用 `normal`、`stress` 等一级测试组。QEMU 和 board
用例都直接从 `test-suit/starryos/` 根目录发现：

```text
test-suit/starryos/<case>/<runtime-config>.toml
test-suit/starryos/<build_wrapper>/<case>/<runtime-config>.toml
```

- QEMU 用例通过 `<case>/qemu-<arch>.toml` 发现。
- Board 用例通过 `<case>/board-<board>.toml` 发现。
- `<build_wrapper>` 用于共享构建配置，例如 `qemu-smp1`、`qemu-smp4`、`board-orangepi-5-plus`。
- 构建配置位于 case 或最近的 build wrapper 中，文件名为 `build-<target>.toml`。
- 如果目录自身同时包含 `build-*` 和 `qemu-*` / `board-*`，它本身也可以作为 case 被发现。
- 批量运行时，没有当前 arch/runtime config 的目录会被跳过。
- 显式 `-c/--test-case` 时，case 必须存在，且必须提供当前 arch 对应的 runtime config。
- Starry QEMU 支持 `qemu-smp1/<subcase>`、`qemu-smp4/<subcase>` 作为
  `qemu-smp*/system` 聚合 case 的单子测例选择器；也可以写成
  `qemu-smp*/system/<subcase>`。
- `<subcase>` 优先使用 `system/` 下的子目录名；如果某个子测例安装的
  `usr/bin/starry-test-suit` binary / CMake target 名与目录名不同，也可以使用唯一的
  binary / target 名，例如 `qemu-smp1/test-uid-gid-re-setters` 会映射到
  `qemu-smp1/system/syscall-test-uid-gid-re-setters`。
- `-l/--list` 列出根目录下发现的 Starry case；`qemu-smp1` 这类仅含 build config 的 wrapper 不会作为 root case 出现。

旧的 Starry `--test-group` 和 `--stress` 入口已经移除。需要运行迁出的压力、K230、
visual 或 golden 类用例时，使用 `cargo xtask starry app ...` 或对应脚本。

## 当前目录概览

```text
test-suit/starryos/
  qemu-smp1/
    build-aarch64-unknown-none-softfloat.toml
    build-loongarch64-unknown-none-softfloat.toml
    build-riscv64gc-unknown-none-elf.toml
    build-x86_64-unknown-none.toml
    system/
      CMakeLists.txt
      prebuild.sh
      qemu-aarch64.toml
      qemu-loongarch64.toml
      qemu-riscv64.toml
      qemu-x86_64.toml
      syscall-test-brk/
        CMakeLists.txt
        src/
      bugfix-bug-futex-wait-wake/
        CMakeLists.txt
        src/
      c-regression-test-msync/
        CMakeLists.txt
        src/
      drm-test-drm-perbuf-dumb/
        CMakeLists.txt
        src/
      evdev-test-evdev-event-primary/
        CMakeLists.txt
        src/
      usb-audio-iso/
        CMakeLists.txt
        src/
      usb-storage/
        CMakeLists.txt
        src/
  qemu-smp4/
    build-<target>.toml
    system/
      CMakeLists.txt
      qemu-<arch>.toml
      <subcase>/CMakeLists.txt
      <subcase>/src/
  board-orangepi-5-plus/
    build-aarch64-unknown-none-softfloat.toml
    npu-yolov8/
      board-orangepi-5-plus.toml
    pcie-enumerate/
      board-orangepi-5-plus.toml
```

`qemu-smp1/system` 和 `qemu-smp4/system` 分别是单核、多核 build wrapper 下的
唯一聚合 QEMU case。`qemu-smp1/` 与 `qemu-smp4/` 根目录只放四架构 build config，
不放 `qemu-*.toml`。

## qemu-smp*/system 聚合

`qemu-smp1/system/qemu-*.toml` 和 `qemu-smp4/system/qemu-*.toml` 共用一次
StarryOS 启动运行对应 SMP 配置下的所有系统类子测例。子测例目录直接放在
`system/` 下，每个子测例只保留自己的资产目录：

```text
qemu-smp1/system/<subcase>/
  CMakeLists.txt
  src/
```

子测例目录不要再放 `qemu-*.toml`。架构过滤不能依赖子目录下的 runtime config，而应在代码或 CMake 中显式处理。

`system/qemu-*.toml` 的 `test_commands` 使用 grouped runner 风格，扫描
`/usr/bin/starry-test-suit/*` 并逐个执行。所有子测例通过后打印：

```text
STARRY_GROUPED_TESTS_PASSED
```

子测例 CMake 产物应安装到：

```cmake
install(TARGETS mytest RUNTIME DESTINATION usr/bin/starry-test-suit)
```

如果某个 C 子测例只支持部分架构，优先使用 `system/common/starry_arch_filter.cmake`
生成 skip 二进制。skip 输出要清楚说明目标和原因，并返回 0。

## QEMU 参数约定

`system/qemu-x86_64.toml`、`system/qemu-aarch64.toml`、`system/qemu-riscv64.toml`
需要同时覆盖常规系统回归、DRM、evdev 和 USB：

- `virtio-blk` 主启动盘 `disk0` 使用 Alpine rootfs。
- `virtio-net` 提供基础网络。
- `virtio-gpu`、`virtio-keyboard`、`virtio-tablet` 支持 DRM/evdev。
- `qemu-xhci,id=xhci,msi=off,msix=off`、`usb-audio`、`usb-storage` 支持 USB 回归。
- USB storage 第二盘使用 `${workspace}/tmp/axbuild/rootfs/rootfs-<arch>-busybox.img`。

`system/qemu-loongarch64.toml` 不带 xHCI、USB audio 或 USB storage。对应 build config
也不启用 `ax-driver/xhci-pci`。USB 测试程序仍会被构建并安装，但在 loongarch64 guest
内立即打印 skip marker 并返回 0，不能访问 USB 设备。

## QEMU 用例类型

运行器会根据 case 目录内容选择一个 asset pipeline。一个 case 只能使用一种 pipeline。

| Pipeline | 触发条件 | 行为 |
| --- | --- | --- |
| `plain` | 无 `test_commands`，且无 `c/`、`sh/`、`python/` | 直接启动共享 rootfs，并追加 QEMU `-snapshot` |
| `c` | case 目录下存在 `c/` | 使用 CMake 交叉编译，安装产物到 rootfs overlay |
| `sh` | case 目录下存在 `sh/` | 将 shell 脚本注入 `/usr/bin/` |
| `python` | case 目录下存在 `python/` | 在 staging rootfs 中安装 `python3`，并注入 `.py` 文件 |
| `grouped` | `qemu-<arch>.toml` 中存在 `test_commands` | 构建子目录资产，注入 grouped runner 需要的 guest 程序 |

Pipeline case 会创建每个 case 独立的 rootfs 副本，并把注入后的 rootfs 缓存在：

```text
target/<target>/qemu-cases/<build_group>/<case>/cache/rootfs/
```

plain case 不复制 rootfs，依赖 QEMU `-snapshot` 保证 guest 写入不落回共享镜像。

## QEMU TOML

每个 `qemu-<arch>.toml` 定义运行配置，而不是构建配置。常用字段如下：

| 字段 | 说明 |
| --- | --- |
| `args` | QEMU 参数，`${workspace}` / `${workspaceFolder}` 会解析为仓库根目录 |
| `uefi` | 是否使用 UEFI |
| `to_bin` | 是否把 ELF 转为裸二进制 |
| `shell_prefix` | 等待 guest shell 的提示符 |
| `shell_init_cmd` | plain/C/sh/python case 的 guest 命令 |
| `test_commands` | grouped case 的 guest 命令列表；不能与 `shell_init_cmd` 同时使用 |
| `success_regex` | 全部匹配才 PASS |
| `fail_regex` | 任一匹配即 FAIL |
| `timeout` | 超时时间，单位秒 |

示例：

```toml
args = [
    "-nographic", "-cpu", "rv64",
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
]
uefi = false
to_bin = true
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 15
```

## C 用例

普通 C case：

```text
<case>/
  qemu-<arch>.toml
  c/
    CMakeLists.txt
    prebuild.sh        # 可选
    src/
      main.c
```

`CMakeLists.txt` 至少应安装可执行文件：

```cmake
cmake_minimum_required(VERSION 3.20)
project(mytest C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(mytest src/main.c)
target_compile_options(mytest PRIVATE -Wall -Wextra -Werror)

install(TARGETS mytest RUNTIME DESTINATION usr/bin)
```

如果该 C case 是 `qemu-smp*/system` 的子测例，安装目录必须改成
`usr/bin/starry-test-suit`。

如果需要在 staging rootfs 中安装额外包，可添加 `c/prebuild.sh`：

```sh
#!/bin/sh
set -eu

apk add zlib-dev
```

`prebuild.sh` 通过 qemu-user 在 staging rootfs 中执行。可用环境变量包括：

- `STARRY_STAGING_ROOT`
- `STARRY_CASE_DIR`
- `STARRY_CASE_C_DIR`
- `STARRY_CASE_WORK_DIR`
- `STARRY_CASE_BUILD_DIR`
- `STARRY_CASE_OVERLAY_DIR`

## Grouped 用例

当多个 guest 程序可以共用同一次 StarryOS 启动时，使用 grouped case：

```text
<case>/
  qemu-<arch>.toml
  <subcase-a>/c/CMakeLists.txt
  <subcase-b>/c/CMakeLists.txt
```

`qemu-smp1/system` 和 `qemu-smp4/system` 是特殊的大型 system grouped
case：`system/CMakeLists.txt` 是唯一 configure 入口，自动 `add_subdirectory()`
各个 subcase；每个 subcase 直接在目录根放 `CMakeLists.txt` 和 `src/`。如果所有
system subcase 需要共享 rootfs 准备步骤，把脚本放在 `system/prebuild.sh`，不要给
单个 subcase 增加 `prebuild.sh`。

调试单个 system subcase 时，不需要新增 CLI 参数，直接复用 `-c/--test-case`：

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/syscall-test-uid-gid-re-setters
cargo xtask starry test qemu --arch x86_64 -c qemu-smp4/test-futex-race
```

这会继续使用对应 wrapper 的 `system/qemu-<arch>.toml`，但只配置、编译和注入指定
subcase 目录。

在 `qemu-<arch>.toml` 中使用 `test_commands`：

```toml
shell_prefix = "root@starry:"
test_commands = [
    "/usr/bin/test-a",
    "/usr/bin/test-b",
]
success_regex = ["(?m)^STARRY_GROUPED_TESTS_PASSED\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b', '(?m)^STARRY_GROUPED_TEST_FAILED:']
```

运行器会稳定排序子目录、构建 C subcase，并注入 grouped runner 支持文件。每个命令执行前后都会打印带 `step=当前/总数`、`epoch=`、`status=` 和 `command=` 的标记，例如：

```text
STARRY_GROUPED_TEST_BEGIN: step=1/2 epoch=... command=/usr/bin/test-a
STARRY_GROUPED_TEST_PASSED: step=1/2 epoch=... status=0 command=/usr/bin/test-a
```

如果 grouped case 超时，CI 日志中最后一个 `STARRY_GROUPED_TEST_BEGIN` 通常就是卡住的子命令。
目前 grouped Rust subcase 还不支持。

## Shell 和 Python 用例

Shell case 使用 `sh/`：

```text
<case>/
  qemu-<arch>.toml
  sh/
    my-test.sh
```

Python case 使用 `python/`：

```text
<case>/
  qemu-<arch>.toml
  python/
    test_hello.py
```

Python pipeline 会自动在 staging rootfs 中安装 `python3`，再把 `.py` 文件复制到 `/usr/bin/`。

## Board 用例

Board 用例目录结构：

```text
<build_wrapper>/
  build-<target>.toml
  <case>/
    board-<board>.toml
```

`board-<board>.toml` 是板测运行配置。发现 board case 后，xtask 会默认映射到：

```text
os/StarryOS/configs/board/<board>.toml
```

并从该 board build config 读取 target。如果当前 build wrapper 下存在匹配的
`build-<target>.toml`，则优先使用 test-suit 中的构建配置。

运行示例：

```bash
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -c board-orangepi-5-plus/pcie-enumerate --board orangepi-5-plus
```

## 运行命令

```bash
# QEMU
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --target riscv64gc-unknown-none-elf
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/system
cargo xtask starry test qemu --arch x86_64 -c qemu-smp1/syscall-test-uid-gid-re-setters

# 列出发现的用例
cargo xtask starry test qemu -l
cargo xtask starry test board -l

# board
cargo xtask starry test board --board orangepi-5-plus
cargo xtask starry test board -c board-orangepi-5-plus/npu-yolov8 --board orangepi-5-plus

# 迁出的 heavy app
cargo xtask starry app qemu -t stress/git --arch riscv64
cargo xtask starry app qemu -t k230-qemu/qemu-k230/kpu-smoke --arch riscv64
```

## 维护注意事项

- 只为实际验证通过的架构添加 `qemu-<arch>.toml`。
- `qemu-smp1` / `qemu-smp4` 的并发度由 build config 决定；不要只改 QEMU `-smp` 而忘记构建配置。
- `shell_init_cmd` 和 `test_commands` 不能同时使用。
- 一个 case 只能定义一种 pipeline；不要同时放 `c/`、`sh/`、`python/` 或 `test_commands`。
- `success_regex` 选择稳定且唯一的成功行。
- `fail_regex` 保持精确，避免匹配正常输出如 `failed: 0`。
- 不要在同一个工作区并行运行多个 `cargo xtask starry test qemu`，rootfs 和生成配置可能互相影响。
- heavy app 不应放回 `test-suit/starryos`；迁出到 `apps/starry` 后加入 `apps/.ignore`，需要时用显式 `-t` 运行。
